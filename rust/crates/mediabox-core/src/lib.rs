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

/// One application this appliance can put on the television.
///
/// MediaBox is a light environment for an Orange Pi rather than a media player
/// with a settings page, so what the box can do is a table rather than a fixed
/// pair of choices. Kodi and the product UI are simply the first two rows; a
/// browser, a screen receiver or anything else that comes later is a row in a
/// file, not a new branch in this enum — which is why the display owner stopped
/// being an enum at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Application {
    /// Stable name used by the API and remembered by the UI. Lower-case ASCII.
    pub id: String,
    /// What a person sees in the launcher.
    pub name: String,
    /// One short line under that name. Never a sentence about the product.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The systemd unit that is this application. mediaboxd starts and stops
    /// the unit and never spawns the program itself: a child of this daemon
    /// inherits this daemon's sandbox, and that has already cost this project
    /// an application with no input devices.
    pub unit: String,
    /// True when running it means taking the display from whatever holds it.
    /// Only one such application may run, because only one process may hold
    /// DRM master.
    #[serde(default = "yes")]
    pub owns_display: bool,
}

fn yes() -> bool {
    true
}

impl Application {
    /// A unit name is passed to systemctl, so it is checked before it is ever
    /// used rather than trusted because it came from a file on this machine.
    pub fn valid(&self) -> bool {
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 32
            && self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        let unit_ok = self.unit.ends_with(".service")
            && self.unit.len() <= 64
            && self
                .unit
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"@._-".contains(&byte));
        id_ok && unit_ok && !self.name.is_empty()
    }
}

/// An application as it stands right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationStatus {
    #[serde(flatten)]
    pub application: Application,
    /// False when the unit is not on this machine. A launcher shows what the
    /// box can actually do, so an entry that is not installed says so rather
    /// than failing when it is chosen.
    pub installed: bool,
    pub active: bool,
}

/// What the television is showing, and what else it could show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayStatus {
    /// The id of the application holding the display, or none when the console
    /// is free.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub applications: Vec<ApplicationStatus>,
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
    /// Play it here, in the interface's own player.
    ///
    /// The film opens as a window of the interface rather than as another
    /// application taking the television: the catalogue stays behind it and
    /// Back returns to it. The daemon creates the media session and points the
    /// player at the loopback address the worker serves it on, because the
    /// player is built against the appliance's Rockchip ffmpeg and that build
    /// has no TLS.
    MediaPlayHere {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<Value>,
        #[serde(default)]
        start_seconds: u64,
    },
    /// Hand what is playing here to Kodi, at the position it had reached.
    ///
    /// The equivalent of "send to an external player": the same film, the same
    /// second, in the application that is better at the rest of the evening.
    MediaHandoffToKodi,
    /// Stop the interface's own player, if one is running.
    MediaStopHere,
    /// Sign in to a Stremio account. The password is used once, forwarded to
    /// the media core and never written down; what is kept is the auth key.
    MediaLogin { email: String, password: String },
    MediaLogout,
    SurfaceStatus,
    SurfaceSwitch { target: Surface },
    /// What the box can run, and what it is running.
    Applications,
    /// Put one application on the television. The id `idle` releases the
    /// display without starting anything.
    ApplicationLaunch { id: String },
    /// Open one web address in the television's browser.
    ///
    /// The browser has no address bar — a remote cannot use one — so the
    /// address is chosen in this interface and handed over here: the daemon
    /// leaves it where the browser reads it at start, then gives the browser
    /// the display.
    BrowserOpen { url: String },
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
