//! The sound settings: where it goes, in what form, and how loud -- driven by
//! four arrows, Ok and Back.
//!
//! Every value on this page is the daemon's. The devices, what the sink at
//! the end of each declares, the kept setting and what it means for a player
//! all arrive in one [`AudioStatus`]; this page discovers nothing itself. A
//! choice is sent the moment it is made and the page shows the answer: a
//! refusal -- a format the receiver does not declare, a device that has gone
//! -- comes back in the daemon's own words. Sound needs no trial the way a
//! display mode does: nothing about it can leave the television unreadable.
//!
//! The same rules drive the film's own panel ([`player_rows`]), so a format
//! turned off during a film is the one the settings show off afterwards.

use mediabox_core::{AudioCodec, AudioDeviceChoice, AudioMode, AudioSetting, AudioStatus};

/// The cards, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    Device,
    Mode,
    Formats,
    Transcode,
    Volume,
    Mute,
}

pub const CARDS: [Card; 6] = [
    Card::Device,
    Card::Mode,
    Card::Formats,
    Card::Transcode,
    Card::Volume,
    Card::Mute,
];

/// What a press asks the application to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    Nothing,
    Changed,
    Set(AudioSetting),
    Step(i16),
    ToggleMute,
}

/// What a move did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nav {
    Moved,
    Unchanged,
    /// Left off the cards: back to the card that opened the page.
    Leave,
    /// Left or Right on the volume: a step, to be sent.
    Send(Press),
}

/// The volume's step, one press of Left or Right.
pub const VOLUME_STEP: i16 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Device,
    Mode,
    Formats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Picker {
    kind: Pick,
    focus: usize,
}

#[derive(Default)]
pub struct Audio {
    status: Option<AudioStatus>,
    focus: usize,
    picker: Option<Picker>,
    note: String,
}

impl Audio {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(&mut self, status: Option<AudioStatus>) {
        self.status = status;
        // A card that cannot be chosen on the device now in use does not
        // keep the remote: back up to the one above it.
        while self.focus > 0 && !self.enabled(self.focused()) {
            self.focus -= 1;
        }
        if let Some(kind) = self.picker.map(|picker| picker.kind) {
            let rows = self.rows_of(kind);
            if let Some(picker) = self.picker.as_mut() {
                picker.focus = picker.focus.min(rows.saturating_sub(1));
            }
        }
    }

    /// The daemon said no, in its own words.
    pub fn refused(&mut self, why: String) {
        self.note = why;
    }

    pub fn answered(&mut self) {
        self.note.clear();
    }

    pub fn available(&self) -> bool {
        self.status.is_some()
    }

    pub fn enter(&mut self) {
        self.focus = 0;
        self.picker = None;
        self.note.clear();
    }

    pub fn focused(&self) -> Card {
        CARDS[self.focus.min(CARDS.len() - 1)]
    }

    /// Whether a card can be chosen on the device in use. A device that takes
    /// PCM only -- a USB headset, the analog output -- has no form to choose,
    /// no formats to pass through and nothing to encode for: those cards say
    /// so and the remote steps over them.
    pub fn enabled(&self, card: Card) -> bool {
        let declared = self.declared();
        match card {
            Card::Mode | Card::Formats => !declared.is_empty(),
            Card::Transcode => declared.contains(&AudioCodec::Ac3),
            Card::Device | Card::Volume | Card::Mute => true,
        }
    }

    fn status(&self) -> AudioStatus {
        self.status.clone().unwrap_or_default()
    }

    fn setting(&self) -> AudioSetting {
        self.status().setting
    }

    /// What the receiver on the device in use declares, as bitstreams.
    fn declared(&self) -> Vec<AudioCodec> {
        declared(&self.status())
    }

    fn rows_of(&self, kind: Pick) -> usize {
        match kind {
            Pick::Device => self.status().devices.len() + 1,
            Pick::Mode => AudioMode::ALL.len(),
            Pick::Formats => self.declared().len(),
        }
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> Nav {
        if let Some(picker) = self.picker.as_mut() {
            if dy == 0 {
                return Nav::Unchanged;
            }
            let rows = match picker.kind {
                Pick::Device => self.status.as_ref().map_or(0, |status| status.devices.len()) + 1,
                Pick::Mode => AudioMode::ALL.len(),
                Pick::Formats => self.status.as_ref().map_or(0, |status| declared(status).len()),
            };
            let next = (picker.focus as i32 + dy).clamp(0, rows.saturating_sub(1) as i32) as usize;
            if next == picker.focus {
                return Nav::Unchanged;
            }
            picker.focus = next;
            return Nav::Moved;
        }
        if dx != 0 {
            if self.focused() == Card::Volume {
                return Nav::Send(Press::Step(dx.signum() as i16 * VOLUME_STEP));
            }
            return if dx < 0 { Nav::Leave } else { Nav::Unchanged };
        }
        let mut next = self.focus as i32;
        loop {
            next += dy.signum();
            if next < 0 || next >= CARDS.len() as i32 {
                return Nav::Unchanged;
            }
            if self.enabled(CARDS[next as usize]) {
                break;
            }
        }
        self.focus = next as usize;
        Nav::Moved
    }

    pub fn press(&mut self) -> Press {
        let setting = self.setting();
        if let Some(picker) = self.picker {
            let status = self.status();
            match picker.kind {
                Pick::Device => {
                    self.picker = None;
                    let device = match picker.focus {
                        0 => AudioDeviceChoice::Auto,
                        n => match status.devices.get(n - 1) {
                            Some(device) => AudioDeviceChoice::Device { id: device.id.clone() },
                            None => return Press::Changed,
                        },
                    };
                    return changed(&setting, AudioSetting { device, ..setting.clone() });
                }
                Pick::Mode => {
                    self.picker = None;
                    let mode = AudioMode::ALL[picker.focus.min(AudioMode::ALL.len() - 1)];
                    return changed(&setting, with_mode(&setting, mode));
                }
                // The list stays open: several formats are turned on and off
                // in one visit, and Back closes it.
                Pick::Formats => {
                    let Some(codec) = self.declared().get(picker.focus).copied() else {
                        return Press::Nothing;
                    };
                    return changed(&setting, toggle_format(&status, codec));
                }
            }
        }
        match self.focused() {
            Card::Device => self.open(Pick::Device),
            Card::Mode => self.open(Pick::Mode),
            Card::Formats => {
                if self.declared().is_empty() {
                    self.note = "Bu çıkışın alıcısı sıkıştırılmış ses bildirmiyor.".into();
                    return Press::Changed;
                }
                self.open(Pick::Formats)
            }
            Card::Transcode => {
                if !self.declared().contains(&AudioCodec::Ac3) {
                    self.note = "Alıcı Dolby Digital bildirmiyor; dönüştürme açılamaz.".into();
                    return Press::Changed;
                }
                changed(&setting, AudioSetting { ac3_transcode: !setting.ac3_transcode, ..setting.clone() })
            }
            Card::Volume => Press::Nothing,
            Card::Mute => Press::ToggleMute,
        }
    }

    fn open(&mut self, kind: Pick) -> Press {
        let setting = self.setting();
        let status = self.status();
        let focus = match kind {
            Pick::Device => match &setting.device {
                AudioDeviceChoice::Auto => 0,
                AudioDeviceChoice::Device { id } => status
                    .devices
                    .iter()
                    .position(|device| &device.id == id)
                    .map_or(0, |at| at + 1),
            },
            Pick::Mode => AudioMode::ALL.iter().position(|mode| *mode == setting.mode).unwrap_or(0),
            Pick::Formats => 0,
        };
        self.picker = Some(Picker { kind, focus });
        Press::Changed
    }

    /// Back: a list closes; the page itself is the settings screen's.
    pub fn back(&mut self) -> bool {
        self.picker.take().is_some()
    }

    pub fn view(&self) -> View {
        let Some(status) = &self.status else {
            return View {
                message: "Ses durumu henüz okunmadı.".into(),
                ..Default::default()
            };
        };
        let setting = &status.setting;
        let plan = status.plan.clone().unwrap_or_else(|| mediabox_core::audio::plan(setting, &status.devices));
        let declared = declared(status);
        let device = plan.device.as_ref();
        let device_value = match (&setting.device, device) {
            (AudioDeviceChoice::Auto, Some(device)) => format!("Otomatik · {}", device.label),
            (_, Some(device)) => device.label.clone(),
            (_, None) => "Yok".into(),
        };
        let passing: Vec<&str> = plan.passthrough.iter().map(|codec| codec.label()).collect();
        let formats_value = match setting.mode {
            _ if declared.is_empty() => "Yok".to_string(),
            AudioMode::Pcm => "Hepsi PCM".to_string(),
            _ if passing.is_empty() => "Yok".into(),
            _ => passing.join(", "),
        };
        let transcode_value = if declared.is_empty() {
            "Bu cihazda yok".to_string()
        } else if !declared.contains(&AudioCodec::Ac3) {
            "Alıcı desteklemiyor".to_string()
        } else if setting.ac3_transcode {
            "Açık".into()
        } else {
            "Kapalı".into()
        };
        let pcm_only = declared.is_empty();
        let card = |card: Card, icon: &str, label: &str, hint: &str, value: String, kind: &str| CardView {
            icon: icon.into(),
            label: label.into(),
            hint: hint.into(),
            value,
            kind: kind.into(),
            focused: self.picker.is_none() && self.focused() == card,
            enabled: self.enabled(card),
        };
        let cards = vec![
            card(Card::Device, "speaker", "Ses çıkışı", "Sesin gittiği cihaz", device_value, "link"),
            if pcm_only {
                card(Card::Mode, "sliders", "Biçim", "Bu cihaz yalnız PCM alır", "PCM".into(), "action")
            } else {
                card(Card::Mode, "sliders", "Biçim", setting.mode.hint(), setting.mode.label().into(), "link")
            },
            card(
                Card::Formats,
                "check",
                "Doğrudan geçen biçimler",
                "Alıcıya olduğu gibi gidenler",
                formats_value,
                "link",
            ),
            card(
                Card::Transcode,
                "refresh",
                "Dolby Digital dönüştürme",
                "Çok kanallı sesi AC-3 olarak gönderir",
                transcode_value,
                "action",
            ),
            card(
                Card::Volume,
                "speaker",
                "Ses seviyesi",
                "Sol ve sağ ile ayarlanır",
                format!("%{}", setting.volume),
                "action",
            ),
            card(
                Card::Mute,
                "speaker",
                "Sessiz",
                "",
                if setting.muted { "Açık".into() } else { "Kapalı".into() },
                "action",
            ),
        ];

        let mut badges = Vec::new();
        let mut rows = Vec::new();
        match device.and_then(|device| device.sink.as_ref()) {
            Some(sink) => {
                badges.push((format!("PCM {} kanal", sink.pcm_channels()), String::new()));
                for codec in &declared {
                    let tone = if plan.passthrough.contains(codec) { "good" } else { "" };
                    badges.push((codec.label().to_string(), tone.to_string()));
                }
                rows.push((
                    "Alıcı bildirimi".to_string(),
                    match sink.source {
                        mediabox_core::CapsSource::Eld => "ELD".into(),
                        mediabox_core::CapsSource::Edid => "EDID (sürücü ELD yayınlamıyor)".into(),
                    },
                ));
                if !sink.speakers.is_empty() {
                    rows.push(("Hoparlörler".into(), sink.speakers.join(" ")));
                }
            }
            None => badges.push(("PCM".into(), String::new())),
        }
        if let Some(encode) = plan.ac3_encode {
            rows.push((
                "Dönüştürme".into(),
                format!(
                    "3+ kanal → Dolby Digital, {}",
                    encode.kbps.map(|kbps| format!("{kbps} kbit/s")).unwrap_or_else(|| "kanal sayısına göre (5.1: 448 kbit/s)".into())
                ),
            ));
        }
        match &status.stream {
            Some(stream) => {
                rows.push(("Şu an çalan".into(), stream.summary.clone()));
                if let Some(codec) = &stream.codec {
                    rows.push((
                        "Kaynak".into(),
                        format!("{} {}", codec.to_uppercase(), stream.channels.clone().unwrap_or_default()),
                    ));
                }
                if stream.bitstream {
                    rows.push((
                        "Ses seviyesi".into(),
                        "Bit akışına yazılım ses seviyesi uygulanmaz; alıcıdan ayarlayın.".into(),
                    ));
                }
            }
            None => rows.push(("Şu an çalan".into(), "Oynatma yok".into())),
        }
        if plan.bitstream() && status.stream.is_none() {
            rows.push((
                "Not".into(),
                "Doğrudan geçen ve dönüştürülen seste ses seviyesi alıcıdadır.".into(),
            ));
        }

        let (picker_open, picker_title, picker, picker_focus) = match self.picker {
            None => (false, String::new(), Vec::new(), 0),
            Some(picker) => {
                let options = match picker.kind {
                    Pick::Device => {
                        let mut options = vec![OptionView {
                            title: "Otomatik".into(),
                            sub: "Görüntünün gittiği ekranın sesi".into(),
                            selected: setting.device == AudioDeviceChoice::Auto,
                            focused: picker.focus == 0,
                        }];
                        options.extend(status.devices.iter().enumerate().map(|(at, device)| OptionView {
                            title: device.label.clone(),
                            sub: format!(
                                "{}{}",
                                device.kind.label(),
                                if device.display { " · görüntünün ekranı" } else { "" }
                            ),
                            selected: setting.device == AudioDeviceChoice::Device { id: device.id.clone() },
                            focused: picker.focus == at + 1,
                        }));
                        options
                    }
                    Pick::Mode => AudioMode::ALL
                        .iter()
                        .enumerate()
                        .map(|(at, mode)| OptionView {
                            title: mode.label().into(),
                            sub: mode.hint().into(),
                            selected: setting.mode == *mode,
                            focused: picker.focus == at,
                        })
                        .collect(),
                    Pick::Formats => declared
                        .iter()
                        .enumerate()
                        .map(|(at, codec)| OptionView {
                            title: codec.label().into(),
                            sub: if plan.passthrough.contains(codec) {
                                "Alıcıya olduğu gibi gider".into()
                            } else {
                                "Burada çözülür".into()
                            },
                            selected: plan.passthrough.contains(codec),
                            focused: picker.focus == at,
                        })
                        .collect(),
                };
                let title = match picker.kind {
                    Pick::Device => "Ses çıkışı",
                    Pick::Mode => "Biçim",
                    Pick::Formats => "Doğrudan geçen biçimler",
                };
                (true, title.to_string(), options, picker.focus)
            }
        };

        let mut note = self.note.clone();
        if note.is_empty() {
            note = plan.notes.join(" ");
        }
        View {
            available: true,
            message: String::new(),
            cards,
            note,
            current_title: device.map(|device| device.label.clone()).unwrap_or_else(|| "Ses gitmiyor".into()),
            current_line: device
                .map(|device| format!("{} · {}", device.kind.label(), device.pcm))
                .unwrap_or_default(),
            current_badges: badges,
            current_rows: rows,
            picker_open,
            picker_title,
            picker,
            picker_focus,
            volume: i32::from(setting.volume),
            muted: setting.muted,
            volume_focused: self.picker.is_none() && self.focused() == Card::Volume,
        }
    }
}

/// `after` to be sent, or nothing when it is what is kept already.
fn changed(before: &AudioSetting, after: AudioSetting) -> Press {
    if *before == after { Press::Changed } else { Press::Set(after) }
}

/// The bitstream formats the device in use can be sent.
pub fn declared(status: &AudioStatus) -> Vec<AudioCodec> {
    status
        .plan
        .as_ref()
        .and_then(|plan| plan.device.as_ref())
        .map(mediabox_core::audio::declared)
        .unwrap_or_default()
}

/// The setting with `mode`, keeping everything else.
pub fn with_mode(setting: &AudioSetting, mode: AudioMode) -> AudioSetting {
    AudioSetting { mode, ..setting.clone() }
}

/// The setting with `codec` turned on or off for passthrough.
///
/// From `Auto` or PCM this is the move to picking formats by hand, starting
/// from what was passing through a moment ago -- so turning DTS off under
/// `Auto` leaves everything else the sink declares on, which is what a
/// person with a Dolby Digital receiver behind a DTS television means.
pub fn toggle_format(status: &AudioStatus, codec: AudioCodec) -> AudioSetting {
    let setting = &status.setting;
    let mut formats: Vec<AudioCodec> = status
        .plan
        .as_ref()
        .map(|plan| plan.passthrough.clone())
        .unwrap_or_default();
    if let Some(at) = formats.iter().position(|format| *format == codec) {
        formats.remove(at);
    } else {
        formats.push(codec);
        formats.sort();
    }
    AudioSetting {
        mode: AudioMode::Passthrough,
        formats: Some(formats),
        ..setting.clone()
    }
}

trait Hint {
    fn hint(self) -> &'static str;
}

impl Hint for AudioMode {
    fn hint(self) -> &'static str {
        match self {
            AudioMode::Auto => "Alıcının bildirdiği her biçim olduğu gibi gider",
            AudioMode::Pcm => "Her şey burada çözülür",
            AudioMode::Passthrough => "Yalnız seçtiğiniz biçimler olduğu gibi gider",
        }
    }
}

/// One row of the film's own panel about sound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRow {
    pub label: String,
    pub detail: String,
    pub active: bool,
    pub act: PlayerAct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerAct {
    /// Whether the film's frame rate chooses the display's refresh.
    RefreshMatching,
    /// Auto, PCM, passthrough, and round again.
    CycleMode,
    Format(AudioCodec),
    Transcode,
    /// "Tercih edilen altyazı dili": the language a film starts with.
    SubtitleLanguage,
    /// "Otomatik eşitleme": whether an external subtitle is checked and
    /// timed.
    SubtitleAutoSync,
    /// "AutoSync uyumsuz altyazıları göster": whether a subtitle shown not to
    /// fit the film stays in the subtitle menu.
    SubtitleShowIncompatible,
}

/// The groups the film's settings panel is divided into, in the order it
/// lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerGroup {
    Audio,
    Subtitles,
    Video,
}

impl PlayerGroup {
    pub const ALL: [PlayerGroup; 3] = [PlayerGroup::Audio, PlayerGroup::Subtitles, PlayerGroup::Video];

    pub fn label(self) -> &'static str {
        match self {
            PlayerGroup::Audio => "Ses",
            PlayerGroup::Subtitles => "Altyazı",
            PlayerGroup::Video => "Video",
        }
    }
}

impl PlayerAct {
    /// Which group a setting is listed under.
    pub fn group(self) -> PlayerGroup {
        match self {
            PlayerAct::RefreshMatching => PlayerGroup::Video,
            PlayerAct::SubtitleLanguage | PlayerAct::SubtitleAutoSync | PlayerAct::SubtitleShowIncompatible => {
                PlayerGroup::Subtitles
            }
            PlayerAct::CycleMode | PlayerAct::Format(_) | PlayerAct::Transcode => PlayerGroup::Audio,
        }
    }
}

/// The panel's rows by group, in `PlayerGroup::ALL`'s order and their own
/// order inside it. A group with nothing in it -- the sound before the daemon
/// has described it -- is not listed.
pub fn grouped(rows: Vec<PlayerRow>) -> Vec<(PlayerGroup, Vec<PlayerRow>)> {
    PlayerGroup::ALL
        .iter()
        .map(|group| (*group, rows.iter().filter(|row| row.act.group() == *group).cloned().collect::<Vec<_>>()))
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// The sound rows of the film's panel: the form, each format the receiver
/// declares, and Dolby Digital transcoding when the receiver takes it.
pub fn player_rows(status: &AudioStatus) -> Vec<PlayerRow> {
    let declared = declared(status);
    let plan = status.plan.clone().unwrap_or_else(|| mediabox_core::audio::plan(&status.setting, &status.devices));
    let mut rows = vec![PlayerRow {
        label: format!("Ses: {}", status.setting.mode.label()),
        detail: status
            .stream
            .as_ref()
            .map(|stream| stream.summary.clone())
            .unwrap_or_default(),
        active: false,
        act: PlayerAct::CycleMode,
    }];
    rows.extend(declared.iter().map(|codec| PlayerRow {
        label: codec.label().to_string(),
        detail: if plan.passthrough.contains(codec) { "Doğrudan".into() } else { "PCM".into() },
        active: plan.passthrough.contains(codec),
        act: PlayerAct::Format(*codec),
    }));
    if declared.contains(&AudioCodec::Ac3) {
        rows.push(PlayerRow {
            label: "Dolby Digital dönüştürme".into(),
            detail: if status.setting.ac3_transcode { "Açık".into() } else { "Kapalı".into() },
            active: status.setting.ac3_transcode,
            act: PlayerAct::Transcode,
        });
    }
    rows
}

/// The film's own settings panel below the choice of player: the refresh,
/// then the sound. Either half is left out when the daemon has not said.
pub fn player_panel(
    output: Option<&mediabox_core::OutputStatus>,
    audio: Option<&AudioStatus>,
) -> Vec<PlayerRow> {
    let mut rows = Vec::new();
    if let Some(output) = output {
        let detail = match &output.content {
            Some(content) => match &content.matched {
                Some(matched) if output.content_matching => {
                    format!("{} fps → {}", content.cadence_label, matched.label)
                }
                Some(_) => format!("{} fps · kalıcı mod", content.cadence_label),
                None => format!("{} fps · bu çözünürlükte uyan mod yok", content.cadence_label),
            },
            None => String::new(),
        };
        rows.push(PlayerRow {
            label: if output.content_matching {
                "Yenileme hızı: içeriğe eşle".into()
            } else {
                "Yenileme hızı: sabit".into()
            },
            detail,
            active: output.content_matching,
            act: PlayerAct::RefreshMatching,
        });
    }
    if let Some(audio) = audio {
        rows.extend(player_rows(audio));
    }
    rows
}

/// What choosing a row of [`player_rows`] sets. Not for
/// [`PlayerAct::RefreshMatching`], which is the display's.
pub fn player_choose(status: &AudioStatus, act: PlayerAct) -> AudioSetting {
    let setting = &status.setting;
    match act {
        PlayerAct::CycleMode => {
            let at = AudioMode::ALL.iter().position(|mode| *mode == setting.mode).unwrap_or(0);
            with_mode(setting, AudioMode::ALL[(at + 1) % AudioMode::ALL.len()])
        }
        PlayerAct::Format(codec) => toggle_format(status, codec),
        PlayerAct::Transcode => AudioSetting { ac3_transcode: !setting.ac3_transcode, ..setting.clone() },
        PlayerAct::RefreshMatching
        | PlayerAct::SubtitleLanguage
        | PlayerAct::SubtitleAutoSync
        | PlayerAct::SubtitleShowIncompatible => setting.clone(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct CardView {
    pub icon: String,
    pub label: String,
    pub hint: String,
    pub value: String,
    pub kind: String,
    pub focused: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct OptionView {
    pub title: String,
    pub sub: String,
    pub selected: bool,
    pub focused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct View {
    pub available: bool,
    pub message: String,
    pub cards: Vec<CardView>,
    pub note: String,
    pub current_title: String,
    pub current_line: String,
    pub current_badges: Vec<(String, String)>,
    pub current_rows: Vec<(String, String)>,
    pub picker_open: bool,
    pub picker_title: String,
    pub picker: Vec<OptionView>,
    pub picker_focus: usize,
    pub volume: i32,
    pub muted: bool,
    pub volume_focused: bool,
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_film_settings_are_grouped_by_what_they_are_about() {
        let row = |label: &str, act: PlayerAct| PlayerRow { label: label.into(), detail: String::new(), active: false, act };
        let rows = vec![
            row("Tercih edilen altyazı dili", PlayerAct::SubtitleLanguage),
            row("Otomatik eşitleme", PlayerAct::SubtitleAutoSync),
            row("AutoSync uyumsuz altyazıları göster", PlayerAct::SubtitleShowIncompatible),
            row("Yenileme hızı", PlayerAct::RefreshMatching),
            row("Ses", PlayerAct::CycleMode),
            row("Dolby Digital dönüştürme", PlayerAct::Transcode),
        ];
        let groups = grouped(rows.clone());
        let names: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|(group, rows)| (group.label(), rows.iter().map(|r| r.label.as_str()).collect()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Ses", vec!["Ses", "Dolby Digital dönüştürme"]),
                ("Altyazı", vec!["Tercih edilen altyazı dili", "Otomatik eşitleme", "AutoSync uyumsuz altyazıları göster"]),
                ("Video", vec!["Yenileme hızı"]),
            ]
        );
        // The sound not described yet: no empty "Ses" group.
        let without_sound: Vec<PlayerRow> = rows.into_iter().filter(|r| r.act.group() != PlayerGroup::Audio).collect();
        assert_eq!(grouped(without_sound).first().map(|(g, _)| *g), Some(PlayerGroup::Subtitles));
    }

    use super::*;
    use mediabox_core::{AudioDevice, AudioKind, CapsSource, SadEntry, SinkAudio};

    fn sad(coding: u8, max_kbps: Option<u32>) -> SadEntry {
        SadEntry { coding, name: String::new(), channels: 6, rates: vec![48_000], max_kbps }
    }

    /// The Sony: AC-3, E-AC-3 and DTS declared.
    fn status(setting: AudioSetting) -> AudioStatus {
        let devices = vec![
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
                sink: Some(SinkAudio {
                    source: CapsSource::Edid,
                    entries: vec![sad(1, None), sad(2, Some(640)), sad(7, Some(1504)), sad(10, None)],
                    speakers: Vec::new(),
                }),
            },
            AudioDevice {
                id: "alsa:rockchipes8388".into(),
                label: "Analog · rockchip-es8388".into(),
                kind: AudioKind::Analog,
                card_id: "rockchipes8388".into(),
                card_index: Some(3),
                pcm: "plughw:CARD=rockchipes8388,DEV=0".into(),
                bitstream: false,
                connector: None,
                display: false,
                sink: None,
            },
        ];
        let plan = mediabox_core::audio::plan(&setting, &devices);
        AudioStatus { setting, devices, plan: Some(plan), stream: None, error: None }
    }

    fn down_to(audio: &mut Audio, card: Card) {
        while audio.focused() != card {
            assert_eq!(audio.step(0, 1), Nav::Moved);
        }
    }

    #[test]
    fn a_dolby_digital_receiver_behind_a_dts_television_turns_dts_off() {
        let mut audio = Audio::new();
        audio.load(Some(status(AudioSetting::default())));
        audio.enter();
        down_to(&mut audio, Card::Formats);
        assert_eq!(audio.press(), Press::Changed, "the list opens");
        let view = audio.view();
        assert!(view.picker_open);
        let titles: Vec<&str> = view.picker.iter().map(|option| option.title.as_str()).collect();
        assert_eq!(titles, ["Dolby Digital", "Dolby Digital Plus", "DTS"]);
        assert!(view.picker.iter().all(|option| option.selected), "Auto passes all three");
        // Down twice to DTS, Ok turns it off: the rest stay on, by hand now.
        audio.step(0, 1);
        audio.step(0, 1);
        let Press::Set(setting) = audio.press() else { panic!("a change to send") };
        assert_eq!(setting.mode, AudioMode::Passthrough);
        assert_eq!(setting.formats, Some(vec![AudioCodec::Ac3, AudioCodec::Eac3]));
        // The daemon keeps it; the list is still open on DTS, now off.
        audio.load(Some(status(setting.clone())));
        let view = audio.view();
        assert!(view.picker_open);
        assert!(!view.picker[2].selected);
        assert!(audio.back(), "Back closes the list");
        assert!(!audio.back(), "and then the page is the settings screen's");
    }

    #[test]
    fn transcoding_and_volume_are_one_press_each() {
        let mut audio = Audio::new();
        audio.load(Some(status(AudioSetting::default())));
        audio.enter();
        down_to(&mut audio, Card::Transcode);
        let Press::Set(setting) = audio.press() else { panic!() };
        assert!(setting.ac3_transcode);
        down_to(&mut audio, Card::Volume);
        assert_eq!(audio.step(1, 0), Nav::Send(Press::Step(VOLUME_STEP)));
        assert_eq!(audio.step(-1, 0), Nav::Send(Press::Step(-VOLUME_STEP)));
        down_to(&mut audio, Card::Mute);
        assert_eq!(audio.press(), Press::ToggleMute);
        // Left off any other card leaves the page, as on every settings page.
        assert_eq!(audio.step(-1, 0), Nav::Leave);
        assert_eq!(audio.step(0, 1), Nav::Unchanged, "the last card is the last");
    }

    #[test]
    fn a_device_is_chosen_from_what_is_there() {
        let mut audio = Audio::new();
        audio.load(Some(status(AudioSetting::default())));
        audio.enter();
        audio.press();
        assert_eq!(audio.view().picker.len(), 3, "Otomatik, the television, the analog output");
        audio.step(0, 1);
        audio.step(0, 1);
        let Press::Set(setting) = audio.press() else { panic!() };
        assert_eq!(setting.device, AudioDeviceChoice::Device { id: "alsa:rockchipes8388".into() });
        // Nothing to send when the choice is what is kept.
        audio.load(Some(status(setting)));
        audio.press();
        assert_eq!(audio.press(), Press::Changed);
    }

    #[test]
    fn a_pcm_only_device_has_no_form_to_choose_and_the_remote_steps_over_it() {
        let setting = AudioSetting {
            device: AudioDeviceChoice::Device { id: "alsa:rockchipes8388".into() },
            ..Default::default()
        };
        let mut audio = Audio::new();
        audio.load(Some(status(setting)));
        audio.enter();
        assert!(!audio.enabled(Card::Mode) && !audio.enabled(Card::Formats) && !audio.enabled(Card::Transcode));
        let view = audio.view();
        assert!(!view.cards[1].enabled && view.cards[1].value == "PCM");
        // Down from the device goes straight to the volume.
        assert_eq!(audio.step(0, 1), Nav::Moved);
        assert_eq!(audio.focused(), Card::Volume);
        assert_eq!(audio.step(0, -1), Nav::Moved);
        assert_eq!(audio.focused(), Card::Device);
    }

    #[test]
    fn the_film_panel_uses_the_same_rules() {
        let status = status(AudioSetting::default());
        let rows = player_rows(&status);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Ses: Otomatik", "Dolby Digital", "Dolby Digital Plus", "DTS", "Dolby Digital dönüştürme"]
        );
        let off = player_choose(&status, PlayerAct::Format(AudioCodec::Dts));
        assert_eq!(off.formats, Some(vec![AudioCodec::Ac3, AudioCodec::Eac3]));
        assert_eq!(player_choose(&status, PlayerAct::CycleMode).mode, AudioMode::Pcm);
        assert!(player_choose(&status, PlayerAct::Transcode).ac3_transcode);
    }
}
