//! The television's interface.
//!
//! It is a client of mediaboxd-rs and of nothing else. The control plane owns
//! which application holds the display, the media core owns what is playable
//! and how, and Kodi owns playback; this process draws, listens to the remote,
//! and asks.

mod images;
mod input;
mod metrics;
mod model;
mod rpc;
mod state;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mediabox_core::{InputAction, InputEvent};
use slint::ComponentHandle;

slint::include_modules!();

/// Where the daemon publishes normalised input. The television asks for `tv=1`
/// because navigation actions are only sent to the client that says it is the
/// one in the living room — without it a phone on the network moves in lockstep
/// with whoever holds the remote.
const EVENTS_URL: &str = "http://127.0.0.1:8787/v1/events?tv=1";

const CACHE_DIR: &str = "/var/lib/mediabox-ui/tv-imgcache";

/// The appliance is the only place this runs in earnest, and there the defaults
/// above are right. The overrides exist so the interface can be driven against
/// a forwarded socket from a developer's machine — which is how a shelf layout
/// gets looked at without a deploy.
fn socket_path() -> String {
    std::env::var("MEDIABOX_TV_SOCKET").unwrap_or_else(|_| rpc::DEFAULT_SOCKET.to_string())
}

fn events_url() -> String {
    std::env::var("MEDIABOX_TV_EVENTS").unwrap_or_else(|_| EVENTS_URL.to_string())
}

fn cache_dir() -> String {
    std::env::var("MEDIABOX_TV_CACHE").unwrap_or_else(|_| CACHE_DIR.to_string())
}

thread_local! {
    /// How the background threads reach the interface. They post a closure to
    /// the event loop and it finds everything here, because nothing the
    /// interface owns is safe to send across a thread.
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&mut App)) {
    let app = APP.with(|slot| slot.borrow().clone());
    if let Some(app) = app {
        f(&mut app.borrow_mut());
    }
}

struct App {
    window: slint::Weak<MediaBoxWindow>,
    home: state::Home,
    images: images::ImageManager,
    meter: metrics::Metrics,
    dispatcher: input::Dispatcher,
}

impl App {
    /// A press that survived the dispatcher.
    fn act(&mut self, action: InputAction) {
        let moved = match action {
            InputAction::Up => self.home.step(0, -1),
            InputAction::Down => self.home.step(0, 1),
            InputAction::Left => self.home.step(-1, 0),
            InputAction::Right => self.home.step(1, 0),
            _ => false,
        };

        if moved {
            self.meter.key_accepted();
            self.paint();
        }
    }

    /// Both answers, or whichever of them arrived.
    ///
    /// The library is the appliance's own and is quick; the catalogues are a
    /// fan-out across third-party hosts. They are asked for together and
    /// whichever lands first is drawn, because a home screen that waits for the
    /// slowest addon is a home screen nobody sees.
    fn loaded(&mut self, home: Option<model::HomeRows>, library: Option<model::LibraryListing>) {
        self.meter.data_arrived();

        let rows = home.unwrap_or(model::HomeRows { rows: Vec::new() });
        let shelves = state::shelves_from(&rows, library.as_ref());

        if shelves.is_empty() {
            self.fail("Hiçbir raf getirilemedi.");
            return;
        }

        self.home.set_shelves(shelves);

        if let Some(window) = self.window.upgrade() {
            window.set_rails(slint::ModelRc::from(self.home.rails.clone()));
            window.set_screen("home".into());
        }
        self.paint();
    }

    fn fail(&mut self, why: &str) {
        if let Some(window) = self.window.upgrade() {
            window.set_failed(true);
            window.set_status(why.into());
        }
    }

    /// Everything the focus decides: which tiles carry a picture, what the hero
    /// says, and where the shelves sit. The Slint side animates between the
    /// values this writes; it does not decide any of them.
    fn paint(&mut self) {
        let Some(window) = self.window.upgrade() else { return };

        self.home.sync_artwork(&mut self.images);

        if let Some((art, second_layer)) = self.home.backdrop(&mut self.images) {
            if second_layer {
                window.set_art_b(art);
            } else {
                window.set_art_a(art);
            }
        }
        window.set_fade(self.home.fade);

        let (title, facts, summary) = self.home.hero();
        window.set_hero_title(title.into());
        window.set_hero_meta(facts.into());
        window.set_hero_summary(summary.into());

        window.set_focus_row(self.home.row as i32);
        window.set_focus_col(self.home.column() as i32);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let trace_input = !std::env::args().any(|a| a == "--quiet-input");

    let window = MediaBoxWindow::new()?;

    let app = Rc::new(RefCell::new(App {
        window: window.as_weak(),
        home: state::Home::new(),
        images: images::ImageManager::new(cache_dir(), || {
            let _ = slint::invoke_from_event_loop(|| {
                with_app(|app| {
                    if app.images.collect() {
                        app.paint();
                    }
                })
            });
        }),
        meter: metrics::Metrics::new(started),
        dispatcher: input::Dispatcher::new(trace_input),
    }));
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));

    // Frames are counted from the renderer rather than from a timer, so the
    // number is what was actually presented and not what we hoped for. It also
    // means the idle CPU figure this reports is not inflated by the thing
    // reporting it.
    {
        let handle = window.as_weak();
        let reported = std::cell::Cell::new(false);
        window
            .window()
            .set_rendering_notifier(move |state, _| {
                if !matches!(state, slint::RenderingState::AfterRendering) {
                    return;
                }
                with_app(|app| app.meter.frame());
                if !reported.replace(true) {
                    if let Some(window) = handle.upgrade() {
                        report_surface(&window);
                    }
                }
            })
            .expect("this renderer cannot report when it has drawn");
    }

    // Keys from the compositor: USB and Bluetooth keyboards. The remote does
    // not come this way — its input device is switched off in the compositor's
    // config, because the daemon is the authority for it.
    window.on_key_pressed(move |text| {
        let Some(action) = input::action_for_key(text.as_str()) else { return };
        with_app(|app| {
            if let Some(action) = app.dispatcher.accept(action, input::Origin::Keyboard) {
                app.act(action);
            }
        });
    });

    // Everything the daemon normalises — the CEC remote, the phone remote, an
    // injected action — arrives on its event stream.
    spawn_bus_listener();
    spawn_loader();

    let reporter = slint::Timer::default();
    reporter.start(slint::TimerMode::Repeated, Duration::from_secs(5), || {
        with_app(|app| {
            let held = app.images.held_mb();
            if let Some(line) = app.meter.report_due() {
                eprintln!("{line} art_cpu_mb={held}");
            }
        });
    });

    window.set_status("Raflar getiriliyor…".into());
    window.run()?;
    Ok(())
}

/// The panel, the buffer and the scale, as this process sees them.
///
/// This is half of the evidence that the interface is not being scaled twice:
/// the other half is the display controller's own state, which is read on the
/// appliance. A logical size smaller than the physical one is expected and
/// correct — that is the whole point of the integer scale — but a *buffer*
/// smaller than the panel's mode would mean wlroots is stretching us, and the
/// picture would be soft and the direct flip refused.
fn report_surface(window: &MediaBoxWindow) {
    let w = window.window();
    let physical = w.size();
    let scale = w.scale_factor();
    eprintln!(
        "mediabox-tv.surface buffer={}x{} scale={:.3} logical={:.0}x{:.0} env_scale={}",
        physical.width,
        physical.height,
        scale,
        physical.width as f32 / scale,
        physical.height as f32 / scale,
        std::env::var("SLINT_SCALE_FACTOR").unwrap_or_else(|_| "unset".into()),
    );
}

/// Asks the control plane for the home surface.
///
/// The two calls go out together on purpose: the library is this appliance's
/// own and answers in milliseconds, while the catalogues are a fan-out over
/// third-party hosts. Waiting for the second before drawing the first is time
/// the viewer spends looking at a name and a spinner.
fn spawn_loader() {
    std::thread::Builder::new()
        .name("mediabox-tv-data".into())
        .spawn(|| {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build()
            else {
                return;
            };

            runtime.block_on(async {
                let client = rpc::Client::new(socket_path());

                let at = Instant::now();
                let (home, library) = tokio::join!(client.home(), client.library());
                eprintln!("mediabox-tv.load media_home+library_ms={}", at.elapsed().as_millis());

                let home = match home {
                    Ok(home) => Some(home),
                    Err(e) => {
                        eprintln!("mediabox-tv.load home failed: {e}");
                        None
                    }
                };
                let library = match library {
                    Ok(library) => Some(library),
                    Err(e) => {
                        eprintln!("mediabox-tv.load library failed: {e}");
                        None
                    }
                };

                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|app| app.loaded(home, library));
                });
            });
        })
        .expect("the loader thread could not be started");
}

/// Reads the daemon's event stream for as long as the process lives,
/// reconnecting when it ends. The daemon is restarted independently of this
/// interface, so a dropped stream is ordinary rather than fatal.
fn spawn_bus_listener() {
    std::thread::Builder::new()
        .name("mediabox-tv-bus".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(e) => {
                    eprintln!("mediabox-tv.bus no runtime: {e}");
                    return;
                }
            };

            runtime.block_on(async move {
                loop {
                    if let Err(e) = read_events().await {
                        eprintln!("mediabox-tv.bus disconnected: {e}");
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        })
        .expect("the input thread could not be started");
}

async fn read_events() -> Result<(), Box<dyn std::error::Error>> {
    let mut response = reqwest::Client::new().get(events_url()).send().await?.error_for_status()?;

    // Frames are newline-delimited and small; assembling them here avoids
    // pulling a stream adapter crate in for four lines of work.
    let mut pending = String::new();
    while let Some(chunk) = response.chunk().await? {
        pending.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(at) = pending.find('\n') {
            let line: String = pending.drain(..=at).collect();
            deliver(line.trim_end());
        }
    }

    Ok(())
}

fn deliver(line: &str) {
    // Keepalives arrive as comment frames and carry no payload.
    let Some(payload) = line.strip_prefix("data: ") else { return };
    let Ok(event) = serde_json::from_str::<InputEvent>(payload) else { return };

    // Only the press edge. Through CEC the press, the hold and the release each
    // arrive as their own event.
    if !event.pressed {
        return;
    }

    let action = event.action;
    let _ = slint::invoke_from_event_loop(move || {
        with_app(|app| {
            if let Some(action) = app.dispatcher.accept(action, input::Origin::Bus) {
                app.act(action);
            }
        });
    });
}
