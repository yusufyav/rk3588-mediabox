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
//! Right or Ok goes in, or changes the control it is on; Left comes back out
//! one column; Back comes out one column too, and from the path it leaves.

use mediabox_core::{AudioCodec, AudioDeviceChoice, AudioMode, AudioSetting, AudioStatus, Cadence, OutputStatus};

use crate::prefs::{
    CaptionBackground, CaptionColor, CaptionEdge, CaptionPosition, CaptionSize, MediaPreferences, PlayerChoice,
    ResumeChoice,
};
use crate::screens::account::Session;
use crate::screens::audio;
use crate::screens::now_playing::preference_name;

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
    /// One of a few; Ok takes the next, round again.
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
    /// Every option, when there are few enough to show at once; empty when
    /// only the value is shown.
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
    let mut rows = vec![Row {
        value: match (&setting.device, device) {
            (AudioDeviceChoice::Auto, Some(device)) => format!("Otomatik · {}", device.label),
            (AudioDeviceChoice::Auto, None) => "Otomatik".into(),
            (_, Some(device)) => device.label.clone(),
            (_, None) => "Yok".into(),
        },
        ..Row::new(Control::AudioDevice, RowKind::Choice, "Ses çıkışı", "Tamam ile sıradaki cihaz")
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
}

/// The place this screen is, in the catalogue's column of places.
pub const HERE: usize = 3;
const PLACES: usize = 4;

/// What a press asks of the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Moved,
    Unchanged,
    /// Change this control.
    Change(Control),
    /// Go to this place of the catalogue.
    Place(usize),
}

impl MediaSettings {
    pub fn new() -> Self {
        Self { zone: Zone::Categories, category: 0, place: HERE, rows: [0; CATEGORIES.len()] }
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
        if self.zone == Zone::Controls && !rows.get(self.rows[at]).is_some_and(|row| row.enabled) {
            match nearest_enabled(rows, self.rows[at]) {
                Some(row) => self.rows[at] = row,
                None => self.zone = Zone::Categories,
            }
        }
    }

    /// `rows` are the controls of the category the remote is on.
    pub fn step(&mut self, dx: i32, dy: i32, rows: &[Row]) -> Step {
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
                if dx > 0 {
                    return self.change(rows);
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
        match self.zone {
            Zone::Places => Step::Place(self.place),
            Zone::Categories => self.enter_controls(rows),
            Zone::Controls => self.change(rows),
        }
    }

    /// Back: out of the controls to the path. False on the path or the
    /// places, which is the screen's way out.
    pub fn back(&mut self) -> bool {
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

    fn change(&self, rows: &[Row]) -> Step {
        match rows.get(self.row()) {
            Some(row) if row.enabled => Step::Change(row.control),
            _ => Step::Unchanged,
        }
    }
}

/// The enabled row at `from`, or the nearest one after it, or before it.
fn nearest_enabled(rows: &[Row], from: usize) -> Option<usize> {
    (from..rows.len()).chain((0..from.min(rows.len())).rev()).find(|at| rows[*at].enabled)
}

// ---------------------------------------------------------------- the changes

/// The sound setting choosing `control` sends, the way the film's panel
/// sends it, or `None` for a control that is not about sound.
pub fn audio_change(status: &AudioStatus, control: Control) -> Option<AudioSetting> {
    let setting = &status.setting;
    Some(match control {
        Control::AudioDevice => AudioSetting { device: next_device(status), ..setting.clone() },
        Control::AudioMode => audio::player_choose(status, audio::PlayerAct::CycleMode),
        Control::AudioFormat(codec) => audio::player_choose(status, audio::PlayerAct::Format(codec)),
        Control::Transcode => audio::player_choose(status, audio::PlayerAct::Transcode),
        _ => return None,
    })
}

/// Automatic, then every device found, round again.
fn next_device(status: &AudioStatus) -> AudioDeviceChoice {
    let devices = &status.devices;
    let at = match &status.setting.device {
        AudioDeviceChoice::Auto => 0,
        AudioDeviceChoice::Device { id } => devices.iter().position(|device| &device.id == id).map_or(0, |at| at + 1),
    };
    let next = (at + 1) % (devices.len() + 1);
    match next {
        0 => AudioDeviceChoice::Auto,
        n => AudioDeviceChoice::Device { id: devices[n - 1].id.clone() },
    }
}

/// `prefs` with `control` stepped on, or `None` for a control kept elsewhere.
pub fn prefs_change(prefs: &MediaPreferences, control: Control) -> Option<MediaPreferences> {
    use crate::prefs::next;
    let mut prefs = *prefs;
    let caption = &mut prefs.caption;
    match control {
        Control::DefaultPlayer => prefs.player = next(&PlayerChoice::ALL, prefs.player),
        Control::Resume => prefs.resume = next(&ResumeChoice::ALL, prefs.resume),
        Control::CaptionSize => caption.size = next(&CaptionSize::ALL, caption.size),
        Control::CaptionColor => caption.color = next(&CaptionColor::ALL, caption.color),
        Control::CaptionEdge => caption.edge = next(&CaptionEdge::ALL, caption.edge),
        Control::CaptionBackground => caption.background = next(&CaptionBackground::ALL, caption.background),
        Control::CaptionPosition => caption.position = next(&CaptionPosition::ALL, caption.position),
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
    /// right or Ok in, left out, Back out one column and then away.
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
        assert_eq!(screen.select(&rows), Step::Change(Control::Resume));
        assert_eq!(screen.step(1, 0, &rows), Step::Change(Control::Resume), "right changes, as Ok does");
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
        assert!(screen.back());
        assert_eq!(screen.zone, Zone::Categories);
        assert!(!screen.back());
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
        let next = prefs_change(&prefs, Control::DefaultPlayer).unwrap();
        assert_eq!(next.player, PlayerChoice::Kodi);
        assert!(prefs_change(&prefs, Control::RefreshMatching).is_none(), "kept by the display, not here");
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
        // Turning DTS off is what the film's panel sends for it.
        let change = audio_change(&status, Control::AudioFormat(AudioCodec::Dts)).unwrap();
        assert_eq!(change, audio::player_choose(&status, audio::PlayerAct::Format(AudioCodec::Dts)));
        assert_eq!(change.formats, Some(vec![AudioCodec::Ac3, AudioCodec::Eac3]));
        // The device: automatic, the Sony, the analog output, and round.
        let sony = audio_change(&status, Control::AudioDevice).unwrap();
        assert_eq!(sony.device, AudioDeviceChoice::Device { id: "display:fdea0000.hdmi".into() });
        let analog_status = crate::screens::audio::tests::status_for_tests(sony);
        let analog = audio_change(&analog_status, Control::AudioDevice).unwrap();
        assert_eq!(analog.device, AudioDeviceChoice::Device { id: "alsa:rockchipes8388".into() });
        // PCM only: nothing to choose about the form, and it says why.
        let pcm_status = crate::screens::audio::tests::status_for_tests(analog);
        context.audio = Some(&pcm_status);
        let rows = super::rows(Category::Audio, &context);
        assert_eq!(controls(&rows), [Control::AudioDevice, Control::AudioMode, Control::Transcode]);
        assert!(!rows[1].enabled && !rows[2].enabled);
        assert_eq!(audio_change(&pcm_status, Control::AudioDevice).unwrap().device, AudioDeviceChoice::Auto);
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
