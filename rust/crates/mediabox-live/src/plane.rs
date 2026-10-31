//! The interface's video plane, reached the way vo_mediabox reaches it.
//!
//! The interface holds DRM master and scans its own surface out on Cluster0;
//! a decoded frame goes on Esmart0 of the same video port as the decoder's own
//! dma-buf. This process never opens the card: it hands the frame's descriptors
//! to the interface over `/run/mediabox-ui/video.sock` with SCM_RIGHTS, and the
//! interface commits it at its next vertical blank. One wire, written down in
//! packaging/mpv/vo_mediabox.c and in the interface's video.rs:
//!
//! ```text
//!  0 magic 'MBV1'  16 fourcc   32 pitches[4]  64 modifier     80 display width
//!  4 kind          20 width    48 offsets[4]  72 descriptors  84 display height
//!  8 id            24 height                  76 flags         88 due (frames)
//!                  28 planes
//! ```
//!
//! A frame message carries 8 bytes more than the others: when it is due on
//! the panel, CLOCK_MONOTONIC nanoseconds -- the clock of the kernel's
//! vblank timestamps. It is sent two refreshes ahead of that, and the
//! interface puts it up at the vblank nearest it, as Android's SurfaceView
//! does with `MediaCodec.releaseOutputBuffer(index, renderTimestampNs)`:
//! "for best performance and quality, call this method when you are about
//! two VSYNCs' time before the desired render time". A reader of the 88
//! bytes alone (SOCK_SEQPACKET truncates) shows it as it comes, as before.
//!
//! What this side owes the wire is the same as vo_mediabox's: a frame is kept
//! -- here, its `gst::Sample`, which is what holds MPP's buffer out of the
//! decoder's pool -- until the interface says the panel is finished with it.
//! Nothing held is let go early; when all are out a new frame waits two fields
//! for a release and is otherwise not sent (it was never on the panel).

use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const MAGIC: u32 = 0x3156_424d;
const FRAME: u32 = 0;
const STOP: u32 = 1;
const RELEASE: u32 = 2;
const HDR: u32 = 3;
const MESSAGE: usize = 88;
const TIMED: usize = MESSAGE + 8;
const REPLY: usize = 16;
const MAX_FDS: usize = 4;

/// Frames with the interface at once: vo_mediabox's MBV_IN_FLIGHT, for the
/// same reason (two committed, one waiting for the vblank, one release on the
/// wire), and each one of MPP's fixed pool.
pub const IN_FLIGHT: usize = 6;
/// Two fields at 50 Hz.
const FULL_WAIT: Duration = Duration::from_millis(40);
const STOP_WAIT: Duration = Duration::from_millis(500);

pub const SOCKET: &str = "/run/mediabox-ui/video.sock";

/// One decoded picture, as the display controller has to be told about it.
pub struct Picture {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub planes: usize,
    pub fds: [RawFd; MAX_FDS],
    pub pitches: [u32; 4],
    pub offsets: [u32; 4],
    pub modifier: u64,
    /// Encoding in bits 0-3, full range in 4-7, the vendor EOTF in 8-11:
    /// vo_mediabox's colour_flags.
    pub colour: u32,
    pub display: (u32, u32),
}

pub enum Sent {
    Shown,
    /// Every buffer was out and none came back in time.
    Dropped,
    Gone,
}

struct Held {
    id: u64,
    _sample: gst::Sample,
}

#[derive(Default)]
struct State {
    held: Vec<Held>,
    gone: bool,
    /// The player is ending: nothing more goes to the plane. Measured
    /// 2026-10-24: frames decoded while the pipeline was being stopped
    /// reached the plane after STOP, and the interface showed the last
    /// channel again over the home screen.
    closed: bool,
    dropped_full: u64,
}

pub struct Plane {
    stream: UnixStream,
    state: Mutex<State>,
    released: Condvar,
    next_id: AtomicU64,
    trace: crate::trace::Trace,
}

impl Plane {
    /// Connect to the interface; `gone` is called once, from the reader
    /// thread, when the interface goes away -- the film goes with it, as it
    /// does in vo_mediabox (interface_gone).
    pub fn connect(path: &str, trace: crate::trace::Trace, gone: impl FnOnce() + Send + 'static) -> Result<Arc<Self>, String> {
        let stream = UnixStream::connect(path).map_err(|e| format!("MediaBox arayüzüne bağlanılamadı ({path}): {e}"))?;
        let reader = stream.try_clone().map_err(|e| e.to_string())?;
        let plane = Arc::new(Self {
            stream,
            state: Mutex::new(State::default()),
            released: Condvar::new(),
            next_id: AtomicU64::new(1),
            trace,
        });
        let weak = Arc::downgrade(&plane);
        std::thread::Builder::new()
            .name("plane-release".into())
            .spawn(move || {
                let mut reader = reader;
                let mut reply = [0u8; REPLY];
                let why = loop {
                    if let Err(error) = reader.read_exact(&mut reply) {
                        break error.to_string();
                    }
                    let Some(plane) = weak.upgrade() else { return };
                    let magic = u32::from_le_bytes(reply[0..4].try_into().unwrap());
                    let kind = u32::from_le_bytes(reply[4..8].try_into().unwrap());
                    let id = u64::from_le_bytes(reply[8..16].try_into().unwrap());
                    if magic == MAGIC && kind == RELEASE {
                        plane.release(id);
                    }
                };
                if let Some(plane) = weak.upgrade() {
                    let mut state = plane.lock();
                    state.gone = true;
                    plane.released.notify_all();
                    drop(state);
                    eprintln!("mediabox-live: MediaBox arayüzü gitti ({why}); yayın kapatılıyor");
                }
                gone();
            })
            .map_err(|e| e.to_string())?;
        Ok(plane)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn release(&self, id: u64) {
        let mut state = self.lock();
        if let Some(at) = state.held.iter().position(|held| held.id == id) {
            // The sample, and with it MPP's buffer, goes back here.
            state.held.remove(at);
            self.trace.event('R', id, 0);
            self.released.notify_all();
        }
    }

    pub fn held(&self) -> usize {
        self.lock().held.len()
    }

    pub fn dropped_full(&self) -> u64 {
        self.lock().dropped_full
    }

    /// Put `picture` on the plane, keeping `sample` until the interface gives
    /// it back. Called when the frame is due: the interface shows it at its
    /// next vertical blank.
    pub fn send(&self, picture: &Picture, sample: gst::Sample, pts_ns: u64, due_ns: u64) -> Sent {
        let mut state = self.lock();
        if state.gone || state.closed {
            return Sent::Gone;
        }
        if state.held.len() >= IN_FLIGHT {
            let deadline = Instant::now() + FULL_WAIT;
            while state.held.len() >= IN_FLIGHT && !state.gone {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                state = self.released.wait_timeout(state, left).unwrap_or_else(|p| p.into_inner()).0;
            }
        }
        if state.gone {
            return Sent::Gone;
        }
        if state.held.len() >= IN_FLIGHT {
            state.dropped_full += 1;
            self.trace.event('D', 0, pts_ns / 1000);
            return Sent::Dropped;
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut message = [0u8; TIMED];
        put32(&mut message, 0, MAGIC);
        put32(&mut message, 4, FRAME);
        message[8..16].copy_from_slice(&id.to_le_bytes());
        put32(&mut message, 16, picture.fourcc);
        put32(&mut message, 20, picture.width);
        put32(&mut message, 24, picture.height);
        put32(&mut message, 28, picture.planes as u32);
        for i in 0..4 {
            put32(&mut message, 32 + i * 4, picture.pitches[i]);
            put32(&mut message, 48 + i * 4, picture.offsets[i]);
        }
        message[64..72].copy_from_slice(&picture.modifier.to_le_bytes());
        put32(&mut message, 72, picture.planes as u32);
        put32(&mut message, 76, picture.colour);
        put32(&mut message, 80, picture.display.0);
        put32(&mut message, 84, picture.display.1);
        message[MESSAGE..TIMED].copy_from_slice(&due_ns.to_le_bytes());
        if let Err(error) = self.send_message(&message, &picture.fds[..picture.planes]) {
            eprintln!("mediabox-live: kare gönderilemedi: {error}");
            state.gone = true;
            return Sent::Gone;
        }
        self.trace.event('S', id, pts_ns / 1000);
        state.held.push(Held { id, _sample: sample });
        Sent::Shown
    }

    /// What the picture is, for the connector: SDR unless the stream says HLG
    /// or PQ. Broadcast carries no mastering metadata in the transport
    /// stream's caps, so only the curve is sent.
    pub fn send_hdr(&self, eotf: u8) {
        let mut message = [0u8; MESSAGE];
        put32(&mut message, 0, MAGIC);
        put32(&mut message, 4, HDR);
        put32(&mut message, 8, u32::from(eotf));
        if let Err(error) = self.send_message(&message, &[]) {
            eprintln!("mediabox-live: HDR bilgisi gönderilemedi: {error}");
        }
    }

    /// From here on nothing is sent; `stop` takes the picture off.
    pub fn close(&self) {
        self.lock().closed = true;
    }

    /// Take the picture off and wait for every frame to come back, as
    /// vo_mediabox's uninit does. The plane stays usable: a channel change
    /// does this between channels. At the player's end, `close` comes first,
    /// so nothing follows the STOP.
    ///
    /// A frame the interface has not given back by then is still kept: its
    /// buffer goes back to MPP only on its RELEASE, or with the interface.
    /// Let go on a timeout, MPP could decode into a buffer the panel was
    /// still scanning out.
    pub fn stop(&self) {
        let mut message = [0u8; MESSAGE];
        put32(&mut message, 0, MAGIC);
        put32(&mut message, 4, STOP);
        if self.send_message(&message, &[]).is_err() {
            return;
        }
        let mut state = self.lock();
        let deadline = Instant::now() + STOP_WAIT;
        while !state.held.is_empty() && !state.gone {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                eprintln!("mediabox-live: arayüz {} kareyi {STOP_WAIT:?} içinde geri vermedi", state.held.len());
                break;
            }
            state = self.released.wait_timeout(state, left).unwrap_or_else(|p| p.into_inner()).0;
        }
        // Gone, the interface has closed its side and nothing scans these
        // buffers out any more.
        if state.gone {
            state.held.clear();
        }
    }

    fn send_message(&self, message: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
        let fds = &fds[..fds.len().min(MAX_FDS)];
        let mut iov = libc::iovec { iov_base: message.as_ptr() as *mut libc::c_void, iov_len: message.len() };
        // CMSG_SPACE(4 * sizeof(int)) with room to spare, aligned for cmsghdr.
        let mut control = [0u64; 8];
        // SAFETY: zeroed msghdr, pointers into live stack buffers; the control
        // buffer is filled through the CMSG macros within its own length.
        unsafe {
            let mut header: libc::msghdr = std::mem::zeroed();
            header.msg_iov = &mut iov;
            header.msg_iovlen = 1;
            if !fds.is_empty() {
                let bytes = std::mem::size_of_val(fds) as u32;
                header.msg_control = control.as_mut_ptr() as *mut libc::c_void;
                header.msg_controllen = libc::CMSG_SPACE(bytes) as _;
                let cmsg = libc::CMSG_FIRSTHDR(&header);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(bytes) as _;
                std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg) as *mut RawFd, fds.len());
            }
            loop {
                if libc::sendmsg(self.stream.as_raw_fd(), &header, libc::MSG_NOSIGNAL) >= 0 {
                    return Ok(());
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

fn put32(message: &mut [u8], at: usize, value: u32) {
    message[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

