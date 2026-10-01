//! What "Filmler ve Diziler" keeps for itself: the choices its own settings
//! screen makes that nothing else in the box owns.
//!
//! Everything else on that screen belongs to somebody else and is only shown
//! there: the refresh to the display's control plane, the sound to the
//! daemon's `audio.json`, the subtitle language and its timing to the
//! daemon's `subtitles.json`, the account to the media core. What is left is
//! this interface's own business -- which player a chosen source opens in,
//! where a film left part-way starts, and how the subtitle this interface
//! draws over the film looks -- and it is kept here, in one file, read once at
//! start and written whole when it changes.
//!
//! Every default is what the interface did before there was a choice, so a
//! box that has never opened the screen behaves exactly as it always did.
//! A file from a newer build, or a value this build does not know, falls back
//! to the default for that field rather than throwing the rest away.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which player a chosen source opens in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerChoice {
    /// This interface's own, on its video plane.
    #[default]
    MediaBox,
    /// Kodi: the television is handed over and the film opens there.
    Kodi,
    /// Asked every time.
    Ask,
}

impl PlayerChoice {
    pub const ALL: [PlayerChoice; 3] = [PlayerChoice::MediaBox, PlayerChoice::Kodi, PlayerChoice::Ask];

    pub fn label(self) -> &'static str {
        match self {
            PlayerChoice::MediaBox => "MediaBox",
            PlayerChoice::Kodi => "Kodi",
            PlayerChoice::Ask => "Sor",
        }
    }
}

/// Where a film the account left part-way starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeChoice {
    /// "Nereden başlasın?", as it always asked.
    #[default]
    Ask,
    Resume,
    FromStart,
}

impl ResumeChoice {
    pub const ALL: [ResumeChoice; 3] = [ResumeChoice::Ask, ResumeChoice::Resume, ResumeChoice::FromStart];

    pub fn label(self) -> &'static str {
        match self {
            ResumeChoice::Ask => "Sor",
            ResumeChoice::Resume => "Kaldığı yerden",
            ResumeChoice::FromStart => "Baştan",
        }
    }

    /// The second a film left at `left_at` starts from, or `None` when the
    /// viewer is to be asked.
    pub fn start(self, left_at: u64) -> Option<u64> {
        if left_at == 0 {
            return Some(0);
        }
        match self {
            ResumeChoice::Ask => None,
            ResumeChoice::Resume => Some(left_at),
            ResumeChoice::FromStart => Some(0),
        }
    }
}

/// The subtitle's size, against the 4 % of the panel's short side it has
/// always been drawn at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionSize {
    Small,
    #[default]
    Normal,
    Large,
    Larger,
}

impl CaptionSize {
    pub const ALL: [CaptionSize; 4] = [CaptionSize::Small, CaptionSize::Normal, CaptionSize::Large, CaptionSize::Larger];

    pub fn label(self) -> &'static str {
        match self {
            CaptionSize::Small => "Küçük",
            CaptionSize::Normal => "Normal",
            CaptionSize::Large => "Büyük",
            CaptionSize::Larger => "Çok büyük",
        }
    }

    pub fn scale(self) -> f32 {
        match self {
            CaptionSize::Small => 0.8,
            CaptionSize::Normal => 1.0,
            CaptionSize::Large => 1.2,
            CaptionSize::Larger => 1.4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionColor {
    #[default]
    White,
    Yellow,
    Cyan,
    Green,
}

impl CaptionColor {
    pub const ALL: [CaptionColor; 4] = [CaptionColor::White, CaptionColor::Yellow, CaptionColor::Cyan, CaptionColor::Green];

    pub fn label(self) -> &'static str {
        match self {
            CaptionColor::White => "Beyaz",
            CaptionColor::Yellow => "Sarı",
            CaptionColor::Cyan => "Camgöbeği",
            CaptionColor::Green => "Yeşil",
        }
    }

    /// 0xRRGGBB. White is the white it has always been drawn in.
    pub fn rgb(self) -> u32 {
        match self {
            CaptionColor::White => 0xffffff,
            CaptionColor::Yellow => 0xffe14d,
            CaptionColor::Cyan => 0x6ee7f5,
            CaptionColor::Green => 0x7cf08c,
        }
    }
}

/// The black outline around the letters, as a fraction of the letter's size.
/// "Normal" is Stremio's 0.15rem against a 4vmin line, which is what the
/// renderer has drawn since it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionEdge {
    None,
    Thin,
    #[default]
    Normal,
    Thick,
}

impl CaptionEdge {
    pub const ALL: [CaptionEdge; 4] = [CaptionEdge::None, CaptionEdge::Thin, CaptionEdge::Normal, CaptionEdge::Thick];

    pub fn label(self) -> &'static str {
        match self {
            CaptionEdge::None => "Yok",
            CaptionEdge::Thin => "İnce",
            CaptionEdge::Normal => "Normal",
            CaptionEdge::Thick => "Kalın",
        }
    }

    pub fn width(self) -> f32 {
        match self {
            CaptionEdge::None => 0.0,
            CaptionEdge::Thin => 0.03,
            CaptionEdge::Normal => 0.05,
            CaptionEdge::Thick => 0.08,
        }
    }
}

/// A band behind each line. None by default: Stremio and Kodi both draw an
/// outlined face with nothing behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionBackground {
    #[default]
    None,
    Shaded,
    Solid,
}

impl CaptionBackground {
    pub const ALL: [CaptionBackground; 3] = [CaptionBackground::None, CaptionBackground::Shaded, CaptionBackground::Solid];

    pub fn label(self) -> &'static str {
        match self {
            CaptionBackground::None => "Yok",
            CaptionBackground::Shaded => "Yarı saydam",
            CaptionBackground::Solid => "Koyu",
        }
    }

    /// 0xAARRGGBB; fully transparent for none.
    pub fn argb(self) -> u32 {
        match self {
            CaptionBackground::None => 0x0000_0000,
            CaptionBackground::Shaded => 0x9900_0000,
            CaptionBackground::Solid => 0xe600_0000,
        }
    }
}

/// How far above the picture's bottom edge the last line sits, as a fraction
/// of its height. 5 % is where it has always been.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionPosition {
    #[default]
    Bottom,
    Raised,
    High,
}

impl CaptionPosition {
    pub const ALL: [CaptionPosition; 3] = [CaptionPosition::Bottom, CaptionPosition::Raised, CaptionPosition::High];

    pub fn label(self) -> &'static str {
        match self {
            CaptionPosition::Bottom => "Alt kenar",
            CaptionPosition::Raised => "Biraz yukarı",
            CaptionPosition::High => "Yukarı",
        }
    }

    pub fn lift(self) -> f32 {
        match self {
            CaptionPosition::Bottom => 0.05,
            CaptionPosition::Raised => 0.10,
            CaptionPosition::High => 0.15,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CaptionStyle {
    #[serde(default, deserialize_with = "or_default")]
    pub size: CaptionSize,
    #[serde(default, deserialize_with = "or_default")]
    pub color: CaptionColor,
    #[serde(default, deserialize_with = "or_default")]
    pub edge: CaptionEdge,
    #[serde(default, deserialize_with = "or_default")]
    pub background: CaptionBackground,
    #[serde(default, deserialize_with = "or_default")]
    pub position: CaptionPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaPreferences {
    #[serde(default, deserialize_with = "or_default")]
    pub player: PlayerChoice,
    #[serde(default, deserialize_with = "or_default")]
    pub resume: ResumeChoice,
    #[serde(default, deserialize_with = "or_default")]
    pub caption: CaptionStyle,
}

/// A value this build does not know is the default, not a reason to drop the
/// whole file.
fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

pub fn read(path: impl AsRef<Path>) -> MediaPreferences {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Written whole and atomically -- a temporary file, fsync, rename -- on a
/// thread of its own, so a press never waits for the disk. A half-written
/// file read at start would be worse than the defaults.
pub fn write(path: PathBuf, prefs: MediaPreferences) {
    let _ = std::thread::Builder::new()
        .name("mediabox-tv-prefs".into())
        .spawn(move || {
            if let Err(error) = write_now(&path, &prefs) {
                eprintln!("mediabox-tv.prefs could not write {}: {error}", path.display());
            }
        });
}

fn write_now(path: &Path, prefs: &MediaPreferences) -> std::io::Result<()> {
    let text = serde_json::to_vec_pretty(prefs).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("tmp");
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&text)?;
        file.sync_all()
    })();
    match written {
        Ok(()) => std::fs::rename(&temporary, path),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are what the renderer drew before there was a choice:
    /// 4 % of the short side at scale 1, white, a 5 % black outline, nothing
    /// behind it, 5 % above the bottom.
    #[test]
    fn the_defaults_are_the_subtitle_as_it_was() {
        let style = CaptionStyle::default();
        assert_eq!(style.size.scale(), 1.0);
        assert_eq!(style.color.rgb(), 0xffffff);
        assert_eq!(style.edge.width(), 0.05);
        assert_eq!(style.background.argb() >> 24, 0);
        assert_eq!(style.position.lift(), 0.05);
        let prefs = MediaPreferences::default();
        assert_eq!(prefs.player, PlayerChoice::MediaBox);
        assert_eq!(prefs.resume, ResumeChoice::Ask);
    }

    #[test]
    fn a_file_round_trips_and_an_unknown_value_is_the_default() {
        let prefs = MediaPreferences {
            player: PlayerChoice::Kodi,
            resume: ResumeChoice::FromStart,
            caption: CaptionStyle { size: CaptionSize::Large, edge: CaptionEdge::Thick, ..Default::default() },
        };
        let text = serde_json::to_string(&prefs).unwrap();
        assert_eq!(serde_json::from_str::<MediaPreferences>(&text).unwrap(), prefs);

        // A newer build's value for one field leaves the others as written.
        let newer = r#"{"player": "vlc", "resume": "resume", "caption": {"size": "huge", "color": "yellow"}}"#;
        let read: MediaPreferences = serde_json::from_str(newer).unwrap();
        assert_eq!(read.player, PlayerChoice::MediaBox);
        assert_eq!(read.resume, ResumeChoice::Resume);
        assert_eq!(read.caption.size, CaptionSize::Normal);
        assert_eq!(read.caption.color, CaptionColor::Yellow);
        assert_eq!(serde_json::from_str::<MediaPreferences>("{}").unwrap(), MediaPreferences::default());
    }

    #[test]
    fn a_missing_or_broken_file_is_the_defaults() {
        let dir = std::env::temp_dir().join(format!("mediabox-tv-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("media-preferences.json");
        assert_eq!(read(&path), MediaPreferences::default());
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(read(&path), MediaPreferences::default());
        let prefs = MediaPreferences { player: PlayerChoice::Ask, ..Default::default() };
        write_now(&path, &prefs).unwrap();
        assert_eq!(read(&path), prefs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn where_a_film_starts() {
        assert_eq!(ResumeChoice::Ask.start(600), None);
        assert_eq!(ResumeChoice::Resume.start(600), Some(600));
        assert_eq!(ResumeChoice::FromStart.start(600), Some(0));
        // Nothing to resume is the start, whatever was chosen.
        assert_eq!(ResumeChoice::Ask.start(0), Some(0));
    }
}
