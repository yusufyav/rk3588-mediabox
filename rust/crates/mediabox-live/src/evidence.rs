//! The evidence of a sound event, kept for it: diagnosis only, off unless
//! `MEDIABOX_LIVE_EVIDENCE=<dir>` names where it goes. Off, every call is
//! one load of a `OnceLock` and nothing else.
//!
//! In memory, rings of the last `KEEP_NS`, each on the monotonic clock the
//! timeline and the incidents are on (`timeline::now_ns`):
//! - `ts`: the selected service's packets as read -- the PAT, its PMT, every
//!   packet of the sound's PID, and the packets of the PCR_PID that carry a
//!   PCR -- a piece at a time;
//! - `pes`: each sound PES start, its PTS (90 kHz) and the running time the
//!   reader put it on (`pts_on_clock`);
//! - `pcm`: the decoded PCM before the volume, as `sound_diag` measures it;
//! - `sink`: what went into the sink, its PTS and its size;
//! - `clock`: every 10 ms (`watch_health`), the pipeline's clock and the
//!   sound's read between two monotonic readings, the sound clock's internal
//!   time and calibration, and the ALSA device's status (hw_ptr, appl_ptr,
//!   delay, avail, tstamp) with how long its read took;
//! - `map`: tsdemux's PES times the reader's did not match within a frame
//!   (`Health::sound_map`), with the reader's starts as they then stood.
//!
//! An event (`request`: a sound incident, a clock reading off its second's
//! median by `CLOCK_STEP_NS`) asks for the window from `BEFORE_NS` before
//! its start to `AFTER_NS` after its end. A thread of its own writes it
//! once that has passed, into a directory of its own made under a
//! temporary name and renamed whole. The directory is held to its quota
//! (`MEDIABOX_LIVE_EVIDENCE_QUOTA_MB`, 256 MiB if not given: an appliance's
//! disk) and `MAX_DUMPS`: the oldest dumps go first, under a lock of the
//! directory's (`.quota.lock`) another player's writer may hold a moment at
//! a channel change -- not had, the clearing waits for the next dump and is
//! counted (`quota_deferred`). A streaming thread never waits on any of
//! this: a ring the writer holds is passed by, and counted (`dropped`).
//!
//! Each channel the evidence is kept for is said once in `active.jsonl`:
//! the player's PID, the channel's generation, and the two settings as this
//! process has them -- that it was on, from the player itself.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub const KEEP_NS: u64 = 15_000_000_000;
pub const BEFORE_NS: u64 = 3_000_000_000;
pub const AFTER_NS: u64 = 3_000_000_000;
/// A clock reading this far from the median of the second before it.
pub const CLOCK_STEP_NS: i64 = 2_000_000;
/// Clock events are many and alike: one dump in this long.
const CLOCK_SPACING_NS: u64 = 120_000_000_000;
const MAX_DUMPS: usize = 64;
const MAX_ITEMS: usize = 8192;
const DEFAULT_QUOTA: u64 = 256 << 20;

static EVIDENCE: OnceLock<Option<Evidence>> = OnceLock::new();

pub fn get() -> Option<&'static Evidence> {
    EVIDENCE
        .get_or_init(|| {
            let dir = std::env::var("MEDIABOX_LIVE_EVIDENCE").ok().filter(|p| !p.is_empty())?;
            let quota = std::env::var("MEDIABOX_LIVE_EVIDENCE_QUOTA_MB").ok().and_then(|q| q.parse::<u64>().ok()).map_or(DEFAULT_QUOTA, |mb| mb << 20);
            Some(Evidence::new(PathBuf::from(dir), quota))
        })
        .as_ref()
        .inspect(|e| e.start_writer())
}

pub fn now() -> u64 {
    mediabox_core::timeline::now_ns()
}

// ------------------------------------------------------------------ rings

pub trait Timed {
    fn at(&self) -> u64;
}

pub struct Ring<T> {
    items: VecDeque<T>,
}

impl<T> Default for Ring<T> {
    fn default() -> Self {
        Self { items: VecDeque::new() }
    }
}

impl<T: Timed + Clone> Ring<T> {
    pub fn push(&mut self, item: T) {
        let newest = item.at();
        self.items.push_back(item);
        while self.items.len() > MAX_ITEMS || self.items.front().is_some_and(|f| newest.saturating_sub(f.at()) > KEEP_NS) {
            self.items.pop_front();
        }
    }

    pub fn window(&self, from: u64, to: u64) -> Vec<T> {
        self.items.iter().filter(|i| i.at() >= from && i.at() <= to).cloned().collect()
    }
}

macro_rules! timed {
    ($($t:ty),+) => { $(impl Timed for $t { fn at(&self) -> u64 { self.at } })+ };
}

#[derive(Debug, Clone)]
pub struct TsPiece {
    pub at: u64,
    pub generation: u64,
    pub packets: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Pes {
    pub at: u64,
    pub generation: u64,
    pub raw: u64,
    pub pts: u64,
}

#[derive(Debug, Clone)]
pub struct Pcm {
    pub at: u64,
    pub generation: u64,
    pub pts: Option<u64>,
    pub discont: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SinkIn {
    pub at: u64,
    pub generation: u64,
    pub pts: u64,
    pub duration: Option<u64>,
    pub bytes: usize,
}

#[derive(Debug, Clone)]
pub struct MapNote {
    pub at: u64,
    pub generation: u64,
    pub theirs: u64,
    pub ours: Vec<u64>,
}

/// The ALSA status file's fields this needs; times in ns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Alsa {
    pub running: bool,
    pub trigger_ns: Option<u64>,
    pub tstamp_ns: Option<u64>,
    pub hw_ptr: Option<u64>,
    pub appl_ptr: Option<u64>,
    pub delay: Option<i64>,
    pub avail: Option<i64>,
}

/// "12345.678901234" (seconds, nanoseconds) as ns.
fn seconds(text: &str) -> Option<u64> {
    let (s, ns) = text.trim().split_once('.')?;
    let ns = format!("{ns:0<9}");
    Some(s.parse::<u64>().ok()? * 1_000_000_000 + ns.get(..9)?.parse::<u64>().ok()?)
}

impl Alsa {
    /// /proc/asound/cardN/pcmMp/sub0/status.
    pub fn parse(text: &str) -> Self {
        let field = |name: &str| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key.trim() == name).then(|| value.trim().to_string())
            })
        };
        Self {
            running: field("state").is_some_and(|s| s == "RUNNING"),
            trigger_ns: field("trigger_time").and_then(|t| seconds(&t)),
            tstamp_ns: field("tstamp").and_then(|t| seconds(&t)),
            hw_ptr: field("hw_ptr").and_then(|v| v.parse().ok()),
            appl_ptr: field("appl_ptr").and_then(|v| v.parse().ok()),
            delay: field("delay").and_then(|v| v.parse().ok()),
            avail: field("avail").and_then(|v| v.parse().ok()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClockSample {
    /// Monotonic just before the pipeline's clock and just after the
    /// sound's: the two readings are no closer in time than this.
    pub at: u64,
    pub after: u64,
    pub generation: u64,
    pub base: u64,
    pub pipeline: Option<u64>,
    pub audio: Option<u64>,
    pub internal: Option<u64>,
    /// The sound clock's calibration: internal, external, rate num, denom.
    pub calibration: Option<[u64; 4]>,
    pub alsa: Option<Alsa>,
    /// How long the status file's read took, ns, and when it began.
    pub alsa_read_at: u64,
    pub alsa_read_ns: u64,
    /// The sink's last buffer's lead on its time (`audio_lead_ns`).
    pub lead_ns: i64,
}

impl ClockSample {
    pub fn late(&self) -> Option<i64> {
        Some(self.pipeline? as i64 - self.audio? as i64)
    }
}

timed!(TsPiece, Pes, Pcm, SinkIn, MapNote, ClockSample);

// ------------------------------------------------------------- the whole

#[derive(Debug, Clone)]
pub struct Request {
    pub kind: String,
    pub from: u64,
    pub to: u64,
    pub note: serde_json::Value,
    /// Written at the player's end before `to`: the end it was asked for.
    pub asked_to: Option<u64>,
}

#[derive(Default)]
struct ClockWatch {
    recent: VecDeque<(u64, i64)>,
    last_request: Option<u64>,
}

pub struct Evidence {
    pub dir: PathBuf,
    pub quota: u64,
    ts: Mutex<Ring<TsPiece>>,
    pes: Mutex<Ring<Pes>>,
    pcm: Mutex<Ring<Pcm>>,
    pcm_format: Mutex<Option<String>>,
    sink: Mutex<Ring<SinkIn>>,
    clock: Mutex<Ring<ClockSample>>,
    map: Mutex<Ring<MapNote>>,
    watch: Mutex<ClockWatch>,
    requests: Mutex<Vec<Request>>,
    /// The service as the reader has it: generation, PAT/PMT, PIDs, codec.
    service: Mutex<serde_json::Value>,
    pub dropped: AtomicU64,
    pub dumps: AtomicU64,
    pub write_errors: AtomicU64,
    pub quota_deferred: AtomicU64,
    /// Channels begun and not yet said in `active.jsonl`.
    active: Mutex<Vec<u64>>,
    active_last: AtomicU64,
    started: std::sync::Once,
    seq: AtomicU64,
}

/// Into a ring without waiting: one the writer holds is passed, counted.
fn put<T: Timed + Clone>(ring: &Mutex<Ring<T>>, item: T, dropped: &AtomicU64) {
    match ring.try_lock() {
        Ok(mut ring) => ring.push(item),
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().push(item),
        Err(std::sync::TryLockError::WouldBlock) => {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The PID of a packet.
pub fn pid(p: &[u8]) -> u16 {
    (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2])
}

/// Whether a packet carries a PCR (adaptation field present, PCR_flag).
pub fn carries_pcr(p: &[u8]) -> bool {
    let afc = (p[3] >> 4) & 0x3;
    afc & 0x2 != 0 && p[4] > 0 && p[5] & 0x10 != 0
}

/// The packets of `data` the evidence keeps: the PAT, the PMTs, the
/// sound's, and the PCR_PID's that carry a PCR.
pub fn keep(data: &[u8], pmts: &[u16], sound: Option<u16>, pcr: Option<u16>) -> Vec<u8> {
    let mut out = Vec::new();
    for p in data.as_chunks::<188>().0 {
        if p[0] != 0x47 {
            continue;
        }
        let id = pid(p);
        if id == 0 || pmts.contains(&id) || Some(id) == sound || (Some(id) == pcr && carries_pcr(p)) {
            out.extend_from_slice(p);
        }
    }
    out
}

impl Evidence {
    pub fn new(dir: PathBuf, quota: u64) -> Self {
        Self {
            dir,
            quota,
            ts: Default::default(),
            pes: Default::default(),
            pcm: Default::default(),
            pcm_format: Default::default(),
            sink: Default::default(),
            clock: Default::default(),
            map: Default::default(),
            watch: Default::default(),
            requests: Default::default(),
            service: Mutex::new(serde_json::Value::Null),
            dropped: Default::default(),
            dumps: Default::default(),
            write_errors: Default::default(),
            quota_deferred: Default::default(),
            active: Default::default(),
            active_last: AtomicU64::new(u64::MAX),
            started: std::sync::Once::new(),
            seq: Default::default(),
        }
    }

    fn start_writer(&'static self) {
        self.started.call_once(|| {
            let _ = std::thread::Builder::new().name("live-evidence".into()).spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    self.write_active();
                    self.write_due(now());
                }
            });
        });
    }

    pub fn ts(&self, generation: u64, data: &[u8], pmts: &[u16], sound: Option<u16>, pcr: Option<u16>) {
        let packets = keep(data, pmts, sound, pcr);
        if !packets.is_empty() {
            put(&self.ts, TsPiece { at: now(), generation, packets }, &self.dropped);
        }
    }

    /// The service as the reader has it; written down only when it changes.
    pub fn service(&self, generation: u64, pmts: &[u16], sound: Option<u16>, pcr: Option<u16>, codec: Option<&'static str>) {
        if self.active_last.swap(generation, Ordering::Relaxed) != generation
            && let Ok(mut active) = self.active.try_lock()
        {
            active.push(generation);
        }
        if let Ok(mut s) = self.service.try_lock() {
            let value = serde_json::json!({ "generation": generation, "pmt_pids": pmts, "sound_pid": sound, "pcr_pid": pcr, "codec": codec });
            if *s != value {
                *s = value;
            }
        }
    }

    pub fn pes(&self, generation: u64, raw: u64, pts: u64) {
        put(&self.pes, Pes { at: now(), generation, raw, pts }, &self.dropped);
    }

    pub fn pcm(&self, generation: u64, format: &str, pts: Option<u64>, discont: bool, data: &[u8]) {
        if let Ok(mut f) = self.pcm_format.try_lock()
            && f.as_deref() != Some(format)
        {
            *f = Some(format.to_string());
        }
        put(&self.pcm, Pcm { at: now(), generation, pts, discont, data: data.to_vec() }, &self.dropped);
    }

    pub fn sink(&self, generation: u64, pts: u64, duration: Option<u64>, bytes: usize) {
        put(&self.sink, SinkIn { at: now(), generation, pts, duration, bytes }, &self.dropped);
    }

    pub fn map(&self, generation: u64, theirs: u64, ours: Vec<u64>) {
        put(&self.map, MapNote { at: now(), generation, theirs, ours }, &self.dropped);
    }

    /// A clock sample; one far off its second's median is an event.
    pub fn clock(&self, sample: ClockSample) {
        let at = sample.at;
        if let Some(late) = sample.late()
            && let Some(off) = lock(&self.watch).see(at, late)
        {
            self.request("clock", at, at, serde_json::json!({ "at": at, "late_ns": late, "off_median_ns": off }));
        }
        put(&self.clock, sample, &self.dropped);
    }

    /// The window around an event from `start` to `end` (monotonic ns).
    pub fn request(&self, kind: &str, start: u64, end: u64, note: serde_json::Value) {
        let from = start.saturating_sub(BEFORE_NS).max(end.saturating_sub(KEEP_NS - AFTER_NS - 1_000_000_000));
        let to = end + AFTER_NS;
        let mut requests = lock(&self.requests);
        // One that overlaps another of its kind joins it.
        if let Some(r) = requests.iter_mut().find(|r| r.kind == kind && from <= r.to && r.from <= to) {
            r.from = r.from.min(from);
            r.to = r.to.max(to);
            return;
        }
        requests.push(Request { kind: kind.to_string(), from, to, note, asked_to: None });
    }

    /// Writes what is due by `now`.
    pub fn write_due(&self, now: u64) {
        let due: Vec<Request> = {
            let mut requests = lock(&self.requests);
            let (due, wait) = requests.drain(..).partition(|r| r.to <= now);
            *requests = wait;
            due
        };
        for request in due {
            self.write(&request);
        }
    }

    /// At the player's end: what was asked for and is not yet due, written
    /// as far as it was gathered -- its window cut at `now`, said so in its
    /// `meta.json` (`truncated_at_shutdown`), never filled with an after it
    /// did not have. A request whose window has passed is written whole.
    /// The caller bounds the time it gives this (`main`): a dump cut off by
    /// the process's exit is a temporary directory, never a half dump.
    pub fn write_pending_at_shutdown(&self, now: u64) -> usize {
        let pending: Vec<Request> = std::mem::take(&mut *lock(&self.requests));
        let count = pending.len();
        for mut request in pending {
            if request.to > now {
                request.asked_to = Some(request.to);
                request.to = now;
            }
            self.write(&request);
        }
        count
    }

    fn write(&self, request: &Request) {
        match self.dump(request) {
            Ok(_) => {
                self.dumps.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                self.write_errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("mediabox-live: olay kanıtı yazılamadı ({}): {error}", request.kind);
            }
        }
        if let Err(error) = self.hold_quota() {
            self.write_errors.fetch_add(1, Ordering::Relaxed);
            eprintln!("mediabox-live: kanıt kotası tutulamadı: {error}");
        }
    }

    /// One window into `<dir>/<seq>-<kind>-<from>`, made whole under a
    /// temporary name first.
    pub fn dump(&self, r: &Request) -> std::io::Result<PathBuf> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let name = format!("{:04}-{}-{}", seq, r.kind, r.from);
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir(&tmp)?;
        let ts = lock(&self.ts).window(r.from, r.to);
        let pes = lock(&self.pes).window(r.from, r.to);
        let pcm = lock(&self.pcm).window(r.from, r.to);
        let sink = lock(&self.sink).window(r.from, r.to);
        let clock = lock(&self.clock).window(r.from, r.to);
        let map = lock(&self.map).window(r.from, r.to);
        let format = lock(&self.pcm_format).clone();
        let service = lock(&self.service).clone();

        let mut file = std::io::BufWriter::new(std::fs::File::create(tmp.join("sound.ts"))?);
        let mut index = std::io::BufWriter::new(std::fs::File::create(tmp.join("ts-index.jsonl"))?);
        for piece in &ts {
            writeln!(index, "{}", serde_json::json!({ "at": piece.at, "gen": piece.generation, "packets": piece.packets.len() / 188 }))?;
            file.write_all(&piece.packets)?;
        }
        file.flush()?;
        index.flush()?;
        let mut file = std::io::BufWriter::new(std::fs::File::create(tmp.join("pcm.raw"))?);
        let mut index = std::io::BufWriter::new(std::fs::File::create(tmp.join("pcm-index.jsonl"))?);
        let mut offset = 0usize;
        for b in &pcm {
            writeln!(
                index,
                "{}",
                serde_json::json!({ "at": b.at, "gen": b.generation, "pts": b.pts, "discont": b.discont, "offset": offset, "bytes": b.data.len() })
            )?;
            file.write_all(&b.data)?;
            offset += b.data.len();
        }
        file.flush()?;
        index.flush()?;
        let lines = |name: &str, values: Vec<serde_json::Value>| -> std::io::Result<()> {
            let mut f = std::io::BufWriter::new(std::fs::File::create(tmp.join(name))?);
            for v in values {
                writeln!(f, "{v}")?;
            }
            f.flush()
        };
        lines("pes.jsonl", pes.iter().map(|p| serde_json::json!({ "at": p.at, "gen": p.generation, "raw": p.raw, "pts": p.pts })).collect())?;
        lines(
            "sink.jsonl",
            sink.iter().map(|s| serde_json::json!({ "at": s.at, "gen": s.generation, "pts": s.pts, "dur": s.duration, "bytes": s.bytes })).collect(),
        )?;
        lines("map.jsonl", map.iter().map(|m| serde_json::json!({ "at": m.at, "gen": m.generation, "theirs": m.theirs, "ours": m.ours })).collect())?;
        lines("clock.jsonl", clock.iter().map(clock_json).collect())?;
        let meta = serde_json::json!({
            "kind": r.kind, "from": r.from, "to": r.to, "note": r.note,
            "truncated_at_shutdown": r.asked_to.is_some(), "asked_to": r.asked_to,
            "pcm_format": format, "service": service,
            "dropped_total": self.dropped.load(Ordering::Relaxed),
            "quota_bytes": self.quota, "quota_deferred_total": self.quota_deferred.load(Ordering::Relaxed),
            "items": { "ts": ts.len(), "pes": pes.len(), "pcm": pcm.len(), "sink": sink.len(), "clock": clock.len(), "map": map.len() },
            "clock": "CLOCK_MONOTONIC ns (mediabox_core::timeline::now_ns), as the timeline and sound-incidents",
        });
        std::fs::write(tmp.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
        let done = self.dir.join(&name);
        std::fs::rename(&tmp, &done)?;
        mediabox_core::timeline!("evidence", "kind={} from={} to={} dir={name}", r.kind, r.from, r.to);
        Ok(done)
    }

    /// The channels begun since the last call, each a line of
    /// `active.jsonl`: the writer's thread, never a streaming one.
    pub fn write_active(&self) {
        let begun: Vec<u64> = std::mem::take(&mut *lock(&self.active));
        if begun.is_empty() {
            return;
        }
        let env = |name: &str| std::env::var(name).ok();
        let result = std::fs::create_dir_all(&self.dir).and_then(|_| {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("active.jsonl"))?;
            for generation in &begun {
                let line = serde_json::json!({
                    "event": "evidence-active", "pid": std::process::id(), "generation": generation, "at": now(),
                    "dir": self.dir, "quota_bytes": self.quota,
                    "MEDIABOX_LIVE_EVIDENCE": env("MEDIABOX_LIVE_EVIDENCE"),
                    "MEDIABOX_LIVE_EVIDENCE_QUOTA_MB": env("MEDIABOX_LIVE_EVIDENCE_QUOTA_MB"),
                });
                // One write a line: lines of two players do not interleave.
                f.write_all(format!("{line}\n").as_bytes())?;
                mediabox_core::timeline!("evidence-active", "gen={generation} quota={}", self.quota);
            }
            Ok(())
        });
        if let Err(error) = result {
            self.write_errors.fetch_add(1, Ordering::Relaxed);
            eprintln!("mediabox-live: kanıt etkinlik kaydı yazılamadı: {error}");
        }
    }

    /// The oldest dumps go until the rest fit the quota and the count --
    /// under the directory's lock, not waited for: held by another
    /// player's writer, the clearing is left to the next dump.
    pub fn hold_quota(&self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.dir.join(".quota.lock"))?;
        // SAFETY: a valid descriptor, a lock that goes with it when `file`
        // closes.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            self.quota_deferred.fetch_add(1, Ordering::Relaxed);
            mediabox_core::timeline!("evidence-quota-deferred", "n={}", self.quota_deferred.load(Ordering::Relaxed));
            return Ok(());
        }
        let mut dumps: Vec<(String, u64)> = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !entry.file_type()?.is_dir() {
                continue;
            }
            dumps.push((name, size_of(&entry.path())));
        }
        dumps.sort();
        let mut total: u64 = dumps.iter().map(|(_, s)| s).sum();
        let mut count = dumps.len();
        for (name, size) in dumps {
            if total <= self.quota && count <= MAX_DUMPS {
                break;
            }
            std::fs::remove_dir_all(self.dir.join(&name))?;
            mediabox_core::timeline!("evidence-removed", "dir={name} bytes={size}");
            total -= size;
            count -= 1;
        }
        Ok(())
    }
}

fn size_of(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map_or(0, |entries| entries.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
}

fn clock_json(c: &ClockSample) -> serde_json::Value {
    let a = c.alsa.clone().unwrap_or_default();
    serde_json::json!({
        "at": c.at, "after": c.after, "gen": c.generation, "base": c.base,
        "pipeline": c.pipeline, "audio": c.audio, "late": c.late(), "internal": c.internal, "calibration": c.calibration,
        "alsa": c.alsa.as_ref().map(|_| serde_json::json!({
            "running": a.running, "trigger": a.trigger_ns, "tstamp": a.tstamp_ns, "hw_ptr": a.hw_ptr, "appl_ptr": a.appl_ptr,
            "delay": a.delay, "avail": a.avail,
        })),
        "alsa_read_at": c.alsa_read_at, "alsa_read_ns": c.alsa_read_ns, "lead_ns": c.lead_ns,
    })
}

impl ClockWatch {
    /// A reading far from the median of the second before it, past the
    /// spacing: how far.
    fn see(&mut self, at: u64, late: i64) -> Option<i64> {
        while self.recent.front().is_some_and(|(t, _)| at.saturating_sub(*t) > 1_000_000_000) {
            self.recent.pop_front();
        }
        let mut found = None;
        if self.recent.len() >= 50 {
            let mut values: Vec<i64> = self.recent.iter().map(|(_, v)| *v).collect();
            values.sort_unstable();
            let off = late - values[values.len() / 2];
            if off.abs() > CLOCK_STEP_NS && self.last_request.is_none_or(|t| at.saturating_sub(t) >= CLOCK_SPACING_NS) {
                self.last_request = Some(at);
                found = Some(off);
            }
        }
        self.recent.push_back((at, late));
        found
    }
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
