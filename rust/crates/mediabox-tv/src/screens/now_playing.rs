//! What is on the television right now.
//!
//! Playback is Kodi's, and this screen is a view of it plus the four controls a
//! remote needs. It is deliberately not a second player: the position, the
//! duration and the state are read from the control plane's `kodi_status`, and
//! every control is a request back to it, so what this screen says is what is
//! actually happening rather than what this process last asked for.

use serde_json::Value;

use mediabox_core::PlaybackState;

/// The controls, in the order they are drawn. Stop is last and at the far end
/// on purpose: it is the one that ends the evening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    SeekBack,
    PlayPause,
    SeekForward,
    Stop,
}

pub const CONTROLS: [Control; 4] = [
    Control::SeekBack,
    Control::PlayPause,
    Control::SeekForward,
    Control::Stop,
];

impl Control {
    pub fn label(self, playing: bool) -> &'static str {
        match self {
            Control::SeekBack => "10 sn geri",
            Control::PlayPause => {
                if playing {
                    "Duraklat"
                } else {
                    "Devam Et"
                }
            }
            Control::SeekForward => "30 sn ileri",
            Control::Stop => "Durdur",
        }
    }

    /// An SVG path in a 24x24 box, drawn rather than fetched.
    pub fn mark(self, playing: bool) -> &'static str {
        match self {
            Control::SeekBack => "M11 6 5 12l6 6V6zM19 6l-6 6 6 6V6z",
            Control::PlayPause => {
                if playing {
                    "M9 5h3v14H9zM14 5h3v14h-3z"
                } else {
                    "M8 5l11 7-11 7z"
                }
            }
            Control::SeekForward => "M13 6l6 6-6 6V6zM5 6l6 6-6 6V6z",
            Control::Stop => "M6 6h12v12H6z",
        }
    }
}

/// The row along the bottom of a film this interface is playing.
///
/// The reference this appliance is measured against is Stremio's own Android
/// player, and this is its row, in its order: the film's state, the way back
/// to the start, then what the film is made of — subtitles, sound, speed, how
/// it meets the panel — and last, which player is showing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Film {
    PlayPause,
    Restart,
    Subtitles,
    Audio,
    Speed,
    Scale,
    /// The film's settings: the refresh it is shown at, and the sound. A
    /// button of its own, beside the choice of player and not inside it.
    Settings,
    Player,
}

pub const FILM_CONTROLS: [Film; 8] = [
    Film::PlayPause,
    Film::Restart,
    Film::Subtitles,
    Film::Audio,
    Film::Speed,
    Film::Scale,
    Film::Settings,
    Film::Player,
];

/// Where the reference puts a hairline between two groups of buttons.
pub const FILM_RULES: [usize; 2] = [1, 5];

impl Film {
    /// The filled part of the mark, as SVG path data in a 24x24 box.
    pub fn fill(self, playing: bool) -> &'static str {
        match self {
            Film::PlayPause => {
                if playing {
                    "M9 5h3.2v14H9zM14.8 5H18v14h-3.2z"
                } else {
                    "M8 5l11.5 7L8 19z"
                }
            }
            Film::Restart => "M11.2 6v12L4 12zM20 6v12l-7.2-6z",
            Film::Subtitles => {
                "M6.2 14.4h7.2v1.7H6.2zM15.1 14.4h2.7v1.7h-2.7zM6.2 11h2.7v1.7H6.2zM10.6 11h7.2v1.7h-7.2z"
            }
            Film::Audio => {
                "M2.6 10.4h1.5v3.2H2.6zM5.6 8.4h1.5v7.2H5.6zM8.6 5.6h1.5v12.8H8.6zM11.6 7.6h1.5v8.8h-1.5zM14.6 4.4h1.5v15.2h-1.5zM17.6 8.4h1.5v7.2h-1.5zM20.6 10.4h1.5v3.2h-1.5z"
            }
            Film::Speed => "M11.1 13.6l4.3-4.9 1.2 1-3.6 5.3z",
            Film::Scale => "",
            Film::Settings => "",
            Film::Player => "M10.9 8.6l4.6 3.1-4.6 3.1z",
        }
    }

    /// The drawn-with-a-line part of the mark, in the same box.
    pub fn line(self) -> &'static str {
        match self {
            Film::Subtitles => "M3.6 6.2h16.8v11.2h-5.6l-3.4 3.4-1-3.4H3.6z",
            Film::Speed => "M3.4 17.6a8.6 8.6 0 1 1 17.2 0z",
            Film::Scale => {
                "M13.4 10.6l6.4-6.4M14.6 3.6h5.6v5.6M10.6 13.4l-6.4 6.4M9.4 20.4H3.8v-5.6"
            }
            Film::Settings => {
                "M12 8.6a3.4 3.4 0 1 0 0 6.8a3.4 3.4 0 1 0 0-6.8z M10.3 2.8h3.4l.5 2.6a7.2 7.2 0 0 1 1.9 1.1l2.5-.9 1.7 2.9-2 1.7a7.4 7.4 0 0 1 0 2.2l2 1.7-1.7 2.9-2.5-.9a7.2 7.2 0 0 1-1.9 1.1l-.5 2.6h-3.4l-.5-2.6a7.2 7.2 0 0 1-1.9-1.1l-2.5.9-1.7-2.9 2-1.7a7.4 7.4 0 0 1 0-2.2l-2-1.7 1.7-2.9 2.5.9a7.2 7.2 0 0 1 1.9-1.1z"
            }
            Film::Player => "M3.6 6.4h16.8v11.2H3.6z",
            _ => "",
        }
    }
}

/// Which panel is down over the film, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Menu {
    #[default]
    None,
    Subtitles,
    Audio,
    Speed,
    /// The film's settings: the refresh and the sound.
    Settings,
    Player,
}

/// One track the film carries, as the player answered it.
#[derive(Debug, Clone, Default)]
pub struct Track {
    pub id: i64,
    pub label: String,
    pub detail: String,
    pub selected: bool,
    /// The control plane's id for a subtitle -- `emb:3`, `ext:…` -- which is
    /// what a choice is sent as. Empty for audio, and for a control plane
    /// that sends only the player's own list.
    pub key: String,
    /// How this subtitle was timed, in the words the panel shows; empty when
    /// there is nothing worth saying.
    pub note: String,
    /// The subtitle's timing has just been put right: said once, in the pill.
    pub fresh: bool,
    /// Canonical language code (`tr`, `en`), empty when the track has none.
    pub language: String,
}

/// The languages "Tercih edilen altyazı dili" offers, in order, after
/// "Kapalı".
pub const PREFERRED_LANGUAGES: [&str; 17] = [
    "tr", "en", "de", "fr", "es", "it", "pt", "pt-br", "ru", "ar", "nl", "pl", "sv", "el", "ja", "ko", "zh",
];

/// How a preference reads on the settings panel.
pub fn preference_name(language: Option<&str>) -> String {
    language.map(language_name).unwrap_or_else(|| "Kapalı".into())
}

/// The preference `step` places along "Kapalı", then `PREFERRED_LANGUAGES`,
/// round again.
pub fn next_preference(current: Option<&str>, step: i32) -> Option<String> {
    let at = current
        .and_then(|code| PREFERRED_LANGUAGES.iter().position(|l| *l == code))
        .map(|index| index as i32 + 1)
        .unwrap_or(0);
    let count = PREFERRED_LANGUAGES.len() as i32 + 1;
    let next = (at + step).rem_euclid(count);
    (next > 0).then(|| PREFERRED_LANGUAGES[next as usize - 1].to_string())
}

/// How many rows of a panel's column are drawn at once. The sheet has room
/// for about eleven; a film with thirty subtitle languages scrolls.
pub const MENU_VISIBLE: usize = 9;

/// The first row drawn of a column of `total` with the remote on `focus`:
/// the focus stays on screen, with a row of what is next below it while
/// there is one.
pub fn menu_window(total: usize, focus: usize) -> usize {
    if total <= MENU_VISIBLE {
        return 0;
    }
    (focus + 2).saturating_sub(MENU_VISIBLE).min(total - MENU_VISIBLE)
}

/// The speeds the reference offers, in its order.
pub const SPEEDS: [f64; 10] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5];

#[derive(Default)]
pub struct NowPlaying {
    /// How many rows each group of the settings panel has, in the order the
    /// panel lists the groups (Ses, Altyazı, Video).
    pub player_groups: Vec<usize>,
    pub title: String,
    pub subtitle: String,
    pub artwork: Option<String>,
    pub backdrop: Option<String>,
    /// The film's own name as a picture, drawn over the backdrop while the
    /// film is opening. Optional: plenty of titles have no logo, and the
    /// opening screen has to read without one.
    pub logo: Option<String>,
    pub elapsed_seconds: u64,
    pub duration_seconds: u64,
    pub state: Option<PlaybackState>,
    /// "Kodi" while the display belongs to the player; what this screen is for
    /// is saying where the film is, not implying this process is drawing it.
    pub surface: String,
    /// Codec, HDR and audio, as the media core decided them. Secondary
    /// information, drawn small.
    pub technical: Vec<(String, String)>,
    pub focus: usize,
    pub note: String,

    /// Whether the film on the panel is this interface's own. The row along
    /// the bottom is a different row for Kodi, which is a view of a player
    /// this process does not own.
    pub film: bool,
    /// Which panel is down over the film.
    pub menu: Menu,
    /// Where the remote is inside it.
    pub menu_focus: usize,
    /// 0 is the languages, 1 is the tracks that language has, 2 is the delay
    /// beside them — the reference's own three columns on the subtitle and
    /// sound panels. A panel without them uses column 0 alone.
    pub menu_column: usize,
    /// Where the remote is in the track column.
    pub menu_track_focus: usize,

    pub subtitles: Vec<Track>,
    /// The settings panel's "Tercih edilen altyazı dili", as the control
    /// plane keeps it. None is "Kapalı".
    pub subtitle_preference: Option<String>,
    /// Its "Otomatik eşitleme", as the control plane keeps it. None until it
    /// has said, which reads as on -- its own default.
    pub subtitle_auto_sync: Option<bool>,
    /// Its "AutoSync uyumsuz altyazıları göster". None until the control
    /// plane has said, which reads as off -- its own default.
    pub subtitle_show_incompatible: Option<bool>,
    pub audio: Vec<Track>,
    pub speed: f64,
    pub sub_delay: f64,
    pub audio_delay: f64,
    pub scale: usize,
    /// What the scale button last did, and until when it is said. The
    /// reference names the mode in a pill at the foot of the picture for a
    /// moment and then takes it away.
    pub pill: String,
    pub pill_until: Option<std::time::Instant>,

    /// Which row the remote is on: the bar, or the buttons.
    ///
    /// The four buttons move a film ten and thirty seconds at a time, which is
    /// the right size for finding the line you missed and a useless one for
    /// finding a scene in a two hour film. So the bar is somewhere the remote
    /// can go, and Left and Right on it are a scrub that accelerates.
    pub row: Row,
    /// Where the scrub has got to, in seconds, while one is in progress. The
    /// first press of a run is sent to the player at once; the rest are drawn
    /// as they come and sent when the run settles (`scrub_to_send`).
    pub scrub: Option<u64>,
    /// What of this run has been sent to the player already.
    sent: Option<u64>,
    /// How many scrub presses have arrived in a row, which is what decides how
    /// far the next one moves.
    run: u32,
    last_scrub: Option<std::time::Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Row {
    /// The bar. Left and Right scrub, Ok goes there.
    #[default]
    Bar,
    /// The four buttons.
    Controls,
}

/// Two presses of the same direction inside this belong to one movement.
const SCRUB_RUN: std::time::Duration = std::time::Duration::from_millis(700);

/// How long after the last press of a run the film goes where the run got
/// to. Kodi waits 750 ms (`seekdelay`); a held key's repeats come every 33 to
/// 125 ms, so this is well past the gap between two of them and short enough
/// to feel like the film moved when the key came up.
const SCRUB_SETTLE: std::time::Duration = std::time::Duration::from_millis(450);

/// How far one press moves the scrub, by how many came before it.
///
/// A remote repeats about nine times a second, so this reaches ten minutes a
/// second after roughly a second and a half of holding the key down: the far
/// end of a three hour film is a few seconds away, and the first press is still
/// ten seconds, so a small correction stays small.
/// How close to the end a scrub may get.
///
/// Not one second. The length comes from the catalogue and the file is not
/// obliged to agree with it — measured on the appliance, a scrub held to the
/// right end of an eighty-five minute film reached the last second of what the
/// catalogue claimed, ran out of file, and the player exited. Nobody scrubbing
/// wants the film to end; they want to be near the end of it.
const SCRUB_TAIL: u64 = 15;

fn scrub_step(run: u32) -> u64 {
    match run {
        0..=2 => 10,
        3..=5 => 30,
        6..=9 => 60,
        10..=14 => 180,
        _ => 600,
    }
}

impl NowPlaying {
    pub fn new() -> Self {
        Self {
            note: "Şu anda bir şey oynatılmıyor".into(),
            ..Self::default()
        }
    }

    pub fn playing(&self) -> bool {
        self.state == Some(PlaybackState::Playing)
    }

    pub fn active(&self) -> bool {
        matches!(
            self.state,
            Some(PlaybackState::Playing) | Some(PlaybackState::Paused)
        )
    }

    /// Whether the length of the film is known at all.
    ///
    /// A session the worker proxies does not carry one. An interface that
    /// invents a number here draws a bar that is wrong and looks right — which
    /// is how ninety-nine minutes became three minutes and forty-five seconds
    /// on the television.
    pub fn duration_known(&self) -> bool {
        self.duration_seconds > 0
    }

    pub fn progress(&self) -> f32 {
        if self.duration_seconds == 0 {
            return 0.0;
        }
        (self.shown_seconds() as f32 / self.duration_seconds as f32).clamp(0.0, 1.0)
    }

    pub fn focused(&self) -> Control {
        CONTROLS[self.focus.min(CONTROLS.len() - 1)]
    }

    /// Which button of the film's own row the remote is on.
    pub fn focused_film(&self) -> Film {
        FILM_CONTROLS[self.focus.min(FILM_CONTROLS.len() - 1)]
    }

    fn row_len(&self) -> usize {
        if self.film {
            FILM_CONTROLS.len()
        } else {
            CONTROLS.len()
        }
    }

    // ------------------------------------------------------------ the panels

    /// The languages a panel's list is grouped by, in the order the file
    /// carries them.
    ///
    /// The reference groups by language and not by track: a disc with four
    /// English audio tracks is four rows of the same word, which says nothing
    /// about which one a viewer wants. The language is the choice; which of
    /// that language's tracks is the choice after it, in the column beside.
    pub fn languages(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for track in self.tracks() {
            if !names.contains(&track.label) {
                names.push(track.label.clone());
            }
        }
        names
    }

    /// The tracks of the open panel's kind.
    pub fn tracks(&self) -> &[Track] {
        match self.menu {
            Menu::Subtitles => &self.subtitles,
            Menu::Audio => &self.audio,
            _ => &[],
        }
    }

    /// Which row of the language column is a language rather than "off".
    fn language_index(&self) -> Option<usize> {
        match self.menu {
            // The subtitle panel leads with "off", the way the reference's
            // does, so every language is one row further down.
            Menu::Subtitles => self.menu_focus.checked_sub(1),
            Menu::Audio => Some(self.menu_focus),
            _ => None,
        }
    }

    /// The tracks the highlighted language has, as indices into `tracks()`.
    pub fn tracks_of_language(&self) -> Vec<usize> {
        let Some(index) = self.language_index() else {
            return Vec::new();
        };
        let Some(name) = self.languages().get(index).cloned() else {
            return Vec::new();
        };
        self.tracks()
            .iter()
            .enumerate()
            .filter(|(_, track)| track.label == name)
            .map(|(index, _)| index)
            .collect()
    }

    /// Ok on one of the buttons that opens a panel.
    pub fn open_menu(&mut self, menu: Menu) -> bool {
        if self.menu == menu {
            return false;
        }
        self.menu = menu;
        self.menu_column = 0;
        self.menu_track_focus = 0;
        self.menu_focus = match menu {
            // Opened on the language the film is using, not on the top of the
            // list: a panel opens where the viewer already is.
            Menu::Subtitles => self
                .subtitles
                .iter()
                .find(|track| track.selected)
                .and_then(|track| {
                    self.languages_of(&self.subtitles)
                        .iter()
                        .position(|name| *name == track.label)
                })
                .map(|index| index + 1)
                .unwrap_or(0),
            Menu::Audio => self
                .audio
                .iter()
                .find(|track| track.selected)
                .and_then(|track| {
                    self.languages_of(&self.audio)
                        .iter()
                        .position(|name| *name == track.label)
                })
                .unwrap_or(0),
            Menu::Speed => SPEEDS
                .iter()
                .position(|value| (value - self.speed).abs() < 0.01)
                .unwrap_or(3),
            Menu::Settings => 0,
            Menu::Player => 0,
            Menu::None => 0,
        };
        true
    }

    pub fn close_menu(&mut self) -> bool {
        if self.menu == Menu::None {
            return false;
        }
        self.menu = Menu::None;
        true
    }

    /// The distinct languages of one list, in the order the file carries them.
    pub fn languages_of(&self, tracks: &[Track]) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for track in tracks {
            if !names.contains(&track.label) {
                names.push(track.label.clone());
            }
        }
        names
    }

    /// How many rows the open panel's first column has.
    pub fn menu_rows(&self) -> usize {
        match self.menu {
            Menu::Subtitles => self.languages().len() + 1,
            Menu::Audio => self.languages().len(),
            Menu::Speed => SPEEDS.len(),
            Menu::Settings => self.player_groups.len(),
            Menu::Player => 2,
            Menu::None => 0,
        }
    }

    /// Whether the open panel has a delay beside its list, the way the
    /// reference's subtitle and sound panels do.
    pub fn menu_has_delay(&self) -> bool {
        matches!(self.menu, Menu::Subtitles | Menu::Audio)
    }

    /// How many settings the group the remote is on has.
    pub fn settings_in_group(&self) -> usize {
        self.player_groups.get(self.menu_focus).copied().unwrap_or(0)
    }

    /// Back inside the open panel: from a group's settings to the groups.
    /// False when Back is the panel's to close.
    pub fn menu_back(&mut self) -> bool {
        if self.menu == Menu::Settings && self.menu_column == 1 {
            self.menu_column = 0;
            return true;
        }
        false
    }

    /// Moves the remote inside the open panel. Returns whether anything moved.
    pub fn step_menu(&mut self, dx: i32, dy: i32) -> bool {
        // The settings panel: the groups, and the settings of the one the
        // remote is on beside them. Right goes into a group, Left comes back.
        if self.menu == Menu::Settings {
            if dx != 0 {
                let next = if dx > 0 && self.settings_in_group() > 0 { 1 } else { 0 };
                if next == self.menu_column {
                    return false;
                }
                self.menu_column = next;
                self.menu_track_focus = 0;
                return true;
            }
            if dy == 0 {
                return false;
            }
            let (focus, rows) = if self.menu_column == 1 {
                (&mut self.menu_track_focus, self.player_groups.get(self.menu_focus).copied().unwrap_or(0))
            } else {
                (&mut self.menu_focus, self.player_groups.len())
            };
            if rows == 0 {
                return false;
            }
            let next = (*focus as i32 + dy).clamp(0, rows as i32 - 1) as usize;
            if next == *focus {
                return false;
            }
            *focus = next;
            if self.menu_column == 0 {
                self.menu_track_focus = 0;
            }
            return true;
        }
        if dx != 0 {
            if !self.menu_has_delay() {
                return false;
            }
            let mut next = (self.menu_column as i32 + dx).clamp(0, 2) as usize;
            // A language with no tracks under it has no column to stand in —
            // "off" on the subtitle panel — so the remote passes over it.
            if next == 1 && self.tracks_of_language().is_empty() {
                next = if dx > 0 { 2 } else { 0 };
            }
            if next == self.menu_column {
                return false;
            }
            self.menu_column = next;
            self.menu_track_focus = 0;
            return true;
        }
        if dy == 0 || self.menu_column == 2 {
            return false;
        }
        if self.menu_column == 1 {
            let rows = self.tracks_of_language().len();
            if rows == 0 {
                return false;
            }
            let next = (self.menu_track_focus as i32 + dy).clamp(0, rows as i32 - 1) as usize;
            if next == self.menu_track_focus {
                return false;
            }
            self.menu_track_focus = next;
            return true;
        }
        let rows = self.menu_rows();
        if rows == 0 {
            return false;
        }
        let next = (self.menu_focus as i32 + dy).clamp(0, rows as i32 - 1) as usize;
        if next == self.menu_focus {
            return false;
        }
        self.menu_focus = next;
        // The column beside it is about the language this one is on.
        self.menu_track_focus = 0;
        true
    }

    /// The next scaling mode, and what it is called.
    pub fn next_scale(&mut self) -> (mediabox_core::ScaleMode, &'static str) {
        self.scale = (self.scale + 1) % 3;
        let (mode, name) = match self.scale {
            1 => (mediabox_core::ScaleMode::Crop, "Kırp"),
            2 => (mediabox_core::ScaleMode::Stretch, "Uzat"),
            _ => (mediabox_core::ScaleMode::Fit, "Sığdır"),
        };
        self.pill = name.to_string();
        self.pill_until = Some(std::time::Instant::now() + PILL_LINGER);
        (mode, name)
    }

    /// How the subtitle that is on was timed, for the subtitle panel.
    pub fn subtitle_note(&self) -> String {
        self.subtitles
            .iter()
            .find(|track| track.selected)
            .map(|track| track.note.clone())
            .unwrap_or_default()
    }

    /// Whether a subtitle is on at all: the line on screen is only asked for
    /// then.
    pub fn subtitle_on(&self) -> bool {
        self.subtitles.iter().any(|track| track.selected)
    }

    /// Whether the pill under the picture has had its moment.
    pub fn pill_expired(&mut self) -> bool {
        if self
            .pill_until
            .is_some_and(|until| until <= std::time::Instant::now())
        {
            self.pill_until = None;
            self.pill.clear();
            return true;
        }
        false
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        if dy != 0 {
            let next = if dy < 0 { Row::Bar } else { Row::Controls };
            if next == self.row {
                return false;
            }
            // Leaving the bar abandons an unconfirmed scrub rather than
            // carrying it somewhere it cannot be seen.
            if next == Row::Controls {
                self.scrub = None;
            }
            self.row = next;
            return true;
        }
        if dx == 0 {
            return false;
        }
        match self.row {
            Row::Bar => self.scrub_by(dx),
            Row::Controls => {
                let next = (self.focus as i32 + dx).clamp(0, self.row_len() as i32 - 1) as usize;
                if next == self.focus {
                    return false;
                }
                self.focus = next;
                true
            }
        }
    }

    /// One press of Left or Right on the bar.
    fn scrub_by(&mut self, dx: i32) -> bool {
        let now = std::time::Instant::now();
        self.run = match self.last_scrub {
            Some(last) if now.duration_since(last) < SCRUB_RUN => self.run.saturating_add(1),
            _ => 0,
        };
        self.last_scrub = Some(now);

        let from = self.scrub.unwrap_or(self.elapsed_seconds) as i64;
        let step = scrub_step(self.run) as i64 * dx.signum() as i64;
        let mut target = from + step;
        if target < 0 {
            target = 0;
        }
        // One whose length is known stops short of the end; see SCRUB_TAIL.
        // A film whose length is unknown is not clamped at all, because there
        // is nothing honest to clamp it to.
        if self.duration_seconds > 0 {
            let last = self.duration_seconds.saturating_sub(SCRUB_TAIL);
            target = target.min(last as i64);
        }
        let target = target as u64;
        if self.scrub == Some(target) {
            return false;
        }
        self.scrub = Some(target);
        true
    }

    /// Where the bar should be drawn: the scrub if there is one, else the film.
    pub fn shown_seconds(&self) -> u64 {
        self.scrub.unwrap_or(self.elapsed_seconds)
    }

    /// Confirms a scrub and says where to go. None when there was not one.
    pub fn take_scrub(&mut self) -> Option<u64> {
        let target = self.scrub.take()?;
        self.run = 0;
        self.last_scrub = None;
        self.sent = None;
        Some(target)
    }

    /// Abandons one. Returns whether there was anything to abandon.
    pub fn cancel_scrub(&mut self) -> bool {
        self.run = 0;
        self.last_scrub = None;
        self.sent = None;
        self.scrub.take().is_some()
    }

    /// Where to send the film now, if anywhere: the first press of a run at
    /// once, so a single press moves the film when it is pressed, and the
    /// run's end once it has settled, so a key held down scrubs the bar and
    /// seeks once rather than on every repeat.
    pub fn scrub_to_send(&mut self, now: std::time::Instant) -> Option<u64> {
        let target = self.scrub?;
        if self.sent.is_none() {
            self.sent = Some(target);
            self.elapsed_seconds = target;
            return Some(target);
        }
        if self.last_scrub.is_some_and(|last| now.duration_since(last) < SCRUB_SETTLE) {
            return None;
        }
        let sent = self.sent;
        self.take_scrub();
        self.elapsed_seconds = target;
        (sent != Some(target)).then_some(target)
    }

    /// One `media_status_here` answer: the interface's own player, not Kodi.
    ///
    /// Shorter than the Kodi one on purpose. The film's name, its artwork and
    /// its technical rows were carried here from the detail screen when it was
    /// opened, and asking the decoder for them again would only be able to
    /// answer worse.
    pub fn take_here(&mut self, status: &Value) {
        let seconds = |key: &str| -> u64 {
            status
                .get(key)
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|value| value as u64)
                .unwrap_or(0)
        };
        // Whoever started the film said what it was called. Never the address
        // it is being read from: a catalogue film is played through a session
        // whose address is a hexadecimal identifier, and taking a name from
        // that put `73c91b7f7242ab69a92b3654aebb344f` across somebody's film.
        if let Some(title) = status
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            self.title = title.to_string();
        }
        self.elapsed_seconds = seconds("position");
        self.duration_seconds = seconds("duration");
        self.surface = "MediaBox".into();
        self.state = Some(
            if status
                .get("paused")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                PlaybackState::Paused
            } else {
                PlaybackState::Playing
            },
        );
        self.note = match self.state {
            Some(PlaybackState::Paused) => "Duraklatıldı".into(),
            _ => String::new(),
        };

        let number = |key: &str, fallback: f64| -> f64 {
            status
                .get(key)
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite())
                .unwrap_or(fallback)
        };
        self.speed = number("speed", 1.0);
        self.sub_delay = number("sub_delay", 0.0);
        self.audio_delay = number("audio_delay", 0.0);

        if let Some(tracks) = status.get("tracks").and_then(Value::as_array) {
            // Carried and fetched subtitles, one list, when the control plane
            // offers it; the player's own list otherwise.
            let subtitles = match status.get("subtitles").and_then(Value::as_array) {
                Some(unified) => read_subtitles(unified),
                None => read_tracks(tracks, "sub"),
            };
            // A timing put on since the last answer is announced once.
            let before: Vec<(String, String)> =
                self.subtitles.iter().map(|t| (t.key.clone(), t.note.clone())).collect();
            self.subtitles = subtitles;
            for track in self.subtitles.iter_mut() {
                track.fresh = track.selected
                    && track.note.starts_with("Otomatik eşitleme ·")
                    && !before.iter().any(|(key, note)| *key == track.key && *note == track.note);
            }
            self.take_subtitle_preferences(status);
            // The preferred language leads the panel when the film has it.
            if let Some(preferred) = self.subtitle_preference.clone() {
                self.subtitles.sort_by_key(|track| track.language != preferred);
            }
            if let Some(fresh) = self.subtitles.iter().find(|t| t.fresh) {
                self.pill = fresh.note.clone();
                self.pill_until = Some(std::time::Instant::now() + PILL_LINGER * 2);
            }
            self.audio = read_tracks(tracks, "audio");
        }
    }

    /// The three subtitle settings the control plane keeps, from any answer
    /// that carries them: the film's own status, or the same status asked
    /// for with no film by "Filmler ve Diziler > Ayarlar". One copy of them
    /// in this interface, read by the film's panel and that screen alike.
    pub fn take_subtitle_preferences(&mut self, status: &Value) {
        if let Some(preferred) = status.get("subtitle_preference") {
            self.subtitle_preference = preferred.as_str().map(str::to_owned);
        }
        if let Some(on) = status.get("subtitle_auto_sync").and_then(Value::as_bool) {
            self.subtitle_auto_sync = Some(on);
        }
        if let Some(on) = status.get("subtitle_show_incompatible").and_then(Value::as_bool) {
            self.subtitle_show_incompatible = Some(on);
        }
    }

    /// One `kodi_status` answer. Everything optional, because a player between
    /// two files answers with almost nothing.
    pub fn take(&mut self, status: &Value) {
        self.state = status
            .get("state")
            .and_then(|value| serde_json::from_value::<PlaybackState>(value.clone()).ok());

        let item = status.get("item");
        let title = item
            .and_then(|item| item.get("title").or_else(|| item.get("label")))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !title.is_empty() {
            self.title = title;
        }

        let year = item
            .and_then(|item| item.get("year"))
            .and_then(Value::as_u64);
        let kind = item
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str);
        let mut facts: Vec<String> = Vec::new();
        if let Some(year) = year.filter(|y| *y > 0) {
            facts.push(year.to_string());
        }
        if let Some(kind) = kind {
            facts.push(match kind {
                "movie" => "Film".into(),
                "episode" => "Bölüm".into(),
                other => other.to_string(),
            });
        }
        self.subtitle = facts.join("  ·  ");

        if let Some(art) = item
            .and_then(|item| {
                item.pointer("/art/poster")
                    .or_else(|| item.pointer("/art/thumb"))
            })
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            self.artwork = Some(art.to_string());
        }
        if let Some(art) = item
            .and_then(|item| item.pointer("/art/fanart"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            self.backdrop = Some(art.to_string());
        }

        self.elapsed_seconds = clock_seconds(status.get("time"));
        self.duration_seconds = clock_seconds(status.get("total_time"));

        let running = status
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.surface = if running {
            "Kodi".into()
        } else {
            String::new()
        };

        self.note = match self.state {
            Some(PlaybackState::Playing) => String::new(),
            Some(PlaybackState::Paused) => "Duraklatıldı".into(),
            _ if running => "Oynatıcı hazır".into(),
            _ => "Şu anda bir şey oynatılmıyor".into(),
        };
    }

    /// What the detail screen knew about the source, carried over so the
    /// technical line is filled before Kodi has said anything.
    pub fn set_technical(&mut self, rows: Vec<(String, String)>) {
        self.technical = rows;
    }

    pub fn clear(&mut self) {
        let focus = self.focus;
        *self = Self::new();
        self.focus = focus;
    }
}

/// Kodi answers with `{hours, minutes, seconds, milliseconds}`.
fn clock_seconds(value: Option<&Value>) -> u64 {
    let Some(value) = value else { return 0 };
    let part = |name: &str| value.get(name).and_then(Value::as_u64).unwrap_or(0);
    part("hours") * 3600 + part("minutes") * 60 + part("seconds")
}

/// `01:23:45`, or `23:45` for anything under an hour.
pub fn timecode(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        // Two digits on the hour, the way the reference writes a length:
        // "01:47:58" rather than "1:47:58".
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_title_that_only_repeats_the_language_is_not_shown() {
        let mut now = NowPlaying::new();
        let entry = |id: &str, language: &str, title: &str| serde_json::json!({
            "id": id, "mpv_id": 1, "source": "embedded", "language": language, "title": title, "selected": false});
        now.take_here(&serde_json::json!({"tracks": [], "subtitles": [
            entry("emb:38", "tr", "T"), entry("emb:1", "en", "English"), entry("emb:10", "en", "English [SDH]"),
            entry("emb:15", "fr", "French Canadian"), entry("emb:3", "bg", "Bulgarian")]}));
        let details: Vec<&str> = now.subtitles.iter().map(|t| t.detail.as_str()).collect();
        assert_eq!(details, ["Orjinal", "Orjinal", "Orjinal · English [SDH]", "Orjinal · French Canadian", "Orjinal"]);
        assert_eq!(now.subtitles[4].label, "Български");
    }


    #[test]
    fn the_preference_cycles_through_off_and_the_languages() {
        assert_eq!(next_preference(None, 1).as_deref(), Some("tr"));
        assert_eq!(next_preference(Some("tr"), 1).as_deref(), Some("en"));
        assert_eq!(next_preference(Some("tr"), -1), None);
        assert_eq!(next_preference(None, -1).as_deref(), Some("zh"));
        assert_eq!(preference_name(None), "Kapalı");
        assert_eq!(preference_name(Some("tr")), "Türkçe");
    }

    #[test]
    fn a_long_panel_keeps_the_remote_on_screen() {
        assert_eq!(menu_window(5, 4), 0);
        assert_eq!(menu_window(30, 0), 0);
        assert_eq!(menu_window(30, 7), 0);
        assert_eq!(menu_window(30, 8), 1);
        for focus in 0..30 {
            let first = menu_window(30, focus);
            assert!(focus >= first && focus < first + MENU_VISIBLE, "{focus}");
        }
        assert_eq!(menu_window(30, 29), 30 - MENU_VISIBLE);
    }

    #[test]
    fn the_preferred_language_leads_the_subtitle_panel() {
        let mut now = NowPlaying::new();
        let entry = |id: &str, language: &str, selected: bool| serde_json::json!({
            "id": id, "mpv_id": null, "source": "addon_external", "language": language, "selected": selected});
        now.take_here(&serde_json::json!({"tracks": [], "subtitle_preference": "tr", "subtitles": [
            entry("emb:1", "en", false), entry("ext:a", "de", false), entry("ext:b", "tr", true)]}));
        assert!(now.open_menu(Menu::Subtitles));
        assert_eq!(now.languages()[0], "Türkçe");
        // Opened on it: "Etkisizleştirildi" is row 0, the preference row 1.
        assert_eq!(now.menu_focus, 1);
        // A film without it does not list it.
        now.take_here(&serde_json::json!({"tracks": [], "subtitle_preference": "tr", "subtitles": [
            entry("emb:1", "en", false)]}));
        assert!(!now.languages().contains(&"Türkçe".to_string()));
    }


    #[test]
    fn a_fetched_subtitle_is_named_by_the_provider_that_found_it() {
        let mut now = NowPlaying::new();
        now.take_here(&serde_json::json!({
            "tracks": [],
            "subtitle_show_incompatible": true,
            "subtitles": [
                {"id": "ext:c", "mpv_id": null, "source": "provider_external", "language": "tr", "title": "WEB-DL",
                 "provider": "opensubtitles_com", "provider_name": "OpenSubtitles.com", "selected": false},
                {"id": "ext:v", "mpv_id": null, "source": "addon_external", "language": "tr", "title": null,
                 "provider": "opensubtitles_v3", "provider_name": "OpenSubtitles v3", "selected": false},
                {"id": "ext:q", "mpv_id": null, "source": "provider_external", "language": "tr", "title": null,
                 "provider_name": "OpenSubtitles.com", "unavailable": "quota-exhausted", "selected": false},
                // The stream's own, whatever it claims, is not a provider's.
                {"id": "ext:s", "mpv_id": null, "source": "stream_external", "language": "tr", "title": null,
                 "provider_name": "OpenSubtitles.com", "selected": false},
                {"id": "ext:x", "mpv_id": null, "source": "addon_external", "language": "tr", "title": null, "selected": false},
            ],
        }));
        let details: Vec<&str> = now.subtitles.iter().map(|t| t.detail.as_str()).collect();
        assert_eq!(
            details,
            [
                "OpenSubtitles.com · WEB-DL",
                "OpenSubtitles v3",
                "OpenSubtitles.com · İndirme kotası doldu",
                "Kaynakla gelen",
                "Harici",
            ]
        );
        assert!(now.subtitles.iter().all(|t| t.label == "Türkçe"));
        assert_eq!(now.subtitle_show_incompatible, Some(true));
    }

    /// Asked with no film, the control plane's status carries no tracks but
    /// still says what it keeps: the settings screen reads it the same way.
    #[test]
    fn the_kept_subtitle_settings_are_read_without_a_film() {
        let mut now = NowPlaying::new();
        assert_eq!(now.subtitle_auto_sync, None);
        let idle = serde_json::json!({"subtitle_preference": "en", "subtitle_auto_sync": false,
            "subtitle_show_incompatible": true});
        now.take_subtitle_preferences(&idle);
        assert_eq!(now.subtitle_preference.as_deref(), Some("en"));
        assert_eq!((now.subtitle_auto_sync, now.subtitle_show_incompatible), (Some(false), Some(true)));
        // "Kapalı" is a null, and it is an answer.
        now.take_subtitle_preferences(&serde_json::json!({"subtitle_preference": null}));
        assert_eq!(now.subtitle_preference, None);
        assert_eq!(now.subtitle_auto_sync, Some(false), "what it did not say is kept");
    }

    #[test]
    fn carried_and_fetched_subtitles_share_the_panel_by_language() {
        let mut now = NowPlaying::new();
        now.take_here(&serde_json::json!({
            "position": 10.0, "duration": 100.0, "paused": false,
            "tracks": [],
            "subtitles": [
                {"id": "emb:1", "mpv_id": 1, "source": "embedded", "language": "en", "title": "SDH", "selected": false, "forced": false},
                {"id": "emb:2", "mpv_id": 2, "source": "embedded", "language": "tr", "title": null, "selected": false, "forced": true},
                {"id": "ext:a", "mpv_id": 3, "source": "addon_external", "language": "tr", "title": "WEB-DL", "selected": true,
                 "sync": {"state": "applied", "model": "offset", "offset": 1.82, "scale": 1.0, "confidence": 0.97}},
                {"id": "ext:b", "mpv_id": null, "source": "stream_external", "language": "tr", "title": null, "selected": false, "sync": null},
            ],
        }));
        assert_eq!(now.subtitles.len(), 4);
        assert!(now.open_menu(Menu::Subtitles));
        assert_eq!(now.languages(), vec!["English".to_string(), "Türkçe".to_string()]);
        // Opened on the language in use; its column lists all three.
        assert_eq!(now.menu_focus, 2);
        let rows: Vec<String> = now.tracks_of_language().iter().map(|i| now.subtitles[*i].detail.clone()).collect();
        assert_eq!(rows, ["Orjinal · Zorunlu", "Harici · WEB-DL", "Kaynakla gelen"]);
        assert_eq!(now.subtitles[2].key, "ext:a");
        assert_eq!(now.subtitle_note(), "Otomatik eşitleme · +1,82 sn · %97");
        // Announced once, in the pill, and not again for the same result.
        assert_eq!(now.pill, "Otomatik eşitleme · +1,82 sn · %97");
        now.pill.clear();
        now.take_here(&serde_json::json!({"tracks": [], "subtitles": [
            {"id": "ext:a", "mpv_id": 3, "source": "addon_external", "language": "tr", "selected": true,
             "sync": {"state": "applied", "model": "offset", "offset": 1.82, "scale": 1.0, "confidence": 0.97}}]}));
        assert_eq!(now.pill, "");
    }

    #[test]
    fn a_refused_subtitle_says_it_cannot_be_timed_and_an_analysis_says_nothing() {
        let note = |sync: serde_json::Value| sync_note(Some(&sync));
        // Why (another timebase, another cut) is not the television's to say.
        for refusal in ["REJECT_TIMEBASE_MISMATCH", "REJECT_WRONG_RELEASE", "REJECT_PARTIAL", "INCONCLUSIVE"] {
            assert_eq!(
                note(serde_json::json!({"state": "rejected", "model": "rejected", "eligibility": refusal})),
                "Otomatik eşitleme kullanılamıyor"
            );
        }
        assert_eq!(note(serde_json::json!({"state": "analysing"})), "");
        // An older control plane's refusal, with no pre-filter answer.
        assert_eq!(note(serde_json::json!({"state": "rejected", "model": "rejected"})), "");
        assert_eq!(
            note(serde_json::json!({"state": "applied", "model": "offset", "offset": -0.5, "confidence": 0.804})),
            "Otomatik eşitleme · -0,50 sn · %80"
        );
    }

    #[test]
    fn the_settings_panel_is_groups_and_the_settings_of_one() {
        let mut now = NowPlaying::new();
        // Ses (4 rows), Altyazı (3), Video (1).
        now.player_groups = vec![4, 3, 1];
        assert!(now.open_menu(Menu::Settings));
        assert_eq!((now.menu_focus, now.menu_column), (0, 0));
        assert_eq!(now.menu_rows(), 3);
        // Down the groups; the settings beside follow the group.
        assert!(now.step_menu(0, 1));
        assert_eq!((now.menu_focus, now.settings_in_group()), (1, 3));
        // Right goes in, and moves among that group's settings only.
        assert!(now.step_menu(1, 0));
        assert_eq!((now.menu_column, now.menu_track_focus), (1, 0));
        assert!(now.step_menu(0, 1) && now.step_menu(0, 1));
        assert!(!now.step_menu(0, 1), "three settings in Altyazı");
        assert_eq!(now.menu_track_focus, 2);
        // No third column.
        assert!(!now.step_menu(1, 0));
        // Back, and Left, come out to the groups; Back there is the panel's.
        assert!(now.menu_back());
        assert_eq!(now.menu_column, 0);
        assert!(!now.menu_back());
        assert!(now.step_menu(1, 0) && now.step_menu(-1, 0));
        assert_eq!(now.menu_column, 0);
        // A group with nothing in it cannot be entered.
        now.player_groups = vec![4, 0];
        now.menu_focus = 1;
        assert!(!now.step_menu(1, 0));
    }

    #[test]
    fn the_auto_sync_setting_is_read_from_the_status_and_is_on_until_said() {
        let mut now = NowPlaying::new();
        assert_eq!(now.subtitle_auto_sync, None);
        now.take_here(&serde_json::json!({"tracks": [], "subtitle_auto_sync": false}));
        assert_eq!(now.subtitle_auto_sync, Some(false));
        // A status without it leaves what is known.
        now.take_here(&serde_json::json!({"tracks": []}));
        assert_eq!(now.subtitle_auto_sync, Some(false));
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn an_idle_player_says_so_and_keeps_its_focus() {
        let mut now = NowPlaying::new();
        now.focus = 2;
        now.take(&json!({"running": false, "state": "idle"}));
        assert!(!now.active());
        assert_eq!(now.focus, 2);
        assert_eq!(now.progress(), 0.0);
    }

    #[test]
    fn a_position_becomes_a_bar_and_a_timecode() {
        let mut now = NowPlaying::new();
        now.take(&json!({
            "running": true,
            "state": "playing",
            "item": {"title": "Arrival", "year": 2016, "type": "movie"},
            "time": {"hours": 0, "minutes": 30, "seconds": 0},
            "total_time": {"hours": 2, "minutes": 0, "seconds": 0},
        }));
        assert!(now.playing());
        assert_eq!(now.title, "Arrival");
        assert_eq!(now.subtitle, "2016  ·  Film");
        assert_eq!(now.elapsed_seconds, 1800);
        assert!((now.progress() - 0.25).abs() < 0.001);
        assert_eq!(timecode(now.elapsed_seconds), "30:00");
        assert_eq!(timecode(now.duration_seconds), "02:00:00");
    }

    #[test]
    fn a_title_is_not_lost_between_two_answers() {
        let mut now = NowPlaying::new();
        now.take(&json!({"running": true, "state": "playing", "item": {"title": "Dune"}}));
        now.take(&json!({"running": true, "state": "paused"}));
        assert_eq!(now.title, "Dune");
        assert_eq!(now.note, "Duraklatıldı");
    }

    #[test]
    fn the_controls_never_step_off_the_end() {
        let mut now = NowPlaying::new();
        now.step(0, 1);
        assert_eq!(now.row, Row::Controls);
        for _ in 0..10 {
            now.step(1, 0);
        }
        assert_eq!(now.focused(), Control::Stop);
        for _ in 0..10 {
            now.step(-1, 0);
        }
        assert_eq!(now.focused(), Control::SeekBack);
    }

    /// The remote starts on the rule, because moving a film is the first thing
    /// a key press during one is for.
    #[test]
    fn the_remote_starts_on_the_rule() {
        let now = NowPlaying::new();
        assert_eq!(now.row, Row::Bar);
        assert_eq!(now.scrub, None);
    }

    /// The fault this exists for: four buttons that move a film ten and thirty
    /// seconds at a time cannot reach the middle of a two hour one. Holding a
    /// direction has to cross it.
    #[test]
    fn holding_a_direction_crosses_a_long_film() {
        let mut now = NowPlaying::new();
        now.duration_seconds = 2 * 3600;
        for _ in 0..40 {
            now.step(1, 0);
        }
        let reached = now.scrub.expect("a scrub");
        assert!(
            reached > 3600,
            "forty presses reached only {reached} seconds of a two hour film"
        );
    }

    /// And the first press stays small, so a correction is a correction.
    #[test]
    fn the_first_press_is_ten_seconds() {
        let mut now = NowPlaying::new();
        now.duration_seconds = 3600;
        now.elapsed_seconds = 600;
        now.step(1, 0);
        assert_eq!(now.scrub, Some(610));
        now.step(-1, 0);
        assert_eq!(now.scrub, Some(600));
    }

    /// A scrub never leaves the film.
    #[test]
    fn a_scrub_stays_inside_the_film() {
        let mut now = NowPlaying::new();
        now.duration_seconds = 100;
        now.elapsed_seconds = 50;
        for _ in 0..30 {
            now.step(-1, 0);
        }
        assert_eq!(now.scrub, Some(0));
        for _ in 0..60 {
            now.step(1, 0);
        }
        assert_eq!(now.scrub, Some(100 - SCRUB_TAIL));
    }

    /// The fault that ended a film instead of moving it: a scrub held to the
    /// right reached the last second the catalogue claimed, the file was
    /// shorter than that, and the player exited.
    #[test]
    fn a_scrub_never_reaches_the_end_of_the_film() {
        let mut now = NowPlaying::new();
        now.duration_seconds = 85 * 60;
        for _ in 0..200 {
            now.step(1, 0);
        }
        let reached = now.scrub.expect("a scrub");
        assert!(
            reached + SCRUB_TAIL <= now.duration_seconds,
            "a scrub reached {reached} of {}",
            now.duration_seconds
        );
    }

    /// Confirming hands the target over once; abandoning gives nothing back.
    #[test]
    fn a_press_moves_the_film_at_once_and_a_held_key_when_it_settles() {
        let mut now = NowPlaying::new();
        now.duration_seconds = 3600;
        now.elapsed_seconds = 600;
        let t0 = std::time::Instant::now();
        // One press: the film goes ten seconds on, now, and not again.
        now.step(1, 0);
        assert_eq!(now.scrub_to_send(t0), Some(610));
        assert_eq!(now.scrub_to_send(t0 + std::time::Duration::from_millis(100)), None);
        assert_eq!(now.scrub_to_send(t0 + SCRUB_SETTLE * 2), None);
        assert_eq!(now.scrub, None);
        assert_eq!(now.elapsed_seconds, 610);

        // A held key: the first repeat is sent, the rest are drawn, and the
        // film goes where they got to once they stop.
        now.step(1, 0);
        assert_eq!(now.scrub_to_send(std::time::Instant::now()), Some(620));
        for _ in 0..8 {
            now.step(1, 0);
            assert_eq!(now.scrub_to_send(std::time::Instant::now()), None);
        }
        let reached = now.scrub.expect("a scrub in progress");
        assert!(reached > 700, "a held key accelerates: {reached}");
        assert_eq!(now.scrub_to_send(std::time::Instant::now() + SCRUB_SETTLE), Some(reached));
        assert_eq!(now.scrub, None);
    }

    #[test]
    fn a_scrub_is_taken_once_or_abandoned() {
        let mut now = NowPlaying::new();
        now.duration_seconds = 3600;
        now.step(1, 0);
        assert_eq!(now.take_scrub(), Some(10));
        assert_eq!(now.take_scrub(), None);

        now.step(1, 0);
        assert!(now.cancel_scrub());
        assert_eq!(now.scrub, None);
        assert!(!now.cancel_scrub());
    }

    /// Leaving the rule abandons an unconfirmed move rather than carrying it
    /// somewhere it cannot be seen.
    #[test]
    fn walking_to_the_words_drops_the_scrub() {
        let mut now = NowPlaying::new();
        now.duration_seconds = 3600;
        now.step(1, 0);
        assert!(now.scrub.is_some());
        now.step(0, 1);
        assert_eq!(now.scrub, None);
    }

    /// The bar follows the tick while it is being moved, not the film.
    #[test]
    fn the_bar_follows_the_tick() {
        let mut now = NowPlaying::new();
        now.duration_seconds = 1000;
        now.elapsed_seconds = 100;
        assert!((now.progress() - 0.1).abs() < 0.001);
        now.step(1, 0);
        assert_eq!(now.shown_seconds(), 110);
        assert!((now.progress() - 0.11).abs() < 0.001);
    }

    #[test]
    fn play_and_pause_are_the_same_button_with_two_faces() {
        assert_eq!(Control::PlayPause.label(true), "Duraklat");
        assert_eq!(Control::PlayPause.label(false), "Devam Et");
        assert_ne!(
            Control::PlayPause.mark(true),
            Control::PlayPause.mark(false)
        );
    }
}

/// How long the scaling mode is named under the picture.
const PILL_LINGER: std::time::Duration = std::time::Duration::from_millis(1600);

/// The player's own track list, as the two lists the panels draw.
///
/// What a track is called is the addon's or the file's, in that order: a
/// language on its own is what most files carry, a title is what a few carry
/// instead, and a track with neither is still a track and is numbered rather
/// than hidden.
fn read_tracks(tracks: &[Value], kind: &str) -> Vec<Track> {
    let mut out = Vec::new();
    for track in tracks {
        if track.get("type").and_then(Value::as_str) != Some(kind) {
            continue;
        }
        let id = track.get("id").and_then(Value::as_i64).unwrap_or(0);
        let language = track
            .get("lang")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(language_name);
        let title = track
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let label = language
            .clone()
            .or_else(|| title.clone())
            .unwrap_or_else(|| format!("{id}. parça"));
        // The other of the two, when the first was not the only thing known.
        let detail = match (language.is_some(), title) {
            (true, Some(title)) => title,
            _ => track
                .get("codec")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        };
        out.push(Track {
            id,
            label,
            detail,
            selected: track
                .get("selected")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ..Track::default()
        });
    }
    out
}

/// The control plane's one list of subtitles: carried, fetched with the
/// stream, fetched by an addon. Grouped by language like the player's own,
/// with where each came from beside it.
fn read_subtitles(list: &[Value]) -> Vec<Track> {
    list.iter()
        .filter_map(|entry| {
            let key = entry.get("id").and_then(Value::as_str)?.to_string();
            let id = entry.get("mpv_id").and_then(Value::as_i64).unwrap_or(-1);
            let code = entry
                .get("language")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or_default()
                .to_string();
            let language = (!code.is_empty()).then(|| language_name(&code));
            let title = entry
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| telling_title(value, &code))
                .map(str::to_string);
            // A fetched one is named by the provider that found it
            // ("OpenSubtitles.com", "OpenSubtitles v3"); the rest by where
            // they came from. Only what the control plane says: a subtitle
            // the stream carried is never given a provider's name.
            let provider = entry
                .get("provider_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty());
            let origin = match (entry.get("source").and_then(Value::as_str), provider) {
                (Some("embedded"), _) => "Orjinal",
                (Some("stream_external"), _) => "Kaynakla gelen",
                (Some("local"), _) => "Yerel",
                (_, Some(name)) => name,
                _ => "Harici",
            };
            let label = language
                .clone()
                .or_else(|| title.clone())
                .unwrap_or_else(|| "Bilinmeyen dil".into());
            let mut detail = match title.filter(|_| language.is_some()) {
                Some(title) => format!("{origin} · {title}"),
                None => origin.to_string(),
            };
            if entry.get("forced").and_then(Value::as_bool).unwrap_or(false) {
                detail.push_str(" · Zorunlu");
            }
            if entry.get("unavailable").and_then(Value::as_str) == Some("quota-exhausted") {
                detail.push_str(" · İndirme kotası doldu");
            }
            // A picture subtitle has no text for this interface to draw.
            if matches!(
                entry.get("codec").and_then(Value::as_str),
                Some("hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub")
            ) {
                detail.push_str(" · Resim (gösterilemez)");
            }
            Some(Track {
                id,
                label,
                detail,
                selected: entry.get("selected").and_then(Value::as_bool).unwrap_or(false),
                key,
                note: sync_note(entry.get("sync")),
                fresh: false,
                language: code,
            })
        })
        .collect()
}

/// Whether a track's title says anything its language does not already.
///
/// Muxers write the language again ("Turkish", "Türkçe", "tur") or a stub of
/// it -- a Ted Lasso release carries its Turkish track as "T". Beside the
/// language that reads as a fault, so only titles that tell two tracks apart
/// ("English [SDH]", "French Canadian", "Forced") are shown.
fn telling_title(title: &str, code: &str) -> bool {
    let lower = title.trim().to_lowercase();
    if lower.chars().count() <= 2 {
        return false;
    }
    if code.is_empty() {
        return true;
    }
    let names = [
        code.to_lowercase(),
        language_name(code).to_lowercase(),
        english_name(code).to_lowercase(),
    ];
    !names.iter().any(|name| *name == lower)
}

/// The English name of a language, which is what muxers most often write
/// as a track's title.
fn english_name(code: &str) -> &'static str {
    match code {
        "tr" => "turkish",
        "en" => "english",
        "de" => "german",
        "fr" => "french",
        "es" => "spanish",
        "it" => "italian",
        "pt" => "portuguese",
        "pt-br" => "portuguese brazilian",
        "ru" => "russian",
        "ar" => "arabic",
        "nl" => "dutch",
        "pl" => "polish",
        "sv" => "swedish",
        "da" => "danish",
        "no" => "norwegian",
        "fi" => "finnish",
        "el" => "greek",
        "he" => "hebrew",
        "hu" => "hungarian",
        "cs" => "czech",
        "ro" => "romanian",
        "bg" => "bulgarian",
        "uk" => "ukrainian",
        "ja" => "japanese",
        "ko" => "korean",
        "zh" => "chinese",
        "hi" => "hindi",
        _ => "",
    }
}

/// What the panel says about a subtitle's automatic timing. Only what a
/// viewer can use: an analysis in progress is the log's business, and why a
/// subtitle was refused (its timebase, its cut) is too -- the television says
/// only that it cannot be timed.
fn sync_note(sync: Option<&Value>) -> String {
    let Some(sync) = sync.filter(|value| !value.is_null()) else {
        return String::new();
    };
    let state = sync.get("state").and_then(Value::as_str).unwrap_or_default();
    let offset = sync.get("offset").and_then(Value::as_f64).unwrap_or(0.0);
    let confidence = sync.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
    let judged = sync.get("eligibility").is_some_and(|value| !value.is_null());
    match state {
        "rejected" if judged => "Otomatik eşitleme kullanılamıyor".into(),
        "applied" => format!(
            "Otomatik eşitleme · {} sn · %{}",
            signed_seconds(offset),
            (confidence * 100.0).round() as i64
        ),
        "ready" => "Otomatik eşitleme hazır · Tamam ile uygula".into(),
        _ => String::new(),
    }
}

fn signed_seconds(seconds: f64) -> String {
    let text = format!("{:+.2}", seconds);
    text.replace('.', ",")
}

/// The languages a film actually carries, in the interface's own language.
/// Anything else is left as the file wrote it — a wrong name is worse than a
/// code.
fn language_name(code: &str) -> String {
    let name = match code.to_ascii_lowercase().as_str() {
        "tur" | "tr" => "Türkçe",
        "eng" | "en" => "English",
        "spa" | "es" => "Español",
        "fre" | "fra" | "fr" => "Français",
        "ger" | "deu" | "de" => "Deutsch",
        "ita" | "it" => "Italiano",
        "pob" | "pt-br" => "Português (Brasil)",
        "por" | "pt" => "Português",
        "rus" | "ru" => "Русский",
        "ara" | "ar" => "العربية",
        "jpn" | "ja" => "日本語",
        "kor" | "ko" => "한국어",
        "chi" | "zho" | "zh" => "中文",
        "dut" | "nld" | "nl" => "Nederlands",
        "pol" | "pl" => "Polski",
        "swe" | "sv" => "Svenska",
        "dan" | "da" => "Dansk",
        "nor" | "no" => "Norsk",
        "fin" | "fi" => "Suomi",
        "gre" | "ell" | "el" => "Ελληνικά",
        "heb" | "he" => "עברית",
        "hin" | "hi" => "हिन्दी",
        "cze" | "ces" | "cs" => "Čeština",
        "hun" | "hu" => "Magyar",
        "rum" | "ron" | "ro" => "Română",
        "ukr" | "uk" => "Українська",
        "bul" | "bg" => "Български",
        "est" | "et" => "Eesti",
        "lit" | "lt" => "Lietuvių",
        "lav" | "lv" => "Latviešu",
        "may" | "msa" | "ms" => "Bahasa Melayu",
        "ind" | "id" => "Bahasa Indonesia",
        "slo" | "slk" | "sk" => "Slovenčina",
        "slv" | "sl" => "Slovenščina",
        "hrv" | "hr" => "Hrvatski",
        "srp" | "sr" => "Српски",
        "tam" | "ta" => "தமிழ்",
        "tel" | "te" => "తెలుగు",
        "tha" | "th" => "ไทย",
        "vie" | "vi" => "Tiếng Việt",
        "per" | "fas" | "fa" => "فارسی",
        "cat" | "ca" => "Català",
        "ice" | "isl" | "is" => "Íslenska",
        other => return other.to_string(),
    };
    name.to_string()
}
