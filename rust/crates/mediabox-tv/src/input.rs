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
    /// From the board's own infrared receiver, through rc-core.
    Infrared,
}

impl Origin {
    fn label(self) -> &'static str {
        match self {
            Origin::Keyboard => "wl",
            Origin::Bus => "sse",
            Origin::Infrared => "ir",
        }
    }
}

/// What an input device on the seat is, for reading it or not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device {
    /// The rc-core device the kernel registers for an HDMI transmitter's CEC
    /// adapter. The daemon already reads that remote from the adapter and
    /// publishes it normalised; reading it here too is every press twice.
    CecDuplicate,
    /// An infrared receiver's rc-core device: this interface's to read.
    Infrared,
    /// Anything else: a keyboard, a Bluetooth or USB remote, a mouse.
    Other,
}

/// Decide what a device is from where the kernel put it.
///
/// `path` is the event node's canonical sysfs path; `rc_protocols` the
/// `protocols` of the rc device it hangs under, when it hangs under one; and
/// `rc_driver` the driver of that rc device's parent. Both HDMI transmitters'
/// CEC and a `gpio-ir-receiver` register an rc device, so "under /rc/" is not
/// an answer: CEC is the rc device whose protocol is `cec`, or whose parent
/// is an HDMI transmitter. A name check is kept for a CEC device that is not
/// under rc-core at all -- the name this board's gives itself is
/// `dw_hdmi_qp`.
pub fn classify(path: &str, name: &str, rc_protocols: Option<&str>, rc_driver: Option<&str>) -> Device {
    let under_rc = path.contains("/rc/rc");
    let cec_protocol = rc_protocols.is_some_and(|protocols| {
        protocols
            .split_whitespace()
            .any(|protocol| protocol.trim_matches(|c| c == '[' || c == ']') == "cec")
    });
    let hdmi_parent = rc_driver.is_some_and(|driver| driver.contains("hdmi") || driver.contains("cec"));
    if cec_protocol
        || (under_rc && hdmi_parent)
        || name == "dw_hdmi_qp"
        || name.to_ascii_lowercase().contains("cec")
    {
        return Device::CecDuplicate;
    }
    if under_rc {
        return Device::Infrared;
    }
    Device::Other
}

/// A key code from a remote, as the appliance's vocabulary names it.
///
/// Every key a television remote has that a keyboard does not -- Back, Home,
/// Menu, the transport and the volume -- in the codes the kernel gives them
/// (include/uapi/linux/input-event-codes.h), whichever way the remote reaches
/// the box: a Bluetooth HID remote's consumer page, or an infrared remote
/// through the keymap in /etc/rc_keymaps. `keyboard` is false for the board's
/// infrared receiver, where the four arrows, Ok and Power are the remote's
/// too; on a keyboard those stay the keyboard's (typing, and no power key).
/// Whether an input node is a Bluetooth device's: its bus is
/// `BUS_BLUETOOTH` (5). The path is no answer -- a classic keyboard sits under
/// the controller (`.../bluetooth/hci0/hci0:3/...`), a Bluetooth LE one under
/// `/devices/virtual/misc/uhid/0005:...`; measured on the Plus with the ROG
/// AZOTH, which the path rule missed.
pub fn is_bluetooth(bustype: Option<&str>, path: &str) -> bool {
    match bustype.map(str::trim) {
        Some(bus) => u16::from_str_radix(bus, 16).is_ok_and(|bus| bus == 5),
        None => path.contains("/bluetooth/hci"),
    }
}

/// The device's own name without the role the kernel appends to each of its
/// input nodes ("MX Master 3S Keyboard", "... Mouse").
pub fn device_title(name: &str) -> String {
    let mut title = name.trim();
    for suffix in [" Consumer Control", " System Control", " Keyboard", " Mouse", " Keys"] {
        if let Some(rest) = title.strip_suffix(suffix) {
            title = rest.trim_end();
            break;
        }
    }
    if title.is_empty() { name.trim().to_string() } else { title.to_string() }
}

pub fn action_for_evdev(code: u32, keyboard: bool) -> Option<InputAction> {
    let remote_only = match code {
        158 | 174 => InputAction::Back,           // KEY_BACK, KEY_EXIT
        172 => InputAction::Home,                 // KEY_HOMEPAGE
        139 | 127 | 357 | 438 => InputAction::Menu, // KEY_MENU, KEY_COMPOSE, KEY_OPTION, KEY_CONTEXT_MENU
        352 | 353 => InputAction::Ok,             // KEY_OK, KEY_SELECT
        164 => InputAction::PlayPause,            // KEY_PLAYPAUSE
        207 | 200 => InputAction::Play,           // KEY_PLAY, KEY_PLAYCD
        201 => InputAction::Pause,                // KEY_PAUSECD
        166 | 128 => InputAction::Stop,           // KEY_STOPCD, KEY_STOP
        208 | 163 => InputAction::SeekForward,    // KEY_FASTFORWARD, KEY_NEXTSONG
        168 | 165 => InputAction::SeekBackward,   // KEY_REWIND, KEY_PREVIOUSSONG
        115 => InputAction::VolumeUp,
        114 => InputAction::VolumeDown,
        113 => InputAction::Mute,
        _ if keyboard => return None,
        103 => InputAction::Up,
        108 => InputAction::Down,
        105 => InputAction::Left,
        106 => InputAction::Right,
        28 | 96 => InputAction::Ok,               // KEY_ENTER, KEY_KPENTER
        1 => InputAction::Back,                   // KEY_ESC
        102 => InputAction::Home,                 // KEY_HOME
        116 => InputAction::Power,                // KEY_POWER: offers the sheet
        _ => return None,
    };
    Some(remote_only)
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
        Self {
            last_step: None,
            last: None,
            trace,
        }
    }

    /// Returns the action to act on, or None if this press should be ignored.
    pub fn accept(&mut self, action: InputAction, origin: Origin) -> Option<InputAction> {
        let now = Instant::now();

        // The same action, down the other road, within the window: one press of
        // one key that the appliance happens to hear twice. Only across
        // origins — two presses of Ok from the same keyboard are two presses,
        // however fast somebody is.
        if let Some((previous, from, at)) = self.last {
            if previous == action && from != origin && now.duration_since(at) < DEDUP_WINDOW {
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

/// Whether a held press of this repeats: moving and the volume, as on every
/// television. Ok, Back and Home do not -- a held Ok is a long press.
pub fn repeats(action: InputAction) -> bool {
    is_step(action) || matches!(action, InputAction::VolumeUp | InputAction::VolumeDown)
}

/// The same for a keyboard's keys, which arrive as text: the arrows, and
/// Backspace in a field.
pub fn text_repeats(text: &str) -> bool {
    is_backspace(text) || action_for_key(text).is_some_and(is_step)
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
///
/// Backspace is deliberately not here. It used to be Back, which on a screen
/// with a text field meant that deleting a mistyped character left the screen
/// — reported from the account form, where it threw away the address as well.
/// It is a deletion where there is something to delete and Back where there is
/// not, and only the screen knows which; `App::backspace` decides.
pub fn is_backspace(text: &str) -> bool {
    text.starts_with(char::from(slint::platform::Key::Backspace))
}

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
        c if c == named(Key::Home) => InputAction::Home,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_arrow_or_volume_repeats_and_a_held_ok_does_not() {
        for action in [InputAction::Left, InputAction::Right, InputAction::Up, InputAction::Down, InputAction::VolumeUp] {
            assert!(repeats(action), "{action:?}");
        }
        for action in [InputAction::Ok, InputAction::Back, InputAction::Home, InputAction::Mute] {
            assert!(!repeats(action), "{action:?}");
        }
        let key = |k: slint::platform::Key| char::from(k).to_string();
        assert!(text_repeats(&key(slint::platform::Key::RightArrow)));
        assert!(text_repeats(&key(slint::platform::Key::Backspace)));
        assert!(!text_repeats(&key(slint::platform::Key::Return)));
        assert!(!text_repeats("a"));
    }

    /// Reported from the account form: a mistyped character could not be
    /// deleted, because Backspace left the screen and took the address with it.
    #[test]
    fn backspace_is_not_a_navigation_key() {
        let backspace = char::from(slint::platform::Key::Backspace).to_string();
        assert!(is_backspace(&backspace));
        assert_eq!(action_for_key(&backspace), None);
        // Escape is still the way out, and the arrows still move.
        let escape = char::from(slint::platform::Key::Escape).to_string();
        assert!(!is_backspace(&escape));
        assert_eq!(action_for_key(&escape), Some(InputAction::Back));
        assert_eq!(
            action_for_key(&char::from(slint::platform::Key::LeftArrow).to_string()),
            Some(InputAction::Left)
        );
    }

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
            assert_eq!(
                dispatcher.accept(InputAction::Ok, first),
                Some(InputAction::Ok)
            );
            assert_eq!(dispatcher.accept(InputAction::Ok, second), None);
        }
    }

    #[test]
    fn back_is_deduplicated_the_same_way() {
        let mut dispatcher = Dispatcher::new(false);
        assert!(
            dispatcher
                .accept(InputAction::Back, Origin::Keyboard)
                .is_some()
        );
        assert!(dispatcher.accept(InputAction::Back, Origin::Bus).is_none());
    }

    /// Two deliberate presses of the same key on the same keyboard are two
    /// presses. A dedup that swallowed them would make a list unusable.
    #[test]
    fn two_presses_from_one_keyboard_are_two_presses() {
        let mut dispatcher = Dispatcher::new(false);
        assert!(
            dispatcher
                .accept(InputAction::Ok, Origin::Keyboard)
                .is_some()
        );
        assert!(
            dispatcher
                .accept(InputAction::Ok, Origin::Keyboard)
                .is_some()
        );
    }

    #[test]
    fn a_different_action_is_never_a_duplicate() {
        let mut dispatcher = Dispatcher::new(false);
        assert!(
            dispatcher
                .accept(InputAction::Ok, Origin::Keyboard)
                .is_some()
        );
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

#[cfg(test)]
mod device_tests {
    use super::*;

    #[test]
    fn a_bluetooth_device_is_known_by_its_path_and_named_once() {
        // Bluetooth LE through uhid, and classic under the controller.
        assert!(is_bluetooth(Some("0005\n"), "/sys/devices/virtual/misc/uhid/0005:0B05:1A85.0003/input/input12/event12"));
        assert!(is_bluetooth(None, "/sys/devices/platform/fe2c0000.serial/serial1/serial1-0/bluetooth/hci0/hci0:3/0005:046D:B034.0001/input/input12/event7"));
        // USB, the CEC rc device, the board's own Bluetooth power key.
        assert!(!is_bluetooth(Some("0003"), "/sys/devices/platform/fc880000.usb/usb3/3-1/3-1:1.0/0003:046D:C52B.0001/input/input5/event5"));
        assert!(!is_bluetooth(Some("0019"), "/sys/devices/platform/wireless-bluetooth/input/input7/event7"));
        assert!(!is_bluetooth(None, "/sys/devices/platform/fdea0000.hdmi/rc/rc1/input1/event1"));
        assert_eq!(device_title("MX Master 3S Keyboard"), "MX Master 3S");
        assert_eq!(device_title("MX Master 3S Mouse"), "MX Master 3S");
        assert_eq!(device_title("ROG AZOTH"), "ROG AZOTH");
        assert_eq!(device_title("Keyboard"), "Keyboard");
    }

    #[test]
    fn the_cec_rc_device_is_a_duplicate_and_the_ir_receiver_is_not() {
        // Measured on the Plus, 2026-09-27: both transmitters' CEC under
        // rc-core with protocol [cec], parent driver dwhdmi-rockchip; the
        // receiver under the overlay's platform device, driver gpio_ir_recv.
        let cec = "/sys/devices/platform/fdea0000.hdmi/rc/rc1/input1/event1";
        assert_eq!(classify(cec, "dw_hdmi_qp", Some("[cec]"), Some("dwhdmi-rockchip")), Device::CecDuplicate);
        assert_eq!(classify(cec, "anything", Some("[cec]"), None), Device::CecDuplicate);
        assert_eq!(classify(cec, "anything", None, Some("dwhdmi-rockchip")), Device::CecDuplicate);
        let ir = "/sys/devices/platform/mediabox-ir-receiver/rc/rc2/input11/event11";
        assert_eq!(
            classify(ir, "gpio_ir_recv", Some("rc-5 nec [nec] rc-6"), Some("gpio_ir_recv")),
            Device::Infrared
        );
        let keyboard = "/sys/devices/virtual/misc/uhid/0005:1D5A:C081.0003/input/input12/event12";
        assert_eq!(classify(keyboard, "UR-02", None, None), Device::Other);
    }

    #[test]
    fn a_remote_s_keys_are_actions_and_a_keyboard_keeps_its_own() {
        assert_eq!(action_for_evdev(158, true), Some(InputAction::Back));
        assert_eq!(action_for_evdev(172, true), Some(InputAction::Home));
        assert_eq!(action_for_evdev(139, true), Some(InputAction::Menu));
        assert_eq!(action_for_evdev(164, true), Some(InputAction::PlayPause));
        assert_eq!(action_for_evdev(115, true), Some(InputAction::VolumeUp));
        assert_eq!(action_for_evdev(352, true), Some(InputAction::Ok));
        // On a keyboard the arrows, Enter, Escape and Power are the keyboard's.
        for code in [103, 108, 105, 106, 28, 1, 116] {
            assert_eq!(action_for_evdev(code, true), None, "{code}");
        }
        // On the infrared receiver they are the remote's; Power only offers.
        assert_eq!(action_for_evdev(103, false), Some(InputAction::Up));
        assert_eq!(action_for_evdev(28, false), Some(InputAction::Ok));
        assert_eq!(action_for_evdev(116, false), Some(InputAction::Power));
        assert_eq!(action_for_evdev(30, false), None, "a letter is not a remote key");
    }
}
