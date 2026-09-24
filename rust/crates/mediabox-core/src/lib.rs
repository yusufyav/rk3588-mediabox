//! MediaBox control plane wire types. These types are the stable local API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod ethernet;
mod output;
mod timing;
pub use output::{
    AppliedOutput, ColourCell, DisplayGeneration, DisplayIdentity, DisplayState, LegacyResolution,
    LegacySetting, OUTPUT_SETTING_SCHEMA, OUTPUT_TRIAL_SECONDS, Observed, ObservedOutput,
    OutputEvent, OutputGroup, OutputLink, OutputModeOffer, OutputOffer, OutputSetting,
    OutputStatus, OutputTrial, OwnerReport, Refusal, ResolutionChoice, Route, SelectedOutput,
    StoredSetting,
};
pub use timing::{ModeTiming, Refresh, TimingKey, mode_flags};
pub use ethernet::{
    ETHERNET_DNS_MAX, ETHERNET_TRIAL_SECONDS, EthernetConfig, EthernetPort, EthernetStatus,
    EthernetTrial, prefix_mask,
};

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


// ---------------------------------------------------------------- the wire
//
// What a television is sent, as a vocabulary the daemon, the control client and
// the interface all share. The measuring lives in `mediabox-platform`; these
// are the words it reports in.

/// How the pixels are laid out on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorFormat {
    Rgb,
    Ycbcr444,
    Ycbcr422,
    Ycbcr420,
}

impl ColorFormat {
    /// The name the vendor stacks use, and the one worth showing a person.
    pub fn label(self) -> &'static str {
        match self {
            ColorFormat::Rgb => "RGB",
            ColorFormat::Ycbcr444 => "YCbCr444",
            ColorFormat::Ycbcr422 => "YCbCr422",
            ColorFormat::Ycbcr420 => "YCbCr420",
        }
    }
}

/// One pixel format at one bit depth: what a sink advertises, and what a link
/// is asked to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorMode {
    pub format: ColorFormat,
    /// Bits per component: 8, 10 or 12.
    pub bits: u8,
}

impl ColorMode {
    pub const fn new(format: ColorFormat, bits: u8) -> Self {
        Self { format, bits }
    }

    /// `YCbCr422 12bit` — the same spelling the vendor box shows.
    pub fn label(self) -> String {
        format!("{} {}bit", self.format.label(), self.bits)
    }

    /// The TMDS character rate this mode needs, in kHz, for a given pixel
    /// clock.
    ///
    /// RGB and 4:4:4 carry every component for every pixel, so deep colour
    /// costs bandwidth in proportion: 10 bits is a quarter more than 8. 4:2:0
    /// halves the horizontal chroma and so halves the rate before that. 4:2:2
    /// is the exception and the reason it exists here at all -- it is carried
    /// in a fixed-width channel, so 12 bits of it costs exactly what 8 bits of
    /// it does, which is what makes HDR fit down a link that cannot take deep
    /// colour any other way.
    pub fn character_rate_khz(self, pixel_clock_khz: u32) -> u32 {
        let clock = u64::from(pixel_clock_khz);
        let rate = match self.format {
            ColorFormat::Ycbcr422 => clock,
            ColorFormat::Ycbcr420 => clock * u64::from(self.bits) / 16,
            ColorFormat::Rgb | ColorFormat::Ycbcr444 => clock * u64::from(self.bits) / 8,
        };
        rate.min(u64::from(u32::MAX)) as u32
    }

    /// Whether this mode can carry HDR10: ten bits per component or more, in
    /// any format. HDR10 is a ten-bit format; eight bits of PQ bands.
    pub fn carries_hdr(self) -> bool {
        self.bits >= 10
    }
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

// ----------------------------------------------------------------- the fan
//
// The fan is the kernel's. The vendor RK3588 kernel's `pwm-fan` driver reads
// two properties off its device-tree node -- `cooling-levels`, a PWM duty per
// cooling state, and `rockchip,temp-trips`, the SoC temperature at which each
// state begins -- and drives the fan from them for as long as the board is on.
// Nothing in this product runs a second governor beside it: a curve is written
// into a device-tree overlay that the boot loader applies, and the kernel
// remains the only thing that ever sets the duty.
//
// Which is also why a curve is not live: it takes effect on the next boot.

/// How few and how many points a curve may have.
///
/// The driver has no ceiling of its own: `rockchip,temp-trips` and
/// `cooling-levels` are counted and allocated at probe, any length. The upper
/// bound is this product's, for the remote: ten points is a list that still
/// fits one screen of the editor. The lower bound is the smallest curve that
/// says anything -- somewhere the fan starts, somewhere it is full.
pub const FAN_CURVE_MIN_POINTS: usize = 2;
pub const FAN_CURVE_MAX_POINTS: usize = 10;

/// The lowest duty, other than off, that a curve may ask for.
///
/// Measured on an Orange Pi 5 Plus: at the vendor's 20 kHz carrier, 50/255 did
/// not start the fan at all; at 50 Hz it did. Nothing between 1 and 49 has been
/// measured on either carrier, and the driver has no kick-start -- going from
/// off to a low duty applies that duty and nothing more, so a duty the fan
/// stalls at is a fan that stays still while the screen says it is cooling.
/// Off (0) is allowed; 1..=49 is not, until somebody measures it.
pub const FAN_PWM_MIN_RUNNING: u32 = 50;

/// Full duty. The last point of every curve has to reach it.
pub const FAN_PWM_MAX: u32 = 255;

/// The window a curve's points may sit in, in whole degrees Celsius.
///
/// The floor is the floor of the property: the driver compares an unsigned
/// millidegree value, and a trip at 0 °C is a real trip that is always
/// crossed -- it is a coordinate, not a claim that the fan runs in a freezer.
/// The ceiling is the SoC's own first passive trip, read off the running
/// board's `soc-thermal` zone as 75 °C: past it the kernel starts cutting
/// clocks, and full duty has to have been reached by then.
pub const FAN_TEMP_MIN_C: u32 = 0;
pub const FAN_TEMP_MAX_C: u32 = 75;

/// One point of a curve: from this SoC temperature up to the next point, this
/// duty. The kernel applies a curve as steps, not as a line between points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanPoint {
    /// Whole degrees Celsius.
    pub temperature_c: u32,
    /// PWM duty, 0..=255. A duty and not a speed: this fan has no tachometer.
    pub pwm: u32,
}

impl FanPoint {
    pub const fn new(temperature_c: u32, pwm: u32) -> Self {
        Self { temperature_c, pwm }
    }
}

/// Whether a duty is one a curve may ask for: off, or at least the lowest
/// duty measured to start the fan.
pub fn fan_pwm_allowed(pwm: u32) -> bool {
    pwm == 0 || (FAN_PWM_MIN_RUNNING..=FAN_PWM_MAX).contains(&pwm)
}

/// A duty as a share of full, the way the interface prints it.
pub fn fan_pwm_percent(pwm: u32) -> f64 {
    (f64::from(pwm) * 1000.0 / f64::from(FAN_PWM_MAX)).round() / 10.0
}

/// A duty typed as a share of full, in whole percent, as the duty the kernel
/// takes. Rounded half up, so every whole percent has one duty and the same
/// one every time: 20 is 51, 100 is 255. `None` past 100.
///
/// The duty that comes back is not checked against the running minimum; that
/// is [`FanCurve::set_pwm`]'s to refuse.
pub fn fan_pwm_from_percent(percent: u32) -> Option<u32> {
    (percent <= 100).then(|| (percent * FAN_PWM_MAX + 50) / 100)
}

/// Which curve a person chose.
///
/// A preset is where editing starts, not a mode: change any point of it and it
/// is a custom curve.
///
/// Points are joined by straight lines. The vendor's pwm-fan driver has no
/// such thing -- it holds each trip's duty until the next trip -- so the line
/// is given to it as one trip per whole degree, each with the line's duty at
/// that degree (see [`FanCurve::kernel_steps`]). What the graph draws and what
/// the fan does then differ by less than a degree.
///
/// There is no "quiet" preset, deliberately. What would make one quieter than
/// Balanced is a duty below the lowest one measured to start this fan, and
/// until that has been measured a quiet preset would be a guess about whether
/// the SoC gets cooled at all. A curve that simply starts later is one the
/// editor can already make.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanProfile {
    /// The vendor's duties at the vendor's temperatures, joined.
    #[default]
    Balanced,
    /// The same duties ten degrees earlier: never less than Balanced at any
    /// temperature.
    Cool,
    /// A person's own points, held to the same rules as the presets.
    Custom,
    /// The board's own curve, as its device tree has it when no curve of this
    /// product's is loaded: the vendor's steps, drawn as the steps they are.
    /// What a board runs out of the box and after "Varsayılana dön".
    Board,
}

impl FanProfile {
    pub const ALL: [FanProfile; 4] = [
        FanProfile::Balanced,
        FanProfile::Cool,
        FanProfile::Custom,
        FanProfile::Board,
    ];
    /// The ones that are a starting point rather than a result.
    pub const PRESETS: [FanProfile; 2] = [FanProfile::Balanced, FanProfile::Cool];

    pub fn label(self) -> &'static str {
        match self {
            FanProfile::Balanced => "Dengeli",
            FanProfile::Cool => "Serin",
            FanProfile::Custom => "Özel",
            FanProfile::Board => "Kartın eğrisi",
        }
    }

    /// The points of a preset; none for a custom curve.
    pub fn preset(self) -> Option<Vec<FanPoint>> {
        let points: &[(u32, u32)] = match self {
            // The vendor's duties at the vendor's temperatures (below), joined
            // by lines instead of held in steps.
            FanProfile::Balanced => &[(50, 50), (55, 100), (60, 150), (65, 200), (70, 255)],
            // Its lowest duty is the vendor's lowest, which is the lowest that
            // has been seen to start the fan.
            FanProfile::Cool => &[(40, 50), (45, 100), (50, 150), (55, 200), (60, 255)],
            // Read off the vendor device tree of both the Plus and the Ultra:
            // cooling-levels <0 50 100 150 200 255>, trips 50..70 °C, each duty
            // held until the next trip. Steps, as points: each duty again one
            // degree before the next begins.
            FanProfile::Board => &[
                (50, 50),
                (54, 50),
                (55, 100),
                (59, 100),
                (60, 150),
                (64, 150),
                (65, 200),
                (69, 200),
                (70, 255),
            ],
            FanProfile::Custom => return None,
        };
        Some(points.iter().map(|&(t, p)| FanPoint::new(t, p)).collect())
    }
}

/// A curve as a person edits it and as the kernel is given it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanCurve {
    pub profile: FanProfile,
    pub points: Vec<FanPoint>,
}

/// Why a curve was refused. Every rule is checked by the control plane
/// whatever the interface already checked, because the control plane is what
/// writes the boot configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanCurveError {
    PointCount(usize),
    /// A preset was sent with points that are not the preset's.
    PresetMismatch(FanProfile),
    /// A custom curve with no points.
    MissingPoints,
    TemperatureOutOfRange {
        index: usize,
        temperature_c: u32,
    },
    TemperatureNotIncreasing {
        index: usize,
    },
    PwmOutOfRange {
        index: usize,
        pwm: u32,
    },
    PwmBelowRunning {
        index: usize,
        pwm: u32,
    },
    PwmDecreasing {
        index: usize,
    },
    FinalNotFull {
        pwm: u32,
    },
}

impl std::fmt::Display for FanCurveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            FanCurveError::PointCount(count) => write!(
                f,
                "eğri {FAN_CURVE_MIN_POINTS}–{FAN_CURVE_MAX_POINTS} noktadan oluşmalı, \
                 {count} geldi"
            ),
            FanCurveError::PresetMismatch(profile) => write!(
                f,
                "{} hazır profilinin noktaları değiştirilemez; özel eğri olarak gönderin",
                profile.label()
            ),
            FanCurveError::MissingPoints => write!(f, "özel eğri için nokta gönderilmedi"),
            FanCurveError::TemperatureOutOfRange {
                index,
                temperature_c,
            } => write!(
                f,
                "{}. nokta {temperature_c} °C; izin verilen aralık \
                 {FAN_TEMP_MIN_C}–{FAN_TEMP_MAX_C} °C",
                index + 1
            ),
            FanCurveError::TemperatureNotIncreasing { index } => write!(
                f,
                "{}. noktanın sıcaklığı bir öncekinden yüksek olmalı",
                index + 1
            ),
            FanCurveError::PwmOutOfRange { index, pwm } => {
                write!(f, "{}. nokta PWM {pwm}; en fazla {FAN_PWM_MAX}", index + 1)
            }
            FanCurveError::PwmBelowRunning { index, pwm } => write!(
                f,
                "{}. nokta PWM {pwm}; 0 (kapalı) ya da fanın döndüğü ölçülmüş en \
                 düşük değer {FAN_PWM_MIN_RUNNING} ve üstü olmalı",
                index + 1
            ),
            FanCurveError::PwmDecreasing { index } => write!(
                f,
                "{}. noktanın PWM değeri bir öncekinden düşük olamaz",
                index + 1
            ),
            FanCurveError::FinalNotFull { pwm } => {
                write!(f, "son nokta tam güç ({FAN_PWM_MAX}) olmalı, {pwm} geldi")
            }
        }
    }
}

impl std::error::Error for FanCurveError {}

/// How far one press moves a point.
pub const FAN_TEMP_STEP_C: u32 = 1;
pub const FAN_PWM_STEP: u32 = 5;

impl FanCurve {
    pub fn preset(profile: FanProfile) -> Option<Self> {
        profile.preset().map(|points| Self { profile, points })
    }

    pub fn balanced() -> Self {
        Self::preset(FanProfile::Balanced).expect("balanced is a preset")
    }

    /// The vendor's curve, which is what a board with no curve of this
    /// product's has.
    pub fn board() -> Self {
        Self::preset(FanProfile::Board).expect("the board's curve is a preset")
    }

    /// What a `fan_curve_set` request asks for, as one checked curve.
    ///
    /// A preset is named, not described: its points may be left out, and if
    /// they are sent they have to be the preset's own.
    pub fn resolve(
        profile: FanProfile,
        points: Option<Vec<FanPoint>>,
    ) -> Result<Self, FanCurveError> {
        let curve = match (Self::preset(profile), points) {
            (Some(preset), None) => preset,
            (Some(preset), Some(points)) if points == preset.points => preset,
            (Some(_), Some(_)) => return Err(FanCurveError::PresetMismatch(profile)),
            (None, None) => return Err(FanCurveError::MissingPoints),
            (None, Some(points)) => Self { profile, points },
        };
        curve.validate()?;
        Ok(curve)
    }

    /// The rules every curve is held to, presets included.
    pub fn validate(&self) -> Result<(), FanCurveError> {
        let count = self.points.len();
        if !(FAN_CURVE_MIN_POINTS..=FAN_CURVE_MAX_POINTS).contains(&count) {
            return Err(FanCurveError::PointCount(count));
        }
        if let Some(preset) = self.profile.preset()
            && self.points != preset
        {
            return Err(FanCurveError::PresetMismatch(self.profile));
        }
        for (index, point) in self.points.iter().enumerate() {
            if !(FAN_TEMP_MIN_C..=FAN_TEMP_MAX_C).contains(&point.temperature_c) {
                return Err(FanCurveError::TemperatureOutOfRange {
                    index,
                    temperature_c: point.temperature_c,
                });
            }
            if point.pwm > FAN_PWM_MAX {
                return Err(FanCurveError::PwmOutOfRange {
                    index,
                    pwm: point.pwm,
                });
            }
            if !fan_pwm_allowed(point.pwm) {
                return Err(FanCurveError::PwmBelowRunning {
                    index,
                    pwm: point.pwm,
                });
            }
            if index > 0 {
                let before = self.points[index - 1];
                if point.temperature_c <= before.temperature_c {
                    return Err(FanCurveError::TemperatureNotIncreasing { index });
                }
                if point.pwm < before.pwm {
                    return Err(FanCurveError::PwmDecreasing { index });
                }
            }
        }
        let last = self.points[count - 1].pwm;
        if last != FAN_PWM_MAX {
            return Err(FanCurveError::FinalNotFull { pwm: last });
        }
        Ok(())
    }

    /// The line's duty at a whole degree, as the kernel is given it: off below
    /// the first point, the last point's duty from it on, and between two
    /// points the straight line between them, rounded half up. A duty the line
    /// passes through below the lowest running one is given as off -- the fan
    /// is not asked for a duty it has not been measured to start at.
    pub fn duty_at_degree(&self, celsius: u32) -> u32 {
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else {
            return 0;
        };
        if celsius < first.temperature_c {
            return 0;
        }
        if celsius >= last.temperature_c {
            return last.pwm;
        }
        let at = self
            .points
            .windows(2)
            .find(|pair| celsius < pair[1].temperature_c)
            .expect("inside the curve");
        let (a, b) = (at[0], at[1]);
        let span = b.temperature_c - a.temperature_c;
        let rise = b.pwm.saturating_sub(a.pwm) * (celsius - a.temperature_c);
        let pwm = a.pwm + (2 * rise + span) / (2 * span);
        if fan_pwm_allowed(pwm) { pwm } else { 0 }
    }

    /// The curve as the kernel is given it: `(°C, duty)` at each whole degree
    /// where the duty changes, from the first point to the last. Below the
    /// first entry the fan is off; each entry holds until the next.
    ///
    /// One per degree at most, so a curve is never more than 76 trips.
    pub fn kernel_steps(&self) -> Vec<(u32, u32)> {
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else {
            return Vec::new();
        };
        let mut steps = Vec::new();
        let mut before = 0;
        for celsius in first.temperature_c..=last.temperature_c {
            let pwm = self.duty_at_degree(celsius);
            if pwm != before {
                steps.push((celsius, pwm));
                before = pwm;
            }
        }
        steps
    }

    /// Whether two curves give the kernel the same trips: the same fan,
    /// whatever points each was drawn with.
    pub fn same_kernel(&self, other: &FanCurve) -> bool {
        self.kernel_steps() == other.kernel_steps()
    }

    /// `cooling-levels`: state 0 is the fan off, and each kernel step is the
    /// next state.
    pub fn cooling_levels(&self) -> Vec<u32> {
        std::iter::once(0)
            .chain(self.kernel_steps().into_iter().map(|(_, pwm)| pwm))
            .collect()
    }

    /// `rockchip,temp-trips`: pairs of millidegrees and the state that begins
    /// there. The driver takes the state of the last trip at or below the
    /// temperature.
    pub fn temp_trips(&self) -> Vec<(u32, u32)> {
        self.kernel_steps()
            .into_iter()
            .enumerate()
            .map(|(index, (celsius, _))| (celsius * 1000, index as u32 + 1))
            .collect()
    }

    /// The curve a device tree describes, when it describes one in this
    /// shape: state 0 off, one trip per further state, in order, on whole
    /// degrees, rising. Anything else is not claimed as a curve at all.
    ///
    /// The trips are turned back into points and the points thinned out for
    /// as long as the kernel would be given the same trips, so a curve this
    /// product wrote comes back as a handful of points rather than one per
    /// degree. A tree that is a preset's comes back as that preset.
    pub fn from_device_tree(levels: &[u32], trips: &[(u32, u32)]) -> Option<Self> {
        if trips.is_empty() || levels.len() != trips.len() + 1 || levels.first() != Some(&0) {
            return None;
        }
        let mut steps = Vec::with_capacity(trips.len());
        for (index, &(millicelsius, state)) in trips.iter().enumerate() {
            if state as usize != index + 1 || millicelsius % 1000 != 0 {
                return None;
            }
            let step = (millicelsius / 1000, levels[index + 1]);
            if steps
                .last()
                .is_some_and(|&(celsius, _): &(u32, u32)| step.0 <= celsius)
            {
                return None;
            }
            steps.push(step);
        }

        if let Some(preset) = [FanProfile::Board, FanProfile::Balanced, FanProfile::Cool]
            .into_iter()
            .filter_map(Self::preset)
            .find(|preset| preset.kernel_steps() == steps)
        {
            return Some(preset);
        }

        // The duty at every degree from the first trip to the last, and the
        // fewest of those degrees whose lines give every other one back
        // exactly: a shortest path, where a hop from one degree to another is
        // allowed when the line between them rounds to what the tree says at
        // every degree in between. The points a curve was written from are
        // one such path, so what comes back is never more of them.
        let (from, to) = (steps[0].0, steps[steps.len() - 1].0);
        let duty = |celsius: u32| {
            steps
                .iter()
                .rev()
                .find(|&&(at, _)| at <= celsius)
                .map_or(0, |&(_, pwm)| pwm)
        };
        let degrees: Vec<FanPoint> = (from..=to).map(|c| FanPoint::new(c, duty(c))).collect();
        let hop = |i: usize, j: usize| {
            let line = Self {
                profile: FanProfile::Custom,
                points: vec![degrees[i], degrees[j]],
            };
            degrees[i].pwm <= degrees[j].pwm
                && degrees[i + 1..j]
                    .iter()
                    .all(|point| line.duty_at_degree(point.temperature_c) == point.pwm)
        };
        let mut best: Vec<(usize, usize)> = vec![(usize::MAX, 0); degrees.len()];
        best[0] = (1, 0);
        for j in 1..degrees.len() {
            for i in 0..j {
                if best[i].0 != usize::MAX && best[i].0 + 1 < best[j].0 && hop(i, j) {
                    best[j] = (best[i].0 + 1, i);
                }
            }
        }
        let mut path = vec![degrees.len() - 1];
        while let Some(&at) = path.last()
            && at != 0
        {
            path.push(best[at].1);
        }
        path.reverse();
        let curve = Self {
            profile: FanProfile::Custom,
            points: path.into_iter().map(|i| degrees[i]).collect(),
        };
        (curve.kernel_steps() == steps).then_some(curve)
    }

    /// The duty the kernel gives at a temperature under this curve: the last
    /// trip at or below it, and off below the first.
    pub fn duty_at(&self, celsius: f64) -> u32 {
        self.kernel_steps()
            .into_iter()
            .rev()
            .find(|&(at, _)| celsius >= f64::from(at))
            .map_or(0, |(_, pwm)| pwm)
    }

    /// The curve as a graph should draw it, from 0 °C to `until`: corners as
    /// `(°C, duty)`, joined by straight lines. Off up to the first point, the
    /// line between points, the last duty after the last point.
    ///
    /// Where a line starts from off, the fan stays off until the first whole
    /// degree the line reaches a running duty, and the drawing says so rather
    /// than showing a duty the kernel is never given.
    pub fn polyline(&self, until: f64) -> Vec<(f64, f64)> {
        let Some(first) = self.points.first() else {
            return vec![(0.0, 0.0), (until, 0.0)];
        };
        let mut corners = vec![(0.0, 0.0)];
        let start = f64::from(first.temperature_c);
        corners.push((start, 0.0));
        corners.push((start, f64::from(first.pwm)));
        for pair in self.points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a.pwm == 0 && b.pwm > 0 {
                let on = (a.temperature_c + 1..=b.temperature_c)
                    .find(|&celsius| self.duty_at_degree(celsius) > 0)
                    .unwrap_or(b.temperature_c);
                let share =
                    f64::from(on - a.temperature_c) / f64::from(b.temperature_c - a.temperature_c);
                let pwm = f64::from(b.pwm) * share;
                corners.push((f64::from(on), 0.0));
                corners.push((f64::from(on), pwm));
            }
            corners.push((f64::from(b.temperature_c), f64::from(b.pwm)));
        }
        let end = self.points.last().expect("not empty");
        corners.push((until.max(f64::from(end.temperature_c)), f64::from(end.pwm)));
        corners.dedup();
        corners
    }

    /// Where this curve gives the fan a different duty from `other`: runs of
    /// whole degrees, `(first, last)`, each widened by a degree on either side
    /// so a drawing of either curve over it joins the part that is the same.
    /// Empty when the two are the same fan.
    pub fn changed_ranges(&self, other: &FanCurve, until: u32) -> Vec<(u32, u32)> {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for celsius in 0..=until {
            if self.duty_at_degree(celsius) == other.duty_at_degree(celsius) {
                continue;
            }
            let (from, to) = (celsius.saturating_sub(1), (celsius + 1).min(until));
            match ranges.last_mut() {
                Some(last) if from <= last.1 => last.1 = to,
                _ => ranges.push((from, to)),
            }
        }
        ranges
    }

    /// [`FanCurve::polyline`] cut to `from..=to` °C, with a corner added where
    /// the cut falls inside a piece.
    pub fn polyline_within(&self, until: f64, from: f64, to: f64) -> Vec<(f64, f64)> {
        let corners = self.polyline(until);
        let on = |pair: &[(f64, f64)], x: f64| {
            let share = (x - pair[0].0) / (pair[1].0 - pair[0].0);
            pair[0].1 + (pair[1].1 - pair[0].1) * share
        };
        // Where a cut falls on a rise, the start takes the top of it -- the
        // piece that goes on to the right -- and the end the piece that
        // arrives from the left.
        let start = corners
            .windows(2)
            .find(|pair| pair[0].0 <= from && from < pair[1].0)
            .map_or(0.0, |pair| on(pair, from));
        let end = corners
            .windows(2)
            .find(|pair| pair[0].0 < to && to <= pair[1].0)
            .map_or(0.0, |pair| on(pair, to));
        let mut cut = vec![(from, start)];
        cut.extend(corners.iter().copied().filter(|&(x, _)| from < x && x < to));
        cut.push((to, end));
        cut
    }

    // ------------------------------------------------------------ editing
    //
    // Every edit keeps the curve valid, so an editor never holds a curve the
    // control plane would refuse, and every edit that changes something makes
    // it a custom curve. An edit that would break a rule does nothing and says
    // so by returning false.

    fn edited(&mut self) -> bool {
        self.profile = FanProfile::Custom;
        true
    }

    /// Where a point's temperature may go without passing its neighbours.
    pub fn temperature_bounds(&self, index: usize) -> (u32, u32) {
        let low = if index == 0 {
            FAN_TEMP_MIN_C
        } else {
            self.points[index - 1].temperature_c + 1
        };
        let high = self
            .points
            .get(index + 1)
            .map_or(FAN_TEMP_MAX_C, |next| next.temperature_c - 1);
        (low, high)
    }

    /// Where a point's duty may go without passing its neighbours. The last
    /// point is full duty and does not move.
    pub fn pwm_bounds(&self, index: usize) -> (u32, u32) {
        if index + 1 == self.points.len() {
            return (FAN_PWM_MAX, FAN_PWM_MAX);
        }
        let low = if index == 0 {
            0
        } else {
            self.points[index - 1].pwm
        };
        let high = self
            .points
            .get(index + 1)
            .map_or(FAN_PWM_MAX, |next| next.pwm);
        (low, high)
    }

    /// One degree up or down.
    pub fn step_temperature(&mut self, index: usize, up: bool) -> bool {
        if index >= self.points.len() {
            return false;
        }
        let (low, high) = self.temperature_bounds(index);
        let now = self.points[index].temperature_c;
        let next = if up {
            now.saturating_add(FAN_TEMP_STEP_C).min(high)
        } else {
            now.saturating_sub(FAN_TEMP_STEP_C).max(low)
        };
        if next == now || !(low..=high).contains(&next) {
            return false;
        }
        self.points[index].temperature_c = next;
        self.edited()
    }

    /// One step of duty up or down. The gap between off and the lowest
    /// running duty is jumped, never landed in.
    pub fn step_pwm(&mut self, index: usize, up: bool) -> bool {
        if index >= self.points.len() {
            return false;
        }
        let (low, high) = self.pwm_bounds(index);
        let now = self.points[index].pwm;
        let mut next = if up {
            (now + FAN_PWM_STEP).min(high)
        } else {
            now.saturating_sub(FAN_PWM_STEP).max(low)
        };
        if !fan_pwm_allowed(next) {
            next = if up { FAN_PWM_MIN_RUNNING } else { 0 };
        }
        if next == now || !(low..=high).contains(&next) {
            return false;
        }
        self.points[index].pwm = next;
        self.edited()
    }

    /// A temperature typed for a point. Refused, and nothing changes, when
    /// it would pass a neighbour or leave the range the kernel is given.
    pub fn set_temperature(&mut self, index: usize, celsius: u32) -> bool {
        if index >= self.points.len() {
            return false;
        }
        let (low, high) = self.temperature_bounds(index);
        if !(low..=high).contains(&celsius) {
            return false;
        }
        if self.points[index].temperature_c != celsius {
            self.points[index].temperature_c = celsius;
            self.edited();
        }
        true
    }

    /// A duty typed for a point. Refused when it is in the gap between off
    /// and the lowest running duty, or would pass a neighbour; the last
    /// point's full duty is not changed this way either.
    pub fn set_pwm(&mut self, index: usize, pwm: u32) -> bool {
        if index >= self.points.len() || !fan_pwm_allowed(pwm) {
            return false;
        }
        let (low, high) = self.pwm_bounds(index);
        if !(low..=high).contains(&pwm) {
            return false;
        }
        if self.points[index].pwm != pwm {
            self.points[index].pwm = pwm;
            self.edited();
        }
        true
    }

    /// A new point halfway between `index` and the point after it -- or, from
    /// the last point, between it and the one before -- on the line between
    /// them. Returns where it went.
    pub fn insert_after(&mut self, index: usize) -> Option<usize> {
        if self.points.len() >= FAN_CURVE_MAX_POINTS || self.points.is_empty() {
            return None;
        }
        let index = index.min(self.points.len() - 1);
        let (a, b) = if index + 1 < self.points.len() {
            (index, index + 1)
        } else if index > 0 {
            (index - 1, index)
        } else {
            return None;
        };
        let (low, high) = (self.points[a], self.points[b]);
        if high.temperature_c - low.temperature_c < 2 {
            return None;
        }
        let temperature_c = (low.temperature_c + high.temperature_c) / 2;
        // On the line already there, so adding a point changes nothing until
        // it is moved.
        let pwm = self.duty_at_degree(temperature_c).clamp(low.pwm, high.pwm);
        self.points.insert(b, FanPoint::new(temperature_c, pwm));
        self.edited();
        Some(b)
    }

    /// Takes a point away. Not the last one -- it is the full-duty point every
    /// curve ends on -- and not below the smallest curve.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.points.len() <= FAN_CURVE_MIN_POINTS || index + 1 >= self.points.len() {
            return false;
        }
        self.points.remove(index);
        self.edited()
    }
}

/// The board-specific carrier correction, where a board needs one.
///
/// The Orange Pi 5 Plus drives its fan with a 20 kHz carrier and the fan does
/// not start at the lowest cooling level on it; the Ultra's device tree uses
/// 50 Hz and its fan does. The Plus gets an overlay that changes the period
/// and nothing else. It is a property of the board, not a setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanBoardFix {
    /// This board's own carrier is the right one.
    #[default]
    NotNeeded,
    /// Needed, and the running device tree has it.
    Active,
    /// Needed, and the running device tree does not have it yet.
    Missing,
}

/// The fan as the kernel is running it, and the curve that was chosen for it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FanStatus {
    /// False on a board with no `pwmfan` hwmon device. Nothing below is then
    /// a reading.
    pub available: bool,
    /// `/proc/device-tree/model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    /// The `soc-thermal` zone, in degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_c: Option<f64>,
    /// The duty the kernel is driving now, 0..=255.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pwm: Option<u32>,
    /// The same duty as a share of full, to one decimal. A duty, not a speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pwm_percent: Option<f64>,
    /// `pwm1_enable` as the kernel reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pwm_enable: Option<u32>,
    /// The PWM carrier's period in the running device tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pwm_period_ns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pwm_frequency_hz: Option<f64>,
    /// Whether the fan reports its speed at all (`fan1_input`). Neither board
    /// has a tachometer wire, so this is false and no speed is ever shown.
    pub rpm_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    /// Who drives the duty. Always the kernel's `pwm-fan`.
    pub control_backend: String,
    /// The curve in the running device tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve: Option<FanCurve>,
    /// The curve this product has written for the next boot; none when the
    /// board's own curve is in use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured: Option<FanCurve>,
    /// A curve was saved or reset during this boot and is not running yet.
    pub pending_reboot: bool,
    pub board_fix: FanBoardFix,
    /// Whether the boot configuration loads this product's curve overlay.
    /// Without it a saved curve would never reach the kernel, so saving is
    /// refused rather than reported as done.
    pub boot_config_ready: bool,
    /// Something is not as it was set up to be, without the fan being
    /// unavailable: a saved curve that the running tree does not carry after a
    /// reboot, a missing carrier correction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
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
    /// The display: what the kernel lists for it, what was chosen, what is on
    /// the wire. Here rather than behind its own call because the settings
    /// screen draws it beside everything else and one poll is one answer.
    #[serde(default)]
    pub output: OutputStatus,
    /// The fan and its curve, for the same reason: one poll, one answer.
    #[serde(default)]
    pub fan: FanStatus,
    /// The wired ports, what each is set to, and a change waiting to be kept.
    #[serde(default)]
    pub ethernet: EthernetStatus,
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
    /// The display: its modes and colour cells, the kept setting, a trial in
    /// progress, and what is on the wire.
    OutputStatus,
    /// Put a mode, and a colour mode at it, on the wire on trial. Undone after
    /// [`OUTPUT_TRIAL_SECONDS`] unless kept. `colour: None` is `Auto` at that
    /// mode.
    OutputTry {
        resolution: ResolutionChoice,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        colour: Option<ColorMode>,
    },
    /// Keep the setting on trial `trial`: it is written down and used from
    /// now on. Refused for any other trial, and for one the owner has not
    /// reported committed on the sink plugged in now.
    OutputKeep { trial: u64 },
    /// Take the setting on trial `trial` back now.
    OutputRevert { trial: u64 },
    /// The wired ports and what each is set to.
    EthernetStatus,
    /// Put an address on a wired port on trial. It is taken back after
    /// [`ETHERNET_TRIAL_SECONDS`] unless kept, and by a reboot in between.
    EthernetTry {
        interface: String,
        config: EthernetConfig,
    },
    /// Keep the address on trial: it is written down and used from now on.
    EthernetKeep,
    /// Take the address on trial back now.
    EthernetRevert,
    /// The fan as the kernel is running it, and the curve chosen for it.
    FanStatus,
    /// Choose the curve the kernel is given from the next boot on.
    ///
    /// The daemon validates it whatever the interface did, writes it into a
    /// device-tree overlay under /boot, and does not touch the running fan:
    /// the kernel stays the only thing that sets the duty. `points` is left
    /// out for a preset and required for `custom`.
    FanCurveSet {
        profile: FanProfile,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        points: Option<Vec<FanPoint>>,
    },
    /// Go back to the board's own curve from the next boot on. The carrier
    /// correction a board needs is not part of the curve and stays.
    FanCurveReset,
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

    // ------------------------------------------------------------ the radios
    //
    // Both boards ship their radios switched off at the kernel level — the
    // journal says "[BT_RFKILL]: bt shut off power" before anything userspace
    // runs — and switching them on needs rfkill, which is root's. The
    // interface's unit mounts /sys read-only and has no business holding a
    // wpa_supplicant control socket either, so the daemon owns this the same
    // way it owns the indicator lights and the colour mode.
    //
    // Wi-Fi goes through wpa_supplicant's own control interface rather than
    // NetworkManager: the appliance already runs wpa_supplicant beside
    // systemd-networkd, and adding a second thing that claims interfaces is
    // how a board ends up with two managers fighting over one radio.
    WifiStatus,
    /// Ask the radio for what is in the air. Seconds, not instant.
    WifiScan,
    /// Join a network. `psk` is absent for an open one; it never reaches a
    /// log, and what is written to disk is the derived key, not the phrase.
    WifiConnect {
        ssid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        psk: Option<String>,
    },
    WifiDisconnect,
    WifiForget {
        ssid: String,
    },
    WifiPower {
        on: bool,
    },

    BluetoothStatus,
    BluetoothScan,
    BluetoothPower {
        on: bool,
    },
    BluetoothPair {
        address: String,
    },
    BluetoothConnect {
        address: String,
    },
    BluetoothDisconnect {
        address: String,
    },
    BluetoothRemove {
        address: String,
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

    // ------------------------------------------------------------- the fan

    fn custom(points: &[(u32, u32)]) -> FanCurve {
        FanCurve {
            profile: FanProfile::Custom,
            points: points.iter().map(|&(t, p)| FanPoint::new(t, p)).collect(),
        }
    }

    /// The brief's own example: starts at 0 °C, off until 30, arbitrary
    /// temperatures and duties, seven points.
    const FROM_ZERO: [(u32, u32); 7] = [
        (0, 0),
        (30, 0),
        (40, 50),
        (48, 80),
        (55, 120),
        (63, 180),
        (72, 255),
    ];

    #[test]
    fn every_preset_passes_its_own_rules() {
        for profile in [FanProfile::Balanced, FanProfile::Cool, FanProfile::Board] {
            let curve = FanCurve::preset(profile).unwrap();
            assert_eq!(curve.validate(), Ok(()), "{profile:?}");
            assert_eq!(curve.profile, profile);
        }
        assert_eq!(FanCurve::preset(FanProfile::Custom), None);
    }

    /// The board's own curve is the vendor tree, byte for byte, in both
    /// directions: steps, drawn as steps.
    #[test]
    fn the_board_s_curve_is_the_vendor_tree() {
        let board = FanCurve::board();
        assert_eq!(board.cooling_levels(), [0, 50, 100, 150, 200, 255]);
        assert_eq!(
            board.temp_trips(),
            [(50000, 1), (55000, 2), (60000, 3), (65000, 4), (70000, 5)]
        );
        let read = FanCurve::from_device_tree(&board.cooling_levels(), &board.temp_trips());
        assert_eq!(read, Some(board));
    }

    /// Balanced is the vendor's duties at the vendor's temperatures, joined:
    /// one trip per degree, each on the line.
    #[test]
    fn balanced_joins_the_vendor_duties() {
        let balanced = FanCurve::balanced();
        let steps = balanced.kernel_steps();
        assert_eq!(steps.len(), 21);
        assert_eq!(steps[0], (50, 50));
        assert_eq!(steps[1], (51, 60));
        assert_eq!(steps[5], (55, 100));
        assert_eq!(steps[16], (66, 211));
        assert_eq!(steps[20], (70, 255));
        assert_eq!(balanced.duty_at(49.9), 0);
        assert_eq!(balanced.duty_at(52.5), 70);
        let read = FanCurve::from_device_tree(&balanced.cooling_levels(), &balanced.temp_trips());
        assert_eq!(read, Some(balanced));
    }

    #[test]
    fn cool_never_gives_less_than_balanced_or_the_board() {
        let cool = FanCurve::preset(FanProfile::Cool).unwrap();
        let read = FanCurve::from_device_tree(&cool.cooling_levels(), &cool.temp_trips());
        assert_eq!(read, Some(cool.clone()));
        for other in [FanCurve::balanced(), FanCurve::board()] {
            for celsius in 0..=100 {
                let c = f64::from(celsius);
                assert!(cool.duty_at(c) >= other.duty_at(c), "{celsius} °C");
            }
        }
    }

    #[test]
    fn arbitrary_temperatures_duties_and_counts_are_accepted() {
        assert_eq!(custom(&FROM_ZERO).validate(), Ok(()));
        // Two points: off until 60, then full.
        assert_eq!(custom(&[(0, 0), (60, 255)]).validate(), Ok(()));
        // The brief's other example, and one with ten points.
        assert_eq!(
            custom(&[
                (0, 0),
                (20, 0),
                (35, 50),
                (45, 80),
                (55, 130),
                (65, 200),
                (75, 255)
            ])
            .validate(),
            Ok(())
        );
        let ten: Vec<(u32, u32)> = (0..10)
            .map(|i| (i * 8, if i == 9 { 255 } else { 50 + i * 20 }))
            .collect();
        assert_eq!(custom(&ten).validate(), Ok(()));
    }

    /// Between two points the kernel is given the line, a trip per degree,
    /// each duty the line's rounded half up -- never off by more than half a
    /// step of 255, and never in the range the fan has not been measured to
    /// start in.
    #[test]
    fn the_kernel_is_given_the_line_one_degree_at_a_time() {
        let curve = custom(&[(35, 50), (45, 80), (72, 255)]);
        assert_eq!(curve.duty_at(20.0), 0);
        assert_eq!(curve.duty_at(35.0), 50);
        assert_eq!(curve.duty_at(40.0), 65);
        assert_eq!(curve.duty_at(44.9), 77);
        assert_eq!(curve.duty_at(45.0), 80);
        assert_eq!(curve.duty_at(71.9), 249);
        assert_eq!(curve.duty_at(90.0), 255);

        let ten: Vec<(u32, u32)> = (0..10)
            .map(|i| (i * 8, if i == 9 { 255 } else { 50 + i * 20 }))
            .collect();
        for points in [&FROM_ZERO[..], &ten[..], &[(0, 0), (75, 255)][..]] {
            let curve = custom(points);
            let steps = curve.kernel_steps();
            assert!(steps.len() <= 76, "{points:?}");
            for pair in steps.windows(2) {
                assert!(pair[0].0 < pair[1].0 && pair[0].1 < pair[1].1, "{pair:?}");
            }
            for &(celsius, pwm) in &steps {
                assert!(fan_pwm_allowed(pwm), "{celsius} °C → {pwm}");
                let exact = exact_line(&curve, celsius);
                assert!(
                    (f64::from(pwm) - exact).abs() <= 0.5 || exact < f64::from(FAN_PWM_MIN_RUNNING),
                    "{celsius} °C: {pwm} against {exact}"
                );
            }
            assert_eq!(steps.last().unwrap().1, 255);
        }
    }

    /// From off, the fan stays off until the line reaches a running duty.
    #[test]
    fn a_line_from_off_starts_the_fan_at_a_running_duty() {
        let curve = custom(&[(30, 0), (40, 100), (60, 255)]);
        assert_eq!(curve.kernel_steps()[0], (35, 50));
        assert_eq!(curve.duty_at(34.9), 0);
        assert_eq!(
            curve.polyline(80.0),
            [
                (0.0, 0.0),
                (30.0, 0.0),
                (35.0, 0.0),
                (35.0, 50.0),
                (40.0, 100.0),
                (60.0, 255.0),
                (80.0, 255.0)
            ]
        );
    }

    #[test]
    fn the_polyline_is_what_a_graph_draws() {
        let curve = custom(&[(35, 50), (45, 80), (72, 255)]);
        assert_eq!(
            curve.polyline(80.0),
            [
                (0.0, 0.0),
                (35.0, 0.0),
                (35.0, 50.0),
                (45.0, 80.0),
                (72.0, 255.0),
                (80.0, 255.0)
            ]
        );
        // From 0 °C and off: off until the line reaches a running duty,
        // which here is its second point.
        let from_zero = custom(&[(0, 0), (30, 50), (70, 255)]);
        assert_eq!(
            from_zero.polyline(80.0),
            [
                (0.0, 0.0),
                (30.0, 0.0),
                (30.0, 50.0),
                (70.0, 255.0),
                (80.0, 255.0)
            ]
        );
        // The board's steps are drawn as steps.
        let board = FanCurve::board().polyline(80.0);
        assert!(board.contains(&(54.0, 50.0)) && board.contains(&(55.0, 100.0)));
    }

    /// What the kernel was given comes back as a few points, not one per
    /// degree, and as the same fan.
    #[test]
    fn a_curve_written_to_the_tree_comes_back_as_the_same_fan() {
        let ten: Vec<(u32, u32)> = (0..10)
            .map(|i| (i * 8, if i == 9 { 255 } else { 50 + i * 20 }))
            .collect();
        for points in [
            &FROM_ZERO[..],
            &ten[..],
            &[(30, 50), (55, 100), (60, 150), (65, 200), (70, 255)][..],
            &[(30, 50), (40, 75), (60, 125), (65, 200), (75, 255)][..],
        ] {
            let curve = custom(points);
            let read = FanCurve::from_device_tree(&curve.cooling_levels(), &curve.temp_trips())
                .unwrap_or_else(|| panic!("{points:?} not read back"));
            assert!(read.same_kernel(&curve), "{points:?} → {:?}", read.points);
            assert!(
                read.points.len() <= FAN_CURVE_MAX_POINTS,
                "{points:?} → {:?}",
                read.points
            );
            assert_eq!(read.validate(), Ok(()), "{points:?} → {:?}", read.points);
        }
    }

    #[test]
    fn a_custom_curve_equal_to_a_preset_is_read_back_as_that_preset() {
        for preset in [FanCurve::balanced(), FanCurve::board()] {
            let mut custom = preset.clone();
            custom.profile = FanProfile::Custom;
            let read =
                FanCurve::from_device_tree(&custom.cooling_levels(), &custom.temp_trips()).unwrap();
            assert_eq!(read, preset);
        }
    }

    /// Only where the fan would differ is marked as changed.
    #[test]
    fn a_change_is_marked_only_where_the_fan_differs() {
        let before = custom(&[(30, 50), (40, 75), (60, 125), (65, 200), (75, 255)]);
        assert!(before.changed_ranges(&before.clone(), 80).is_empty());
        let mut after = before.clone();
        assert!(after.set_pwm(1, 100));
        let ranges = after.changed_ranges(&before, 80);
        // Both lines into and out of the moved point, and nothing else.
        assert_eq!(ranges, [(30, 60)]);
        let piece = before.polyline_within(80.0, 30.0, 60.0);
        assert_eq!(piece.first(), Some(&(30.0, 50.0)));
        assert_eq!(piece[1], (40.0, 75.0));
        assert_eq!(piece.last(), Some(&(60.0, 125.0)));
        // A cut inside a piece gets a corner on the line.
        let inside = before.polyline_within(80.0, 35.0, 50.0);
        assert_eq!(inside, [(35.0, 62.5), (40.0, 75.0), (50.0, 100.0)]);
    }

    /// The exact line between two points, for checking the rounding.
    fn exact_line(curve: &FanCurve, celsius: u32) -> f64 {
        let at = curve
            .points
            .windows(2)
            .find(|pair| celsius >= pair[0].temperature_c && celsius <= pair[1].temperature_c)
            .unwrap();
        let (a, b) = (at[0], at[1]);
        f64::from(a.pwm)
            + f64::from(b.pwm - a.pwm) * f64::from(celsius - a.temperature_c)
                / f64::from(b.temperature_c - a.temperature_c)
    }

    #[test]
    fn unsafe_curves_are_refused() {
        // Duplicate temperatures.
        assert_eq!(
            custom(&[(40, 50), (40, 100), (70, 255)]).validate(),
            Err(FanCurveError::TemperatureNotIncreasing { index: 1 })
        );
        // Temperatures going down.
        assert_eq!(
            custom(&[(50, 50), (45, 100), (70, 255)]).validate(),
            Err(FanCurveError::TemperatureNotIncreasing { index: 1 })
        );
        // Duty going down.
        assert_eq!(
            custom(&[(40, 100), (50, 50), (70, 255)]).validate(),
            Err(FanCurveError::PwmDecreasing { index: 1 })
        );
        // Beyond the register.
        assert_eq!(
            custom(&[(40, 50), (70, 300)]).validate(),
            Err(FanCurveError::PwmOutOfRange { index: 1, pwm: 300 })
        );
        // Between off and the lowest duty seen to start the fan.
        assert_eq!(
            custom(&[(40, 20), (70, 255)]).validate(),
            Err(FanCurveError::PwmBelowRunning { index: 0, pwm: 20 })
        );
        assert_eq!(
            custom(&[(40, 49), (70, 255)]).validate(),
            Err(FanCurveError::PwmBelowRunning { index: 0, pwm: 49 })
        );
        // The last step short of full, including a curve that turns the fan off.
        assert_eq!(
            custom(&[(40, 50), (70, 250)]).validate(),
            Err(FanCurveError::FinalNotFull { pwm: 250 })
        );
        assert_eq!(
            custom(&[(0, 0), (70, 0)]).validate(),
            Err(FanCurveError::FinalNotFull { pwm: 0 })
        );
        // The case the first brief named: 95 °C at PWM 20.
        assert!(custom(&[(50, 50), (95, 20)]).validate().is_err());
        // Full duty only after the SoC has started throttling at 75 °C.
        assert_eq!(
            custom(&[(50, 50), (76, 255)]).validate(),
            Err(FanCurveError::TemperatureOutOfRange {
                index: 1,
                temperature_c: 76
            })
        );
        // Too few or too many points.
        assert_eq!(
            custom(&[(60, 255)]).validate(),
            Err(FanCurveError::PointCount(1))
        );
        assert_eq!(custom(&[]).validate(), Err(FanCurveError::PointCount(0)));
        let eleven: Vec<(u32, u32)> = (0..11)
            .map(|i| (i * 6, if i == 10 { 255 } else { 60 }))
            .collect();
        assert_eq!(
            custom(&eleven).validate(),
            Err(FanCurveError::PointCount(11))
        );
    }

    #[test]
    fn a_preset_is_named_not_described() {
        assert_eq!(
            FanCurve::resolve(FanProfile::Cool, None),
            Ok(FanCurve::preset(FanProfile::Cool).unwrap())
        );
        assert!(FanCurve::resolve(FanProfile::Balanced, FanProfile::Balanced.preset()).is_ok());
        assert_eq!(
            FanCurve::resolve(FanProfile::Balanced, FanProfile::Cool.preset()),
            Err(FanCurveError::PresetMismatch(FanProfile::Balanced))
        );
        assert_eq!(
            FanCurve::resolve(FanProfile::Custom, None),
            Err(FanCurveError::MissingPoints)
        );
    }

    #[test]
    fn a_tree_not_in_this_shape_is_not_claimed_as_a_curve() {
        assert_eq!(
            FanCurve::from_device_tree(&[10, 50, 100], &[(50000, 1), (55000, 2)]),
            None
        );
        assert_eq!(
            FanCurve::from_device_tree(&[0, 50, 100], &[(50000, 2), (55000, 1)]),
            None
        );
        assert_eq!(
            FanCurve::from_device_tree(&[0, 50], &[(50000, 1), (55000, 2)]),
            None
        );
        assert_eq!(FanCurve::from_device_tree(&[0, 50], &[(50500, 1)]), None);
    }

    // ------------------------------------------------------------ editing

    /// Typed from a keyboard: whole percent to duty is one rounding, the same
    /// every time, and the gap below the running minimum stays closed.
    #[test]
    fn a_typed_percent_is_one_duty_and_the_gap_is_refused() {
        assert_eq!(fan_pwm_from_percent(0), Some(0));
        assert_eq!(fan_pwm_from_percent(19), Some(48));
        assert_eq!(fan_pwm_from_percent(20), Some(51));
        assert_eq!(fan_pwm_from_percent(50), Some(128));
        assert_eq!(fan_pwm_from_percent(100), Some(255));
        assert_eq!(fan_pwm_from_percent(101), None);
        // Every whole percent maps back to itself on the screen.
        for percent in 0..=100 {
            let pwm = fan_pwm_from_percent(percent).unwrap();
            assert_eq!(fan_pwm_percent(pwm).round() as u32, percent);
        }

        let mut curve = FanCurve::balanced();
        // 19 % is 48: in the gap, refused, nothing moved.
        assert!(!curve.set_pwm(0, fan_pwm_from_percent(19).unwrap()));
        assert_eq!(curve, FanCurve::balanced());
        // Off is allowed.
        assert!(curve.set_pwm(0, 0));
        assert_eq!(curve.points[0], FanPoint::new(50, 0));
        assert_eq!(curve.profile, FanProfile::Custom);
        // Past the next point's duty: refused.
        assert!(!curve.set_pwm(0, 101));
        // The full-duty point stays full.
        assert!(!curve.set_pwm(4, 200));
        assert!(curve.set_pwm(4, 255));
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn a_typed_temperature_stays_between_its_neighbours() {
        let mut curve = FanCurve::balanced();
        // Same value: accepted, and it is still the preset.
        assert!(curve.set_temperature(1, 55));
        assert_eq!(curve.profile, FanProfile::Balanced);
        assert!(!curve.set_temperature(1, 50));
        assert!(!curve.set_temperature(1, 60));
        assert!(curve.set_temperature(1, 59));
        assert!(curve.set_temperature(0, 0));
        assert!(!curve.set_temperature(4, 76));
        assert!(curve.set_temperature(4, 75));
        assert!(!curve.set_temperature(9, 10));
        assert_eq!(curve.points[0].temperature_c, 0);
        assert_eq!(curve.profile, FanProfile::Custom);
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn editing_a_preset_makes_it_custom_and_every_point_is_editable() {
        let mut curve = FanCurve::balanced();
        assert!(curve.step_temperature(0, false));
        assert_eq!(curve.profile, FanProfile::Custom);
        assert_eq!(curve.points[0].temperature_c, 49);
        for index in 0..curve.points.len() {
            let mut copy = FanCurve::balanced();
            assert!(copy.step_temperature(index, true) || copy.step_temperature(index, false));
            assert_eq!(copy.validate(), Ok(()));
        }
    }

    #[test]
    fn a_curve_can_be_walked_down_to_zero_degrees() {
        let mut curve = FanCurve::balanced();
        while curve.step_temperature(0, false) {}
        assert_eq!(curve.points[0].temperature_c, 0);
        assert!(!curve.step_temperature(0, false));
        assert_eq!(curve.validate(), Ok(()));
    }

    /// Stepping a duty down from the lowest running duty lands on off, and up
    /// from off lands on the lowest running duty: the stall range is jumped.
    #[test]
    fn duty_steps_jump_the_unmeasured_range() {
        let mut curve = custom(&[(30, 50), (50, 100), (70, 255)]);
        assert!(curve.step_pwm(0, false));
        assert_eq!(curve.points[0].pwm, 0);
        assert!(!curve.step_pwm(0, false));
        assert!(curve.step_pwm(0, true));
        assert_eq!(curve.points[0].pwm, 50);
        assert!(curve.step_pwm(0, true));
        assert_eq!(curve.points[0].pwm, 55);
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn a_point_never_passes_its_neighbours() {
        let mut curve = custom(&[(40, 50), (41, 60), (70, 255)]);
        // No room between 40 and 41.
        assert!(!curve.step_temperature(0, true));
        assert!(!curve.step_temperature(1, false));
        // Duty held between the points either side.
        let mut curve = custom(&[(40, 100), (50, 105), (60, 110), (70, 255)]);
        assert!(curve.step_pwm(1, true));
        assert!(!curve.step_pwm(1, true));
        assert!(curve.step_pwm(1, false));
        assert!(curve.step_pwm(1, false));
        assert!(!curve.step_pwm(1, false));
        // The last point is full duty and stays there; its temperature stops
        // at the SoC's passive trip.
        let mut curve = custom(&[(40, 50), (74, 255)]);
        assert!(!curve.step_pwm(1, false));
        assert!(curve.step_temperature(1, true));
        assert!(!curve.step_temperature(1, true));
        assert_eq!(curve.points[1].temperature_c, FAN_TEMP_MAX_C);
    }

    #[test]
    fn points_are_added_between_and_removed_within_the_limits() {
        let mut curve = FanCurve::balanced();
        let before = curve.clone();
        assert_eq!(curve.insert_after(1), Some(2));
        // On the line between 55 and 60 °C: the fan is the same until the new
        // point is moved.
        assert_eq!(curve.points[2], FanPoint::new(57, 120));
        assert!(curve.same_kernel(&before));
        assert_eq!(curve.profile, FanProfile::Custom);
        assert_eq!(curve.validate(), Ok(()));
        // From the last point, the new one goes before it.
        assert_eq!(curve.insert_after(5), Some(5));
        assert_eq!(curve.validate(), Ok(()));
        // No room between adjacent degrees.
        let mut tight = custom(&[(40, 50), (41, 255)]);
        assert_eq!(tight.insert_after(0), None);
        // Up to the ceiling and no further.
        // Always into the widest gap: inserting after the same point halves
        // one gap until there is no whole degree left in it, which is right
        // but is not what this checks.
        let mut curve = custom(&[(0, 0), (75, 255)]);
        loop {
            let widest = (0..curve.points.len() - 1)
                .max_by_key(|&i| curve.points[i + 1].temperature_c - curve.points[i].temperature_c)
                .unwrap();
            if curve.insert_after(widest).is_none() {
                break;
            }
        }
        assert_eq!(curve.points.len(), FAN_CURVE_MAX_POINTS);
        assert_eq!(curve.validate(), Ok(()));
        // Down to the floor and no further; the full-duty point stays.
        while curve.remove(0) {}
        assert_eq!(curve.points.len(), FAN_CURVE_MIN_POINTS);
        assert_eq!(curve.points.last().unwrap().pwm, FAN_PWM_MAX);
        let mut two = custom(&[(40, 50), (70, 255)]);
        assert!(!two.remove(1));
        assert!(!two.remove(0));
    }

    /// A new point between off and a running duty is not left in the stall
    /// range.
    #[test]
    fn an_inserted_point_never_lands_in_the_unmeasured_range() {
        let mut curve = custom(&[(30, 0), (50, 60), (70, 255)]);
        let at = curve.insert_after(0).unwrap();
        assert!(fan_pwm_allowed(curve.points[at].pwm));
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn a_custom_curve_round_trips_through_json() {
        let curve = custom(&FROM_ZERO);
        let json = serde_json::to_string(&curve).unwrap();
        assert_eq!(serde_json::from_str::<FanCurve>(&json).unwrap(), curve);
        let request = Request::FanCurveSet {
            profile: FanProfile::Custom,
            points: Some(curve.points.clone()),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
    }

    #[test]
    fn fan_requests_are_snake_case_on_the_wire() {
        let set: Request = serde_json::from_str(
            r#"{"command":"fan_curve_set","profile":"custom","points":[
                {"temperature_c":0,"pwm":0},{"temperature_c":40,"pwm":50},
                {"temperature_c":70,"pwm":255}]}"#,
        )
        .unwrap();
        assert!(matches!(
            set,
            Request::FanCurveSet {
                profile: FanProfile::Custom,
                points: Some(ref points)
            } if points.len() == 3
        ));
        let preset: Request =
            serde_json::from_str(r#"{"command":"fan_curve_set","profile":"cool"}"#).unwrap();
        assert_eq!(
            preset,
            Request::FanCurveSet {
                profile: FanProfile::Cool,
                points: None
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"command":"fan_curve_reset"}"#).unwrap(),
            Request::FanCurveReset
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"command":"fan_status"}"#).unwrap(),
            Request::FanStatus
        );
        assert!(
            serde_json::from_str::<Request>(r#"{"command":"fan_curve_set","profile":"quiet"}"#)
                .is_err()
        );
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
