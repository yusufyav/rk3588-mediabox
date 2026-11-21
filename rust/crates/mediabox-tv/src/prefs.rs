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

/// Written whole and atomically -- a temporary file, fsync, rename -- off
/// the UI event loop. A single writer processes changes in the order they were
/// requested, so rapid layout switches cannot race over the shared .tmp path
/// or leave an older choice as the last persisted one.
pub fn write(path: PathBuf, prefs: MediaPreferences) {
    write_in_background(path, prefs);
}

type WriteJob = Box<dyn FnOnce() + Send>;

/// Shared by media preferences and the system's interface preferences.
/// All sends originate from the UI event loop in production, where channel
/// order is the exact order the user made the changes. No disk I/O runs there.
fn writer() -> &'static std::sync::mpsc::Sender<WriteJob> {
    use std::sync::{mpsc, OnceLock};
    static WRITER: OnceLock<mpsc::Sender<WriteJob>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<WriteJob>();
        if let Err(error) = std::thread::Builder::new()
            .name("mediabox-tv-prefs".into())
            .spawn(move || {
                for write in receiver {
                    write();
                }
            })
        {
            eprintln!("mediabox-tv.prefs could not start preference writer: {error}");
        }
        sender
    })
}

/// The same ordered atomic writer for any small preference file this interface
/// keeps for itself (`layout.rs` too). Values are serialized only by the
/// writer, avoiding disk and serialization latency on a remote-control press.
pub(crate) fn write_in_background<T: Serialize + Send + 'static>(path: PathBuf, value: T) {
    let job: WriteJob = Box::new(move || {
        if let Err(error) = write_now(&path, &value) {
            eprintln!("mediabox-tv.prefs could not write {}: {error}", path.display());
        }
    });
    if writer().send(job).is_err() {
        eprintln!("mediabox-tv.prefs preference writer has stopped");
    }
}

/// Returns once every write queued before it is on the disk: the writer
/// runs jobs in order, so a job that only answers is behind all of them.
#[cfg(test)]
pub(crate) fn flush() {
    let (done, wait) = std::sync::mpsc::channel::<()>();
    let job: WriteJob = Box::new(move || {
        let _ = done.send(());
    });
    if writer().send(job).is_ok() {
        let _ = wait.recv_timeout(std::time::Duration::from_secs(10));
    }
}

pub(crate) fn write_now<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let text = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
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

    /// A burst of user choices must leave the newest one on disk. Each
    /// request uses the same target and temporary path: parallel writers
    /// previously raced at this point.
    #[test]
    fn rapid_preferences_keep_the_last_choice() {
        let dir = std::env::temp_dir().join(format!(
            "mediabox-tv-ordered-writes-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("interface-preferences.json");
        for choice in 0..24 {
            write_in_background(path.clone(), choice);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::fs::read_to_string(&path).ok().as_deref() != Some("23") {
            assert!(
                std::time::Instant::now() < deadline,
                "the last queued preference was never persisted"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!path.with_extension("tmp").exists(), "no unfinished temp file");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Media and interface choices pressed faster than the disk, mixed:
    /// each file ends as the last one pressed for it, and neither file's
    /// writes touch the other's.
    #[test]
    fn rapid_choices_keep_the_last_one_for_each_file() {
        let dir = std::env::temp_dir().join(format!("mediabox-tv-prefs-burst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("media-preferences.json");
        let other = dir.join("interface-preferences.json");
        let player = |kodi: bool| if kodi { PlayerChoice::Kodi } else { PlayerChoice::MediaBox };
        for round in 0..20 {
            for i in 0..50u32 {
                write(media.clone(), MediaPreferences { player: player(i % 2 == 0), ..Default::default() });
                write_in_background(other.clone(), serde_json::json!({ "n": i, "round": round }));
            }
            let last = round % 2 == 0;
            write(media.clone(), MediaPreferences { player: player(last), ..Default::default() });
            flush();
            assert_eq!(read(&media).player, player(last), "round {round}");
            let kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&other).unwrap()).unwrap();
            assert_eq!(kept, serde_json::json!({ "n": 49, "round": round }));
            assert!(!media.with_extension("tmp").exists() && !other.with_extension("tmp").exists());
        }
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
