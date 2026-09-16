//! Which product owns the television, across a reboot.
//!
//! This board carries two products. rk3588-screenbridge is a capture appliance
//! and MediaBox is a media appliance, and they want the same DRM master: only
//! one of them can be on the panel at a time. Nothing recorded which one that
//! should be, so the answer was made accidentally, by this daemon claiming the
//! display at every start — a Plus that had been a ScreenBridge box for months
//! came up as a MediaBox box the moment MediaBox was installed on it, at every
//! boot after that, with no-one having chosen it.
//!
//! So the choice is written down, in one file, and this is the only place that
//! reads or writes it.
//!
//! The default is ScreenBridge and that is the whole point of the default:
//! installing a second product must not quietly take a working appliance's role
//! away from it. A board becomes a MediaBox board when somebody says so, and
//! then stays one.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The one source of truth. Read at start, written whenever the display
/// changes hands.
pub const OWNER_FILE: &str = "/var/lib/mediabox/display-owner";

/// The other product's daemon. Named here and nowhere else.
pub const SCREENBRIDGE_UNIT: &str = "screenbridge-daemon.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayOwner {
    MediaBox,
    ScreenBridge,
}

impl DisplayOwner {
    pub fn as_str(self) -> &'static str {
        match self {
            DisplayOwner::MediaBox => "mediabox",
            DisplayOwner::ScreenBridge => "screenbridge",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "mediabox" => Some(DisplayOwner::MediaBox),
            "screenbridge" => Some(DisplayOwner::ScreenBridge),
            _ => None,
        }
    }
}

fn path() -> PathBuf {
    PathBuf::from(
        std::env::var("MEDIABOX_DISPLAY_OWNER_FILE").unwrap_or_else(|_| OWNER_FILE.into()),
    )
}

/// What the file says, or ScreenBridge.
///
/// Every way of not being a recognised word is the same answer: no file, an
/// empty file, half a file left by a power cut, a word written by a newer
/// version than this one. Guessing in the other direction would start a second
/// display owner beside a running one, which is the failure this is here to
/// prevent; refusing to start MediaBox's shell is recoverable with one command.
pub fn read() -> DisplayOwner {
    read_from(&path())
}

fn read_from(file: &Path) -> DisplayOwner {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|value| DisplayOwner::parse(&value))
        .unwrap_or(DisplayOwner::ScreenBridge)
}

/// Record it, atomically.
///
/// Written to a temporary name in the same directory and renamed over the top,
/// so a reader never sees half a word and a power cut leaves either the old
/// answer or the new one. The mode is deliberate: the file decides which
/// product owns the hardware at boot, so it is root's to write and everyone
/// else's to read.
pub fn write(owner: DisplayOwner) -> std::io::Result<()> {
    write_to(&path(), owner)
}

fn write_to(file: &Path, owner: DisplayOwner) -> std::io::Result<()> {
    if read_from(file) == owner && file.exists() {
        return Ok(());
    }
    let directory = file.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(
        ".{}.new",
        file.file_name().and_then(|n| n.to_str()).unwrap_or("owner")
    ));
    {
        let mut handle = std::fs::File::create(&temporary)?;
        handle.write_all(owner.as_str().as_bytes())?;
        handle.write_all(b"\n")?;
        // The mode goes on before the sync rather than after it, so the one
        // sync covers it. Set afterwards it is a metadata change nothing has
        // committed, and a file that came back from a power cut carrying the
        // umask's mode instead of this one would be a file this product had
        // never actually written the way it says it writes it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            handle.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        }
        handle.sync_all()?;
    }
    match std::fs::rename(&temporary, file) {
        Ok(()) => sync_directory(directory),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// Commit the rename, not just what was renamed.
///
/// `sync_all` on the temporary file makes its contents durable. It says
/// nothing about the directory entry that the rename then created: the name
/// `display-owner` pointing at that inode lives in the parent directory, and
/// until the parent is synced a power cut can take the rename away and leave
/// the previous answer — or, on a first write, no answer at all — while the
/// content it was pointing at survives perfectly. Which product this board
/// comes up as is exactly the thing that must not be lost by pulling the plug,
/// so the directory is synced too.
///
/// The error is passed on rather than swallowed. By this point the new value
/// is already visible to any reader, so nothing is broken by reporting it; a
/// directory fsync failing on the appliance's own ext4 root is a real fault
/// and saying "recorded" about it would be a claim this cannot make.
fn sync_directory(directory: &Path) -> std::io::Result<()> {
    std::fs::File::open(directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_dir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mediabox-owner-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn an_absent_file_is_screenbridge() {
        let file = temporary_dir().join("absent");
        let _ = std::fs::remove_file(&file);
        assert_eq!(read_from(&file), DisplayOwner::ScreenBridge);
    }

    #[test]
    fn a_word_nobody_recognises_is_screenbridge() {
        let file = temporary_dir().join("unknown");
        for content in ["", "   ", "kodi", "MediaBox\n", "mediab", "\0mediabox"] {
            std::fs::write(&file, content).unwrap();
            assert_eq!(
                read_from(&file),
                DisplayOwner::ScreenBridge,
                "content {content:?} should not be read as an owner"
            );
        }
    }

    #[test]
    fn both_words_round_trip() {
        let file = temporary_dir().join("round-trip");
        for owner in [DisplayOwner::MediaBox, DisplayOwner::ScreenBridge] {
            write_to(&file, owner).unwrap();
            assert_eq!(read_from(&file), owner);
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "screenbridge\n");
    }

    #[test]
    fn trailing_whitespace_is_not_a_different_answer() {
        let file = temporary_dir().join("whitespace");
        std::fs::write(&file, "  mediabox  \n\n").unwrap();
        assert_eq!(read_from(&file), DisplayOwner::MediaBox);
    }

    #[test]
    fn writing_leaves_no_temporary_behind() {
        let directory = temporary_dir().join("clean");
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("display-owner");
        write_to(&file, DisplayOwner::MediaBox).unwrap();
        let strays: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "display-owner")
            .collect();
        assert!(strays.is_empty(), "left behind: {strays:?}");
    }

    #[test]
    fn a_directory_can_be_synced_on_this_platform() {
        // The fifth step of the write is an fsync of a directory handle, which
        // is well defined on Linux and not everywhere. If that ever stops
        // working the writes stop being durable silently, so it is asserted
        // rather than assumed.
        let directory = temporary_dir();
        sync_directory(&directory).expect("the parent directory could not be synced");
    }

    #[test]
    fn the_file_is_not_world_writable() {
        use std::os::unix::fs::PermissionsExt;
        let file = temporary_dir().join("modes");
        write_to(&file, DisplayOwner::MediaBox).unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o022, 0, "mode {mode:o} is writable by others");
    }
}
