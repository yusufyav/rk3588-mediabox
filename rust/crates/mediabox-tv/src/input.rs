//! Where a press comes from, and why there is exactly one road for each.
//!
//! The appliance has two input paths and they overlap. The kernel's CEC driver
//! registers a real input device as well as talking to the daemon, so one press
//! of Right on the television remote can arrive twice: once through the
//! compositor as an ordinary key, and once through mediaboxd-rs as a normalised
//! action. Acting on both walks the focus two tiles.
//!
//! The daemon stays the authority for normalised input — CEC, the phone remote
//! and anything injected over the API all come from its bus — so the split is
//! made by source rather than by guessing:
//!
//!   * CEC remote, browser remote, injected actions -> the daemon's event stream
//!   * USB and Bluetooth keyboards                  -> wl_keyboard, directly
//!
//! The CEC input device is switched off in the compositor's config, which is
//! what makes the first column true. The dedup below is the second lock on the
//! same door: if a build ever reaches a television where that rule did not
//! apply, the duplicate is dropped here instead of being felt by the viewer.

use std::time::{Duration, Instant};

use mediabox_core::InputAction;

/// A television remote does not send one event per press. Through CEC the
/// press, the hold and the release each arrive, and one deliberate press of
/// Right used to walk the focus three or four tiles along. Only the four
/// directions are gated: a second OK must never be dropped.
const STEP_INTERVAL: Duration = Duration::from_millis(110);

/// How long after an action from the daemon the same action arriving as a key
/// is treated as the same press.
const DEDUP_WINDOW: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// From the compositor, as an ordinary key.
    Keyboard,
    /// From mediaboxd-rs, already normalised.
    Bus,
}

impl Origin {
    fn label(self) -> &'static str {
        match self {
            Origin::Keyboard => "wl",
            Origin::Bus => "sse",
        }
    }
}

pub struct Dispatcher {
    last_step: Option<Instant>,
    last_bus: Option<(InputAction, Instant)>,
    /// Logs every accepted and rejected press. The gate in phase one is a count
    /// of these lines, so they are not debug output — they are the evidence.
    trace: bool,
}

impl Dispatcher {
    pub fn new(trace: bool) -> Self {
        Self { last_step: None, last_bus: None, trace }
    }

    /// Returns the action to act on, or None if this press should be ignored.
    pub fn accept(&mut self, action: InputAction, origin: Origin) -> Option<InputAction> {
        let now = Instant::now();

        if origin == Origin::Keyboard {
            if let Some((previous, at)) = self.last_bus {
                if previous == action && now.duration_since(at) < DEDUP_WINDOW {
                    self.log("drop-duplicate", action, origin);
                    return None;
                }
            }
        } else {
            self.last_bus = Some((action, now));
        }

        if is_step(action) {
            if let Some(at) = self.last_step {
                if now.duration_since(at) < STEP_INTERVAL {
                    self.log("drop-repeat", action, origin);
                    return None;
                }
            }
            self.last_step = Some(now);
        }

        self.log("accept", action, origin);
        Some(action)
    }

    fn log(&self, verdict: &str, action: InputAction, origin: Origin) {
        if self.trace {
            eprintln!(
                "mediabox-tv.input {} action={:?} source={}",
                verdict,
                action,
                origin.label()
            );
        }
    }
}

fn is_step(action: InputAction) -> bool {
    matches!(
        action,
        InputAction::Up | InputAction::Down | InputAction::Left | InputAction::Right
    )
}

/// What the compositor sends us, in Slint's encoding, mapped onto the same
/// vocabulary the daemon uses. Anything not listed is not a navigation key and
/// is left to whatever has focus.
pub fn action_for_key(text: &str) -> Option<InputAction> {
    use slint::platform::Key;

    let key = text.chars().next()?;

    let named = |k: Key| -> char {
        // Slint encodes its named keys as single private-use characters.
        char::from(k)
    };

    Some(match key {
        c if c == named(Key::UpArrow) => InputAction::Up,
        c if c == named(Key::DownArrow) => InputAction::Down,
        c if c == named(Key::LeftArrow) => InputAction::Left,
        c if c == named(Key::RightArrow) => InputAction::Right,
        c if c == named(Key::Return) => InputAction::Ok,
        '\n' | '\r' => InputAction::Ok,
        c if c == named(Key::Escape) => InputAction::Back,
        c if c == named(Key::Backspace) => InputAction::Back,
        c if c == named(Key::Home) => InputAction::Home,
        _ => return None,
    })
}
