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
    /// Hand the film over to Kodi, where it has got to.
    ///
    /// This is where the handover lives now, and not on the film's page: a
    /// viewer does not know before starting a film that they will want Kodi's
    /// player for it, and asking them to choose a player before they have seen
    /// a frame is asking the wrong question at the wrong time.
    ToKodi,
    Stop,
}

pub const CONTROLS: [Control; 5] = [
    Control::SeekBack,
    Control::PlayPause,
    Control::SeekForward,
    Control::ToKodi,
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
            Control::ToKodi => "Kodi'ye Aktar",
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
            // A television with a picture on it.
            Control::ToKodi => "M2 4h20v13H2zM9 19h6v2H9zM10.5 8.2l5 3.3-5 3.3z",
            Control::Stop => "M6 6h12v12H6z",
        }
    }
}

#[derive(Default)]
pub struct NowPlaying {
    pub title: String,
    pub subtitle: String,
    pub artwork: Option<String>,
    pub backdrop: Option<String>,
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
                let next = (self.focus as i32 + dx).clamp(0, CONTROLS.len() as i32 - 1) as usize;
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

/// `1:23:45`, or `23:45` for anything under an hour.
pub fn timecode(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
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
        assert_eq!(timecode(now.duration_seconds), "2:00:00");
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
