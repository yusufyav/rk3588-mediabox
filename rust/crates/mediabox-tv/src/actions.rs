//! One table, from a normalised action to what the interface does about it.
//!
//! This module exists because of a fault that reached the living room: a press
//! of Ok was followed, twenty-nine milliseconds later, by the appliance
//! restarting. The cause was not in this process — systemd-logind was watching
//! the HDMI block's CEC remote-control device, which carries the
//! `power-switch` udev tag, with `HandlePowerKey=poweroff` and
//! `HandleRebootKey=reboot`, so a code from the television's own remote reached
//! init instead of an application. That is fixed where it happens, in a udev
//! rule and a logind drop-in.
//!
//! What is fixed *here* is the second half of the same promise: that no path
//! through this interface can turn an ordinary press into a power decision.
//! Routing is therefore one pure function over (route, modal, action), with no
//! access to the control plane, and the invariants below are tests rather than
//! comments:
//!
//!   * Ok is never Power and never Restart.
//!   * Back is never Power and never Restart.
//!   * No direction ever reaches the machine.
//!   * The only action that can even *offer* a power decision is Power itself,
//!     and offering is not doing: every destructive answer is behind a
//!     confirmation whose focus starts on "no".

use mediabox_core::InputAction;

use crate::route::Route;

/// What the interface does with a press. Deliberately small and closed: a
/// screen interprets `Move`/`Select`/`Dismiss` in its own terms, and nothing
/// outside this file decides that an action means something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// A step in the focus grid: (dx, dy).
    Move(i32, i32),
    /// Ok.
    Select,
    /// Back.
    Dismiss,
    /// The Home key: back to the home screen from wherever this is.
    GoHome,
    /// Transport keys, which belong to whatever is playing.
    Transport(Transport),
    /// The Power key. Opens the power sheet; it never acts on its own.
    OfferPower,
    /// Not this interface's to answer.
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    PlayPause,
    Stop,
    Seek(i64),
    /// Straight to a second of the film. Only a scrub produces one: the keys
    /// on a remote cannot.
    SeekTo(u64),
    VolumeUp,
    VolumeDown,
    Mute,
}

/// What a decision would do to the machine. Used by the interface to draw the
/// right confirmation, and by the tests below to state the invariant in the
/// vocabulary the fault was reported in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEffect {
    /// Nothing outside this process.
    None,
    /// Ask the television to go to standby over CEC. The appliance keeps
    /// running; this is what the Power key on a television remote means.
    StandbyTelevision,
    Restart,
    Shutdown,
}

/// The one routing table.
///
/// `modal` is true while a sheet owns the remote — the source selector, the
/// power sheet, a confirmation. A modal takes the directions and Ok and Back
/// and nothing else, which is what keeps the screen behind it still.
pub fn intent_for(_route: Route, modal: bool, action: InputAction) -> Intent {
    // Directions, Ok and Back mean the same thing on every screen and in every
    // modal: they move and choose within whatever owns the remote. They are
    // listed once, before any screen-specific handling, so no screen can ever
    // give one of them a different meaning.
    match action {
        InputAction::Up => return Intent::Move(0, -1),
        InputAction::Down => return Intent::Move(0, 1),
        InputAction::Left => return Intent::Move(-1, 0),
        InputAction::Right => return Intent::Move(1, 0),
        InputAction::Ok => return Intent::Select,
        InputAction::Back => return Intent::Dismiss,
        _ => {}
    }

    // A sheet swallows everything else. Volume through a modal would be
    // defensible; Home through one would leave a sheet open over a screen that
    // is no longer there.
    if modal {
        return match action {
            InputAction::VolumeUp => Intent::Transport(Transport::VolumeUp),
            InputAction::VolumeDown => Intent::Transport(Transport::VolumeDown),
            InputAction::Mute => Intent::Transport(Transport::Mute),
            _ => Intent::Ignore,
        };
    }

    match action {
        InputAction::Home => Intent::GoHome,
        InputAction::Play | InputAction::Pause | InputAction::PlayPause => {
            Intent::Transport(Transport::PlayPause)
        }
        InputAction::Stop => Intent::Transport(Transport::Stop),
        InputAction::SeekForward => Intent::Transport(Transport::Seek(30)),
        InputAction::SeekBackward => Intent::Transport(Transport::Seek(-10)),
        InputAction::VolumeUp => Intent::Transport(Transport::VolumeUp),
        InputAction::VolumeDown => Intent::Transport(Transport::VolumeDown),
        InputAction::Mute => Intent::Transport(Transport::Mute),
        // Offering, not doing. The sheet this opens starts with the remote on
        // "Vazgeç", and the destructive rows need a second, deliberate press.
        InputAction::Power => Intent::OfferPower,
        // Answered above, before the modal gate. Listed rather than left to a
        // wildcard so an action added to the core vocabulary has to be given a
        // meaning here instead of silently doing nothing.
        InputAction::Up
        | InputAction::Down
        | InputAction::Left
        | InputAction::Right
        | InputAction::Ok
        | InputAction::Back => Intent::Ignore,
    }
}

/// What an intent, on its own, is permitted to do to the machine.
///
/// The answer is `None` for every intent there is. A power decision is not an
/// intent — it is a row on a sheet, chosen with `Select` while that sheet is
/// open and confirmed on a second sheet. This function is the statement that
/// no press can shortcut that.
pub fn effect_of(intent: Intent) -> SystemEffect {
    match intent {
        // Opening a sheet is drawing a rectangle.
        Intent::OfferPower => SystemEffect::None,
        _ => SystemEffect::None,
    }
}

/// Every action the appliance's vocabulary has. Used by the tests, and by the
/// diagnostics screen to show what the remote can send.
pub const ALL_ACTIONS: [InputAction; 17] = [
    InputAction::Up,
    InputAction::Down,
    InputAction::Left,
    InputAction::Right,
    InputAction::Ok,
    InputAction::Back,
    InputAction::Home,
    InputAction::Play,
    InputAction::Pause,
    InputAction::PlayPause,
    InputAction::Stop,
    InputAction::SeekForward,
    InputAction::SeekBackward,
    InputAction::VolumeUp,
    InputAction::VolumeDown,
    InputAction::Mute,
    InputAction::Power,
];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTES: [Route; 9] = [
        Route::Home,
        Route::Media,
        Route::Search,
        Route::Library,
        Route::Detail,
        Route::NowPlaying,
        Route::Settings,
        Route::Diagnostics,
        Route::Boot,
    ];

    /// The fault this module was written for: Ok restarted the appliance.
    #[test]
    fn ok_is_never_power_and_never_restart() {
        for route in ROUTES {
            for modal in [false, true] {
                let intent = intent_for(route, modal, InputAction::Ok);
                assert_eq!(intent, Intent::Select, "{route:?} modal={modal}");
                assert_ne!(intent, Intent::OfferPower);
                assert_eq!(effect_of(intent), SystemEffect::None);
            }
        }
    }

    #[test]
    fn back_is_never_power_and_never_restart() {
        for route in ROUTES {
            for modal in [false, true] {
                let intent = intent_for(route, modal, InputAction::Back);
                assert_eq!(intent, Intent::Dismiss, "{route:?} modal={modal}");
                assert_eq!(effect_of(intent), SystemEffect::None);
            }
        }
    }

    #[test]
    fn navigation_never_reaches_the_machine() {
        let directions = [
            (InputAction::Up, Intent::Move(0, -1)),
            (InputAction::Down, Intent::Move(0, 1)),
            (InputAction::Left, Intent::Move(-1, 0)),
            (InputAction::Right, Intent::Move(1, 0)),
        ];
        for route in ROUTES {
            for modal in [false, true] {
                for (action, expected) in directions {
                    let intent = intent_for(route, modal, action);
                    assert_eq!(intent, expected, "{route:?} modal={modal} {action:?}");
                    assert_eq!(effect_of(intent), SystemEffect::None);
                }
            }
        }
    }

    /// Only Power may even mention power, and what it does is open a sheet.
    #[test]
    fn only_the_power_key_offers_power() {
        for route in ROUTES {
            for modal in [false, true] {
                for action in ALL_ACTIONS {
                    let intent = intent_for(route, modal, action);
                    if intent == Intent::OfferPower {
                        assert_eq!(action, InputAction::Power, "{route:?} modal={modal}");
                        assert!(!modal, "a sheet must not open another sheet");
                    }
                }
            }
        }
    }

    /// No intent, on its own, touches the machine — including Power's.
    #[test]
    fn no_intent_has_a_system_effect() {
        for route in ROUTES {
            for modal in [false, true] {
                for action in ALL_ACTIONS {
                    assert_eq!(
                        effect_of(intent_for(route, modal, action)),
                        SystemEffect::None,
                        "{route:?} modal={modal} {action:?}"
                    );
                }
            }
        }
    }

    /// A sheet must not let Home pull the screen out from under it.
    #[test]
    fn a_modal_swallows_everything_but_movement_and_volume() {
        for route in ROUTES {
            assert_eq!(intent_for(route, true, InputAction::Home), Intent::Ignore);
            assert_eq!(intent_for(route, true, InputAction::Power), Intent::Ignore);
            assert_eq!(intent_for(route, true, InputAction::Stop), Intent::Ignore);
        }
    }

    /// Every action in the appliance's vocabulary is answered deliberately.
    #[test]
    fn every_action_is_routed() {
        for action in ALL_ACTIONS {
            let intent = intent_for(Route::Home, false, action);
            assert_ne!(intent, Intent::Ignore, "{action:?} falls through");
        }
    }
}
