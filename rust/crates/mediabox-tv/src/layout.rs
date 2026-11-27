//! Which of the two faces the home screen and the system settings wear.
//!
//! One application, one state, one set of screens' logic: the layout only
//! chooses which Slint component draws the home screen and the settings
//! (`ui/home.slint` and `ui/settings.slint`, or `ui/quiet/`). Every
//! focus, pointer zone, row and action is the same under both.
//!
//! The file is the interface's own preferences (`appearance.rs` keeps the
//! rest of it: Quiet OS's motion and the opening); this reads the face.
//!
//! Kept in a file of its own beside the session file rather than in
//! `media-preferences.json`, which is "Filmler ve Diziler"'s own: the face of
//! the system is not that application's business. A box that never chose, a
//! missing file, a broken one, or a value this build does not know, is the
//! classic face -- what every box drew before there was a choice.

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

/// The face kept, read from the whole file (`appearance::read`).
#[cfg(test)]
pub fn read(path: impl AsRef<std::path::Path>) -> Layout {
    crate::appearance::read(path).layout
}

/// Written only as part of the whole file (`appearance::write`): the face
/// alone would drop the motion and the opening kept beside it.
#[cfg(test)]
fn write(path: std::path::PathBuf, layout: Layout) {
    let kept = crate::appearance::read(&path);
    crate::appearance::write(path, crate::appearance::Interface { layout, ..kept });
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
            &crate::appearance::Interface {
                layout: Layout::Quiet,
                ..Default::default()
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
