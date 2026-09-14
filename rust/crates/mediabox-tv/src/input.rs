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
//!   * USB and Bluetooth keyboards                  -> libinput, directly
//!
//! The first column used to be true because the CEC input device was switched
//! off in the compositor's configuration. There is no compositor any more, and
//! taking it away quietly opened that door again: the platform's own libinput
//! reads every device on the seat, `rc0` included, so one press of the
//! television remote arrived twice — once as an ordinary key and once
//! normalised.
//!
//! So there are two locks now. The platform skips the remote-control devices by
//! identity (see `platform::remote_control`), and the dispatcher below drops a
//! duplicate whichever road it comes down first. The second lock matters: the
//! original was one-way — it only dropped a *key* that followed a *bus* action —
//! and the remote is faster through libinput than through the daemon, so the
//! order on the appliance was the other one. Directions survived that because
//! they are also rate-limited; Ok did not, and a single press of Ok on a source
//! list chose a source and closed the sheet in the same breath.

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
    /// The last press this dispatcher acted on, and where it came from.
    last: Option<(InputAction, Origin, Instant)>,
    /// Logs every accepted and rejected press. The gate in phase one is a count
    /// of these lines, so they are not debug output — they are the evidence.
    trace: bool,
}

impl Dispatcher {
    pub fn new(trace: bool) -> Self {
        Self { last_step: None, last: None, trace }
    }

    /// Returns the action to act on, or None if this press should be ignored.
    pub fn accept(&mut self, action: InputAction, origin: Origin) -> Option<InputAction> {
        let now = Instant::now();

        // The same action, down the other road, within the window: one press of
        // one key that the appliance happens to hear twice. Only across
        // origins — two presses of Ok from the same keyboard are two presses,
        // however fast somebody is.
        if let Some((previous, from, at)) = self.last {
            if previous == action
                && from != origin
                && now.duration_since(at) < DEDUP_WINDOW
            {
                self.log("drop-duplicate", action, origin);
                return None;
            }
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

        self.last = Some((action, origin, now));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The fault this dispatcher was rewritten for: one press of Ok on the
    /// television remote reached the interface twice, and the second one chose
    /// whatever the first had just opened.
    #[test]
    fn one_press_heard_twice_is_one_press_whichever_road_is_first() {
        for (first, second) in [
            (Origin::Keyboard, Origin::Bus),
            (Origin::Bus, Origin::Keyboard),
        ] {
            let mut dispatcher = Dispatcher::new(false);
            assert_eq!(dispatcher.accept(InputAction::Ok, first), Some(InputAction::Ok));
            assert_eq!(dispatcher.accept(InputAction::Ok, second), None);
        }
    }

    #[test]
    fn back_is_deduplicated_the_same_way() {
        let mut dispatcher = Dispatcher::new(false);
        assert!(dispatcher.accept(InputAction::Back, Origin::Keyboard).is_some());
        assert!(dispatcher.accept(InputAction::Back, Origin::Bus).is_none());
    }

    /// Two deliberate presses of the same key on the same keyboard are two
    /// presses. A dedup that swallowed them would make a list unusable.
    #[test]
    fn two_presses_from_one_keyboard_are_two_presses() {
        let mut dispatcher = Dispatcher::new(false);
        assert!(dispatcher.accept(InputAction::Ok, Origin::Keyboard).is_some());
        assert!(dispatcher.accept(InputAction::Ok, Origin::Keyboard).is_some());
    }

    #[test]
    fn a_different_action_is_never_a_duplicate() {
        let mut dispatcher = Dispatcher::new(false);
        assert!(dispatcher.accept(InputAction::Ok, Origin::Keyboard).is_some());
        assert!(dispatcher.accept(InputAction::Back, Origin::Bus).is_some());
    }

    /// A television remote sends the press, the hold and the release, and one
    /// deliberate press of Right used to walk the focus three tiles.
    #[test]
    fn a_held_direction_steps_once() {
        let mut dispatcher = Dispatcher::new(false);
        assert!(dispatcher.accept(InputAction::Right, Origin::Bus).is_some());
        assert!(dispatcher.accept(InputAction::Right, Origin::Bus).is_none());
    }

    /// Nothing in the keyboard map is a power key. The other half of this is in
    /// platform::key_text, which has no code for one.
    #[test]
    fn no_key_becomes_power_or_home_by_accident() {
        assert_eq!(action_for_key("q"), None);
        assert_eq!(action_for_key(""), None);
    }
}
