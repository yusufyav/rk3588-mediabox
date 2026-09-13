use clap::Parser;
use mediabox_cec::{Adapter, unavailable_status};
use mediabox_core::{CecStatus, InputAction, InputMode, InputSource, Surface};
use mediabox_input::InputManager;
use mediaboxd_rs::daemon::{AppState, CecRuntime, apply_kodi_route, serve_unix, socket_is_live};
use mediaboxd_rs::kodi::KodiClient;
use mediaboxd_rs::lifecycle::{ApplicationManager, KodiLifecycle, SurfaceManager};
use mediaboxd_rs::media::MediaClient;
use mediaboxd_rs::web::{PeerPolicy, WebConfig, serve as serve_web};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, UnixListener};

#[derive(Debug, Parser)]
#[command(version, about = "MediaBox Rust control plane")]
struct Args {
    #[arg(long, default_value = "/run/mediabox/mediaboxd.sock")]
    socket: PathBuf,
    #[arg(long)]
    http: Option<SocketAddr>,
    #[arg(long, default_value = "http://127.0.0.1:8080/jsonrpc")]
    kodi_endpoint: String,
    #[arg(long, default_value = "kodi.service")]
    kodi_unit: String,
    /// The unit that puts the product UI on the television. Only ever started
    /// and stopped; the daemon never becomes a compositor itself.
    #[arg(long, default_value = "mediabox-tv-ui.service")]
    ui_unit: String,
    /// The table of applications this box can put on the television. Absent,
    /// the two it has always had — Kodi and the product UI — are used, so an
    /// appliance that was never configured still works.
    #[arg(long)]
    applications: Option<PathBuf>,
    /// Installed product UI. Without it the daemon serves the API only.
    #[arg(long)]
    ui_root: Option<PathBuf>,
    /// Extra listener for the home network. It serves the same UI and the same
    /// control endpoint, and admits private-network peers only.
    #[arg(long)]
    lan_http: Option<SocketAddr>,
    #[arg(long, default_value = "http://127.0.0.1:8790")]
    media_endpoint: String,
    #[arg(long)]
    cec_device: Option<PathBuf>,
    #[arg(long)]
    disable_cec: bool,
    #[arg(long)]
    require_cec: bool,
    #[arg(long, value_enum, default_value_t = ModeArg::Ui)]
    input_mode: ModeArg,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Ui,
    KodiPlayback,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if let Some(address) = args.http
        && !address.ip().is_loopback()
    {
        return Err("HTTP API güvenlik gereği yalnız loopback adresine bağlanabilir".into());
    }
    prepare_socket(&args.socket)?;
    let input = InputManager::new(match args.input_mode {
        ModeArg::Ui => InputMode::Ui,
        ModeArg::KodiPlayback => InputMode::KodiPlayback,
    });
    let (adapter, unavailable) = if args.disable_cec {
        (
            None,
            CecStatus {
                error: Some("CEC komut satırından kapatıldı".into()),
                ..Default::default()
            },
        )
    } else {
        match Adapter::discover(args.cec_device.as_deref()) {
            Ok(adapter) => (Some(adapter), CecStatus::default()),
            Err(error) if args.require_cec => return Err(error.into()),
            Err(error) => (None, unavailable_status(args.cec_device.as_deref(), &error)),
        }
    };
    let kodi = Arc::new(KodiClient::new(
        &args.kodi_endpoint,
        Duration::from_secs(2),
    )?);
    let ui_root = match args.ui_root {
        Some(root) => Some(root.canonicalize().map_err(|error| {
            format!("--ui-root okunamadı ({}): {error}", root.display())
        })?),
        None => None,
    };
    let state = Arc::new(AppState {
        kodi: kodi.clone(),
        lifecycle: KodiLifecycle::new(&args.kodi_unit)?,
        surface: SurfaceManager::new(&args.kodi_unit, &args.ui_unit)?,
        applications: ApplicationManager::load(
            args.applications.as_deref(),
            &args.kodi_unit,
            &args.ui_unit,
        )?,
        cec: CecRuntime {
            adapter: adapter.clone(),
            unavailable,
        },
        input: input.clone(),
        media: Arc::new(MediaClient::new(
            &args.media_endpoint,
            Duration::from_secs(30),
        )?),
    });

    let stop = Arc::new(AtomicBool::new(false));
    let receiver = if let Some(adapter) = adapter {
        let stop = stop.clone();
        let input = input.clone();
        let kodi = kodi.clone();
        let state = state.clone();
        let runtime = tokio::runtime::Handle::current();
        Some(
            std::thread::Builder::new()
                .name("mediabox-cec-rx".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match adapter.receive(250) {
                            Ok(Some((parsed, Some((action, pressed))))) => {
                                let mode = input.mode();
                                let decision = input.publish(
                                    action,
                                    InputSource::Cec,
                                    pressed,
                                    Some(parsed.event.timestamp_ns),
                                );
                                if pressed {
                                    let kodi = kodi.clone();
                                    runtime.spawn(async move {
                                        let _ = apply_kodi_route(&kodi, decision).await;
                                    });
                                }
                                // Home reclaims the display; Back does not.
                                //
                                // Back belongs to whatever is on screen. The
                                // kernel's CEC driver publishes the remote as
                                // a real input device, so Kodi receives Back
                                // itself and answers it the way Kodi does —
                                // leave full screen, show the menu, keep the
                                // film running. Taking the display away on
                                // Back instead is what made a single press
                                // stop the film and quit Kodi.
                                if pressed
                                    && mode == InputMode::KodiPlayback
                                    && matches!(action, InputAction::Home)
                                {
                                    let state = state.clone();
                                    runtime.spawn(async move {
                                        if let Err(error) =
                                            state.switch_surface(Surface::Ui).await
                                        {
                                            eprintln!("surface -> ui: {error}");
                                        }
                                    });
                                }
                            }
                            Ok(_) => {}
                            Err(error) => eprintln!("CEC receive: {error}"),
                        }
                    }
                })?,
        )
    } else {
        None
    };

    let unix = UnixListener::bind(&args.socket)?;
    let unix_task = tokio::spawn(serve_unix(unix, state.clone()));
    let mut web_tasks = Vec::new();
    if let Some(address) = args.http {
        let config = Arc::new(WebConfig {
            ui_root: ui_root.clone(),
            policy: PeerPolicy::LoopbackOnly,
        });
        let listener = TcpListener::bind(address).await?;
        web_tasks.push(tokio::spawn(serve_web(listener, state.clone(), config)));
    }
    if let Some(address) = args.lan_http {
        let config = Arc::new(WebConfig {
            ui_root: ui_root.clone(),
            policy: PeerPolicy::LoopbackAndPrivate,
        });
        let listener = TcpListener::bind(address).await?;
        eprintln!("mediaboxd-rs LAN arayüzü: http://{address}/");
        web_tasks.push(tokio::spawn(serve_web(listener, state.clone(), config)));
    }
    eprintln!("mediaboxd-rs hazır: {}", args.socket.display());

    // The display belongs to the interface until something asks for it.
    //
    // Which process owns the panel is this daemon's decision, and it was a
    // decision it never actually made at startup: it was made by whichever
    // unit systemd happened to have enabled. Kodi was, so a cold boot came up
    // in the player — not on the home screen — and stayed there until someone
    // pressed Home. The claim is made here instead, once the daemon is serving
    // and can be asked to hand the display over again.
    //
    // It is deliberately not fatal. A box with no TV-local interface installed
    // answers "not installed" and carries on as the control plane for whatever
    // else is on the screen.
    match state.switch_surface(Surface::Ui).await {
        Ok(_) => eprintln!("mediaboxd-rs: ekran arayüze verildi"),
        Err(error) => eprintln!("mediaboxd-rs: ekran arayüze verilemedi: {error}"),
    }

    shutdown_signal().await?;
    stop.store(true, Ordering::Relaxed);
    unix_task.abort();
    for task in web_tasks {
        task.abort();
    }
    if let Some(thread) = receiver {
        let _ = thread.join();
    }
    drop(state);
    let _ = std::fs::remove_file(&args.socket);
    Ok(())
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<(), Box<dyn std::error::Error>> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result?,
        _ = terminate.recv() => {},
    }
    Ok(())
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<(), Box<dyn std::error::Error>> {
    tokio::signal::ctrl_c().await?;
    Ok(())
}

fn prepare_socket(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        if socket_is_live(path) {
            return Err(format!("socket zaten canlı: {}", path.display()).into());
        }
        std::fs::remove_file(path)?;
    }
    Ok(())
}
