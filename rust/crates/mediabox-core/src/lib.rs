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

/// What the two indicator lights on the board are doing.
///
/// The board carries three lights. Two of them hang off GPIO — `blue_led` on
/// gpio-21 and `green_led` on gpio-22, both active low — and the kernel gives
/// them a `heartbeat` trigger from the device tree, so out of the box they
/// pulse for as long as the appliance is on. The third is red, appears nowhere
/// in the device tree, and is wired to the supply rather than to any pin of the
/// SoC: no amount of software reaches it. This type therefore describes the two
/// that can be told what to do, and nothing pretends otherwise.
///
/// A person watching a film in a dark room is the reason this is a setting at
/// all: a pulsing light beside the television is the one part of an appliance
/// that draws the eye away from it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedMode {
    /// Dark. Writes `none` to the trigger, then 0 to the brightness — the
    /// trigger first, because a trigger left in place simply overwrites any
    /// brightness written under it.
    #[default]
    Off,
    /// Lit and still.
    On,
    /// The kernel's own pulse, which is what the device tree asks for.
    Heartbeat,
}

impl LedMode {
    /// The order the settings row steps through. Off leads, because it is the
    /// mode this setting exists to reach.
    pub const ALL: [LedMode; 3] = [LedMode::Off, LedMode::On, LedMode::Heartbeat];

    /// The next mode round the ring, for a row that is chosen rather than
    /// typed into. A television remote has no text field.
    pub fn next(self) -> Self {
        let at = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }

    /// What goes in `/sys/class/leds/<led>/trigger`.
    pub fn trigger(self) -> &'static str {
        match self {
            LedMode::Off | LedMode::On => "none",
            LedMode::Heartbeat => "heartbeat",
        }
    }

    /// What goes in `brightness`, for the two modes that hold a level. The
    /// kernel owns the level under `heartbeat`, so that mode writes none.
    pub fn brightness(self) -> Option<u8> {
        match self {
            LedMode::Off => Some(0),
            LedMode::On => Some(1),
            LedMode::Heartbeat => None,
        }
    }

    /// As it reads on the television, from the sofa.
    pub fn label(self) -> &'static str {
        match self {
            LedMode::Off => "Kapalı",
            LedMode::On => "Açık",
            LedMode::Heartbeat => "Nabız",
        }
    }
}

/// Defaults to "no lights, none of them on": what a board that is not this one
/// answers, and what the settings screen shows before the daemon has replied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedStatus {
    /// False on a board whose lights are not where this expects them, which is
    /// every board that is not this one. The setting then shows why rather
    /// than offering a choice that would do nothing.
    pub available: bool,
    pub mode: LedMode,
    /// The lights this actually found, in sysfs order. Shown in diagnostics;
    /// empty when `available` is false.
    pub leds: Vec<String>,
    /// Why there is nothing to control, when there is nothing to control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    #[serde(default)]
    pub leds: LedStatus,
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
    KodiSeek {
        seconds: i64,
    },
    KodiOpen {
        url: String,
        resume_seconds: u64,
    },
    KodiRestart,
    CecStatus,
    CecDevices,
    CecActiveSource,
    CecWakeTv,
    CecStandbyTv,
    LedsStatus,
    /// Set the indicator lights and remember the choice across boots. The
    /// daemon owns this because `/sys/class/leds` is root-only and the
    /// interface runs under a unit that mounts /sys read-only.
    LedsSet {
        mode: LedMode,
    },
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
    MediaMeta {
        media_type: String,
        id: String,
    },
    MediaSubtitles {
        media_type: String,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video_id: Option<String>,
    },
    MediaLibrary,
    MediaLibraryItem {
        id: String,
    },
    MediaResolve {
        stream: Value,
    },
    MediaStreamPlan {
        stream: Value,
    },
    MediaSearch {
        query: String,
    },
    MediaInspect {
        url: String,
    },
    MediaStreams {
        media_type: String,
        id: String,
    },
    MediaPolicy {
        url: String,
    },
    MediaSessions,
    MediaSessionStart {
        url: String,
    },
    MediaSessionStop {
        id: String,
    },
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
        /// What the catalogue calls it.
        ///
        /// The player cannot work this out and must not try: a film opened
        /// from the catalogue is played through a session whose address is a
        /// hexadecimal identifier, and taking a name from that address put
        /// `73c91b7f7242ab69a92b3654aebb344f` across somebody's film.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// How long the film is, as the catalogue knows it.
        ///
        /// Also not the player's to answer. A session the worker proxies does
        /// not carry the container's duration, and mpv then estimates one from
        /// what it has: a ninety-nine minute film read as three minutes and
        /// forty-five seconds, with a progress bar nearly full at 3:15.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_seconds: Option<u64>,
    },
    /// Hand what is playing here to Kodi, at the position it had reached.
    ///
    /// The equivalent of "send to an external player": the same film, the same
    /// second, in the application that is better at the rest of the evening.
    MediaHandoffToKodi,
    /// Stop the interface's own player, if one is running.
    MediaStopHere,
    /// How far into the film the interface's own player has got, and whether
    /// it is paused. Answered by the player itself rather than remembered,
    /// because the person watching may have moved it.
    MediaStatusHere,
    /// Move what is playing here, without stopping it.
    ///
    /// A closed set rather than a pass-through to the player's own command
    /// language: the interface asks for a pause or a jump, and what that means
    /// to whatever is decoding is the daemon's business.
    MediaTransportHere {
        action: TransportAction,
    },
    /// Sign in to a Stremio account. The password is used once, forwarded to
    /// the media core and never written down; what is kept is the auth key.
    MediaLogin {
        email: String,
        password: String,
    },
    MediaLogout,
    SurfaceStatus,
    SurfaceSwitch {
        target: Surface,
    },
    /// Which product owns the television at boot, and what is on it now.
    ///
    /// This board can carry rk3588-screenbridge beside this product, and the
    /// two cannot both hold the display. The answer is a recorded preference
    /// rather than whoever started last; see mediaboxd-rs's owner module.
    DisplayOwner,
    /// Give the television to one product and remember the choice.
    DisplayOwnerSet {
        owner: String,
    },
    /// What the box can run, and what it is running.
    Applications,
    /// Put one application on the television. The id `idle` releases the
    /// display without starting anything.
    ApplicationLaunch {
        id: String,
    },
    /// Open one web address in the television's browser.
    ///
    /// The browser has no address bar — a remote cannot use one — so the
    /// address is chosen in this interface and handed over here: the daemon
    /// leaves it where the browser reads it at start, then gives the browser
    /// the display.
    BrowserOpen {
        url: String,
    },
    InputInject {
        action: InputAction,
    },
    InputMonitor,
    /// Restart or shut the appliance down.
    ///
    /// A closed enum rather than a command line, and one of exactly two
    /// answers. It exists because the alternative was worse: with no way to
    /// restart the box from the television, the appliance kept
    /// `HandlePowerKey`/`HandleRebootKey` doing it in systemd-logind, where a
    /// code from the *television's own remote* — the HDMI block registers a CEC
    /// remote-control input device, and udev tags it `power-switch` — restarted
    /// the machine with no application involved. That is switched off now, and
    /// this is the deliberate path in its place: the interface asks twice
    /// before sending it.
    SystemPower {
        action: PowerAction,
    },
}

/// What a remote can do to a film that is already playing here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportAction {
    PlayPause,
    /// Forwards on a positive number, back on a negative one, in seconds.
    Seek {
        seconds: i64,
    },
    /// Straight to a point in the film. What a scrub asks for: ten seconds at a
    /// time cannot reach the middle of a two hour film, and a relative jump
    /// computed from a position that has moved since is not the same place.
    SeekTo {
        seconds: u64,
    },
    /// Which subtitle track is on. A negative id turns them off, which is what
    /// the list's first row is.
    Subtitle {
        id: i64,
    },
    /// Which audio track is on.
    Audio {
        id: i64,
    },
    /// How far the subtitles are moved against the picture, in seconds.
    SubtitleDelay {
        seconds: f64,
    },
    /// How far the sound is moved against the picture, in seconds.
    AudioDelay {
        seconds: f64,
    },
    /// How fast, as a multiple of the film's own rate.
    Speed {
        value: f64,
    },
    /// How the picture meets the panel: "fit" keeps the whole frame and the
    /// bars with it, "crop" fills the panel and loses the edges, "stretch"
    /// fills it and bends the shapes.
    Scale {
        mode: ScaleMode,
    },
}

/// What "fit", "crop" and "stretch" mean to a player, kept as a type so the
/// interface and the control plane cannot disagree about the words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleMode {
    Fit,
    Crop,
    Stretch,
}

/// The only two things this appliance will do to its own power state on
/// request. Not a superset "and also suspend, and also kexec": a television box
/// that can be told to do something no remote can undo is a television box that
/// needs a keyboard to recover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    Restart,
    Shutdown,
}

impl PowerAction {
    /// The systemctl verb. Fixed strings, never interpolated from a request.
    pub fn verb(self) -> &'static str {
        match self {
            PowerAction::Restart => "reboot",
            PowerAction::Shutdown => "poweroff",
        }
    }
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

    #[test]
    fn power_is_a_closed_pair_of_verbs() {
        let restart: Request =
            serde_json::from_str(r#"{"command":"system_power","action":"restart"}"#).unwrap();
        assert_eq!(
            restart,
            Request::SystemPower {
                action: PowerAction::Restart
            }
        );
        assert_eq!(PowerAction::Restart.verb(), "reboot");
        assert_eq!(PowerAction::Shutdown.verb(), "poweroff");
        // Anything that is not one of the two is not a power request.
        assert!(
            serde_json::from_str::<Request>(r#"{"command":"system_power","action":"kexec"}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Request>(r#"{"command":"system_power","action":"reboot"}"#)
                .is_err()
        );
    }
}
