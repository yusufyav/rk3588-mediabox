//! The switch-on: the logo, the opening, the interface -- one picture handed
//! on to the next, at one mode.
//!
//! The kernel's console is off the television (`console=serial`,
//! mediabox-hdmi-prepare). Early in the boot, packaging/mediabox-boot-logo puts
//! this interface's logo on the wire at the mode chosen for the sink in
//! Settings (or its fastest), so the interface's first frame -- the same logo
//! at the same mode -- is no modeset. The logo stays while Quiet OS's scenes
//! are made under it, then goes as the opening begins; the opening ends on the
//! menu. Nothing after the logo changes the mode.

use std::io::Write;
use std::path::Path;

use crate::appearance::{Intro, Switch};

/// What this start shows before the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opening {
    /// The logo, first: a switch-on, not a return.
    pub logo: bool,
    /// And then the opening.
    pub intro: bool,
}

/// What a start shows. `adopted`: the display was kept lit across the last
/// stop -- a return from Kodi or the browser, never a switch-on. `last`: the
/// switch-on recorded by an earlier start.
pub fn decide(adopted: bool, last: Option<&Switch>, now: &Switch, intro: &Intro, reduced: bool, quiet: bool) -> Opening {
    let switched_on = switched_on(adopted, last, now);
    Opening {
        logo: switched_on,
        intro: switched_on && quiet && intro.plays(reduced, last, now),
    }
}

/// Whether a start is a switch-on: a boot not yet recorded, on a display
/// nobody kept lit.
pub fn switched_on(adopted: bool, last: Option<&Switch>, now: &Switch) -> bool {
    !adopted && last.is_none_or(|last| last.boot != now.boot)
}

/// This boot, this day, this build.
pub fn now() -> Switch {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let version = std::env::current_exe()
        .and_then(std::fs::metadata)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| format!("{}+{}", env!("CARGO_PKG_VERSION"), d.as_secs()))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    Switch { boot, day: local_day(), version }
}

/// Days since 1970 on the wall clock here.
fn local_day() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // SAFETY: localtime_r writes only the struct it is given.
    let offset = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = secs as libc::time_t;
        if libc::localtime_r(&t, &mut tm).is_null() { 0 } else { tm.tm_gmtoff }
    };
    (secs + offset).div_euclid(86_400)
}

pub fn read_last(path: &Path) -> Option<Switch> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Recorded before anything is shown, so that a crash and a restart in the
/// same boot do not show it again.
pub fn record(path: &Path, now: &Switch) {
    if let Ok(bytes) = serde_json::to_vec(now) {
        let _ = write_atomically(path, &bytes);
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.new",
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::IntroPolicy;

    fn on(boot: &str) -> Switch {
        Switch { boot: boot.into(), day: 1, version: "v".into() }
    }

    #[test]
    fn the_logo_is_for_a_switch_on_only() {
        let intro = Intro::default();
        let first = decide(false, None, &on("a"), &intro, false, true);
        assert_eq!(first, Opening { logo: true, intro: true });
        // A restart within the same boot: neither.
        let again = decide(false, Some(&on("a")), &on("a"), &intro, false, true);
        assert_eq!(again, Opening { logo: false, intro: false });
        // A return to a display kept lit: neither, even on a new boot record.
        let back = decide(true, Some(&on("a")), &on("b"), &intro, false, true);
        assert_eq!(back, Opening { logo: false, intro: false });
        // A new boot.
        let next = decide(false, Some(&on("a")), &on("b"), &intro, false, true);
        assert_eq!(next, Opening { logo: true, intro: true });
    }

    #[test]
    fn the_logo_stays_when_the_opening_does_not_play() {
        let off = Intro { enabled: false, ..Intro::default() };
        assert_eq!(decide(false, None, &on("a"), &off, false, true), Opening { logo: true, intro: false });
        assert_eq!(decide(false, None, &on("a"), &Intro::default(), true, true), Opening { logo: true, intro: false });
        // The classic face has no opening.
        assert_eq!(decide(false, None, &on("a"), &Intro::default(), false, false), Opening { logo: true, intro: false });
        let daily = Intro { policy: IntroPolicy::FirstOfDay, ..Intro::default() };
        assert_eq!(decide(false, Some(&on("a")), &on("b"), &daily, false, true), Opening { logo: true, intro: false });
    }

    #[test]
    fn the_switch_on_is_kept_and_read_back() {
        let dir = std::env::temp_dir().join(format!("mediabox-opening-{}", std::process::id()));
        let path = dir.join("opening-switch.json");
        record(&path, &on("x"));
        assert_eq!(read_last(&path), Some(on("x")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
