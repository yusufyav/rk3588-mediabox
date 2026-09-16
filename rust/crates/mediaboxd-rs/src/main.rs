use clap::Parser;
use mediabox_cec::{Adapter, unavailable_status};
use mediabox_core::{CecStatus, InputAction, InputMode, InputSource, Surface};
use mediabox_input::InputManager;
use mediaboxd_rs::daemon::{AppState, CecRuntime, apply_kodi_route, serve_unix, socket_is_live};
use mediaboxd_rs::kodi::KodiClient;
use mediaboxd_rs::leds::LedController;
use mediaboxd_rs::lifecycle::{ApplicationManager, KodiLifecycle, SurfaceManager};
use mediaboxd_rs::media::MediaClient;
use mediaboxd_rs::player::PlayerManager;
use mediaboxd_rs::transition::DisplayTransition;
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
    /// Which CEC adapter to hold. Left out — which is how the product runs —
    /// the adapter belonging to the display output the box is actually on is
    /// used. Naming one is for a board being debugged, not for an install.
    #[arg(long)]
    cec_device: Option<PathBuf>,
    #[arg(long)]
    disable_cec: bool,
    #[arg(long)]
    require_cec: bool,
    #[arg(long, value_enum, default_value_t = ModeArg::Ui)]
    input_mode: ModeArg,
}

/// The CEC adapter on the selected display output, if this board has one
/// there.
///
/// `None` is a normal answer and not a fault: DisplayPort carries no CEC, and
/// a box with nothing plugged in has no selected output to ask about. The
/// caller falls back to plain adapter discovery, and the daemon reports CEC as
/// unavailable if that finds nothing either.
fn discovered_cec_adapter() -> Option<PathBuf> {
    let platform = mediabox_platform::Platform::discover();
    for warning in &platform.warnings {
        eprintln!("mediaboxd-rs.platform {warning}");
    }
    let output = platform.selected_output()?;
    let adapter = output.cec.as_ref()?;
    eprintln!(
        "mediaboxd-rs: CEC {} ({} üzerinden)",
        adapter.device.display(),
        output.connector.name
    );
    Some(adapter.device.clone())
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
        // Which adapter is this television's is a topology question, not a
        // numbering one. A board with two HDMI transmitters has two adapters,
        // and the lowest-numbered one is the socket nothing is plugged into as
        // often as it is the right one. So the adapter is the one that belongs
        // to the output the display stage selected — the same choice the
        // television interface makes, from the same resolver.
        let wanted = args.cec_device.clone().or_else(discovered_cec_adapter);
        match Adapter::discover(wanted.as_deref()) {
            Ok(adapter) => (Some(adapter), CecStatus::default()),
            Err(error) if args.require_cec => return Err(error.into()),
            Err(error) => (None, unavailable_status(wanted.as_deref(), &error)),
        }
    };
    let kodi = Arc::new(KodiClient::new(
        &args.kodi_endpoint,
        Duration::from_secs(2),
    )?);
    let ui_root = match args.ui_root {
        Some(root) => Some(
            root.canonicalize()
                .map_err(|error| format!("--ui-root okunamadı ({}): {error}", root.display()))?,
        ),
        None => None,
    };
    // One gate for every change of display ownership, shared by both managers
    // and readable from outside the daemon by `mediabox-display-guard`. Two
    // of these would be two gates, which is the bug it exists to prevent.
    let handovers = DisplayTransition::new();
    let state = Arc::new(AppState {
        kodi: kodi.clone(),
        lifecycle: KodiLifecycle::new(&args.kodi_unit)?,
        surface: SurfaceManager::new(&args.kodi_unit, &args.ui_unit, handovers.clone())?,
        player: Arc::new(PlayerManager::new(
            "/opt/rk3588-mediabox/bin/mediabox-player",
            "/run/mediabox/player.sock",
        )),
        applications: ApplicationManager::load(
            args.applications.as_deref(),
            &args.kodi_unit,
            &args.ui_unit,
            handovers,
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
        // Constructed here rather than lazily, because constructing it is what
        // re-applies the remembered mode: the kernel puts the device tree's
        // heartbeat back on every boot, and this is the earliest the daemon can
        // take it off again.
        leds: LedController::system(),
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
                                        if let Err(error) = state.switch_surface(Surface::Ui).await
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

    // The display goes to whoever this board is set to come up as.
    //
    // Which process owns the panel is this daemon's decision, and it was a
    // decision it never actually made at startup: it was made by whichever
    // unit systemd happened to have enabled. Kodi was, so a cold boot came up
    // in the player — not on the home screen — and stayed there until someone
    // pressed Home. The claim is made here instead, once the daemon is serving
    // and can be asked to hand the display over again.
    //
    // But the claim used to be unconditional, and on a board that also carries
    // rk3588-screenbridge that was a second wrong answer in the other
    // direction: a capture appliance that had been doing its job for months
    // became a media appliance the moment MediaBox was installed on it, and
    // then again at every boot, with nobody having chosen that. So the answer
    // is read from the recorded preference, which defaults to the other
    // product — see mediaboxd_rs::owner for why that is the safe way round.
    //
    // It is deliberately not fatal. A box with no TV-local interface installed
    // answers "not installed" and carries on as the control plane for whatever
    // else is on the screen.
    match mediaboxd_rs::owner::read() {
        mediaboxd_rs::owner::DisplayOwner::MediaBox => {
            match state.switch_surface(Surface::Ui).await {
                Ok(_) => eprintln!("mediaboxd-rs: ekran arayüze verildi (tercih: mediabox)"),
                Err(error) => eprintln!("mediaboxd-rs: ekran arayüze verilemedi: {error}"),
            }
        }
        mediaboxd_rs::owner::DisplayOwner::ScreenBridge => {
            eprintln!(
                "mediaboxd-rs: ekran bu kartta screenbridge'in (tercih: screenbridge, {}); \
                 değiştirmek için: mediaboxctl display-owner set mediabox",
                mediaboxd_rs::owner::OWNER_FILE
            );
        }
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
