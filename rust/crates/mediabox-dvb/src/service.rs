//! The process the control plane starts for live television: one source,
//! one service out of it, one reader, and a status file that says at every
//! moment what it is doing and, at the end, why it ended.
//!
//! ```text
//! source  recording (a captured multiplex, read to its end)
//!       | satellite (the receiver: tune, lock, EP 0x84 stream)
//!   -> Analyser (integrity of the whole multiplex)
//!   -> Selector (one programme, single-programme TS)
//!   -> output queue -> writer thread -> FIFO (mpv) / file / stdout
//! status  /run/mediabox-dvb/status.json, rewritten every second and at
//!         every change of state; the last one written before the output
//!         closes, so a reader that sees the end of the stream finds why.
//! ```
//!
//! How it ends ([`End`]) is the whole point of the status: the natural end of
//! a recording is not a broken stream, and the player going away is not a
//! receiver fault. A live source never ends by itself.

use std::collections::VecDeque;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::avl62x1::{AcqStatus, Avl62x1};
use crate::bridge::Bridge;
use crate::firmware::Firmware;
use crate::log::Log;
use crate::tkgs::TkgsCollector;
use crate::ts::{Analyser, Integrity, RapCache, Selection, Selector};
use crate::tune::{self, LnbControl, Streamed, TuneParams, TuneResult, BAND_SPLIT_KHZ};
use crate::usb::{HardwareAccess, OpenError, Session, StreamStats, TsStream};

/// Set by SIGTERM / SIGINT / SIGHUP: end gracefully, clean up, exit.
pub static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Signals only raise the flag; the cleanup is never interrupted by one.
pub fn install_signal_handlers() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe.
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
}

#[derive(Debug, Clone)]
pub enum Source {
    /// Never opens USB.
    Recording(PathBuf),
    /// The receiver itself; only with the operator's explicit choice.
    Satellite { params: TuneParams, firmware_dir: PathBuf, access: HardwareAccess },
}

#[derive(Debug, Clone)]
pub enum Output {
    /// A FIFO created here; opened once a reader has it open.
    Fifo(PathBuf),
    /// A new regular file.
    File(PathBuf),
    Stdout,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub service_id: u16,
    /// Echoed in the status: which start of the control plane this is, so a
    /// status left by an earlier one is never read as this one's.
    pub token: Option<String>,
    pub source: Source,
    pub output: Output,
    pub status: Option<PathBuf>,
    /// How long the reader may take to open the FIFO.
    pub reader_timeout: Duration,
    /// How long the service may take to appear in the stream.
    pub service_timeout: Duration,
    pub usb_transfers: usize,
    pub usb_buffer_bytes: usize,
    pub usb_queue_bytes: usize,
    /// Demodulator status read while streaming, every so often (None: never).
    pub signal_every: Option<Duration>,
    /// After a USB disconnect, how long to wait for the device to come back.
    pub reconnect: Duration,
    /// Write the whole multiplex, unchanged, instead of one service's
    /// programme: for reading the network's own tables (NIT, SDT other),
    /// which say where the other services are.
    pub raw: bool,
    /// A recording is read at the pace of its own programme clock (PCR),
    /// as a broadcast would arrive, instead of as fast as the reader takes it.
    pub pace: bool,
    /// Bytes the writer may hold for a reader that has stopped reading
    /// (paused). Beyond it a live stream's output is dropped, and counted --
    /// never the USB side, which must keep up whatever the player does.
    pub output_queue_bytes: usize,
    /// Keep the receiver warm across players: when the FIFO's reader goes,
    /// carry on and hand the stream to the next reader that opens it, for
    /// this long at most. With it the control plane changes channel by
    /// writing `select.json` beside the status -- a service id for the
    /// selector -- and starting a player, with nothing reset, reloaded or
    /// retuned (a cold start costs 6 s of demodulator patch and lock).
    pub hold_reader: Option<Duration>,
    /// Boot, lock, and wait in standby for the first `Play`: the receiver
    /// made ready before anybody watches, as a television's tuner is.
    pub start_standby: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    WaitingForPlayer,
    Tuning,
    Streaming,
    /// Booted and locked, the USB stream stopped: nobody is watching.
    Standby,
    Reconnecting,
    /// Locked once, then the carrier went (the dish's cable out, rain): the
    /// player is kept and the same transponder is tried again until it is
    /// back, as a television does.
    NoSignal,
    Stopping,
    Ended,
    Failed,
}

/// Why it ended. Only `RecordingEnd` is the source ending by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum End {
    /// A recording was read to its last byte.
    RecordingEnd,
    /// Asked to stop (SIGTERM from the control plane, Back).
    Stopped,
    /// The reader closed its end (the player quit).
    PlayerClosed,
    /// The reader never opened the FIFO.
    PlayerNeverOpened,
    UsbDisconnected,
    DeviceNotFound,
    NoLock,
    TuneFailed,
    ServiceNotFound,
    SourceFailed,
    OutputFailed,
}

impl End {
    /// An ending nobody asked for and the source did not reach by itself.
    pub fn is_failure(self) -> bool {
        !matches!(self, End::RecordingEnd | End::Stopped | End::PlayerClosed)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OutputStats {
    pub bytes: u64,
    pub dropped_bytes: u64,
    pub dropped_chunks: u64,
    pub max_queued_bytes: usize,
    /// Readers that came after the first (`Options::hold_reader`).
    pub readers: u64,
    /// Bytes nobody was there to read, between readers.
    pub unread_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TuneSummary {
    pub firmware: Option<String>,
    pub firmware_loaded: bool,
    pub lock_ms: Option<u64>,
    pub search_offset_khz: Option<i32>,
    pub rf_verdict: Option<&'static str>,
    pub stopped: Option<String>,
    pub error: Option<String>,
    /// Read only; MediaBox never powers or steers the LNB.
    pub lnb_pins_at_reset: Option<(u32, u32)>,
    pub lnb_pins_untouched: Option<bool>,
    pub ts_ep4_off: Option<bool>,
    pub repeater_closed: Option<bool>,
    pub untouched_ok: Option<bool>,
    pub gpio_restored: Option<bool>,
    pub cleanup_ok: bool,
}

impl From<&TuneResult> for TuneSummary {
    fn from(res: &TuneResult) -> Self {
        Self {
            firmware: res.firmware.clone(),
            firmware_loaded: res.firmware_loaded,
            lock_ms: res.lock_ms,
            search_offset_khz: res.search_offset_khz,
            rf_verdict: res.rf_verdict,
            stopped: res.stopped.clone(),
            error: res.error.clone(),
            lnb_pins_at_reset: res.lnb_pins_at_reset,
            lnb_pins_untouched: res.lnb_pins_untouched,
            ts_ep4_off: res.ts_ep4_off,
            repeater_closed: res.repeater_closed,
            untouched_ok: res.untouched_ok,
            gpio_restored: res.gpio_restored,
            cleanup_ok: res.cleanup_done && res.cleanup_ok(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub token: Option<String>,
    pub state: State,
    pub end: Option<End>,
    pub message: Option<String>,
    pub source: &'static str,
    pub service_id: u16,
    pub service_name: Option<String>,
    pub pid: u32,
    pub elapsed_ms: u64,
    /// From start to the first byte of the service written.
    pub first_output_ms: Option<u64>,
    pub selection: Selection,
    pub integrity: Integrity,
    pub output: OutputStats,
    pub usb: Option<StreamStats>,
    pub tune: Option<TuneSummary>,
    pub signal: Option<AcqStatus>,
    /// Receiver sessions: more than one after a reconnect.
    pub sessions: u32,
    /// Channel changes made without a restart (`select.json`).
    pub switches: u32,
    pub rss_kib: u64,
}

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| pages * 4)
        .unwrap_or(0)
}

/// The status, and where it is written.
struct Reporter {
    path: Option<PathBuf>,
    status: Status,
    start: Instant,
    last_write: Option<Instant>,
}

impl Reporter {
    fn new(path: Option<PathBuf>, options: &Options) -> Self {
        let source = match options.source {
            Source::Recording(_) => "recording",
            Source::Satellite { .. } => "satellite",
        };
        Self {
            path,
            status: Status {
                token: options.token.clone(),
                state: State::Starting,
                end: None,
                message: None,
                source,
                service_id: options.service_id,
                service_name: None,
                pid: std::process::id(),
                elapsed_ms: 0,
                first_output_ms: None,
                selection: Selection { service_id: options.service_id, ..Default::default() },
                integrity: Integrity::default(),
                output: OutputStats::default(),
                usb: None,
                tune: None,
                signal: None,
                sessions: 0,
                switches: 0,
                rss_kib: 0,
            },
            start: Instant::now(),
            last_write: None,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn state(&mut self, state: State, log: &Log) {
        if self.status.state != state {
            log.line(format!("STATE {:?} -> {state:?}", self.status.state));
            self.status.state = state;
            self.write();
        }
    }

    fn maybe_write(&mut self) {
        if self.last_write.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
            self.write();
        }
    }

    fn write(&mut self) {
        self.last_write = Some(Instant::now());
        self.status.elapsed_ms = self.elapsed_ms();
        self.status.rss_kib = rss_kib();
        let Some(path) = &self.path else { return };
        let Ok(text) = serde_json::to_string(&self.status) else { return };
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, path);
        }
    }
}

// ------------------------------------------------------------------ output

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pressure {
    /// The source can wait (a recording): the writer's queue blocks it.
    Block,
    /// The source cannot (a receiver): what does not fit is dropped, counted.
    Drop,
}

/// Where the stream goes. A FIFO stays non-blocking and is written with
/// poll(), so that a reader which has stopped reading (a player held on a
/// frame, paused, wedged) can never keep this process from ending: the
/// writer looks at `abort` at least every 100 ms.
enum Sink {
    Fifo(std::fs::File),
    Plain(Box<dyn Write + Send>),
}

impl Sink {
    fn write_all(&mut self, mut data: &[u8], abort: &AtomicBool) -> std::io::Result<bool> {
        match self {
            Sink::Plain(sink) => sink.write_all(data).and_then(|()| sink.flush()).map(|()| true),
            Sink::Fifo(file) => {
                while !data.is_empty() {
                    if abort.load(Ordering::SeqCst) {
                        return Ok(false);
                    }
                    match file.write(data) {
                        Ok(n) => data = &data[n..],
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            let mut fd = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
                            // SAFETY: one valid descriptor, a stack pollfd.
                            unsafe { libc::poll(&mut fd, 1, 100) };
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(true)
            }
        }
    }
}

struct OutputQueue {
    state: Mutex<(VecDeque<Vec<u8>>, usize, bool)>, // chunks, bytes, closing
    changed: Condvar,
    limit: usize,
    closed: AtomicBool,
    /// Give up on what is queued and on the chunk being written.
    abort: AtomicBool,
    /// What waits for the next reader is the service before a switch: let
    /// it go.
    forget_pending: AtomicBool,
    /// A reader has come and what waited for it is more than its start:
    /// the writer asks (START_ASKED) for the channel from its last access
    /// point instead, and the stream answers (`start_from_access_point`).
    start: std::sync::atomic::AtomicU8,
    /// Since when no reader has had the FIFO open (`reopen` writers).
    reader_gone: Mutex<Option<Instant>>,
    /// Let the reader go: what follows is for the next one, on `fifo`.
    drop_reader: AtomicBool,
    /// The FIFO a next reader opens (`reopen` writers).
    fifo: Mutex<Option<PathBuf>>,
    error: Mutex<Option<std::io::Error>>,
    stats: Mutex<OutputStats>,
}

struct Writer {
    queue: Arc<OutputQueue>,
    thread: Option<std::thread::JoinHandle<()>>,
    pressure: Pressure,
}

impl Writer {
    /// `sink` None: no reader yet; the first one is waited for as a next
    /// one is (`reopen` is then needed). A `reopen` writer never ends for
    /// want of a reader: how long it has had none is the stream's to judge
    /// (`unread_for`).
    fn start(sink: Option<Sink>, limit: usize, pressure: Pressure, reopen: Option<PathBuf>) -> Self {
        let queue = Arc::new(OutputQueue {
            state: Mutex::new((VecDeque::new(), 0, false)),
            changed: Condvar::new(),
            limit,
            closed: AtomicBool::new(false),
            abort: AtomicBool::new(false),
            forget_pending: AtomicBool::new(false),
            start: std::sync::atomic::AtomicU8::new(START_IDLE),
            reader_gone: Mutex::new(sink.is_none().then(Instant::now)),
            drop_reader: AtomicBool::new(false),
            fifo: Mutex::new(reopen),
            error: Mutex::new(None),
            stats: Mutex::new(OutputStats::default()),
        });
        let shared = Arc::clone(&queue);
        let thread = std::thread::Builder::new()
            .name("ts-output".into())
            .spawn(move || {
                let mut sink = sink;
                let gone = |since: Option<Instant>| *shared.reader_gone.lock().unwrap_or_else(|p| p.into_inner()) = since;
                // Between readers, what arrives is kept for the next one --
                // above all a channel's start from its last access point
                // (`RapCache`), written before the player has opened the FIFO
                // -- up to a bound; the oldest goes first beyond it.
                let mut pending: VecDeque<Vec<u8>> = VecDeque::new();
                let mut pending_bytes = 0usize;
                loop {
                    let chunk = {
                        let mut state = shared.state.lock().unwrap_or_else(|p| p.into_inner());
                        loop {
                            if let Some(chunk) = state.0.pop_front() {
                                state.1 -= chunk.len();
                                shared.changed.notify_all();
                                break Some(chunk);
                            }
                            if state.2 {
                                break None;
                            }
                            state = shared.changed.wait(state).unwrap_or_else(|p| p.into_inner());
                        }
                    };
                    let Some(chunk) = chunk else { break };
                    if shared.forget_pending.swap(false, Ordering::SeqCst) {
                        pending.clear();
                        pending_bytes = 0;
                    }
                    if shared.drop_reader.swap(false, Ordering::SeqCst) && sink.take().is_some() {
                        gone(Some(Instant::now()));
                    }
                    // Between readers: the next one is looked for with every
                    // chunk, and the chunk waits for it.
                    let next = shared.fifo.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    if sink.is_none()
                        && let Some(path) = &next
                    {
                        match reopen_fifo(path) {
                            Some(file) => {
                                sink = Some(Sink::Fifo(file));
                                gone(None);
                                shared.stats.lock().unwrap_or_else(|p| p.into_inner()).readers += 1;
                                // Live, the reader starts where the broadcast
                                // is, not where it was when the receiver began:
                                // what waited is replaced by the channel from
                                // its last access point, if the stream has one
                                // to give (Plus, 2026-10-24: a cold start
                                // handed mpv a second of backlog and it played
                                // that second behind for the whole channel).
                                if !pending.is_empty() {
                                    shared.start.store(START_ASKED, Ordering::SeqCst);
                                    let until = Instant::now() + Duration::from_millis(200);
                                    while shared.start.load(Ordering::SeqCst) == START_ASKED && Instant::now() < until {
                                        std::thread::sleep(Duration::from_millis(5));
                                    }
                                    if shared.start.swap(START_IDLE, Ordering::SeqCst) == START_GIVEN {
                                        pending.clear();
                                        pending_bytes = 0;
                                        // This chunk is in what was given.
                                        continue;
                                    }
                                }
                                let mut lost = false;
                                while let Some(waiting) = pending.pop_front() {
                                    pending_bytes -= waiting.len();
                                    let Some(out) = sink.as_mut() else { break };
                                    match out.write_all(&waiting, &shared.abort) {
                                        Ok(true) => {
                                            shared.stats.lock().unwrap_or_else(|p| p.into_inner()).bytes += waiting.len() as u64
                                        }
                                        Ok(false) => return,
                                        Err(_) => {
                                            lost = true;
                                            break;
                                        }
                                    }
                                }
                                if lost {
                                    sink = None;
                                    gone(Some(Instant::now()));
                                    continue;
                                }
                            }
                            None => {
                                pending_bytes += chunk.len();
                                pending.push_back(chunk);
                                while pending_bytes > PENDING_LIMIT {
                                    let Some(old) = pending.pop_front() else { break };
                                    pending_bytes -= old.len();
                                    shared.stats.lock().unwrap_or_else(|p| p.into_inner()).unread_bytes += old.len() as u64;
                                }
                                continue;
                            }
                        }
                    }
                    let Some(out) = sink.as_mut() else { return };
                    match out.write_all(&chunk, &shared.abort) {
                        Ok(true) => shared.stats.lock().unwrap_or_else(|p| p.into_inner()).bytes += chunk.len() as u64,
                        Ok(false) => return,
                        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe && next.is_some() => {
                            // The player went; the receiver stays for the next.
                            sink = None;
                            gone(Some(Instant::now()));
                        }
                        Err(error) => {
                            *shared.error.lock().unwrap_or_else(|p| p.into_inner()) = Some(error);
                            shared.closed.store(true, Ordering::SeqCst);
                            shared.changed.notify_all();
                            return;
                        }
                    }
                }
            })
            .expect("spawn the output thread");
        Self { queue, thread: Some(thread), pressure }
    }

    /// Hand a chunk to the writer. False once the reader is gone.
    fn send(&self, chunk: Vec<u8>) -> bool {
        if chunk.is_empty() {
            return !self.closed();
        }
        let mut state = self.queue.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if self.closed() {
                return false;
            }
            if state.1 + chunk.len() <= self.queue.limit || state.0.is_empty() {
                break;
            }
            if self.pressure == Pressure::Drop {
                let mut stats = self.queue.stats.lock().unwrap_or_else(|p| p.into_inner());
                stats.dropped_bytes += chunk.len() as u64;
                stats.dropped_chunks += 1;
                return true;
            }
            if STOP.load(Ordering::SeqCst) {
                return true;
            }
            state = self.queue.changed.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(|p| p.into_inner()).0;
        }
        state.1 += chunk.len();
        state.0.push_back(chunk);
        let mut stats = self.queue.stats.lock().unwrap_or_else(|p| p.into_inner());
        stats.max_queued_bytes = stats.max_queued_bytes.max(state.1);
        drop(stats);
        self.queue.changed.notify_all();
        true
    }

    fn closed(&self) -> bool {
        self.queue.closed.load(Ordering::SeqCst)
    }

    /// Let the reader go -- it sees the end of its file -- and write what
    /// follows to whoever opens `fifo` next.
    fn redirect(&self, fifo: PathBuf) {
        *self.queue.fifo.lock().unwrap_or_else(|p| p.into_inner()) = Some(fifo);
        self.queue.drop_reader.store(true, Ordering::SeqCst);
        self.queue.changed.notify_all();
    }

    /// How long no reader has had the FIFO open; None while one has.
    fn unread_for(&self) -> Option<Duration> {
        self.queue.reader_gone.lock().unwrap_or_else(|p| p.into_inner()).map(|since| since.elapsed())
    }

    /// A new Play gets the full reader grace period. Standby may have kept
    /// this writer alive without a reader for much longer than that period.
    fn await_reader(&self) {
        let mut since = self.queue.reader_gone.lock().unwrap_or_else(|p| p.into_inner());
        if since.is_some() {
            *since = Some(Instant::now());
        }
    }

    /// A reader has come and waits for its start (see `OutputQueue::start`).
    fn wants_start(&self) -> bool {
        self.queue.start.load(Ordering::SeqCst) == START_ASKED
    }

    /// Answer `wants_start`: `from` is the channel from its last access
    /// point; empty, the reader gets what waited for it.
    fn start_from_access_point(&self, from: Vec<u8>) {
        if from.is_empty() {
            self.queue.start.store(START_NONE, Ordering::SeqCst);
            return;
        }
        self.discard();
        self.send(from);
        self.queue.start.store(START_GIVEN, Ordering::SeqCst);
    }

    /// Forget what is queued: it is the service before a switch.
    fn discard(&self) {
        let mut state = self.queue.state.lock().unwrap_or_else(|p| p.into_inner());
        state.0.clear();
        state.1 = 0;
        self.queue.forget_pending.store(true, Ordering::SeqCst);
        self.queue.changed.notify_all();
    }

    fn stats(&self) -> OutputStats {
        self.queue.stats.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn broken_pipe(&self) -> bool {
        self.queue
            .error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
    }

    /// Write what is queued (unless the reader is gone or a stop was asked
    /// for), then close the output.
    fn finish(mut self, flush: bool) -> OutputStats {
        {
            let mut state = self.queue.state.lock().unwrap_or_else(|p| p.into_inner());
            if !flush {
                state.0.clear();
                state.1 = 0;
                self.queue.abort.store(true, Ordering::SeqCst);
            }
            state.2 = true;
            self.queue.changed.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.stats()
    }
}

/// A new FIFO at `path`, in place of one an earlier run left.
fn make_fifo(path: &Path, log: &Log) -> Result<(), End> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_fifo() {
            log.line(format!("output {} exists and is not a FIFO", path.display()));
            return Err(End::OutputFailed);
        }
        let _ = std::fs::remove_file(path);
    }
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| End::OutputFailed)?;
    // SAFETY: a NUL-terminated path.
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
        log.line(format!("mkfifo {}: {}", path.display(), std::io::Error::last_os_error()));
        return Err(End::OutputFailed);
    }
    log.line(format!("output FIFO {} made", path.display()));
    Ok(())
}

/// Create the FIFO, say so (`created`), and wait for its reader; the open end.
///
/// The order is the point. The status says "waiting for the player" only once
/// this run's FIFO exists: said before, a FIFO left by the previous run was
/// still there, and a player started on it would open the old one, which
/// nobody will ever write.
fn open_fifo(path: &Path, timeout: Duration, log: &Log, created: impl FnOnce()) -> Result<std::fs::File, End> {
    use std::os::unix::ffi::OsStrExt;
    make_fifo(path, log)?;
    log.line(format!("output FIFO {} waiting for its reader", path.display()));
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| End::OutputFailed)?;
    created();
    let deadline = Instant::now() + timeout;
    loop {
        // O_NONBLOCK: opening for writing fails with ENXIO while nobody reads,
        // instead of blocking past a stop or the timeout.
        // SAFETY: a NUL-terminated path; the descriptor is owned below.
        let fd = unsafe { libc::open(name.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd >= 0 {
            // SAFETY: fd was just opened and is owned by nobody else.
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            // Left non-blocking: the writer polls (see Sink).
            log.line("output FIFO: reader connected");
            return Ok(std::fs::File::from(owned));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENXIO) {
            log.line(format!("open {}: {error}", path.display()));
            return Err(End::OutputFailed);
        }
        if STOP.load(Ordering::SeqCst) {
            return Err(End::Stopped);
        }
        if Instant::now() > deadline {
            return Err(End::PlayerNeverOpened);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

const START_IDLE: u8 = 0;
const START_ASKED: u8 = 1;
const START_GIVEN: u8 = 2;
const START_NONE: u8 = 3;

/// What a warm receiver keeps for a reader that has not come yet: a few
/// seconds of one service.
const PENDING_LIMIT: usize = 16 << 20;

/// The FIFO again, for the next reader, if one has it open.
fn reopen_fifo(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: a NUL-terminated path; the descriptor is owned below.
    let fd = unsafe { libc::open(name.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    // SAFETY: fd was just opened and is owned by nobody else.
    (fd >= 0).then(|| std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

fn open_output(output: &Output, timeout: Duration, log: &Log, created: impl FnOnce()) -> Result<Sink, End> {
    match output {
        Output::Fifo(path) => Ok(Sink::Fifo(open_fifo(path, timeout, log, created)?)),
        Output::File(path) => {
            created();
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map(|file| Sink::Plain(Box::new(std::io::BufWriter::with_capacity(1 << 20, file))))
                .map_err(|error| {
                    log.line(format!("output {}: {error}", path.display()));
                    End::OutputFailed
                })
        }
        Output::Stdout => {
            created();
            Ok(Sink::Plain(Box::new(std::io::stdout())))
        }
    }
}

// ------------------------------------------------------------------ pipeline

/// Analyser + selector + writer, shared by both sources.
struct Pipeline {
    /// While the signal is being found again: the transport stream the
    /// channel was on. A lock is the channel's only once its PAT says this.
    relock_tsid: Option<u16>,
    /// When the next retune of a lost signal is due.
    retune_at: Option<Instant>,
    /// After a retune to another transponder: the transport stream of the
    /// one before. Read again, the tuner has not moved (VDR's sdt.c: "We
    /// expect a change in NID/TID", tsWrong) and is tuned again.
    left_tsid: Option<u16>,
    /// The select.json modification read last, for the tuner to know a
    /// newer one has come while it searches (`tune::run`, `superseded`).
    select_seen: std::sync::Arc<std::sync::Mutex<Option<std::time::SystemTime>>>,
    analyser: Analyser,
    selector: Selector,
    writer: Writer,
    scratch: Vec<u8>,
    raw: bool,
    guide: GuideFile,
    /// The service being selected, and since when.
    service_id: u16,
    selected_at: Instant,
    select: Option<SelectFile>,
    /// Warm receivers only: where each programme can start from.
    rap: Option<RapCache>,
    /// The transponder tuned (IF kHz, kS/s), and how a retune searches.
    tuned: Option<TuneParams>,
    /// `Options::start_standby`, until the first lock has been had.
    standby_first: bool,
    /// The FIFO written now.
    fifo: Option<PathBuf>,
    tkgs: TkgsCollector,
    tkgs_path: Option<PathBuf>,
    tkgs_token: Option<String>,
    tkgs_started: Option<Instant>,
    tkgs_done: bool,
}

/// `select.json`: the control plane's word on what the receiver is for now
/// -- a service, perhaps on another transponder, or standby -- read at most
/// every 100 ms and only when it has changed.
struct SelectFile {
    path: PathBuf,
    token: Option<String>,
    modified: Option<std::time::SystemTime>,
    checked: Instant,
}

#[derive(serde::Deserialize)]
struct Select {
    token: Option<String>,
    want: Want,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Want {
    /// Pass `service_id`; on `transponder` if given and not the one tuned.
    Play {
        service_id: u16,
        #[serde(default)]
        transponder: Option<Transponder>,
        /// The player goes on, opening the new service as a new file: the
        /// output moves to a new FIFO of this name beside the old one, and
        /// the old one's reader sees its end. A new path, so the reader of
        /// the old file can never be taken for the new one's.
        #[serde(default)]
        new_fifo: Option<String>,
    },
    /// Nobody is watching: the USB stream stops, the demodulator stays
    /// booted and locked for the next `Play`.
    Standby,
    /// Read the complete TKGS table; the result is written beside status.
    Tkgs { transponder: Transponder },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
struct Transponder {
    frequency_khz: u32,
    symbol_rate_ksps: u32,
    #[serde(default)]
    polarisation: Polarisation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Polarisation {
    Horizontal,
    #[default]
    Vertical,
}

/// What `select.json` asks of the stream loop.
#[derive(Debug)]
enum Order {
    Nothing,
    /// Stream (again, from standby).
    Play,
    Standby,
    /// Another transponder: lock it with the demodulator as it is.
    Retune(TuneParams),
}

/// The multiplex's services and their event information, written beside the
/// status (`guide.json`) for the control plane: what the channel list and
/// the programme guide are made of. Only what the broadcaster sent.
#[derive(Serialize)]
pub struct Guide<'a> {
    pub token: Option<&'a str>,
    pub transport_stream_id: Option<u16>,
    pub services: Vec<GuideService<'a>>,
    pub events: Vec<GuideEvents<'a>>,
}

#[derive(Serialize)]
pub struct GuideService<'a> {
    pub service_id: u16,
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<&'a str>,
    pub service_type: Option<u8>,
    pub free_ca_mode: bool,
}

#[derive(Serialize)]
pub struct GuideEvents<'a> {
    pub service_id: u16,
    pub events: Vec<&'a crate::ts::Event>,
}

/// Events that ended this long ago are no longer written.
const GUIDE_KEEPS_ENDED_S: i64 = 3 * 3600;
/// At most this often, and only when something changed.
const GUIDE_INTERVAL: Duration = Duration::from_secs(3);

struct GuideFile {
    path: Option<PathBuf>,
    token: Option<String>,
    /// The analyser's change count last written, and when.
    written: std::cell::Cell<(u64, Option<Instant>)>,
}

impl GuideFile {
    fn new(status: Option<&Path>, token: Option<String>) -> Self {
        Self {
            path: status.map(|status| status.with_file_name("guide.json")),
            token,
            written: std::cell::Cell::new((0, None)),
        }
    }

    fn write(&self, analyser: &Analyser) {
        let Some(path) = &self.path else { return };
        let (changes, at) = self.written.get();
        if changes == analyser.guide_changes || at.is_some_and(|at| at.elapsed() < GUIDE_INTERVAL) {
            return;
        }
        self.written.set((analyser.guide_changes, Some(Instant::now())));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let guide = guide_of(analyser, self.token.as_deref(), now);
        let Ok(text) = serde_json::to_string(&guide) else { return };
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, path);
        }
    }
}

/// What the analyser knows of the multiplex's services and events at `now`.
pub fn guide_of<'a>(analyser: &'a Analyser, token: Option<&'a str>, now: i64) -> Guide<'a> {
    Guide {
        token,
        transport_stream_id: analyser.integrity().transport_stream_id,
        services: analyser
            .sdt
            .iter()
            .filter_map(|(&service_id, service)| {
                Some(GuideService {
                    service_id,
                    name: service.name.as_deref().filter(|name| !name.is_empty())?,
                    provider: service.provider.as_deref().filter(|p| !p.is_empty()),
                    service_type: service.service_type,
                    free_ca_mode: service.free_ca_mode,
                })
            })
            .collect(),
        events: analyser
            .events
            .iter()
            .map(|(&service_id, events)| GuideEvents {
                service_id,
                events: events
                    .values()
                    .filter(|e| e.start + e.duration as i64 >= now - GUIDE_KEEPS_ENDED_S)
                    .collect(),
            })
            .filter(|service| !service.events.is_empty())
            .collect(),
    }
}

impl Pipeline {
    /// One chunk of the multiplex. False once the reader is gone.
    fn feed(&mut self, chunk: &[u8], reporter: &mut Reporter) -> bool {
        let selector = &mut self.selector;
        let scratch = &mut self.scratch;
        let tkgs = &mut self.tkgs;
        match self.rap.as_mut() {
            Some(rap) => {
                rap.learn(&self.analyser.pat, &self.analyser.pmts);
                self.analyser.feed(chunk, |packet| {
                    tkgs.packet(packet);
                    rap.packet(packet);
                    selector.packet(packet, scratch);
                });
            }
            None => self.analyser.feed(chunk, |packet| {
                tkgs.packet(packet);
                selector.packet(packet, scratch);
            }),
        }
        if self.tkgs_started.is_some() && !self.tkgs_done && let Some(table) = self.tkgs.table() {
            self.write_tkgs(serde_json::json!({
                "token": self.tkgs_token, "state": "complete",
                "transponders": table.transponders.len(), "channels": table.channel_count(), "table": table,
            }));
            self.tkgs_done = true;
        }
        if self.raw {
            self.scratch.clear();
            self.scratch.extend_from_slice(chunk);
        }
        if !self.scratch.is_empty() && reporter.status.first_output_ms.is_none() {
            reporter.status.first_output_ms = Some(reporter.elapsed_ms());
        }
        let out = std::mem::take(&mut self.scratch);
        self.writer.send(out)
    }

    fn write_tkgs(&self, value: serde_json::Value) {
        let Some(path) = &self.tkgs_path else { return };
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, value.to_string()).is_ok() {
            let _ = std::fs::rename(&temporary, path);
        }
    }

    fn begin_tkgs(&mut self) {
        self.tkgs = TkgsCollector::new();
        self.tkgs_started = Some(Instant::now());
        self.tkgs_done = false;
        self.write_tkgs(serde_json::json!({"token": self.tkgs_token, "state": "reading"}));
    }

    fn expire_tkgs(&mut self) {
        if self.tkgs_started.is_some_and(|at| at.elapsed() >= Duration::from_secs(75)) && !self.tkgs_done {
            let (sections, expected) = self.tkgs.progress();
            let detail = self.tkgs.error().map(str::to_owned);
            self.write_tkgs(serde_json::json!({
                "token": self.tkgs_token, "state": "failed",
                "error": detail.unwrap_or_else(|| format!("TKGS tablosu tamamlanmadı ({sections}/{expected} bölüm)")),
            }));
            self.tkgs_done = true;
        }
    }

    fn report(&self, reporter: &mut Reporter) {
        let status = &mut reporter.status;
        // The rate the display is matched to is read from here, and the
        // player is held until it is: said at once, not at the next second.
        let video_found = status.selection.video.is_none() && self.selector.selection().video.is_some();
        status.selection = self.selector.selection().clone();
        status.integrity = self.analyser.integrity();
        status.output = self.writer.stats();
        if status.service_name.is_none() {
            status.service_name = self.analyser.sdt.get(&status.service_id).and_then(|s| s.name.clone());
        }
        self.guide.write(&self.analyser);
        if video_found {
            reporter.write();
        }
    }

    /// The output to a new FIFO, `name` beside the current one; the old one
    /// goes once its reader has been let go.
    fn move_output(&mut self, name: &str, log: &Log) {
        let valid = !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        let Some(old) = self.fifo.clone().filter(|_| valid) else {
            log.line(format!("SELECT new FIFO {name:?} refused"));
            return;
        };
        let new = old.with_file_name(name);
        if new == old || make_fifo(&new, log).is_err() {
            return;
        }
        self.writer.redirect(new.clone());
        let _ = std::fs::remove_file(&old);
        log.line(format!("SELECT output moves to {}", new.display()));
        self.fifo = Some(new);
    }

    /// Standby: what was kept for a reader, and where each programme could
    /// start from, are of the stream before -- stale when it resumes.
    fn idle(&mut self) {
        self.writer.discard();
        if let Some(rap) = self.rap.as_mut() {
            *rap = RapCache::default();
        }
    }

    /// What the control plane asks now, if it asks anything new. A service
    /// of the multiplex tuned is a new start for the selector, from that
    /// service's last access point, with what was queued for the old one
    /// gone; a service elsewhere is a retune.
    fn follow_select(&mut self, reporter: &mut Reporter, log: &Log) -> Order {
        if self.tkgs_done && self.tkgs_started.take().is_some() {
            self.tkgs_done = false;
            log.line("TKGS read ended; returning to standby");
            return Order::Standby;
        }
        let Some(select) = self.select.as_mut() else { return Order::Nothing };
        if select.checked.elapsed() < Duration::from_millis(100) {
            return Order::Nothing;
        }
        select.checked = Instant::now();
        let modified = std::fs::metadata(&select.path).and_then(|m| m.modified()).ok();
        if modified.is_none() || modified == select.modified {
            return Order::Nothing;
        }
        select.modified = modified;
        *self.select_seen.lock().unwrap_or_else(|p| p.into_inner()) = modified;
        let Some(wanted) = std::fs::read_to_string(&select.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Select>(&text).ok())
            .filter(|wanted| wanted.token == select.token)
        else {
            return Order::Nothing;
        };
        let (service_id, transponder, tkgs) = match wanted.want {
            Want::Standby => {
                log.line("SELECT standby");
                return Order::Standby;
            }
            Want::Play { service_id, transponder, new_fifo } => {
                if let Some(name) = new_fifo {
                    self.move_output(&name, log);
                }
                self.writer.await_reader();
                (service_id, transponder, false)
            }
            Want::Tkgs { transponder } => (self.service_id, Some(transponder), true),
        };
        if tkgs {
            log.line("SELECT TKGS table");
            self.begin_tkgs();
        }
        let retune = transponder.and_then(|t| {
            let rf_khz = crate::tune::universal_if_khz(t.frequency_khz);
            let lnb = Some(LnbControl {
                vertical: t.polarisation == Polarisation::Vertical,
                high_band: t.frequency_khz >= BAND_SPLIT_KHZ,
            });
            let tuned = self.tuned.as_ref()?;
            ((tuned.rf_khz, tuned.symbol_rate_ksps, tuned.lnb) != (rf_khz, t.symbol_rate_ksps, lnb)).then(|| TuneParams {
                rf_khz,
                symbol_rate_ksps: t.symbol_rate_ksps,
                lnb,
                lock_timeout_ms: tuned.lock_timeout_ms,
                search_offsets_khz: tuned.search_offsets_khz.clone(),
                first_offset_khz: None,
            })
        });
        if service_id != self.service_id || retune.is_some() {
            log.line(format!("SELECT service {} -> {service_id}", self.service_id));
            self.service_id = service_id;
            // Another channel: what was being waited for was the last one's.
            self.relock_tsid = None;
            self.retune_at = None;
            self.left_tsid = retune.as_ref().and(self.selector.selection().transport_stream_id);
            self.selected_at = Instant::now();
            self.selector = Selector::new(service_id);
            self.writer.discard();
            if retune.is_some() {
                // Another multiplex: nothing of this one's tables or access
                // points is of use there.
                self.analyser = Analyser::new();
                self.rap = self.rap.as_ref().map(|_| RapCache::default());
            } else if let Some(replay) = self.rap.as_ref().map(|rap| rap.replay(service_id)).filter(|r| !r.is_empty()) {
                // The new channel starts from its last access point, not the next.
                let mut out = Vec::with_capacity(replay.len());
                for packet in replay.chunks(crate::ts::PACKET) {
                    self.selector.packet(packet, &mut out);
                }
                log.line(format!("SELECT replay {} B from the last access point", out.len()));
                self.writer.send(out);
            }
            let status = &mut reporter.status;
            status.service_id = service_id;
            status.service_name = None;
            status.selection = Selection { service_id, ..Default::default() };
            status.switches += 1;
            reporter.write();
        }
        match retune {
            Some(params) => {
                self.tuned = Some(params.clone());
                Order::Retune(params)
            }
            None => Order::Play,
        }
    }
}

/// Run the whole service; the final status. The exit code follows `end`.
pub fn run(options: Options, log: &Log) -> Status {
    let mut reporter = Reporter::new(options.status.clone(), &options);
    reporter.write();
    // A warm receiver does not wait for its player to tune: the demodulator's
    // boot and the lock (5 s cold) run while the player starts, and what
    // arrives before the player opens the FIFO waits for it in the writer.
    let early = matches!(options.source, Source::Satellite { .. }) && options.hold_reader.is_some();
    let sink = match &options.output {
        Output::Fifo(path) if early => match make_fifo(path, log) {
            Ok(()) => {
                reporter.state(State::WaitingForPlayer, log);
                None
            }
            Err(end) => return finish(reporter, None, end, None, log),
        },
        output => match open_output(output, options.reader_timeout, log, || reporter.state(State::WaitingForPlayer, log)) {
            Ok(sink) => Some(sink),
            Err(end) => return finish(reporter, None, end, None, log),
        },
    };
    let pressure = match options.source {
        Source::Recording(_) => Pressure::Block,
        Source::Satellite { .. } => Pressure::Drop,
    };
    let mut pipeline = Pipeline {
        analyser: Analyser::new(),
        selector: Selector::new(options.service_id),
        writer: Writer::start(
            sink,
            options.output_queue_bytes,
            pressure,
            match (&options.output, options.hold_reader) {
                (Output::Fifo(path), Some(_)) => Some(path.clone()),
                _ => None,
            },
        ),
        scratch: Vec::with_capacity(1 << 20),
        raw: options.raw,
        guide: GuideFile::new(options.status.as_deref(), options.token.clone()),
        service_id: options.service_id,
        selected_at: Instant::now(),
        select: options.hold_reader.and(options.status.as_ref()).map(|status| SelectFile {
            path: status.with_file_name("select.json"),
            token: options.token.clone(),
            modified: None,
            checked: Instant::now(),
        }),
        rap: options.hold_reader.map(|_| RapCache::default()),
        tuned: match &options.source {
            Source::Satellite { params, .. } => Some(params.clone()),
            Source::Recording(_) => None,
        },
        standby_first: options.start_standby,
        relock_tsid: None,
        retune_at: None,
        left_tsid: None,
        // What is there at the start is no newer tune: the first read of it
        // is follow_select's to make.
        select_seen: std::sync::Arc::new(std::sync::Mutex::new(
            options
                .status
                .as_ref()
                .and_then(|status| std::fs::metadata(status.with_file_name("select.json")).ok())
                .and_then(|meta| meta.modified().ok()),
        )),
        fifo: match &options.output {
            Output::Fifo(path) => Some(path.clone()),
            _ => None,
        },
        tkgs: TkgsCollector::new(),
        tkgs_path: options.status.as_ref().map(|status| status.with_file_name("tkgs.json")),
        tkgs_token: options.token.clone(),
        tkgs_started: None,
        tkgs_done: false,
    };
    let (end, message) = match &options.source {
        Source::Recording(path) => play_recording(path, &options, &mut pipeline, &mut reporter, log),
        Source::Satellite { params, firmware_dir, access } => {
            play_satellite(params, firmware_dir, access, &options, &mut pipeline, &mut reporter, log)
        }
    };
    finish(reporter, Some(pipeline), end, message, log)
}

fn finish(mut reporter: Reporter, pipeline: Option<Pipeline>, end: End, message: Option<String>, log: &Log) -> Status {
    reporter.state(State::Stopping, log);
    if let Some(pipeline) = &pipeline {
        pipeline.report(&mut reporter);
    }
    reporter.status.end = Some(end);
    reporter.status.message = message.clone();
    reporter.status.state = if end.is_failure() { State::Failed } else { State::Ended };
    log.line(format!(
        "END {end:?}{}: {:?}",
        message.map(|m| format!(" ({m})")).unwrap_or_default(),
        reporter.status.integrity
    ));
    // The last word is written before the output closes: whoever sees the end
    // of the stream finds why it ended.
    reporter.write();
    if let Some(pipeline) = pipeline {
        let flush = end == End::RecordingEnd;
        reporter.status.output = pipeline.writer.finish(flush);
        reporter.write();
    }
    reporter.status
}

fn check_service(pipeline: &Pipeline, options: &Options, streaming_since: Instant) -> Option<(End, Option<String>)> {
    let since = streaming_since.max(pipeline.selected_at);
    (!options.raw && pipeline.tkgs_started.is_none() && !pipeline.selector.ready() && since.elapsed() > options.service_timeout).then(|| {
        (End::ServiceNotFound, Some(format!("SID {} bu yayında bulunamadı", pipeline.service_id)))
    })
}

fn play_recording(path: &Path, options: &Options, pipeline: &mut Pipeline, reporter: &mut Reporter, log: &Log) -> (End, Option<String>) {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return (End::SourceFailed, Some(format!("{}: {error}", path.display()))),
    };
    log.line(format!("source: recording {}", path.display()));
    reporter.state(State::Streaming, log);
    let since = Instant::now();
    let mut pacer = Pacer::default();
    let mut buffer = vec![0u8; 188 * 256];
    loop {
        if STOP.load(Ordering::SeqCst) {
            return (End::Stopped, None);
        }
        let n = match file.read(&mut buffer) {
            Ok(0) => {
                if !options.raw && !pipeline.selector.ready() {
                    return (End::ServiceNotFound, Some(format!("SID {} kayıtta yok", options.service_id)));
                }
                return (End::RecordingEnd, None);
            }
            Ok(n) => n,
            Err(error) => return (End::SourceFailed, Some(error.to_string())),
        };
        if !pipeline.feed(&buffer[..n], reporter) {
            return closed(pipeline);
        }
        if options.pace
            && let Some(pcr) = pipeline.selector.last_pcr()
        {
            pacer.wait(pcr);
        }
        if let Some(failed) = check_service(pipeline, options, since) {
            return failed;
        }
        // A recording is one service, played once: nothing to change to.
        let _ = pipeline.follow_select(reporter, log);
        pipeline.report(reporter);
        reporter.maybe_write();
    }
}

/// Holds a recording to its programme clock. A PCR that goes backwards or
/// jumps more than a second (a discontinuity, a wrap) starts the clock again.
#[derive(Default)]
struct Pacer {
    origin: Option<(u64, Instant)>,
}

impl Pacer {
    const TICKS_PER_SECOND: f64 = 27_000_000.0;

    fn wait(&mut self, pcr: u64) {
        let (pcr0, t0) = *self.origin.get_or_insert((pcr, Instant::now()));
        let due = if pcr >= pcr0 { (pcr - pcr0) as f64 / Self::TICKS_PER_SECOND } else { f64::NAN };
        let now = t0.elapsed().as_secs_f64();
        if !due.is_finite() || due - now > 1.0 || now - due > 1.0 {
            self.origin = Some((pcr, Instant::now()));
            return;
        }
        if due > now && !STOP.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs_f64(due - now));
        }
    }
}

fn closed(pipeline: &Pipeline) -> (End, Option<String>) {
    if pipeline.writer.broken_pipe() { (End::PlayerClosed, None) } else { (End::OutputFailed, None) }
}

/// What the end of one receiver session means for the service.
#[derive(Debug, PartialEq)]
enum AfterSession {
    /// The device went away under a running service: wait for it.
    Reconnect,
    End(End, Option<String>),
}

fn after_session(
    res: &TuneResult,
    outcome: Option<(End, Option<String>)>,
    stopping: bool,
    reconnect: Duration,
    player_gone: bool,
) -> AfterSession {
    if res.no_device && !stopping && !reconnect.is_zero() && !player_gone {
        return AfterSession::Reconnect;
    }
    let (end, message) = match outcome {
        Some(ended) => ended,
        None if res.no_device => (End::UsbDisconnected, res.error.clone()),
        None if stopping => (End::Stopped, None),
        None if res.boot.is_some() && res.lock_polls.is_some() && !res.fec_locked() => (End::NoLock, res.stopped.clone()),
        None => (End::TuneFailed, res.error.clone().or(res.stopped.clone())),
    };
    AfterSession::End(end, message)
}

fn play_satellite(
    params: &TuneParams,
    firmware_dir: &Path,
    access: &HardwareAccess,
    options: &Options,
    pipeline: &mut Pipeline,
    reporter: &mut Reporter,
    log: &Log,
) -> (End, Option<String>) {
    let firmware = match Firmware::load(firmware_dir) {
        Ok(firmware) => firmware,
        Err(error) => return (End::SourceFailed, Some(error.to_string())),
    };
    let clock = crate::RealClock::default();
    let mut reconnect_until: Option<Instant> = None;
    // A device that is still enumerating (just plugged in) gets a moment.
    let mut open_until = Instant::now() + Duration::from_secs(3);
    loop {
        if STOP.load(Ordering::SeqCst) {
            return (End::Stopped, None);
        }
        let session = match Session::open(access) {
            Ok(session) => session,
            Err(error) => {
                // Not there yet, or there but not ready (enumerating, busy):
                // waited for while a window is open -- at start for a device
                // just plugged in, after a disconnect for the whole reconnect window.
                let until = reconnect_until.unwrap_or(open_until);
                let retry = matches!(error, OpenError::NotFound) || reconnect_until.is_some();
                if retry && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
                return match (reconnect_until, error) {
                    (Some(_), error) => (End::UsbDisconnected, Some(format!("cihaz geri gelmedi ({error})"))),
                    (None, OpenError::NotFound) => (End::DeviceNotFound, Some(OpenError::NotFound.to_string())),
                    (None, error) => (End::SourceFailed, Some(error.to_string())),
                };
            }
        };
        open_until = Instant::now();
        reporter.status.sessions += 1;
        reporter.state(State::Tuning, log);
        log.line(format!("source: satellite, session {} {params:?}", reporter.status.sessions));
        let mut bridge = Bridge::new(&*session, log, &clock, []);
        let mut outcome = None;
        let keep_tuning = options.hold_reader.is_some();
        // A select.json written after the one read last: a newer channel.
        let select_path = pipeline.select.as_ref().map(|select| select.path.clone());
        let select_seen = std::sync::Arc::clone(&pipeline.select_seen);
        let superseded = move || {
            select_path.as_ref().is_some_and(|path| {
                let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
                modified.is_some() && modified != *select_seen.lock().unwrap_or_else(|p| p.into_inner())
            })
        };
        let res = tune::run(&mut bridge, &firmware, params, keep_tuning, &STOP, &superseded, |bridge, demod, tuned| {
            reporter.status.tune = Some(TuneSummary::from(tuned));
            // Tuned and not locked: no signal, until the lock says otherwise.
            // Given up for a newer channel is not that: the stream reads it.
            if !tuned.fec_locked() && !tuned.superseded && reporter.status.state != State::NoSignal {
                log.line("SIGNAL NONE: not locked after the tune; trying again in 10 s");
                pipeline.relock_tsid = None;
                pipeline.retune_at = Some(Instant::now() + Duration::from_secs(10));
                reporter.state(State::NoSignal, log);
            } else if tuned.fec_locked() && reporter.status.state == State::NoSignal && pipeline.relock_tsid.is_none() {
                log.line("SIGNAL BACK: locked after the tune");
                pipeline.retune_at = None;
                reporter.state(State::Streaming, log);
            }
            let (streamed, ended) = stream(session.clone(), bridge, demod, options, pipeline, reporter, log);
            outcome = ended;
            streamed
        });
        reporter.status.tune = Some(TuneSummary::from(&res));
        pipeline.report(reporter);
        reporter.write();
        drop(bridge);
        if let Some(error) = session.release() {
            log.line(format!("USB release: {error}"));
        }
        drop(session);

        match after_session(&res, outcome, STOP.load(Ordering::SeqCst), options.reconnect, pipeline.writer.closed()) {
            AfterSession::Reconnect => {
                log.line(format!("USB disconnected; waiting up to {:?} for the receiver", options.reconnect));
                reporter.state(State::Reconnecting, log);
                reconnect_until = Some(Instant::now() + options.reconnect);
            }
            AfterSession::End(end, message) => return (end, message),
        }
    }
}

/// The locked receiver's stream, until something ends it. Returns what the
/// tuner needs to know (the device went away) and, if the stream decided
/// it, how the whole service ends.
fn stream(
    session: Arc<Session>,
    bridge: &mut Bridge,
    demod: &mut Avl62x1,
    options: &Options,
    pipeline: &mut Pipeline,
    reporter: &mut Reporter,
    log: &Log,
) -> (Streamed, Option<(End, Option<String>)>) {
    let open = |log: &Log| -> Result<TsStream, String> {
        let mut stream = TsStream::new(Arc::clone(&session), options.usb_transfers, options.usb_buffer_bytes, options.usb_queue_bytes)
            .map_err(|error| error.to_string())?;
        log.line(format!("TS stream: {} x {} B bulk transfers queued", options.usb_transfers, options.usb_buffer_bytes));
        stream.start();
        Ok(stream)
    };
    let mut stream = if std::mem::take(&mut pipeline.standby_first) {
        log.line("STANDBY: locked and ready; the TS stream waits for the first Play");
        reporter.state(State::Standby, log);
        None
    } else {
        match open(log) {
            Ok(stream) => {
                // Without a signal it is Streaming only once data comes.
                if reporter.status.state != State::NoSignal {
                    reporter.state(State::Streaming, log);
                }
                Some(stream)
            }
            Err(error) => return (Streamed { no_device: false, retune: None, relock: false }, Some((End::SourceFailed, Some(error)))),
        }
    };
    let since = Instant::now();
    let mut next_signal = options.signal_every.map(|every| Instant::now() + every);
    let mut next_line = Instant::now() + Duration::from_secs(5);
    let mut seen_overflow = 0;
    let mut no_device = false;
    let mut retune = None;
    let mut relock = false;
    // The demodulator's lock status (0x408), read once a second, as the
    // reference does. Traced from Kanky on SK1, 2026-10-24, the dish's cable
    // pulled for half a minute: 0x408 reads 01 while locked and 00 within a
    // second of the cable going; the reference retunes the transponder 12 s
    // after the loss, then every 10 s, reading 0x408 in between, and is back
    // on the first retune after the cable is. Not inferred from the stream:
    // with the cable out the receiver keeps sending (40 000 packets a second).
    let mut lock_checked = Instant::now();
    let ended = loop {
        if STOP.load(Ordering::SeqCst) {
            break Some((End::Stopped, None));
        }
        // The control plane's word first: it decides whether there is a
        // stream to read at all.
        pipeline.expire_tkgs();
        match pipeline.follow_select(reporter, log) {
            Order::Nothing => {}
            Order::Retune(params) => {
                retune = Some(params);
                break None;
            }
            Order::Standby => {
                if let Some(mut running) = stream.take() {
                    let stats = running.stop();
                    log.line(format!("STANDBY: TS stream stopped, the demodulator stays locked: {stats:?}"));
                }
                pipeline.idle();
                reporter.state(State::Standby, log);
            }
            Order::Play => {
                if stream.is_none() {
                    match open(log) {
                        Ok(running) => stream = Some(running),
                        Err(error) => break Some((End::SourceFailed, Some(error))),
                    }
                    reporter.state(State::Streaming, log);
                }
            }
        }
        if pipeline.writer.wants_start() {
            let from = pipeline.rap.as_ref().map(|rap| rap.replay(pipeline.service_id)).unwrap_or_default();
            log.line(format!("READER: starts from the last access point ({} B)", from.len()));
            pipeline.writer.start_from_access_point(from);
        }
        // Nobody has read for the hold: standby, as if asked.
        if stream.is_some()
            && pipeline.tkgs_started.is_none()
            && options.hold_reader.is_some_and(|hold| pipeline.writer.unread_for().is_some_and(|gone| gone > hold))
            && let Some(mut running) = stream.take()
        {
            let stats = running.stop();
            log.line(format!("STANDBY: no reader for {:?}; TS stream stopped: {stats:?}", options.hold_reader));
            pipeline.idle();
            reporter.state(State::Standby, log);
        }
        let Some(running) = stream.as_mut() else {
            std::thread::sleep(Duration::from_millis(50));
            pipeline.report(reporter);
            reporter.maybe_write();
            continue;
        };
        let chunk = running.get(Duration::from_millis(100));
        let stats = running.stats();
        if stats.overflow_chunks != seen_overflow {
            seen_overflow = stats.overflow_chunks;
            log.line(format!(
                "USB QUEUE OVERFLOW: {} bytes in {} chunks dropped so far; last gap at stream byte {:?}",
                stats.overflow_bytes,
                seen_overflow,
                stats.gaps.last()
            ));
        }
        // Found again: the carrier is the channel's when its PAT names the
        // transport stream the channel was on; another one is no signal
        // either, and is tuned away from.
        if let Some(left) = pipeline.left_tsid
            && let Some(got) = pipeline.analyser.integrity().transport_stream_id
        {
            if got == left
                && let Some(tuned) = pipeline.tuned.clone()
            {
                log.line(format!("RETUNE: still on transport stream {got}, the one left; tuning again"));
                pipeline.analyser = Analyser::new();
                retune = Some(tuned);
                relock = true;
                break None;
            }
            pipeline.left_tsid = None;
        }
        if let Some(want) = pipeline.relock_tsid
            && let Some(got) = pipeline.analyser.integrity().transport_stream_id
        {
            if got == want {
                log.line(format!("SIGNAL BACK: transport stream {got} again"));
                pipeline.relock_tsid = None;
                pipeline.retune_at = None;
                reporter.state(State::Streaming, log);
            } else if let Some(tuned) = pipeline.tuned.clone() {
                log.line(format!("RELOCK: locked transport stream {got}, not {want}; trying again"));
                pipeline.analyser = Analyser::new();
                retune = Some(tuned);
                relock = true;
                break None;
            }
        }
        if lock_checked.elapsed() >= Duration::from_secs(1)
            && let Some(tuned) = pipeline.tuned.clone()
        {
            lock_checked = Instant::now();
            match demod.lock_flags(bridge) {
                Ok((0, _)) => {
                    if reporter.status.state != State::NoSignal {
                        pipeline.relock_tsid = pipeline.selector.selection().transport_stream_id;
                        pipeline.retune_at = Some(Instant::now() + Duration::from_secs(12));
                        log.line(format!("SIGNAL LOST: lock status 0; transport stream {:?}", pipeline.relock_tsid));
                        reporter.state(State::NoSignal, log);
                        // What it said about the transport stream was before
                        // the loss; only a PAT after it counts.
                        pipeline.analyser = Analyser::new();
                    }
                    if pipeline.retune_at.is_none_or(|at| Instant::now() >= at) {
                        pipeline.retune_at = Some(Instant::now() + Duration::from_secs(10));
                        pipeline.analyser = Analyser::new();
                        retune = Some(tuned);
                        relock = true;
                        break None;
                    }
                }
                // Locked again, with no transport stream to tell it by (lost
                // before the first PAT): the lock is all there is to go on.
                Ok(_) if reporter.status.state == State::NoSignal && pipeline.relock_tsid.is_none() => {
                    log.line("SIGNAL BACK: locked");
                    pipeline.retune_at = None;
                    reporter.state(State::Streaming, log);
                }
                Ok(_) => {}
                Err(error) if error.is_no_device() => {
                    no_device = true;
                    break None;
                }
                Err(error) => log.line(format!("lock status failed: {error}")),
            }
        }
        match chunk {
            Some(chunk) => {
                if !pipeline.feed(&chunk, reporter) {
                    break Some(closed(pipeline));
                }
            }
            None if running.dead() => {
                no_device = stats.no_device > 0 || !stats.submit_errors.is_empty();
                log.line("USB stream ended (no transfer in flight: disconnect?)");
                break None;
            }
            None => {}
        }
        // Without a signal the service cannot be found; that is no signal,
        // not a service missing from the multiplex.
        if reporter.status.state != State::NoSignal
            && let Some(failed) = check_service(pipeline, options, since)
        {
            break Some(failed);
        }
        // The demodulator, asked between chunks on this thread while the
        // event thread keeps the transfers going.
        if let (Some(at), Some(every)) = (next_signal, options.signal_every)
            && Instant::now() >= at
        {
            next_signal = Some(Instant::now() + every);
            match demod.acq_status(bridge) {
                Ok(status) => reporter.status.signal = Some(status),
                Err(error) => {
                    log.line(format!("signal status failed: {error}"));
                    if error.is_no_device() {
                        no_device = true;
                        break None;
                    }
                }
            }
        }
        reporter.status.usb = Some(stats);
        pipeline.report(reporter);
        reporter.maybe_write();
        if Instant::now() >= next_line {
            next_line = Instant::now() + Duration::from_secs(5);
            let s = &reporter.status;
            log.line(format!(
                "LIVE t={} s in={} pkts out={} B usb_overflow={} out_dropped={} out_max_queued={} cc={} tei={} sync={} \
                 service: cc={} pcr={} pcr_rep={} pcr_disc={} pcr_max={:.1}ms pts_err={} pts_max={:.1}ms signal={:?}",
                since.elapsed().as_secs(),
                s.integrity.packets,
                s.output.bytes,
                s.usb.as_ref().map_or(0, |u| u.overflow_bytes),
                s.output.dropped_bytes,
                s.output.max_queued_bytes,
                s.integrity.cc_errors,
                s.integrity.tei,
                s.integrity.sync_errors,
                s.selection.timing.cc_errors,
                s.selection.timing.pcr_count,
                s.selection.timing.pcr_repetition_errors,
                s.selection.timing.pcr_discontinuities,
                s.selection.timing.pcr_max_interval_ms,
                s.selection.timing.pts_errors,
                s.selection.timing.pts_max_interval_ms,
                s.signal.as_ref().map(|a| (a.fec, a.snr_db, a.strength_pct)),
            ));
        }
    };
    if let Some(mut running) = stream {
        let stats = running.stop();
        // What arrived before the stop is still stream data -- unless the
        // reader is gone, a stop was asked for, or another transponder is.
        let tail = running.drain();
        if ended.is_none() && retune.is_none() {
            for chunk in tail {
                if !pipeline.feed(&chunk, reporter) {
                    break;
                }
            }
        }
        no_device |= stats.no_device > 0;
        log.line(format!("TS stream stopped: {stats:?}"));
        reporter.status.usb = Some(stats);
    }
    if retune.is_some() && !relock {
        reporter.state(State::Tuning, log);
    }
    (Streamed { no_device, retune, relock }, ended)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(no_device: bool, locked: bool) -> TuneResult {
        TuneResult {
            no_device,
            boot: None,
            lock_polls: Some(10),
            lock_flags: Some((0, locked as u8)),
            ..Default::default()
        }
    }

    #[test]
    fn a_disconnect_under_a_running_service_waits_for_the_receiver() {
        let twenty = Duration::from_secs(20);
        assert_eq!(after_session(&session(true, true), None, false, twenty, false), AfterSession::Reconnect);
        // ... unless it is being stopped, the player is gone, or nobody asked for reconnects.
        assert_eq!(
            after_session(&session(true, true), Some((End::Stopped, None)), true, twenty, false),
            AfterSession::End(End::Stopped, None)
        );
        assert_eq!(
            after_session(&session(true, true), Some((End::PlayerClosed, None)), false, twenty, true),
            AfterSession::End(End::PlayerClosed, None)
        );
        assert!(matches!(
            after_session(&session(true, true), None, false, Duration::ZERO, false),
            AfterSession::End(End::UsbDisconnected, _)
        ));
    }

    #[test]
    fn a_session_that_ends_by_itself_says_why() {
        let none = Duration::ZERO;
        assert!(matches!(after_session(&session(false, true), None, true, none, false), AfterSession::End(End::Stopped, _)));
        let mut unlocked = session(false, false);
        unlocked.boot = Some(crate::avl62x1::BootInfo {
            sys_clk_hz: 1,
            fec_clk_hz: 1,
            mpeg_clk_hz: 1,
            srst: 0,
            ready_word: String::new(),
            ready_after_polls: 0,
            running_patch: String::new(),
            patch_commands: 0,
        });
        assert!(matches!(after_session(&unlocked, None, false, none, false), AfterSession::End(End::NoLock, _)));
        assert!(matches!(after_session(&session(false, false), None, false, none, false), AfterSession::End(End::TuneFailed, _)));
    }

    #[test]
    fn the_status_says_waiting_only_once_this_runs_fifo_exists() {
        let dir = std::env::temp_dir().join(format!("mediabox-dvb-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("live.ts");
        std::fs::write(&path, b"").ok();
        let _ = std::fs::remove_file(&path);
        // A FIFO a previous run left behind.
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        use std::os::unix::fs::MetadataExt;
        let stale = std::fs::metadata(&path).unwrap().ino();
        let log = Log::silent();
        let mut seen = None;
        let result = open_fifo(&path, Duration::from_millis(100), &log, || {
            seen = Some(std::fs::metadata(&path).unwrap().ino());
        });
        assert_eq!(result.err(), Some(End::PlayerNeverOpened));
        assert!(seen.is_some_and(|ino| ino != stale), "announced on the new FIFO, not the stale one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_writer_whose_reader_stopped_reading_still_finishes() {
        let dir = std::env::temp_dir().join(format!("mediabox-dvb-stall-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("live.ts");
        let reader = {
            let path = path.clone();
            std::thread::spawn(move || {
                // Opens, reads nothing, keeps the FIFO open: a wedged player.
                for _ in 0..100 {
                    if let Ok(file) = std::fs::File::open(&path) {
                        std::thread::sleep(Duration::from_secs(3));
                        drop(file);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        };
        let log = Log::silent();
        let file = open_fifo(&path, Duration::from_secs(5), &log, || {}).unwrap();
        let writer = Writer::start(Some(Sink::Fifo(file)), 64 << 20, Pressure::Drop, None);
        for _ in 0..64 {
            writer.send(vec![0x47; 188 * 1024]); // far more than a pipe holds
        }
        let started = Instant::now();
        writer.finish(false);
        assert!(started.elapsed() < Duration::from_millis(500), "finish took {:?}", started.elapsed());
        reader.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_warm_writer_hands_the_stream_to_the_next_reader() {
        use std::io::Read;
        let dir = std::env::temp_dir().join(format!("mediabox-dvb-warm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("live.ts");
        // One reader at a time, each taking a few packets and going, as a
        // player does on a channel change.
        let readers = {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut got = Vec::new();
                for _ in 0..2 {
                    let mut file = loop {
                        if let Ok(file) = std::fs::File::open(&path) {
                            break file;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    };
                    let mut buffer = [0u8; 188];
                    file.read_exact(&mut buffer).unwrap();
                    got.push(buffer[1]);
                    drop(file);
                    std::thread::sleep(Duration::from_millis(50));
                }
                got
            })
        };
        let log = Log::silent();
        let file = open_fifo(&path, Duration::from_secs(5), &log, || {}).unwrap();
        let writer = Writer::start(Some(Sink::Fifo(file)), 1 << 20, Pressure::Drop, Some(path.clone()));
        let started = Instant::now();
        while !readers.is_finished() && started.elapsed() < Duration::from_secs(5) {
            let mut packet = vec![0x47u8; 188];
            packet[1] = 7;
            assert!(writer.send(packet), "a reader going does not end a warm writer");
            std::thread::sleep(Duration::from_millis(1));
        }
        let got = readers.join().unwrap();
        assert_eq!(got, [7, 7], "both readers were written to");
        assert!(writer.stats().readers >= 1);
        writer.finish(false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_arrives_before_the_first_reader_waits_for_it() {
        use std::io::Read;
        let dir = std::env::temp_dir().join(format!("mediabox-dvb-early-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("live.ts");
        make_fifo(&path, &Log::silent()).unwrap();
        let writer = Writer::start(None, 1 << 20, Pressure::Drop, Some(path.clone()));
        // The stream starts before anybody reads it: the tune does not wait.
        let mut first = vec![0x47u8; 188];
        first[1] = 9;
        assert!(writer.send(first));
        let reader = {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut file = std::fs::File::open(&path).unwrap();
                let mut buffer = [0u8; 188];
                file.read_exact(&mut buffer).unwrap();
                buffer[1]
            })
        };
        let started = Instant::now();
        while !reader.is_finished() && started.elapsed() < Duration::from_secs(5) {
            writer.send(vec![0x47u8; 188]);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(reader.join().unwrap(), 9, "the first reader gets the stream from its start");
        writer.finish(false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_control_planes_requests_read_as_it_writes_them() {
        let play: Select = serde_json::from_str(
            r#"{"token":"1-1","want":{"kind":"play","service_id":10801,"transponder":{"frequency_khz":12188000,"symbol_rate_ksps":27500}}}"#,
        )
        .unwrap();
        assert_eq!(
            play.want,
            Want::Play {
                service_id: 10801,
                transponder: Some(Transponder {
                    frequency_khz: 12_188_000,
                    symbol_rate_ksps: 27_500,
                    polarisation: Polarisation::Vertical,
                }),
                new_fifo: None,
            }
        );
        let standby: Select = serde_json::from_str(r#"{"token":"1-1","want":{"kind":"standby"}}"#).unwrap();
        assert_eq!(standby.want, Want::Standby);
        assert!(serde_json::from_str::<Select>(r#"{"token":"1-1","service_id":10801}"#).is_err(), "the old form is not guessed at");
    }

    #[test]
    fn a_redirected_writer_ends_the_old_file_and_starts_the_new_one() {
        use std::io::Read;
        let dir = std::env::temp_dir().join(format!("mediabox-dvb-handover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (old, new) = (dir.join("live.ts"), dir.join("live-1.ts"));
        make_fifo(&old, &Log::silent()).unwrap();
        let writer = Writer::start(None, 1 << 20, Pressure::Drop, Some(old.clone()));
        let packet = |mark: u8| {
            let mut p = vec![0x47u8; 188];
            p[1] = mark;
            p
        };
        let first = {
            let old = old.clone();
            std::thread::spawn(move || {
                let mut all = Vec::new();
                std::fs::File::open(&old).unwrap().read_to_end(&mut all).unwrap();
                all
            })
        };
        writer.send(packet(1));
        std::thread::sleep(Duration::from_millis(100));
        make_fifo(&new, &Log::silent()).unwrap();
        writer.redirect(new.clone());
        writer.send(packet(2)); // the new channel's start
        let seen = first.join().unwrap();
        assert!(!seen.is_empty() && seen.chunks(188).all(|p| p[1] == 1), "the old file ended with the old service");
        let next = std::thread::spawn(move || {
            let mut buffer = [0u8; 188];
            std::fs::File::open(&new).unwrap().read_exact(&mut buffer).unwrap();
            buffer[1]
        });
        let started = Instant::now();
        while !next.is_finished() && started.elapsed() < Duration::from_secs(5) {
            writer.send(packet(3));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(next.join().unwrap(), 2, "the new file starts at the new service's start");
        writer.finish(false);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

