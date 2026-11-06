//! The first seconds of each stream epoch the receiver writes -- a channel's
//! opening -- kept as a file, for a measurement: a defect seen once at an
//! opening (2026-11-06, one key frame's second field missing in ~235
//! openings) can only be replayed from the bytes that made it. Only in a
//! diagnostic build (feature `opening-capture`) and only while the
//! timeline is on: `/run/mediabox-trace/opening-<epoch>-<ns>.ts`, the
//! output as the player read it, `SECONDS` of it, the newest `KEEP` kept.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SECONDS: Duration = Duration::from_secs(2);
const KEEP: usize = 20;

#[derive(Default)]
pub struct Capture {
    file: Option<(std::fs::File, Instant)>,
    /// The epoch whose opening is (or was) kept: each once.
    epoch: Option<u64>,
}

impl Capture {
    /// What was written to the player in stream epoch `epoch`: the first
    /// write of an epoch -- epoch 0, a channel's first, has no marker of
    /// its own -- starts its file, kept while the opening lasts.
    pub fn written(&mut self, epoch: u64, chunk: &[u8]) {
        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.start(epoch);
        }
        let Some((file, since)) = self.file.as_mut() else { return };
        if since.elapsed() > SECONDS || file.write_all(chunk).is_err() {
            self.file = None;
        }
    }

    fn start(&mut self, epoch: u64) {
        self.file = None;
        if !mediabox_core::timeline::enabled() {
            return;
        }
        let dir = trace_dir();
        let path = dir.join(format!("opening-{epoch}-{}.ts", mediabox_core::timeline::now_ns()));
        if let Ok(file) = std::fs::File::create(&path) {
            mediabox_core::timeline!("capture", "path={}", path.display());
            self.file = Some((file, Instant::now()));
            prune(&dir);
        }
    }

}

fn trace_dir() -> PathBuf {
    std::env::var_os("MEDIABOX_TRACE_DIR").map_or_else(|| PathBuf::from(mediabox_core::timeline::DIR), PathBuf::from)
}

/// The newest `KEEP` openings stay, the trace directory being tmpfs.
fn prune(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut kept: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("opening-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    kept.sort();
    let excess = kept.len().saturating_sub(KEEP);
    for (_, path) in kept.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opening_is_kept_for_its_first_seconds_and_the_newest_stay() {
        let dir = std::env::temp_dir().join(format!("mediabox-dvb-capture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for k in 0..(KEEP + 3) {
            std::fs::write(dir.join(format!("opening-{k}-0.ts")), b"x").unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        prune(&dir);
        let left = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(left, KEEP);
        assert!(!dir.join("opening-0-0.ts").exists(), "the oldest went");
        let mut capture = Capture { file: Some((std::fs::File::create(dir.join("now.ts")).unwrap(), Instant::now())), epoch: Some(0) };
        capture.written(0, b"abc");
        capture.file.as_mut().unwrap().1 = Instant::now() - SECONDS - Duration::from_millis(1);
        capture.written(0, b"def");
        assert!(capture.file.is_none(), "closed after its seconds");
        assert_eq!(std::fs::read(dir.join("now.ts")).unwrap(), b"abc");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
