//! Where the machine's own description of itself is read from.
//!
//! Every path this crate looks at goes through here, and the reason is the
//! tests. Discovery that opens `/sys` directly can only be tested on the board
//! it was written for, which is exactly the failure this crate exists to stop.
//! Pointed at a directory of captured sysfs instead, the same code answers
//! questions about a board that is not present — a Plus, a board whose cards
//! enumerated in a different order, a board with no display attached at all.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Roots {
    /// `/sys`
    pub sys: PathBuf,
    /// `/dev`
    pub dev: PathBuf,
    /// Where the appliance remembers things across boots. The daemon's own
    /// `StateDirectory`; a remembered output choice lives beside the
    /// indicator-light mode rather than in a store of this crate's own.
    pub state: PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Self::system()
    }
}

impl Roots {
    /// The running machine.
    pub fn system() -> Self {
        Self {
            sys: PathBuf::from("/sys"),
            dev: PathBuf::from("/dev"),
            state: PathBuf::from("/var/lib/mediabox"),
        }
    }

    /// A captured or synthetic tree: `<root>/sys` and `<root>/dev`.
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            sys: root.join("sys"),
            dev: root.join("dev"),
            state: root.join("var/lib/mediabox"),
        }
    }

    /// What this crate reads as the environment says, so a diagnostic run can
    /// be pointed at a capture without rebuilding anything.
    pub fn from_env() -> Self {
        match std::env::var_os("MEDIABOX_PLATFORM_ROOT") {
            Some(root) if !root.is_empty() => Self::under(root),
            _ => Self::system(),
        }
    }

    pub fn sys(&self, rest: &str) -> PathBuf {
        self.sys.join(rest)
    }

    pub fn state(&self, rest: &str) -> PathBuf {
        self.state.join(rest)
    }

    /// A device node's path as the rest of the system will see it.
    ///
    /// Under a capture this is the capture's own `dev/`, so a fixture can carry
    /// device nodes that do not exist on the machine running the test; on the
    /// appliance it is `/dev`, which is what a consumer needs to open.
    pub fn dev(&self, rest: &str) -> PathBuf {
        self.dev.join(rest)
    }
}

/// `read_to_string` with the trailing newline off and errors flattened to
/// `None`. Every one of these files is optional on some board or other, and a
/// missing one is an answer rather than a fault.
pub fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim_end_matches(['\n', '\0']).trim().to_string())
        .filter(|text| !text.is_empty())
}

/// The last component of a symlink's target, which is how sysfs names the
/// driver or device something is bound to.
pub fn link_basename(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_link(path.as_ref()).ok().and_then(|target| {
        target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
}

/// Children of a directory, in a stable order. Directory order is not
/// meaningful and differs between filesystems; every listing this crate makes
/// decisions from is sorted.
pub fn sorted_children(dir: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    entries
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
