//! Non-exclusive Linux input discovery and normalized event routing.
//!
//! The daemon deliberately does not EVIOCGRAB devices. Kodi continues to own its
//! physical keyboard/remote path. CEC and API events enter the shared bus here.

use mediabox_core::{InputAction, InputEvent, InputMode, InputSource};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    Keyboard,
    ConsumerControl,
    RemoteControl,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputDevice {
    pub path: PathBuf,
    pub name: String,
    pub class: DeviceClass,
    pub source: InputSource,
    pub grabbed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KodiRoute {
    PlayPause,
    Play,
    Pause,
    Stop,
    Seek(i64),
    VolumeUp,
    VolumeDown,
    Mute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    PublishOnly,
    Kodi(KodiRoute),
}

pub fn route(mode: InputMode, action: InputAction) -> RouteDecision {
    if mode == InputMode::Ui {
        return RouteDecision::PublishOnly;
    }
    match action {
        InputAction::PlayPause => RouteDecision::Kodi(KodiRoute::PlayPause),
        InputAction::Play => RouteDecision::Kodi(KodiRoute::Play),
        InputAction::Pause => RouteDecision::Kodi(KodiRoute::Pause),
        InputAction::Stop => RouteDecision::Kodi(KodiRoute::Stop),
        InputAction::SeekForward => RouteDecision::Kodi(KodiRoute::Seek(30)),
        InputAction::SeekBackward => RouteDecision::Kodi(KodiRoute::Seek(-10)),
        InputAction::VolumeUp => RouteDecision::Kodi(KodiRoute::VolumeUp),
        InputAction::VolumeDown => RouteDecision::Kodi(KodiRoute::VolumeDown),
        InputAction::Mute => RouteDecision::Kodi(KodiRoute::Mute),
        // Direction/OK/Back remain on the event bus. Physical USB/BT/CEC evdev
        // devices remain ungrabbed, so production Kodi receives them directly.
        _ => RouteDecision::PublishOnly,
    }
}

#[derive(Clone)]
pub struct InputManager {
    mode: Arc<RwLock<InputMode>>,
    sender: broadcast::Sender<InputEvent>,
}

impl InputManager {
    pub fn new(mode: InputMode) -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            mode: Arc::new(RwLock::new(mode)),
            sender,
        }
    }

    pub fn mode(&self) -> InputMode {
        *self.mode.read().expect("input mode lock")
    }
    pub fn set_mode(&self, mode: InputMode) {
        *self.mode.write().expect("input mode lock") = mode;
    }
    pub fn subscribe(&self) -> broadcast::Receiver<InputEvent> {
        self.sender.subscribe()
    }

    pub fn publish(
        &self,
        action: InputAction,
        source: InputSource,
        pressed: bool,
        timestamp_ns: Option<u64>,
    ) -> RouteDecision {
        let event = InputEvent {
            action,
            source,
            pressed,
            timestamp_ns: timestamp_ns.unwrap_or_else(now_ns),
        };
        let _ = self.sender.send(event);
        if pressed {
            route(self.mode(), action)
        } else {
            RouteDecision::PublishOnly
        }
    }
}

pub fn enumerate() -> Vec<InputDevice> {
    enumerate_at(Path::new("/dev/input"), Path::new("/sys/class/input"))
}

pub fn enumerate_at(dev_root: &Path, sys_root: &Path) -> Vec<InputDevice> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(dev_root) else {
        return result;
    };
    for entry in entries.filter_map(Result::ok) {
        let file = entry.file_name();
        let name = file.to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        let base = sys_root.join(&file).join("device");
        let label = read_trimmed(&base.join("name")).unwrap_or_else(|| name.to_string());
        let keys = read_trimmed(&base.join("capabilities/key")).unwrap_or_default();
        let canonical = std::fs::canonicalize(&base).unwrap_or(base);
        let sys_path = canonical.to_string_lossy();
        let lower = label.to_ascii_lowercase();
        let is_cec =
            lower.contains("cec") || lower.contains("dw_hdmi") || sys_path.contains("/rc/");
        let class = classify(&label, &keys, is_cec);
        let source = if is_cec {
            InputSource::Cec
        } else if lower.contains("bluetooth") || lower.contains(" bt ") {
            InputSource::BluetoothHid
        } else {
            InputSource::UsbHid
        };
        result.push(InputDevice {
            path: entry.path(),
            name: label,
            class,
            source,
            grabbed: false,
        });
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    result
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn classify(name: &str, key_bitmap: &str, is_cec: bool) -> DeviceClass {
    let lower = name.to_ascii_lowercase();
    if is_cec || lower.contains("remote") {
        return DeviceClass::RemoteControl;
    }
    let words = parse_bitmap(key_bitmap);
    let has = |code: usize| {
        words
            .get(code / 64)
            .is_some_and(|word| word & (1u64 << (code % 64)) != 0)
    };
    if has(30) && has(44) {
        DeviceClass::Keyboard
    } else if [113usize, 114, 115, 164, 166].into_iter().any(has) {
        DeviceClass::ConsumerControl
    } else {
        DeviceClass::Unknown
    }
}

fn parse_bitmap(value: &str) -> Vec<u64> {
    // sysfs prints the highest word first. Reverse so bit N is words[N/64].
    value
        .split_whitespace()
        .rev()
        .filter_map(|word| u64::from_str_radix(word, 16).ok())
        .collect()
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_preserves_navigation_and_routes_media_in_playback_mode() {
        assert_eq!(
            route(InputMode::Ui, InputAction::Stop),
            RouteDecision::PublishOnly
        );
        assert_eq!(
            route(InputMode::KodiPlayback, InputAction::Up),
            RouteDecision::PublishOnly
        );
        assert_eq!(
            route(InputMode::KodiPlayback, InputAction::Stop),
            RouteDecision::Kodi(KodiRoute::Stop)
        );
        assert_eq!(
            route(InputMode::KodiPlayback, InputAction::SeekBackward),
            RouteDecision::Kodi(KodiRoute::Seek(-10))
        );
    }

    #[test]
    fn manager_broadcasts_without_grabbing() {
        let manager = InputManager::new(InputMode::Ui);
        let mut rx = manager.subscribe();
        manager.publish(InputAction::Ok, InputSource::Api, true, Some(42));
        let event = rx.try_recv().unwrap();
        assert_eq!(event.action, InputAction::Ok);
        assert_eq!(event.timestamp_ns, 42);
    }

    #[test]
    fn classifies_known_device_shapes() {
        assert_eq!(classify("dw_hdmi_qp", "", true), DeviceClass::RemoteControl);
        assert_eq!(classify("CEC Remote", "", true), DeviceClass::RemoteControl);
        let mut words = [0u64; 3];
        words[0] |= 1 << 30;
        words[0] |= 1 << 44;
        let sysfs = words
            .iter()
            .rev()
            .map(|v| format!("{v:x}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            classify("USB Keyboard", &sysfs, false),
            DeviceClass::Keyboard
        );
    }
}
