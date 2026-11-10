//! GStreamer's own debug log, kept off the eMMC.
//!
//! GStreamer writes its log from the thread that logs, synchronously: with
//! `GST_DEBUG_FILE` on the eMMC the streaming thread stopped 75-120 ms at a
//! time in `ext4_da_write_begin -> __folio_lock -> io_schedule`, the AC-3
//! lost bursts and the picture dropped frames (FAZ 3E.8); off, or on tmpfs,
//! none of it. So, before `gst::init` reads them:
//!
//! - In production (no `MEDIABOX_LIVE_GST_DEBUG`) GStreamer logs nothing:
//!   `GST_DEBUG`, `GST_DEBUG_FILE` and `GST_DEBUG_DUMP_DOT_DIR` left in the
//!   environment (`systemctl set-environment`, a shell) are removed.
//! - Diagnosis is asked for by name: `MEDIABOX_LIVE_GST_DEBUG=<GST_DEBUG
//!   spec>`, its file `MEDIABOX_LIVE_GST_DEBUG_FILE` (or `GST_DEBUG_FILE`).
//!   The file is only ever under `/run/` (tmpfs); any other path, stderr
//!   or stdout is turned to `DEFAULT_FILE`. It is bounded (`bound`): past
//!   `MAX_BYTES` GStreamer's default log function is taken away and the
//!   file grows no more.
//!
//! Each start that changes anything says so in one line.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub const DIAG: &str = "MEDIABOX_LIVE_GST_DEBUG";
pub const DIAG_FILE: &str = "MEDIABOX_LIVE_GST_DEBUG_FILE";
pub const DEFAULT_FILE: &str = "/run/mediabox-live/gst-debug.log";
/// The most the diagnosis log is let grow to.
pub const MAX_BYTES: u64 = 64 << 20;
/// GStreamer's variables that write anywhere, cleared unless diagnosis asks.
const OUTSIDE: [&str; 3] = ["GST_DEBUG", "GST_DEBUG_FILE", "GST_DEBUG_DUMP_DOT_DIR"];

/// What the environment becomes before `gst::init`.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub set: Vec<(&'static str, String)>,
    pub unset: Vec<&'static str>,
    /// The diagnosis log, bounded by `bound`.
    pub file: Option<PathBuf>,
    pub warning: Option<String>,
}

/// A file under `/run/` (tmpfs), named without `.` or `..`.
pub fn on_tmpfs(path: &str) -> bool {
    let mut parts = Path::new(path).components();
    parts.next() == Some(Component::RootDir)
        && parts.next() == Some(Component::Normal("run".as_ref()))
        && {
            let rest: Vec<_> = parts.collect();
            !rest.is_empty() && rest.iter().all(|part| matches!(part, Component::Normal(_)))
        }
}

pub fn plan(var: impl Fn(&str) -> Option<String>) -> Plan {
    let present = |name: &str| var(name).is_some();
    let Some(spec) = var(DIAG).filter(|spec| !spec.trim().is_empty()) else {
        let unset: Vec<_> = OUTSIDE.iter().copied().filter(|name| present(name)).collect();
        let warning = (!unset.is_empty()).then(|| {
            format!("mediabox-live: dış ortamdan gelen {} yok sayıldı; GStreamer günlüğü üretimde kapalı (tanı: {DIAG}=<düzey>)", unset.join(", "))
        });
        return Plan { unset, warning, ..Plan::default() };
    };
    let wanted = var(DIAG_FILE).or_else(|| var("GST_DEBUG_FILE")).filter(|file| !file.is_empty());
    let (file, turned) = match wanted {
        Some(file) if on_tmpfs(&file) => (file, None),
        Some(file) => (DEFAULT_FILE.to_string(), Some(file)),
        None => (DEFAULT_FILE.to_string(), None),
    };
    let mut warning = format!("mediabox-live: TANI AÇIK: GST_DEBUG={spec}, günlük {file} (tmpfs, en çok {} MiB)", MAX_BYTES >> 20);
    if let Some(turned) = turned {
        warning.push_str(&format!("; istenen {turned} tmpfs değil, kullanılmadı"));
    }
    Plan {
        set: vec![("GST_DEBUG", spec), ("GST_DEBUG_FILE", file.clone())],
        unset: OUTSIDE[2..].iter().copied().filter(|name| present(name)).collect(),
        file: Some(PathBuf::from(file)),
        warning: Some(warning),
    }
}

/// The plan put in force. Called first thing in `main`, before any thread.
pub fn apply(plan: &Plan) {
    for name in &plan.unset {
        // SAFETY: before gst::init and before this process starts a thread.
        unsafe { std::env::remove_var(name) };
    }
    for (name, value) in &plan.set {
        // SAFETY: as above.
        unsafe { std::env::set_var(name, value) };
    }
    if let Some(parent) = plan.file.as_deref().and_then(Path::parent) {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Some(warning) = &plan.warning {
        eprintln!("{warning}");
    }
}

/// Watches the diagnosis log from a thread of its own (never a media
/// thread): once it is past `max`, `stop` is called, once.
pub fn bound(file: PathBuf, max: u64, every: Duration, stop: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(every);
            if std::fs::metadata(&file).is_ok_and(|meta| meta.len() > max) {
                stop();
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn plan_of(vars: &[(&str, &str)]) -> Plan {
        let vars: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        plan(|name| vars.get(name).cloned())
    }

    #[test]
    fn production_lets_no_outside_debug_through() {
        let plan = plan_of(&[("GST_DEBUG", "*:5"), ("GST_DEBUG_FILE", "/var/tmp/gst.log")]);
        assert_eq!(plan.unset, vec!["GST_DEBUG", "GST_DEBUG_FILE"]);
        assert!(plan.set.is_empty());
        assert_eq!(plan.file, None);
        assert!(plan.warning.unwrap().contains("GST_DEBUG, GST_DEBUG_FILE yok sayıldı"));
    }

    #[test]
    fn production_without_debug_changes_nothing() {
        assert_eq!(plan_of(&[("PATH", "/usr/bin")]), Plan::default());
        // An empty switch is not diagnosis.
        assert_eq!(plan_of(&[(DIAG, " ")]), Plan::default());
    }

    #[test]
    fn diagnosis_turns_a_disk_path_to_tmpfs() {
        for disk in ["/var/tmp/gst.log", "/home/root/gst.log", "/run/../var/gst.log", "gst.log", "-", "/run"] {
            let plan = plan_of(&[(DIAG, "audiobasesink:5"), (DIAG_FILE, disk)]);
            assert_eq!(plan.set, vec![("GST_DEBUG", "audiobasesink:5".into()), ("GST_DEBUG_FILE", DEFAULT_FILE.into())], "{disk}");
            assert!(plan.warning.unwrap().contains(&format!("istenen {disk} tmpfs değil")));
        }
        // GST_DEBUG_FILE from outside is held to the same rule.
        let plan = plan_of(&[(DIAG, "*:3"), ("GST_DEBUG_FILE", "/var/tmp/gst.log")]);
        assert_eq!(plan.file.as_deref(), Some(Path::new(DEFAULT_FILE)));
    }

    #[test]
    fn diagnosis_keeps_a_run_path() {
        let plan = plan_of(&[(DIAG, "*:3"), (DIAG_FILE, "/run/mediabox-diag/gst.log"), ("GST_DEBUG", "*:9"), ("GST_DEBUG_DUMP_DOT_DIR", "/var/tmp")]);
        assert_eq!(plan.set, vec![("GST_DEBUG", "*:3".into()), ("GST_DEBUG_FILE", "/run/mediabox-diag/gst.log".into())]);
        assert_eq!(plan.unset, vec!["GST_DEBUG_DUMP_DOT_DIR"]);
        let warning = plan.warning.unwrap();
        assert!(warning.contains("TANI AÇIK") && !warning.contains("kullanılmadı"), "{warning}");
        // No file asked for: the default, not stderr.
        assert_eq!(plan_of(&[(DIAG, "*:3")]).file.as_deref(), Some(Path::new(DEFAULT_FILE)));
    }

    #[test]
    fn the_log_is_stopped_past_its_bound() {
        let file = std::env::temp_dir().join(format!("mediabox-live-gst-bound-{}", std::process::id()));
        std::fs::write(&file, [0u8; 8]).unwrap();
        let (told, stopped) = std::sync::mpsc::channel();
        let watcher = bound(file.clone(), 16, Duration::from_millis(5), move || told.send(()).unwrap());
        assert!(stopped.recv_timeout(Duration::from_millis(100)).is_err(), "within its bound");
        std::fs::write(&file, [0u8; 17]).unwrap();
        stopped.recv_timeout(Duration::from_secs(2)).expect("stopped past its bound");
        watcher.join().unwrap();
        let _ = std::fs::remove_file(&file);
    }
}
