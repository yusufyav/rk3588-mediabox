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
use crate::ts::{
    Admission, Analyser, Expected, Gate, Gated, Integrity, LeftMux, Quarantine, RapCache,
    Selection, Selector, StaleClock,
};
use crate::tune::{self, BAND_SPLIT_KHZ, LnbControl, Streamed, TuneParams, TuneResult};
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
    /// The channel's transport stream, where the channel list knows it.
    pub expect: Option<Expected>,
    /// Where a CC error on the service played leaves its evidence: the raw
    /// stream around it. None: nowhere.
    pub evidence_dir: Option<PathBuf>,
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
    /// The receiver is on a link too slow for a transport stream
    /// (`usblink`): full speed, 12 Mbit/s, where 480 is needed.
    SlowLink,
    /// Locked, and the receiver sends no transport stream.
    NoTransport,
    NoLock,
    TuneFailed,
    ServiceNotFound,
    /// Transport stream packets, and no whole PAT in them: no programme
    /// can be found at all (not a service missing from a multiplex).
    NoPat,
    /// PAT packets whose sections never pass their CRC.
    PsiCorrupt,
    /// After a retune (or out of standby), only what the receiver sent
    /// before it: the multiplex left, or the same one from before the stop,
    /// through a quarantine and one more tune.
    StaleTransport,
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
    /// The last stream start's quarantine (`ts::Gate`).
    pub quarantine: Option<Quarantine>,
    /// Packets lost in the admitted transport stream: on any PID of the
    /// multiplex, and on the PIDs of the service played.
    pub continuity: Continuity,
    pub rss_kib: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Continuity {
    pub mux_cc_errors: u64,
    pub selected_service_cc_errors: u64,
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
                quarantine: None,
                continuity: Continuity::default(),
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
                // The first write to a reader just opened, for the timeline.
                let mut first_write = false;
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
                                mediabox_core::timeline!(
                                    "fifo-open",
                                    "path={} pending_bytes={pending_bytes}",
                                    path.display()
                                );
                                first_write = true;
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
                                            if std::mem::take(&mut first_write) {
                                                mediabox_core::timeline!(
                                                    "fifo-write",
                                                    "bytes={} kind=pending",
                                                    waiting.len()
                                                );
                                            }
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
                        Ok(true) => {
                            if std::mem::take(&mut first_write) {
                                mediabox_core::timeline!(
                                    "fifo-write",
                                    "bytes={} kind=queued",
                                    chunk.len()
                                );
                            }
                            shared.stats.lock().unwrap_or_else(|p| p.into_inner()).bytes +=
                                chunk.len() as u64
                        }
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
    /// A fresh lock, at this offset from the nominal IF, not yet known to be
    /// the transponder's: it is once its PAT names the service.
    unconfirmed_offset: Option<i32>,
    /// The select.json modification read last, for the tuner to know a
    /// newer one has come while it searches (`tune::run`, `superseded`).
    select_seen: std::sync::Arc<std::sync::Mutex<Option<std::time::SystemTime>>>,
    /// A new stream's quarantine, until its transport stream is admitted;
    /// stream starts so far, and what came of the last one.
    gate: Option<Gate>,
    /// The multiplex the last retune left; the channel's transport stream
    /// where known; the programme clock when the stream last stopped.
    left_mux: Option<LeftMux>,
    expect: Option<Expected>,
    stopped_clock: Option<StaleClock>,
    /// Tunes made by a quarantine since a stream was last admitted or a
    /// channel asked for: at most one.
    gate_retunes: u32,
    /// Transfers fed since the stream started; the raw transfers of the
    /// last seconds, and evidence being gathered (`Evidence`).
    transfers: u64,
    recent: VecDeque<(Instant, Vec<u8>)>,
    recent_bytes: usize,
    evidence: Option<Evidence>,
    evidence_dir: Option<PathBuf>,
    evidence_written: u32,
    selected_logged: usize,
    /// The programme PIDs of the multiplex left by the last retune.
    left_pids: std::collections::BTreeSet<u16>,
    /// The analyser's first CC errors already written to the log.
    cc_logged: usize,
    stream_starts: u32,
    quarantine: Option<Quarantine>,
    /// After a loss, a PAT of another transport stream, twice: retune.
    other_tsid: Option<u16>,
    /// TS packets read from the receiver, held back or not: that the
    /// receiver sends at all (`TS_DEAD_AFTER`).
    packets_read: u64,
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
    /// The timeline's firsts since the last selection (`SEEN_*`).
    seen: u8,
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
    /// The sections of the table had and expected, as last written.
    tkgs_reported: (usize, usize),
    /// A manual scan: the multiplex is read for what it carries, no service
    /// is looked for and no reader waited for.
    scanning: bool,
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
    /// The control plane's request this selection is for, on the timeline.
    #[serde(default)]
    rid: Option<u64>,
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
        /// The channel's transport stream, where the channel list knows it.
        #[serde(default)]
        original_network_id: Option<u16>,
        #[serde(default)]
        transport_stream_id: Option<u16>,
    },
    /// Nobody is watching: the USB stream stops, the demodulator stays
    /// booted and locked for the next `Play`.
    Standby,
    /// Read the complete TKGS table; the result is written beside status.
    Tkgs { transponder: Transponder },
    /// The channel setup's manual scan: stream `transponder` for its PAT and
    /// SDT, no service wanted; what it carries is `guide.json`.
    Scan { transponder: Transponder },
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
    pub original_network_id: Option<u16>,
    /// The NIT's carriers: what a network scan goes on to.
    pub network: Vec<GuideCarrier<'a>>,
    pub nit_packets: u64,
    pub nit_tables: Vec<u8>,
    /// Every section of the SDT actual, and of the NIT actual, is in: a
    /// scan has all this multiplex says of its services and network.
    pub sdt_complete: bool,
    pub nit_complete: bool,
    pub services: Vec<GuideService<'a>>,
    pub events: Vec<GuideEvents<'a>>,
}

#[derive(Serialize)]
pub struct GuideCarrier<'a> {
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub frequency_khz: u32,
    pub symbol_rate_ksps: u32,
    pub polarisation: &'a str,
    pub dvb_s2: bool,
    /// Orbital position in tenths of a degree, east or west.
    pub orbital_position_tenths: u16,
    pub east: bool,
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

    /// The next `write` writes, whatever the analyser's changes.
    fn force(&self) {
        self.written.set((u64::MAX, None));
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
        original_network_id: analyser.original_network_id,
        nit_packets: analyser.packets_on(crate::ts::PID_NIT),
        nit_tables: analyser.nit_tables.iter().copied().collect(),
        sdt_complete: analyser.sdt_sections.complete(),
        nit_complete: analyser.nit_sections.complete(),
        network: analyser
            .network
            .iter()
            .map(|(&transport_stream_id, (original_network_id, d))| GuideCarrier {
                transport_stream_id,
                original_network_id: *original_network_id,
                frequency_khz: d.frequency_khz,
                symbol_rate_ksps: d.symbol_rate_ksps,
                polarisation: &d.polarisation,
                dvb_s2: d.dvb_s2,
                orbital_position_tenths: d.orbital_position_tenths,
                east: d.east,
            })
            .collect(),
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
    fn feed(&mut self, chunk: &[u8], reporter: &mut Reporter, log: &Log) -> bool {
        self.transfers += 1;
        self.keep_recent(chunk, log);
        let mut admitted = None;
        if let Some(gate) = self.gate.as_mut() {
            let held = gate.stats.packets;
            let verdict = gate.feed(chunk);
            self.packets_read += gate.stats.packets.saturating_sub(held);
            match verdict {
                Gated::Waiting => return true,
                Gated::Other(tsid) => {
                    self.other_tsid = Some(tsid);
                    return true;
                }
                Gated::Admitted(rest) => {
                    self.close_gate("admitted", log);
                    admitted = Some(rest);
                }
            }
        }
        let chunk = admitted.as_deref().unwrap_or(chunk);
        let analysed = self.analyser.integrity().packets;
        let traced = self.seen != SEEN_ALL && mediabox_core::timeline::enabled();
        let video_pid = if traced && self.seen & SEEN_RAP == 0 {
            self.selector
                .selection()
                .streams
                .iter()
                .find(|s| s.kind == "video")
                .map(|s| s.pid)
        } else {
            None
        };
        let mut rap_seen = false;
        let mut rap_check = |packet: &[u8]| {
            if let Some(video) = video_pid
                && !rap_seen
                && packet[3] & 0x20 != 0
                && packet[4] > 0
                && packet[5] & 0x40 != 0
                && crate::ts::pid_of(packet) == video
            {
                rap_seen = true;
            }
        };
        let selector = &mut self.selector;
        let scratch = &mut self.scratch;
        let tkgs = &mut self.tkgs;
        match self.rap.as_mut() {
            Some(rap) => {
                rap.learn(&self.analyser.pat, &self.analyser.pmts);
                self.analyser.feed(chunk, |packet| {
                    tkgs.packet(packet);
                    rap.packet(packet);
                    rap_check(packet);
                    selector.packet(packet, scratch);
                });
            }
            None => self.analyser.feed(chunk, |packet| {
                tkgs.packet(packet);
                rap_check(packet);
                selector.packet(packet, scratch);
            }),
        }
        if traced {
            self.trace_firsts(rap_seen);
        }
        if self.tkgs_started.is_some() && !self.tkgs_done && let Some(table) = self.tkgs.table() {
            self.write_tkgs(serde_json::json!({
                "token": self.tkgs_token, "state": "complete",
                "transponders": table.transponders.len(), "channels": table.channel_count(), "table": table,
            }));
            self.tkgs_done = true;
        } else if self.tkgs_started.is_some() && !self.tkgs_done && self.tkgs.progress() != self.tkgs_reported {
            // How far the table has come, for the channel setup's progress.
            self.tkgs_reported = self.tkgs.progress();
            let (sections, expected) = self.tkgs_reported;
            self.write_tkgs(serde_json::json!({
                "token": self.tkgs_token, "state": "reading", "sections": sections, "expected": expected,
            }));
        }
        self.packets_read += self.analyser.integrity().packets.saturating_sub(analysed);
        self.log_cc_errors(log);
        self.selected_cc(reporter, log);
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

    /// A stream starts (after a tune, or out of standby): held back until
    /// its transport stream is known (`ts::Gate`). Not the raw multiplex,
    /// which is written unchanged by contract, for diagnostics.
    fn quarantine_stream(&mut self) {
        if self.raw {
            return;
        }
        // Evidence is of one stream: what comes after this is another's.
        if let Some(mut evidence) = self.evidence.take() {
            evidence
                .note
                .push_str("\n(cut short: the stream ended before the time after the error was in)");
            evidence.write();
        }
        self.stream_starts += 1;
        self.transfers = 0;
        self.other_tsid = None;
        let service_id = (self.tkgs_started.is_none() && !self.scanning).then_some(self.service_id);
        let left = self.left_tsid.and(self.left_mux.clone());
        // The same multiplex again (out of standby, a relock, the first
        // stream after a stop): what carries the clock of the stop is stale.
        let stale_clock = if left.is_none() {
            self.stopped_clock.take()
        } else {
            None
        };
        let rule = Admission {
            service_id,
            left,
            expect: service_id.and(self.expect),
            want_tsid: self.relock_tsid,
            stale_clock,
        };
        let left_pids = if self.left_tsid.is_some() {
            self.left_pids.clone()
        } else {
            Default::default()
        };
        self.gate = Some(Gate::new(rule, self.stream_starts, left_pids));
    }

    /// The stream stops: the programme clock then, for the next start of the
    /// same multiplex to tell the bridge's stale tail by.
    fn stream_stopped(&mut self) {
        let selection = self.selector.selection();
        self.stopped_clock = match (selection.pcr_pid, self.selector.last_pcr()) {
            (Some(pid), Some(pcr)) => Some(StaleClock {
                pid,
                pcr,
                at: Instant::now(),
            }),
            _ => None,
        };
    }

    /// The quarantine ends: one line for it. Admitted, the new transport
    /// stream begins here, with nothing of the one before -- tables,
    /// counters, access points.
    fn close_gate(&mut self, outcome: &str, log: &Log) {
        let Some(gate) = self.gate.take() else { return };
        let q = gate.stats;
        log.line(format!(
            "QUARANTINE {outcome}: start={} service={:?} left={:?} want={:?} admitted={:?} reason={:?} \
             expect={:?} onid={:?} held transfers={} packets={} bytes={} left_pid_packets={} pat_packets={} pats={} \
             crc_errors={} old_pats={} other_pats={} stale_pats={} stale_clocks={} clock_ahead_ms={:?} stopped_ms={:?} after_ms={:?}",
            q.generation,
            q.service_id,
            q.left_tsid,
            q.want_tsid,
            q.admitted_tsid,
            q.reason,
            q.expect_tsid,
            q.admitted_onid,
            q.transfers,
            q.packets,
            q.bytes,
            q.left_pid_packets,
            q.pat_packets,
            q.pat_sections,
            q.crc_errors,
            q.old_pats,
            q.other_pats,
            q.stale_pats,
            q.stale_clocks,
            q.clock_ahead_ms,
            q.stopped_ms,
            q.admit_ms,
        ));
        if q.reason.is_some() {
            self.gate_retunes = 0;
            self.selector.new_epoch();
            self.analyser = Analyser::new();
            if let Some(rap) = self.rap.as_mut() {
                *rap = RapCache::default();
            }
            self.left_tsid = None;
        }
        self.quarantine = Some(q);
    }

    /// The admitted stream's first CC errors, each once, with whether the
    /// PID is the service's and whether the multiplex left carried it.
    fn log_cc_errors(&mut self, log: &Log) {
        let first = &self.analyser.cc_first;
        if first.len() < self.cc_logged {
            self.cc_logged = 0; // another stream
        }
        for e in &first[self.cc_logged..] {
            log.line(format!(
                "CC ERROR start={} packet={} pid=0x{:04X} prev={} expected={} got={} pid_packets={} selected={} left_pid={}",
                self.stream_starts,
                e.packet,
                e.pid,
                e.previous,
                (e.previous + 1) & 0xF,
                e.got,
                e.pid_packets,
                self.selector.selection().pids.contains(&e.pid),
                self.left_pids.contains(&e.pid),
            ));
        }
        self.cc_logged = first.len();
    }

    /// The raw transfers of the last `EVIDENCE_AROUND`, and the evidence
    /// being gathered: once its time after the error is in, written out.
    fn keep_recent(&mut self, chunk: &[u8], log: &Log) {
        if self.evidence_dir.is_none() {
            return;
        }
        let now = Instant::now();
        self.recent.push_back((now, chunk.to_vec()));
        self.recent_bytes += chunk.len();
        while let Some((at, front)) = self.recent.front()
            && (now.duration_since(*at) > EVIDENCE_AROUND
                || self.recent_bytes > EVIDENCE_MAX_BYTES / 2)
        {
            self.recent_bytes -= front.len();
            self.recent.pop_front();
        }
        if let Some(evidence) = self.evidence.as_mut() {
            if evidence.data.len() + chunk.len() <= EVIDENCE_MAX_BYTES {
                evidence.data.extend_from_slice(chunk);
            }
            if now >= evidence.due {
                let evidence = self.evidence.take().unwrap();
                log.line(format!(
                    "EVIDENCE: {} ({} bytes)",
                    evidence.path.display(),
                    evidence.data.len()
                ));
                evidence.write();
            }
        }
    }

    /// A CC error on the service played: said with where it came, and the
    /// raw stream around it kept (`Options::evidence_dir`), at most
    /// `EVIDENCE_FILES` times a session.
    fn selected_cc(&mut self, reporter: &Reporter, log: &Log) {
        let events = self.selector.cc_events();
        if events.len() < self.selected_logged {
            self.selected_logged = 0; // another selection
        }
        if events.len() == self.selected_logged {
            return;
        }
        let usb = reporter.status.usb.as_ref();
        let since_admission = self.analyser.integrity().packets;
        let mut lines = Vec::new();
        for e in &events[self.selected_logged..] {
            lines.push(format!(
                "SELECTED CC ERROR start={} transfer={} service_packet={} stream_packet={} pid=0x{:04X} prev={} expected={} got={} \
                 usb completed={:?} short={:?} timeouts={:?} overflow={:?} quarantine_after_ms={:?}",
                self.stream_starts,
                self.transfers,
                e.packet,
                since_admission,
                e.pid,
                e.previous,
                (e.previous + 1) & 0xF,
                e.got,
                usb.map(|u| u.completed),
                usb.map(|u| u.short_reads),
                usb.map(|u| u.timeouts),
                usb.map(|u| u.overflow_bytes),
                self.quarantine.as_ref().and_then(|q| q.admit_ms),
            ));
        }
        self.selected_logged = events.len();
        for line in &lines {
            log.line(line);
        }
        if self.evidence.is_none()
            && self.evidence_written < EVIDENCE_FILES
            && let Some(dir) = &self.evidence_dir
        {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let path = dir.join(format!(
                "selected-cc-{stamp}-start{}.ts",
                self.stream_starts
            ));
            let mut data = Vec::with_capacity(self.recent_bytes * 2);
            for (_, chunk) in &self.recent {
                data.extend_from_slice(chunk);
            }
            self.evidence_written += 1;
            self.evidence = Some(Evidence {
                path,
                note: lines.join("\n"),
                data,
                due: Instant::now() + EVIDENCE_AROUND,
            });
        }
    }

    /// A scan or TKGS read with no PAT in its time (`PAT_ABSENT`): what was
    /// held, which nothing identified, is dropped, and the transponder is
    /// read as it comes; its tables repeat for the read to gather.
    fn open_gate(&mut self, log: &Log) {
        if let Some(gate) = self.gate.as_mut() {
            gate.open_dropping_held();
        }
        self.close_gate("opened", log);
    }

    /// The selected service's first PAT, PMT, PCR and access point since
    /// it was selected, on the timeline.
    fn trace_firsts(&mut self, rap_seen: bool) {
        let mut first = |bit: u8, now: bool, event: &str| {
            if now && self.seen & bit == 0 {
                self.seen |= bit;
                mediabox_core::timeline!(event, "sid={}", self.service_id);
            }
        };
        first(SEEN_PAT, !self.analyser.pat.is_empty(), "pat");
        first(SEEN_PMT, self.selector.ready(), "pmt");
        first(SEEN_PCR, self.selector.last_pcr().is_some(), "pcr");
        first(SEEN_RAP, rap_seen, "rap");
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
        self.tkgs_reported = (0, 0);
        self.write_tkgs(serde_json::json!({"token": self.tkgs_token, "state": "reading", "sections": 0, "expected": 0}));
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
        status.continuity = Continuity {
            mux_cc_errors: status.integrity.cc_errors,
            selected_service_cc_errors: status.selection.timing.cc_errors,
        };
        status.quarantine = self
            .gate
            .as_ref()
            .map(|gate| gate.stats.clone())
            .or_else(|| self.quarantine.clone());
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
        mediabox_core::timeline!("fifo-move", "path={}", new.display());
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
        mediabox_core::timeline!(
            "select",
            "rid={} want={:?}",
            wanted.rid.unwrap_or(0),
            format!("{:?}", wanted.want).replace(' ', "")
        );
        let (service_id, transponder, tkgs, scan) = match wanted.want {
            Want::Standby => {
                log.line("SELECT standby");
                self.scanning = false;
                return Order::Standby;
            }
            Want::Play {
                service_id,
                transponder,
                new_fifo,
                original_network_id,
                transport_stream_id,
            } => {
                if let Some(name) = new_fifo {
                    self.move_output(&name, log);
                }
                self.expect = transport_stream_id.map(|tsid| Expected {
                    onid: original_network_id,
                    tsid,
                });
                self.writer.await_reader();
                (service_id, transponder, false, false)
            }
            Want::Tkgs { transponder } => (self.service_id, Some(transponder), true, false),
            Want::Scan { transponder } => (self.service_id, Some(transponder), false, true),
        };
        self.scanning = scan;
        if tkgs {
            log.line("SELECT TKGS table");
            self.begin_tkgs();
        }
        if scan {
            log.line("SELECT scan");
            // What the multiplex carries is said again at once, even if it
            // is the one tuned and nothing in it changes.
            self.guide.force();
        }
        let retune = transponder.and_then(|t| {
            let rf_khz = crate::tune::universal_if_khz(t.frequency_khz);
            let tuned = self.tuned.as_ref()?;
            let lnb = Some(LnbControl {
                vertical: t.polarisation == Polarisation::Vertical,
                high_band: t.frequency_khz >= BAND_SPLIT_KHZ,
                diseqc_port: tuned.lnb.and_then(|lnb| lnb.diseqc_port),
            });
            ((tuned.rf_khz, tuned.symbol_rate_ksps, tuned.lnb) != (rf_khz, t.symbol_rate_ksps, lnb)).then(|| TuneParams {
                rf_khz,
                symbol_rate_ksps: t.symbol_rate_ksps,
                lnb,
                lock_timeout_ms: tuned.lock_timeout_ms,
                search_offsets_khz: tuned.search_offsets_khz.clone(),
                first_offset_khz: None,
                wrong_offsets_khz: Vec::new(),
            })
        });
        if service_id != self.service_id || retune.is_some() {
            log.line(format!("SELECT service {} -> {service_id}", self.service_id));
            self.service_id = service_id;
            // Another channel: what was being waited for was the last one's.
            self.relock_tsid = None;
            self.gate_retunes = 0;
            self.retune_at = None;
            // A scan takes whatever the transponder is; it leaves nothing.
            self.left_tsid = retune.as_ref().filter(|_| !scan).and(self.selector.selection().transport_stream_id);
            self.selected_at = Instant::now();
            self.seen = 0;
            mediabox_core::timeline!(
                "selected",
                "sid={service_id} retune={}",
                u8::from(retune.is_some())
            );
            self.selector = Selector::new(service_id);
            self.writer.discard();
            if retune.is_some() {
                // Another multiplex: nothing of this one's tables or access
                // points is of use there.
                self.left_pids = self.analyser.programme_pids();
                self.left_mux = self.left_tsid.map(|tsid| LeftMux {
                    tsid,
                    onid: self.analyser.original_network_id,
                    programmes: self.analyser.pat.clone(),
                });
                self.analyser = Analyser::new();
                self.rap = self.rap.as_ref().map(|_| RapCache::default());
            } else if let Some(replay) = self.rap.as_ref().map(|rap| rap.tables(service_id)).filter(|r| !r.is_empty()) {
                // The new channel starts at its next access point, live: only
                // its tables are handed on, nothing from before the switch
                // (2026-11-01: from the last access point it started up to
                // 1.1 s behind the broadcast, and catching up threw pictures
                // and sound away).
                let mut out = Vec::with_capacity(replay.len());
                for packet in replay.chunks(crate::ts::PACKET) {
                    self.selector.packet(packet, &mut out);
                }
                log.line(format!("SELECT tables {} B; the picture starts at the next access point", out.len()));
                trace_replay("select", &out);
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

const SEEN_PAT: u8 = 1;
const SEEN_PMT: u8 = 2;
const SEEN_PCR: u8 = 4;
const SEEN_RAP: u8 = 8;
const SEEN_ALL: u8 = 15;

/// A start from the last access point, on the timeline: how much, and how
/// much of the broadcast it spans by its PCRs -- how old the access point
/// is against the live edge.
fn trace_replay(at: &str, data: &[u8]) {
    if !mediabox_core::timeline::enabled() {
        return;
    }
    let pcrs: Vec<u64> = data
        .as_chunks::<{ crate::ts::PACKET }>()
        .0
        .iter()
        .filter_map(|p| crate::ts::pcr_of(p))
        .collect();
    let span = match (pcrs.first(), pcrs.last()) {
        (Some(&a), Some(&b)) => ((b + (1 << 33) * 300 - a) % ((1 << 33) * 300)) as f64 / 27_000.0,
        _ => -1.0,
    };
    mediabox_core::timeline!(
        "replay",
        "at={at} bytes={} pcrs={} pcr_span_ms={span:.1}",
        data.len(),
        pcrs.len()
    );
}

/// Whether the receiver can be used: present (waited for 3 s, as one just
/// plugged in enumerates) and at high speed (`usblink`). None: it can.
fn receiver_link(log: &Log) -> Option<(End, String)> {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match crate::usblink::kanky_speed() {
            Some((_, speed)) if speed >= crate::usblink::NEEDED_MBPS => return None,
            Some((name, speed)) => {
                log.line(format!(
                    "USB LINK: the receiver ({name}) is at {speed} Mbit/s; a transport stream needs {}",
                    crate::usblink::NEEDED_MBPS
                ));
                return Some((End::SlowLink, format!("{speed} Mbit/s")));
            }
            None if Instant::now() >= until => {
                log.line("USB LINK: no receiver on the bus");
                return Some((End::DeviceNotFound, OpenError::NotFound.to_string()));
            }
            None => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// Run the whole service; the final status. The exit code follows `end`.
pub fn run(options: Options, log: &Log) -> Status {
    let mut reporter = Reporter::new(options.status.clone(), &options);
    reporter.write();
    // The receiver there, and on a link that can carry a stream, before the
    // output exists: the control plane starts the player once the FIFO is
    // there, so an ending found only after it (on opening the USB device)
    // reached the viewer 6 s late, after a display mode change for a film
    // that never came (2026-11-01 19:14).
    if matches!(options.source, Source::Satellite { .. })
        && let Some((end, message)) = receiver_link(log)
    {
        return finish(reporter, None, end, Some(message), log);
    }
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
    let writer = Writer::start(
        sink,
        options.output_queue_bytes,
        pressure,
        match (&options.output, options.hold_reader) {
            (Output::Fifo(path), Some(_)) => Some(path.clone()),
            _ => None,
        },
    );
    let mut pipeline = new_pipeline(&options, writer);
    let (end, message) = match &options.source {
        Source::Recording(path) => {
            play_recording(path, &options, &mut pipeline, &mut reporter, log)
        }
        Source::Satellite {
            params,
            firmware_dir,
            access,
        } => play_satellite(
            params,
            firmware_dir,
            access,
            &options,
            &mut pipeline,
            &mut reporter,
            log,
        ),
    };
    finish(reporter, Some(pipeline), end, message, log)
}

fn new_pipeline(options: &Options, writer: Writer) -> Pipeline {
    Pipeline {
        gate: None,
        left_mux: None,
        expect: options.expect,
        stopped_clock: None,
        gate_retunes: 0,
        transfers: 0,
        recent: VecDeque::new(),
        recent_bytes: 0,
        evidence: None,
        evidence_dir: options.evidence_dir.clone(),
        evidence_written: 0,
        selected_logged: 0,
        left_pids: Default::default(),
        cc_logged: 0,
        stream_starts: 0,
        quarantine: None,
        other_tsid: None,
        packets_read: 0,
        analyser: Analyser::new(),
        selector: Selector::new(options.service_id),
        writer,
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
        seen: 0,
        tuned: match &options.source {
            Source::Satellite { params, .. } => Some(params.clone()),
            Source::Recording(_) => None,
        },
        standby_first: options.start_standby,
        relock_tsid: None,
        retune_at: None,
        unconfirmed_offset: None,
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
        tkgs_reported: (0, 0),
        scanning: false,
    }
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
    (!options.raw
        && pipeline.gate.is_none()
        && pipeline.tkgs_started.is_none()
        && !pipeline.scanning
        && !pipeline.selector.ready()
        && since.elapsed() > options.service_timeout)
        .then(|| {
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
        if !pipeline.feed(&buffer[..n], reporter, log) {
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

/// Evidence of a CC error on the service played: the raw transfers from
/// `EVIDENCE_AROUND` before it to as long after, and what the log said.
struct Evidence {
    path: PathBuf,
    note: String,
    data: Vec<u8>,
    due: Instant,
}

const EVIDENCE_AROUND: Duration = Duration::from_secs(2);
const EVIDENCE_MAX_BYTES: usize = 64 << 20;
const EVIDENCE_FILES: u32 = 4;

impl Evidence {
    /// Off the stream's thread: the stream does not wait for the disk.
    fn write(self) {
        std::thread::spawn(move || {
            if let Some(dir) = self.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&self.path, &self.data);
            let _ = std::fs::write(self.path.with_extension("txt"), self.note + "\n");
        });
    }
}

/// A scan's or TKGS read's time for a PAT: one comes at least every 0.5 s
/// (ETSI TR 101 290 1.3a), so four such intervals without a stream in its
/// own right are none. Short enough that the read keeps its own time for
/// the tables: a scan waits 12 s for the NIT (sent every 10 s), TKGS 75 s.
const PAT_ABSENT: Duration = Duration::from_secs(2);

/// What a quarantine that has run its time comes to.
#[derive(Debug, PartialEq, Eq)]
enum AtDeadline {
    Wait,
    /// Tune again, once: only the multiplex left came (the tuner has not
    /// moved, VDR sdt.c tsWrong), or only the stream from before the stop,
    /// or a relock found nothing of the channel's. Never before the time:
    /// a tail is no reason.
    Retune,
    /// A scan or a TKGS read takes what there is.
    Open,
    /// The session ends, and says which of these it was.
    End(End),
}

fn at_deadline(
    q: &Quarantine,
    rule: &Admission,
    age: Duration,
    budget: Duration,
    retunes: u32,
) -> AtDeadline {
    let reading = rule.service_id.is_none() && rule.want_tsid.is_none();
    if age < if reading { PAT_ABSENT } else { budget } {
        return AtDeadline::Wait;
    }
    if reading {
        return AtDeadline::Open;
    }
    let stale = q.other_pats == 0 && (q.old_pats > 0 || q.stale_pats > 0);
    if rule.want_tsid.is_some() || stale {
        return match (retunes, rule.want_tsid) {
            (0, _) => AtDeadline::Retune,
            // A lost signal's own retunes go on while it is lost.
            (_, Some(_)) => AtDeadline::Wait,
            _ => AtDeadline::End(End::StaleTransport),
        };
    }
    if q.packets == 0 {
        // Nothing at all: the stream's own NoTransport says that.
        AtDeadline::Wait
    } else if q.pat_sections == 0 && q.pat_packets > 0 && q.crc_errors > 0 {
        AtDeadline::End(End::PsiCorrupt)
    } else if q.pat_sections == 0 {
        AtDeadline::End(End::NoPat)
    } else {
        AtDeadline::End(End::ServiceNotFound)
    }
}

/// What a quarantine's ending says, with its numbers.
fn quarantine_failure(end: End, q: &Quarantine, service_id: u16) -> String {
    match end {
        End::NoPat => format!(
            "{} TS paketi geldi, geçerli PAT yok ({} PAT paketi)",
            q.packets, q.pat_packets
        ),
        End::PsiCorrupt => format!("PAT {} kez CRC denetiminden geçmedi", q.crc_errors),
        End::StaleTransport => match q.left_tsid {
            Some(tsid) => format!("yalnız önceki yayının (TSID {tsid}) verisi geldi"),
            None => "yalnız durdurmadan önceki veri geldi".to_string(),
        },
        _ => format!("SID {service_id} bu yayının PAT'ında yok"),
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
        // A link too slow for the stream is not tuned at all: it locks and
        // delivers nothing (`usblink`). What the viewer is told is the link.
        if let Some((name, speed)) = crate::usblink::kanky_speed()
            && speed < crate::usblink::NEEDED_MBPS
        {
            log.line(format!(
                "USB LINK: the receiver ({name}) is at {speed} Mbit/s; a transport stream needs {} -- not tuned",
                crate::usblink::NEEDED_MBPS
            ));
            if let Some(error) = session.release() {
                log.line(format!("USB release: {error}"));
            }
            return (End::SlowLink, Some(format!("{speed} Mbit/s")));
        }
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
            pipeline.unconfirmed_offset = tuned.fec_locked().then_some(tuned.search_offset_khz).flatten();
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

const TS_DEAD_AFTER: Duration = Duration::from_secs(10);

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
                pipeline.quarantine_stream();
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
    // The first transfer of this lock, for the timeline.
    let mut usb_first = true;
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
    // TS packets read from the bridge, and when that last grew. Packets,
    // not completed transfers: a transfer can complete short with nothing in
    // it (one did on 2026-11-01, the receiver behind a USB hub, and the
    // stream that never came was reported as a service not found).
    let mut delivered = (0u64, Instant::now());
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
                pipeline.close_gate("stream stopped", log);
                pipeline.stream_stopped();
                pipeline.idle();
                reporter.state(State::Standby, log);
            }
            Order::Play => {
                if stream.is_none() {
                    match open(log) {
                        Ok(running) => {
                            pipeline.quarantine_stream();
                            stream = Some(running);
                        }
                        Err(error) => break Some((End::SourceFailed, Some(error))),
                    }
                    reporter.state(State::Streaming, log);
                }
            }
        }
        if pipeline.writer.wants_start() {
            let from = pipeline.rap.as_ref().map(|rap| rap.tables(pipeline.service_id)).unwrap_or_default();
            log.line(format!("READER: starts live, from the tables ({} B)", from.len()));
            trace_replay("reader", &from);
            pipeline.writer.start_from_access_point(from);
        }
        // Nobody has read for the hold: standby, as if asked.
        if stream.is_some()
            && pipeline.tkgs_started.is_none()
            && !pipeline.scanning
            && options.hold_reader.is_some_and(|hold| pipeline.writer.unread_for().is_some_and(|gone| gone > hold))
            && let Some(mut running) = stream.take()
        {
            let stats = running.stop();
            log.line(format!("STANDBY: no reader for {:?}; TS stream stopped: {stats:?}", options.hold_reader));
            pipeline.close_gate("stream stopped", log);
            pipeline.stream_stopped();
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
        // A new stream's quarantine: decided by the time a service has to
        // appear, never by the tail of the transport stream left.
        if let Some(gate) = pipeline.gate.as_ref() {
            let rule = gate.rule().clone();
            let q = gate.stats.clone();
            match at_deadline(
                &q,
                &rule,
                gate.age(),
                options.service_timeout,
                pipeline.gate_retunes,
            ) {
                AtDeadline::Wait => {}
                AtDeadline::Retune => {
                    if let Some(tuned) = pipeline.tuned.clone() {
                        log.line(format!(
                            "RETUNE: no transport stream admitted in {:?} (left {:?}, wanted {:?}, old PATs {}, stale PATs {}); tuning again, once",
                            options.service_timeout, q.left_tsid, rule.want_tsid, q.old_pats, q.stale_pats
                        ));
                        pipeline.gate_retunes += 1;
                        pipeline.close_gate("timed out", log);
                        retune = Some(tuned);
                        relock = true;
                        break None;
                    }
                }
                AtDeadline::Open => pipeline.open_gate(log),
                AtDeadline::End(end) if reporter.status.state != State::NoSignal => {
                    let message = quarantine_failure(end, &q, pipeline.service_id);
                    pipeline.close_gate("timed out", log);
                    break Some((end, Some(message)));
                }
                AtDeadline::End(_) => {}
            }
        }
        // Found again: only the transport stream the channel was on is let
        // in; another one, twice, is tuned away from -- once; then the lost
        // signal's own retunes carry on.
        if let Some(got) = pipeline.other_tsid.take()
            && pipeline.gate_retunes == 0
            && let Some(tuned) = pipeline.tuned.clone()
        {
            log.line(format!(
                "RELOCK: locked transport stream {got}, not {:?}; trying again",
                pipeline.relock_tsid
            ));
            pipeline.gate_retunes += 1;
            pipeline.close_gate("other transport stream", log);
            retune = Some(tuned);
            relock = true;
            break None;
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
        // A carrier the search found is the transponder only when its PAT
        // has the service; another (a neighbour ±10 MHz away: 12092 H locked
        // transport stream 41903 at +10 MHz for 41005, 2026-10-25) is
        // searched on from, the offset left out, as VDR tunes away from a
        // wrong transport stream (sdt.c, tsWrong). Not the TKGS read, which
        // wants no service, nor a relock, which checks its own stream.
        if let Some(offset) = pipeline.unconfirmed_offset
            && pipeline.relock_tsid.is_none()
            && pipeline.left_tsid.is_none()
            && pipeline.tkgs_started.is_none()
            && !pipeline.scanning
            && !options.raw
            && !pipeline.analyser.pat.is_empty()
        {
            pipeline.unconfirmed_offset = None;
            if !pipeline.analyser.pat.contains_key(&pipeline.service_id)
                && let Some(mut next) = pipeline.tuned.clone()
            {
                next.wrong_offsets_khz.push(offset);
                if !next.offsets().is_empty() {
                    log.line(format!(
                        "WRONG CARRIER: transport stream {:?} at {offset:+} kHz has no service {}; searching on",
                        pipeline.analyser.integrity().transport_stream_id,
                        pipeline.service_id
                    ));
                    pipeline.analyser = Analyser::new();
                    // Kept, so the next wrong one is added to it, not
                    // instead of it.
                    pipeline.tuned = Some(next.clone());
                    retune = Some(next);
                    break None;
                }
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
                if std::mem::take(&mut usb_first) {
                    mediabox_core::timeline!(
                        "usb-first",
                        "bytes={} sync={}",
                        chunk.len(),
                        u8::from(chunk.first() == Some(&0x47))
                    );
                }
                if !pipeline.feed(&chunk, reporter, log) {
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
        // not a service missing from the multiplex. Nor is it with no TS
        // packet ever read: that is a receiver sending no stream, which
        // ends as that below (both 10 s; this check came first and ended
        // the receiver "service not found" with 0 bytes read -- 2026-10-31,
        // twice, and 2026-11-01 behind a USB hub).
        if reporter.status.state != State::NoSignal
            && delivered.0 > 0
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
        // FEC locked and not one transfer for 10 s: the receiver sends no
        // stream. The session ends and says so. Its port's power is not cut:
        // that cycle (in place until 2026-11-01) ran twice that day on a
        // receiver that had come up at full speed and changed nothing -- the
        // same firmware, the same 12 Mbit/s link -- and the receiver is not
        // to be power-cycled by software at all.
        if pipeline.packets_read != delivered.0 {
            delivered = (pipeline.packets_read, Instant::now());
        } else if delivered.1.elapsed() >= TS_DEAD_AFTER
            && matches!(demod.lock_flags(bridge), Ok((_, fec)) if fec != 0)
        {
            let link = crate::usblink::kanky_speed().map_or_else(|| "?".to_string(), |(_, speed)| speed.to_string());
            log.line(format!("TS DEAD: locked, no transfer completed for {TS_DEAD_AFTER:?} (link {link} Mbit/s); the session ends"));
            break Some((End::NoTransport, Some(format!("{} sn veri yok", TS_DEAD_AFTER.as_secs()))));
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
                if !pipeline.feed(&chunk, reporter, log) {
                    break;
                }
            }
        }
        no_device |= stats.no_device > 0;
        log.line(format!("TS stream stopped: {stats:?}"));
        reporter.status.usb = Some(stats);
    }
    pipeline.close_gate("stream ended", log);
    pipeline.stream_stopped();
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

    // ------------------------------------------------------- transport gate

    /// What a test's pipeline writes, to read back once it has finished.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn options(service_id: u16, status: Option<PathBuf>) -> Options {
        Options {
            service_id,
            token: Some("t".into()),
            source: Source::Recording(PathBuf::new()),
            output: Output::Stdout,
            status,
            reader_timeout: Duration::from_secs(1),
            service_timeout: Duration::from_secs(10),
            usb_transfers: 8,
            usb_buffer_bytes: 153_600,
            usb_queue_bytes: 1 << 20,
            signal_every: None,
            reconnect: Duration::ZERO,
            raw: false,
            pace: false,
            output_queue_bytes: 64 << 20,
            hold_reader: Some(Duration::from_secs(30)),
            start_standby: false,
            expect: None,
            evidence_dir: None,
        }
    }

    fn harness(service_id: u16, status: Option<PathBuf>) -> (Pipeline, Reporter, Captured) {
        let options = options(service_id, status);
        let out = Captured::default();
        let writer = Writer::start(
            Some(Sink::Plain(Box::new(out.clone()))),
            64 << 20,
            Pressure::Block,
            None,
        );
        (
            new_pipeline(&options, writer),
            Reporter::new(None, &options),
            out,
        )
    }

    /// Everything written, once the writer is done.
    fn written(pipeline: Pipeline, out: &Captured) -> Vec<u8> {
        let Pipeline { writer, .. } = pipeline;
        writer.finish(true);
        out.0.lock().unwrap().clone()
    }

    fn ts(pid: u16, unit_start: bool, cc: u8, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![
            0x47,
            (if unit_start { 0x40 } else { 0 }) | (pid >> 8) as u8,
            pid as u8,
            0x10 | (cc & 0xF),
        ];
        p.extend_from_slice(payload);
        p.resize(crate::ts::PACKET, 0xFF);
        p
    }

    fn psi(pid: u16, cc: u8, table: u8, ext: u16, body: &[u8]) -> Vec<u8> {
        let length = 5 + body.len() + 4;
        let mut section = vec![table, 0xB0 | (length >> 8) as u8, length as u8];
        section.extend_from_slice(&ext.to_be_bytes());
        section.extend_from_slice(&[0xC1, 0, 0]);
        section.extend_from_slice(body);
        let crc = crate::ts::crc32_mpeg(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        let mut payload = vec![0];
        payload.extend_from_slice(&section);
        ts(pid, true, cc, &payload)
    }

    /// One transport stream: `tsid`, its service `sid` (PMT `pmt`, H.264
    /// video `es`), the packets of `rounds` PAT/PMT/video cycles.
    struct Mux {
        tsid: u16,
        sid: u16,
        pmt: u16,
        es: u16,
    }

    impl Mux {
        fn pat(&self, cc: u8) -> Vec<u8> {
            let [s0, s1] = self.sid.to_be_bytes();
            psi(
                0,
                cc,
                0x00,
                self.tsid,
                &[s0, s1, 0xE0 | (self.pmt >> 8) as u8, self.pmt as u8],
            )
        }
        fn pmt(&self, cc: u8) -> Vec<u8> {
            let es = [0xE0 | (self.es >> 8) as u8, self.es as u8];
            psi(
                self.pmt,
                cc,
                0x02,
                self.sid,
                &[es[0], es[1], 0xF0, 0, 0x1B, es[0], es[1], 0xF0, 0],
            )
        }
        fn es(&self, cc: u8) -> Vec<u8> {
            ts(self.es, false, cc, &[0xAB, self.tsid as u8])
        }
        /// What a retune away from this multiplex remembers of it.
        fn left(&self, onid: Option<u16>) -> crate::ts::LeftMux {
            crate::ts::LeftMux {
                tsid: self.tsid,
                onid,
                programmes: [(self.sid, self.pmt)].into(),
            }
        }
        /// Its SDT actual: the network `onid`, one service.
        fn sdt(&self, onid: u16, cc: u8) -> Vec<u8> {
            let [o0, o1] = onid.to_be_bytes();
            let [s0, s1] = self.sid.to_be_bytes();
            psi(
                0x11,
                cc,
                0x42,
                self.tsid,
                &[o0, o1, 0xFF, s0, s1, 0xFC, 0x80, 0x00],
            )
        }
        fn rounds(&self, rounds: u8) -> Vec<Vec<u8>> {
            let mut out = Vec::new();
            for r in 0..rounds {
                out.push(self.pat(r));
                out.push(self.pmt(r));
                for k in 0..6 {
                    out.push(self.es(r * 6 + k));
                }
            }
            out
        }
    }

    const OLD: Mux = Mux {
        tsid: 41402,
        sid: 10300,
        pmt: 3100,
        es: 3050,
    };
    const NEW: Mux = Mux {
        tsid: 42104,
        sid: 51616,
        pmt: 2616,
        es: 2610,
    };

    /// A retune from OLD to NEW: the selector already on NEW's service, the
    /// stream started, quarantined.
    fn retuned() -> (Pipeline, Reporter, Captured) {
        let (mut pipeline, reporter, out) = harness(NEW.sid, None);
        pipeline.left_tsid = Some(OLD.tsid);
        pipeline.left_mux = Some(OLD.left(None));
        pipeline.left_pids = [OLD.pmt, OLD.es].into();
        pipeline.quarantine_stream();
        (pipeline, reporter, out)
    }

    /// A transfer: these packets and null packets after them, as a real one
    /// has hundreds (alignment is found on five in a row).
    fn transfer(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut out = packets.concat();
        for _ in 0..8 {
            out.extend(ts(0x1FFF, false, 0, &[]));
        }
        out
    }

    fn pids(data: &[u8]) -> Vec<u16> {
        data.chunks(crate::ts::PACKET)
            .map(crate::ts::pid_of)
            .collect()
    }

    #[test]
    fn the_tail_of_the_transport_stream_left_reaches_nothing() {
        let (mut pipeline, mut reporter, out) = retuned();
        let log = Log::silent();
        // Its PATs, PMTs and video, in transfers, three PATs among them.
        for chunk in OLD.rounds(3).chunks(5) {
            assert!(pipeline.feed(&chunk.concat(), &mut reporter, &log));
        }
        let gate = pipeline.gate.as_ref().expect("still quarantined");
        assert_eq!(
            (
                gate.stats.old_pats,
                gate.stats.packets,
                gate.stats.admitted_tsid
            ),
            (3, 24, None)
        );
        assert_eq!(gate.stats.left_pid_packets, 21, "its PMT and video");
        // Not a retune, not a reason for one before the deadline; nothing analysed.
        assert_eq!(pipeline.other_tsid, None);
        assert_eq!(
            at_deadline(
                &gate.stats,
                gate.rule(),
                gate.age(),
                Duration::from_secs(10),
                0
            ),
            AtDeadline::Wait
        );
        assert_eq!(pipeline.analyser.integrity().packets, 0);
        assert_eq!(
            pipeline.packets_read, 24,
            "the receiver is sending: no NoTransport"
        );
        assert!(
            written(pipeline, &out).is_empty(),
            "not one byte to the player"
        );
    }

    #[test]
    fn the_stream_starts_at_the_new_pat_inside_the_transfer() {
        let (mut pipeline, mut reporter, out) = retuned();
        let log = Log::silent();
        // One transfer: a broken packet's end, the old stream with its PAT,
        // then the new PAT, PMT and video.
        let mut chunk = vec![0x12u8; 100];
        let old = OLD.rounds(1);
        let new = NEW.rounds(2);
        chunk.extend(old.concat());
        chunk.extend(new.concat());
        assert!(pipeline.feed(&chunk, &mut reporter, &log));
        assert!(pipeline.gate.is_none(), "admitted");
        let q = pipeline.quarantine.clone().unwrap();
        assert_eq!(
            (q.admitted_tsid, q.reason, q.old_pats),
            (Some(NEW.tsid), Some(crate::ts::Admitted::Service), 1)
        );
        assert_eq!(
            (q.packets, q.bytes),
            (
                old.len() as u64,
                100 + (old.len() * crate::ts::PACKET) as u64
            )
        );
        assert_eq!(q.left_pid_packets, old.len() as u64 - 1);
        assert_eq!(pipeline.left_tsid, None);
        // Analysed from the new PAT on: its packets and nothing else.
        let integrity = pipeline.analyser.integrity();
        assert_eq!(
            (
                integrity.packets,
                integrity.transport_stream_id,
                integrity.cc_errors
            ),
            (new.len() as u64, Some(NEW.tsid), 0)
        );
        let data = written(pipeline, &out);
        let got = pids(&data);
        assert_eq!(
            &got[..2],
            &[0, NEW.pmt],
            "the output starts with the new PAT, then its PMT"
        );
        assert!(
            got.iter().all(|pid| [0, NEW.pmt, NEW.es].contains(pid)),
            "{got:?}"
        );
        let video: Vec<&[u8]> = data
            .chunks(crate::ts::PACKET)
            .filter(|p| crate::ts::pid_of(p) == NEW.es)
            .collect();
        assert_eq!(video.len(), 12);
        assert!(
            data.chunks(crate::ts::PACKET)
                .all(|p| !old.iter().any(|o| o.as_slice() == p)),
            "no old packet"
        );
        assert!(
            !data.windows(2).any(|w| w == [0xAB, OLD.tsid as u8]),
            "no byte of the old video"
        );
    }

    #[test]
    fn the_left_pat_again_and_again_is_waited_through() {
        let (mut pipeline, mut reporter, out) = retuned();
        let log = Log::silent();
        for cc in 0..5 {
            pipeline.feed(&transfer(&[OLD.pat(cc), OLD.es(cc)]), &mut reporter, &log);
        }
        let gate = pipeline.gate.as_ref().unwrap();
        assert_eq!((gate.stats.old_pats, pipeline.other_tsid), (5, None));
        assert_eq!(
            at_deadline(
                &gate.stats,
                gate.rule(),
                Duration::from_secs(9),
                Duration::from_secs(10),
                0
            ),
            AtDeadline::Wait
        );
        // The new one comes.
        pipeline.feed(&NEW.rounds(1).concat(), &mut reporter, &log);
        assert!(pipeline.gate.is_none());
        assert_eq!(pids(&written(pipeline, &out))[0], 0);
    }

    #[test]
    fn a_new_carrier_without_the_service_is_searched_on_from_as_before() {
        let (mut pipeline, mut reporter, out) = retuned();
        let log = Log::silent();
        let wrong = Mux {
            tsid: 41903,
            sid: 41005,
            pmt: 1005,
            es: 1010,
        };
        pipeline.feed(
            &transfer(&[wrong.pat(0), wrong.pmt(0), wrong.es(0)]),
            &mut reporter,
            &log,
        );
        assert!(pipeline.gate.is_some(), "one PAT could still be a tail");
        pipeline.feed(
            &transfer(&[wrong.pat(1), wrong.pmt(1), wrong.es(1)]),
            &mut reporter,
            &log,
        );
        assert!(pipeline.gate.is_none());
        assert_eq!(
            pipeline.quarantine.as_ref().unwrap().reason,
            Some(crate::ts::Admitted::Repeated)
        );
        // What the wrong-carrier search reads: a PAT, without the service, no stream left pending.
        assert!(!pipeline.analyser.pat.is_empty() && !pipeline.analyser.pat.contains_key(&NEW.sid));
        assert_eq!(pipeline.left_tsid, None);
        let got = pids(&written(pipeline, &out));
        assert!(
            got.iter().all(|pid| *pid == 0),
            "nothing of the wrong service: {got:?}"
        );
    }

    #[test]
    fn a_relock_lets_in_only_the_channel_s_transport_stream() {
        let (mut pipeline, mut reporter, out) = harness(NEW.sid, None);
        let log = Log::silent();
        pipeline.relock_tsid = Some(NEW.tsid);
        pipeline.quarantine_stream();
        let other = Mux {
            tsid: 7,
            sid: 11701,
            pmt: 1701,
            es: 1702,
        };
        pipeline.feed(&transfer(&[other.pat(0), other.es(0)]), &mut reporter, &log);
        assert_eq!(pipeline.other_tsid, None);
        pipeline.feed(&transfer(&[other.pat(1), other.es(1)]), &mut reporter, &log);
        assert_eq!(
            pipeline.other_tsid,
            Some(7),
            "another stream, twice: tuned away from"
        );
        assert!(pipeline.gate.is_some());
        pipeline.other_tsid = None;
        pipeline.feed(&NEW.rounds(1).concat(), &mut reporter, &log);
        assert_eq!(
            pipeline.quarantine.as_ref().unwrap().reason,
            Some(crate::ts::Admitted::Relock)
        );
        let got = pids(&written(pipeline, &out));
        assert!(!got.contains(&other.es) && got.contains(&NEW.es));
    }

    #[test]
    fn a_service_of_the_same_transponder_is_no_new_stream() {
        let dir = std::env::temp_dir().join(format!("mediabox-dvb-same-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut pipeline, mut reporter, out) = harness(NEW.sid, Some(dir.join("status.json")));
        let log = Log::silent();
        let here = (12_458_000u32, 30_000u32);
        pipeline.tuned = Some(TuneParams {
            rf_khz: crate::tune::universal_if_khz(here.0),
            symbol_rate_ksps: here.1,
            lnb: Some(LnbControl {
                vertical: true,
                high_band: true,
                diseqc_port: None,
            }),
            lock_timeout_ms: None,
            search_offsets_khz: Vec::new(),
            first_offset_khz: None,
            wrong_offsets_khz: Vec::new(),
        });
        pipeline.quarantine_stream();
        pipeline.feed(&NEW.rounds(2).concat(), &mut reporter, &log);
        let (starts, packets) = (
            pipeline.stream_starts,
            pipeline.analyser.integrity().packets,
        );
        let select = |sid: u16, frequency: u32| {
            std::fs::write(
                dir.join("select.json"),
                format!(r#"{{"token":"t","want":{{"kind":"play","service_id":{sid},"transponder":{{"frequency_khz":{frequency},"symbol_rate_ksps":30000}}}}}}"#),
            )
            .unwrap();
        };
        let mut follow = |pipeline: &mut Pipeline, reporter: &mut Reporter| {
            let file = pipeline.select.as_mut().unwrap();
            file.checked = Instant::now() - Duration::from_secs(1);
            file.modified = None;
            pipeline.follow_select(reporter, &log)
        };
        select(51617, here.0);
        assert!(matches!(follow(&mut pipeline, &mut reporter), Order::Play));
        assert_eq!(pipeline.service_id, 51617);
        assert!(pipeline.gate.is_none(), "no quarantine");
        assert_eq!(
            (
                pipeline.stream_starts,
                pipeline.analyser.integrity().packets
            ),
            (starts, packets),
            "the same epoch"
        );
        let second = Mux {
            tsid: NEW.tsid,
            sid: 51617,
            pmt: 2617,
            es: 2611,
        };
        pipeline.feed(&transfer(&second.rounds(1)), &mut reporter, &log);
        // Another transponder is a retune: the stream is started anew (and quarantined then).
        select(10300, 12_207_000);
        assert!(matches!(
            follow(&mut pipeline, &mut reporter),
            Order::Retune(_)
        ));
        assert_eq!(pipeline.left_tsid, Some(NEW.tsid));
        let _ = written(pipeline, &out);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn after_the_pat_a_loss_is_a_cc_error_and_the_counter_rules_hold() {
        let (mut pipeline, mut reporter, out) = retuned();
        let log = Log::silent();
        pipeline.feed(&[OLD.es(3), OLD.es(9)].concat(), &mut reporter, &log);
        pipeline.feed(&NEW.rounds(1).concat(), &mut reporter, &log);
        assert_eq!(
            pipeline.analyser.integrity().cc_errors,
            0,
            "the tail's counters are not the new stream's"
        );
        // Adaptation only (no payload): the counter stays; a discontinuity sets a new base.
        let mut adaptation = ts(NEW.es, false, 9, &[]);
        adaptation[3] = 0x20 | 9;
        adaptation[4] = 183;
        adaptation[5] = 0;
        let mut announced = ts(NEW.es, false, 12, &[]);
        announced[3] = 0x30 | 12;
        announced[4] = 1;
        announced[5] = 0x80;
        pipeline.feed(
            &[NEW.es(6), adaptation, NEW.es(7), announced, NEW.es(13)].concat(),
            &mut reporter,
            &log,
        );
        assert_eq!(pipeline.analyser.integrity().cc_errors, 0);
        // A packet lost: an error.
        pipeline.feed(&[NEW.es(15)].concat(), &mut reporter, &log);
        assert_eq!(pipeline.analyser.integrity().cc_errors, 1);
        let _ = written(pipeline, &out);
    }

    fn quarantine(f: impl FnOnce(&mut Quarantine)) -> Quarantine {
        let mut q = Quarantine {
            packets: 5000,
            pat_packets: 40,
            pat_sections: 40,
            ..Quarantine::default()
        };
        f(&mut q);
        q
    }

    #[test]
    fn a_quarantine_ends_in_time_and_says_which_failure_it_was() {
        let ten = Duration::from_secs(10);
        let play = Admission {
            service_id: Some(NEW.sid),
            ..Admission::default()
        };
        let at = |q: &Quarantine, rule: &Admission, age: Duration, retunes: u32| {
            at_deadline(q, rule, age, ten, retunes)
        };
        // Before the time: nothing, whatever came.
        assert_eq!(
            at(
                &quarantine(|q| q.old_pats = 9),
                &play,
                Duration::from_secs(9),
                0
            ),
            AtDeadline::Wait
        );
        // No PAT at all; PAT packets that never pass their CRC; PATs without the service.
        let none = quarantine(|q| (q.pat_packets, q.pat_sections) = (0, 0));
        assert_eq!(at(&none, &play, ten, 0), AtDeadline::End(End::NoPat));
        let corrupt = quarantine(|q| (q.pat_sections, q.crc_errors) = (0, 37));
        assert_eq!(
            at(&corrupt, &play, ten, 0),
            AtDeadline::End(End::PsiCorrupt)
        );
        let incomplete = quarantine(|q| q.pat_sections = 0);
        assert_eq!(at(&incomplete, &play, ten, 0), AtDeadline::End(End::NoPat));
        let other = quarantine(|q| q.other_pats = 40);
        assert_eq!(
            at(&other, &play, ten, 0),
            AtDeadline::End(End::ServiceNotFound)
        );
        // No packet: NoTransport is the stream's to say, not the quarantine's.
        assert_eq!(
            at(&quarantine(|q| q.packets = 0), &play, ten, 0),
            AtDeadline::Wait
        );
        // Only the multiplex left (or the stream from before a stop): one
        // more tune, then said as that -- never a loop.
        let left = Admission {
            left: Some(OLD.left(None)),
            ..play.clone()
        };
        let only_left = quarantine(|q| q.old_pats = 40);
        assert_eq!(at(&only_left, &left, ten, 0), AtDeadline::Retune);
        assert_eq!(
            at(&only_left, &left, ten, 1),
            AtDeadline::End(End::StaleTransport)
        );
        let only_stale = quarantine(|q| q.stale_pats = 40);
        assert_eq!(at(&only_stale, &play, ten, 0), AtDeadline::Retune);
        assert_eq!(
            at(&only_stale, &play, ten, 1),
            AtDeadline::End(End::StaleTransport)
        );
        assert_eq!(
            at(
                &quarantine(|q| (q.old_pats, q.other_pats) = (3, 9)),
                &left,
                ten,
                0
            ),
            AtDeadline::End(End::ServiceNotFound)
        );
        // A relock tries once more, then leaves it to the lost signal's own retunes.
        let relock = Admission {
            want_tsid: Some(NEW.tsid),
            ..play.clone()
        };
        assert_eq!(at(&quarantine(|_| {}), &relock, ten, 0), AtDeadline::Retune);
        assert_eq!(at(&quarantine(|_| {}), &relock, ten, 1), AtDeadline::Wait);
        // A scan or TKGS read: its own, shorter time, then it takes what there is.
        let read = Admission::default();
        assert_eq!(
            at(&none, &read, PAT_ABSENT - Duration::from_millis(1), 0),
            AtDeadline::Wait
        );
        assert_eq!(at(&none, &read, PAT_ABSENT, 0), AtDeadline::Open);
        // Every path to a retune is counted: replayed for 100 deadlines,
        // whatever came, at most one retune and then an end or a wait.
        for q in [&none, &corrupt, &other, &only_left, &only_stale] {
            for rule in [&play, &left, &relock] {
                let mut retunes = 0;
                for _ in 0..100 {
                    match at(q, rule, ten, retunes) {
                        AtDeadline::Retune => retunes += 1,
                        AtDeadline::End(_) | AtDeadline::Wait | AtDeadline::Open => break,
                    }
                }
                assert!(retunes <= 1, "{q:?} {rule:?}");
            }
        }
    }

    #[test]
    fn a_failed_quarantine_says_what_failed() {
        let q =
            quarantine(|q| (q.pat_sections, q.crc_errors, q.left_tsid) = (0, 37, Some(OLD.tsid)));
        assert!(quarantine_failure(End::PsiCorrupt, &q, NEW.sid).contains("CRC"));
        assert!(quarantine_failure(End::NoPat, &q, NEW.sid).contains("PAT yok"));
        assert!(quarantine_failure(End::StaleTransport, &q, NEW.sid).contains("41402"));
        assert!(quarantine_failure(End::ServiceNotFound, &q, NEW.sid).contains("51616"));
    }

    #[test]
    fn a_scan_whose_time_for_a_pat_is_up_reads_the_new_tables_only() {
        let (mut pipeline, mut reporter, out) = harness(NEW.sid, None);
        let log = Log::silent();
        pipeline.scanning = true;
        pipeline.quarantine_stream();
        assert_eq!(pipeline.gate.as_ref().unwrap().rule().service_id, None);
        // The tail: the old multiplex's SDT, no PAT of its own stream yet.
        pipeline.feed(&transfer(&[OLD.sdt(1, 0), OLD.es(0)]), &mut reporter, &log);
        let gate = pipeline.gate.as_ref().unwrap();
        assert_eq!(
            at_deadline(
                &gate.stats,
                gate.rule(),
                PAT_ABSENT,
                Duration::from_secs(10),
                0
            ),
            AtDeadline::Open
        );
        pipeline.open_gate(&log);
        assert_eq!(
            pipeline.quarantine.as_ref().unwrap().reason,
            Some(crate::ts::Admitted::Deadline)
        );
        // The new multiplex's tables, as they repeat.
        for round in 0..2 {
            pipeline.feed(
                &transfer(&[NEW.pat(round), NEW.sdt(318, round), NEW.pmt(round)]),
                &mut reporter,
                &log,
            );
        }
        assert_eq!(pipeline.analyser.original_network_id, Some(318));
        assert!(pipeline.analyser.sdt.contains_key(&NEW.sid));
        assert!(
            !pipeline.analyser.sdt.contains_key(&OLD.sid),
            "nothing of the old multiplex"
        );
        // Its time leaves the reads theirs: the scan's NIT wait (12 s, the
        // NIT sent every 10 s) and TKGS's 75 s.
        assert!(PAT_ABSENT + Duration::from_secs(10) <= Duration::from_secs(12));
        assert!(PAT_ABSENT < Duration::from_secs(75));
        let _ = written(pipeline, &out);
    }

    /// A packet of `pid` carrying the programme clock `pcr` (27 MHz).
    fn clock(pid: u16, cc: u8, pcr: u64) -> Vec<u8> {
        let (base, ext) = (pcr / 300, pcr % 300);
        let mut p = vec![
            0x47,
            (pid >> 8) as u8,
            pid as u8,
            0x20 | (cc & 0xF),
            183,
            0x10,
        ];
        p.extend_from_slice(&[
            (base >> 25) as u8,
            (base >> 17) as u8,
            (base >> 9) as u8,
            (base >> 1) as u8,
        ]);
        p.push(((base & 1) as u8) << 7 | 0x7E | (ext >> 8) as u8);
        p.push(ext as u8);
        p.resize(crate::ts::PACKET, 0xFF);
        p
    }

    /// Video of `mux` marked as from before (0xEE): told from the new by its bytes.
    fn stale_es(mux: &Mux, cc: u8) -> Vec<u8> {
        ts(mux.es, false, cc, &[0xEE, 0xEE])
    }

    fn retuned_from(left: crate::ts::LeftMux, sid: u16) -> (Pipeline, Reporter, Captured) {
        let (mut pipeline, reporter, out) = harness(sid, None);
        pipeline.left_tsid = Some(left.tsid);
        pipeline.left_pids = left.programmes.values().copied().collect();
        pipeline.left_mux = Some(left);
        pipeline.quarantine_stream();
        (pipeline, reporter, out)
    }

    #[test]
    fn another_transponder_with_the_same_tsid_and_another_service_is_let_in() {
        // Two transponders, one TSID: the PAT's programmes tell them apart.
        let twin = Mux {
            tsid: OLD.tsid,
            sid: 51616,
            pmt: 2616,
            es: 2610,
        };
        let (mut pipeline, mut reporter, out) = retuned_from(OLD.left(Some(1)), twin.sid);
        let log = Log::silent();
        pipeline.feed(
            &[OLD.rounds(1).concat(), twin.rounds(2).concat()].concat(),
            &mut reporter,
            &log,
        );
        let q = pipeline.quarantine.clone().expect("admitted");
        // The tail's PAT is the left one's: with its network known it waits
        // for an SDT, and the twin's PAT admits before one comes.
        assert_eq!(
            (q.reason, q.admitted_tsid),
            (Some(crate::ts::Admitted::Service), Some(OLD.tsid))
        );
        let got = pids(&written(pipeline, &out));
        assert!(
            got.iter().all(|pid| [0, twin.pmt, twin.es].contains(pid)) && got.contains(&twin.es),
            "{got:?}"
        );
    }

    #[test]
    fn the_same_pat_on_another_network_is_told_by_its_sdt() {
        // Same TSID, same service and PMT: only the SDT's network differs.
        let twin = Mux {
            tsid: OLD.tsid,
            sid: OLD.sid,
            pmt: OLD.pmt,
            es: OLD.es,
        };
        let (mut pipeline, mut reporter, out) = retuned_from(OLD.left(Some(1)), OLD.sid);
        let log = Log::silent();
        // One transfer: the tail (its PAT, its SDT on network 1, its video),
        // then the new stream's PAT, SDT on network 318, PMT and video.
        let tail = [
            OLD.pat(0),
            OLD.sdt(1, 0),
            stale_es(&OLD, 0),
            stale_es(&OLD, 1),
        ]
        .concat();
        let new = [
            twin.pat(1),
            twin.sdt(318, 1),
            twin.pmt(1),
            twin.es(2),
            twin.es(3),
            twin.es(4),
        ]
        .concat();
        pipeline.feed(&[tail, new].concat(), &mut reporter, &log);
        let q = pipeline.quarantine.clone().expect("admitted on the SDT");
        assert_eq!(
            (q.reason, q.admitted_onid, q.old_pats),
            (Some(crate::ts::Admitted::Network), Some(318), 1)
        );
        let data = written(pipeline, &out);
        assert_eq!(pids(&data)[0], 0, "from the new PAT");
        assert!(
            !data.windows(2).any(|w| w == [0xEE, 0xEE]),
            "nothing of the tail"
        );
        assert!(
            data.chunks(crate::ts::PACKET)
                .any(|p| crate::ts::pid_of(p) == twin.es)
        );
    }

    #[test]
    fn the_same_pat_on_the_same_network_is_the_tail_and_ends_bounded() {
        let (mut pipeline, mut reporter, out) = retuned_from(OLD.left(Some(1)), OLD.sid);
        let log = Log::silent();
        for round in 0..5 {
            pipeline.feed(
                &transfer(&[OLD.pat(round), OLD.sdt(1, round), stale_es(&OLD, round)]),
                &mut reporter,
                &log,
            );
        }
        let gate = pipeline.gate.as_ref().expect("never admitted");
        assert_eq!((gate.stats.old_pats, pipeline.other_tsid), (5, None));
        let ten = Duration::from_secs(10);
        assert_eq!(
            at_deadline(&gate.stats, gate.rule(), ten, ten, 0),
            AtDeadline::Retune
        );
        assert_eq!(
            at_deadline(&gate.stats, gate.rule(), ten, ten, 1),
            AtDeadline::End(End::StaleTransport)
        );
        assert!(written(pipeline, &out).is_empty());
    }

    #[test]
    fn an_expected_network_tells_the_same_pat_apart_without_the_left_one_s() {
        let (mut pipeline, mut reporter, out) = harness(OLD.sid, None);
        let log = Log::silent();
        pipeline.left_tsid = Some(OLD.tsid);
        pipeline.left_mux = Some(OLD.left(None));
        pipeline.expect = Some(crate::ts::Expected {
            onid: Some(318),
            tsid: OLD.tsid,
        });
        pipeline.quarantine_stream();
        pipeline.feed(&transfer(&[OLD.pat(0), OLD.sdt(1, 0)]), &mut reporter, &log);
        assert!(pipeline.gate.is_some(), "network 1 is not the channel's");
        pipeline.feed(
            &transfer(&[OLD.pat(1), OLD.sdt(318, 1), OLD.pmt(1), OLD.es(0)]),
            &mut reporter,
            &log,
        );
        assert_eq!(
            pipeline.quarantine.as_ref().unwrap().reason,
            Some(crate::ts::Admitted::Network)
        );
        // And a known TSID that is not the PAT's: a carrier without the channel.
        let (mut other, mut reporter2, out2) = harness(NEW.sid, None);
        other.expect = Some(crate::ts::Expected {
            onid: None,
            tsid: 999,
        });
        other.quarantine_stream();
        other.feed(&transfer(&NEW.rounds(1)), &mut reporter2, &log);
        assert!(
            other.gate.is_some(),
            "carries the service, but is not transport stream 999"
        );
        other.feed(&transfer(&NEW.rounds(1)), &mut reporter2, &log);
        assert_eq!(
            other.quarantine.as_ref().unwrap().reason,
            Some(crate::ts::Admitted::Repeated)
        );
        let _ = (written(pipeline, &out), written(other, &out2));
    }

    #[test]
    fn out_of_standby_the_stale_tail_of_the_same_multiplex_is_held() {
        let (mut pipeline, mut reporter, out) = harness(NEW.sid, None);
        let log = Log::silent();
        let then = 1_000 * 27_000_000u64;
        pipeline.stopped_clock = Some(crate::ts::StaleClock {
            pid: NEW.es,
            pcr: then,
            at: Instant::now() - Duration::from_secs(30),
        });
        pipeline.quarantine_stream();
        assert!(pipeline.gate.as_ref().unwrap().rule().stale_clock.is_some());
        assert!(pipeline.stopped_clock.is_none(), "used once");
        // The bridge's tail: the same PAT, a clock from just after the stop, video.
        let tail = [
            NEW.pat(0),
            clock(NEW.es, 0, then + 27_000_000 / 10),
            stale_es(&NEW, 1),
            NEW.pat(1),
        ];
        pipeline.feed(&transfer(&tail), &mut reporter, &log);
        let gate = pipeline.gate.as_ref().expect("held");
        assert_eq!(gate.stats.stale_pats, 2);
        // The broadcast now: 30 s on.
        let now = [
            clock(NEW.es, 2, then + 30 * 27_000_000),
            NEW.pat(2),
            NEW.pmt(2),
            NEW.es(3),
            NEW.es(4),
        ];
        pipeline.feed(&transfer(&now), &mut reporter, &log);
        assert_eq!(
            pipeline.quarantine.as_ref().unwrap().reason,
            Some(crate::ts::Admitted::Service)
        );
        let data = written(pipeline, &out);
        assert_eq!(pids(&data)[0], 0);
        assert!(
            !data.windows(2).any(|w| w == [0xEE, 0xEE]),
            "no stale picture"
        );
    }

    #[test]
    fn the_same_service_out_of_standby_counts_no_loss_at_the_boundary() {
        let (mut pipeline, mut reporter, out) = harness(NEW.sid, None);
        let log = Log::silent();
        pipeline.quarantine_stream();
        pipeline.feed(&transfer(&NEW.rounds(2)), &mut reporter, &log);
        assert!(pipeline.gate.is_none());
        // Standby: the stream stops; back, the same selector, counters 9 on.
        pipeline.stream_stopped();
        pipeline.quarantine_stream();
        let back: Vec<Vec<u8>> = (0..3u8)
            .flat_map(|r| [NEW.pat(r + 9), NEW.pmt(r + 9), NEW.es(r + 9)])
            .collect();
        pipeline.feed(&transfer(&back), &mut reporter, &log);
        assert!(pipeline.gate.is_none());
        assert_eq!(
            pipeline.selector.selection().timing.cc_errors,
            0,
            "the boundary is not a loss"
        );
        assert!(pipeline.selector.cc_events().is_empty());
        // A loss after it still is.
        pipeline.feed(&transfer(&[NEW.es(14)]), &mut reporter, &log);
        assert_eq!(pipeline.selector.selection().timing.cc_errors, 1);
        let _ = written(pipeline, &out);
    }

    #[test]
    fn a_stale_clock_is_only_the_clock_from_before_the_stop() {
        let at = Instant::now() - Duration::from_secs(20);
        let c = crate::ts::StaleClock {
            pid: 1,
            pcr: 5 * 27_000_000,
            at,
        };
        assert!(c.stale(5 * 27_000_000 + 27_000_000));
        assert!(c.stale(5 * 27_000_000 - 27_000_000 / 4), "a little behind");
        assert!(!c.stale(25 * 27_000_000), "the broadcast now");
        assert!(!c.stale(3), "a discontinuity is the broadcast too");
        let short = crate::ts::StaleClock {
            at: Instant::now(),
            ..c
        };
        assert!(!short.stale(5 * 27_000_000), "a stop too short to tell");
    }

    #[test]
    fn a_clock_pid_no_longer_sent_does_not_hold_the_stream() {
        let (mut pipeline, mut reporter, out) = harness(NEW.sid, None);
        let log = Log::silent();
        pipeline.stopped_clock = Some(crate::ts::StaleClock {
            pid: 0x0777,
            pcr: 0,
            at: Instant::now() - Duration::from_secs(5),
        });
        pipeline.quarantine_stream();
        pipeline.feed(&transfer(&[NEW.pat(0)]), &mut reporter, &log);
        assert!(pipeline.gate.is_some());
        std::thread::sleep(Duration::from_millis(1050));
        pipeline.feed(&transfer(&NEW.rounds(1)), &mut reporter, &log);
        assert!(
            pipeline.gate.is_none(),
            "a second without the clock's PID: let in"
        );
        let _ = written(pipeline, &out);
    }

    #[test]
    fn a_cc_error_on_the_service_leaves_the_raw_stream_around_it() {
        let dir =
            std::env::temp_dir().join(format!("mediabox-dvb-evidence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let options = Options {
            evidence_dir: Some(dir.clone()),
            ..options(NEW.sid, None)
        };
        let out = Captured::default();
        let writer = Writer::start(
            Some(Sink::Plain(Box::new(out.clone()))),
            64 << 20,
            Pressure::Block,
            None,
        );
        let mut pipeline = new_pipeline(&options, writer);
        let mut reporter = Reporter::new(None, &options);
        let log = Log::silent();
        pipeline.quarantine_stream();
        let before = transfer(&NEW.rounds(1));
        pipeline.feed(&before, &mut reporter, &log);
        // A packet of the service's video lost; a mux PID's loss is no trigger.
        let lossy = transfer(&[NEW.es(6), NEW.es(8)]);
        pipeline.feed(
            &transfer(&[ts(0x1FF5, false, 0, &[]), ts(0x1FF5, false, 5, &[])]),
            &mut reporter,
            &log,
        );
        assert!(pipeline.evidence.is_none());
        pipeline.feed(&lossy, &mut reporter, &log);
        assert!(pipeline.evidence.is_some(), "gathering");
        std::thread::sleep(EVIDENCE_AROUND + Duration::from_millis(50));
        let after = transfer(&[NEW.es(9)]);
        pipeline.feed(&after, &mut reporter, &log);
        assert!(pipeline.evidence.is_none(), "written");
        std::thread::sleep(Duration::from_millis(200));
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let ts_file = files
            .iter()
            .find(|p| p.extension().is_some_and(|e| e == "ts"))
            .expect("the raw stream");
        let raw = std::fs::read(ts_file).unwrap();
        assert!(
            raw.starts_with(&before) && raw.ends_with(&after),
            "from before the error to after it"
        );
        let note = std::fs::read_to_string(ts_file.with_extension("txt")).unwrap();
        assert!(
            note.contains("SELECTED CC ERROR") && note.contains("pid=0x0A32"),
            "{note}"
        );
        // Cut short by the next stream: written then, only this stream's.
        let mut lossy2 = NEW.rounds(1);
        lossy2.push(NEW.es(13));
        pipeline.feed(&transfer(&lossy2), &mut reporter, &log);
        assert!(pipeline.evidence.is_some());
        pipeline.quarantine_stream();
        assert!(pipeline.evidence.is_none(), "written at the stream's end");
        std::thread::sleep(Duration::from_millis(200));
        let notes = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok())
            .filter(|t| t.contains("cut short"))
            .count();
        assert_eq!(notes, 1);
        let _ = written(pipeline, &out);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_raw_multiplex_is_never_held() {
        let options = Options {
            raw: true,
            ..options(NEW.sid, None)
        };
        let out = Captured::default();
        let writer = Writer::start(
            Some(Sink::Plain(Box::new(out.clone()))),
            1 << 20,
            Pressure::Block,
            None,
        );
        let mut pipeline = new_pipeline(&options, writer);
        pipeline.left_tsid = Some(OLD.tsid);
        pipeline.quarantine_stream();
        assert!(pipeline.gate.is_none());
        let mut reporter = Reporter::new(None, &options);
        let old = OLD.rounds(1).concat();
        pipeline.feed(&old, &mut reporter, &Log::silent());
        assert_eq!(written(pipeline, &out), old);
    }

    #[test]
    fn continuity_is_said_for_the_multiplex_and_for_the_service_once_each() {
        let (mut pipeline, mut reporter, out) = retuned();
        let path = std::env::temp_dir().join(format!("mediabox-dvb-cc-{}.log", std::process::id()));
        let mut log = Log::new(Some(&path)).unwrap();
        log.silent = true;
        pipeline.feed(&NEW.rounds(1).concat(), &mut reporter, &log);
        // One packet lost on the service's video, and ten on another PID of
        // the multiplex (counted all; the first few kept and written once).
        let other = 0x1FF5;
        let mut lossy = vec![NEW.es(6), NEW.es(8)];
        for k in 0..11u8 {
            lossy.push(ts(other, false, k * 2, &[]));
        }
        pipeline.feed(&lossy.concat(), &mut reporter, &log);
        pipeline.feed(&transfer(&[NEW.es(9)]), &mut reporter, &log);
        pipeline.report(&mut reporter);
        let continuity = reporter.status.continuity.clone();
        assert_eq!(
            (
                continuity.mux_cc_errors,
                continuity.selected_service_cc_errors
            ),
            (11, 1)
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.contains(" CC ERROR start=") && !l.contains("SELECTED"))
            .collect();
        let selected: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("SELECTED CC ERROR"))
            .collect();
        assert_eq!(selected.len(), 1, "{text}");
        assert!(
            selected[0].contains("pid=0x0A32 prev=6 expected=7 got=8")
                && selected[0].contains("usb completed="),
            "{}",
            selected[0]
        );
        assert_eq!(lines.len(), crate::ts::CC_FIRST_KEPT, "{text}");
        assert!(
            lines[0].contains("pid=0x0A32 prev=6 expected=7 got=8")
                && lines[0].contains("selected=true left_pid=false"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains("pid=0x1FF5") && lines[1].contains("selected=false"),
            "{}",
            lines[1]
        );
        let _ = written(pipeline, &out);
        let _ = std::fs::remove_file(&path);
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
                original_network_id: None,
                transport_stream_id: None,
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
        // The old reader is opened before the file is left: the writer opens
        // a FIFO only when a chunk comes and a reader is there, as a
        // broadcast's next chunk always comes. One chunk sent before the
        // reader's thread had opened was kept back, and the old reader then
        // waited for ever for a writer that had moved on.
        let started = Instant::now();
        while writer.stats().readers == 0 && started.elapsed() < Duration::from_secs(5) {
            writer.send(packet(1));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(writer.stats().readers, 1, "the old reader opened");
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

