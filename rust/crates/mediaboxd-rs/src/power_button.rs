//! The board's own power button turns the appliance off.
//!
//! systemd-logind is told to ignore every power key on this box
//! (packaging/systemd/logind.conf.d/10-mediabox.conf): the television's
//! remote, a keyboard's system-control endpoint and the Wi-Fi module's
//! `bt-powerkey` all carry KEY_POWER, and one of them once turned the box off
//! with nobody near it. So the one button that is meant to be a power button
//! is read here, by the name its driver gives it, and nothing else is.
//!
//! The PMIC's button is "rk805 pwrkey" on the boards this ships on: the
//! rk805 driver serves the RK806 as well (measured on the Plus,
//! /proc/bus/input/devices). The device is not grabbed. A press asks for the
//! same shutdown Settings asks for, through the daemon's own request.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// The kernel's name for the PMIC's power button.
pub const BOARD_BUTTON: &str = "rk805 pwrkey";

const EV_KEY: u16 = 0x01;
const KEY_POWER: u16 = 116;
/// `struct input_event` on a 64-bit kernel: a 16-byte timeval, then type,
/// code and value.
const EVENT_SIZE: usize = 24;

/// The board button's event node, found by name under `sys_root`
/// (/sys/class/input) and given under `dev_root` (/dev/input).
pub fn find(sys_root: &Path, dev_root: &Path) -> Option<PathBuf> {
    let mut nodes: Vec<String> = std::fs::read_dir(sys_root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("event"))
        .collect();
    nodes.sort();
    nodes.into_iter().find_map(|node| {
        let name = std::fs::read_to_string(sys_root.join(&node).join("device/name")).ok()?;
        (name.trim() == BOARD_BUTTON).then(|| dev_root.join(&node))
    })
}

/// Whether one input event is the power key going down. Key repeat (2) and
/// release (0) are not presses.
pub fn is_press(event: &[u8; EVENT_SIZE]) -> bool {
    let kind = u16::from_ne_bytes([event[16], event[17]]);
    let code = u16::from_ne_bytes([event[18], event[19]]);
    let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
    kind == EV_KEY && code == KEY_POWER && value == 1
}

/// Reads the board button for as long as it is there, calling `pressed` on
/// each press. Blocking; meant for a thread of its own.
pub fn watch(path: &Path, mut pressed: impl FnMut()) -> io::Result<()> {
    let mut device = File::open(path)?;
    let mut event = [0u8; EVENT_SIZE];
    loop {
        device.read_exact(&mut event)?;
        if is_press(&event) {
            pressed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u16, code: u16, value: i32) -> [u8; EVENT_SIZE] {
        let mut bytes = [0u8; EVENT_SIZE];
        bytes[16..18].copy_from_slice(&kind.to_ne_bytes());
        bytes[18..20].copy_from_slice(&code.to_ne_bytes());
        bytes[20..24].copy_from_slice(&value.to_ne_bytes());
        bytes
    }

    #[test]
    fn only_the_power_key_going_down_is_a_press() {
        assert!(is_press(&event(EV_KEY, KEY_POWER, 1)));
        assert!(!is_press(&event(EV_KEY, KEY_POWER, 0)));
        assert!(!is_press(&event(EV_KEY, KEY_POWER, 2)));
        assert!(!is_press(&event(0, 0, 0)));
        assert!(!is_press(&event(EV_KEY, 28, 1)));
    }

    #[test]
    fn the_board_button_is_found_by_its_name_and_no_other_power_key_is() {
        let root = std::env::temp_dir().join(format!("mediabox-power-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (node, name) in [
            ("event0", "dw_hdmi_qp"),
            ("event7", "bt-powerkey"),
            ("event9", "rk805 pwrkey"),
            ("event12", "ROG AZOTH Keyboard"),
        ] {
            let dir = root.join(node).join("device");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("name"), format!("{name}\n")).unwrap();
        }
        assert_eq!(
            find(&root, Path::new("/dev/input")),
            Some(PathBuf::from("/dev/input/event9"))
        );
        std::fs::remove_dir_all(root.join("event9")).unwrap();
        assert_eq!(find(&root, Path::new("/dev/input")), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
