use clap::Parser;
use mediabox_cec::{Adapter, CecError, unavailable_status};
use mediabox_core::{CecStatus, InputAction, InputMode, InputSource, Surface};
use mediabox_input::InputManager;
use mediaboxd_rs::daemon::{AppState, CecRuntime, apply_kodi_route, serve_unix, socket_is_live};
use mediaboxd_rs::output::Output;
use mediaboxd_rs::fan::FanController;
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
    // Every CEC adapter on the board, not the one belonging to whatever was
    // plugged in when the daemon started. The kernel keeps one adapter per
    // HDMI transmitter and gives a physical address to the socket a sink is
    // plugged into; configured once as a playback device, an adapter claims
    // its logical address by itself whenever that happens. Choosing a single
    // adapter here, once, is what left a box that booted with the television
    // off -- or had its cable moved to the other socket -- with no CEC at all.
    let (adapters, unavailable) = if args.disable_cec {
        (
            Vec::new(),
            CecStatus {
                error: Some("CEC komut satırından kapatıldı".into()),
                ..Default::default()
            },
        )
    } else {
        let adapters = match args.cec_device.as_deref() {
            Some(path) => match Adapter::open(path) {
                Ok(adapter) => vec![adapter],
                Err(error) if args.require_cec => return Err(error.into()),
                Err(error) => {
                    eprintln!("CEC {}: {error}", path.display());
                    Vec::new()
                }
            },
            None => Adapter::open_all(),
        };
        if adapters.is_empty() && args.require_cec {
            return Err(CecError::NotFound.into());
        }
        let unavailable = unavailable_status(args.cec_device.as_deref(), &CecError::NotFound);
        (adapters, unavailable)
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
    let transitions = handovers.clone();
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
            adapters: adapters.clone(),
            unavailable,
            // A named adapter is an operator's decision and is where commands
            // go. Otherwise the selected output's, resolved at send time: the
            // television can be moved to the other socket between commands.
            target: match args.cec_device.clone() {
                Some(path) => Arc::new(move || mediaboxd_rs::cec::CecTarget::Adapter {
                    path: path.clone(),
                    expected_address: None,
                    confidence: mediabox_platform::Confidence::Exact,
                }),
                None => Arc::new(|| {
                    mediaboxd_rs::cec::target(&mediabox_platform::Platform::discover())
                }),
            },
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
        // The remembered colour mode. Nothing is applied here -- the link is
        // established by whatever draws the television -- so this only restores
        // the choice the player will read.
        output: Output::new(mediaboxd_rs::output::Paths::system(), || {
            // Hotplug recovery is one unit's job; this only asks for it when
            // the observer saw the sink change and udev's event may not have
            // arrived. The unit compares against its own record and does
            // nothing when the change was already handled.
            std::thread::spawn(|| {
                let _ = std::process::Command::new("/usr/bin/systemctl")
                    .args(["start", "--no-block", "mediabox-display-changed.service"])
                    .status();
            });
        }),
        fan: FanController::system(),
        ethernet: mediaboxd_rs::ethernet::Ethernet::system(),
    });
    // An address left on trial by a run of this daemon that did not finish
    // is taken back before anything else is asked of the network.
    state.ethernet.recover().await;

    // A display setting on trial belongs to the owner it was sent to.
    transitions.on_begin({
        let output = state.output.clone();
        move || output.owner_changing()
    });
    // The observer's snapshot, followed: level-triggered, once a second, so
    // a new generation -- a sink changed, a mode list read -- is acted on
    // whether or not anybody asks for the status.
    tokio::spawn({
        let output = state.output.clone();
        async move {
            let mut beat = tokio::time::interval(Duration::from_secs(1));
            loop {
                beat.tick().await;
                output.reconcile();
            }
        }
    });

    let stop = Arc::new(AtomicBool::new(false));
    // What wakes the receiver to stop. It sleeps in poll() with no timeout,
    // so without this it would only notice a shutdown on the next CEC message.
    // SAFETY: a plain eventfd, owned here and closed at exit.
    let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
    let receiver = if !adapters.is_empty() && wake >= 0 {
        let stop = stop.clone();
        let input = input.clone();
        let kodi = kodi.clone();
        let state = state.clone();
        let runtime = tokio::runtime::Handle::current();
        Some(
            std::thread::Builder::new()
                .name("mediabox-cec-rx".into())
                .spawn(move || {
                    // One wait on every adapter: the remote's keys arrive on
                    // whichever socket the television is on.
                    let mut waits: Vec<libc::pollfd> = adapters
                        .iter()
                        .map(|adapter| adapter.raw_fd())
                        .chain(std::iter::once(wake))
                        .map(|fd| libc::pollfd {
                            fd,
                            events: libc::POLLIN,
                            revents: 0,
                        })
                        .collect();
                    while !stop.load(Ordering::Relaxed) {
                        for wait in &mut waits {
                            wait.revents = 0;
                        }
                        // Asleep in the kernel until the television sends
                        // something or the daemon stops: no timeout, so an
                        // idle box never wakes this thread.
                        // SAFETY: the descriptors belong to adapters this
                        // thread holds for as long as it runs, and the wake fd.
                        let ready = unsafe {
                            libc::poll(waits.as_mut_ptr(), waits.len() as libc::nfds_t, -1)
                        };
                        if ready <= 0 {
                            continue;
                        }
                        for (adapter, wait) in adapters.iter().zip(&waits) {
                            if wait.revents & libc::POLLIN == 0 {
                                continue;
                            }
                            match adapter.receive(1) {
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
    if wake >= 0 {
        let one: u64 = 1;
        // SAFETY: eight bytes to the eventfd created above.
        unsafe { libc::write(wake, (&one as *const u64).cast(), 8) };
    }
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
