//! The display, as the daemon, the interfaces and `mediaboxctl` speak of it.
//!
//! Five things are kept apart here, because each has a different author and
//! mixing any two of them is how a requested value was once reported as the
//! wire:
//!
//! * **user requested** -- [`OutputSetting`], kept on disk, and a new one on
//!   trial ([`OutputTrial`]);
//! * **policy selected** -- what that setting resolves to on the sink plugged
//!   in now ([`SelectedOutput`]), by the HDMI rules in `mediabox-platform`;
//! * **DRM applied** -- what the display's owner committed ([`AppliedOutput`]),
//!   reported by the owner itself over the local socket ([`OwnerReport`]),
//!   never through [`crate::Request`];
//! * **driver observed** -- what the display controller says it sends
//!   ([`ObservedOutput`]), read by the observer from debugfs, `Unknown` when
//!   that cannot be read;
//! * **physical** -- the transmitter PHY's own clock, where the board publishes
//!   it, in the same place.
//!
//! What the sink is and what it offers comes from the observer alone
//! ([`DisplayState`], [`OutputOffer`]): a controller on the LAN can ask for a
//! setting and read all of this, and cannot tell the daemon what the EDID,
//! the offer, the wire or the applied state is.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ColorMode, ModeTiming, Refresh, TimingKey};

/// Which mode the television is driven at, as a person has decided it.
///
/// `Auto` first, as the reference Android box lists it: the largest mode in
/// the panel's own shape at the fastest refresh the link carries in any
/// format. A fixed choice names one timing the sink listed, by its identity:
/// two timings can share a size and a refresh and still be two modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolutionChoice {
    #[default]
    Auto,
    Timing { key: TimingKey },
}

/// What a person chose for the display, and the display it was chosen on.
///
/// This is the reference Android box's model, read out of its own
/// `systemcontrol` (Amlogic): one record -- `hdmimode`, `is.bestmode`, and a
/// colour per mode as `<mode>_deepcolor` -- bound to the sink it was made on.
/// When a display with a different EDID is plugged in the box does not carry
/// the choice over: it takes the best mode and binds the record to the new
/// sink. A choice made for one display is never tried on another, which is
/// how a stale mode once panicked this board.
///
/// The sink is named by the SHA-256 of its whole, validated EDID. The block
/// checksums the reference box binds to are not an identity -- two different
/// EDIDs can share them -- and a record written that way is migrated once
/// ([`LegacySetting`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSetting {
    /// SHA-256 of the EDID of the sink this was chosen on. Empty for a box
    /// that has never been told anything.
    #[serde(default)]
    pub edid_sha256: String,
    #[serde(default)]
    pub resolution: ResolutionChoice,
    /// A colour mode per timing. A timing with no entry is on `Auto`.
    #[serde(default)]
    pub colours: BTreeMap<TimingKey, ColorMode>,
}

impl OutputSetting {
    /// The setting as it applies to the sink whose EDID is `edid_sha256`:
    /// this one if it was made there, `Auto` bound to that sink otherwise.
    pub fn for_sink(&self, edid_sha256: &str) -> OutputSetting {
        if self.edid_sha256 == edid_sha256 {
            self.clone()
        } else {
            OutputSetting {
                edid_sha256: edid_sha256.to_string(),
                ..Default::default()
            }
        }
    }
}

/// The version of `output.json` this build writes.
pub const OUTPUT_SETTING_SCHEMA: u32 = 2;

#[derive(Serialize, Deserialize)]
struct Stored {
    schema: u32,
    #[serde(flatten)]
    setting: OutputSetting,
}

/// What `output.json` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredSetting {
    Current(OutputSetting),
    /// Written before the record moved to the EDID's SHA-256 and to timing
    /// keys. Read, migrated once against the sink it is found on, and never
    /// written again.
    Legacy(LegacySetting),
}

impl StoredSetting {
    /// `None` for anything that is neither: a box that has never been told,
    /// or a file that is not this record.
    pub fn parse(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
        match value.get("schema").and_then(serde_json::Value::as_u64) {
            Some(schema) if schema == u64::from(OUTPUT_SETTING_SCHEMA) => {
                let stored: Stored = serde_json::from_value(value).ok()?;
                Some(Self::Current(stored.setting))
            }
            Some(_) => None,
            None => serde_json::from_value(value).ok().map(Self::Legacy),
        }
    }

    /// The kept setting for the sink `offer` describes, with the modes the
    /// kernel lists for it: the current record as it applies there, or the
    /// legacy one migrated -- and what the migration could not carry over.
    pub fn for_offer(&self, offer: &OutputOffer, kernel: &[ModeTiming]) -> (OutputSetting, Vec<String>) {
        match self {
            Self::Current(setting) => (setting.for_sink(&offer.edid_sha256), Vec::new()),
            Self::Legacy(legacy) => legacy.migrate(offer, kernel),
        }
    }
}

impl OutputSetting {
    /// The record as it is written to disk.
    pub fn to_stored(&self) -> String {
        let stored = Stored {
            schema: OUTPUT_SETTING_SCHEMA,
            setting: self.clone(),
        };
        format!("{}\n", serde_json::to_string(&stored).unwrap_or_default())
    }
}

/// `output.json` as it was: bound to the EDID's block checksums, a mode by
/// size and millihertz, colours by label.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacySetting {
    #[serde(default)]
    pub sink: String,
    #[serde(default)]
    pub resolution: LegacyResolution,
    #[serde(default)]
    pub colours: BTreeMap<String, ColorMode>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LegacyResolution {
    #[default]
    Auto,
    Fixed {
        width: u16,
        height: u16,
        refresh_mhz: u32,
        interlaced: bool,
    },
}

impl LegacySetting {
    /// The record for the sink `offer` describes.
    ///
    /// The old binding is accepted once: a record whose checksums are this
    /// sink's is taken to have been made on it, and from then on it is bound
    /// to the SHA-256 -- so a second display that shares the checksums is a
    /// different display. A record for other checksums is another sink's, and
    /// this one is on `Auto`.
    ///
    /// A mode kept as size and millihertz names the one timing the kernel
    /// lists that matches it the way the old code matched it (within five
    /// millihertz); a colour kept by label, the one timing with that label.
    /// When more than one timing matches, nothing is guessed: that choice is
    /// `Auto`, and the notes say so.
    pub fn migrate(&self, offer: &OutputOffer, kernel: &[ModeTiming]) -> (OutputSetting, Vec<String>) {
        let mut notes = Vec::new();
        let mut setting = OutputSetting {
            edid_sha256: offer.edid_sha256.clone(),
            ..Default::default()
        };
        if self.sink != offer.legacy_checkvalue {
            if !self.sink.is_empty() {
                notes.push(format!(
                    "eski kayıt başka bir ekranın ({}); bu ekran otomatik",
                    self.sink
                ));
            }
            return (setting, notes);
        }
        let mut keys: Vec<TimingKey> = kernel.iter().map(|mode| mode.key()).collect();
        keys.sort();
        keys.dedup();
        if let LegacyResolution::Fixed {
            width,
            height,
            refresh_mhz,
            interlaced,
        } = self.resolution
        {
            let matching: Vec<TimingKey> = keys
                .iter()
                .copied()
                .filter(|key| {
                    let mode = key.timing();
                    mode.hdisplay == width
                        && mode.vdisplay == height
                        && mode.interlaced() == interlaced
                        && mode.refresh().millihertz().abs_diff(refresh_mhz) <= 5
                })
                .collect();
            match matching.as_slice() {
                [key] if offer.modes().any(|mode| mode.timing_key == *key) => {
                    setting.resolution = ResolutionChoice::Timing { key: *key };
                }
                [] | [_] => notes.push(format!(
                    "eski seçim {width}x{height} {refresh_mhz} mHz bu ekranda yok; otomatik"
                )),
                several => notes.push(format!(
                    "eski seçim {width}x{height} {refresh_mhz} mHz {} ayrı zamanlamaya uyuyor; \
                     tahmin edilmedi, otomatik",
                    several.len()
                )),
            }
        }
        for (label, colour) in &self.colours {
            let matching: Vec<TimingKey> = keys
                .iter()
                .copied()
                .filter(|key| key.timing().label() == *label)
                .collect();
            match matching.as_slice() {
                [key] if offer.modes().any(|mode| mode.timing_key == *key) => {
                    setting.colours.insert(*key, *colour);
                }
                [] | [_] => notes.push(format!("{label} rengi bu ekranda yok; otomatik")),
                several => notes.push(format!(
                    "{label} rengi {} ayrı zamanlamaya uyuyor; tahmin edilmedi, otomatik",
                    several.len()
                )),
            }
        }
        (setting, notes)
    }
}

/// Why a colour mode cannot be sent at a mode. Each is one rule of the
/// kernel's, and [`Refusal::text`] names the specification it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Refusal {
    /// CTA-861-F s5.4: VIC 1 is eight bits only.
    EightBitOnly,
    /// No HDMI VSDB: a DVI sink takes RGB at eight bits.
    NotHdmi,
    /// Y420VDB: the sink takes this mode as 4:2:0 and nothing else.
    Only420,
    /// 4:2:0 at a mode in neither the Y420VDB nor the Y420CMDB.
    No420Here,
    /// The sink does not declare this format (CTA-861 extension byte 3).
    FormatNotDeclared,
    /// The sink does not declare this depth for this format.
    DepthNotDeclared { bits: u8 },
    /// The source has no such depth.
    SourceDepth { bits: u8, max: u8 },
    /// Over the sink's declared character rate.
    OverSink { need_khz: u32, max_khz: u32 },
    /// Over what this transmitter sends.
    OverSource { need_khz: u32, max_khz: u32 },
}

impl Refusal {
    pub fn text(self, format: crate::ColorFormat) -> String {
        match self {
            Refusal::EightBitOnly => {
                "640×480 (VIC 1) yalnızca 8 bit gönderilebilir (CTA-861-F §5.4).".into()
            }
            Refusal::NotHdmi => {
                "Ekran HDMI bloğu bildirmiyor; DVI ekrana yalnızca RGB 8 bit gider.".into()
            }
            Refusal::Only420 => {
                "Ekran bu modu yalnızca 4:2:0 ile kabul ediyor (CTA-861 Y420VDB).".into()
            }
            Refusal::No420Here => "Ekran bu modda 4:2:0 bildirmiyor; 4:2:0 yalnızca \
                 Y420 listesindeki modlarda (CTA-861 Y420VDB/Y420CMDB)."
                .into(),
            Refusal::FormatNotDeclared => format!(
                "Ekran {} bildirmiyor (CTA-861 uzantı bloğu).",
                format.label()
            ),
            Refusal::DepthNotDeclared { bits } => match format {
                crate::ColorFormat::Ycbcr420 => format!(
                    "Ekran 4:2:0'da {bits} bit bildirmiyor (HF-VSDB DC_{}bit_420).",
                    bits * 3
                ),
                _ => format!(
                    "Ekran {} için {bits} bit bildirmiyor (HDMI VSDB DC_{}bit).",
                    format.label(),
                    bits * 3
                ),
            },
            Refusal::SourceDepth { bits, max } => {
                format!("Bu kart en çok {max} bit gönderiyor; {bits} bit yok.")
            }
            Refusal::OverSink { need_khz, max_khz } => format!(
                "Gereken {} MHz; bu giriş en çok {} MHz taşıyor (ekranın EDID'i).",
                need_khz / 1000,
                max_khz / 1000
            ),
            Refusal::OverSource { need_khz, max_khz } => format!(
                "Gereken {} MHz; bu kart en çok {} MHz gönderiyor.",
                need_khz / 1000,
                max_khz / 1000
            ),
        }
    }
}

/// One colour mode at one mode: what it costs on the wire, whether it can be
/// sent there, and whether HDR10 can be sent in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColourCell {
    pub mode: ColorMode,
    pub rate_khz: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<Refusal>,
    /// Sendable, and every HDR10 condition holds for this format and depth:
    /// the sink's PQ transfer and Static Metadata Type 1, BT.2020 in this
    /// encoding (RGB or YCC), ten bits or more, and a source that signals it.
    #[serde(default)]
    pub hdr10: bool,
}

/// One mode the kernel lists for the display, with every colour cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputModeOffer {
    /// `3840x2160p59.94`: what a person reads. Unique within one offer; never
    /// what a choice is kept under.
    pub label: String,
    pub width: u16,
    pub height: u16,
    pub refresh_mhz: u32,
    pub interlaced: bool,
    pub pixel_clock_khz: u32,
    pub htotal: u16,
    pub vtotal: u16,
    pub preferred: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vic: Option<u8>,
    /// The timing's identity: what a choice names and what the owner finds
    /// the kernel's mode by.
    pub timing_key: TimingKey,
    /// The whole timing, so that what is derived from it -- the refresh Kodi
    /// and sway are told, the rate on the wire -- is the kernel's numbers.
    pub timing: ModeTiming,
    pub cells: Vec<ColourCell>,
    /// What `Auto` sends here for SDR, and for HDR10. `None` when nothing
    /// fits, or, for HDR, when no cell carries HDR10.
    pub auto_sdr: Option<ColorMode>,
    pub auto_hdr: Option<ColorMode>,
}

impl OutputModeOffer {
    pub fn allowed(&self) -> impl Iterator<Item = ColorMode> + '_ {
        self.cells
            .iter()
            .filter(|cell| cell.refused.is_none())
            .map(|cell| cell.mode)
    }

    pub fn carries(&self, mode: ColorMode) -> bool {
        self.allowed().any(|allowed| allowed == mode)
    }

    /// Whether HDR10 can be sent in `mode` here.
    pub fn carries_hdr10(&self, mode: ColorMode) -> bool {
        self.cells.iter().any(|cell| cell.mode == mode && cell.hdr10)
    }

    pub fn hdr10(&self) -> bool {
        self.auto_hdr.is_some()
    }

    /// The exact refresh; an interlaced mode's is its field rate.
    pub fn refresh(&self) -> Refresh {
        self.timing.refresh()
    }

    /// What `colour` costs on the wire here, in kHz: the platform's own
    /// figure when it computed one for this cell, the timing's otherwise.
    pub fn character_rate_khz(&self, colour: ColorMode) -> u32 {
        match self.cells.iter().find(|cell| cell.mode == colour) {
            Some(cell) => cell.rate_khz,
            None => self.timing.hdmi_character_rate_khz(colour),
        }
    }

    pub fn choice(&self) -> ResolutionChoice {
        ResolutionChoice::Timing {
            key: self.timing_key,
        }
    }

    /// Whether a choice names this mode.
    pub fn is(&self, choice: ResolutionChoice) -> bool {
        choice == self.choice()
    }
}

/// Every mode of one size, fastest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputGroup {
    pub width: u16,
    pub height: u16,
    /// The industry's name for the size (`4K UHD`, `Full HD`), empty when it
    /// has none.
    pub name: String,
    /// No mode of this size is a CTA-861 television mode other than VIC 1:
    /// these are a computer monitor's modes (VESA DMT).
    pub computer: bool,
    pub modes: Vec<OutputModeOffer>,
}

/// What the display declared about its link, and what the source can send,
/// for the "EDID" sheet. Each HDR10 condition is its own line: a sink that
/// declares PQ and nothing else does not take HDR10.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLink {
    pub max_character_rate_khz: u32,
    /// `HDMI VSDB`, `HF-VSDB`, or empty when the sink declared no rate.
    pub declared_by: String,
    pub is_hdmi: bool,
    pub ycbcr444: bool,
    pub ycbcr422: bool,
    pub rgb_deep: Vec<u8>,
    pub ycbcr420_deep: Vec<u8>,
    pub y420_only: Vec<u8>,
    pub y420_also: Vec<u8>,
    /// HDR Static Metadata data block: the EOTFs.
    pub st2084: bool,
    pub hlg: bool,
    /// The same block: Static Metadata Descriptor Type 1, which is what the
    /// HDR10 infoframe carries.
    #[serde(default)]
    pub static_metadata_type1: bool,
    /// Colorimetry data block: BT.2020 for RGB, and for YCbCr.
    #[serde(default)]
    pub bt2020_rgb: bool,
    #[serde(default)]
    pub bt2020_ycc: bool,
    pub source_max_khz: u32,
    pub source_max_bits: u8,
    /// Whether the source profile signals HDR10 at all.
    #[serde(default)]
    pub source_hdr10: bool,
    /// The source profile the limits above are from, and how it was matched
    /// (`rk3588-vendor61-dw-hdmi-qp@2 (matched)`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_profile: String,
}

/// Everything the display screen draws, computed from the modes the kernel
/// lists for the display plugged in and from its EDID, by the rules in
/// `mediabox_platform::video`. Every interface draws this; none of them
/// computes a rule of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputOffer {
    /// SHA-256 of the EDID exactly as received, whole and valid: the sink's
    /// identity, and what a kept setting is bound to. There is no offer for
    /// an EDID that failed its checks.
    pub edid_sha256: String,
    /// The EDID's block checksums, as the reference box and the old record
    /// kept them. Not an identity; here to migrate a record written that way
    /// ([`LegacySetting`]) and to show.
    pub legacy_checkvalue: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink_name: Option<String>,
    pub connector: String,
    pub link: OutputLink,
    /// The label of the mode `Auto` resolves to.
    pub auto: String,
    pub groups: Vec<OutputGroup>,
}

impl OutputOffer {
    pub fn modes(&self) -> impl Iterator<Item = &OutputModeOffer> {
        self.groups.iter().flat_map(|group| group.modes.iter())
    }

    pub fn mode(&self, label: &str) -> Option<&OutputModeOffer> {
        self.modes().find(|mode| mode.label == label)
    }

    pub fn mode_by_key(&self, key: TimingKey) -> Option<&OutputModeOffer> {
        self.modes().find(|mode| mode.timing_key == key)
    }

    /// The mode a resolution choice resolves to here: the chosen one when this
    /// display lists it and something can be sent at it, `Auto` otherwise.
    pub fn resolve(&self, choice: ResolutionChoice) -> Option<&OutputModeOffer> {
        self.modes()
            .find(|mode| mode.is(choice) && mode.allowed().next().is_some())
            .or_else(|| self.mode(&self.auto))
    }

    /// The colour sent at `mode` for an HDR10 film under `setting`: the
    /// chosen one when HDR10 can be sent in it, what `Auto` sends HDR10 as
    /// otherwise, `None` when nothing at this mode carries HDR10.
    pub fn hdr_colour(&self, setting: &OutputSetting, mode: &OutputModeOffer) -> Option<ColorMode> {
        setting
            .colours
            .get(&mode.timing_key)
            .copied()
            .filter(|chosen| mode.carries_hdr10(*chosen))
            .or(mode.auto_hdr)
    }

    /// The colour sent at `mode` for SDR under `setting`: the chosen one when
    /// it can be sent there, `Auto` otherwise.
    pub fn colour(&self, setting: &OutputSetting, mode: &OutputModeOffer) -> Option<ColorMode> {
        setting
            .colours
            .get(&mode.timing_key)
            .copied()
            .filter(|chosen| mode.carries(*chosen))
            .or(mode.auto_sdr)
    }

    /// Policy: what `setting` puts on the wire here.
    pub fn select(&self, setting: &OutputSetting) -> Option<SelectedOutput> {
        let mode = self.resolve(setting.resolution)?;
        Some(SelectedOutput {
            timing_key: mode.timing_key,
            label: mode.label.clone(),
            sdr: self.colour(setting, mode),
            hdr: self.hdr_colour(setting, mode),
        })
    }
}

/// A display generation, as the observer numbers it: `seq` moves when what it
/// sees materially changes, within one boot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayGeneration {
    pub boot_id: String,
    pub seq: u64,
}

/// Which output and which sink something is about: an applied report, a plan,
/// a trial. Anything bound to one identity is not about any other.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayIdentity {
    pub connector: String,
    pub edid_sha256: String,
}

/// Where something that follows the selected output goes -- its sound card,
/// its CEC adapter -- or why it goes nowhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// The display as the observer sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayState {
    pub generation: DisplayGeneration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edid_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transmitter: Option<String>,
    /// How firmly the connector is tied to that transmitter.
    pub topology: String,
    pub audio: Route,
    pub cec: Route,
    pub source_profile: String,
}

impl DisplayState {
    pub fn identity(&self) -> Option<DisplayIdentity> {
        Some(DisplayIdentity {
            connector: self.connector.clone()?,
            edid_sha256: self.edid_sha256.clone()?,
        })
        .filter(|_| self.connected)
    }
}

/// Policy: the mode and colours the kept (or trial) setting resolves to on the
/// sink plugged in now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedOutput {
    pub timing_key: TimingKey,
    pub label: String,
    pub sdr: Option<ColorMode>,
    pub hdr: Option<ColorMode>,
}

/// What the display's owner committed through DRM, as it reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedOutput {
    pub identity: DisplayIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<u64>,
    pub timing_key: TimingKey,
    pub label: String,
    /// `None`: the colour was left to the driver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<ColorMode>,
    pub hdr: bool,
}

/// A value read from somewhere that may not be there. Unknown is an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Observed<T> {
    Known(T),
    Unknown(String),
}

impl<T> Observed<T> {
    pub fn known(self) -> Option<T> {
        match self {
            Observed::Known(value) => Some(value),
            Observed::Unknown(_) => None,
        }
    }

    pub fn as_known(&self) -> Option<&T> {
        match self {
            Observed::Known(value) => Some(value),
            Observed::Unknown(_) => None,
        }
    }
}

/// What the display controller says it sends, and the transmitter PHY's own
/// clock. Nothing here is what anybody asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedOutput {
    /// The mode label the driver reports for the connector.
    pub mode: Observed<String>,
    /// The media bus format, verbatim (`UYYVYY8_0_5X24`).
    pub bus_format: Observed<String>,
    /// That bus format as a colour mode, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<ColorMode>,
    /// The transmitter PHY's clock, in kHz.
    pub phy_clock_khz: Observed<u64>,
}

impl ObservedOutput {
    /// Whether the driver's report of the mode is `applied`'s timing, as far
    /// as the report can say: it names the size, the scan and the refresh.
    ///
    /// The refresh is written two ways by the vendor drivers this runs on:
    /// rounded to a whole hertz (6.1.115: `3840x2160p60`, for 59.94 as well)
    /// or with two decimals (6.1.172: `3840x2160p60.00`, `3840x2160p23.98`).
    /// Either is read as the number it is and held to the precision it was
    /// written with -- never told apart by kernel version.
    pub fn confirms(&self, applied: &AppliedOutput) -> bool {
        let Some(reported) = self.mode.as_known() else {
            return false;
        };
        let timing = applied.timing_key.timing();
        let scan = if timing.interlaced() { "i" } else { "p" };
        let size = format!("{}x{}{}", timing.hdisplay, timing.vdisplay, scan);
        let Some(hertz) = reported.strip_prefix(&size) else {
            return false;
        };
        let refresh = timing.refresh();
        match hertz.split_once('.') {
            None => hertz.parse::<u32>().ok() == Some(refresh.hertz_rounded()),
            Some((whole, fraction))
                if !fraction.is_empty()
                    && fraction.len() <= 3
                    && fraction.bytes().all(|b| b.is_ascii_digit()) =>
            {
                let Ok(whole) = whole.parse::<u64>() else {
                    return false;
                };
                let scale = 10u64.pow(3 - fraction.len() as u32);
                let reported_mhz =
                    whole * 1000 + fraction.parse::<u64>().unwrap_or_default() * scale;
                // Half the last written digit, plus the millihertz the
                // timing's own value is rounded down by.
                let tolerance = scale / 2 + 1;
                reported_mhz.abs_diff(u64::from(refresh.millihertz())) <= tolerance
            }
            Some(_) => false,
        }
    }
}

impl OutputStatus {
    /// The mode on the wire as the driver reports it: the applied mode's own
    /// label when the report confirms it -- the driver rounds the refresh --
    /// the driver's own words otherwise, `None` when it cannot be read.
    pub fn wire_mode(&self) -> Option<String> {
        let observed = self.observed.as_ref()?;
        match &self.applied {
            Some(applied) if observed.confirms(applied) => Some(applied.label.clone()),
            _ => observed.mode.as_known().cloned(),
        }
    }

    /// The colour on the wire as the driver reports it.
    pub fn wire_colour(&self) -> Option<ColorMode> {
        self.observed.as_ref()?.colour
    }

    /// Whether the owner committed HDR signalling for this sink. What the
    /// owner did, not a measurement of the wire.
    pub fn hdr_applied(&self) -> bool {
        self.applied.as_ref().is_some_and(|applied| applied.hdr)
    }
}

/// A choice on trial: sent to the owner, not written down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputTrial {
    /// Unique across restarts of the daemon: `Keep` and `Revert` name it.
    pub id: u64,
    pub setting: OutputSetting,
    pub previous: OutputSetting,
    pub seconds_left: u32,
    /// The owner reported it committed, for this sink. Only then can it be
    /// kept.
    pub applied: bool,
}

/// The display, as the daemon knows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputStatus {
    /// What the observer sees. `None` until it has published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayState>,
    /// What can be sent to the sink plugged in now. `None` until the
    /// observer has read the kernel's mode list for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<OutputOffer>,
    /// User requested: the kept setting, as it applies to the sink in `offer`.
    pub setting: OutputSetting,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<OutputTrial>,
    /// Policy selected: what the setting on the wire resolves to here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<SelectedOutput>,
    /// DRM applied, by the owner, for this sink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<AppliedOutput>,
    /// Driver observed, and the PHY.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<ObservedOutput>,
    /// What a record written before this version could not carry over.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What the daemon tells the interfaces about the display, on the event
/// stream beside the remote's presses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "output", rename_all = "snake_case")]
pub enum OutputEvent {
    /// Put this on the wire of the output and sink `identity` names, and of
    /// no other. `trial` is set while it is on trial.
    Apply {
        identity: DisplayIdentity,
        setting: OutputSetting,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trial: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trial_seconds: Option<u32>,
    },
    /// The trial was kept.
    Kept { trial: u64 },
    /// The trial ended without being kept, and why; the `Apply` for the
    /// earlier setting comes with it.
    Reverted {
        trial: u64,
        timed_out: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The daemon's answer changed: a new generation, an owner's report.
    Changed,
}

/// What only the display's owner on this machine tells the daemon: what it
/// committed. Not part of [`crate::Request`], so no HTTP listener can even
/// parse one; the daemon takes it on its local socket, from root, and only
/// for the output and sink the observer sees now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "report", rename_all = "snake_case")]
pub enum OwnerReport {
    Applied(AppliedOutput),
    Failed {
        identity: DisplayIdentity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trial: Option<u64>,
        error: String,
    },
}

/// How long a new display setting stays on trial before it is taken back.
/// The reference for the flow is the desktop's "Keep these display settings?"
/// (Windows: 15 seconds).
pub const OUTPUT_TRIAL_SECONDS: u32 = 15;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorFormat, mode_flags};

    fn uhd(clock_khz: u32, htotal: u16) -> ModeTiming {
        ModeTiming::new(
            clock_khz,
            3840,
            4016,
            4104,
            htotal,
            0,
            2160,
            2168,
            2178,
            2250,
            0,
            mode_flags::PHSYNC | mode_flags::PVSYNC,
        )
    }

    #[test]
    fn the_record_is_read_as_what_it_is_and_nothing_else() {
        let current = OutputSetting {
            edid_sha256: "ab".repeat(32),
            resolution: ResolutionChoice::Timing { key: uhd(297_000, 4400).key() },
            colours: [(uhd(297_000, 4400).key(), ColorMode::new(ColorFormat::Ycbcr422, 10))]
                .into_iter()
                .collect(),
        };
        let text = current.to_stored();
        assert!(text.contains("\"schema\":2"));
        assert_eq!(StoredSetting::parse(&text), Some(StoredSetting::Current(current)));
        let legacy = r#"{"sink":"716b","resolution":{"kind":"auto"},"colours":{}}"#;
        assert!(matches!(StoredSetting::parse(legacy), Some(StoredSetting::Legacy(ref old)) if old.sink == "716b"));
        assert_eq!(StoredSetting::parse("not json"), None);
        assert_eq!(StoredSetting::parse(r#"{"schema":3,"edid_sha256":"x"}"#), None, "a later schema is not guessed at");
        // A current record is never read as a legacy one, or the reverse.
        assert!(serde_json::from_str::<OutputSetting>(legacy).map(|s| s.edid_sha256.is_empty()).unwrap_or(true));
    }

    #[test]
    fn a_status_calls_the_wire_only_what_the_driver_reports() {
        let key = uhd(593_407, 4400).key();
        let applied = AppliedOutput {
            identity: DisplayIdentity::default(),
            trial: None,
            timing_key: key,
            label: key.timing().label(),
            colour: Some(ColorMode::new(ColorFormat::Rgb, 8)),
            hdr: false,
        };
        let observed = |mode: Observed<String>| ObservedOutput {
            mode,
            bus_format: Observed::Unknown("no debugfs".into()),
            colour: None,
            phy_clock_khz: Observed::Unknown("no debugfs".into()),
        };
        let mut status = OutputStatus {
            applied: Some(applied),
            observed: Some(observed(Observed::Unknown("no debugfs".into()))),
            ..Default::default()
        };
        // Nothing readable: nothing is claimed, whatever was applied.
        assert_eq!(status.wire_mode(), None);
        assert_eq!(status.wire_colour(), None);
        // The driver rounds 59.94 to 60; it confirms the applied timing, whose
        // own label is the precise one.
        status.observed = Some(observed(Observed::Known("3840x2160p60".into())));
        assert_eq!(status.wire_mode().as_deref(), Some("3840x2160p59.94"));
        // The driver says something else: that is what is on the wire.
        status.observed = Some(observed(Observed::Known("1920x1080p60".into())));
        assert_eq!(status.wire_mode().as_deref(), Some("1920x1080p60"));
        // A driver that writes two decimals (6.1.172) confirms the same
        // timing, and tells 59.94 from 60 where it can.
        status.observed = Some(observed(Observed::Known("3840x2160p59.94".into())));
        assert_eq!(status.wire_mode().as_deref(), Some("3840x2160p59.94"));
        status.observed = Some(observed(Observed::Known("3840x2160p60.00".into())));
        assert_eq!(status.wire_mode().as_deref(), Some("3840x2160p60.00"));
        status.observed = Some(observed(Observed::Known("3840x2160p59.9x".into())));
        assert_eq!(status.wire_mode().as_deref(), Some("3840x2160p59.9x"));
    }
}
