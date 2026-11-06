//! A binary sidecar of fixed records, for measuring a path packet by packet
//! without writing text per packet: diagnosis only, off unless the process
//! is given a path (each user names its own variable).
//!
//! Format version 2. A record is 56 bytes, little-endian: kind (u16), id
//! (u16), n (u32), then six u64 -- the global sequence, CLOCK_MONOTONIC and
//! CLOCK_MONOTONIC_RAW ns, and three values the kind defines.
//!
//! - The sequence is given under the sender's lock, so a file holds it
//!   strictly increasing; a record not written (the channel full, the trace
//!   finished, the writer stopped) is a hole in it and is counted by cause.
//!   A u64 does not wrap in any run: a sequence going down is a broken file.
//! - Each file starts with a header (`KIND_HEADER`: n the version, a the
//!   magic, b the file's index, seq the next sequence expected) and ends
//!   with a footer and the loss counters (`KIND_FOOTER`, `KIND_LOSS`,
//!   `KIND_IO`). A file without the header is not read (a version 1 file
//!   starts with a data record).
//! - Files are `<path>.0000`, `<path>.0001`, ... at most `cap` bytes each and
//!   `max_files` of them: past that, or with less than `min_free_bytes` free
//!   on the disk, recording stops and what comes after is counted, never
//!   blocking the caller. `<path>.manifest` gets a line per closed file.
//! - The caller hands records to a bounded channel (`try_send`: the caller
//!   never waits); a thread of its own writes them. `finish` closes the
//!   sender, lets the writer drain, write the footer, flush and sync, and
//!   waits for it at most a given time.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const VERSION: u32 = 2;
pub const MAGIC: u64 = u64::from_le_bytes(*b"MBTRACE2");
pub const RECORD: usize = 56;
pub const KIND_HEADER: u16 = 0xFFFF;
/// id: why the file ended (`END_*`), n: its index, seq: the last sequence
/// written in it (u64::MAX for none), a: its data records, b: sequences
/// given so far.
pub const KIND_FOOTER: u16 = 0xFFFE;
/// a: channel full, b: sent after the sender closed, c: discarded by a
/// stopped writer.
pub const KIND_LOSS: u16 = 0xFFFD;
/// a: open errors, b: write errors, c: flush errors << 32 | rotation errors.
pub const KIND_IO: u16 = 0xFFFC;
pub const END_ROTATION: u16 = 0;
pub const END_FINISH: u16 = 1;
pub const END_STOPPED: u16 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Record {
    pub kind: u16,
    pub id: u16,
    pub n: u32,
    pub seq: u64,
    pub mono: u64,
    pub raw: u64,
    pub a: u64,
    pub b: u64,
    pub c: u64,
}

impl Record {
    pub fn bytes(&self) -> [u8; RECORD] {
        let mut out = [0u8; RECORD];
        out[0..2].copy_from_slice(&self.kind.to_le_bytes());
        out[2..4].copy_from_slice(&self.id.to_le_bytes());
        out[4..8].copy_from_slice(&self.n.to_le_bytes());
        for (i, v) in [self.seq, self.mono, self.raw, self.a, self.b, self.c].iter().enumerate() {
            out[8 + i * 8..16 + i * 8].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    pub fn parse(b: &[u8; RECORD]) -> Self {
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"));
        Self {
            kind: u16::from_le_bytes([b[0], b[1]]),
            id: u16::from_le_bytes([b[2], b[3]]),
            n: u32::from_le_bytes(b[4..8].try_into().expect("4 bytes")),
            seq: u64_at(8),
            mono: u64_at(16),
            raw: u64_at(24),
            a: u64_at(32),
            b: u64_at(40),
            c: u64_at(48),
        }
    }

    fn is_meta(&self) -> bool {
        matches!(self.kind, KIND_HEADER | KIND_FOOTER | KIND_LOSS | KIND_IO)
    }
}

fn clock(id: i32) -> u64 {
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, tp: *mut Timespec) -> i32;
    }
    let mut now = Timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid clock id and a timespec to fill.
    unsafe { clock_gettime(id, &mut now) };
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

/// CLOCK_MONOTONIC and CLOCK_MONOTONIC_RAW, ns.
pub fn clocks() -> (u64, u64) {
    (clock(1), clock(4))
}

/// CLOCK_MONOTONIC_RAW, ns.
pub fn raw_now() -> u64 {
    clock(4)
}

/// Free bytes for an unprivileged writer on the filesystem holding `dir`.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn free_bytes(dir: &Path) -> Option<u64> {
    #[repr(C)]
    struct Statvfs {
        f_bsize: u64,
        f_frsize: u64,
        f_blocks: u64,
        f_bfree: u64,
        f_bavail: u64,
        f_files: u64,
        f_ffree: u64,
        f_favail: u64,
        f_fsid: u64,
        f_flag: u64,
        f_namemax: u64,
        spare: [i32; 6],
    }
    unsafe extern "C" {
        fn statvfs(path: *const std::ffi::c_char, buf: *mut Statvfs) -> i32;
    }
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: a NUL-terminated path and a buffer of the glibc 64-bit layout.
    let mut buf: Statvfs = unsafe { std::mem::zeroed() };
    (unsafe { statvfs(path.as_ptr(), &mut buf) } == 0).then(|| buf.f_bavail.saturating_mul(buf.f_frsize))
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
fn free_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// Where the writer puts bytes: a file in production, anything in a test.
pub trait Sink: Write + Send {
    fn sync(&mut self) -> std::io::Result<()>;
}

impl Sink for std::fs::File {
    fn sync(&mut self) -> std::io::Result<()> {
        self.sync_all()
    }
}

pub type Opener = Box<dyn Fn(&Path) -> std::io::Result<Box<dyn Sink>> + Send>;

pub struct Config {
    /// Bytes a file, header and footer included.
    pub cap: u64,
    pub max_files: u32,
    /// Below this free on the disk no new file is started.
    pub min_free_bytes: u64,
    /// Records the channel holds.
    pub channel: usize,
    pub open: Opener,
}

impl Config {
    pub fn files(cap: u64) -> Self {
        Self {
            cap,
            max_files: 16,
            min_free_bytes: 1 << 30,
            channel: 1 << 16,
            open: Box::new(|path: &Path| {
                let file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(path)?;
                Ok(Box::new(file) as Box<dyn Sink>)
            }),
        }
    }
}

/// What was lost, by cause; with `written` and `given` the balance is exact
/// once the writer is done: given = written + channel_full + after_close +
/// discarded.
#[derive(Debug, Default)]
pub struct Counters {
    pub given: AtomicU64,
    pub written: AtomicU64,
    pub channel_full: AtomicU64,
    pub after_close: AtomicU64,
    pub discarded: AtomicU64,
    pub open_errors: AtomicU64,
    pub write_errors: AtomicU64,
    pub flush_errors: AtomicU64,
    pub rotation_errors: AtomicU64,
    pub quota_stops: AtomicU64,
    pub disk_stops: AtomicU64,
    /// The last sequence written, u64::MAX before any.
    pub last_written: AtomicU64,
    pub files: AtomicU64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Summary {
    pub version: u32,
    pub given: u64,
    pub written: u64,
    pub channel_full: u64,
    pub after_close: u64,
    pub discarded: u64,
    pub open_errors: u64,
    pub write_errors: u64,
    pub flush_errors: u64,
    pub rotation_errors: u64,
    pub quota_stops: u64,
    pub disk_stops: u64,
    pub last_written: Option<u64>,
    pub files: u64,
    /// The writer drained, wrote the footer, flushed and synced in time.
    pub finished: bool,
}

impl Summary {
    pub fn dropped(&self) -> u64 {
        self.channel_full + self.after_close + self.discarded
    }
}

impl Counters {
    fn summary(&self, finished: bool) -> Summary {
        let get = |c: &AtomicU64| c.load(Ordering::SeqCst);
        let last = get(&self.last_written);
        Summary {
            version: VERSION,
            given: get(&self.given),
            written: get(&self.written),
            channel_full: get(&self.channel_full),
            after_close: get(&self.after_close),
            discarded: get(&self.discarded),
            open_errors: get(&self.open_errors),
            write_errors: get(&self.write_errors),
            flush_errors: get(&self.flush_errors),
            rotation_errors: get(&self.rotation_errors),
            quota_stops: get(&self.quota_stops),
            disk_stops: get(&self.disk_stops),
            last_written: (last != u64::MAX).then_some(last),
            files: get(&self.files),
            finished,
        }
    }
}

pub fn file_name(path: &Path, index: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index:04}"));
    PathBuf::from(name)
}

struct Writer {
    path: PathBuf,
    config: Config,
    counters: Arc<Counters>,
    out: Option<std::io::BufWriter<Box<dyn Sink>>>,
    index: u32,
    bytes: u64,
    first: Option<u64>,
    last: Option<u64>,
    records: u64,
    /// Recording has stopped (an error, the quota, the disk); what comes is
    /// counted, not written.
    stopped: bool,
}

impl Writer {
    fn meta(r: Record) -> Record {
        let (mono, raw) = clocks();
        Record { mono, raw, ..r }
    }

    fn stop(&mut self) {
        self.stopped = true;
    }

    /// The next file, its header first; false (and stopped) if it cannot be.
    fn open(&mut self, index: u32) -> bool {
        if index >= self.config.max_files {
            self.counters.quota_stops.fetch_add(1, Ordering::SeqCst);
            self.stop();
            return false;
        }
        let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        if free_bytes(dir).is_some_and(|free| free < self.config.min_free_bytes) {
            self.counters.disk_stops.fetch_add(1, Ordering::SeqCst);
            self.stop();
            return false;
        }
        let sink = match (self.config.open)(&file_name(&self.path, index)) {
            Ok(sink) => sink,
            Err(_) => {
                let counter = if index == 0 { &self.counters.open_errors } else { &self.counters.rotation_errors };
                counter.fetch_add(1, Ordering::SeqCst);
                self.stop();
                return false;
            }
        };
        self.out = Some(std::io::BufWriter::with_capacity(1 << 20, sink));
        self.index = index;
        self.bytes = 0;
        self.first = None;
        self.last = None;
        self.records = 0;
        self.counters.files.fetch_add(1, Ordering::SeqCst);
        let next = self.counters.last_written.load(Ordering::SeqCst).wrapping_add(1);
        let header = Self::meta(Record { kind: KIND_HEADER, n: VERSION, seq: next, a: MAGIC, b: u64::from(index), c: self.config.cap, ..Record::default() });
        self.put(&header)
    }

    fn put(&mut self, record: &Record) -> bool {
        let Some(out) = self.out.as_mut() else { return false };
        if out.write_all(&record.bytes()).is_err() {
            self.counters.write_errors.fetch_add(1, Ordering::SeqCst);
            self.out = None;
            self.stop();
            return false;
        }
        self.bytes += RECORD as u64;
        true
    }

    /// The footer and the counters, flushed and synced; the manifest line.
    fn close(&mut self, why: u16) {
        if self.out.is_none() {
            return;
        }
        let c = Arc::clone(&self.counters);
        let get = |x: &AtomicU64| x.load(Ordering::SeqCst);
        let footer = Self::meta(Record {
            kind: KIND_FOOTER,
            id: why,
            n: self.index,
            seq: self.last.unwrap_or(u64::MAX),
            a: self.records,
            b: get(&c.given),
            ..Record::default()
        });
        let loss = Self::meta(Record {
            kind: KIND_LOSS,
            a: get(&c.channel_full),
            b: get(&c.after_close),
            c: get(&c.discarded),
            ..Record::default()
        });
        let io = Self::meta(Record {
            kind: KIND_IO,
            a: get(&c.open_errors),
            b: get(&c.write_errors),
            c: (get(&c.flush_errors) << 32) | (get(&c.rotation_errors) & 0xFFFF_FFFF),
            ..Record::default()
        });
        let _ = self.put(&footer) && self.put(&loss) && self.put(&io);
        if let Some(mut out) = self.out.take()
            && !(out.flush().is_ok() && out.get_mut().sync().is_ok())
        {
            c.flush_errors.fetch_add(1, Ordering::SeqCst);
        }
        let line = format!(
            "{} first={} last={} records={} bytes={} end={}\n",
            file_name(&self.path, self.index).display(),
            self.first.map_or("-".into(), |s| s.to_string()),
            self.last.map_or("-".into(), |s| s.to_string()),
            self.records,
            self.bytes,
            ["rotation", "finish", "stopped"].get(usize::from(why)).copied().unwrap_or("?"),
        );
        let mut manifest = self.path.as_os_str().to_owned();
        manifest.push(".manifest");
        if let Ok(mut m) = std::fs::OpenOptions::new().create(true).append(true).open(PathBuf::from(manifest)) {
            let _ = m.write_all(line.as_bytes());
        }
    }

    fn record(&mut self, record: Record) {
        if self.stopped {
            self.counters.discarded.fetch_add(1, Ordering::SeqCst);
            return;
        }
        // Room for this record and the three closing ones.
        if self.bytes + 4 * RECORD as u64 > self.config.cap {
            self.close(END_ROTATION);
            let next = self.index + 1;
            if !self.open(next) {
                self.counters.discarded.fetch_add(1, Ordering::SeqCst);
                return;
            }
        }
        if !self.put(&record) {
            self.counters.discarded.fetch_add(1, Ordering::SeqCst);
            return;
        }
        self.first.get_or_insert(record.seq);
        self.last = Some(record.seq);
        self.records += 1;
        self.counters.written.fetch_add(1, Ordering::SeqCst);
        self.counters.last_written.store(record.seq, Ordering::SeqCst);
    }

    fn run(mut self, rx: Receiver<Record>) {
        if !self.open(0) {
            // Nothing can be written: what comes is counted.
        }
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(record) => self.record(record),
                // Idle: what is buffered goes to the disk.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(out) = self.out.as_mut()
                        && out.flush().is_err()
                    {
                        self.counters.flush_errors.fetch_add(1, Ordering::SeqCst);
                        self.out = None;
                        self.stop();
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let why = if self.stopped { END_STOPPED } else { END_FINISH };
        self.close(why);
    }
}

pub struct Trace {
    /// The sender, and the next sequence: given under this lock, so the
    /// channel (and a file) holds them in order. None once finished.
    tx: Mutex<Option<(SyncSender<Record>, u64)>>,
    counters: Arc<Counters>,
    done: Mutex<Option<(Receiver<()>, std::thread::JoinHandle<()>)>>,
    finished: Mutex<Option<Summary>>,
}

impl Trace {
    /// A trace to `<path>.NNNN`, `cap` bytes a file, the default quota.
    pub fn start(path: &str, cap: u64) -> Option<Self> {
        Self::with(Path::new(path), Config::files(cap))
    }

    pub fn with(path: &Path, config: Config) -> Option<Self> {
        let (tx, rx) = sync_channel::<Record>(config.channel);
        let counters = Arc::new(Counters { last_written: AtomicU64::new(u64::MAX), ..Counters::default() });
        let writer = Writer {
            path: path.to_path_buf(),
            config,
            counters: Arc::clone(&counters),
            out: None,
            index: 0,
            bytes: 0,
            first: None,
            last: None,
            records: 0,
            stopped: false,
        };
        let (done_tx, done_rx) = sync_channel::<()>(1);
        let handle = std::thread::Builder::new()
            .name("bintrace".into())
            .spawn(move || {
                writer.run(rx);
                let _ = done_tx.send(());
            })
            .ok()?;
        Some(Self {
            tx: Mutex::new(Some((tx, 0))),
            counters,
            done: Mutex::new(Some((done_rx, handle))),
            finished: Mutex::new(None),
        })
    }

    /// From the environment variable `var`, if it names a path.
    pub fn from_env(var: &str, cap: u64) -> Option<Self> {
        let path = std::env::var(var).ok().filter(|p| !p.is_empty())?;
        Self::start(&path, cap)
    }

    pub fn put(&self, mut record: Record) {
        let mut tx = self.tx.lock().unwrap_or_else(|p| p.into_inner());
        let Some((sender, next)) = tx.as_mut() else {
            drop(tx);
            self.counters.after_close.fetch_add(1, Ordering::SeqCst);
            return;
        };
        record.seq = *next;
        *next += 1;
        self.counters.given.fetch_add(1, Ordering::SeqCst);
        match sender.try_send(record) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.counters.channel_full.fetch_add(1, Ordering::SeqCst);
            }
            // The writer is gone (it cannot be before `finish`, but if it
            // is, the record is lost and counted).
            Err(TrySendError::Disconnected(_)) => {
                self.counters.after_close.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// A record stamped now.
    pub fn now(&self, kind: u16, id: u16, n: u32, a: u64, b: u64, c: u64) {
        let (mono, raw) = clocks();
        self.put(Record { kind, id, n, seq: 0, mono, raw, a, b, c });
    }

    /// The counters so far (`finished` false until `finish` has seen the
    /// writer done).
    pub fn summary(&self) -> Summary {
        if let Some(done) = self.finished.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            return done;
        }
        self.counters.summary(false)
    }

    /// Close the sender, let the writer drain, close the file (footer,
    /// flush, sync) and wait for it at most `timeout`. Called again, the
    /// first answer.
    pub fn finish(&self, timeout: Duration) -> Summary {
        let mut finished = self.finished.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(done) = finished.clone() {
            return done;
        }
        drop(self.tx.lock().unwrap_or_else(|p| p.into_inner()).take());
        let done = self.done.lock().unwrap_or_else(|p| p.into_inner()).take();
        let in_time = match done {
            Some((rx, handle)) if rx.recv_timeout(timeout).is_ok() => {
                let _ = handle.join();
                true
            }
            // Not done in time: the writer is left, the caller not held.
            _ => false,
        };
        let summary = self.counters.summary(in_time);
        if in_time {
            *finished = Some(summary.clone());
        }
        summary
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FormatError {
    Short,
    NotVersion2,
    Version(u32),
    Truncated(usize),
}

/// The records of one file, its header checked.
pub fn read(bytes: &[u8]) -> Result<Vec<Record>, FormatError> {
    if bytes.len() < RECORD {
        return Err(FormatError::Short);
    }
    if !bytes.len().is_multiple_of(RECORD) {
        return Err(FormatError::Truncated(bytes.len()));
    }
    let records: Vec<Record> = bytes.as_chunks::<RECORD>().0.iter().map(Record::parse).collect();
    let head = records[0];
    if head.kind != KIND_HEADER || head.a != MAGIC {
        return Err(FormatError::NotVersion2);
    }
    if head.n != VERSION {
        return Err(FormatError::Version(head.n));
    }
    Ok(records)
}

/// What a set of files says about itself.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Check {
    pub records: u64,
    pub first: Option<u64>,
    pub last: Option<u64>,
    /// Sequences missing: between records, and after the last one up to
    /// what the closing footer says was given.
    pub missing: u64,
    /// Sequences not above the one before.
    pub out_of_order: u64,
    /// The last file ends with a finish footer whose last sequence is the
    /// last record's.
    pub closed: bool,
    pub loss: Option<(u64, u64, u64)>,
}

/// Check files in order (their index order).
pub fn check(files: &[Vec<Record>]) -> Check {
    let mut out = Check::default();
    let mut previous: Option<u64> = None;
    for records in files {
        for r in records.iter().filter(|r| !r.is_meta()) {
            out.records += 1;
            out.first.get_or_insert(r.seq);
            match previous {
                Some(p) if r.seq <= p => out.out_of_order += 1,
                Some(p) => out.missing += r.seq - p - 1,
                None => {}
            }
            previous = Some(r.seq);
            out.last = Some(r.seq);
        }
    }
    if let Some(records) = files.last() {
        let footer = records.iter().rev().find(|r| r.kind == KIND_FOOTER);
        out.closed = footer.is_some_and(|f| f.id == END_FINISH && Some(f.seq) == out.last.or(Some(u64::MAX)));
        if let Some(f) = footer {
            let next = out.last.map_or(0, |l| l + 1);
            out.missing += f.b.saturating_sub(next);
        }
        out.loss = records.iter().rev().find(|r| r.kind == KIND_LOSS).map(|l| (l.a, l.b, l.c));
    }
    out
}

#[cfg(test)]
#[path = "bintrace_tests.rs"]
mod tests;
