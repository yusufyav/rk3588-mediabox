//! "Filmler ve Diziler > Ayarlar": the catalogue's own settings, and the
//! player's.
//!
//! Not the box's settings screen in another colour. That one is about the
//! appliance -- its network, its fan, its lights. This one is about watching:
//! what happens when a source is chosen, how the picture and the sound leave
//! the player, how the subtitle reads, and whose account the catalogue is.
//!
//! It owns almost nothing. Each value on it belongs to whoever already kept
//! it, and the film's quick panel reaches the same owner, so a change in one
//! place is what the other shows:
//!
//! - the refresh: the control plane's display state (`content_matching`);
//! - the sound: the daemon's `audio.json`, changed with the same
//!   [`audio::player_choose`] the film's panel uses;
//! - the subtitle language and its timing: the daemon's `subtitles.json`;
//! - the account: the media core's provider block.
//!
//! What is left -- the player a source opens in, where a film left part-way
//! starts, how the subtitle this interface draws looks -- is this interface's
//! own and is kept in [`crate::prefs`].
//!
//! The remote: the places down the left, then the path of categories, then a
//! category's controls. Up and Down move inside the column the remote is in;
//! Right or Ok goes into the path's category; Left and Back come back out one
//! layer, and from the path they leave. Moving never changes a setting: on a
//! control only Ok does -- it turns a switch over, or opens the control's
//! options with every one of them in view, where Up and Down move, Ok takes
//! the one under the remote and Back or Left closes the list unchanged.

use mediabox_core::{AudioCodec, AudioDeviceChoice, AudioMode, AudioSetting, AudioStatus, Cadence, OutputStatus};

use crate::prefs::{
    CaptionBackground, CaptionColor, CaptionEdge, CaptionPosition, CaptionSize, MediaPreferences, PlayerChoice,
    ResumeChoice,
};
use crate::screens::account::Session;
use crate::screens::audio;
use crate::screens::now_playing::{PREFERRED_LANGUAGES, preference_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Player,
    Video,
    Audio,
    Subtitles,
    Account,
}

/// The path, in the order a film travels it: chosen and opened, drawn,
/// heard, read -- and the account the catalogue comes from at the end.
pub const CATEGORIES: [Category; 5] =
    [Category::Player, Category::Video, Category::Audio, Category::Subtitles, Category::Account];

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Player => "Oynatıcı",
            Category::Video => "Görüntü",
            Category::Audio => "Ses",
            Category::Subtitles => "Altyazı",
            Category::Account => "Hesap",
        }
    }

    /// The mark, by its name in the interface's own line set.
    pub fn icon(self) -> &'static str {
        match self {
            Category::Player => "play",
            Category::Video => "monitor",
            Category::Audio => "speaker",
            Category::Subtitles => "captions",
            Category::Account => "user",
        }
    }

    /// Which surface the panel draws for it.
    pub fn surface(self) -> &'static str {
        match self {
            Category::Player => "player",
            Category::Video => "video",
            Category::Audio => "audio",
            Category::Subtitles => "subtitles",
            Category::Account => "account",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Category::Player => "Bir kaynak seçildiğinde film nerede ve nereden açılır.",
            Category::Video => "Film, ekrana kendi kare hızına uyan modda gider.",
            Category::Audio => "Her ses biçimi alıcıya olduğu gibi mi gider, burada mı çözülür.",
            Category::Subtitles => "Film hangi dilde başlar, altyazı nasıl zamanlanır ve nasıl görünür.",
            Category::Account => "Katalog, kitaplık ve izleme geçmişi bu hesaptan gelir.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// The catalogue's places, down the left edge.
    Places,
    /// The path of categories.
    Categories,
    /// The controls of the category the remote is on.
    Controls,
}

/// Every control the screen has, and what changing it touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    DefaultPlayer,
    Resume,
    RefreshMatching,
    AudioDevice,
    AudioMode,
    AudioFormat(AudioCodec),
    Transcode,
    SubtitleLanguage,
    AutoSync,
    ShowIncompatible,
    CaptionSize,
    CaptionColor,
    CaptionEdge,
    CaptionBackground,
    CaptionPosition,
    SignIn,
    SignOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// On or off; Ok turns it over.
    Toggle,
    /// One of several; Ok opens the list of them.
    Choice,
    /// Does something or goes somewhere.
    Action,
}

impl RowKind {
    pub fn name(self) -> &'static str {
        match self {
            RowKind::Toggle => "toggle",
            RowKind::Choice => "choice",
            RowKind::Action => "action",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub control: Control,
    /// The heading of the group this row opens, empty when it continues one.
    pub group: String,
    pub label: String,
    /// One short line: what it does, or why it cannot be changed here.
    pub hint: String,
    pub kind: RowKind,
    pub value: String,
    pub on: bool,
    /// Every option, in the order the list shows them.
    pub choices: Vec<String>,
    pub selected: usize,
    pub enabled: bool,
}

impl Row {
    fn new(control: Control, kind: RowKind, label: &str, hint: impl Into<String>) -> Self {
        Self {
            control,
            group: String::new(),
            label: label.into(),
            hint: hint.into(),
            kind,
            value: String::new(),
            on: false,
            choices: Vec::new(),
            selected: 0,
            enabled: true,
        }
    }

    fn toggle(control: Control, label: &str, hint: impl Into<String>, on: bool) -> Self {
        Self {
            on,
            value: if on { "Açık".into() } else { "Kapalı".into() },
            ..Self::new(control, RowKind::Toggle, label, hint)
        }
    }

    fn choice<T: Copy + PartialEq>(
        control: Control,
        label: &str,
        hint: impl Into<String>,
        all: &[T],
        current: T,
        name: impl Fn(T) -> &'static str,
    ) -> Self {
        let selected = all.iter().position(|value| *value == current).unwrap_or(0);
        Self {
            value: name(current).into(),
            choices: all.iter().map(|value| name(*value).to_string()).collect(),
            selected,
            ..Self::new(control, RowKind::Choice, label, hint)
        }
    }

    fn in_group(mut self, group: &str) -> Self {
        self.group = group.into();
        self
    }

    fn disabled(mut self, why: impl Into<String>) -> Self {
        self.enabled = false;
        self.hint = why.into();
        self
    }
}

/// What the rows are built from: the owners' last answers.
pub struct Context<'a> {
    pub prefs: &'a MediaPreferences,
    pub audio: Option<&'a AudioStatus>,
    pub output: Option<&'a OutputStatus>,
    /// The daemon's subtitle preferences, as the film's panel keeps them.
    /// `auto_sync` is `None` until the daemon has said anything at all.
    pub subtitle_language: Option<&'a str>,
    pub subtitle_auto_sync: Option<bool>,
    pub subtitle_show_incompatible: Option<bool>,
    pub session: &'a Session,
    /// Kodi is installed, as far as the launcher knows.
    pub kodi: bool,
}

pub fn rows(category: Category, ctx: &Context) -> Vec<Row> {
    match category {
        Category::Player => player_rows(ctx),
        Category::Video => video_rows(ctx),
        Category::Audio => audio_rows(ctx),
        Category::Subtitles => subtitle_rows(ctx),
        Category::Account => account_rows(ctx),
    }
}

fn player_rows(ctx: &Context) -> Vec<Row> {
    let mut player = Row::choice(
        Control::DefaultPlayer,
        "Varsayılan oynatıcı",
        "Sor: her filmde hangisinde açılacağı sorulur",
        &PlayerChoice::ALL,
        ctx.prefs.player,
        PlayerChoice::label,
    );
    if !ctx.kodi {
        player = player.disabled("Kodi kurulu değil; filmler MediaBox'ta açılır");
    }
    vec![
        player,
        Row::choice(
            Control::Resume,
            "Yarıda kalan film",
            "Hesabın kaldığı yeri bildiği filmlerde",
            &ResumeChoice::ALL,
            ctx.prefs.resume,
            ResumeChoice::label,
        ),
    ]
}

fn video_rows(ctx: &Context) -> Vec<Row> {
    let row = Row::toggle(
        Control::RefreshMatching,
        "Kare hızına göre yenileme",
        "Film bitince ekran kendi moduna döner",
        ctx.output.is_some_and(|output| output.content_matching),
    );
    match ctx.output {
        Some(_) => vec![row],
        None => vec![row.disabled("Ekran durumu henüz okunmadı")],
    }
}

fn audio_rows(ctx: &Context) -> Vec<Row> {
    let Some(status) = ctx.audio else {
        return vec![Row::new(Control::AudioDevice, RowKind::Choice, "Ses çıkışı", "").disabled("Ses durumu henüz okunmadı")];
    };
    let setting = &status.setting;
    let plan = status.plan.clone().unwrap_or_else(|| mediabox_core::audio::plan(setting, &status.devices));
    let declared = audio::declared(status);

    let device = plan.device.as_ref();
    let mut choices = vec!["Otomatik".to_string()];
    choices.extend(status.devices.iter().map(|device| device.label.clone()));
    let mut rows = vec![Row {
        value: match (&setting.device, device) {
            (AudioDeviceChoice::Auto, Some(device)) => format!("Otomatik · {}", device.label),
            (AudioDeviceChoice::Auto, None) => "Otomatik".into(),
            (_, Some(device)) => device.label.clone(),
            (_, None) => "Yok".into(),
        },
        choices,
        selected: match &setting.device {
            AudioDeviceChoice::Auto => 0,
            AudioDeviceChoice::Device { id } => {
                status.devices.iter().position(|device| &device.id == id).map_or(0, |at| at + 1)
            }
        },
        ..Row::new(Control::AudioDevice, RowKind::Choice, "Ses çıkışı", "Sesin gittiği cihaz")
    }];

    let mode = Row::choice(
        Control::AudioMode,
        "Biçim",
        match setting.mode {
            AudioMode::Auto => "Alıcının bildirdiği her biçim olduğu gibi gider",
            AudioMode::Pcm => "Her şey burada çözülür",
            AudioMode::Passthrough => "Yalnız açtığınız biçimler olduğu gibi gider",
        },
        &AudioMode::ALL,
        setting.mode,
        AudioMode::label,
    );
    rows.push(if declared.is_empty() { mode.disabled("Bu cihaz yalnız PCM alır") } else { mode });

    for (at, codec) in declared.iter().enumerate() {
        let passing = plan.passthrough.contains(codec);
        let row = Row::toggle(
            Control::AudioFormat(*codec),
            codec.label(),
            if passing { "Alıcıya olduğu gibi gider" } else { "Burada çözülür" },
            passing,
        );
        rows.push(if at == 0 { row.in_group("Doğrudan geçen biçimler") } else { row });
    }

    let transcode = Row::toggle(
        Control::Transcode,
        "Dolby Digital dönüştürme",
        "Çok kanallı ses AC-3 olarak gider",
        setting.ac3_transcode,
    )
    .in_group("Dönüştürme");
    rows.push(if declared.contains(&AudioCodec::Ac3) {
        transcode
    } else {
        transcode.disabled("Alıcı Dolby Digital bildirmiyor")
    });
    rows
}

fn subtitle_rows(ctx: &Context) -> Vec<Row> {
    let known = ctx.subtitle_auto_sync.is_some();
    let mut rows = vec![
        Row {
            value: preference_name(ctx.subtitle_language),
            choices: std::iter::once(None)
                .chain(PREFERRED_LANGUAGES.iter().map(|code| Some(*code)))
                .map(preference_name)
                .collect(),
            selected: ctx
                .subtitle_language
                .and_then(|code| PREFERRED_LANGUAGES.iter().position(|known| *known == code))
                .map_or(0, |at| at + 1),
            ..Row::new(
                Control::SubtitleLanguage,
                RowKind::Choice,
                "Tercih edilen altyazı dili",
                "Film bu dilde bir altyazıyla başlar",
            )
        }
        .in_group("Dil ve zamanlama"),
        Row::toggle(
            Control::AutoSync,
            "Otomatik eşitleme",
            "İndirilen altyazı filmin zamanına oturtulur",
            ctx.subtitle_auto_sync.unwrap_or(true),
        ),
        Row::toggle(
            Control::ShowIncompatible,
            "AutoSync uyumsuz altyazıları göster",
            "Filme uymadığı anlaşılanlar da menüde kalır",
            ctx.subtitle_show_incompatible.unwrap_or(false),
        ),
    ];
    if !known {
        for row in rows.iter_mut() {
            row.enabled = false;
            row.hint = "Altyazı tercihleri okunuyor…".into();
            row.value = "—".into();
        }
    }
    let caption = &ctx.prefs.caption;
    rows.extend([
        Row::choice(Control::CaptionSize, "Boyut", "", &CaptionSize::ALL, caption.size, CaptionSize::label)
            .in_group("Görünüm"),
        Row::choice(Control::CaptionColor, "Renk", "", &CaptionColor::ALL, caption.color, CaptionColor::label),
        Row::choice(Control::CaptionEdge, "Kenar", "", &CaptionEdge::ALL, caption.edge, CaptionEdge::label),
        Row::choice(
            Control::CaptionBackground,
            "Arka plan",
            "",
            &CaptionBackground::ALL,
            caption.background,
            CaptionBackground::label,
        ),
        Row::choice(
            Control::CaptionPosition,
            "Konum",
            "",
            &CaptionPosition::ALL,
            caption.position,
            CaptionPosition::label,
        ),
    ]);
    rows
}

fn account_rows(ctx: &Context) -> Vec<Row> {
    if ctx.session.authenticated {
        vec![Row::new(
            Control::SignOut,
            RowKind::Action,
            "Oturumu kapat",
            "Hesabın bu cihazla bağlantısı kesilir",
        )]
    } else {
        vec![Row::new(Control::SignIn, RowKind::Action, "Oturum aç", "Stremio e-posta adresi ve parolasıyla")]
    }
}

/// One line under each category on the path: where it stands now.
pub fn summary(category: Category, ctx: &Context) -> String {
    match category {
        Category::Player => {
            let player = if ctx.kodi { ctx.prefs.player } else { PlayerChoice::MediaBox };
            let player = match player {
                PlayerChoice::Ask => "Oynatıcı sorulur",
                other => other.label(),
            };
            let resume = match ctx.prefs.resume {
                ResumeChoice::Ask => "devam sorulur",
                ResumeChoice::Resume => "kaldığı yerden",
                ResumeChoice::FromStart => "hep baştan",
            };
            format!("{player} · {resume}")
        }
        Category::Video => match ctx.output {
            Some(output) if output.content_matching => "Kare hızına eşlenir".into(),
            Some(_) => "Yenileme sabit".into(),
            None => "Okunuyor…".into(),
        },
        Category::Audio => match ctx.audio {
            Some(status) => {
                let declared = audio::declared(status);
                if declared.is_empty() {
                    "PCM".into()
                } else {
                    status.setting.mode.label().into()
                }
            }
            None => "Okunuyor…".into(),
        },
        Category::Subtitles => {
            if ctx.subtitle_auto_sync.is_none() {
                return format!("{} · {}", ctx.prefs.caption.size.label(), ctx.prefs.caption.color.label());
            }
            let language = preference_name(ctx.subtitle_language);
            if ctx.subtitle_auto_sync == Some(true) {
                format!("{language} · AutoSync")
            } else {
                language
            }
        }
        Category::Account => {
            if ctx.session.authenticated {
                if ctx.session.email.is_empty() { "Oturum açık".into() } else { ctx.session.email.clone() }
            } else {
                "Oturum açılmadı".into()
            }
        }
    }
}

// ------------------------------------------------------------------ the focus

pub struct MediaSettings {
    pub zone: Zone,
    pub category: usize,
    /// On the places column: the one the remote is on.
    pub place: usize,
    /// Each category's own row, so leaving one and coming back lands where
    /// it was.
    rows: [usize; CATEGORIES.len()],
    /// A control's options, open over it.
    pub picker: Option<Picker>,
}

/// The options of one control, open: which control, and the option the
/// remote is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picker {
    pub control: Control,
    pub focus: usize,
}

/// How many options a list shows at once; a longer one scrolls.
pub const PICKER_VISIBLE: usize = 7;

/// The first option drawn of `total` with the remote on `focus`: the focus
/// stays in view with one more below it while there is one.
pub fn picker_first(total: usize, focus: usize) -> usize {
    if total <= PICKER_VISIBLE {
        return 0;
    }
    (focus + 2).saturating_sub(PICKER_VISIBLE).min(total - PICKER_VISIBLE)
}

/// The place this screen is, in the catalogue's column of places.
pub const HERE: usize = 3;
const PLACES: usize = 4;

/// What a press asks of the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Moved,
    Unchanged,
    /// Turn this switch over, or do this action.
    Change(Control),
    /// Set this control to its option at this index.
    Pick(Control, usize),
    /// Go to this place of the catalogue.
    Place(usize),
}

impl MediaSettings {
    pub fn new() -> Self {
        Self { zone: Zone::Categories, category: 0, place: HERE, rows: [0; CATEGORIES.len()], picker: None }
    }

    pub fn current(&self) -> Category {
        CATEGORIES[self.category.min(CATEGORIES.len() - 1)]
    }

    pub fn row(&self) -> usize {
        self.rows[self.category.min(CATEGORIES.len() - 1)]
    }

    /// Opened from the catalogue: on the path, on the category it was last
    /// on.
    pub fn enter(&mut self) {
        self.zone = Zone::Categories;
        self.place = HERE;
        self.picker = None;
    }

    /// Back on this screen from one it opened (the sign-in form): where it
    /// was, unless that was the places.
    pub fn arrive(&mut self) {
        if self.zone == Zone::Places {
            self.zone = Zone::Categories;
            self.place = HERE;
        }
    }

    /// The controls changed under the remote -- a device went, the account
    /// signed out: it stays on a row that is there and can be changed, or
    /// goes back to the path.
    pub fn settle(&mut self, rows: &[Row]) {
        let at = self.category.min(CATEGORIES.len() - 1);
        if rows.is_empty() {
            self.rows[at] = 0;
        } else {
            self.rows[at] = self.rows[at].min(rows.len() - 1);
        }
        // A list stays open only over the control it belongs to, while that
        // control can still be changed.
        if let Some(picker) = self.picker {
            match rows.get(self.rows[at]) {
                Some(row) if row.enabled && row.control == picker.control && !row.choices.is_empty() => {
                    let focus = picker.focus.min(row.choices.len() - 1);
                    self.picker = Some(Picker { focus, ..picker });
                }
                _ => self.picker = None,
            }
        }
        if self.zone == Zone::Controls && !rows.get(self.rows[at]).is_some_and(|row| row.enabled) {
            match nearest_enabled(rows, self.rows[at]) {
                Some(row) => self.rows[at] = row,
                None => self.zone = Zone::Categories,
            }
        }
    }

    /// `rows` are the controls of the category the remote is on.
    pub fn step(&mut self, dx: i32, dy: i32, rows: &[Row]) -> Step {
        if let Some(picker) = self.picker.as_mut() {
            // Left is out of the list, unchanged; Right means nothing in it.
            if dx < 0 {
                self.picker = None;
                return Step::Moved;
            }
            if dy == 0 {
                return Step::Unchanged;
            }
            let total = rows.get(self.rows[self.category]).map_or(0, |row| row.choices.len());
            let next = (picker.focus as i32 + dy.signum()).clamp(0, total.saturating_sub(1) as i32) as usize;
            if next == picker.focus {
                return Step::Unchanged;
            }
            picker.focus = next;
            return Step::Moved;
        }
        match self.zone {
            Zone::Places => {
                if dx > 0 {
                    self.zone = Zone::Categories;
                    self.place = HERE;
                    return Step::Moved;
                }
                if dy != 0 {
                    let next = (self.place as i32 + dy.signum()).clamp(0, PLACES as i32 - 1) as usize;
                    let moved = next != self.place;
                    self.place = next;
                    return if moved { Step::Moved } else { Step::Unchanged };
                }
                Step::Unchanged
            }
            Zone::Categories => {
                if dx > 0 {
                    return self.enter_controls(rows);
                }
                if dx < 0 {
                    self.zone = Zone::Places;
                    self.place = HERE;
                    return Step::Moved;
                }
                if dy != 0 {
                    let next = (self.category as i32 + dy.signum()).clamp(0, CATEGORIES.len() as i32 - 1) as usize;
                    let moved = next != self.category;
                    self.category = next;
                    return if moved { Step::Moved } else { Step::Unchanged };
                }
                Step::Unchanged
            }
            Zone::Controls => {
                if dx < 0 {
                    self.zone = Zone::Categories;
                    return Step::Moved;
                }
                // Right changes nothing: a setting is changed with Ok.
                if dx > 0 {
                    return Step::Unchanged;
                }
                if dy != 0 {
                    let at = self.category;
                    let mut next = self.rows[at] as i32;
                    loop {
                        next += dy.signum();
                        if next < 0 || next >= rows.len() as i32 {
                            return Step::Unchanged;
                        }
                        if rows[next as usize].enabled {
                            break;
                        }
                    }
                    self.rows[at] = next as usize;
                    return Step::Moved;
                }
                Step::Unchanged
            }
        }
    }

    /// Ok.
    pub fn select(&mut self, rows: &[Row]) -> Step {
        if let Some(picker) = self.picker.take() {
            return Step::Pick(picker.control, picker.focus);
        }
        match self.zone {
            Zone::Places => Step::Place(self.place),
            Zone::Categories => self.enter_controls(rows),
            Zone::Controls => self.change(rows),
        }
    }

    /// Back: an open list closes unchanged; the controls go back to the
    /// path. False on the path or the places, which is the screen's way out.
    pub fn back(&mut self) -> bool {
        if self.picker.take().is_some() {
            return true;
        }
        match self.zone {
            Zone::Controls => {
                self.zone = Zone::Categories;
                true
            }
            Zone::Categories | Zone::Places => false,
        }
    }

    fn enter_controls(&mut self, rows: &[Row]) -> Step {
        let at = self.category;
        let remembered = self.rows[at].min(rows.len().saturating_sub(1));
        let Some(row) = nearest_enabled(rows, remembered) else {
            return Step::Unchanged;
        };
        self.rows[at] = row;
        self.zone = Zone::Controls;
        Step::Moved
    }

    /// Ok on a control: a switch turns over, an action is done, and a
    /// choice opens its list on the option in force.
    fn change(&mut self, rows: &[Row]) -> Step {
        match rows.get(self.row()) {
            Some(row) if row.enabled && row.kind == RowKind::Choice && !row.choices.is_empty() => {
                self.picker = Some(Picker { control: row.control, focus: row.selected.min(row.choices.len() - 1) });
                Step::Moved
            }
            Some(row) if row.enabled && row.kind != RowKind::Choice => Step::Change(row.control),
            _ => Step::Unchanged,
        }
    }

    /// The subtitle's look while a list of it is open: the option under the
    /// remote, so the preview shows it before it is taken. What is kept
    /// otherwise.
    pub fn caption_preview(&self, prefs: &MediaPreferences) -> crate::prefs::CaptionStyle {
        self.picker
            .and_then(|picker| prefs_pick(prefs, picker.control, picker.focus))
            .map_or(prefs.caption, |candidate| candidate.caption)
    }
}

/// The enabled row at `from`, or the nearest one after it, or before it.
fn nearest_enabled(rows: &[Row], from: usize) -> Option<usize> {
    (from..rows.len()).chain((0..from.min(rows.len())).rev()).find(|at| rows[*at].enabled)
}

// ---------------------------------------------------------------- the changes

/// The sound setting a switch about sound sends when turned over, the way
/// the film's panel sends it, or `None` for a control that is not one.
pub fn audio_toggle(status: &AudioStatus, control: Control) -> Option<AudioSetting> {
    Some(match control {
        Control::AudioFormat(codec) => audio::player_choose(status, audio::PlayerAct::Format(codec)),
        Control::Transcode => audio::player_choose(status, audio::PlayerAct::Transcode),
        _ => return None,
    })
}

/// The sound setting choosing option `index` of a sound list sends, or
/// `None` for a control that is not one, or an option that is not there.
pub fn audio_pick(status: &AudioStatus, control: Control, index: usize) -> Option<AudioSetting> {
    let setting = &status.setting;
    Some(match control {
        Control::AudioDevice => AudioSetting {
            device: match index {
                0 => AudioDeviceChoice::Auto,
                n => AudioDeviceChoice::Device { id: status.devices.get(n - 1)?.id.clone() },
            },
            ..setting.clone()
        },
        Control::AudioMode => audio::with_mode(setting, *AudioMode::ALL.get(index)?),
        _ => return None,
    })
}

/// The subtitle language option `index` of its list stands for: `None` is
/// "Kapalı".
pub fn language_pick(index: usize) -> Option<String> {
    index.checked_sub(1).and_then(|at| PREFERRED_LANGUAGES.get(at)).map(|code| code.to_string())
}

/// `prefs` with `control` set to its option `index`, or `None` for a
/// control kept elsewhere or an option that is not there.
pub fn prefs_pick(prefs: &MediaPreferences, control: Control, index: usize) -> Option<MediaPreferences> {
    let mut prefs = *prefs;
    let caption = &mut prefs.caption;
    match control {
        Control::DefaultPlayer => prefs.player = *PlayerChoice::ALL.get(index)?,
        Control::Resume => prefs.resume = *ResumeChoice::ALL.get(index)?,
        Control::CaptionSize => caption.size = *CaptionSize::ALL.get(index)?,
        Control::CaptionColor => caption.color = *CaptionColor::ALL.get(index)?,
        Control::CaptionEdge => caption.edge = *CaptionEdge::ALL.get(index)?,
        Control::CaptionBackground => caption.background = *CaptionBackground::ALL.get(index)?,
        Control::CaptionPosition => caption.position = *CaptionPosition::ALL.get(index)?,
        _ => return None,
    }
    Some(prefs)
}

// ------------------------------------------------------------- the surfaces

/// One stop on a chain the surfaces draw: the film, the player, the screen,
/// the receiver.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub eyebrow: String,
    pub title: String,
    pub lines: Vec<String>,
    /// "good", "warn" or empty, as everywhere else.
    pub tone: String,
}

/// What happens when a source is chosen, as the player settings stand: the
/// steps in order.
pub fn player_flow(prefs: &MediaPreferences, kodi: bool) -> Vec<Node> {
    let resume = match prefs.resume {
        ResumeChoice::Ask => Node {
            eyebrow: "Yarıda kalmışsa".into(),
            title: "Nereden başlasın?".into(),
            lines: vec!["Kaldığın yerden ya da baştan, sorulur".into()],
            tone: String::new(),
        },
        ResumeChoice::Resume => Node {
            eyebrow: "Yarıda kalmışsa".into(),
            title: "Kaldığı yerden".into(),
            lines: vec!["Hesabın bildiği saniyeden, sormadan".into()],
            tone: String::new(),
        },
        ResumeChoice::FromStart => Node {
            eyebrow: "Yarıda kalmışsa".into(),
            title: "Baştan".into(),
            lines: vec!["Kaldığı yer hesapta kalır, film baştan başlar".into()],
            tone: String::new(),
        },
    };
    let player = match (prefs.player, kodi) {
        (PlayerChoice::Kodi, true) => Node {
            eyebrow: "Oynatıcı".into(),
            title: "Kodi".into(),
            lines: vec!["Televizyon Kodi'ye geçer; Kodi kapanınca film sayfasına dönülür".into()],
            tone: String::new(),
        },
        (PlayerChoice::Ask, true) => Node {
            eyebrow: "Oynatıcı".into(),
            title: "MediaBox ya da Kodi".into(),
            lines: vec!["Her filmde sorulur".into()],
            tone: String::new(),
        },
        (_, kodi) => Node {
            eyebrow: "Oynatıcı".into(),
            title: "MediaBox".into(),
            lines: vec![if kodi || prefs.player == PlayerChoice::MediaBox {
                "Bu ekranın görüntü, ses ve altyazı ayarlarıyla".into()
            } else {
                "Kodi kurulu değil; bu ekranın ayarlarıyla MediaBox".into()
            }],
            tone: if kodi || prefs.player == PlayerChoice::MediaBox { String::new() } else { "warn".into() },
        },
    };
    vec![
        Node {
            eyebrow: "Film sayfası".into(),
            title: "Kaynak seçilir".into(),
            lines: vec!["Kaynaklar sütununda Tamam".into()],
            tone: String::new(),
        },
        resume,
        player,
    ]
}

/// What the account brings to the catalogue, as the provider block says it.
pub fn account_flow(session: &Session) -> Vec<Node> {
    if !session.authenticated {
        return vec![Node {
            eyebrow: "Hesap".into(),
            title: "Oturum açılmadı".into(),
            lines: vec!["Kitaplık, izleme geçmişi ve hesabın eklentileri oturum açınca gelir".into()],
            tone: String::new(),
        }];
    }
    vec![
        Node {
            eyebrow: "Eklentiler".into(),
            title: format!("{} eklenti", session.addons),
            lines: vec!["Pano ve Keşfet'teki kataloglar bunlardan gelir".into()],
            tone: String::new(),
        },
        Node {
            eyebrow: "Kitaplık".into(),
            title: "Kitaplık ve İzlemeye devam edin".into(),
            lines: vec!["Kaldığın yer ve izlenenler bu hesapta tutulur".into()],
            tone: String::new(),
        },
    ]
}

/// The frame rates a film comes in, in the order a person thinks of them.
const FILM_RATES: [(u64, u64); 8] =
    [(24000, 1001), (24, 1), (25, 1), (30000, 1001), (30, 1), (50, 1), (60000, 1001), (60, 1)];

#[derive(Debug, Clone, PartialEq)]
pub struct RateRow {
    pub film: String,
    pub mode: String,
    pub fit: String,
    pub tone: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoView {
    pub film: Node,
    pub screen: Node,
    pub rates: Vec<RateRow>,
    pub note: String,
}

/// The picture's path: the film playing now, if one is, the screen as it is
/// driven, and what every common frame rate would be shown at -- worked out
/// with the control plane's own rule ([`mediabox_core::OutputOffer::content_mode`])
/// against the display's own list of modes, never guessed.
pub fn video_view(output: Option<&OutputStatus>) -> VideoView {
    let Some(output) = output else {
        return VideoView { note: "Ekran durumu henüz okunmadı.".into(), ..Default::default() };
    };
    let film = match &output.content {
        Some(content) => Node {
            eyebrow: "Şu an oynayan film".into(),
            title: format!("{} fps", content.cadence_label),
            lines: vec![match &content.matched {
                Some(matched) => format!("{} · {}", pretty_mode(&matched.label), matched.fit.label()),
                None => "Bu çözünürlükte uyan mod yok".into(),
            }],
            tone: "good".into(),
        },
        None => Node {
            eyebrow: "Film".into(),
            title: "Oynatma yok".into(),
            lines: vec!["Kare hızı film başlayınca oynatıcıdan okunur".into()],
            tone: String::new(),
        },
    };
    let mut lines = Vec::new();
    if let Some(offer) = &output.offer {
        lines.push(match &offer.sink_name {
            Some(name) if !name.is_empty() => format!("{name} · {}", offer.connector),
            _ => offer.connector.clone(),
        });
    }
    let screen = match (&output.applied, &output.selected) {
        (Some(applied), _) => {
            if applied.hdr {
                lines.push("HDR10 sinyali".into());
            }
            Node { eyebrow: "Ekran şu an".into(), title: pretty_mode(&applied.label), lines, tone: String::new() }
        }
        (None, Some(selected)) => {
            Node { eyebrow: "Ekran ayarı".into(), title: pretty_mode(&selected.label), lines, tone: String::new() }
        }
        (None, None) => Node {
            eyebrow: "Ekran".into(),
            title: "Bilinmiyor".into(),
            lines: vec!["Ekran bağlı değil ya da mod listesi okunmadı".into()],
            tone: "warn".into(),
        },
    };
    let Some(offer) = &output.offer else {
        return VideoView { film, screen, rates: Vec::new(), note: "Ekranın mod listesi henüz okunmadı.".into() };
    };
    let kept = offer.resolve(output.setting.resolution).map(|mode| pretty_mode(&mode.label)).unwrap_or_default();
    let rates = FILM_RATES
        .iter()
        .filter_map(|(num, den)| Cadence::new(*num, *den))
        .map(|cadence| {
            let film = format!("{} fps", cadence.label());
            if !output.content_matching {
                return RateRow { film, mode: kept.clone(), fit: "sabit".into(), tone: String::new() };
            }
            match offer.content_mode(&output.setting, cadence) {
                Some(matched) => RateRow {
                    film,
                    mode: pretty_mode(&matched.label),
                    fit: matched.fit.label(),
                    tone: if matches!(matched.fit, mediabox_core::CadenceFit::Pulldown32) {
                        "warn".into()
                    } else {
                        "good".into()
                    },
                },
                None => RateRow { film, mode: kept.clone(), fit: "uyan mod yok".into(), tone: String::new() },
            }
        })
        .collect();
    let note = if output.content_matching {
        "Ekranın bildirdiği modlardan, seçili çözünürlükte. Çözünürlük hiç değişmez.".into()
    } else {
        "Eşleme kapalı: her film ekranın seçili modunda oynar.".into()
    };
    VideoView { film, screen, rates, note }
}

/// `3840x2160p23.98` as a person reads it: `3840×2160 · 23.98 Hz`.
pub fn pretty_mode(label: &str) -> String {
    let Some((size, rest)) = label.split_once(['p', 'i']) else {
        return label.to_string();
    };
    let interlaced = label[size.len()..].starts_with('i');
    if !size.contains('x') || rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return label.to_string();
    }
    format!("{} · {rest} Hz{}", size.replace('x', "×"), if interlaced { " (geçmeli)" } else { "" })
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteRow {
    pub codec: String,
    pub route: String,
    pub tone: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioView {
    pub stream: Node,
    pub routes: Vec<RouteRow>,
    pub device: Node,
    pub note: String,
}

/// The sound's path: what the player is sending now, if anything, what
/// becomes of each compressed format, and the device at the end with what
/// its receiver declares. All of it the daemon's own account.
pub fn audio_view(status: Option<&AudioStatus>) -> AudioView {
    let Some(status) = status else {
        return AudioView { note: "Ses durumu henüz okunmadı.".into(), ..Default::default() };
    };
    let plan = status.plan.clone().unwrap_or_else(|| mediabox_core::audio::plan(&status.setting, &status.devices));
    let declared = audio::declared(status);
    let stream = match &status.stream {
        Some(stream) => Node {
            eyebrow: "Şu an çalan".into(),
            title: stream.summary.clone(),
            lines: stream
                .codec
                .as_ref()
                .map(|codec| format!("{} {}", codec.to_uppercase(), stream.channels.clone().unwrap_or_default()))
                .into_iter()
                .collect(),
            tone: if stream.bitstream { "good".into() } else { String::new() },
        },
        None => Node {
            eyebrow: "MediaBox Player".into(),
            title: "Oynatma yok".into(),
            lines: vec!["Her biçimin gideceği yol aşağıda".into()],
            tone: String::new(),
        },
    };
    let decoded = if plan.ac3_encode.is_some() { "Çözülür → 3+ kanal AC-3" } else { "Çözülür → PCM" };
    let mut routes: Vec<RouteRow> = AudioCodec::ALL
        .iter()
        .map(|codec| {
            if plan.passthrough.contains(codec) {
                RouteRow { codec: codec.label().into(), route: "Doğrudan → alıcı".into(), tone: "good".into() }
            } else if declared.contains(codec) {
                RouteRow { codec: codec.label().into(), route: format!("{decoded} (kapalı)"), tone: String::new() }
            } else {
                RouteRow { codec: codec.label().into(), route: decoded.into(), tone: String::new() }
            }
        })
        .collect();
    routes.push(RouteRow { codec: "Diğer biçimler".into(), route: decoded.into(), tone: String::new() });

    let device = match plan.device.as_ref() {
        Some(device) => {
            let mut lines = vec![device.kind.label().to_string()];
            match device.sink.as_ref() {
                Some(sink) => {
                    lines.push(format!("PCM {} kanal", sink.pcm_channels()));
                    if !declared.is_empty() {
                        lines.push(format!(
                            "Bildirdiği: {}",
                            declared.iter().map(|codec| codec.label()).collect::<Vec<_>>().join(", ")
                        ));
                    }
                }
                None => lines.push("Yalnız PCM".into()),
            }
            Node {
                eyebrow: if plan.fallback { "Seçilen yok · ekranın sesi".into() } else { "Ses çıkışı".into() },
                title: device.label.clone(),
                lines,
                tone: if plan.fallback { "warn".into() } else { String::new() },
            }
        }
        None => Node {
            eyebrow: "Ses çıkışı".into(),
            title: "Ses gitmiyor".into(),
            lines: vec!["Bağlı bir ses cihazı bulunamadı".into()],
            tone: "warn".into(),
        },
    };
    let mut note = plan.notes.join(" ");
    if note.is_empty() && plan.bitstream() {
        note = "Doğrudan geçen ve dönüştürülen seste ses seviyesi alıcıdadır.".into();
    }
    AudioView { stream, routes, device, note }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(signed: bool) -> Session {
        Session { authenticated: signed, email: if signed { "a@b.c".into() } else { String::new() }, addons: 3, avatar: None }
    }

    fn ctx<'a>(prefs: &'a MediaPreferences, session: &'a Session) -> Context<'a> {
        Context {
            prefs,
            audio: None,
            output: None,
            subtitle_language: None,
            subtitle_auto_sync: None,
            subtitle_show_incompatible: None,
            session,
            kodi: true,
        }
    }

    fn controls(rows: &[Row]) -> Vec<Control> {
        rows.iter().map(|row| row.control).collect()
    }

    /// The contract the remote is held to: up and down inside a column,
    /// right or Ok into the path's category, left and Back out one layer.
    #[test]
    fn the_remote_goes_in_and_out_one_column_at_a_time() {
        let prefs = MediaPreferences::default();
        let session = session(false);
        let rows = rows(Category::Player, &ctx(&prefs, &session));
        let mut screen = MediaSettings::new();
        screen.enter();
        assert_eq!(screen.zone, Zone::Categories);
        assert_eq!(screen.step(1, 0, &rows), Step::Moved);
        assert_eq!(screen.zone, Zone::Controls);
        assert_eq!(screen.row(), 0);
        assert_eq!(screen.step(0, 1, &rows), Step::Moved);
        assert_eq!(screen.step(0, 1, &rows), Step::Unchanged, "two rows");
        assert_eq!(screen.step(-1, 0, &rows), Step::Moved);
        assert_eq!(screen.zone, Zone::Categories);
        assert_eq!(screen.step(-1, 0, &rows), Step::Moved);
        assert_eq!(screen.zone, Zone::Places);
        assert_eq!(screen.place, HERE, "on the place this screen is");
        assert_eq!(screen.step(0, -1, &rows), Step::Moved);
        assert_eq!(screen.select(&rows), Step::Place(2));
        assert_eq!(screen.step(1, 0, &rows), Step::Moved);
        assert_eq!(screen.zone, Zone::Categories);
        // Back: the controls to the path, and from the path away.
        screen.select(&rows);
        assert_eq!(screen.zone, Zone::Controls);
        assert!(screen.back());
        assert_eq!(screen.zone, Zone::Categories);
        assert!(!screen.back());
    }

    /// Moving never changes a setting, on the keyboard or the remote: only
    /// Ok does. A choice opens its list on the option in force, every option
    /// in it; Up and Down move in it, Ok takes one, Back and Left close it
    /// with nothing changed.
    #[test]
    fn only_ok_changes_a_setting_and_a_choice_shows_every_option() {
        let prefs = MediaPreferences::default();
        let session = session(false);
        let mut context = ctx(&prefs, &session);
        context.subtitle_language = Some("tr");
        context.subtitle_auto_sync = Some(true);
        context.subtitle_show_incompatible = Some(false);
        let rows = rows(Category::Subtitles, &context);
        let mut screen = MediaSettings::new();
        screen.category = 3;
        screen.select(&rows);
        assert_eq!(rows[screen.row()].control, Control::SubtitleLanguage);
        // Left and right, on every row: nothing is changed.
        for _ in 0..rows.len() {
            assert!(!matches!(screen.step(1, 0, &rows), Step::Change(_) | Step::Pick(..)));
            screen.step(0, 1, &rows);
        }
        while screen.row() > 0 {
            screen.step(0, -1, &rows);
        }
        // Ok on the language: its list, on Türkçe, "Kapalı" first.
        assert_eq!(rows[0].choices.len(), 1 + PREFERRED_LANGUAGES.len());
        assert_eq!(rows[0].choices[0], "Kapalı");
        assert_eq!(screen.select(&rows), Step::Moved);
        assert_eq!(screen.picker, Some(Picker { control: Control::SubtitleLanguage, focus: 1 }));
        assert_eq!(screen.step(1, 0, &rows), Step::Unchanged, "right means nothing in the list");
        assert_eq!(screen.step(0, 1, &rows), Step::Moved);
        assert_eq!(screen.select(&rows), Step::Pick(Control::SubtitleLanguage, 2));
        assert_eq!(language_pick(2).as_deref(), Some("en"));
        assert_eq!(language_pick(0), None, "Kapalı");
        assert_eq!(screen.picker, None);
        // Back closes a list unchanged, and stays on the row.
        screen.select(&rows);
        screen.step(0, 1, &rows);
        assert!(screen.back());
        assert_eq!((screen.picker, screen.zone), (None, Zone::Controls));
        // Left closes it too.
        screen.select(&rows);
        assert_eq!(screen.step(-1, 0, &rows), Step::Moved);
        assert_eq!((screen.picker, screen.zone), (None, Zone::Controls));
        // A switch turns over on Ok, and has no list.
        screen.step(0, 1, &rows);
        assert_eq!(screen.select(&rows), Step::Change(Control::AutoSync));
        assert_eq!(screen.picker, None);
    }

    /// The preview shows the option under the remote while a list of the
    /// subtitle's look is open; nothing is kept until Ok.
    #[test]
    fn the_preview_follows_the_list_before_anything_is_kept() {
        let prefs = MediaPreferences::default();
        let session = session(false);
        let rows = rows(Category::Subtitles, &ctx(&prefs, &session));
        let mut screen = MediaSettings::new();
        screen.category = 3;
        screen.select(&rows);
        assert_eq!(rows[screen.row()].control, Control::CaptionSize);
        assert_eq!(screen.caption_preview(&prefs), prefs.caption);
        screen.select(&rows);
        screen.step(0, 1, &rows);
        assert_eq!(screen.caption_preview(&prefs).size, CaptionSize::Large);
        assert_eq!(prefs.caption.size, CaptionSize::Normal, "kept only on Ok");
        screen.back();
        assert_eq!(screen.caption_preview(&prefs), prefs.caption);
        let picked = prefs_pick(&prefs, Control::CaptionSize, 2).unwrap();
        assert_eq!(picked.caption.size, CaptionSize::Large);
        assert!(prefs_pick(&prefs, Control::CaptionSize, 9).is_none(), "no such option");
    }

    #[test]
    fn a_long_list_scrolls_with_the_remote() {
        assert_eq!(picker_first(3, 2), 0);
        assert_eq!(picker_first(18, 0), 0);
        assert_eq!(picker_first(18, 6), 1);
        assert_eq!(picker_first(18, 17), 11);
    }

    #[test]
    fn each_category_remembers_its_row() {
        let prefs = MediaPreferences::default();
        let session = session(false);
        let context = ctx(&prefs, &session);
        let mut screen = MediaSettings::new();
        let subtitles = rows(Category::Subtitles, &context);
        while screen.current() != Category::Subtitles {
            screen.step(0, 1, &[]);
        }
        // The daemon has not answered: its three rows cannot be reached, and
        // the remote lands on the first one that can.
        assert_eq!(screen.select(&subtitles), Step::Moved);
        assert_eq!(subtitles[screen.row()].control, Control::CaptionSize);
        screen.step(0, 1, &subtitles);
        screen.step(0, 1, &subtitles);
        assert_eq!(subtitles[screen.row()].control, Control::CaptionEdge);
        assert_eq!(screen.step(0, -1, &subtitles), Step::Moved);
        assert_eq!(screen.step(0, -1, &subtitles), Step::Moved);
        assert_eq!(screen.step(0, -1, &subtitles), Step::Unchanged, "the unread rows are stepped over");
        screen.step(0, 1, &subtitles);
        screen.back();
        screen.step(0, -1, &[]);
        screen.step(0, 1, &[]);
        screen.select(&subtitles);
        assert_eq!(subtitles[screen.row()].control, Control::CaptionColor);
    }

    #[test]
    fn the_path_never_steps_off_its_ends() {
        let mut screen = MediaSettings::new();
        for _ in 0..10 {
            screen.step(0, 1, &[]);
        }
        assert_eq!(screen.current(), Category::Account);
        for _ in 0..10 {
            screen.step(0, -1, &[]);
        }
        assert_eq!(screen.current(), Category::Player);
        // Nothing to go into is staying on the path.
        assert_eq!(screen.select(&[]), Step::Unchanged);
        assert_eq!(screen.zone, Zone::Categories);
    }

    #[test]
    fn a_row_that_goes_away_takes_the_remote_somewhere_that_is_there() {
        let prefs = MediaPreferences::default();
        let signed = session(true);
        let rows = rows(Category::Account, &ctx(&prefs, &signed));
        assert_eq!(controls(&rows), [Control::SignOut]);
        let mut screen = MediaSettings::new();
        screen.category = 4;
        screen.select(&rows);
        assert_eq!(screen.select(&rows), Step::Change(Control::SignOut));
        let out = session(false);
        let rows = super::rows(Category::Account, &ctx(&prefs, &out));
        screen.settle(&rows);
        assert_eq!(screen.select(&rows), Step::Change(Control::SignIn));
    }

    #[test]
    fn a_player_choice_steps_round_and_kodi_absent_is_said() {
        let prefs = MediaPreferences::default();
        let session = session(false);
        let mut context = ctx(&prefs, &session);
        let rows = rows(Category::Player, &context);
        assert_eq!(rows[0].choices, ["MediaBox", "Kodi", "Sor"]);
        assert_eq!(rows[0].selected, 0);
        let next = prefs_pick(&prefs, Control::DefaultPlayer, 1).unwrap();
        assert_eq!(next.player, PlayerChoice::Kodi);
        assert!(prefs_pick(&prefs, Control::RefreshMatching, 0).is_none(), "kept by the display, not here");
        context.kodi = false;
        let rows = super::rows(Category::Player, &context);
        assert!(!rows[0].enabled);
        assert!(rows[0].hint.contains("Kodi"));
        assert_eq!(summary(Category::Player, &context), "MediaBox · devam sorulur");
    }

    #[test]
    fn the_subtitle_rows_are_the_film_panels_and_say_what_the_daemon_keeps() {
        let prefs = MediaPreferences::default();
        let session = session(false);
        let mut context = ctx(&prefs, &session);
        context.subtitle_language = Some("tr");
        context.subtitle_auto_sync = Some(false);
        context.subtitle_show_incompatible = Some(true);
        let rows = rows(Category::Subtitles, &context);
        assert_eq!(
            rows.iter().take(3).map(|row| row.label.as_str()).collect::<Vec<_>>(),
            ["Tercih edilen altyazı dili", "Otomatik eşitleme", "AutoSync uyumsuz altyazıları göster"],
            "the same words as the film's own panel"
        );
        assert_eq!(rows[0].value, "Türkçe");
        assert!(!rows[1].on && rows[2].on);
        assert!(rows.iter().all(|row| row.enabled));
        assert_eq!(rows[3].group, "Görünüm");
        assert_eq!(rows[3].choices[rows[3].selected], "Normal", "the default is the size it always was");
        assert_eq!(summary(Category::Subtitles, &context), "Türkçe");
    }

    #[test]
    fn the_sound_rows_and_its_changes_are_the_film_panels() {
        let prefs = MediaPreferences::default();
        let session = session(false);
        let status = crate::screens::audio::tests::status_for_tests(AudioSetting::default());
        let mut context = ctx(&prefs, &session);
        context.audio = Some(&status);
        let rows = rows(Category::Audio, &context);
        assert_eq!(
            controls(&rows),
            [
                Control::AudioDevice,
                Control::AudioMode,
                Control::AudioFormat(AudioCodec::Ac3),
                Control::AudioFormat(AudioCodec::Eac3),
                Control::AudioFormat(AudioCodec::Dts),
                Control::Transcode,
            ]
        );
        assert!(rows.iter().all(|row| row.enabled));
        assert!(rows[0].value.starts_with("Otomatik · HDMI-A-2"));
        assert_eq!(rows[0].choices, ["Otomatik", "HDMI-A-2 · SONY TV", "Analog · rockchip-es8388"]);
        assert_eq!(rows[0].selected, 0);
        // Turning DTS off is what the film's panel sends for it.
        let change = audio_toggle(&status, Control::AudioFormat(AudioCodec::Dts)).unwrap();
        assert_eq!(change, audio::player_choose(&status, audio::PlayerAct::Format(AudioCodec::Dts)));
        assert_eq!(change.formats, Some(vec![AudioCodec::Ac3, AudioCodec::Eac3]));
        // The device, chosen from its list.
        let sony = audio_pick(&status, Control::AudioDevice, 1).unwrap();
        assert_eq!(sony.device, AudioDeviceChoice::Device { id: "display:fdea0000.hdmi".into() });
        let analog = audio_pick(&status, Control::AudioDevice, 2).unwrap();
        assert_eq!(analog.device, AudioDeviceChoice::Device { id: "alsa:rockchipes8388".into() });
        assert!(audio_pick(&status, Control::AudioDevice, 3).is_none(), "no such device");
        assert_eq!(audio_pick(&status, Control::AudioMode, 1).unwrap().mode, AudioMode::Pcm);
        // PCM only: nothing to choose about the form, and it says why.
        let pcm_status = crate::screens::audio::tests::status_for_tests(analog);
        context.audio = Some(&pcm_status);
        let rows = super::rows(Category::Audio, &context);
        assert_eq!(controls(&rows), [Control::AudioDevice, Control::AudioMode, Control::Transcode]);
        assert!(!rows[1].enabled && !rows[2].enabled);
        assert_eq!(rows[0].selected, 2, "the analog output, chosen by hand");
        assert_eq!(audio_pick(&pcm_status, Control::AudioDevice, 0).unwrap().device, AudioDeviceChoice::Auto);
    }

    #[test]
    fn the_sound_path_says_what_becomes_of_each_format() {
        let status = crate::screens::audio::tests::status_for_tests(AudioSetting::default());
        let view = audio_view(Some(&status));
        let route = |name: &str| view.routes.iter().find(|row| row.codec == name).unwrap().route.clone();
        assert_eq!(route("DTS"), "Doğrudan → alıcı");
        assert_eq!(route("Dolby TrueHD"), "Çözülür → PCM");
        assert_eq!(view.device.title, "HDMI-A-2 · SONY TV");
        let transcoding = crate::screens::audio::tests::status_for_tests(AudioSetting {
            ac3_transcode: true,
            ..AudioSetting::default()
        });
        let view = audio_view(Some(&transcoding));
        assert_eq!(view.routes.iter().find(|row| row.codec == "Dolby TrueHD").unwrap().route, "Çözülür → 3+ kanal AC-3");
        assert!(audio_view(None).routes.is_empty());
    }

    /// The table is the control plane's own rule run on the display's own
    /// modes: the Sony at 4K lists 24, 30 and 60 Hz.
    #[test]
    fn the_frame_rate_table_is_the_control_planes_rule_on_this_display() {
        let mut output = crate::screens::output::tests::status_for_tests();
        output.content_matching = true;
        let view = video_view(Some(&output));
        let rate = |film: &str| view.rates.iter().find(|row| row.film == film).cloned().unwrap();
        let film24 = rate("24 fps");
        assert_eq!((film24.mode.as_str(), film24.fit.as_str()), ("3840×2160 · 24 Hz", "birebir"));
        assert_eq!(rate("60 fps").mode, "3840×2160 · 60 Hz");
        assert_eq!(rate("30 fps").fit, "birebir");
        // 25 has no multiple at 4K here and no pulldown: left alone.
        assert_eq!(rate("25 fps").fit, "uyan mod yok");
        assert_eq!(view.screen.title, "3840×2160 · 60 Hz");
        assert_eq!(view.film.title, "Oynatma yok");
        // Matching off: every rate is the kept mode, as the player will do.
        output.content_matching = false;
        let view = video_view(Some(&output));
        assert!(view.rates.iter().all(|row| row.fit == "sabit"));
        assert!(video_view(None).rates.is_empty());
    }

    #[test]
    fn a_mode_label_reads_as_a_size_and_a_rate() {
        assert_eq!(pretty_mode("3840x2160p23.98"), "3840×2160 · 23.98 Hz");
        assert_eq!(pretty_mode("1920x1080i50"), "1920×1080 · 50 Hz (geçmeli)");
        assert_eq!(pretty_mode("Auto"), "Auto");
    }

    #[test]
    fn the_flow_says_what_a_chosen_source_will_do() {
        let prefs = MediaPreferences { player: PlayerChoice::Kodi, resume: ResumeChoice::Resume, ..Default::default() };
        let flow = player_flow(&prefs, true);
        assert_eq!(flow.iter().map(|node| node.title.as_str()).collect::<Vec<_>>(), ["Kaynak seçilir", "Kaldığı yerden", "Kodi"]);
        let flow = player_flow(&prefs, false);
        assert_eq!(flow[2].title, "MediaBox");
        assert_eq!(flow[2].tone, "warn");
    }
}
