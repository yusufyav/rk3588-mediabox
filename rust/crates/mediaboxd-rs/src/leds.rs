//! The two indicator lights on the board, and remembering what they were told.
//!
//! Three things made this a module rather than two `echo`s in a script.
//!
//! The first is that `/sys/class/leds` lists more than the board's own lights.
//! On this appliance it also carries the `input*::capslock` family — the LEDs
//! of whatever USB keyboard is plugged in — and an `mmc0::` entry that is not a
//! light at all. Turning "the LEDs" off by walking that directory would darken
//! somebody's keyboard. The board's own lights are the ones whose device path
//! goes through `platform/gpio-leds`, so that is what [`discover`] looks for,
//! and everything else in the directory is left alone.
//!
//! The second is that a trigger overrides a brightness. The device tree gives
//! both lights `linux,default-trigger = "heartbeat"`, and while that trigger is
//! in place the kernel rewrites the brightness a few times a second: writing 0
//! to `brightness` under it changes the value for about as long as it takes to
//! read it back. Every mode here therefore writes the trigger first.
//!
//! The third is that a choice made from the sofa has to survive a power cut.
//! The mode is written to a small file under the daemon's state directory and
//! applied again when the daemon starts, which is why this is the daemon's job
//! and not the interface's: the unit that draws the television mounts /sys
//! read-only, and `/sys/class/leds` is root's either way.
//!
//! What this module cannot do is the red light. It is wired to the supply, not
//! to a pin, and appears nowhere in the device tree — see [`mediabox_core::LedMode`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mediabox_core::{LedMode, LedStatus};

/// Where the kernel lists every LED class device.
pub const SYSFS_LEDS: &str = "/sys/class/leds";

/// Where the chosen mode is remembered. Under the daemon's `StateDirectory`,
/// so systemd creates it with the right owner and mode before the daemon runs.
pub const STATE_FILE: &str = "/var/lib/mediabox/leds";

/// The board's own lights, the mode they were last told, and the file that
/// remembers it.
pub struct LedController {
    /// Absolute paths of the LED directories, not their names: the name is for
    /// showing and the path is for writing.
    leds: Vec<PathBuf>,
    state: PathBuf,
    mode: Mutex<LedMode>,
    /// Why there are no lights to control, when there are none.
    error: Option<String>,
}

impl LedController {
    /// Finds the board's lights, restores the remembered mode, and applies it.
    ///
    /// A failure to apply is not a failure to start. The appliance's job is to
    /// put a film on a television; an indicator light that stayed lit because
    /// sysfs refused a write is worth a line in the status and nothing more.
    pub fn new(sysfs_root: impl AsRef<Path>, state: impl Into<PathBuf>) -> Self {
        let state = state.into();
        let (leds, error) = match discover(sysfs_root.as_ref()) {
            Ok(leds) if leds.is_empty() => (
                Vec::new(),
                Some(format!(
                    "{} içinde gpio-leds'e bağlı ışık yok",
                    sysfs_root.as_ref().display()
                )),
            ),
            Ok(leds) => (leds, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };

        // No remembered choice is not the same as a remembered "off": on a box
        // that has never been told, the lights keep doing whatever the device
        // tree asked for, and this reads what that is rather than assuming.
        // Reporting a mode the lights are not in is worse than having no
        // opinion: the settings screen would name one thing and the board
        // would show another.
        let remembered = read_state(&state);
        let mode = remembered.or_else(|| observe(&leds)).unwrap_or_default();
        let controller = Self {
            leds,
            state,
            mode: Mutex::new(mode),
            error,
        };
        if let Some(mode) = remembered {
            let _ = controller.write(mode);
        }
        controller
    }

    /// The controller as it is on the appliance.
    pub fn system() -> Self {
        Self::new(SYSFS_LEDS, STATE_FILE)
    }

    pub fn available(&self) -> bool {
        !self.leds.is_empty()
    }

    pub fn mode(&self) -> LedMode {
        *self.mode.lock().expect("led mode mutex")
    }

    pub fn status(&self) -> LedStatus {
        LedStatus {
            available: self.available(),
            mode: self.mode(),
            leds: self
                .leds
                .iter()
                .filter_map(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect(),
            error: self.error.clone(),
        }
    }

    /// Applies a mode and remembers it.
    ///
    /// The order matters in both halves: sysfs first so the lights change even
    /// if the state directory is unwritable, and the state file second so a
    /// remembered mode is one that was actually reached.
    pub fn set(&self, mode: LedMode) -> Result<LedStatus, String> {
        if !self.available() {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "bu kartta denetlenebilir ışık yok".into()));
        }
        self.write(mode).map_err(|error| error.to_string())?;
        *self.mode.lock().expect("led mode mutex") = mode;
        if let Err(error) = write_state(&self.state, mode) {
            // The lights are already right; only the memory of it failed. Say
            // so rather than reporting a failure that did not happen.
            return Err(format!(
                "ışıklar {} yapıldı ama kalıcı kaydedilemedi: {error}",
                mode.label()
            ));
        }
        Ok(self.status())
    }

    /// Writes one mode to every light, trigger before brightness.
    fn write(&self, mode: LedMode) -> io::Result<()> {
        let mut failure = None;
        for led in &self.leds {
            if let Err(error) = write_one(led, mode) {
                // Carry on to the other light: one light that refuses a write
                // is not a reason to leave the other one pulsing.
                failure.get_or_insert(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn write_one(led: &Path, mode: LedMode) -> io::Result<()> {
    fs::write(led.join("trigger"), mode.trigger())?;
    if let Some(level) = mode.brightness() {
        fs::write(led.join("brightness"), level.to_string())?;
    }
    Ok(())
}

/// The board's own lights: the LED class devices that hang off the
/// `gpio-leds` platform device.
///
/// Everything else in `/sys/class/leds` belongs to something that is not this
/// board — a keyboard's lock lights, an mmc entry with no light behind it —
/// and is deliberately not returned.
pub fn discover(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        // Every entry here is a symlink into /sys/devices; the real path is
        // what says which device the light belongs to.
        let Ok(real) = path.canonicalize() else {
            continue;
        };
        if !real
            .components()
            .any(|part| part.as_os_str() == "gpio-leds")
        {
            continue;
        }
        if !real.join("trigger").is_file() {
            continue;
        }
        found.push(real);
    }
    // read_dir order is whatever the filesystem says. Sort so the status
    // reads the same on every call.
    found.sort();
    Ok(found)
}

/// What the lights are doing right now, read back out of sysfs.
///
/// Used on a box that has never been told anything, so that the status
/// describes the board rather than a default. The first light answers for both:
/// they are only ever written together.
///
/// `trigger` reads as a list with the active one in brackets — `none
/// [heartbeat] timer …` — so the answer is the bracketed word, and a trigger
/// other than `none` means the kernel owns the level.
fn observe(leds: &[PathBuf]) -> Option<LedMode> {
    let led = leds.first()?;
    let trigger = fs::read_to_string(led.join("trigger")).ok()?;
    let active = trigger
        .split_whitespace()
        .find_map(|word| word.strip_prefix('[')?.strip_suffix(']'))
        .unwrap_or("none");
    if active == "heartbeat" {
        return Some(LedMode::Heartbeat);
    }
    if active != "none" {
        // Some other trigger — disk activity, a timer, whatever a previous
        // session set. Not one of this setting's three modes, so it is not
        // claimed as one.
        return None;
    }
    let brightness = fs::read_to_string(led.join("brightness")).ok()?;
    match brightness.trim() {
        "0" => Some(LedMode::Off),
        _ => Some(LedMode::On),
    }
}

fn read_state(path: &Path) -> Option<LedMode> {
    let text = fs::read_to_string(path).ok()?;
    match text.trim() {
        "off" => Some(LedMode::Off),
        "on" => Some(LedMode::On),
        "heartbeat" => Some(LedMode::Heartbeat),
        _ => None,
    }
}

fn write_state(path: &Path, mode: LedMode) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = match mode {
        LedMode::Off => "off",
        LedMode::On => "on",
        LedMode::Heartbeat => "heartbeat",
    };
    fs::write(path, format!("{text}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board, as sysfs presents one: real directories under `devices`, and
    /// `class/leds` pointing at them.
    struct Board {
        _dir: tempfile::TempDir,
        class: PathBuf,
        state: PathBuf,
        devices: PathBuf,
    }

    impl Board {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().to_path_buf();
            let class = root.join("class/leds");
            let devices = root.join("devices/platform");
            fs::create_dir_all(&class).unwrap();
            fs::create_dir_all(&devices).unwrap();
            Self {
                _dir: dir,
                class,
                state: root.join("state/leds"),
                devices,
            }
        }

        /// A light under some platform device, linked into class/leds the way
        /// the kernel links it.
        fn light(&self, parent: &str, name: &str) -> PathBuf {
            let real = self.devices.join(parent).join("leds").join(name);
            fs::create_dir_all(&real).unwrap();
            fs::write(real.join("trigger"), "none [heartbeat]\n").unwrap();
            fs::write(real.join("brightness"), "1\n").unwrap();
            std::os::unix::fs::symlink(&real, self.class.join(name)).unwrap();
            real
        }

        fn controller(&self) -> LedController {
            LedController::new(&self.class, &self.state)
        }

        fn read(path: &Path, field: &str) -> String {
            fs::read_to_string(path.join(field))
                .unwrap()
                .trim()
                .to_string()
        }
    }

    /// The fault this filter exists to prevent: darkening a keyboard.
    #[test]
    fn only_the_boards_own_lights_are_touched() {
        let board = Board::new();
        let blue = board.light("gpio-leds", "blue_led");
        let green = board.light("gpio-leds", "green_led");
        let keyboard = board.light("usb1/1-1/input8", "input8::capslock");

        let controller = board.controller();
        assert!(controller.available());
        assert_eq!(controller.status().leds, vec!["blue_led", "green_led"]);

        controller.set(LedMode::Off).expect("off");
        assert_eq!(Board::read(&blue, "brightness"), "0");
        assert_eq!(Board::read(&green, "brightness"), "0");
        // Untouched: still whatever it was before.
        assert_eq!(Board::read(&keyboard, "brightness"), "1");
        assert_eq!(Board::read(&keyboard, "trigger"), "none [heartbeat]");
    }

    /// The trigger has to go first, or the kernel keeps rewriting brightness.
    #[test]
    fn the_trigger_is_cleared_before_the_brightness() {
        let board = Board::new();
        let blue = board.light("gpio-leds", "blue_led");
        board.controller().set(LedMode::Off).expect("off");
        assert_eq!(Board::read(&blue, "trigger"), "none");
        assert_eq!(Board::read(&blue, "brightness"), "0");
    }

    #[test]
    fn heartbeat_hands_the_level_back_to_the_kernel() {
        let board = Board::new();
        let blue = board.light("gpio-leds", "blue_led");
        let controller = board.controller();
        controller.set(LedMode::On).expect("on");
        assert_eq!(Board::read(&blue, "brightness"), "1");
        controller.set(LedMode::Heartbeat).expect("heartbeat");
        assert_eq!(Board::read(&blue, "trigger"), "heartbeat");
        // Not written: under a trigger the level is the kernel's.
        assert_eq!(Board::read(&blue, "brightness"), "1");
    }

    /// The whole point of the state file.
    #[test]
    fn the_choice_survives_a_restart() {
        let board = Board::new();
        let blue = board.light("gpio-leds", "blue_led");
        board.controller().set(LedMode::Off).expect("off");

        // As if the box had been power-cycled: the kernel put the device
        // tree's trigger back, and a fresh daemon starts.
        fs::write(blue.join("trigger"), "none [heartbeat]\n").unwrap();
        fs::write(blue.join("brightness"), "1\n").unwrap();

        let restarted = board.controller();
        assert_eq!(restarted.mode(), LedMode::Off);
        assert_eq!(Board::read(&blue, "trigger"), "none");
        assert_eq!(Board::read(&blue, "brightness"), "0");
    }

    /// A box that has never been told anything keeps the device tree's pulse.
    #[test]
    fn nothing_remembered_changes_nothing() {
        let board = Board::new();
        let blue = board.light("gpio-leds", "blue_led");
        let controller = board.controller();
        assert_eq!(controller.mode(), LedMode::Heartbeat);
        assert_eq!(Board::read(&blue, "trigger"), "none [heartbeat]");
    }

    /// Caught on the appliance: with no state file the daemon reported
    /// "heartbeat" while both lights were dark, because it assumed the device
    /// tree's trigger instead of reading the board.
    #[test]
    fn with_nothing_remembered_the_mode_is_read_off_the_board() {
        for (trigger, brightness, expected) in [
            ("[none]", "0", LedMode::Off),
            ("[none]", "1", LedMode::On),
            ("none [heartbeat]", "1", LedMode::Heartbeat),
        ] {
            let board = Board::new();
            let blue = board.light("gpio-leds", "blue_led");
            fs::write(blue.join("trigger"), format!("{trigger}\n")).unwrap();
            fs::write(blue.join("brightness"), format!("{brightness}\n")).unwrap();

            let controller = board.controller();
            assert_eq!(
                controller.mode(),
                expected,
                "trigger={trigger} brightness={brightness}"
            );
            // Reading is not writing: nothing was applied.
            assert_eq!(Board::read(&blue, "trigger"), trigger);
            assert_eq!(Board::read(&blue, "brightness"), brightness);
        }
    }

    /// A trigger this setting does not offer is not relabelled as one of its
    /// three modes.
    #[test]
    fn an_unrelated_trigger_is_not_claimed_as_a_mode() {
        let board = Board::new();
        let blue = board.light("gpio-leds", "blue_led");
        fs::write(blue.join("trigger"), "none heartbeat [mmc0]\n").unwrap();
        assert_eq!(observe(&[blue]), None);
        // Falls back to the default rather than lying about it.
        assert_eq!(board.controller().mode(), LedMode::default());
    }

    /// Every board that is not this one.
    #[test]
    fn a_board_with_no_gpio_lights_says_so_instead_of_offering_a_choice() {
        let board = Board::new();
        board.light("usb1/1-1/input8", "input8::capslock");
        let controller = board.controller();
        assert!(!controller.available());
        assert!(controller.status().error.is_some());
        assert!(controller.set(LedMode::Off).is_err());
    }

    #[test]
    fn the_modes_step_round_a_ring_starting_at_off() {
        assert_eq!(LedMode::default(), LedMode::Off);
        assert_eq!(LedMode::Off.next(), LedMode::On);
        assert_eq!(LedMode::On.next(), LedMode::Heartbeat);
        assert_eq!(LedMode::Heartbeat.next(), LedMode::Off);
    }
}
