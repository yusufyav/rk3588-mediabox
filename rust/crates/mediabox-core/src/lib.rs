//! MediaBox control plane wire types. These types are the stable local API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    Offline,
    Idle,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputAction {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Home,
    Play,
    Pause,
    PlayPause,
    Stop,
    SeekForward,
    SeekBackward,
    VolumeUp,
    VolumeDown,
    Mute,
    Power,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    #[default]
    Ui,
    KodiPlayback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    Cec,
    UsbHid,
    BluetoothHid,
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputEvent {
    pub action: InputAction,
    pub source: InputSource,
    pub pressed: bool,
    pub timestamp_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CecDevice {
    pub logical_address: u8,
    pub physical_address: Option<String>,
    pub device_type: Option<String>,
    pub last_seen_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CecEvent {
    pub initiator: u8,
    pub destination: u8,
    pub opcode: Option<u8>,
    pub operands: Vec<u8>,
    pub timestamp_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CecErrorCounters {
    pub receive: u64,
    pub transmit: u64,
    pub arbitration_lost: u64,
    pub nack: u64,
    pub low_drive: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CecStatus {
    pub available: bool,
    pub adapter: Option<String>,
    pub driver: Option<String>,
    pub capabilities: Vec<String>,
    pub physical_address: Option<String>,
    pub logical_addresses: Vec<u8>,
    pub known_devices: Vec<CecDevice>,
    pub last_rx: Option<CecEvent>,
    pub last_tx: Option<CecEvent>,
    pub errors: CecErrorCounters,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KodiStatus {
    pub running: bool,
    pub jsonrpc_reachable: bool,
    pub state: PlaybackState,
    pub active_player_id: Option<i64>,
    pub item: Option<Value>,
    pub speed: Option<i64>,
    pub time: Option<Value>,
    pub total_time: Option<Value>,
    pub error: Option<String>,
}

/// Which process owns the appliance display. Only one may hold DRM master, so
/// this is a single authoritative value rather than a set of flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// Kodi is on the television.
    Kodi,
    /// The MediaBox product UI is on the television.
    Ui,
    /// Neither owns the display; the console is free.
    #[default]
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceStatus {
    pub active: Surface,
    pub kodi_active: bool,
    pub ui_active: bool,
    /// False when the TV-local UI unit is not installed, which is the normal
    /// state on an appliance driven only from a LAN browser.
    pub ui_installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceHealth {
    pub name: String,
    pub healthy: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemStatus {
    pub version: String,
    pub hostname: String,
    pub kernel: String,
    pub architecture: String,
    pub uptime_seconds: u64,
    pub input_mode: InputMode,
    pub input_devices: Vec<Value>,
    pub services: Vec<ServiceHealth>,
    pub kodi: KodiStatus,
    pub cec: CecStatus,
    pub media: Value,
    pub surface: SurfaceStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Status,
    System,
    Diagnostics,
    KodiStatus,
    KodiPlayPause,
    KodiStop,
    KodiSeek { seconds: i64 },
    KodiOpen { url: String, resume_seconds: u64 },
    KodiRestart,
    CecStatus,
    CecDevices,
    CecActiveSource,
    CecWakeTv,
    CecStandbyTv,
    MediaStatus,
    MediaCapabilities,
    MediaHome,
    MediaCatalog {
        media_type: String,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        addon_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    MediaMeta { media_type: String, id: String },
    MediaSubtitles {
        media_type: String,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video_id: Option<String>,
    },
    MediaLibrary,
    MediaLibraryItem { id: String },
    MediaResolve { stream: Value },
    MediaStreamPlan { stream: Value },
    MediaSearch { query: String },
    MediaInspect { url: String },
    MediaStreams { media_type: String, id: String },
    MediaPolicy { url: String },
    MediaSessions,
    MediaSessionStart { url: String },
    MediaSessionStop { id: String },
    /// The one authoritative "play this on the television" path. The daemon
    /// creates the media session, takes the display back from the UI and opens
    /// the result in Kodi; no caller reproduces that ordering itself.
    MediaPlayOnKodi {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<Value>,
        #[serde(default)]
        start_seconds: u64,
    },
    SurfaceStatus,
    SurfaceSwitch { target: Surface },
    InputInject { action: InputAction },
    InputMonitor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl Response {
    pub fn success<T: Serialize>(value: T) -> Self {
        Self {
            ok: true,
            result: Some(serde_json::to_value(value).expect("serializable response")),
            error: None,
        }
    }

    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(ApiError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_input_action_round_trips() {
        let actions = [
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
        for action in actions {
            let json = serde_json::to_string(&action).unwrap();
            assert_eq!(serde_json::from_str::<InputAction>(&json).unwrap(), action);
        }
    }

    #[test]
    fn unknown_commands_are_rejected() {
        assert!(serde_json::from_str::<Request>(r#"{"command":"shell","argv":["id"]}"#).is_err());
    }
}
