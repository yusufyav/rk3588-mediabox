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
    Player,
}

pub const FILM_CONTROLS: [Film; 7] = [
    Film::PlayPause,
    Film::Restart,
    Film::Subtitles,
    Film::Audio,
    Film::Speed,
    Film::Scale,
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
    Player,
}

/// One track the film carries, as the player answered it.
#[derive(Debug, Clone, Default)]
pub struct Track {
    pub id: i64,
    pub label: String,
    pub detail: String,
    pub selected: bool,
}

/// The speeds the reference offers, in its order.
pub const SPEEDS: [f64; 10] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5];

#[derive(Default)]
pub struct NowPlaying {
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
    /// film keeps playing behind it; nothing is asked of the player until the
    /// scrub is confirmed.
    pub scrub: Option<u64>,
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
            Menu::Player => 2,
            Menu::None => 0,
        }
    }

    /// Whether the open panel has a delay beside its list, the way the
    /// reference's subtitle and sound panels do.
    pub fn menu_has_delay(&self) -> bool {
        matches!(self.menu, Menu::Subtitles | Menu::Audio)
    }

    /// Moves the remote inside the open panel. Returns whether anything moved.
    pub fn step_menu(&mut self, dx: i32, dy: i32) -> bool {
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
        Some(target)
    }

    /// Abandons one. Returns whether there was anything to abandon.
    pub fn cancel_scrub(&mut self) -> bool {
        self.run = 0;
        self.last_scrub = None;
        self.scrub.take().is_some()
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
            self.subtitles = read_tracks(tracks, "sub");
            self.audio = read_tracks(tracks, "audio");
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
        });
    }
    out
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
        other => return other.to_string(),
    };
    name.to_string()
}
