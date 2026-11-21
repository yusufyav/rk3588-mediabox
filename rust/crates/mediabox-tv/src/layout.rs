//! Which of the two faces the home screen and the system settings wear.
//!
//! One application, one state, one set of screens' logic: the layout only
//! chooses which Slint component draws the home screen and the settings
//! (`ui/home.slint` and `ui/settings.slint`, or `ui/quiet.slint`). Every
//! focus, pointer zone, row and action is the same under both.
//!
//! Kept in a file of its own beside the session file rather than in
//! `media-preferences.json`, which is "Filmler ve Diziler"'s own: the face of
//! the system is not that application's business. A box that never chose, a
//! missing file, a broken one, or a value this build does not know, is the
//! classic face -- what every box drew before there was a choice.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Classic,
    Quiet,
}

impl Layout {
    pub const ALL: [Layout; 2] = [Layout::Classic, Layout::Quiet];

    pub fn label(self) -> &'static str {
        match self {
            Layout::Classic => "Klasik MediaBox",
            Layout::Quiet => "Quiet OS",
        }
    }

    /// What `MediaBoxWindow.layout` is told.
    pub fn name(self) -> &'static str {
        match self {
            Layout::Classic => "classic",
            Layout::Quiet => "quiet",
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    #[serde(default, deserialize_with = "or_default")]
    layout: Layout,
}

fn or_default<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Layout, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

pub fn read(path: impl AsRef<Path>) -> Layout {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Kept>(&text).ok())
        .map(|kept| kept.layout)
        .unwrap_or_default()
}

/// Written whole and atomically, off the event loop, as the media
/// preferences are (`prefs::write_in_background`).
pub fn write(path: PathBuf, layout: Layout) {
    crate::prefs::write_in_background(path, Kept { layout });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_that_never_chose_or_cannot_read_its_choice_is_classic() {
        let dir = std::env::temp_dir().join(format!("mediabox-tv-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("interface-preferences.json");
        assert_eq!(read(&path), Layout::Classic, "no file");
        for broken in [
            "{not json",
            "{\"layout\":\"neon\"}",
            "{\"layout\":3}",
            "[]",
            "",
        ] {
            std::fs::write(&path, broken).unwrap();
            assert_eq!(read(&path), Layout::Classic, "{broken}");
        }
        crate::prefs::write_now(
            &path,
            &Kept {
                layout: Layout::Quiet,
            },
        )
        .unwrap();
        assert_eq!(read(&path), Layout::Quiet);
        std::fs::write(&path, "{\"layout\":\"quiet\",\"later\":1}").unwrap();
        assert_eq!(read(&path), Layout::Quiet, "a newer build's extra field");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Klasik -> Quiet -> Klasik -> Quiet pressed faster than the disk is kept
    /// as Quiet; and the other way round as Klasik.
    #[test]
    fn rapid_switching_keeps_the_last_layout() {
        let dir =
            std::env::temp_dir().join(format!("mediabox-tv-layout-burst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("interface-preferences.json");
        for last in [Layout::Quiet, Layout::Classic, Layout::Quiet] {
            for _ in 0..25 {
                for layout in [
                    Layout::Classic,
                    Layout::Quiet,
                    Layout::Classic,
                    Layout::Quiet,
                ] {
                    write(path.clone(), layout);
                }
            }
            write(
                path.clone(),
                if last == Layout::Quiet {
                    Layout::Classic
                } else {
                    Layout::Quiet
                },
            );
            write(path.clone(), last);
            crate::prefs::flush();
            assert_eq!(read(&path), last);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
