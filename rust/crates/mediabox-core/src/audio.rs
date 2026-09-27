//! Sound: which device it goes to, in what form, and how loud.
//!
//! Three things are kept apart, for the reason the display keeps them apart:
//!
//! * **what a person chose** -- [`AudioSetting`], kept by the daemon in
//!   `audio.json`: a device (or `Auto`, the display's own), a form (`Auto`,
//!   PCM, or passthrough of the formats they picked), Dolby Digital
//!   transcoding on or off, and a volume;
//! * **what the hardware is** -- [`AudioDevice`], discovered by
//!   `mediabox-platform` from ALSA and the device tree every time it is
//!   asked, each with a stable identity that is never an ALSA card number,
//!   and for a display the formats its sink declares ([`SinkAudio`]) read from
//!   the ELD or, where the driver publishes none, from the same EDID the
//!   display offer comes from;
//! * **what that means for a player** -- [`AudioPlan`], computed here, by
//!   [`plan`], and nowhere else. The player, the browser and Kodi are all
//!   started from it; none of them decides anything of its own.
//!
//! Nothing is offered that the sink does not declare. A bitstream is never
//! volume-scaled: software volume is for PCM, and a plan says so when it
//! cannot apply.

use serde::{Deserialize, Serialize};

/// A compressed format that can be sent to a receiver as it is, as an
/// IEC 61937 burst over HDMI -- in mpv's own names for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    Ac3,
    Eac3,
    Dts,
    DtsHd,
    TrueHd,
}

impl AudioCodec {
    pub const ALL: [AudioCodec; 5] = [
        AudioCodec::Ac3,
        AudioCodec::Eac3,
        AudioCodec::Dts,
        AudioCodec::DtsHd,
        AudioCodec::TrueHd,
    ];

    /// What `--audio-spdif` calls it.
    pub fn mpv(self) -> &'static str {
        match self {
            AudioCodec::Ac3 => "ac3",
            AudioCodec::Eac3 => "eac3",
            AudioCodec::Dts => "dts",
            AudioCodec::DtsHd => "dts-hd",
            AudioCodec::TrueHd => "truehd",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AudioCodec::Ac3 => "Dolby Digital",
            AudioCodec::Eac3 => "Dolby Digital Plus",
            AudioCodec::Dts => "DTS",
            AudioCodec::DtsHd => "DTS-HD",
            AudioCodec::TrueHd => "Dolby TrueHD",
        }
    }

    /// The CTA-861 audio format code a Short Audio Descriptor carries it
    /// under (CTA-861-G table 37): 2 AC-3, 7 DTS, 10 E-AC-3, 11 DTS-HD,
    /// 12 MAT (TrueHD).
    pub fn from_cta(code: u8) -> Option<Self> {
        Some(match code {
            2 => AudioCodec::Ac3,
            7 => AudioCodec::Dts,
            10 => AudioCodec::Eac3,
            11 => AudioCodec::DtsHd,
            12 => AudioCodec::TrueHd,
            _ => return None,
        })
    }

    /// Whether a burst of it needs the HDMI High Bit Rate layout -- eight
    /// channels at 192 kHz. DTS-HD and TrueHD do.
    pub fn needs_hbr(self) -> bool {
        matches!(self, AudioCodec::DtsHd | AudioCodec::TrueHd)
    }
}

/// One Short Audio Descriptor, as the sink declared it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SadEntry {
    /// The CTA-861 audio format code.
    pub coding: u8,
    /// What a person reads: `LPCM`, `AC-3`, `DTS`.
    pub name: String,
    pub channels: u8,
    /// Sample rates, in hertz.
    pub rates: Vec<u32>,
    /// For AC-3, DTS and the other codes 2 to 8: the maximum bit rate, in
    /// kbit/s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_kbps: Option<u32>,
}

/// Where a sink's audio capabilities were read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapsSource {
    /// The ELD the HDMI driver published for the sound card.
    Eld,
    /// The display's EDID -- the CTA audio data block the kernel would have
    /// built the ELD from, read where the driver publishes none.
    Edid,
}

/// What a display's sink says it can play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkAudio {
    pub source: CapsSource,
    pub entries: Vec<SadEntry>,
    /// The speaker allocation, as CTA-861 names the positions (`FL/FR`,
    /// `LFE`, `FC`, `RL/RR`).
    #[serde(default)]
    pub speakers: Vec<String>,
}

impl SinkAudio {
    /// The bitstream formats the sink declares, in [`AudioCodec::ALL`] order.
    pub fn codecs(&self) -> Vec<AudioCodec> {
        let mut found: Vec<AudioCodec> = self
            .entries
            .iter()
            .filter_map(|entry| AudioCodec::from_cta(entry.coding))
            .collect();
        found.sort();
        found.dedup();
        found
    }

    pub fn declares(&self, codec: AudioCodec) -> bool {
        self.codecs().contains(&codec)
    }

    /// The most PCM channels any LPCM descriptor declares; 2 when none does
    /// (every HDMI sink takes basic audio).
    pub fn pcm_channels(&self) -> u8 {
        self.entries
            .iter()
            .filter(|entry| entry.coding == 1)
            .map(|entry| entry.channels)
            .max()
            .unwrap_or(2)
    }

    /// The highest AC-3 bit rate the sink declares, in kbit/s.
    pub fn ac3_max_kbps(&self) -> Option<u32> {
        self.entries
            .iter()
            .filter(|entry| entry.coding == 2)
            .filter_map(|entry| entry.max_kbps)
            .max()
    }
}

/// What kind of output a device is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioKind {
    Hdmi,
    DisplayPort,
    Usb,
    Analog,
    Other,
}

impl AudioKind {
    pub fn label(self) -> &'static str {
        match self {
            AudioKind::Hdmi => "HDMI",
            AudioKind::DisplayPort => "DisplayPort",
            AudioKind::Usb => "USB",
            AudioKind::Analog => "Analog",
            AudioKind::Other => "Ses kartı",
        }
    }
}

/// One real place sound can go, as the platform found it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    /// Stable across boots, plugging and probe order: `display:<transmitter>`
    /// for a display's own sound, `usb:<vid>:<pid>:<card id>` for a USB
    /// device, `alsa:<card id>` for anything else. Never a card number.
    pub id: String,
    /// What a person reads: `HDMI-A-2 · SONY TV`, `USB · DAC`.
    pub label: String,
    pub kind: AudioKind,
    /// ALSA's id for the card this boot (`rockchiphdmi1`), and its number,
    /// for a person reading a log. Neither is an identity.
    pub card_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_index: Option<u32>,
    /// The PCM players open for PCM: `hdmi:CARD=<id>,DEV=0` where the
    /// compressed-audio definition exists, `plughw:CARD=<id>,DEV=<n>`
    /// otherwise.
    pub pcm: String,
    /// Whether a compressed burst can be sent: an HDMI or DisplayPort card
    /// whose IEC 958 definition is installed.
    pub bitstream: bool,
    /// The display connector it belongs to, when it is a display's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    /// It is the sound of the output the picture is on.
    #[serde(default)]
    pub display: bool,
    /// What the sink on the other end declares, when it declares anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink: Option<SinkAudio>,
}

/// Where the sound goes, as a person chose it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioDeviceChoice {
    /// The sound of the display the picture is on.
    #[default]
    Auto,
    Device { id: String },
}

/// In what form sound is sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioMode {
    /// Every format the sink declares goes to it as it is; the rest is PCM.
    #[default]
    Auto,
    /// Everything is decoded here.
    Pcm,
    /// The formats a person picked, of those the sink declares.
    Passthrough,
}

impl AudioMode {
    pub const ALL: [AudioMode; 3] = [AudioMode::Auto, AudioMode::Pcm, AudioMode::Passthrough];

    pub fn label(self) -> &'static str {
        match self {
            AudioMode::Auto => "Otomatik",
            AudioMode::Pcm => "PCM",
            AudioMode::Passthrough => "Doğrudan geçiş",
        }
    }
}

/// What a person chose. Kept by the daemon in `audio.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSetting {
    #[serde(default)]
    pub device: AudioDeviceChoice,
    #[serde(default)]
    pub mode: AudioMode,
    /// The formats passed through under [`AudioMode::Passthrough`]. `None`:
    /// every one the sink declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formats: Option<Vec<AudioCodec>>,
    /// Encode multichannel sound that cannot go as it is to Dolby Digital.
    #[serde(default)]
    pub ac3_transcode: bool,
    /// 0 to 100, applied to PCM.
    #[serde(default = "full")]
    pub volume: u8,
    #[serde(default)]
    pub muted: bool,
}

fn full() -> u8 {
    100
}

impl Default for AudioSetting {
    fn default() -> Self {
        Self {
            device: AudioDeviceChoice::Auto,
            mode: AudioMode::Auto,
            formats: None,
            ac3_transcode: false,
            volume: 100,
            muted: false,
        }
    }
}

/// The version of `audio.json` this build writes.
pub const AUDIO_SETTING_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize)]
struct StoredAudio {
    schema: u32,
    #[serde(flatten)]
    setting: AudioSetting,
}

impl AudioSetting {
    /// `audio.json`, or `None` for a file that is not this record -- a later
    /// schema is not guessed at.
    pub fn parse(text: &str) -> Option<Self> {
        let stored: StoredAudio = serde_json::from_str(text.trim()).ok()?;
        (stored.schema == AUDIO_SETTING_SCHEMA).then_some(stored.setting)
    }

    pub fn to_stored(&self) -> String {
        let stored = StoredAudio {
            schema: AUDIO_SETTING_SCHEMA,
            setting: self.clone(),
        };
        format!("{}\n", serde_json::to_string(&stored).unwrap_or_default())
    }

    /// Whether this setting can be kept at all, on any hardware: a volume in
    /// range, a named device that is named, a format list without repeats.
    /// What the hardware offers is checked against the devices, in
    /// [`check`].
    pub fn valid(&self) -> Result<(), String> {
        if self.volume > 100 {
            return Err("Ses seviyesi 0 ile 100 arasında olmalı.".into());
        }
        if let AudioDeviceChoice::Device { id } = &self.device
            && id.trim().is_empty()
        {
            return Err("Cihaz adı boş.".into());
        }
        if let Some(formats) = &self.formats {
            let mut sorted = formats.clone();
            sorted.sort();
            sorted.dedup();
            if sorted.len() != formats.len() {
                return Err("Aynı biçim iki kez seçilmiş.".into());
            }
        }
        Ok(())
    }
}

/// Whether `setting` can be chosen on the devices found now, and why not.
///
/// A device must exist to be chosen (one that was chosen and is unplugged
/// stays chosen: that is [`plan`]'s business); passthrough formats must be
/// ones the device's sink declares; transcoding needs a sink that declares
/// AC-3.
pub fn check(setting: &AudioSetting, devices: &[AudioDevice], previous: &AudioSetting) -> Result<(), String> {
    setting.valid()?;
    if let AudioDeviceChoice::Device { id } = &setting.device
        && setting.device != previous.device
        && !devices.iter().any(|device| &device.id == id)
    {
        return Err("Bu cihaz şu an takılı değil.".into());
    }
    let device = resolve(&setting.device, devices);
    let declared = device.map(declared).unwrap_or_default();
    // Checked when they are what changed. A kept list that the device now in
    // use does not declare -- a USB DAC chosen after passthrough was set up
    // for the television -- is not a reason to refuse the device: the plan
    // sends what this device takes and keeps the list for the next one.
    if let Some(formats) = &setting.formats
        && setting.mode == AudioMode::Passthrough
        && (setting.formats != previous.formats || previous.mode != AudioMode::Passthrough)
    {
        for codec in formats {
            if !declared.contains(codec) {
                return Err(format!(
                    "{} bu çıkışın alıcısında bildirilmiyor; seçilemez.",
                    codec.label()
                ));
            }
        }
    }
    if setting.ac3_transcode && !previous.ac3_transcode && !declared.contains(&AudioCodec::Ac3) {
        return Err("Alıcı Dolby Digital bildirmiyor; dönüştürme açılamaz.".into());
    }
    Ok(())
}

/// The device a choice resolves to among those found now: the chosen one
/// when it is there, the display's own otherwise.
pub fn resolve<'a>(choice: &AudioDeviceChoice, devices: &'a [AudioDevice]) -> Option<&'a AudioDevice> {
    let display = || devices.iter().find(|device| device.display);
    match choice {
        AudioDeviceChoice::Auto => display(),
        AudioDeviceChoice::Device { id } => devices.iter().find(|device| &device.id == id).or_else(display),
    }
}

/// The bitstream formats a device can be sent: what its sink declares, when
/// it can carry a burst at all. HBR formats (DTS-HD, TrueHD) need eight
/// channels at 192 kHz, which the sink's LPCM descriptor has to declare too.
pub fn declared(device: &AudioDevice) -> Vec<AudioCodec> {
    if !device.bitstream {
        return Vec::new();
    }
    let Some(sink) = &device.sink else {
        return Vec::new();
    };
    let hbr = sink
        .entries
        .iter()
        .any(|entry| entry.coding == 1 && entry.channels >= 8 && entry.rates.contains(&192_000));
    sink.codecs()
        .into_iter()
        .filter(|codec| !codec.needs_hbr() || hbr)
        .collect()
}

/// Dolby Digital encoding, as the player is asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ac3Encode {
    /// kbit/s. `None`: the encoder's own choice by channel count -- 448 for
    /// 5.1, the rate every Dolby Digital decoder has taken since DVD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kbps: Option<u32>,
    /// Fewer channels than this are left as PCM: stereo is never encoded.
    pub min_channels: u8,
}

/// The encoder's own rate for 5.1 (libavcodec `ac3enc`, five full-bandwidth
/// channels): the one a sink must declare at least for `auto` to be safe.
const AC3_AUTO_KBPS: u32 = 448;

/// AC-3's bit rates (ATSC A/52 table 5.18), which the encoder accepts.
const AC3_RATES: [u32; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

/// What a player is started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioPlan {
    /// The device sound goes to; `None`: nowhere (no device, or one whose
    /// connection to the picture is not firmly known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<AudioDevice>,
    /// The chosen device is not here and the display's own is used.
    #[serde(default)]
    pub fallback: bool,
    /// Formats sent as they are.
    pub passthrough: Vec<AudioCodec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac3_encode: Option<Ac3Encode>,
    pub volume: u8,
    pub muted: bool,
    /// Why a part of the setting does not apply here, in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl AudioPlan {
    /// Whether anything can reach the receiver as a bitstream, which is when
    /// software volume cannot be the whole story.
    pub fn bitstream(&self) -> bool {
        !self.passthrough.is_empty() || self.ac3_encode.is_some()
    }

    /// mpv's `audio-device` for this plan.
    pub fn mpv_device(&self) -> String {
        match &self.device {
            Some(device) => format!("alsa/{}", device.pcm),
            None => "null".into(),
        }
    }

    /// mpv's `audio-spdif`: empty for none.
    pub fn mpv_spdif(&self) -> String {
        self.passthrough.iter().map(|codec| codec.mpv()).collect::<Vec<_>>().join(",")
    }

    /// mpv's `af`: the Dolby Digital encoder, or empty.
    pub fn mpv_af(&self) -> String {
        match self.ac3_encode {
            Some(encode) => format!(
                "lavcac3enc=tospdif=yes:bitrate={}:minch={}",
                encode.kbps.map(|kbps| kbps.to_string()).unwrap_or_else(|| "auto".into()),
                encode.min_channels
            ),
            None => String::new(),
        }
    }

    /// mpv's arguments for this plan, one per element.
    ///
    /// `--audio-spdif` lists the passthrough formats; mpv falls back to PCM
    /// for everything else by itself. Dolby Digital encoding is mpv's own
    /// `lavcac3enc` with `tospdif`, below which anything with fewer than
    /// three channels is passed on untouched. `--volume` is software gain,
    /// which mpv applies to PCM only -- never to an IEC 61937 burst.
    pub fn mpv_args(&self) -> Vec<String> {
        if self.device.is_none() {
            return vec!["--ao=null".into()];
        }
        let mut args = vec!["--ao=alsa".to_string(), format!("--audio-device={}", self.mpv_device())];
        if !self.passthrough.is_empty() {
            args.push(format!("--audio-spdif={}", self.mpv_spdif()));
        }
        if self.ac3_encode.is_some() {
            args.push(format!("--af={}", self.mpv_af()));
        }
        args.push(format!("--volume={}", self.volume));
        args.push(format!("--mute={}", if self.muted { "yes" } else { "no" }));
        args
    }
}

/// What `setting` means on the devices found now.
pub fn plan(setting: &AudioSetting, devices: &[AudioDevice]) -> AudioPlan {
    let mut notes = Vec::new();
    let device = resolve(&setting.device, devices).cloned();
    let fallback = matches!(&setting.device, AudioDeviceChoice::Device { id }
        if !devices.iter().any(|device| &device.id == id));
    if fallback {
        notes.push("Seçilen cihaz takılı değil; ekranın sesi kullanılıyor.".into());
    }
    if device.is_none() {
        notes.push("Sesin gideceği bir cihaz yok; ses gönderilmiyor.".into());
    }
    let declared = device.as_ref().map(declared).unwrap_or_default();
    let passthrough = match setting.mode {
        AudioMode::Pcm => Vec::new(),
        AudioMode::Auto => declared.clone(),
        AudioMode::Passthrough => {
            let wanted = setting.formats.clone().unwrap_or_else(|| declared.clone());
            let kept: Vec<AudioCodec> = declared.iter().copied().filter(|codec| wanted.contains(codec)).collect();
            if kept.len() < wanted.len() {
                notes.push("Seçilen biçimlerin bir kısmını bu alıcı bildirmiyor; onlar PCM gider.".into());
            }
            kept
        }
    };
    if setting.mode != AudioMode::Pcm && declared.is_empty() && device.is_some() {
        notes.push("Bu çıkış sıkıştırılmış ses bildirmiyor; her şey PCM gider.".into());
    }
    let ac3_encode = if setting.ac3_transcode && setting.mode != AudioMode::Pcm {
        match device.as_ref().and_then(|device| device.sink.as_ref()) {
            Some(sink) if declared.contains(&AudioCodec::Ac3) => {
                let max = sink.ac3_max_kbps();
                let kbps = match max {
                    Some(max) if max < AC3_AUTO_KBPS => {
                        AC3_RATES.iter().copied().filter(|rate| *rate <= max).max()
                    }
                    _ => None,
                };
                Some(Ac3Encode { kbps, min_channels: 3 })
            }
            _ => {
                notes.push("Alıcı Dolby Digital bildirmiyor; dönüştürme uygulanmıyor.".into());
                None
            }
        }
    } else {
        if setting.ac3_transcode && setting.mode == AudioMode::Pcm {
            notes.push("PCM seçili; Dolby Digital dönüştürme uygulanmıyor.".into());
        }
        None
    };
    AudioPlan {
        device,
        fallback,
        passthrough,
        ac3_encode,
        volume: setting.volume.min(100),
        muted: setting.muted,
        notes,
    }
}

/// What the player is doing with sound right now, as it says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamAudio {
    /// The track's codec, as the player names it (`dts`, `aac`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// The track's channel layout (`5.1`, `stereo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// What leaves the box: the output format as the player names it
    /// (`spdif-ac3`, `s16`, `floatp`) and its layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_channels: Option<String>,
    /// In words: `Doğrudan geçiş: DTS`, `Dolby Digital'e dönüştürülüyor`,
    /// `PCM 2.0`.
    pub summary: String,
    /// A bitstream is leaving: software volume does not apply to it.
    #[serde(default)]
    pub bitstream: bool,
}

impl StreamAudio {
    /// From mpv's own properties: `audio-codec-name`, `audio-params`,
    /// `audio-out-params`, and whether the encoder filter is in the chain.
    pub fn from_mpv(
        codec: Option<String>,
        params: Option<&serde_json::Value>,
        out: Option<&serde_json::Value>,
        encoding: bool,
    ) -> Self {
        let field = |value: Option<&serde_json::Value>, name: &str| {
            value
                .and_then(|value| value.get(name))
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        let channels = field(params, "hr-channels").or_else(|| field(params, "channels"));
        let sample_rate = params
            .and_then(|value| value.get("samplerate"))
            .and_then(|value| value.as_u64())
            .map(|rate| rate as u32);
        let output_format = field(out, "format");
        let output_channels = field(out, "hr-channels").or_else(|| field(out, "channels"));
        let spdif = output_format.as_deref().and_then(|format| format.strip_prefix("spdif-"));
        let bitstream = spdif.is_some();
        let summary = match spdif {
            Some("ac3") if encoding && codec.as_deref() != Some("ac3") => format!(
                "Dolby Digital'e dönüştürülüyor ({} {})",
                codec.as_deref().unwrap_or("?").to_uppercase(),
                channels.as_deref().unwrap_or("?")
            ),
            Some(name) => format!("Doğrudan geçiş: {}", name.to_uppercase()),
            None => match (&output_format, &output_channels) {
                (Some(_), Some(layout)) => format!("PCM {layout}"),
                (Some(_), None) => "PCM".into(),
                _ => "Ses yok".into(),
            },
        };
        StreamAudio {
            codec,
            channels,
            sample_rate,
            output_format,
            output_channels,
            summary,
            bitstream,
        }
    }
}

/// The sound, as the daemon knows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioStatus {
    /// What is kept.
    pub setting: AudioSetting,
    /// Every device found now.
    pub devices: Vec<AudioDevice>,
    /// What the setting means on them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<AudioPlan>,
    /// What the player is doing with sound, while it plays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<StreamAudio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sad(coding: u8, name: &str, channels: u8, rates: &[u32], max_kbps: Option<u32>) -> SadEntry {
        SadEntry {
            coding,
            name: name.into(),
            channels,
            rates: rates.to_vec(),
            max_kbps,
        }
    }

    /// The Sony KD-65XE9005's audio data block, as edid-decode reads it.
    fn sony() -> SinkAudio {
        SinkAudio {
            source: CapsSource::Edid,
            entries: vec![
                sad(1, "LPCM", 6, &[32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000], None),
                sad(2, "AC-3", 6, &[32_000, 44_100, 48_000], Some(640)),
                sad(7, "DTS", 6, &[32_000, 44_100, 48_000], Some(1504)),
                sad(10, "E-AC-3", 8, &[48_000], None),
            ],
            speakers: vec!["FL/FR".into(), "LFE".into(), "FC".into(), "RL/RR".into()],
        }
    }

    fn hdmi(sink: Option<SinkAudio>) -> AudioDevice {
        AudioDevice {
            id: "display:fdea0000.hdmi".into(),
            label: "HDMI-A-2 · SONY TV".into(),
            kind: AudioKind::Hdmi,
            card_id: "rockchiphdmi1".into(),
            card_index: Some(2),
            pcm: "hdmi:CARD=rockchiphdmi1,DEV=0".into(),
            bitstream: true,
            connector: Some("HDMI-A-2".into()),
            display: true,
            sink,
        }
    }

    fn usb() -> AudioDevice {
        AudioDevice {
            id: "usb:0d8c:0014:Device".into(),
            label: "USB · Audio Device".into(),
            kind: AudioKind::Usb,
            card_id: "Device".into(),
            card_index: Some(4),
            pcm: "plughw:CARD=Device,DEV=0".into(),
            bitstream: false,
            connector: None,
            display: false,
            sink: None,
        }
    }

    #[test]
    fn auto_passes_through_what_the_sink_declares_and_nothing_else() {
        let devices = [hdmi(Some(sony())), usb()];
        let plan = plan(&AudioSetting::default(), &devices);
        assert_eq!(plan.device.as_ref().unwrap().id, "display:fdea0000.hdmi");
        assert_eq!(plan.passthrough, vec![AudioCodec::Ac3, AudioCodec::Eac3, AudioCodec::Dts]);
        assert_eq!(plan.ac3_encode, None, "transcoding is the viewer's to turn on");
        assert_eq!(
            plan.mpv_args(),
            vec![
                "--ao=alsa",
                "--audio-device=alsa/hdmi:CARD=rockchiphdmi1,DEV=0",
                "--audio-spdif=ac3,eac3,dts",
                "--volume=100",
                "--mute=no"
            ]
        );
    }

    #[test]
    fn pcm_sends_no_bitstream_and_no_encoder() {
        let devices = [hdmi(Some(sony()))];
        let setting = AudioSetting { mode: AudioMode::Pcm, ac3_transcode: true, volume: 40, ..Default::default() };
        let plan = plan(&setting, &devices);
        assert!(plan.passthrough.is_empty());
        assert!(plan.ac3_encode.is_none());
        assert!(!plan.bitstream());
        assert!(plan.mpv_args().contains(&"--volume=40".to_string()));
        assert!(!plan.mpv_args().iter().any(|arg| arg.contains("spdif") || arg.contains("lavcac3enc")));
    }

    #[test]
    fn transcoding_is_auto_bitrate_and_never_stereo() {
        let devices = [hdmi(Some(sony()))];
        let setting = AudioSetting { ac3_transcode: true, ..Default::default() };
        let plan = plan(&setting, &devices);
        assert_eq!(plan.ac3_encode, Some(Ac3Encode { kbps: None, min_channels: 3 }));
        assert!(plan.mpv_args().contains(&"--af=lavcac3enc=tospdif=yes:bitrate=auto:minch=3".to_string()));
        // A sink that declares less than the encoder's own 448 is given the
        // highest rate it declares.
        let mut low = sony();
        low.entries[1].max_kbps = Some(384);
        let plan = super::plan(&setting, &[hdmi(Some(low))]);
        assert_eq!(plan.ac3_encode.unwrap().kbps, Some(384));
    }

    #[test]
    fn nothing_is_offered_that_the_sink_does_not_declare() {
        // No AC-3 in the sink: no encoding, whatever was asked.
        let mut pcm_only = sony();
        pcm_only.entries.truncate(1);
        let setting = AudioSetting { ac3_transcode: true, mode: AudioMode::Passthrough, ..Default::default() };
        let plan = plan(&setting, &[hdmi(Some(pcm_only.clone()))]);
        assert!(plan.passthrough.is_empty() && plan.ac3_encode.is_none());
        assert!(!plan.notes.is_empty());
        // Passthrough of a format the sink does not declare is refused when
        // it is chosen, with the reason.
        let devices = [hdmi(Some(sony()))];
        let wants_truehd = AudioSetting {
            mode: AudioMode::Passthrough,
            formats: Some(vec![AudioCodec::TrueHd]),
            ..Default::default()
        };
        assert!(check(&wants_truehd, &devices, &AudioSetting::default()).unwrap_err().contains("TrueHD"));
        // Changing the device away from the receiver keeps the list and is
        // not refused for it: a USB headset takes PCM, and the television's
        // list is there again when the television is chosen again.
        let usb_device = AudioDevice {
            id: "usb:1038:230a:GameBuds".into(),
            bitstream: false,
            sink: None,
            display: false,
            ..hdmi(None)
        };
        let manual = AudioSetting {
            mode: AudioMode::Passthrough,
            formats: Some(vec![AudioCodec::Ac3, AudioCodec::Dts]),
            ..Default::default()
        };
        let to_usb = AudioSetting { device: AudioDeviceChoice::Device { id: usb_device.id.clone() }, ..manual.clone() };
        let both = [hdmi(Some(sony())), usb_device];
        check(&to_usb, &both, &manual).unwrap();
        assert!(super::plan(&to_usb, &both).passthrough.is_empty());
        // Manual passthrough keeps only what was picked.
        let only_ac3 = AudioSetting { formats: Some(vec![AudioCodec::Ac3]), ..wants_truehd };
        check(&only_ac3, &devices, &AudioSetting::default()).unwrap();
        assert_eq!(super::plan(&only_ac3, &devices).passthrough, vec![AudioCodec::Ac3]);
        // DTS-HD and TrueHD need the HBR layout, which a 6-channel LPCM sink
        // cannot take even if it lists them.
        let mut hd = sony();
        hd.entries.push(sad(11, "DTS-HD", 8, &[48_000], None));
        assert!(!declared(&hdmi(Some(hd))).contains(&AudioCodec::DtsHd));
        // A card with no IEC 958 path carries no burst whatever its sink says.
        let mut plain = hdmi(Some(sony()));
        plain.bitstream = false;
        assert!(declared(&plain).is_empty());
    }

    #[test]
    fn a_chosen_device_that_is_unplugged_stays_chosen_and_the_display_plays() {
        let chosen = AudioSetting {
            device: AudioDeviceChoice::Device { id: usb().id },
            ..Default::default()
        };
        let plugged = plan(&chosen, &[hdmi(Some(sony())), usb()]);
        assert_eq!(plugged.device.unwrap().id, usb().id);
        assert!(plugged.passthrough.is_empty(), "a USB DAC takes PCM");
        let unplugged = plan(&chosen, &[hdmi(Some(sony()))]);
        assert!(unplugged.fallback);
        assert_eq!(unplugged.device.unwrap().id, "display:fdea0000.hdmi");
        // It cannot be newly chosen while it is not there…
        let other = AudioSetting { device: AudioDeviceChoice::Device { id: "usb:x".into() }, ..Default::default() };
        assert!(check(&other, &[hdmi(Some(sony()))], &AudioSetting::default()).is_err());
        // …but a setting that already names it can be changed in other ways.
        let louder = AudioSetting { volume: 50, ..chosen.clone() };
        check(&louder, &[hdmi(Some(sony()))], &chosen).unwrap();
        // No device at all: nowhere, said out loud.
        let nowhere = plan(&AudioSetting::default(), &[]);
        assert_eq!(nowhere.mpv_args(), vec!["--ao=null"]);
    }

    #[test]
    fn the_record_round_trips_and_a_later_schema_is_not_guessed() {
        let setting = AudioSetting {
            device: AudioDeviceChoice::Device { id: "usb:0d8c:0014:Device".into() },
            mode: AudioMode::Passthrough,
            formats: Some(vec![AudioCodec::Ac3]),
            ac3_transcode: true,
            volume: 35,
            muted: true,
        };
        let text = setting.to_stored();
        assert!(text.contains("\"schema\":1"));
        assert_eq!(AudioSetting::parse(&text), Some(setting));
        assert_eq!(AudioSetting::parse(r#"{"schema":2,"volume":3}"#), None);
        assert_eq!(AudioSetting::parse("{}"), None);
        assert!(AudioSetting { volume: 101, ..Default::default() }.valid().is_err());
    }

    #[test]
    fn the_stream_is_described_as_the_player_reports_it() {
        let params = serde_json::json!({"format": "floatp", "samplerate": 48000, "channels": "5.1", "hr-channels": "5.1"});
        let encoded = StreamAudio::from_mpv(
            Some("dts".into()),
            Some(&params),
            Some(&serde_json::json!({"format": "spdif-ac3", "channels": "stereo"})),
            true,
        );
        assert!(encoded.bitstream);
        assert!(encoded.summary.starts_with("Dolby Digital'e dönüştürülüyor"), "{}", encoded.summary);
        let passed = StreamAudio::from_mpv(
            Some("ac3".into()),
            Some(&params),
            Some(&serde_json::json!({"format": "spdif-ac3"})),
            true,
        );
        assert_eq!(passed.summary, "Doğrudan geçiş: AC3");
        let pcm = StreamAudio::from_mpv(
            Some("aac".into()),
            Some(&params),
            Some(&serde_json::json!({"format": "s16", "hr-channels": "stereo"})),
            false,
        );
        assert!(!pcm.bitstream);
        assert_eq!(pcm.summary, "PCM stereo");
    }
}
