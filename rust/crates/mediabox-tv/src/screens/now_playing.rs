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

    pub fn progress(&self) -> f32 {
        if self.duration_seconds == 0 {
            return 0.0;
        }
        (self.elapsed_seconds as f32 / self.duration_seconds as f32).clamp(0.0, 1.0)
    }

    pub fn focused(&self) -> Control {
        CONTROLS[self.focus.min(CONTROLS.len() - 1)]
    }

    pub fn step(&mut self, dx: i32, _dy: i32) -> bool {
        if dx == 0 {
            return false;
        }
        let next = (self.focus as i32 + dx).clamp(0, CONTROLS.len() as i32 - 1) as usize;
        if next == self.focus {
            return false;
        }
        self.focus = next;
        true
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
        // A film opened from this interface carried its name here; one opened
        // from the web interface did not, and "MediaBox" over somebody's film
        // is worse than the file's own name.
        if self.title.is_empty()
            && let Some(source) = status.get("source").and_then(Value::as_str)
        {
            let tail = source.rsplit('/').next().unwrap_or(source);
            let name = tail.split('?').next().unwrap_or(tail);
            let name = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
            if !name.is_empty() {
                self.title = name.replace(['.', '_'], " ");
            }
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
        for _ in 0..10 {
            now.step(1, 0);
        }
        assert_eq!(now.focused(), Control::Stop);
        for _ in 0..10 {
            now.step(-1, 0);
        }
        assert_eq!(now.focused(), Control::SeekBack);
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
