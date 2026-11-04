//! The raw transport stream around a continuity error on the service played,
//! kept for diagnosis -- and only when asked for (`--evidence-dir`). Off,
//! there is no ring, and a transfer is not copied, allocated for or written
//! anywhere.
//!
//! On, the last `AROUND` of transfers live in one ring allocated once
//! (`RING_BYTES`); an error takes what the ring holds and gathers as long
//! again after it, then hands both to a writer thread. The directory is
//! shared by every process that writes to it: under its lock, the oldest
//! records go first beyond `DIR_FILES` or `DIR_BYTES`, a record is written
//! to temporaries and renamed into place (the `.txt` last: a record whose
//! note is there is whole), and temporaries left by a writer that died are
//! removed.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Before the error, and as long after it.
pub const AROUND: Duration = Duration::from_secs(2);
/// The ring: 2 s of the fastest transponder measured (12458 V, ~62 Mbit/s,
/// 15.5 MB) with room to spare.
pub const RING_BYTES: usize = 24 << 20;
/// One record's stream at most: the ring and as much again.
const RECORD_BYTES: usize = 2 * RING_BYTES;
/// Transfers the ring can account for: far more than `RING_BYTES` holds of
/// the receiver's transfers (tens of KiB each).
const MARKS: usize = 16_384;
/// A session's records.
pub const SESSION_FILES: u32 = 4;
/// The directory's, across sessions and processes.
pub const DIR_FILES: usize = 16;
pub const DIR_BYTES: u64 = 256 << 20;

pub struct Capture {
    dir: PathBuf,
    ring: Box<[u8]>,
    /// Bytes ever put in the ring: the next byte's place is `total % len`.
    total: u64,
    /// Each transfer's arrival and where in `total` it starts.
    marks: VecDeque<(Instant, u64)>,
    gathering: Option<Gathering>,
    session_files: u32,
    writing: Option<std::thread::JoinHandle<std::io::Result<Stored>>>,
}

struct Gathering {
    stem: String,
    note: String,
    data: Vec<u8>,
    due: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub path: PathBuf,
    pub bytes: u64,
    /// Older records removed for the directory's bounds.
    pub removed: usize,
}

impl Capture {
    pub fn new(dir: PathBuf) -> Self {
        Self::with_ring(dir, RING_BYTES)
    }

    fn with_ring(dir: PathBuf, bytes: usize) -> Self {
        Self {
            dir,
            ring: vec![0u8; bytes].into_boxed_slice(),
            total: 0,
            marks: VecDeque::with_capacity(MARKS),
            gathering: None,
            session_files: 0,
            writing: None,
        }
    }

    /// A transfer as it came. Returns what a record that finished writing
    /// came to, once.
    pub fn push(&mut self, chunk: &[u8], now: Instant) -> Option<std::io::Result<Stored>> {
        let len = self.ring.len();
        let tail = &chunk[chunk.len().saturating_sub(len)..];
        let start = (self.total % len as u64) as usize;
        let first = tail.len().min(len - start);
        self.ring[start..start + first].copy_from_slice(&tail[..first]);
        self.ring[..tail.len() - first].copy_from_slice(&tail[first..]);
        if self.marks.len() == MARKS {
            self.marks.pop_front();
        }
        self.marks
            .push_back((now, self.total + (chunk.len() - tail.len()) as u64));
        self.total += chunk.len() as u64;
        let oldest = self.total.saturating_sub(len as u64);
        while let Some(&(at, offset)) = self.marks.front()
            && (now.duration_since(at) > AROUND || offset < oldest)
        {
            self.marks.pop_front();
        }
        if let Some(gathering) = self.gathering.as_mut() {
            let room = RECORD_BYTES.saturating_sub(gathering.data.len());
            gathering
                .data
                .extend_from_slice(&chunk[..chunk.len().min(room)]);
            if now >= gathering.due {
                self.store(None);
            }
        }
        self.finished()
    }

    /// An error: gather around it, unless a record is being gathered or the
    /// session has had its records. `stem` names the record.
    pub fn trigger(&mut self, stem: String, note: String, now: Instant) -> bool {
        if self.gathering.is_some() || self.session_files >= SESSION_FILES {
            return false;
        }
        let from = self.marks.front().map_or(self.total, |&(_, offset)| offset);
        let held = (self.total - from) as usize;
        let mut data = Vec::with_capacity(held + held.max(1 << 20));
        let len = self.ring.len();
        let start = (from % len as u64) as usize;
        let first = held.min(len - start);
        data.extend_from_slice(&self.ring[start..start + first]);
        data.extend_from_slice(&self.ring[..held - first]);
        self.session_files += 1;
        self.gathering = Some(Gathering {
            stem,
            note,
            data,
            due: now + AROUND,
        });
        true
    }

    pub fn gathering(&self) -> bool {
        self.gathering.is_some()
    }

    /// The stream ends (or another starts): what is being gathered is
    /// written as it is, saying why it is short; the ring is of the stream
    /// that ended and is forgotten.
    pub fn cut(&mut self, why: &str) {
        self.store(Some(why));
        self.marks.clear();
    }

    /// The session ends: everything written before it returns -- no record
    /// is left half written.
    pub fn finish(&mut self) -> Option<std::io::Result<Stored>> {
        self.store(Some(
            "the stream ended before the time after the error was in",
        ));
        self.writing.take().map(join)
    }

    fn store(&mut self, cut: Option<&str>) {
        let Some(mut gathering) = self.gathering.take() else {
            return;
        };
        if let Some(why) = cut {
            gathering.note.push_str(&format!("\n(cut short: {why})"));
        }
        // One writer at a time: the one before is waited for.
        let before = self.writing.take().map(join);
        let dir = self.dir.clone();
        self.writing = Some(std::thread::spawn(move || {
            if let Some(Err(error)) = before {
                // Said by the record after it: it is not lost silently.
                gathering
                    .note
                    .push_str(&format!("\n(the record before failed: {error})"));
            }
            store(&dir, &gathering.stem, &gathering.data, &gathering.note)
        }));
    }

    fn finished(&mut self) -> Option<std::io::Result<Stored>> {
        if self.writing.as_ref().is_some_and(|w| w.is_finished()) {
            return self.writing.take().map(join);
        }
        None
    }
}

fn join(handle: std::thread::JoinHandle<std::io::Result<Stored>>) -> std::io::Result<Stored> {
    handle
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("the evidence writer panicked")))
}

/// One record into `dir`, within its bounds, under its lock.
fn store(dir: &Path, stem: &str, data: &[u8], note: &str) -> std::io::Result<Stored> {
    std::fs::create_dir_all(dir)?;
    let lock = std::fs::File::create(dir.join(".lock"))?;
    lock.lock()?;
    let new = (data.len() + note.len() + 1) as u64;
    if new > DIR_BYTES {
        return Err(std::io::Error::other(format!(
            "{new} bytes exceed the directory's {DIR_BYTES}"
        )));
    }
    // Records by stem (named by time: oldest first) and their bytes;
    // temporaries are a dead writer's: only the lock's holder writes.
    let mut records: std::collections::BTreeMap<String, u64> = Default::default();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".tmp") {
            std::fs::remove_file(entry.path())?;
            continue;
        }
        let Some(record) = name
            .strip_suffix(".ts")
            .or_else(|| name.strip_suffix(".txt"))
        else {
            continue;
        };
        *records.entry(record.to_string()).or_default() += entry.metadata()?.len();
    }
    let mut bytes: u64 = records.values().sum();
    let mut removed = 0;
    while let Some((oldest, size)) = records.first_key_value().map(|(k, v)| (k.clone(), *v))
        && (records.len() + 1 > DIR_FILES || bytes + new > DIR_BYTES)
    {
        for extension in ["ts", "txt"] {
            match std::fs::remove_file(dir.join(format!("{oldest}.{extension}"))) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        records.remove(&oldest);
        bytes -= size;
        removed += 1;
    }
    let path = dir.join(format!("{stem}.ts"));
    let place = |extension: &str, content: &[u8]| -> std::io::Result<()> {
        let temporary = dir.join(format!("{stem}.{extension}.tmp"));
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
        std::fs::rename(&temporary, dir.join(format!("{stem}.{extension}")))
    };
    place("ts", data)?;
    place("txt", format!("{note}\n").as_bytes())?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(Stored {
        path,
        bytes: data.len() as u64,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mediabox-dvb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_ring_keeps_the_last_seconds_without_growing() {
        let mut capture = Capture::with_ring(dir("ring"), 1000);
        let t0 = Instant::now();
        let ring = capture.ring.as_ptr();
        // Transfers of 300 bytes, numbered: the ring wraps.
        for n in 0..10u8 {
            capture.push(&[n; 300], t0);
        }
        assert_eq!(capture.ring.as_ptr(), ring, "allocated once");
        assert!(capture.marks.capacity() >= MARKS);
        assert!(capture.trigger("r".into(), "note".into(), t0));
        let data = &capture.gathering.as_ref().unwrap().data;
        // The ring's 1000 bytes reach back into transfer 6; whole transfers
        // from 7 on are what is kept, in order.
        assert_eq!(data.len(), 900);
        assert!(data[..300].iter().all(|&b| b == 7) && data[600..].iter().all(|&b| b == 9));
        // Older than AROUND: gone from what an error takes.
        let mut capture = Capture::with_ring(dir("ring2"), 1000);
        capture.push(&[1; 100], t0);
        capture.push(&[2; 100], t0 + AROUND + Duration::from_millis(1));
        capture.trigger(
            "r".into(),
            "n".into(),
            t0 + AROUND + Duration::from_millis(1),
        );
        assert_eq!(capture.gathering.as_ref().unwrap().data, vec![2; 100]);
    }

    #[test]
    fn a_record_is_written_whole_and_the_directory_stays_in_bounds() {
        let dir = dir("records");
        std::fs::create_dir_all(&dir).unwrap();
        // A dead writer's temporary, and records over the bound.
        std::fs::write(dir.join("selected-cc-0000000000001-x.ts.tmp"), b"half").unwrap();
        for n in 0..DIR_FILES {
            std::fs::write(dir.join(format!("selected-cc-{n:013}-x.ts")), b"old").unwrap();
            std::fs::write(dir.join(format!("selected-cc-{n:013}-x.txt")), b"old").unwrap();
        }
        let stored = store(&dir, "selected-cc-9999999999999-x", &[0x47; 188], "note").unwrap();
        assert_eq!((stored.bytes, stored.removed), (188, 1));
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".lock")
            .collect();
        assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("selected-cc-0000000000000-")),
            "the oldest went"
        );
        assert_eq!(names.len(), 2 * DIR_FILES);
        assert_eq!(
            std::fs::read_to_string(dir.join("selected-cc-9999999999999-x.txt")).unwrap(),
            "note\n"
        );
        // A record bigger than the directory may hold: an error, said.
        assert!(store(&dir, "big", &vec![0; DIR_BYTES as usize + 1], "n").is_err());
    }

    #[test]
    fn a_failed_write_is_said_not_swallowed() {
        let file = dir("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let mut capture = Capture::with_ring(file.clone(), 1000);
        let t0 = Instant::now();
        capture.push(&[1; 10], t0);
        capture.trigger("r".into(), "n".into(), t0);
        assert!(matches!(capture.finish(), Some(Err(_))));
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn a_cut_record_is_whole_when_the_session_ends() {
        let dir = dir("cut");
        let mut capture = Capture::with_ring(dir.clone(), 1000);
        let t0 = Instant::now();
        capture.push(&[1; 10], t0);
        assert!(capture.trigger("selected-cc-1".into(), "SELECTED CC ERROR".into(), t0));
        capture.push(&[2; 10], t0);
        let stored = capture.finish().expect("written").expect("ok");
        assert_eq!(
            std::fs::read(&stored.path).unwrap(),
            [[1u8; 10], [2u8; 10]].concat()
        );
        let note = std::fs::read_to_string(stored.path.with_extension("txt")).unwrap();
        assert!(note.contains("cut short"), "{note}");
        // A session's bound.
        for n in 1..SESSION_FILES {
            assert!(capture.trigger(format!("s{n}"), "n".into(), t0));
            capture.cut("next stream");
        }
        assert!(!capture.trigger("over".into(), "n".into(), t0));
        capture.finish();
    }
}
