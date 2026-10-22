//! One owner for the receiver's USB device.
//!
//! One libusb context, one device handle, interface 0 claimed once. Bridge
//! commands (EP 0x02 / 0x81) are synchronous bulk transfers on this handle;
//! the transport stream (EP 0x84) is read with queued asynchronous bulk
//! transfers on the *same* handle ([`TsStream`]). No second connection is ever
//! opened.
//!
//! Commands and the stream may run at the same time, which the reference
//! implementation forbade and the Android reference does (`docs/ANDROID-
//! REFERENCE-DIAG.md`: ~39 commands/s while streaming): libusb completes a
//! synchronous transfer by handling events itself, coordinated with the
//! stream's event thread through libusb's own event lock, so the two never
//! race on the handle. Commands are serialised by their caller (one
//! [`crate::bridge::Bridge`], one thread); the stream has its own thread.
//!
//! [`TsStream`] keeps `transfers` bulk reads in flight. The completion
//! callback (event thread) copies the bytes, appends them to a byte-bounded
//! queue and resubmits; nothing else. Completion order on one endpoint is
//! submission order; it is checked anyway: `out_of_order` counts a
//! data-carrying completion that arrives after a later-submitted
//! data-carrying one (the byte stream itself would be out of order), and
//! `reorder_empty` an empty one (normal while cancelling).

use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::libusb::{self, Context, DeviceHandle, Transfer};

pub const VID: u16 = 0x23E2;
pub const PID: u16 = 0x5270;
pub const EP_OUT: u8 = 0x02;
pub const EP_IN: u8 = 0x81;
pub const EP_TS: u8 = 0x84;
/// Bulk max packet size (USB 2.0 high speed).
pub const USB_PACKET: usize = 512;
/// The bridge's TS frame is 816 packets (0xDD88/89 = 816 * 188 / 4 words),
/// ending with a short packet. Buffers are whole 512-byte packets so that a
/// full packet never meets a partial buffer (LIBUSB_ERROR_OVERFLOW, lost data).
pub const DEFAULT_BUFFER: usize = (816 * 188usize).div_ceil(USB_PACKET) * USB_PACKET;
pub const DEFAULT_TRANSFERS: usize = 8;
pub const DEFAULT_QUEUE_BYTES: usize = 64 << 20;
pub const DEFAULT_TRANSFER_TIMEOUT_MS: u32 = 1000;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{name} ({code}) during {during}")]
pub struct UsbError {
    pub code: i32,
    pub name: String,
    pub during: &'static str,
}

impl UsbError {
    fn new(code: c_int, during: &'static str) -> Self {
        Self { code, name: libusb::error_name(code), during }
    }

    pub fn is_timeout(&self) -> bool {
        self.code == libusb::ERROR_TIMEOUT
    }

    pub fn is_no_device(&self) -> bool {
        self.code == libusb::ERROR_NO_DEVICE || self.code == libusb::ERROR_NOT_FOUND
    }
}

/// Bridge commands: a write and its reply. The real device or a replay.
pub trait Transport {
    fn write(&self, endpoint: u8, data: &[u8], timeout_ms: u32) -> Result<usize, UsbError>;
    fn read(&self, endpoint: u8, length: usize, timeout_ms: u32) -> Result<Vec<u8>, UsbError>;
}

/// Why opening the receiver failed, in the words the status reports.
#[derive(Debug, Clone, thiserror::Error)]
pub enum OpenError {
    #[error("{0}")]
    Library(String),
    #[error("Kanky Link-4K (23e2:5270) bulunamadı")]
    NotFound,
    #[error("kernel sürücüsü aktif; cihaz bağlantısına müdahale edilmiyor")]
    KernelDriver,
    #[error("beklenmeyen USB configuration {0}")]
    Configuration(i32),
    #[error("{0}")]
    Usb(UsbError),
}

/// Permission to touch the receiver: open it, reset its demodulator, boot
/// firmware, tune. Made in one place only -- `mediabox-dvb play --hardware`
/// -- and asked for by the one function that opens the device, so a path
/// without it (a recording, `selftest`, `firmware-check`, `select`,
/// `analyse`) cannot reach the hardware, whatever it is asked to do.
#[derive(Debug, Clone)]
pub struct HardwareAccess {
    _chosen: (),
}

impl HardwareAccess {
    /// The operator chose the real receiver (`--hardware`). Nothing else may
    /// call this.
    pub fn chosen_with_hardware_flag() -> Self {
        Self { _chosen: () }
    }
}

/// How many times this process has tried to open the receiver. `selftest`
/// and a recording report it, and it must stay 0 for them.
pub static OPEN_ATTEMPTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The device, opened and claimed.
pub struct Session {
    lib: &'static libusb::Lib,
    context: *mut Context,
    handle: *mut DeviceHandle,
    claimed: Mutex<bool>,
}

// SAFETY: libusb contexts and device handles are thread-safe (libusb's own
// documentation); the raw pointers are only ever passed to libusb.
unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Session {
    pub fn open(_access: &HardwareAccess) -> Result<Arc<Self>, OpenError> {
        OPEN_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let lib = libusb::lib().map_err(OpenError::Library)?;
        let mut context = std::ptr::null_mut();
        // SAFETY: plain libusb calls with out-pointers owned here; every
        // failure path below releases what was acquired.
        unsafe {
            let code = (lib.init)(&mut context);
            if code != 0 {
                return Err(OpenError::Usb(UsbError::new(code, "libusb_init")));
            }
            let handle = (lib.open_device_with_vid_pid)(context, VID, PID);
            if handle.is_null() {
                (lib.exit)(context);
                return Err(OpenError::NotFound);
            }
            let fail = |error| {
                (lib.close)(handle);
                (lib.exit)(context);
                Err(error)
            };
            match (lib.kernel_driver_active)(handle, 0) {
                0 => {}
                1 => return fail(OpenError::KernelDriver),
                code => return fail(OpenError::Usb(UsbError::new(code, "kernel_driver_active"))),
            }
            let mut configuration = 0;
            let code = (lib.get_configuration)(handle, &mut configuration);
            if code != 0 {
                return fail(OpenError::Usb(UsbError::new(code, "get_configuration")));
            }
            if configuration != 1 {
                return fail(OpenError::Configuration(configuration));
            }
            let code = (lib.claim_interface)(handle, 0);
            if code != 0 {
                return fail(OpenError::Usb(UsbError::new(code, "claim_interface")));
            }
            Ok(Arc::new(Self { lib, context, handle, claimed: Mutex::new(true) }))
        }
    }

    fn bulk(&self, endpoint: u8, buffer: &mut [u8], timeout_ms: u32, during: &'static str) -> Result<usize, UsbError> {
        let mut transferred = 0;
        // SAFETY: the buffer outlives the synchronous call.
        let code = unsafe {
            (self.lib.bulk_transfer)(
                self.handle,
                endpoint,
                buffer.as_mut_ptr(),
                buffer.len() as c_int,
                &mut transferred,
                timeout_ms,
            )
        };
        if code != 0 {
            return Err(UsbError::new(code, during));
        }
        Ok(transferred as usize)
    }

    /// Release the interface; the handle and the context go when the last
    /// owner (this session or a stream still holding it) drops.
    pub fn release(&self) -> Option<UsbError> {
        let mut claimed = self.claimed.lock().unwrap_or_else(|p| p.into_inner());
        if !*claimed {
            return None;
        }
        *claimed = false;
        // SAFETY: the interface was claimed on this handle.
        let code = unsafe { (self.lib.release_interface)(self.handle, 0) };
        (code != 0 && code != libusb::ERROR_NO_DEVICE).then(|| UsbError::new(code, "release_interface"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.release();
        // SAFETY: nothing else holds the handle or the context any more: a
        // stream keeps an Arc to this session while a transfer is in flight.
        unsafe {
            (self.lib.close)(self.handle);
            (self.lib.exit)(self.context);
        }
    }
}

impl Transport for Session {
    fn write(&self, endpoint: u8, data: &[u8], timeout_ms: u32) -> Result<usize, UsbError> {
        let mut buffer = data.to_vec();
        self.bulk(endpoint, &mut buffer, timeout_ms, "bulk write")
    }

    fn read(&self, endpoint: u8, length: usize, timeout_ms: u32) -> Result<Vec<u8>, UsbError> {
        let mut buffer = vec![0; length];
        let got = self.bulk(endpoint, &mut buffer, timeout_ms, "bulk read")?;
        buffer.truncate(got);
        Ok(buffer)
    }
}

impl<T: Transport + ?Sized> Transport for Arc<T> {
    fn write(&self, endpoint: u8, data: &[u8], timeout_ms: u32) -> Result<usize, UsbError> {
        (**self).write(endpoint, data, timeout_ms)
    }
    fn read(&self, endpoint: u8, length: usize, timeout_ms: u32) -> Result<Vec<u8>, UsbError> {
        (**self).read(endpoint, length, timeout_ms)
    }
}

// ------------------------------------------------------------------ stream

/// What the USB stack did with the stream, counted separately.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StreamStats {
    pub transfers: usize,
    pub buffer_size: usize,
    pub submitted: u64,
    pub completed: u64,
    pub short_reads: u64,
    pub timeouts: u64,
    pub timeout_bytes: u64,
    pub cancelled: u64,
    pub cancelled_bytes: u64,
    pub errors: std::collections::BTreeMap<String, u64>,
    pub no_device: u64,
    pub out_of_order: u64,
    pub reorder_empty: u64,
    pub overflow_chunks: u64,
    pub overflow_bytes: u64,
    /// (stream offset, length) of the first gaps an overflow left.
    pub gaps: Vec<(u64, usize)>,
    pub max_queued_bytes: usize,
    pub submit_errors: Vec<String>,
    pub event_errors: Vec<String>,
    pub stuck_transfers: usize,
    /// Bytes the USB stack delivered, in order.
    pub stream_bytes: u64,
}

impl StreamStats {
    /// Anything in here that means bytes were lost or misordered.
    pub fn data_lost(&self) -> bool {
        self.overflow_bytes > 0 || self.out_of_order > 0 || !self.errors.is_empty()
    }
}

struct State {
    queue: VecDeque<Vec<u8>>,
    queued: usize,
    pending: usize,
    running: bool,
    stop_requested: bool,
    seq_of: Vec<u64>,
    next_seq: u64,
    max_seq: Option<u64>,
    max_data_seq: Option<u64>,
    stats: StreamStats,
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    max_queue_bytes: usize,
    buffer_size: usize,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// What a transfer's callback needs: where to put its bytes, and which
/// transfer it is.
struct Slot {
    shared: Arc<Shared>,
    lib: &'static libusb::Lib,
    index: usize,
}

pub struct TsStream {
    session: Arc<Session>,
    shared: Arc<Shared>,
    transfers: Vec<*mut Transfer>,
    buffers: Vec<Box<[u8]>>,
    /// Boxed: libusb holds each slot's address (user_data) while the Vec grows.
    #[allow(clippy::vec_box)]
    slots: Vec<Box<Slot>>,
    thread: Option<std::thread::JoinHandle<()>>,
    stop_timeout: Duration,
}

// SAFETY: the transfer pointers are owned by this stream and only touched by
// libusb (event thread) and by this stream under the shared lock's protocol.
unsafe impl Send for TsStream {}

const MAX_GAPS: usize = 64;

impl TsStream {
    pub fn new(session: Arc<Session>, transfers: usize, buffer_size: usize, max_queue_bytes: usize) -> Result<Self, UsbError> {
        if !(1..=64).contains(&transfers) {
            return Err(UsbError { code: -2, name: "transfers must be 1..64".into(), during: "stream setup" });
        }
        if buffer_size == 0 || !buffer_size.is_multiple_of(USB_PACKET) {
            return Err(UsbError { code: -2, name: format!("buffer_size must be a multiple of {USB_PACKET}"), during: "stream setup" });
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                queued: 0,
                pending: 0,
                running: false,
                stop_requested: false,
                seq_of: vec![0; transfers],
                next_seq: 0,
                max_seq: None,
                max_data_seq: None,
                stats: StreamStats { transfers, buffer_size, ..Default::default() },
            }),
            ready: Condvar::new(),
            max_queue_bytes,
            buffer_size,
        });
        let mut stream = Self {
            session,
            shared,
            transfers: Vec::new(),
            buffers: Vec::new(),
            slots: Vec::new(),
            thread: None,
            stop_timeout: Duration::from_secs(3),
        };
        let lib = stream.session.lib;
        for index in 0..transfers {
            // SAFETY: a transfer with no isochronous packets; freed in Drop
            // once libusb has given it back.
            let transfer = unsafe { (lib.alloc_transfer)(0) };
            if transfer.is_null() {
                return Err(UsbError { code: -11, name: "libusb_alloc_transfer".into(), during: "stream setup" });
            }
            let mut buffer = vec![0u8; buffer_size].into_boxed_slice();
            let mut slot = Box::new(Slot { shared: Arc::clone(&stream.shared), lib, index });
            // SAFETY: the transfer was just allocated; buffer and slot are
            // boxed and live as long as the stream does.
            unsafe {
                let t = &mut *transfer;
                t.dev_handle = stream.session.handle;
                t.flags = 0;
                t.endpoint = EP_TS;
                t.kind = libusb::TRANSFER_TYPE_BULK;
                t.timeout = DEFAULT_TRANSFER_TIMEOUT_MS;
                t.length = buffer_size as c_int;
                t.actual_length = 0;
                t.callback = Some(completed);
                t.user_data = (&mut *slot as *mut Slot).cast::<c_void>();
                t.buffer = buffer.as_mut_ptr();
                t.num_iso_packets = 0;
            }
            stream.transfers.push(transfer);
            stream.buffers.push(buffer);
            stream.slots.push(slot);
        }
        Ok(stream)
    }

    pub fn start(&mut self) {
        {
            let mut state = self.shared.lock();
            state.running = true;
            for (index, transfer) in self.transfers.iter().enumerate() {
                submit(self.session.lib, *transfer, index, &mut state);
            }
        }
        let shared = Arc::clone(&self.shared);
        let session = Arc::clone(&self.session);
        let stop_timeout = self.stop_timeout;
        self.thread = Some(
            std::thread::Builder::new()
                .name("usb-ts-events".into())
                .spawn(move || events(session, shared, stop_timeout))
                .expect("spawn the USB event thread"),
        );
    }

    /// The next chunk in stream order, or None after `timeout` without one.
    pub fn get(&self, timeout: Duration) -> Option<Vec<u8>> {
        let mut state = self.shared.lock();
        if state.queue.is_empty() {
            state = self.shared.ready.wait_timeout(state, timeout).unwrap_or_else(|p| p.into_inner()).0;
        }
        let chunk = state.queue.pop_front()?;
        state.queued -= chunk.len();
        Some(chunk)
    }

    /// No transfer left in flight although nobody asked to stop: the device is
    /// gone or a submit failed; the consumer must end rather than wait.
    pub fn dead(&self) -> bool {
        let state = self.shared.lock();
        !state.stop_requested && state.pending == 0
    }

    pub fn stats(&self) -> StreamStats {
        self.shared.lock().stats.clone()
    }

    /// Cancel what is in flight, wait for every callback, join the event
    /// thread. The flag is lowered under the lock (callbacks resubmit only
    /// while it is up); cancel runs outside it, because libusb may hold its
    /// own locks while waiting for a callback that takes ours.
    pub fn stop(&mut self) -> StreamStats {
        {
            let mut state = self.shared.lock();
            state.running = false;
            state.stop_requested = true;
        }
        for transfer in &self.transfers {
            // SAFETY: cancelling a transfer that is not in flight returns an
            // error and does nothing.
            unsafe { (self.session.lib.cancel_transfer)(*transfer) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let mut state = self.shared.lock();
        state.stats.stuck_transfers = state.pending;
        state.stats.clone()
    }

    /// Whatever arrived before the stop: still stream data.
    pub fn drain(&self) -> Vec<Vec<u8>> {
        let mut state = self.shared.lock();
        state.queued = 0;
        state.queue.drain(..).collect()
    }
}

impl Drop for TsStream {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop();
        }
        // Never free a transfer libusb still owns: a stuck one is leaked with
        // its buffer and its slot, deliberately -- and with the session, so
        // the handle it was submitted on is not closed under it either.
        if self.shared.lock().pending > 0 {
            std::mem::forget(std::mem::take(&mut self.buffers));
            std::mem::forget(std::mem::take(&mut self.slots));
            std::mem::forget(Arc::clone(&self.session));
            return;
        }
        for transfer in self.transfers.drain(..) {
            // SAFETY: not in flight (pending is 0).
            unsafe { (self.session.lib.free_transfer)(transfer) };
        }
    }
}

fn submit(lib: &libusb::Lib, transfer: *mut Transfer, index: usize, state: &mut State) {
    // SAFETY: the transfer is set up and not in flight.
    let code = unsafe { (lib.submit_transfer)(transfer) };
    if code != 0 {
        state.stats.submit_errors.push(libusb::error_name(code));
        return;
    }
    state.seq_of[index] = state.next_seq;
    state.next_seq += 1;
    state.pending += 1;
    state.stats.submitted += 1;
}

fn events(session: Arc<Session>, shared: Arc<Shared>, stop_timeout: Duration) {
    let mut deadline: Option<Instant> = None;
    loop {
        {
            let state = shared.lock();
            if !state.running {
                if state.pending == 0 {
                    return;
                }
                match deadline {
                    None => deadline = Some(Instant::now() + stop_timeout),
                    Some(at) if Instant::now() > at => return,
                    Some(_) => {}
                }
            }
        }
        let mut timeout = libusb::Timeval { tv_sec: 0, tv_usec: 100_000 };
        // SAFETY: the context outlives this thread (it holds the session).
        let code = unsafe {
            (session.lib.handle_events_timeout_completed)(session.context, &mut timeout, std::ptr::null_mut())
        };
        if code != 0 && code != libusb::ERROR_TIMEOUT {
            let mut state = shared.lock();
            state.stats.event_errors.push(libusb::error_name(code));
            if state.stats.event_errors.len() > 10 {
                state.running = false;
                shared.ready.notify_all();
                return;
            }
        }
    }
}

/// The completion callback: copy, enqueue, resubmit. Nothing else.
extern "C" fn completed(transfer: *mut Transfer) {
    // SAFETY: libusb hands back a transfer this stream submitted; its
    // user_data is the slot set up in TsStream::new, alive while the stream is.
    let (slot, status, length, data) = unsafe {
        let t = &*transfer;
        let slot = &*(t.user_data as *const Slot);
        let length = t.actual_length.max(0) as usize;
        let data = if length > 0 { std::slice::from_raw_parts(t.buffer, length).to_vec() } else { Vec::new() };
        (slot, t.status, length, data)
    };
    let shared = &slot.shared;
    let mut state = shared.lock();
    state.pending -= 1;
    let seq = state.seq_of[slot.index];
    if length > 0 {
        if state.max_data_seq.is_some_and(|max| seq < max) {
            state.stats.out_of_order += 1;
        }
        state.max_data_seq = Some(state.max_data_seq.map_or(seq, |max| max.max(seq)));
    } else if state.max_seq.is_some_and(|max| seq < max) {
        state.stats.reorder_empty += 1;
    }
    state.max_seq = Some(state.max_seq.map_or(seq, |max| max.max(seq)));
    match status {
        libusb::TRANSFER_COMPLETED => {
            state.stats.completed += 1;
            if length < shared.buffer_size {
                state.stats.short_reads += 1;
            }
        }
        libusb::TRANSFER_TIMED_OUT => {
            state.stats.timeouts += 1;
            state.stats.timeout_bytes += length as u64;
        }
        libusb::TRANSFER_CANCELLED => {
            state.stats.cancelled += 1;
            state.stats.cancelled_bytes += length as u64;
        }
        libusb::TRANSFER_NO_DEVICE => {
            state.stats.no_device += 1;
            state.running = false;
        }
        other => {
            *state.stats.errors.entry(libusb::transfer_status_name(other).to_string()).or_default() += 1;
        }
    }
    // The partial data of a timed-out or cancelled transfer is stream data too.
    if !data.is_empty() {
        let offset = state.stats.stream_bytes;
        if state.queued + data.len() > shared.max_queue_bytes {
            state.stats.overflow_chunks += 1;
            state.stats.overflow_bytes += data.len() as u64;
            if state.stats.gaps.len() < MAX_GAPS {
                state.stats.gaps.push((offset, data.len()));
            }
        } else {
            state.queued += data.len();
            state.stats.max_queued_bytes = state.stats.max_queued_bytes.max(state.queued);
            state.queue.push_back(data);
            shared.ready.notify_one();
        }
        state.stats.stream_bytes += length as u64;
    }
    if state.running {
        submit(slot.lib, transfer, slot.index, &mut state);
    }
    if state.pending == 0 {
        shared.ready.notify_all();
    }
}
