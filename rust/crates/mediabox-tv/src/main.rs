//! The television's interface.
//!
//! It is a client of mediaboxd-rs and of nothing else. The control plane owns
//! which application holds the display, the media core owns what is playable
//! and how, and Kodi owns playback; this process draws, listens to the remote,
//! and asks.

mod input;
mod metrics;
mod model;
mod rpc;

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

thread_local! {
    /// How the input thread reaches the interface. It posts a closure to the
    /// event loop, and the closure finds the handler here rather than carrying
    /// it: everything the handler touches lives on this thread and is not Send.
    static HANDLER: RefCell<Option<Rc<dyn Fn(InputAction, input::Origin)>>> =
        const { RefCell::new(None) };
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let trace_input = !std::env::args().any(|a| a == "--quiet-input");

    let window = MediaBoxWindow::new()?;

    let meter = Rc::new(RefCell::new(metrics::Metrics::new(started)));
    let dispatcher = Rc::new(RefCell::new(input::Dispatcher::new(trace_input)));

    // Frames are counted from the renderer rather than from a timer, so the
    // number is what was actually presented and not what we hoped for. It also
    // means the idle CPU figure this reports is not inflated by the thing
    // reporting it.
    {
        let meter = meter.clone();
        let handle = window.as_weak();
        let reported = std::cell::Cell::new(false);
        window
            .window()
            .set_rendering_notifier(move |state, _| {
                if !matches!(state, slint::RenderingState::AfterRendering) {
                    return;
                }
                meter.borrow_mut().frame();
                if !reported.replace(true) {
                    if let Some(window) = handle.upgrade() {
                        report_surface(&window);
                    }
                }
            })
            .expect("this renderer cannot report when it has drawn");
    }

    // One handler, two roads into it.
    let handler: Rc<dyn Fn(InputAction, input::Origin)> = {
        let handle = window.as_weak();
        let dispatcher = dispatcher.clone();
        let meter = meter.clone();
        Rc::new(move |action, origin| {
            let Some(window) = handle.upgrade() else { return };
            if let Some(action) = dispatcher.borrow_mut().accept(action, origin) {
                meter.borrow_mut().key_accepted();
                apply(&window, action);
            }
        })
    };
    HANDLER.with(|slot| *slot.borrow_mut() = Some(handler.clone()));

    // Keys from the compositor: USB and Bluetooth keyboards. The remote does
    // not come this way — its input device is switched off in the compositor's
    // config, because the daemon is the authority for it.
    window.on_key_pressed(move |text| {
        if let Some(action) = input::action_for_key(text.as_str()) {
            handler(action, input::Origin::Keyboard);
        }
    });

    // Everything the daemon normalises — the CEC remote, the phone remote, an
    // injected action — arrives on its event stream.
    spawn_bus_listener();

    // The only periodic work in the process.
    let reporter = slint::Timer::default();
    {
        let meter = meter.clone();
        reporter.start(slint::TimerMode::Repeated, Duration::from_secs(5), move || {
            if let Some(line) = meter.borrow_mut().report_due() {
                eprintln!("{line}");
            }
        });
    }

    window.set_status("Denetim düzlemine bağlanılıyor…".into());
    window.run()?;
    Ok(())
}

/// What a press does. In phase one there is one screen and nothing to move
/// between; the dispatcher above is what is being proven, so the screen reports
/// what reached it.
fn apply(window: &MediaBoxWindow, action: InputAction) {
    window.set_status(format!("{action:?}").into());
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
    let mut response = reqwest::Client::new().get(EVENTS_URL).send().await?.error_for_status()?;

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
        let handler = HANDLER.with(|slot| slot.borrow().clone());
        if let Some(handler) = handler {
            handler(action, input::Origin::Bus);
        }
    });
}
