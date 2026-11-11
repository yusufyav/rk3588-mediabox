//! RK3588 split render/display Slint platform.
//!
//! Rendering and scanning out are two DRM devices here. Every frame is rendered
//! into a GBM buffer object on the render device, exported as a dma-buf,
//! PRIME-imported on the display device and kept alive until its KMS page-flip
//! event arrives.
//!
//! Which two devices those are is not written down. `mediabox-platform` finds
//! the display device by asking which DRM device owns connectors, and the
//! render device by asking which one belongs to the same hardware — because on
//! a board whose NPU probes first, the numbers this was first written against
//! (`card0`, `renderD128`) name the NPU instead. The connector is chosen the
//! same way, and the audio endpoint and CEC adapter follow from that choice
//! rather than being picked separately.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::num::NonZeroU32;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::time::Duration;

use drm::Device as DrmDevice;
use drm::buffer::{Buffer, DrmFourcc, DrmModifier, Handle as BufferHandle, PlanarBuffer};
use drm::control::{self, Device as ControlDevice};
use gbm::AsRaw;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{ContextApi, ContextAttributesBuilder};
use glutin::display::{AsRawDisplay, DisplayApiPreference, GetGlDisplay, GlDisplay, RawDisplay};
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use input::LibinputInterface;
use input::event::EventTrait;
use input::event::keyboard::{KeyState, KeyboardEventTrait};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use slint::platform::femtovg_renderer::{FemtoVGRenderer, OpenGLInterface};
use slint::platform::{EventLoopProxy, Platform, PlatformError, WindowAdapter, WindowEvent};

/// The size the interface is laid out for, seen from a sofa. Every metric in
/// the theme derives from the viewport, and its `min()` bounds are written
/// against this: handed a 3840x2560 logical viewport instead, every one of them
/// saturates at its cap and the home screen is no longer the home screen.
const DESIGN: (f32, f32) = (1920.0, 1080.0);

#[derive(Clone)]
struct SharedKms(Arc<OwnedFd>);

impl AsFd for SharedKms {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl DrmDevice for SharedKms {}
impl ControlDevice for SharedKms {}

struct ImportedPlane {
    handle: BufferHandle,
    size: (u32, u32),
    pitch: u32,
    offset: u32,
    modifier: DrmModifier,
}

impl Buffer for ImportedPlane {
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn format(&self) -> DrmFourcc {
        DrmFourcc::Argb8888
    }
    fn pitch(&self) -> u32 {
        self.pitch
    }
    fn handle(&self) -> BufferHandle {
        self.handle
    }
}

impl PlanarBuffer for ImportedPlane {
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn format(&self) -> DrmFourcc {
        DrmFourcc::Argb8888
    }
    fn modifier(&self) -> Option<DrmModifier> {
        (!matches!(self.modifier, DrmModifier::Invalid)).then_some(self.modifier)
    }
    fn pitches(&self) -> [u32; 4] {
        [self.pitch, 0, 0, 0]
    }
    fn handles(&self) -> [Option<BufferHandle>; 4] {
        [Some(self.handle), None, None, None]
    }
    fn offsets(&self) -> [u32; 4] {
        [self.offset, 0, 0, 0]
    }
}

/// What it costs to make one GBM buffer scannable by the display controller,
/// kept for as long as that buffer exists.
///
/// A GBM surface cycles through a small fixed set of buffer objects — two or
/// three — and hands the same ones back round. The first version of this
/// platform exported a dma-buf, imported it over PRIME and created a
/// framebuffer *every frame*, then destroyed both on the next one: four ioctls
/// per frame on the display device, each with its own cache maintenance, for a
/// result that was identical to the one thrown away sixteen milliseconds
/// earlier.
///
/// This is attached to the buffer object as GBM user data instead, so it is
/// built once per buffer and destroyed exactly when the buffer is — which is
/// when the surface itself goes, at shutdown. Releasing a locked front buffer
/// back to the surface does not destroy it, so the next time that buffer comes
/// round its framebuffer is already there.
struct Scanout {
    kms: SharedKms,
    framebuffer: control::framebuffer::Handle,
    imported_handle: BufferHandle,
    _dma_buf: OwnedFd,
}

/// Set once the display has been handed over lit: from then on the frame on
/// the panel belongs to the next owner's start, and nothing here may remove
/// it -- removing the framebuffer on a primary plane switches the CRTC off.
static KEEP_SCANOUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Drop for Scanout {
    fn drop(&mut self) {
        if KEEP_SCANOUT.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if let Err(e) = self.kms.destroy_framebuffer(self.framebuffer) {
            eprintln!("mediabox-tv.kms cleanup rmfb failed: {e}");
        }
        if let Err(e) = self.kms.close_buffer(self.imported_handle) {
            eprintln!("mediabox-tv.kms cleanup gem-close failed: {e}");
        }
    }
}

/// A buffer that is on the panel, or about to be.
///
/// Holding the locked `BufferObject` is what keeps GBM from handing it back to
/// the renderer while the display controller is still reading it; it is
/// released only once a later page flip has reported that the panel has moved
/// on.
struct PresentedFrame {
    framebuffer: control::framebuffer::Handle,
    _bo: gbm::BufferObject<Scanout>,
    /// Only ever `Some` under `MEDIABOX_TV_NO_FB_CACHE`, where the framebuffer
    /// belongs to the frame rather than to the buffer and is destroyed with it.
    _uncached: Option<Scanout>,
}

/// The interface's own frames: the buffers it drew, which only its thread
/// may touch (GBM). When they go up is the presentation's (`Stage`).
#[derive(Default)]
struct Presentation {
    current: Option<PresentedFrame>,
    /// The frame the panel showed before `current`, released once the
    /// commit that replaced it (`pending_seq`) has flipped.
    pending_previous: Option<PresentedFrame>,
    pending_seq: u64,
    first_frame_logged: bool,
    /// How many buffers have needed a framebuffer built for them. On a healthy
    /// run this stops at the surface's buffer count — two or three — and the
    /// per-frame import work is gone. A number that keeps climbing means the
    /// cache is not working and is worth seeing in the journal.
    imports: u32,
}

/// What goes on the panel and when: the film's frames, the commit in
/// flight, the vblanks. One lock, one owner of the timing.
///
/// The film used to be put up by the interface's event loop, between its
/// drawing and its input: a frame due at a vblank waited for whatever that
/// loop was doing. Measured on the Plus over a live match (2026-11-03):
/// opening the channel band drew for 30-31 ms in one pass and the picture
/// due in that time never went up (`unseen same-vblank`, the live monitor's
/// "atlanan"); 27 such pictures in two hours, in clusters at key presses and
/// at a Bluetooth keyboard going away. Now a thread of its own
/// (`presenter`) reads the player's socket and the display's events and
/// commits each film frame at its vblank, whatever the interface is doing.
/// The interface hands over a drawn frame (`SplitDisplay::present_frame`)
/// and the rare whole-display changes (a mode, a colour, the hand-over)
/// hold this lock while they talk to the kernel, so only one commit is ever
/// made at a time and never two in flight on the CRTC.
struct StageState {
    sink: crate::video::Sink<SharedKms>,
    /// The overlays the film can go on (`VideoPlane::candidates`).
    video: Vec<crate::video::VideoPlane>,
    /// A commit is in flight: nothing else may be committed until its page
    /// flip event has been read.
    waiting_for_flip: bool,
    /// The interface frame the commit in flight carries, by its number.
    flip_ui_seq: Option<u64>,
    /// The interface's frame handed over and not yet committed: its plane,
    /// framebuffer and number. The presenter puts it in its next commit --
    /// with the film frame due then, or alone -- so the interface never
    /// commits and never waits on a CRTC the film keeps busy.
    ui_next: Option<(control::plane::Handle, control::framebuffer::Handle, u64)>,
    /// The number of the interface's last frame handed over, and of the last
    /// one whose flip has been read.
    ui_seq: u64,
    ui_done_seq: u64,
    /// A commit carrying the interface's frame was refused: said to it at
    /// its next frame, as a commit of its own used to be.
    ui_error: Option<String>,
    /// The interface is changing the whole display (a mode, a colour, the
    /// hand-over): nothing is committed until it lets go (`hold`).
    hold: bool,
    /// The commit in flight: what it carries and when it went, for a flip
    /// event that never comes (`recover_flip`).
    inflight: Option<Inflight>,
    /// Since when commits have been refused as busy or not applied, with no
    /// flip read: past `FLIP_FATAL` the presentation is declared broken.
    stuck_since: Option<std::time::Instant>,
    /// Lost flip events, by outcome, and failed reads of the display's
    /// events: for the journal and the report.
    flips_reconciled: u64,
    flips_requeued: u64,
    read_errors: u64,
    /// The kernel has been asked for its next vblank and has not answered.
    vblank_asked: bool,
    /// When the last commit reached its vblank.
    last_flip: Option<std::time::Instant>,
    /// The refresh period of the mode on the wire, and the panel's size.
    period_ns: u64,
    panel: (u32, u32),
    /// The interface has a frame on the panel: a film frame may go up.
    ui_on_panel: bool,
    /// The display has been handed over: nothing more goes up.
    released: bool,
    /// What the interface last took from here (`sync_presented`).
    subtitle_seen: u64,
    hdr_seen: Option<crate::video::HdrStatic>,
}

struct Stage {
    state: Mutex<StageState>,
    /// Notified after every pass of the presenter: a flip read, a frame
    /// arrived.
    changed: Condvar,
    /// Wakes the presenter out of its poll: an interface frame handed over,
    /// a hold let go.
    wake: OwnedFd,
}

impl Stage {
    fn lock(&self) -> MutexGuard<'_, StageState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn poke(&self) {
        let one: u64 = 1;
        // SAFETY: an eventfd this struct owns; eight bytes from the stack.
        unsafe { libc::write(self.wake.as_raw_fd(), (&one as *const u64).cast(), 8) };
    }
}

/// The stage held by the interface (`SplitDisplay::hold`): nothing is
/// committed while this lives; dropped, the presenter goes on.
struct Held<'a> {
    stage: &'a Stage,
    guard: Option<MutexGuard<'a, StageState>>,
}

impl std::ops::Deref for Held<'_> {
    type Target = StageState;
    fn deref(&self) -> &StageState {
        self.guard.as_ref().expect("held until dropped")
    }
}

impl std::ops::DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut StageState {
        self.guard.as_mut().expect("held until dropped")
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if let Some(mut guard) = self.guard.take() {
            guard.hold = false;
            drop(guard);
            self.stage.poke();
        }
    }
}

/// How long the interface waits for a commit's page flip before deciding it
/// is lost. A flip that never comes used to hang the event loop for good.
const FLIP_LOST: Duration = Duration::from_secs(1);

/// A commit whose flip event has not come.
#[derive(Clone, Copy)]
struct Inflight {
    at: std::time::Instant,
    /// The interface frame it carries: plane, framebuffer, number.
    ui: Option<(control::plane::Handle, control::framebuffer::Handle, u64)>,
    /// The film frame it carries: plane, framebuffer.
    film: Option<(control::plane::Handle, control::framebuffer::Handle)>,
    /// The CRTC's vblank count when it was committed: two vblanks later a
    /// nonblocking commit with nothing before it has been applied.
    vblank: Option<u32>,
}

/// The CRTC's vblank count now and when that vblank was (CLOCK_MONOTONIC ns).
fn vblank_now(kms: &SharedKms, crtc: control::crtc::Handle) -> Option<(u32, Option<u64>)> {
    let pipe = kms.resource_handles().ok()?.crtcs().iter().position(|c| *c == crtc)?;
    let reply = kms.wait_vblank(drm::VblankWaitTarget::Relative(0), drm::VblankWaitFlags::empty(), pipe as u32, 0).ok()?;
    Some((reply.frame(), reply.time().map(|t| t.as_nanos() as u64)))
}

/// A flip this many periods late is looked for: the vblank count and the
/// planes. (Three-halves: the vblank after the commit, and half a period
/// for the event to have been read had it come.)
const FLIP_OVERDUE_HALF_PERIODS: u32 = 3;
/// Not on the planes this many periods after the commit: it was not applied.
const FLIP_UNAPPLIED_PERIODS: u32 = 10;
/// No flip at all for this long, with recovery tried: the display is broken
/// and the process says so and ends (systemd starts it again) rather than
/// leave a frozen picture.
const FLIP_FATAL: Duration = Duration::from_secs(3);

/// Why a commit did not go.
enum CommitError {
    /// The kernel still has a commit of ours pending on the CRTC.
    Busy,
    Refused(String),
}

impl std::fmt::Display for CommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommitError::Busy => write!(f, "atomic commit: Device or resource busy (a commit of ours still pending)"),
            CommitError::Refused(error) => write!(f, "{error}"),
        }
    }
}

/// MEDIABOX_TV_FAULT=dropflip=N / readerr=N: every Nth page-flip event
/// thrown away, or every Nth read of the display's events failed, on
/// purpose -- to show a lost flip is recovered. Unset, never.
fn flip_fault() -> (u64, u64) {
    static FAULT: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    *FAULT.get_or_init(|| {
        let spec = std::env::var("MEDIABOX_TV_FAULT").unwrap_or_default();
        let get = |key: &str| {
            spec.split(',').find_map(|part| part.strip_prefix(key).and_then(|n| n.parse().ok())).unwrap_or(0)
        };
        let fault = (get("dropflip="), get("readerr="));
        if fault != (0, 0) {
            eprintln!("mediabox-tv.kms: BİLEREK KUSUR: her {}. flip olayı atılıyor, her {}. olay okuması hatalı sayılıyor", fault.0, fault.1);
        }
        fault
    })
}

/// The refresh period of `mode`, ns.
fn period_of(mode: &control::Mode) -> u64 {
    let (_, _, htotal) = mode.hsync();
    let (_, _, vtotal) = mode.vsync();
    let clock_khz = u64::from(mode.clock());
    if clock_khz == 0 || htotal == 0 || vtotal == 0 {
        return 16_666_667;
    }
    u64::from(htotal) * u64::from(vtotal) * 1_000_000 / clock_khz
}

/// The board's crystal's error against CLOCK_MONOTONIC, ppm (positive:
/// fast): how much faster CLOCK_MONOTONIC_RAW -- the crystal itself, which
/// the HDMI PHY's PLL runs from -- goes than CLOCK_MONOTONIC, which NTP
/// slews and the film's frames are due on and the page flips are stamped
/// with. Measured from this process's start; for its first
/// `CRYSTAL_WINDOW`, what an earlier process of this boot measured
/// (`CRYSTAL_FILE`), or None when none did (`crystal_ppm_settled` waits).
///
/// The film's frames are due on CLOCK_MONOTONIC at the broadcast's rate
/// (the PCR came within 0.5 ppm of it, 2026-11-05) and the panel ran at the
/// crystal's: NTP's freq -11.40 ppm, the vblanks -10.90 ppm against
/// CLOCK_MONOTONIC, a picture shown twice every 29 min 22 s (22:03:14 and
/// 22:32:36 on TRT 1). adjtimex(2) would say the same, but the service's
/// ProtectClock= takes it away; reading the two clocks is not.
fn crystal_ppm() -> Option<f64> {
    static START: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    let now = raw_and_monotonic();
    let start = *START.get_or_init(|| now);
    let raw = now.0.checked_sub(start.0)?;
    let mono = now.1.checked_sub(start.1)?;
    if mono < CRYSTAL_WINDOW.as_nanos() as u64 {
        return crystal_kept().or_else(|| CRYSTAL_EARLY.get().copied());
    }
    let ppm = (raw as f64 / mono as f64 - 1.0) * 1e6;
    let ppm = (ppm.abs() <= CRYSTAL_PPM_MAX).then_some(ppm)?;
    crystal_keep(ppm);
    Some(ppm)
}

/// The crystal's error for a mode set that is about to be made: at once when
/// it is known, else once `crystal_early` or `CRYSTAL_WINDOW` has it (a first
/// process of a boot, in its first seconds). A panel set on the EDID's clock
/// then would repeat a picture every half hour of the film that follows: the
/// mode is not set again while it plays (2026-11-06, 10:17:22 restart, a
/// channel 6 s later on 594000 kHz, a repeat at 10:31:46).
fn crystal_ppm_settled() -> Option<f64> {
    if let Some(ppm) = crystal_ppm() {
        return Some(ppm);
    }
    let started = std::time::Instant::now();
    while started.elapsed() < CRYSTAL_WINDOW + Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(ppm) = crystal_ppm() {
            eprintln!("mediabox-tv.platform waited {:?} for the crystal's error before the mode", started.elapsed());
            mediabox_core::timeline!("crystal-wait", "ms={}", started.elapsed().as_millis());
            return Some(ppm);
        }
    }
    None
}

/// The crystal's error from the first seconds, before `CRYSTAL_WINDOW`:
/// set once two windows of `CRYSTAL_STEP` that share no instant put a 594 MHz
/// clock on the same kHz (1.7 ppm), the rate over both then. Windows of 10 s
/// spread by 0.06 ppm on this board (11.20..11.26, 2026-11-06), 2 s ones by
/// five times that at most: under a kHz's fifth, and a disagreement near a
/// kHz's edge waits for the next pair, up to the whole window.
static CRYSTAL_EARLY: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
const CRYSTAL_STEP: Duration = Duration::from_secs(2);

fn crystal_early(start: (u64, u64)) {
    let rate = |a: (u64, u64), b: (u64, u64)| ((b.0 - a.0) as f64 / (b.1 - a.1) as f64 - 1.0) * 1e6;
    let khz = |ppm: f64| (594_000.0 / (1.0 + ppm * 1e-6)).round() as u32;
    let mut points = vec![start];
    while points.len() * CRYSTAL_STEP.as_secs() as usize <= CRYSTAL_WINDOW.as_secs() as usize {
        std::thread::sleep(CRYSTAL_STEP);
        points.push(raw_and_monotonic());
        let n = points.len();
        if n < 3 {
            continue;
        }
        let (a, b) = (rate(points[n - 3], points[n - 2]), rate(points[n - 2], points[n - 1]));
        mediabox_core::timeline!("crystal-step", "a={a:.3} b={b:.3} khz_a={} khz_b={}", khz(a), khz(b));
        if khz(a) == khz(b) && a.abs() <= CRYSTAL_PPM_MAX {
            let ppm = rate(start, points[n - 1]);
            let _ = CRYSTAL_EARLY.set(ppm);
            eprintln!("mediabox-tv.platform crystal {ppm:+.3} ppm after {:?} ({a:+.3}, {b:+.3})", CRYSTAL_STEP * (n as u32 - 1));
            return;
        }
    }
}

/// Where a process's measurement is kept for the next one of the same boot
/// (a restart of the interface): `boot_id ppm`. A rate measured over
/// `CRYSTAL_WINDOW` or more, the same the process itself would believe.
const CRYSTAL_FILE: &str = "/var/lib/mediabox-ui/crystal-ppm";

fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok().map(|id| id.trim().to_string())
}

fn crystal_kept() -> Option<f64> {
    let kept = std::fs::read_to_string(CRYSTAL_FILE).ok()?;
    let (boot, ppm) = kept.trim().split_once(' ')?;
    let ppm: f64 = ppm.parse().ok()?;
    (Some(boot) == boot_id().as_deref() && ppm.abs() <= CRYSTAL_PPM_MAX).then_some(ppm)
}

fn crystal_keep(ppm: f64) {
    let Some(boot) = boot_id() else { return };
    let temporary = format!("{CRYSTAL_FILE}.new");
    if std::fs::write(&temporary, format!("{boot} {ppm:.4}\n")).is_ok() {
        let _ = std::fs::rename(&temporary, CRYSTAL_FILE);
    }
}

/// How long the two clocks are compared before their rates are believed:
/// two reads 0.1 us apart over 10 s are 0.01 ppm.
const CRYSTAL_WINDOW: Duration = Duration::from_secs(10);

/// CLOCK_MONOTONIC_RAW and CLOCK_MONOTONIC at one instant (the raw one read
/// either side of the other, and halved), ns.
fn raw_and_monotonic() -> (u64, u64) {
    let read = |clock| {
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a valid clock id and a timespec on the stack.
        unsafe { libc::clock_gettime(clock, &mut t) };
        t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
    };
    let before = read(libc::CLOCK_MONOTONIC_RAW);
    let mono = read(libc::CLOCK_MONOTONIC);
    let after = read(libc::CLOCK_MONOTONIC_RAW);
    (before + (after - before) / 2, mono)
}

/// A crystal's error past this is not believed (a board's is ±20 ppm by
/// spec, NTP's freq is bounded at 500).
const CRYSTAL_PPM_MAX: f64 = 100.0;

/// `mode` with its pixel clock scaled so that the panel's refresh, made from
/// the crystal, is the mode's own on CLOCK_MONOTONIC. The PHY makes any rate
/// to the kHz with its fractional PLL (phy-rockchip-samsung-hdptx-hdmi.c
/// `hdptx_phy_clk_pll_calc`), and setting it is a modeset like any other:
/// done where one is done anyway, never while a film plays.
fn on_the_wire(mode: control::Mode, crystal_ppm: Option<f64>) -> control::Mode {
    let Some(ppm) = crystal_ppm else { return mode };
    let mut info: drm_ffi::drm_mode_modeinfo = mode.into();
    let khz = (f64::from(info.clock) / (1.0 + ppm * 1e-6)).round() as u32;
    if khz == info.clock || khz == 0 {
        return mode;
    }
    info.clock = khz;
    control::Mode::from(info)
}

fn say_wire(mode: &control::Mode, wire: &control::Mode) {
    if mode.clock() != wire.clock() {
        let ppm = (f64::from(wire.clock()) / f64::from(mode.clock()) - 1.0) * 1e6;
        eprintln!(
            "mediabox-tv.platform pixel clock {} -> {} kHz ({ppm:+.2} ppm): the crystal's error, from the clock discipline",
            mode.clock(),
            wire.clock()
        );
        mediabox_core::timeline!("wire-clock", "edid_khz={} wire_khz={} ppm={ppm:.2}", mode.clock(), wire.clock());
    }
}

/// One commit for this vblank: the interface's frame (`ui`, on its plane) if
/// there is one, the waiting film frame if there is one, or both.
/// Nonblocking, with an event when the vblank has passed; nothing else is
/// committed until the presenter has read it. True when something was
/// committed.
fn commit_on_stage(
    kms: &SharedKms,
    crtc: control::crtc::Handle,
    s: &mut StageState,
    ui: Option<(control::plane::Handle, control::framebuffer::Handle, u64)>,
) -> Result<bool, CommitError> {
    use control::property::Value;
    let mut request = control::atomic::AtomicModeReq::new();
    if let Some((plane, framebuffer, _)) = ui {
        let fb = find_property(kms, plane, "FB_ID")
            .ok_or_else(|| CommitError::Refused("no FB_ID on the interface's plane".into()))?;
        request.add_property(plane, fb, Value::Framebuffer(Some(framebuffer)));
    }
    let staged = s.sink.stage(kms, crtc, &s.video, &mut request);
    // No film frame: the film's plane veiled while a player is connected,
    // so the film's first frame does not turn a 4K video plane on in its
    // own commit (`video::yuv_4k`).
    let veil = if staged.is_none() && s.sink.wants_veil() {
        s.sink.stage_veil(kms, crtc, &s.video, s.panel, &mut request)
    } else {
        None
    };
    if ui.is_none() && staged.is_none() && veil.is_none() {
        return Ok(false);
    }
    let flags = control::AtomicCommitFlags::NONBLOCK | control::AtomicCommitFlags::PAGE_FLIP_EVENT;
    let committed = kms.atomic_commit(flags, request);
    if let Some(index) = veil {
        match &committed {
            Ok(()) => s.sink.veil_committed(&s.video, index),
            Err(error) => s.sink.veil_refused(&error.to_string()),
        }
    }
    let committed = match (committed, staged.or(veil), ui) {
        (Ok(()), _, _) => Ok(staged),
        // The film's part refused it: the interface's frame goes on its
        // own rather than the panel freezing on the last one.
        (Err(error), Some(_), Some((plane, framebuffer, _))) => {
            s.sink.refused_commit(&error.to_string());
            let fb = find_property(kms, plane, "FB_ID").ok_or_else(|| CommitError::Refused("no FB_ID".into()))?;
            let mut alone = control::atomic::AtomicModeReq::new();
            alone.add_property(plane, fb, Value::Framebuffer(Some(framebuffer)));
            kms.atomic_commit(flags, alone).map(|()| None)
        }
        (Err(error), _, _) => {
            if staged.is_some() {
                s.sink.refused_commit(&error.to_string());
            }
            Err(error)
        }
    };
    let staged = committed.map_err(|e| {
        if e.raw_os_error() == Some(libc::EBUSY) {
            CommitError::Busy
        } else {
            CommitError::Refused(format!("atomic commit: {e}"))
        }
    })?;
    crate::video::trace::event('C', u64::from(ui.is_some()), u64::from(staged.is_some()));
    let mut film = None;
    if let Some(index) = staged {
        film = s.sink.committed(&s.video, index);
        VIDEO.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    mediabox_core::timeline!(
        "commit",
        "pid={} id={} ui={} thread={}",
        s.sink.peer(),
        film.unwrap_or(0),
        u8::from(ui.is_some()),
        std::thread::current().name().unwrap_or("?")
    );
    s.waiting_for_flip = true;
    s.flip_ui_seq = ui.map(|(_, _, seq)| seq);
    s.inflight = Some(Inflight {
        at: std::time::Instant::now(),
        ui,
        film: if film.is_some() { s.sink.on_panel(&s.video) } else { None },
        vblank: vblank_now(kms, crtc).map(|(seq, _)| seq),
    });
    Ok(true)
}

/// The commit in flight has reached the panel at `at` (CLOCK_MONOTONIC ns):
/// read from its page-flip event, or, the event lost, from the planes
/// (`recover_flip`).
fn flip_completed(s: &mut StageState, at: u64) {
    s.sink.flipped(at, s.period_ns);
    s.sink.vblank(at, s.period_ns);
    s.waiting_for_flip = false;
    s.inflight = None;
    s.stuck_since = None;
    s.last_flip = Some(std::time::Instant::now());
    if let Some(seq) = s.flip_ui_seq.take() {
        s.ui_done_seq = seq;
    }
}

/// Every event the display has for this process: page flips end the commit
/// in flight, vblanks asked for set the grid. A read that fails is said
/// (errno, and what was in flight) and counted; the commit it may have
/// carried the flip of is then recovered from the planes (`recover_flip`).
fn read_display_events(kms: &SharedKms, crtc: control::crtc::Handle, s: &mut StageState) {
    static READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static FLIPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (drop_every, fail_every) = flip_fault();
    let read = READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let events = match kms.receive_events() {
        Ok(events) if fail_every > 0 && read % fail_every == 0 => {
            drop(events.count());
            Err(std::io::Error::other("injected (MEDIABOX_TV_FAULT=readerr)"))
        }
        result => result,
    };
    let events = match events {
        Ok(events) => events,
        Err(error) => {
            s.read_errors += 1;
            mediabox_core::timeline!("drm-read-error", "errno={:?} waiting={} n={}", error.raw_os_error(), s.waiting_for_flip, s.read_errors);
            if s.read_errors <= 5 || s.read_errors.is_power_of_two() {
                eprintln!(
                    "mediabox-tv.kms reading the display's events failed ({error}); commit in flight={}, {} times so far",
                    s.waiting_for_flip, s.read_errors
                );
            }
            return;
        }
    };
    for event in events {
        match event {
            control::Event::PageFlip(flip) => {
                let n = FLIPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if drop_every > 0 && n % drop_every == 0 {
                    mediabox_core::timeline!("fault", "dropflip n={n} seq={}", flip.frame);
                    continue;
                }
                crate::video::trace::event('F', 0, 0);
                crate::video::trace::vblank(flip.frame, flip.duration);
                let at = flip.duration.as_nanos() as u64;
                mediabox_core::timeline!("flip", "seq={} at={at}", flip.frame);
                flip_completed(s, at);
            }
            control::Event::Vblank(vblank) if vblank.crtc == crtc => {
                s.vblank_asked = false;
                mediabox_core::timeline!("vblank-event", "seq={} at={}", vblank.frame, vblank.time.as_nanos());
                s.sink.vblank(vblank.time.as_nanos() as u64, s.period_ns);
            }
            _ => {}
        }
    }
}

/// The commit in flight has had no flip event for `FLIP_OVERDUE_PERIODS`:
/// the planes say whether it was applied.
///
/// - Every plane it set shows what it set: the commit reached the panel and
///   only its event was lost. The state goes on as if the event had come,
///   at the vblank the kernel reports now.
/// - After `FLIP_UNAPPLIED_PERIODS` a plane still shows something else: it
///   was not applied. Nothing it carried is taken for shown; the
///   interface's frame goes back to wait for the next commit, and the
///   buffers on the panel stay held.
/// - No flip for `FLIP_FATAL` with this tried: the presentation is broken,
///   and the process says so and ends rather than freeze the picture.
fn recover_flip(kms: &SharedKms, crtc: control::crtc::Handle, s: &mut StageState) {
    let Some(inflight) = s.inflight else {
        return;
    };
    let period = Duration::from_nanos(s.period_ns.max(1));
    let late = inflight.at.elapsed();
    if late < period * FLIP_OVERDUE_HALF_PERIODS / 2 {
        return;
    }
    // GETPLANE reports the state committed, not what is scanned out: the
    // planes alone say the commit was taken, the vblank count that a vblank
    // has passed since -- the one it went up at.
    let shows = |plane: control::plane::Handle, fb: control::framebuffer::Handle| {
        kms.get_plane(plane).ok().is_some_and(|info| info.framebuffer() == Some(fb))
    };
    let now = vblank_now(kms, crtc);
    let passed = match (inflight.vblank, now) {
        (Some(then), Some((seq, _))) => seq.wrapping_sub(then) >= 2 && seq.wrapping_sub(then) < 1 << 30,
        // No count to go by: three periods, as before.
        _ => late >= period * 3,
    };
    let applied = passed
        && inflight.ui.is_none_or(|(plane, fb, _)| shows(plane, fb))
        && inflight.film.is_none_or(|(plane, fb)| shows(plane, fb));
    let stuck = *s.stuck_since.get_or_insert(inflight.at);
    if applied && (inflight.ui.is_some() || inflight.film.is_some()) {
        // It went up at the vblank after its commit: that vblank's time,
        // from the one the kernel reports now. Unreadable, the time now.
        let at = match (inflight.vblank, now) {
            (Some(then), Some((seq, Some(time)))) => {
                time.saturating_sub(u64::from(seq.wrapping_sub(then).saturating_sub(1)) * s.period_ns)
            }
            _ => monotonic_now_ns(),
        };
        s.flips_reconciled += 1;
        mediabox_core::timeline!("flip-reconciled", "late_ms={:.1} ui={} film={}", late.as_secs_f64() * 1e3, inflight.ui.is_some(), inflight.film.is_some());
        eprintln!(
            "mediabox-tv.kms the page-flip event of a commit did not come in {:.0} ms; the planes show it, so it is taken as flipped ({} so far)",
            late.as_secs_f64() * 1e3,
            s.flips_reconciled
        );
        flip_completed(s, at);
        return;
    }
    if late < period * FLIP_UNAPPLIED_PERIODS {
        return;
    }
    if stuck.elapsed() >= FLIP_FATAL {
        mediabox_core::timeline!("flip-fatal", "stuck_ms={}", stuck.elapsed().as_millis());
        eprintln!(
            "mediabox-tv.kms FATAL: no commit has reached the panel for {} ms and recovery did not bring one; ending so the service starts again",
            stuck.elapsed().as_millis()
        );
        std::process::exit(70);
    }
    // Not applied: nothing it carried is on the panel.
    s.flips_requeued += 1;
    mediabox_core::timeline!("flip-unapplied", "late_ms={:.1} ui={} film={}", late.as_secs_f64() * 1e3, inflight.ui.is_some(), inflight.film.is_some());
    eprintln!(
        "mediabox-tv.kms a commit was not applied in {:.0} ms (no flip, the planes unchanged); its interface frame waits for the next commit ({} so far)",
        late.as_secs_f64() * 1e3,
        s.flips_requeued
    );
    s.waiting_for_flip = false;
    s.inflight = None;
    s.flip_ui_seq = None;
    if let Some(frame) = inflight.ui
        && s.ui_next.is_none()
    {
        s.ui_next = Some(frame);
    }
}

fn monotonic_now_ns() -> u64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid clock id and a timespec on the stack.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

/// Ask the kernel for its next vblank on this CRTC, as an event
/// (`DRM_VBLANK_EVENT`): when the grid of the mode is not known -- after a
/// mode change, before any page flip of the new mode -- a timed film frame
/// waits for it, and the event carries the vblank's own timestamp
/// (CLOCK_MONOTONIC, as page flips do). Refused, frames go up as they come.
fn ask_vblank_on_stage(kms: &SharedKms, crtc: control::crtc::Handle, s: &mut StageState) {
    if s.vblank_asked || !s.sink.wants_vblank() {
        return;
    }
    let pipe = kms.resource_handles().ok().and_then(|resources| resources.crtcs().iter().position(|c| *c == crtc));
    let asked = pipe.ok_or_else(|| "the CRTC is not the device's".to_string()).and_then(|pipe| {
        kms.wait_vblank(drm::VblankWaitTarget::Relative(1), drm::VblankWaitFlags::EVENT, pipe as u32, 0)
            .map_err(|error| error.to_string())
    });
    match asked {
        Ok(_) => {
            mediabox_core::timeline!("vblank-ask", "pipe={}", pipe.unwrap_or(0));
            s.vblank_asked = true;
        }
        Err(error) => {
            eprintln!("mediabox-tv.video the kernel will not report a vblank ({error}); film frames go up as they come");
            s.sink.vblank_unavailable();
        }
    }
}

/// The presentation thread: the player's socket and the display's events,
/// and each film frame committed for its vblank.
///
/// It runs at a real-time priority, as Android's SurfaceFlinger does
/// (SCHED_FIFO, main_surfaceflinger.cpp); a pass is well under a
/// millisecond (an import, a commit), so the CPUs are not taken from
/// anything. At an ordinary priority, with the CPUs busy (4 spinning
/// threads and 2 copying memory, load 7.6 on 8 cores) the loop was held
/// 24-26 ms in one pass and frames missed their vblank: 77 skipped in 25
/// minutes of NTV, none without the load (measured 2026-11-01). The kernel
/// keeps 5 % of each second for everything else (sched_rt_runtime_us).
fn presenter(stage: Arc<Stage>, kms: SharedKms, crtc: control::crtc::Handle) {
    /// Where SCHED_FIFO is refused -- on this appliance always: the kernel
    /// is built with CONFIG_RT_GROUP_SCHED and the root cgroup hands the
    /// cpu controller down (`cgroup.subtree_control`: cpu memory pids), so
    /// every service's group has no real-time budget, which cgroup v2 has
    /// no file to give it, and sched_setscheduler is EPERM for root with
    /// CAP_SYS_NICE (measured 2026-11-04). The strongest ordinary weight
    /// instead, well above the interface's (`INTERFACE_NICE`): CFS gives
    /// nice -20 88761 against nice -5's 3121, and the presenter's passes
    /// are a fraction of a millisecond.
    const NICE: libc::c_int = -20;
    let param = libc::sched_param { sched_priority: 2 };
    // SAFETY: sets the calling thread's policy; `param` is valid.
    let rc = unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) };
    let mut nice = None;
    if rc != 0 {
        // Refused where the kernel groups real-time time by cgroup
        // (CONFIG_RT_GROUP_SCHED) and systemd has the cpu controller on.
        let fifo = std::io::Error::last_os_error();
        // SAFETY: gettid and setpriority on this thread only.
        let set = unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, NICE) };
        nice = (set == 0).then_some(NICE);
        eprintln!(
            "mediabox-tv.platform the presentation thread: no SCHED_FIFO ({fifo}); nice {}",
            nice.map_or_else(|| format!("refused ({})", std::io::Error::last_os_error()), |n| n.to_string())
        );
    }
    mediabox_core::timeline!("sched", "thread=present fifo={} nice={:?}", u8::from(rc == 0), nice);
    /// The interface's frame waits for the film's frame of the vblank it
    /// would go up at -- until this long before that vblank, and no longer.
    /// Sent the moment the last vblank passed, it took the CRTC from the
    /// film frame of that period, which then waited a whole period, the
    /// next one arrived on top of it and the older was given back unseen:
    /// 76 of 552 film frames with a key held over a 4K film (2026-10-12).
    const UI_MARGIN: Duration = Duration::from_millis(4);
    loop {
        let (video, timeout) = {
            let s = stage.lock();
            if s.released {
                return;
            }
            if mediabox_core::timeline::enabled() {
                s.sink.trace_first_wait(s.waiting_for_flip, s.ui_on_panel);
            }
            let idle = s.waiting_for_flip || !s.ui_on_panel || s.hold;
            let mut timeout = Duration::from_millis(250);
            if let Some(until) = s.sink.until_due().filter(|until| !(until.is_zero() && idle)) {
                timeout = timeout.min(until);
            }
            if !idle && s.ui_next.is_some() {
                timeout = timeout.min(ui_wait(&s, UI_MARGIN));
            }
            // A flip overdue is looked for on the planes (`recover_flip`).
            if let Some(inflight) = s.inflight {
                let period = Duration::from_nanos(s.period_ns.max(1));
                let overdue = period * FLIP_OVERDUE_HALF_PERIODS / 2;
                let next = if inflight.at.elapsed() < overdue {
                    overdue
                } else if inflight.at.elapsed() < period * 3 {
                    period * 3
                } else {
                    period * FLIP_UNAPPLIED_PERIODS
                };
                timeout = timeout.min(next.saturating_sub(inflight.at.elapsed()).max(Duration::from_millis(1)));
            }
            (s.sink.poll_fd(), timeout.as_micros().div_ceil(1000) as i32)
        };
        let mut pollfds = [
            libc::pollfd { fd: video, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: kms.as_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: stage.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let asleep = mediabox_core::timeline::now_ns();
        // SAFETY: three valid pollfd entries on the stack.
        let rc = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, timeout) };
        // Woken by the timeout: how long past it this thread ran again.
        let woke_late = (rc == 0).then(|| mediabox_core::timeline::now_ns().saturating_sub(asleep + timeout.max(0) as u64 * 1_000_000));
        if rc < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            eprintln!("mediabox-tv.platform presentation poll: {}", std::io::Error::last_os_error());
            std::thread::sleep(Duration::from_millis(5));
        }
        if pollfds[2].revents & libc::POLLIN != 0 {
            let mut value: u64 = 0;
            // SAFETY: reads the eventfd's eight-byte counter.
            unsafe { libc::read(stage.wake.as_raw_fd(), (&mut value as *mut u64).cast(), 8) };
        }
        let started = mediabox_core::timeline::now_ns();
        let mut s = stage.lock();
        let locked = mediabox_core::timeline::now_ns();
        if s.released {
            return;
        }
        if pollfds[1].revents & libc::POLLIN != 0 {
            read_display_events(&kms, crtc, &mut s);
        }
        if pollfds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 || s.sink.has_pending() {
            let (width, height) = s.panel;
            let into = crate::video::Rect { x: 0, y: 0, width, height };
            let state = &mut *s;
            state.sink.pump(&kms, crtc, &state.video, into);
            VIDEO.store(state.sink.showing() || state.sink.has_pending(), std::sync::atomic::Ordering::Relaxed);
            *ARRIVED.lock().unwrap_or_else(|e| e.into_inner()) = state.sink.last_arrival();
            // The interface draws the subtitle and tells the television
            // about HDR: wake it when either has changed.
            if state.sink.subtitle().0 != state.subtitle_seen || state.sink.hdr() != state.hdr_seen {
                let _ = slint::invoke_from_event_loop(|| {});
            }
        }
        if s.waiting_for_flip {
            recover_flip(&kms, crtc, &mut s);
        }
        ask_vblank_on_stage(&kms, crtc, &mut s);
        if !s.waiting_for_flip && s.ui_on_panel && !s.hold {
            let film = s.sink.has_pending() || s.sink.wants_veil();
            let ui = s.ui_next.is_some() && (film || ui_wait(&s, UI_MARGIN).is_zero());
            if film || ui {
                let frame = if ui { s.ui_next.take() } else { None };
                match commit_on_stage(&kms, crtc, &mut s, frame) {
                    Ok(_) => {}
                    Err(CommitError::Busy) => {
                        // A commit of ours the kernel still holds, its flip
                        // not read: waited for like one in flight, and
                        // recovered from the planes if it never comes.
                        mediabox_core::timeline!("commit-busy", "ui={}", frame.is_some());
                        if frame.is_some() && s.ui_next.is_none() {
                            s.ui_next = frame;
                        }
                        s.waiting_for_flip = true;
                        s.flip_ui_seq = None;
                        s.inflight = Some(Inflight { at: std::time::Instant::now(), ui: None, film: None, vblank: None });
                        s.stuck_since.get_or_insert_with(std::time::Instant::now);
                    }
                    Err(error) => {
                        eprintln!("mediabox-tv.video {error}");
                        if let Some((_, _, seq)) = frame {
                            // Not on the panel: the interface hears of it.
                            s.ui_error = Some(error.to_string());
                            s.ui_done_seq = s.ui_done_seq.max(seq);
                        }
                    }
                }
            }
        }
        drop(s);
        stage.changed.notify_all();
        let done = mediabox_core::timeline::now_ns();
        crate::monitor::presenter_pass(woke_late, locked - started, done - locked);
        if mediabox_core::timeline::enabled() && done - started > 2_000_000 {
            mediabox_core::timeline::write(
                "present-slow",
                format_args!(
                    "lock={:.2} pass={:.2} revents={},{},{}",
                    (locked - started) as f64 / 1e6,
                    (done - locked) as f64 / 1e6,
                    pollfds[0].revents,
                    pollfds[1].revents,
                    pollfds[2].revents
                ),
            );
        }
    }
}

/// How long the interface's waiting frame still waits for the film's frame
/// of the coming vblank: zero once no film is moving, or `margin` before
/// that vblank.
fn ui_wait(s: &StageState, margin: Duration) -> Duration {
    if !s.sink.moving(Duration::from_millis(120)) {
        return Duration::ZERO;
    }
    let Some(flip) = s.last_flip else {
        return Duration::ZERO;
    };
    let deadline = flip + Duration::from_nanos(s.period_ns).saturating_sub(margin);
    deadline.saturating_duration_since(std::time::Instant::now())
}

/// Where a frame's time actually goes, split at the three boundaries this
/// platform owns.
///
/// The brief for this work said to measure before optimising, and there was
/// nothing to measure with: the interface reported frames per second and
/// nothing about what a frame was spent on. These four numbers separate the
/// scene (Slint laying out and FemtoVG drawing it) from the display pipeline
/// (waiting for the panel, handing the buffer to EGL, and the KMS calls), which
/// is the difference between a renderer problem and a scanout problem.
#[derive(Clone, Copy, Default)]
pub struct Phases {
    pub frames: u32,
    /// Everything `FemtoVGRenderer::render` did, including the three below.
    pub total_us: u64,
    /// Blocked on the previous page flip — the panel's own pace, not ours.
    pub flip_wait_us: u64,
    /// `eglSwapBuffers` on the Mali GBM surface.
    pub swap_us: u64,
    /// Locking the front buffer, and the page flip ioctl.
    pub present_us: u64,
}

impl Phases {
    /// Scene time: what is left once the display pipeline is taken out.
    pub fn draw_us(&self) -> u64 {
        self.total_us
            .saturating_sub(self.flip_wait_us)
            .saturating_sub(self.swap_us)
            .saturating_sub(self.present_us)
    }
}

thread_local! {
    static PHASES: Cell<Phases> = const { Cell::new(Phases {
        frames: 0, total_us: 0, flip_wait_us: 0, swap_us: 0, present_us: 0,
    }) };
    /// The panel's own mode, so the interface can state its frame budget in
    /// terms of what the television actually asked for rather than a constant.
    static MODE: Cell<(u32, u32, u32)> = const { Cell::new((0, 0, 0)) };
}

/// Where to write the next frame, when something has asked for one.
///
/// Taken by the swap that follows, so a snapshot is a real frame off the panel
/// rather than a re-render into a different surface: what lands in the file is
/// the image the display controller is about to scan out, at the panel's own
/// resolution.
thread_local! {
    static SNAPSHOT: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
}

/// Asks for the next drawn frame to be written to `path` as a PNG. Must be
/// called on the event loop's thread.
pub fn snapshot_to(path: std::path::PathBuf) {
    SNAPSHOT.with(|cell| *cell.borrow_mut() = Some(path));
}

/// Whether to rebuild each frame's framebuffer instead of keeping it with the
/// buffer. Off in production; the measurement path, and nothing else.
fn no_fb_cache() -> bool {
    static ANSWER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ANSWER.get_or_init(|| std::env::var_os("MEDIABOX_TV_NO_FB_CACHE").is_some())
}

fn add_phase(f: impl FnOnce(&mut Phases)) {
    PHASES.with(|cell| {
        let mut phases = cell.get();
        f(&mut phases);
        cell.set(phases);
    });
}

/// The last reporting window, kept so the diagnostics screen can show what the
/// renderer is doing without running a benchmark of its own.
#[derive(Clone, Copy, Default)]
pub struct Report {
    pub frames: u32,
    pub fps: f64,
    pub draw_ms: f64,
    pub flip_ms: f64,
}

thread_local! {
    static LAST: Cell<Report> = const { Cell::new(Report {
        frames: 0, fps: 0.0, draw_ms: 0.0, flip_ms: 0.0,
    }) };
}

pub fn set_last_report(report: Report) {
    LAST.with(|cell| cell.set(report));
}

pub fn last_report() -> Report {
    LAST.with(|cell| cell.get())
}

/// Takes the accumulated timings and starts a new window.
pub fn drain_phases() -> Phases {
    PHASES.with(|cell| cell.replace(Phases::default()))
}

/// Width, height and refresh of the mode the display controller is driving.
pub fn active_mode() -> (u32, u32, u32) {
    MODE.with(|cell| cell.get())
}

/// Set by the presentation thread (`presenter`), read by the interface.
static VIDEO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// When the film's last frame arrived from its player.
static ARRIVED: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// When the film's last frame arrived from its player, if one has.
pub fn video_arrived() -> Option<std::time::Instant> {
    *ARRIVED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether a film is on the video plane at this moment.
///
/// The interface is still drawn underneath it — the film is a window of this
/// process, not another application — but the plane sits above the primary, so
/// while this is true the panel is showing the film and not the catalogue.
pub fn video_showing() -> bool {
    VIDEO.load(std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    /// The picture subtitle on the film now, ready for the interface to draw,
    /// and the sink's change number it was made from.
    static PICTURE_SUBTITLE: RefCell<(u64, Option<(slint::Image, crate::video::SubtitlePlacement)>)> =
        const { RefCell::new((0, None)) };
}

/// The picture subtitle to draw over the film and where, if one is on.
///
/// The player draws nothing itself; a text subtitle the interface draws from
/// its words, and one that is a picture (a Blu-ray's PGS) from this.
pub fn picture_subtitle() -> Option<(slint::Image, crate::video::SubtitlePlacement)> {
    PICTURE_SUBTITLE.with(|cell| cell.borrow().1.clone())
}

/// Hand the sink's picture subtitle to the interface when it has changed.
fn publish_subtitle(changes: u64, shown: Option<(&crate::video::SubtitleImage, crate::video::SubtitlePlacement)>) {
    let changed = PICTURE_SUBTITLE.with(|cell| cell.borrow().0 != changes);
    if !changed {
        return;
    }
    let made = shown.map(|(image, placement)| {
        let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
            &image.rgba,
            image.width,
            image.height,
        );
        (slint::Image::from_rgba8_premultiplied(pixels), placement)
    });
    PICTURE_SUBTITLE.with(|cell| *cell.borrow_mut() = (changes, made));
    let _ = slint::invoke_from_event_loop(crate::picture_subtitle_changed);
}

/// A new mode and colour asked for, and what to go back to if the kernel
/// refuses them.
struct Switch {
    previous_mode: control::Mode,
    previous_colour: Option<mediabox_core::ColorMode>,
    previous_setting: (mediabox_core::OutputSetting, Option<u64>),
    resized: bool,
}

struct SplitDisplay {
    kms: SharedKms,
    /// The film's socket, its planes and the commit timing, shared with the
    /// presentation thread (`presenter`). The overlays the appliance's own
    /// player can draw into are enumerated at startup and held for the life
    /// of the process: asking the display controller for a window while a
    /// film is waiting to start is a question with a worse moment to ask it.
    stage: Arc<Stage>,
    connector: control::connector::Info,
    crtc: control::crtc::Handle,
    /// The mode on the wire. It changes when a person picks another one in
    /// the display settings, without this process starting again.
    mode: Cell<control::Mode>,
    /// The pixel clock the mode went on the wire with (kHz): the EDID's,
    /// scaled by the crystal's error (`on_the_wire`).
    wire_khz: Cell<u32>,
    gbm_device: gbm::Device<OwnedFd>,
    /// What the interface draws into, at the mode's size. Replaced when the
    /// size changes; `generation` counts the replacements so the EGL surface
    /// drawn through can follow.
    gbm_surface: RefCell<gbm::Surface<Scanout>>,
    generation: Cell<u64>,
    /// The surface of the previous size, kept until a frame of the new one is
    /// on the panel: the frame showing now was drawn into it.
    retired: RefCell<Option<gbm::Surface<Scanout>>>,
    /// A mode and colour asked for and not yet on the wire.
    switch: RefCell<Option<Switch>>,
    /// Everything this display offers, by the HDMI rules
    /// (`mediabox_platform::output`), computed once from the kernel's mode
    /// list and the EDID.
    offer: Option<mediabox_core::OutputOffer>,
    presentation: RefCell<Presentation>,
    /// The interface's own plane, found once it is showing a frame.
    primary: Cell<Option<control::plane::Handle>>,
    released: Cell<bool>,
    /// Whether the television is currently away, so the journal says so once
    /// rather than sixty times a second.
    dark: Cell<bool>,
    /// The setting on the wire, as the daemon last sent it (or as it was
    /// read from disk at start), and the daemon's trial it belongs to.
    setting: RefCell<mediabox_core::OutputSetting>,
    trial: Cell<Option<u64>>,
    /// What this source can send and how its driver is spoken to, resolved
    /// once against the running system (`mediabox_platform::source`).
    source: &'static mediabox_platform::source::SourceProfile,
    /// The mode in use, in the terms the colour rules are written in.
    timing: Cell<mediabox_platform::video::Timing>,
    /// The colour sent for everything but an HDR film: the one chosen for
    /// this mode if it can be sent here, what `Auto` gives otherwise.
    colour: Cell<Option<mediabox_core::ColorMode>>,
    /// The format and depth last written to the connector.
    applied: Cell<Option<mediabox_core::ColorMode>>,
    /// The last colour state written to the connector, so the properties are
    /// not rewritten on every frame -- and so the television is put back to
    /// SDR when the film ends rather than left in BT.2020.
    signalled: Cell<Option<crate::video::HdrStatic>>,
    /// What the film last asked for, so the decision is reported when the film
    /// changes and not only when the answer does. "Asked for HDR and was
    /// refused" is the interesting line, and it is the one that would
    /// otherwise never be printed.
    asked: Cell<Option<crate::video::HdrStatic>>,
}

impl SplitDisplay {
    /// Discovery, and then patience.
    ///
    /// A television that is switched off is not a broken appliance, and this
    /// is what the box used to do about one: `Platform::discover` found no
    /// connected output, this returned an error, the process exited 1, and
    /// `Restart=on-failure` started it again three seconds later. Measured on
    /// the Ultra on 2026-09-20: two hundred and fifty of those in eighteen
    /// minutes, each one a second and a half of CPU, and not one of them could
    /// have succeeded -- the connector was reading `disconnected` the whole
    /// time. The person watching saw nothing at all until they woke the set,
    /// and the journal had room for nothing else.
    ///
    /// So the interface waits instead. The connector is re-read once a second;
    /// the moment a television is plugged in or wakes up, the run continues
    /// into exactly the same code as if it had been there all along. Nothing
    /// else can be done in the meantime -- there is no panel to draw the
    /// waiting on -- and nothing else needs to be: this is a process whose
    /// entire job is the picture.
    ///
    /// It waits without a deadline on purpose. A deadline would only turn a
    /// television that is off for an hour back into the restart storm.
    fn wait_for_a_television() -> mediabox_platform::Platform {
        const BEAT: std::time::Duration = std::time::Duration::from_secs(1);
        let mut waited = 0u64;
        loop {
            let discovered = mediabox_platform::Platform::discover();
            if discovered.selected_output().is_some() {
                if waited > 0 {
                    eprintln!(
                        "mediabox-tv.platform a television answered after {waited}s of waiting"
                    );
                }
                return discovered;
            }
            // Once when it starts, then once a minute. The point of the line
            // is that somebody reading the journal knows the box is alive and
            // what it is waiting for, not that they can count the seconds.
            if waited == 0 {
                eprintln!(
                    "mediabox-tv.platform no television is connected; waiting for one \
                     (the interface does not give up, and does not restart)"
                );
            } else if waited % 60 == 0 {
                eprintln!("mediabox-tv.platform still no television after {waited}s");
            }
            std::thread::sleep(BEAT);
            waited += 1;
            // `main` blocks SIGTERM and SIGINT before anything here runs, so
            // that the display is released by ordinary code rather than from a
            // handler -- but the thread that consumes them is not started
            // until the event loop is. A process waiting here would therefore
            // ignore `systemctl stop` outright and be killed fifteen seconds
            // later, every time, including during a deploy. So the wait looks
            // for itself.
            if crate::session::exit_was_asked() {
                eprintln!("mediabox-tv.exit asked to stop while waiting for a television");
                std::process::exit(0);
            }
        }
    }

    fn new() -> Result<Rc<Self>, PlatformError> {
        // The two clocks' comparison starts here (`crystal_ppm`), and is
        // kept for the next process once its window is full.
        let _ = crystal_ppm();
        let start = raw_and_monotonic();
        let _ = std::thread::Builder::new().name("crystal".into()).spawn(move || {
            if crystal_kept().is_none() {
                crystal_early(start);
            }
            std::thread::sleep(CRYSTAL_WINDOW.saturating_sub(Duration::from_nanos(raw_and_monotonic().1 - start.1)) + Duration::from_millis(100));
            let _ = crystal_ppm();
        });
        let discovered = Self::wait_for_a_television();
        for warning in &discovered.warnings {
            eprintln!("mediabox-tv.platform {warning}");
        }
        let kms_path = discovered
            .kms
            .as_ref()
            .map(|node| node.device.display().to_string())
            .ok_or_else(|| {
                PlatformError::from("no DRM device on this board owns a connector".to_string())
            })?;
        let render_path = discovered
            .render
            .as_ref()
            .map(|node| node.device.display().to_string())
            .ok_or_else(|| {
                PlatformError::from(format!("no render device shares hardware with {kms_path}"))
            })?;
        // Present by construction: `wait_for_a_television` does not return
        // until discovery has chosen one.
        let wanted = discovered
            .selected_output()
            .ok_or_else(|| PlatformError::from("no connected display output".to_string()))?
            .connector
            .name
            .clone();

        let render_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&render_path)
            .map_err(|e| format!("open render node {render_path}: {e}"))?;
        let kms_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&kms_path)
            .map_err(|e| format!("open KMS node {kms_path}: {e}"))?;
        let kms = SharedKms(Arc::new(kms_file.into()));
        kms.acquire_master_lock()
            .or_else(|e| {
                if e.raw_os_error() == Some(libc::EINVAL) {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .map_err(|e| format!("acquire DRM master on {kms_path}: {e}"))?;

        // Ask to be shown every plane, not only the overlays.
        //
        // Without this the kernel hides the primary and cursor planes from
        // `drmModeGetPlaneResources`, and on this display controller the window
        // that carries video — Esmart0, plane 73 — reports its type as Cursor.
        // So the interface enumerated three planes, none of them the one its
        // own video port actually drives, and the best candidate for a film was
        // invisible to the process that needed it.
        if let Err(e) = kms.set_client_capability(drm::ClientCapability::UniversalPlanes, true) {
            eprintln!("mediabox-tv.platform universal planes unavailable: {e}");
        }

        // And to be shown the properties that only exist for an atomic client.
        //
        // `EOTF` -- which transfer curve the display controller reads a film's
        // plane with -- is one of them. Measured on the Plus: `modetest -p`
        // lists no EOTF at all, `modetest -a -p` lists eight, one per plane.
        // So this interface, which asked for no such capability, could not see
        // the property, could not write it, and reported nothing: a lookup
        // that finds nothing is not an error.
        //
        // What that cost is a picture. The property belongs to the plane and
        // outlives the process that set it, Kodi is an atomic client and
        // leaves it on ST 2084 after an HDR film, and the next film this
        // player put on that plane -- `format: NV12` -- was scanned out as
        // `HDR10[2]`. On the television: magenta.
        //
        // The capability is asked for, not required. Nothing below switches to
        // atomic commits; this only makes the driver's own properties visible
        // to the legacy path that was already setting COLOR_ENCODING and
        // COLOR_RANGE on the same plane.
        if let Err(e) = kms.set_client_capability(drm::ClientCapability::Atomic, true) {
            eprintln!("mediabox-tv.platform atomic properties unavailable: {e}");
        }

        let static_signature = mediabox_platform::source::static_signature(
            &mediabox_platform::Roots::from_env(),
            &discovered,
        );
        let (connector, crtc, mode, offer, colour, source, setting) =
            find_output(&kms, &wanted, &static_signature)?;
        let gbm_device = gbm::Device::new(OwnedFd::from(render_file))
            .map_err(|e| format!("create GBM device on {render_path}: {e}"))?;
        if gbm_device.backend_name() != "armsoc" {
            return Err(format!(
                "software/non-vendor GBM rejected: backend={}",
                gbm_device.backend_name()
            )
            .into());
        }
        let (width, height) = mode.size();
        let gbm_surface = gbm_device
            .create_surface::<Scanout>(
                width.into(),
                height.into(),
                gbm::Format::Argb8888,
                gbm::BufferObjectFlags::RENDERING
                    | gbm::BufferObjectFlags::SCANOUT
                    | gbm::BufferObjectFlags::LINEAR,
            )
            .map_err(|e| format!("create Mali GBM XRGB8888 surface: {e}"))?;

        eprintln!(
            "mediabox-tv.platform split-kms render={} gbm={} display={} output={}-{} \
             audio={} cec={} mode={}x{}@{}",
            render_path,
            gbm_device.backend_name(),
            kms_path,
            connector.interface().as_str(),
            connector.interface_id(),
            discovered
                .selected_output()
                .and_then(|output| output.audio_route().ok())
                .map(|audio| audio.card_id.as_str())
                .unwrap_or("-"),
            discovered
                .selected_output()
                .and_then(|output| output.cec_route().ok())
                .map(|cec| cec.device.display().to_string())
                .unwrap_or_else(|| "-".into()),
            width,
            height,
            mode.vrefresh()
        );

        MODE.with(|cell| cell.set((width.into(), height.into(), mode.vrefresh())));

        // The other windows of this video port. The interface keeps the primary
        // and keeps DRM master; a film goes on one of these, scaled and
        // converted by the display controller rather than by anything of ours.
        // The interface's own window goes on top of the film's, and its
        // surface carries alpha, so what this process draws is an overlay on a
        // film rather than a thing hidden behind one.
        match crate::video::raise_interface(&kms, crtc) {
            Some(raised) => eprintln!("mediabox-tv.video interface raised {raised}"),
            None => eprintln!(
                "mediabox-tv.video could not raise the interface above the film; \
                 player controls will not be visible"
            ),
        }

        let video = crate::video::VideoPlane::candidates(&kms, crtc, None);
        eprintln!(
            "mediabox-tv.video overlay candidates={} [{}]",
            video.len(),
            video
                .iter()
                .map(|plane| format!("{}:nv15={}", plane.name, plane.accepts(DrmFourcc::Nv15)))
                .collect::<Vec<_>>()
                .join(" ")
        );

        // The socket is opened whether or not anything is ever going to draw
        // on it: a player that starts and finds nobody listening has no second
        // way to reach the panel, and the interface is what outlives a film.
        let socket = std::env::var("MEDIABOX_VIDEO_SOCKET")
            .unwrap_or_else(|_| crate::video::DEFAULT_SOCKET.to_string());
        let sink = crate::video::Sink::bind(&socket)
            .map_err(|e| format!("listen for the player on {socket}: {e}"))?;
        eprintln!("mediabox-tv.video listening on {socket}");

        if let Some(offer) = &offer
            && let Some(offered) = offer.modes().find(|offered| same_mode(&mode, offered))
        {
            eprintln!(
                "mediabox-tv.platform sink {}: ST2084={} SMT1={} BT2020 rgb={} ycc={} link {} kHz, HDR10 at this mode: {}",
                &offer.edid_sha256[..12],
                offer.link.st2084,
                offer.link.static_metadata_type1,
                offer.link.bt2020_rgb,
                offer.link.bt2020_ycc,
                offer.link.max_character_rate_khz,
                offered.auto_hdr.map(|hdr| hdr.label()).unwrap_or_else(|| "yok".into())
            );
        }

        let identity = offer.as_ref().map(|offer| mediabox_core::DisplayIdentity {
            connector: offer.connector.clone(),
            edid_sha256: offer.edid_sha256.clone(),
        });
        IDENTITY.with(|cell| *cell.borrow_mut() = identity);
        {
            let (width, height) = mode.size();
            let (htotal, vtotal) = (u64::from(mode.hsync().2), u64::from(mode.vsync().2));
            mediabox_core::timeline!(
                "mode",
                "w={width} h={height} hz={} period={} at=start",
                mode.vrefresh(),
                htotal * vtotal * 1_000_000 / u64::from(mode.clock()).max(1)
            );
        }
        let stage = Arc::new(Stage {
            state: Mutex::new(StageState {
                sink,
                video,
                waiting_for_flip: false,
                flip_ui_seq: None,
                ui_next: None,
                ui_seq: 0,
                ui_done_seq: 0,
                ui_error: None,
                hold: false,
                inflight: None,
                stuck_since: None,
                flips_reconciled: 0,
                flips_requeued: 0,
                read_errors: 0,
                vblank_asked: false,
                last_flip: None,
                period_ns: period_of(&mode),
                panel: {
                    let (width, height) = mode.size();
                    (width.into(), height.into())
                },
                ui_on_panel: false,
                released: false,
                subtitle_seen: 0,
                hdr_seen: None,
            }),
            changed: Condvar::new(),
            wake: {
                // SAFETY: eventfd returns a new descriptor or -1.
                let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
                if fd < 0 {
                    return Err(format!("eventfd: {}", std::io::Error::last_os_error()).into());
                }
                // SAFETY: a descriptor just created and owned by nothing else.
                unsafe { OwnedFd::from_raw_fd(fd) }
            },
        });
        {
            let stage = Arc::clone(&stage);
            let kms = kms.clone();
            std::thread::Builder::new()
                .name("mediabox-tv-present".into())
                .spawn(move || presenter(stage, kms, crtc))
                .map_err(|e| PlatformError::from(format!("the presentation thread: {e}")))?;
        }
        let display = Rc::new(Self {
            kms,
            connector,
            crtc,
            mode: Cell::new(mode),
            wire_khz: Cell::new(0),
            setting: RefCell::new(setting),
            trial: Cell::new(None),
            source,
            timing: Cell::new(timing_of(&mode)),
            colour: Cell::new(colour),
            offer,
            applied: Cell::new(None),
            signalled: Cell::new(None),
            asked: Cell::new(None),
            gbm_device,
            gbm_surface: RefCell::new(gbm_surface),
            generation: Cell::new(0),
            retired: RefCell::new(None),
            switch: RefCell::new(None),
            stage: Arc::clone(&stage),
            presentation: RefCell::new(Presentation::default()),
            primary: Cell::new(None),
            released: Cell::new(false),
            dark: Cell::new(false),
        });
        eprintln!(
            "mediabox-tv.platform scale={:.3} logical={:.0}x{:.0} design={:.0}x{:.0}",
            display.scale_factor(),
            f32::from(width) / display.scale_factor(),
            f32::from(height) / display.scale_factor(),
            DESIGN.0,
            DESIGN.1
        );
        Ok(display)
    }

    fn size(&self) -> slint::PhysicalSize {
        let (width, height) = self.mode.get().size();
        slint::PhysicalSize::new(width.into(), height.into())
    }

    /// How many physical pixels one logical pixel is worth on this panel.
    ///
    /// The same arithmetic `mediabox-display-scale` does for the compositor
    /// this replaced, kept here because there is no longer a launcher in front
    /// of the binary to work it out and put it in the environment: the tighter
    /// of the two axes decides, so the design always fits and a panel taller
    /// than 16:9 gets the extra room as room rather than as a cropped edge, and
    /// whole numbers only, because a fractional scale buys nothing on a
    /// surface that is scanned out directly.
    ///
    /// `SLINT_SCALE_FACTOR` still wins, so a panel that wants something else
    /// can be told without a rebuild. Slint itself only reads that variable in
    /// its winit backend, which this platform replaces.
    fn scale_factor(&self) -> f32 {
        if let Some(forced) = std::env::var("SLINT_SCALE_FACTOR")
            .ok()
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|value| *value > 0.0)
        {
            return forced;
        }
        let (width, height) = self.mode.get().size();
        let fit = (f32::from(width) / DESIGN.0).min(f32::from(height) / DESIGN.1);
        // Rounded up rather than down: on a panel between two whole scales the
        // tighter one is the television-shaped answer.
        (fit - 1e-6).ceil().clamp(1.0, 4.0)
    }

    /// Until the interface's last frame handed over is on the panel (its
    /// commit's flip read by the presenter), and the frame before it given
    /// back to GBM.
    fn wait_for_page_flip(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Bounded by the presenter: a commit that never reaches the panel
        // is recovered or ends the process within `FLIP_FATAL`. Past five
        // seconds here something else is wrong, and it is an error -- the
        // frame before is never given back while it may still be on the
        // panel.
        let mut s = self.stage.lock();
        let started = std::time::Instant::now();
        let mut said = 0;
        while s.ui_on_panel && (s.ui_next.is_some() || s.ui_done_seq < s.ui_seq) {
            let waited = started.elapsed();
            if waited >= FLIP_LOST * 5 {
                mediabox_core::timeline!("ui-frame-lost", "waiting={} hold={}", s.waiting_for_flip, s.hold);
                return Err(format!(
                    "the interface's frame did not reach the panel in {waited:?} (commit in flight={}, hold={})",
                    s.waiting_for_flip, s.hold
                )
                .into());
            }
            if waited >= FLIP_LOST * (said + 1) {
                said += 1;
                eprintln!(
                    "mediabox-tv.platform the interface's frame is not on the panel after {waited:?} (commit in flight={} hold={})",
                    s.waiting_for_flip, s.hold
                );
            }
            s = self.stage.changed.wait_timeout(s, FLIP_LOST / 4).unwrap_or_else(|e| e.into_inner()).0;
        }
        let done = s.ui_done_seq;
        drop(s);
        self.release_shown(done);
        Ok(())
    }

    /// The mode as it goes on the wire, said once, its clock kept.
    fn wire_mode(&self) -> control::Mode {
        let mode = self.mode.get();
        let wire = on_the_wire(mode, crystal_ppm());
        self.wire_khz.set(wire.clock());
        say_wire(&mode, &wire);
        wire
    }

    /// Nothing more committed until the guard's `hold` is let go
    /// (`let_go`), and nothing in flight: for whatever changes the whole
    /// display -- a mode, the connector's colour, the hand-over.
    fn hold(&self) -> Held<'_> {
        // Until the commit in flight has flipped -- or been recovered by the
        // presenter (`recover_flip`), which ends the process if it cannot.
        // Never going on with a commit still in flight.
        let mut s = self.stage.lock();
        s.hold = true;
        let started = std::time::Instant::now();
        let mut said = 0;
        while s.waiting_for_flip && !s.released {
            let waited = started.elapsed();
            if waited >= FLIP_LOST * (said + 1) {
                said += 1;
                eprintln!("mediabox-tv.platform holding the display: the commit in flight has not flipped after {waited:?}");
                mediabox_core::timeline!("hold-waiting", "ms={}", waited.as_millis());
            }
            s = self.stage.changed.wait_timeout(s, FLIP_LOST / 4).unwrap_or_else(|e| e.into_inner()).0;
        }
        // Not under a change of the whole display: the veil's rectangle is
        // the mode's before it.
        let state = &mut *s;
        state.sink.drop_veil(&self.kms, self.crtc, &state.video);
        Held { stage: &self.stage, guard: Some(s) }
    }

    /// The interface frame the panel showed before the current one, given
    /// back to GBM once the commit that replaced it has flipped.
    fn release_shown(&self, done: u64) {
        let old = {
            let mut state = self.presentation.borrow_mut();
            if state.pending_previous.is_some() && done >= state.pending_seq {
                state.pending_previous.take()
            } else {
                None
            }
        };
        drop(old);
    }

    /// Locks whatever the renderer just finished and returns it ready to flip.
    ///
    /// The expensive half — dma-buf export, PRIME import, ADDFB2 — happens only
    /// the first time each of the surface's buffers is seen. After that this is
    /// one `gbm_surface_lock_front_buffer` and a handle read.
    fn import_front_buffer(
        &self,
    ) -> Result<PresentedFrame, Box<dyn std::error::Error + Send + Sync>> {
        let mut bo = unsafe { self.gbm_surface.borrow().lock_front_buffer() }
            .map_err(|e| format!("lock Mali GBM front buffer: {e}"))?;
        if bo.format() != gbm::Format::Argb8888 {
            return Err(format!("unexpected GBM format: {:?}", bo.format()).into());
        }

        if !no_fb_cache() {
            if let Some(scanout) = bo.userdata() {
                let framebuffer = scanout.framebuffer;
                return Ok(PresentedFrame {
                    framebuffer,
                    _bo: bo,
                    _uncached: None,
                });
            }
        }

        let dma_buf = bo
            .fd_for_plane(0)
            .map_err(|e| format!("export GBM BO as dma-buf: {e}"))?;
        let imported_handle = self
            .kms
            .prime_fd_to_buffer(dma_buf.as_fd())
            .map_err(|e| format!("DRM_IOCTL_PRIME_FD_TO_HANDLE(display device): {e}"))?;
        let plane = ImportedPlane {
            handle: imported_handle,
            size: (bo.width(), bo.height()),
            pitch: bo.stride_for_plane(0),
            offset: bo.offset(0),
            modifier: bo.modifier(),
        };
        let flags = if plane.modifier().is_some() {
            control::FbCmd2Flags::MODIFIERS
        } else {
            control::FbCmd2Flags::empty()
        };
        let framebuffer = match self.kms.add_planar_framebuffer(&plane, flags) {
            Ok(fb) => fb,
            Err(e) => {
                let _ = self.kms.close_buffer(imported_handle);
                return Err(format!("DRM_IOCTL_MODE_ADDFB2(PRIME buffer): {e}").into());
            }
        };

        let imports = {
            let mut state = self.presentation.borrow_mut();
            state.imports += 1;
            state.imports
        };
        // A healthy run prints this two or three times and never again — once
        // per buffer the GBM surface owns. The cap is for the measurement path
        // below, where every frame imports: a line per frame would be the
        // journal measuring itself.
        if imports <= 8 {
            eprintln!(
                "mediabox-tv.kms scanout-buffer n={} fb={} modifier={:?} pitch={}",
                imports,
                u32::from(framebuffer),
                bo.modifier(),
                bo.stride_for_plane(0)
            );
        }

        if no_fb_cache() {
            // The measurement path: the framebuffer and the PRIME handle are
            // torn down as soon as the frame is off the panel, so the next
            // frame pays for them again. This is what the platform used to do
            // on every frame, kept behind a variable so the cost of it can be
            // measured on the appliance rather than argued about.
            return Ok(PresentedFrame {
                framebuffer,
                _bo: bo,
                _uncached: Some(Scanout {
                    kms: self.kms.clone(),
                    framebuffer,
                    imported_handle,
                    _dma_buf: dma_buf,
                }),
            });
        }

        bo.set_userdata(Scanout {
            kms: self.kms.clone(),
            framebuffer,
            imported_handle,
            _dma_buf: dma_buf,
        });

        Ok(PresentedFrame {
            framebuffer,
            _bo: bo,
            _uncached: None,
        })
    }

    /// Is there still a television on the other end of the cable?
    ///
    /// Asked of the connector rather than of the error, because the errors a
    /// vanished sink produces are not one error: a flip can come back EACCES,
    /// ENOENT or EINVAL depending on how far the modeset had got. `false` for
    /// the probe argument: this reads what the driver already knows from the
    /// hot-plug line and does not make it re-read the EDID sixty times a
    /// second.
    fn television_is_gone(&self) -> bool {
        match self.kms.get_connector(self.connector.handle(), false) {
            Ok(info) => info.state() == control::connector::State::Disconnected,
            // If the connector cannot be read at all, something worse than an
            // unplugged cable is happening and the caller should hear about it.
            Err(_) => false,
        }
    }

    /// A frame on the panel -- or nothing at all, if there is no panel.
    ///
    /// Somebody switching the television off, or to another input, is an
    /// ordinary thing for an appliance and used to be fatal here: the flip
    /// failed, the error went up through Slint, and the process exited. That
    /// is what began the worst run this box has had. Measured on the Ultra on
    /// 2026-09-20: the interface died at 12:48:29 with a film playing, the
    /// display controller was left disabled, and because the connector then
    /// read `disconnected` every restart failed in the same place -- two
    /// hundred and fifty of them over eighteen minutes, until the set was
    /// woken by hand. The film, owned by nobody, played its sound throughout.
    ///
    /// So a failure with no television behind it is not a failure. The
    /// interface stays up, says so once, and throws away what it knew about
    /// the display: the kernel's console owns the controller while we are
    /// away, so the frame after the set comes back has to set the mode again
    /// rather than flip onto somebody else's configuration.
    fn present(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match self.present_frame() {
            Ok(()) => {
                if self.dark.replace(false) {
                    eprintln!("mediabox-tv.platform the television is back");
                }
                Ok(())
            }
            Err(error) => {
                if !self.television_is_gone() {
                    return Err(error);
                }
                if !self.dark.replace(true) {
                    eprintln!(
                        "mediabox-tv.platform the television went away ({error}); \
                         the interface stays up and waits for it"
                    );
                }
                {
                    let mut s = self.stage.lock();
                    s.waiting_for_flip = false;
                    s.flip_ui_seq = None;
                    s.ui_next = None;
                    s.ui_done_seq = s.ui_seq;
                    s.ui_on_panel = false;
                }
                let mut state = self.presentation.borrow_mut();
                state.pending_previous = None;
                state.current = None;
                Ok(())
            }
        }
    }

    fn present_frame(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.switch.borrow().is_some() {
            return self.commit_switch();
        }
        let frame = self.import_front_buffer()?;
        if self.presentation.borrow().current.is_none() {
            {
                // Nothing may be committed while the CRTC is set.
                let mut s = self.hold();
                let set = self.kms.set_crtc(
                    self.crtc,
                    Some(frame.framebuffer),
                    (0, 0),
                    &[self.connector.handle()],
                    Some(self.wire_mode()),
                );
                if set.is_ok() {
                    s.ui_on_panel = true;
                }
                drop(s);
                set.map_err(|e| format!("DRM_IOCTL_MODE_SETCRTC(first frame): {e}"))?;
            }
            self.presentation.borrow_mut().current = Some(frame);
        } else {
            // One interface frame at a time: the one before is on the panel
            // and the one before that back with GBM.
            self.wait_for_page_flip()?;
            let plane = self.primary_plane().ok_or("the interface's plane is not on the panel")?;
            let seq = {
                let mut s = self.stage.lock();
                if let Some(error) = s.ui_error.take() {
                    return Err(format!("atomic commit: {error}").into());
                }
                s.ui_seq += 1;
                let seq = s.ui_seq;
                s.ui_next = Some((plane, frame.framebuffer, seq));
                seq
            };
            // The presenter puts it up, with the film's frame of its vblank.
            self.stage.poke();
            let old = {
                let mut state = self.presentation.borrow_mut();
                let shown = state.current.replace(frame);
                state.pending_seq = seq;
                std::mem::replace(&mut state.pending_previous, shown)
            };
            // Its successor's flip was read before this one was handed over.
            drop(old);
        }
        let mut state = self.presentation.borrow_mut();
        if !state.first_frame_logged {
            let current = state.current.as_ref().unwrap();
            eprintln!(
                "mediabox-tv.present dma-buf=active scanout_fb={} native={}x{}",
                u32::from(current.framebuffer),
                self.size().width,
                self.size().height
            );
            state.first_frame_logged = true;
            crate::fdstore::release_inherited();
            drop(state);
            self.signal_output_colour(self.asked.get());
            self.report();
            return Ok(());
        }
        Ok(())
    }

    /// One refresh of the mode on the wire, from its own timing.
    fn frame_period(&self) -> Duration {
        Duration::from_nanos(period_of(&self.mode.get()))
    }

    /// What the presentation thread has for the interface: the picture
    /// subtitle to draw, and whether the film asks for HDR. Read on every
    /// pass of the event loop; the presenter wakes it when either changes.
    fn sync_presented(&self) {
        let hdr = {
            let mut s = self.stage.lock();
            let (changes, shown) = s.sink.subtitle();
            publish_subtitle(changes, shown);
            s.subtitle_seen = changes;
            s.hdr_seen = s.sink.hdr();
            s.hdr_seen
        };
        self.signal_output_colour(hdr);
        let done = self.stage.lock().ui_done_seq;
        self.release_shown(done);
    }

    /// The interface's plane: the one on this CRTC showing its frame.
    fn primary_plane(&self) -> Option<control::plane::Handle> {
        if let Some(plane) = self.primary.get() {
            return Some(plane);
        }
        let current = self.presentation.borrow().current.as_ref().map(|frame| frame.framebuffer)?;
        let plane = self.kms.plane_handles().ok()?.into_iter().find(|handle| {
            self.kms
                .get_plane(*handle)
                .is_ok_and(|plane| plane.crtc() == Some(self.crtc) && plane.framebuffer() == Some(current))
        })?;
        self.primary.set(Some(plane));
        Some(plane)
    }

    /// Tell the television what it is being sent.
    ///
    /// The plane's `EOTF` is how the display controller reads the film; this
    /// is the other half, and without it a PQ picture goes out over a link
    /// still describing itself as SDR BT.709 -- which is what this appliance
    /// did until now, while Kodi on the same board set all three of these and
    /// got HDR10 on the wire.
    ///
    /// Three properties, the same ones measured on Kodi here:
    ///   * `HDR_OUTPUT_METADATA`, the CTA-861 mastering infoframe;
    ///   * `Colorspace`, BT2020_RGB;
    ///   * `color_depth`, ten bits, because eight-bit HDR bands visibly.
    ///
    /// And one decision before them: whether HDR10 can go out at the timing
    /// that is actually set, and in what. That is the offer's answer -- the
    /// same one Kodi's plan carries -- and it holds every condition apart:
    /// the sink's PQ, Static Metadata Type 1 and BT.2020 in the encoding
    /// sent, ten bits, the link, the Y420 rules and the source profile. A 4K
    /// film at 23.976 on a 300 MHz input goes out as 4:2:2 at ten bits; at
    /// 60 Hz nothing carries it, and HDR is given up rather than asked for
    /// and silently subsampled.
    fn signal_output_colour(&self, film: Option<crate::video::HdrStatic>) {
        let offered = self.offer.as_ref().and_then(|offer| {
            let mode = offer.modes().find(|offered| same_mode(&self.mode.get(), offered))?;
            Some((offer, mode))
        });
        let setting = self.setting.borrow().clone();
        let hdr_colour = offered.and_then(|(offer, mode)| offer.hdr_colour(&setting, mode));
        let fits = hdr_colour.is_some();
        let wanted = match film {
            Some(hdr) if hdr.is_hdr() && fits => Some(hdr),
            _ => None,
        };
        if self.asked.replace(film) != film {
            match film {
                Some(hdr) if hdr.is_hdr() && !fits => eprintln!(
                    "mediabox-tv.platform the film asks for HDR (eotf {}) and HDR10 cannot go \
                     out at {}: sending SDR, the plane is tone-mapped",
                    hdr.eotf,
                    self.timing.get().label()
                ),
                Some(hdr) if hdr.is_hdr() => eprintln!(
                    "mediabox-tv.platform the film asks for HDR (eotf {}), and it fits",
                    hdr.eotf
                ),
                Some(_) => eprintln!("mediabox-tv.platform the film is SDR"),
                None => {}
            }
        }
        // The format and depth on the wire: for SDR the colour chosen for this
        // mode (already checked against it) or what `Auto` sends; for an HDR
        // film the HDR10 colour the offer names.
        let timing = self.timing.get();
        let format = match wanted {
            Some(_) => hdr_colour,
            None => self.colour.get(),
        };
        if self.signalled.get() == wanted && self.applied.get() == format {
            return;
        }

        // The connector's properties change while nothing is committed.
        let _held = self.hold();
        let connector = self.connector.handle();
        let Ok(properties) = self.kms.get_properties(connector) else {
            return;
        };
        let mut metadata = None;
        let mut colorspace = None;
        let mut depth = None;
        let mut pixel_format = None;
        for handle in properties.as_props_and_values().0.iter().copied() {
            let Ok(info) = self.kms.get_property(handle) else {
                continue;
            };
            match info.name().to_str() {
                Ok("HDR_OUTPUT_METADATA") => metadata = Some(handle),
                Ok("Colorspace") => colorspace = Some(handle),
                Ok("color_depth") => depth = Some(handle),
                Ok("color_format") => pixel_format = Some(handle),
                _ => {}
            }
        }

        // `Colorspace` is the standard enum: Default=0, BT2020_RGB=9,
        // BT2020_YCC=10. `color_depth` and `color_format` are the vendor's,
        // and their values come from the source profile, which was matched
        // against this driver's own enums -- under a profile with no vendor
        // ABI they are left to the driver.
        use mediabox_core::ColorFormat;
        let (space, bits, layout) = match format {
            Some(mode) => (
                match (wanted, mode.format) {
                    (Some(_), ColorFormat::Rgb) => 9,
                    (Some(_), _) => 10,
                    (None, _) => 0,
                },
                self.source.color_depth_value(mode.bits),
                self.source.color_format_value(mode.format),
            ),
            // No EDID to reason from: the driver's own choice (`Automatic`).
            None => (0, self.source.abi.map(|_| 0), None),
        };
        let blob = match wanted {
            Some(hdr) => match self.kms.create_property_blob(&hdr.blob()) {
                Ok(value) => Some(value),
                Err(e) => {
                    eprintln!("mediabox-tv.platform HDR metadata blob: {e}");
                    None
                }
            },
            None => None,
        };
        if let Some(property) = metadata {
            let value = blob
                .map(|value| {
                    let raw: u64 = value.into();
                    raw
                })
                .unwrap_or(0);
            if let Err(e) = self.kms.set_property(connector, property, value) {
                eprintln!("mediabox-tv.platform HDR_OUTPUT_METADATA: {e}");
            }
        }
        if let Some(property) = colorspace
            && let Err(e) = self.kms.set_property(connector, property, space)
        {
            eprintln!("mediabox-tv.platform Colorspace: {e}");
        }
        if let (Some(property), Some(bits)) = (depth, bits)
            && let Err(e) = self.kms.set_property(connector, property, bits)
        {
            eprintln!("mediabox-tv.platform color_depth: {e}");
        }
        if let (Some(property), Some(layout)) = (pixel_format, layout)
            && let Err(e) = self.kms.set_property(connector, property, layout)
        {
            eprintln!("mediabox-tv.platform color_format: {e}");
        }
        self.applied.set(format);
        eprintln!(
            "mediabox-tv.platform output colour: {}",
            match wanted {
                Some(hdr) => format!(
                    "HDR10 eotf {} peak {} cd/m2 at {} kHz",
                    hdr.eotf,
                    hdr.max_luminance,
                    self.mode.get().clock()
                ),
                None => "SDR".to_string(),
            }
        );
        eprintln!(
            "mediabox-tv.platform output format: {} at {}",
            format.map(|mode| mode.label()).unwrap_or_else(|| "sürücünün seçimi".into()),
            timing.label()
        );
        self.signalled.set(wanted);
    }

    /// Asks for `mode`, with `colour` on the wire, from the next frame.
    ///
    /// A different size needs a surface of that size to draw the next frame
    /// into; the one the panel is showing is kept until that frame is up.
    /// Nothing reaches the display controller here: the next frame is
    /// committed with the mode in one atomic commit (see `commit_switch`).
    fn begin_switch(
        &self,
        mode: control::Mode,
        colour: Option<mediabox_core::ColorMode>,
        setting: mediabox_core::OutputSetting,
        trial: Option<u64>,
    ) -> Result<bool, String> {
        self.wait_for_page_flip().map_err(|e| e.to_string())?;
        let previous_mode = self.mode.get();
        let resized = mode.size() != previous_mode.size();
        if resized {
            let (width, height) = mode.size();
            let surface = self
                .gbm_device
                .create_surface::<Scanout>(
                    width.into(),
                    height.into(),
                    gbm::Format::Argb8888,
                    gbm::BufferObjectFlags::RENDERING
                        | gbm::BufferObjectFlags::SCANOUT
                        | gbm::BufferObjectFlags::LINEAR,
                )
                .map_err(|e| format!("create a {width}x{height} surface: {e}"))?;
            let old = self.gbm_surface.replace(surface);
            *self.retired.borrow_mut() = Some(old);
            self.generation.set(self.generation.get() + 1);
        }
        *self.switch.borrow_mut() = Some(Switch {
            previous_mode,
            previous_colour: self.colour.get(),
            previous_setting: (self.setting.replace(setting), self.trial.replace(trial)),
            resized,
        });
        self.mode.set(mode);
        self.timing.set(timing_of(&mode));
        self.colour.set(colour);
        let (width, height) = mode.size();
        MODE.with(|cell| cell.set((width.into(), height.into(), mode.vrefresh())));
        mediabox_core::timeline!(
            "mode-req",
            "w={width} h={height} hz={} clock_khz={}",
            mode.vrefresh(),
            mode.clock()
        );
        Ok(resized)
    }

    /// The first frame after `begin_switch`, with the mode and the colour, in
    /// one atomic commit -- asked first with `TEST_ONLY`, so a mode the kernel
    /// will not take changes nothing. One commit is one link training: the
    /// television resynchronises once, not once for the mode and again for
    /// each colour property.
    fn commit_switch(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let frame = self.import_front_buffer()?;
        let switch = self.switch.borrow_mut().take().expect("a switch was asked for");
        let mode = self.mode.get();
        let colour = self.colour.get();
        // The presentation stands still while the mode changes: a film frame
        // committed in the middle of it would be on the old mode's grid.
        self.wait_for_page_flip()?;
        let mut stage = self.hold();
        let modeset = self.atomic_modeset(frame.framebuffer, mode, colour);
        if modeset.is_ok() {
            stage.waiting_for_flip = false;
            stage.flip_ui_seq = None;
            stage.period_ns = period_of(&mode);
            let (width, height) = mode.size();
            stage.panel = (width.into(), height.into());
            stage.ui_on_panel = true;
            // The vblanks known were of the mode before.
            stage.sink.forget_vblank();
        }
        drop(stage);
        match modeset {
            Ok(()) => {
                let (pending, current) = {
                    let mut state = self.presentation.borrow_mut();
                    (state.pending_previous.take(), state.current.replace(frame))
                };
                // The frames of the old size first, then the surface they came
                // from.
                drop(pending);
                drop(current);
                drop(self.retired.borrow_mut().take());
                self.applied.set(colour);
                self.signalled.set(None);
                OUTPUT_ERROR.with(|cell| cell.borrow_mut().take());
                let (width, height) = mode.size();
                mediabox_core::timeline!(
                    "mode",
                    "w={width} h={height} hz={} period={}",
                    mode.vrefresh(),
                    self.frame_period().as_nanos()
                );
                eprintln!(
                    "mediabox-tv.platform output now {} {}",
                    self.timing.get().label(),
                    colour.map(|mode| mode.label()).unwrap_or_else(|| "sürücünün seçimi".into())
                );
                // An HDR film, if one is playing, is signalled again on top.
                self.signal_output_colour(self.asked.get());
            }
            Err(error) => {
                mediabox_core::timeline!("mode-refused", "hz={}", mode.vrefresh());
                eprintln!(
                    "mediabox-tv.platform the kernel refused {} {}: {error}; staying on {}",
                    self.timing.get().label(),
                    colour.map(|mode| mode.label()).unwrap_or_default(),
                    timing_of(&switch.previous_mode).label()
                );
                OUTPUT_ERROR.with(|cell| {
                    *cell.borrow_mut() = Some(format!("Çekirdek bu modu kabul etmedi: {error}"))
                });
                if let Some(identity) = self.identity() {
                    report(mediabox_core::OwnerReport::Failed {
                        identity,
                        trial: self.trial.get(),
                        error: error.clone(),
                    });
                }
                let (setting, trial) = switch.previous_setting;
                *self.setting.borrow_mut() = setting;
                self.trial.set(trial);
                self.mode.set(switch.previous_mode);
                self.timing.set(timing_of(&switch.previous_mode));
                self.colour.set(switch.previous_colour);
                let (width, height) = switch.previous_mode.size();
                MODE.with(|cell| {
                    cell.set((width.into(), height.into(), switch.previous_mode.vrefresh()))
                });
                drop(frame);
                if switch.resized
                    && let Some(old) = self.retired.borrow_mut().take()
                {
                    drop(self.gbm_surface.replace(old));
                    self.generation.set(self.generation.get() + 1);
                }
            }
        }
        self.report();
        Ok(())
    }

    /// The commit itself: this CRTC on this connector at `mode`, the primary
    /// plane showing `framebuffer` over the whole of it, and the connector's
    /// colour properties -- tested, then applied.
    fn atomic_modeset(
        &self,
        framebuffer: control::framebuffer::Handle,
        mode: control::Mode,
        colour: Option<mediabox_core::ColorMode>,
    ) -> Result<(), String> {
        use control::property::Value;
        let current = self
            .presentation
            .borrow()
            .current
            .as_ref()
            .map(|frame| frame.framebuffer);
        // The plane the interface is on: the one on this CRTC showing the
        // frame on the panel now.
        let plane = self
            .kms
            .plane_handles()
            .map_err(|e| format!("planes: {e}"))?
            .into_iter()
            .find(|handle| {
                self.kms.get_plane(*handle).is_ok_and(|plane| {
                    plane.crtc() == Some(self.crtc)
                        && current.is_some()
                        && plane.framebuffer() == current
                })
            })
            .ok_or("the interface's plane is not on the panel")?;
        fn need<H: control::ResourceHandle>(
            kms: &SharedKms,
            object: H,
            name: &str,
        ) -> Result<control::property::Handle, String> {
            find_property(kms, object, name).ok_or_else(|| format!("no {name} property"))
        }
        let connector = self.connector.handle();
        let (width, height) = mode.size();
        let wire = on_the_wire(mode, crystal_ppm());
        let blob = self
            .kms
            .create_property_blob(&wire)
            .map_err(|e| format!("mode blob: {e}"))?;
        self.wire_khz.set(wire.clock());
        say_wire(&mode, &wire);
        let mut request = control::atomic::AtomicModeReq::new();
        request.add_property(connector, need(&self.kms, connector, "CRTC_ID")?, Value::CRTC(Some(self.crtc)));
        request.add_property(self.crtc, need(&self.kms, self.crtc, "MODE_ID")?, blob);
        request.add_property(self.crtc, need(&self.kms, self.crtc, "ACTIVE")?, Value::Boolean(true));
        let at = |name: &str, value: Value<'static>| -> Result<(control::property::Handle, Value<'static>), String> {
            Ok((need(&self.kms, plane, name)?, value))
        };
        for (property, value) in [
            at("FB_ID", Value::Framebuffer(Some(framebuffer)))?,
            at("CRTC_ID", Value::CRTC(Some(self.crtc)))?,
            at("SRC_X", Value::UnsignedRange(0))?,
            at("SRC_Y", Value::UnsignedRange(0))?,
            at("SRC_W", Value::UnsignedRange(u64::from(width) << 16))?,
            at("SRC_H", Value::UnsignedRange(u64::from(height) << 16))?,
            at("CRTC_X", Value::SignedRange(0))?,
            at("CRTC_Y", Value::SignedRange(0))?,
            at("CRTC_W", Value::UnsignedRange(width.into()))?,
            at("CRTC_H", Value::UnsignedRange(height.into()))?,
        ] {
            request.add_property(plane, property, value);
        }
        // `color_format` and `color_depth` as the source profile's vendor ABI
        // numbers them (on this driver rgb=0, ycbcr444=1, ycbcr422=2,
        // ycbcr420=3; depth in bits); SDR is `Colorspace` Default and no HDR
        // metadata. A profile with no vendor ABI writes neither vendor
        // property: the driver's own choice is safer than a guessed number.
        if let Some(colour) = colour {
            for (name, value) in [
                ("color_format", self.source.color_format_value(colour.format)),
                ("color_depth", self.source.color_depth_value(colour.bits)),
                ("Colorspace", Some(0)),
                ("HDR_OUTPUT_METADATA", Some(0)),
            ] {
                if let Some(value) = value
                    && let Some(property) = find_property(&self.kms, connector, name)
                {
                    request.add_property(connector, property, Value::UnsignedRange(value));
                }
            }
        }
        let flags = control::AtomicCommitFlags::ALLOW_MODESET;
        let tested = self
            .kms
            .atomic_commit(flags | control::AtomicCommitFlags::TEST_ONLY, request.clone())
            .map_err(|e| format!("TEST_ONLY: {e}"));
        let result = tested.and_then(|()| {
            self.kms
                .atomic_commit(flags, request)
                .map_err(|e| format!("commit: {e}"))
        });
        if let Value::Blob(id) = blob {
            let _ = self.kms.destroy_property_blob(id);
        }
        result
    }

    /// The output and sink this process drives: what a report, and an
    /// `Apply` from the daemon, are about. `None` without a valid EDID.
    fn identity(&self) -> Option<mediabox_core::DisplayIdentity> {
        self.offer.as_ref().map(|offer| mediabox_core::DisplayIdentity {
            connector: offer.connector.clone(),
            edid_sha256: offer.edid_sha256.clone(),
        })
    }

    /// Tells the daemon what this process committed -- the mode, the colour
    /// it asked for, whether HDR is signalled, and the trial it belongs to --
    /// for this output and sink. What the display offers and what the
    /// driver reports are the observer's to say, not this process's.
    fn report(&self) {
        ON_WIRE.with(|cell| *cell.borrow_mut() = Some((self.setting.borrow().clone(), self.trial.get())));
        let Some(identity) = self.identity() else { return };
        let timing = self.timing.get();
        report(mediabox_core::OwnerReport::Applied(mediabox_core::AppliedOutput {
            identity,
            trial: self.trial.get(),
            timing_key: timing.key(),
            label: timing.label(),
            colour: self.applied.get(),
            hdr: self.signalled.get().is_some(),
        }));
    }

    fn release_display(&self) {
        if self.released.replace(true) {
            return;
        }
        // Nothing in flight, and then nothing more from the presentation
        // thread: it stops at `released`.
        let _ = self.wait_for_page_flip();
        let mut stage = self.hold();
        stage.released = true;
        // The film first: a plane left enabled on a CRTC that is about to be
        // turned off is how the next master inherits a picture it did not draw.
        let state = &mut *stage;
        state.sink.clear(&self.kms, self.crtc, &state.video);
        VIDEO.store(false, std::sync::atomic::Ordering::Relaxed);
        // And the stacking order, which belongs to the connector rather than to
        // whoever set it: Kodi assigns this port's windows for itself and must
        // not inherit a primary this process raised.
        crate::video::lower_interface(&self.kms, self.crtc);
        // The CRTC is left lit, on this process's last frame, and the device
        // is handed to systemd instead of closed; see `fdstore`. Switching it
        // off here -- as this used to -- is what let the kernel console put the
        // sink's preferred 1080p on the wire between two owners, and Kodi then
        // took that 1080p as the mode to start from and to restore.
        //
        // Only when systemd cannot take the device does the old behaviour
        // stand: an unkept file closes at exit and removes its framebuffers
        // anyway, so the CRTC is switched off cleanly rather than by accident.
        let kept = self
            .presentation
            .borrow()
            .current
            .is_some()
            && crate::fdstore::keep(self.kms.as_fd().as_raw_fd());
        if kept {
            KEEP_SCANOUT.store(true, std::sync::atomic::Ordering::SeqCst);
        } else {
            let _ = self.kms.set_crtc(self.crtc, None, (0, 0), &[], None);
        }
        let (pending, current) = {
            let mut state = self.presentation.borrow_mut();
            (state.pending_previous.take(), state.current.take())
        };
        drop(pending);
        drop(current);
        let _ = self.kms.release_master_lock();
        drop(stage);
        self.stage.changed.notify_all();
        eprintln!(
            "mediabox-tv.platform DRM released{}",
            if kept { ", display left lit for the next owner" } else { "" }
        );
    }
}

impl Drop for SplitDisplay {
    fn drop(&mut self) {
        self.release_display();
    }
}

/// The sink's EDID, as the connector's `EDID` property blob publishes it.
///
/// Read through the descriptor this process already holds as master. The
/// same bytes are in `/sys/class/drm/<connector>/edid` -- measured on the Plus
/// with a Sony and an HP, the sysfs file and this blob have the same SHA-256
/// -- and that is where the observer reads them; this one is read here only
/// because it is already at hand.
fn connector_edid(kms: &SharedKms, connector: control::connector::Handle) -> Option<Vec<u8>> {
    let properties = kms.get_properties(connector).ok()?;
    let (handles, values) = properties.as_props_and_values();
    for (handle, value) in handles.iter().zip(values.iter()) {
        let Ok(info) = kms.get_property(*handle) else {
            continue;
        };
        if info.name().to_str() == Ok("EDID") && *value != 0 {
            return kms.get_property_blob(*value).ok();
        }
    }
    None
}

/// The connector's properties as the source profile compares them: the
/// vendor `color_format` and `color_depth` enums, whole, and whether the
/// standard HDR infoframe and colorimetry properties are there. Read on the
/// descriptor this process holds as master; nothing is set.
fn property_signature(
    kms: &SharedKms,
    connector: control::connector::Handle,
) -> mediabox_platform::source::PropertySignature {
    use mediabox_platform::source::EnumProperty;
    let mut signature = mediabox_platform::source::PropertySignature::default();
    let Ok(properties) = kms.get_properties(connector) else {
        return signature;
    };
    for handle in properties.as_props_and_values().0.iter().copied() {
        let Ok(info) = kms.get_property(handle) else {
            continue;
        };
        let enums = || match info.value_type() {
            control::property::ValueType::Enum(values) => EnumProperty {
                present: true,
                values: values
                    .values()
                    .1
                    .iter()
                    .map(|value| (value.name().to_string_lossy().into_owned(), value.value()))
                    .collect(),
            },
            _ => EnumProperty::default(),
        };
        match info.name().to_str() {
            Ok("color_format") => signature.color_format = enums(),
            Ok("color_depth") => signature.color_depth = enums(),
            Ok("Colorspace") => signature.colorspace = enums(),
            Ok("HDR_OUTPUT_METADATA") => signature.hdr_output_metadata = true,
            _ => {}
        }
    }
    signature
}

/// The connector discovery chose, found again through the open KMS device.
///
/// Discovery works from sysfs and names an output; this needs the DRM objects
/// behind that name. They are matched by connector *name* — type plus type
/// index, which is what sysfs publishes — and never by DRM object id: object
/// ids are allocated per boot and a different kernel hands out different ones.
fn find_output(
    kms: &SharedKms,
    wanted: &str,
    static_signature: &mediabox_platform::source::StaticSignature,
) -> Result<
    (
        control::connector::Info,
        control::crtc::Handle,
        control::Mode,
        Option<mediabox_core::OutputOffer>,
        Option<mediabox_core::ColorMode>,
        &'static mediabox_platform::source::SourceProfile,
        mediabox_core::OutputSetting,
    ),
    PlatformError,
> {
    let resources = kms
        .resource_handles()
        .map_err(|e| format!("read KMS resources: {e}"))?;
    let connector = resources
        .connectors()
        .iter()
        .find_map(|handle| {
            let connector = kms.get_connector(*handle, false).ok()?;
            let name = format!(
                "{}-{}",
                connector.interface().as_str(),
                connector.interface_id()
            );
            (name == wanted
                && connector.state() == control::connector::State::Connected
                && !connector.modes().is_empty())
            .then_some(connector)
        })
        .ok_or_else(|| {
            PlatformError::from(format!(
                "{wanted} was discovered but the display device does not offer it"
            ))
        })?;

    // Which mode, by the HDMI rules and the person's choice.
    //
    // Every mode the kernel lists for this display, with the colour modes the
    // EDID and the link allow at each, is the offer -- the same function, on
    // the same inputs, as the observer's -- and the kept setting is read from
    // the daemon's state and applies only to the display it was made on (the
    // SHA-256 of its EDID), `Auto` otherwise -- the reference Android box's
    // rule. A record from before that binding is read the way the daemon
    // migrates it, and not written: the daemon writes it. `Auto` is the largest mode in the panel's shape at the fastest
    // refresh the link carries in any format; on the Sony's 300 MHz input that
    // is 2160p60 in 4:2:0, not the 1080p it lists as preferred, which is what
    // this interface once ran a 4K panel at.
    //
    // What the source can send is the source profile the running system
    // matches: the product scope from sysfs and the device tree, the vendor
    // ABI from this connector's own properties, read on the descriptor this
    // process already holds. A system that is not the one a profile was
    // written for gets the conservative one -- RGB, eight bits, SDR.
    let resolved = mediabox_platform::source::resolve(&mediabox_platform::source::SourceSignature {
        static_part: static_signature.clone(),
        properties: Some(property_signature(kms, connector.handle())),
    });
    eprintln!("mediabox-tv.platform source profile {}", resolved.describe());
    let modes: Vec<control::Mode> = connector.modes().to_vec();
    let timings: Vec<mediabox_platform::video::Timing> = modes.iter().map(timing_of).collect();
    let edid = connector_edid(kms, connector.handle()).unwrap_or_default();
    let mut offer = mediabox_platform::output::offer(
        &timings,
        &edid,
        wanted,
        mediabox_platform::output::edid_name(&edid),
        &resolved.profile.caps,
    );
    if let Some(offer) = offer.as_mut() {
        offer.link.source_profile = resolved.describe();
    }
    let (mode, colour, setting) = match &offer {
        Some(offer) => {
            let kernel: Vec<mediabox_core::ModeTiming> = timings.iter().map(|timing| timing.mode).collect();
            let (setting, notes) = read_setting()
                .map(|stored| stored.for_offer(offer, &kernel))
                .unwrap_or_else(|| (mediabox_core::OutputSetting::default().for_sink(&offer.edid_sha256), Vec::new()));
            for note in notes {
                eprintln!("mediabox-tv.platform kept setting: {note}");
            }
            let chosen = offer
                .resolve(setting.resolution)
                .ok_or_else(|| PlatformError::from(format!("{wanted} has no mode")))?;
            let mode = modes
                .iter()
                .copied()
                .find(|mode| same_mode(mode, chosen))
                .ok_or_else(|| PlatformError::from(format!("{} is not in the mode list", chosen.label)))?;
            eprintln!(
                "mediabox-tv.platform {wanted} {} chosen ({}), link {} kHz",
                chosen.label,
                match setting.resolution {
                    mediabox_core::ResolutionChoice::Auto => "otomatik",
                    _ => "seçim",
                },
                offer.link.max_character_rate_khz
            );
            let colour = offer.colour(&setting, chosen);
            (mode, colour, setting)
        }
        // No EDID to reason from: the largest mode, fastest first.
        None => {
            let chosen = mediabox_platform::video::auto_timing(&timings, None)
                .ok_or_else(|| PlatformError::from(format!("{wanted} has no mode")))?;
            let at = timings.iter().position(|timing| *timing == chosen).unwrap_or(0);
            eprintln!("mediabox-tv.platform {wanted} has no readable EDID; {} chosen", chosen.label());
            (modes[at], None, mediabox_core::OutputSetting::default())
        }
    };

    // Not whichever video port the kernel happened to leave this socket on.
    //
    // On RK3588 the video ports are not equals. The driver says so itself, on
    // each CRTC, and this board answers:
    //
    //     CRTC 89   PORT_ID 0   FEATURE 7
    //     CRTC 130  PORT_ID 1   FEATURE 1
    //     CRTC 170  PORT_ID 2   FEATURE 1
    //
    // VP0 is the one with the HDR conversion block behind it -- a film there
    // can be tone-mapped and an SDR interface drawn over an HDR film is lifted
    // into the signal instead of coming out washed out and displaced, which is
    // exactly what a television on the second socket showed.
    //
    // Which socket can reach which port is the board's to say and this
    // appliance's to use: with both crossings open, each HDMI transmitter
    // offers VP0 and VP1, so the television that is actually connected is put
    // on the better port whichever socket somebody plugged it into. A board
    // whose device tree still pins one socket to one port gets its only
    // choice, and no worse a picture than before.
    let candidates: Vec<control::crtc::Handle> = connector
        .encoders()
        .iter()
        .filter_map(|handle| kms.get_encoder(*handle).ok())
        .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
        .collect();
    let rank = |crtc: &control::crtc::Handle| -> (u32, i64) {
        let (mut feature, mut port) = (0u32, i64::MAX);
        if let Ok(properties) = kms.get_properties(*crtc) {
            let (handles, values) = properties.as_props_and_values();
            for (handle, value) in handles.iter().zip(values.iter()) {
                let Ok(info) = kms.get_property(*handle) else {
                    continue;
                };
                match info.name().to_str() {
                    // A bitmask of what the port can do. More of it is better,
                    // and counting the bits keeps this from depending on which
                    // bit means what in one kernel's numbering.
                    Ok("FEATURE") => feature = (*value as u32).count_ones(),
                    Ok("PORT_ID") => port = *value as i64,
                    _ => {}
                }
            }
        }
        (feature, -port)
    };
    let best = candidates.iter().copied().max_by_key(|crtc| rank(crtc));
    let current = connector
        .current_encoder()
        .filter(|handle| connector.encoders().contains(handle))
        .and_then(|handle| kms.get_encoder(handle).ok())
        .and_then(|encoder| encoder.crtc());
    // The port it is already on is kept only when nothing better is on offer;
    // a modeset onto the good port is worth one flicker at startup.
    let crtc = match (best, current) {
        (Some(best), Some(current)) if best != current && rank(&best) > rank(&current) => {
            eprintln!(
                "mediabox-tv.platform moving {wanted} to the better video port: {best:?} over {current:?}"
            );
            best
        }
        (_, Some(current)) => current,
        (Some(best), None) => best,
        (None, None) => {
            return Err(PlatformError::from(format!(
                "{wanted} has no compatible CRTC"
            )));
        }
    };
    Ok((connector, crtc, mode, offer, colour, resolved.profile, setting))
}

/// Whether a kernel mode is the one offered: by the timing's key -- two
/// modes can share a size, a clock and totals and still be two timings.
fn same_mode(mode: &control::Mode, offered: &mediabox_core::OutputModeOffer) -> bool {
    timing_of(mode).key() == offered.timing_key
}

impl HasWindowHandle for SplitDisplay {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe {
            let handle = raw_window_handle::GbmWindowHandle::new(
                std::ptr::NonNull::from(&*self.gbm_surface.borrow().as_raw()).cast(),
            );
            raw_window_handle::WindowHandle::borrow_raw(raw_window_handle::RawWindowHandle::Gbm(
                handle,
            ))
        })
    }
}

impl HasDisplayHandle for SplitDisplay {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe {
            let handle = raw_window_handle::GbmDisplayHandle::new(
                std::ptr::NonNull::from(&*self.gbm_device.as_raw()).cast(),
            );
            raw_window_handle::DisplayHandle::borrow_raw(raw_window_handle::RawDisplayHandle::Gbm(
                handle,
            ))
        })
    }
}

struct GlContext {
    context: glutin::context::PossiblyCurrentContext,
    config: glutin::config::Config,
    /// Drawn through into the display's GBM surface; made again when that is
    /// replaced by one of another size (`SplitDisplay::generation`).
    surface: RefCell<glutin::surface::Surface<WindowSurface>>,
    generation: Cell<u64>,
    display: Rc<SplitDisplay>,
}

impl GlContext {
    fn new(display: Rc<SplitDisplay>) -> Result<Self, PlatformError> {
        let display_handle = display
            .display_handle()
            .map_err(|e| format!("GBM display handle: {e}"))?;
        let window_handle = display
            .window_handle()
            .map_err(|e| format!("GBM window handle: {e}"))?;
        let gl_display = unsafe {
            glutin::display::Display::new(display_handle.as_raw(), DisplayApiPreference::Egl)
        }
        .map_err(|e| format!("create EGL display on Mali GBM: {e}"))?;
        let template = ConfigTemplateBuilder::new()
            .with_transparency(true)
            .with_alpha_size(8)
            .build();
        let config = unsafe { gl_display.find_configs(template) }
            .map_err(|e| format!("enumerate EGL configs: {e}"))?
            .filter(|config| matches!(config, glutin::config::Config::Egl(egl) if egl.native_visual() as u32 == gbm::Format::Argb8888 as u32))
            .min_by_key(|config| config.num_samples())
            .ok_or_else(|| PlatformError::from("no EGL ARGB8888 window config".to_string()))?;
        let attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(glutin::context::Version {
                major: 2,
                minor: 0,
            })))
            .build(Some(window_handle.as_raw()));
        let context = unsafe { gl_display.create_context(&config, &attrs) }
            .map_err(|e| format!("create Mali EGL GLES context: {e}"))?;
        let surface = window_surface(&config, &display)?;
        let context = context
            .make_current(&surface)
            .map_err(|e| format!("make Mali EGL context current: {e}"))?;

        let egl_vendor = egl_string(&gl_display, 0x3053).unwrap_or_else(|| "unknown".into());
        let egl_version =
            egl_string(&gl_display, 0x3054).unwrap_or_else(|| gl_display.version_string());
        let gl_vendor = gl_string(&gl_display, 0x1F00).unwrap_or_else(|| "unknown".into());
        let gl_renderer = gl_string(&gl_display, 0x1F01).unwrap_or_else(|| "unknown".into());
        if egl_vendor != "ARM" || !gl_renderer.contains("Mali") {
            return Err(format!(
                "software/non-Mali EGL rejected: EGL_VENDOR={egl_vendor} GL_RENDERER={gl_renderer}"
            )
            .into());
        }
        eprintln!(
            "mediabox-tv.gpu EGL_VENDOR={} EGL_VERSION={} GL_VENDOR={} GL_RENDERER={}",
            egl_vendor, egl_version, gl_vendor, gl_renderer
        );
        let generation = display.generation.get();
        Ok(Self {
            context,
            config,
            surface: RefCell::new(surface),
            generation: Cell::new(generation),
            display,
        })
    }

    /// A new EGL surface over the display's GBM surface when that has been
    /// replaced. The old one is destroyed here, before the GBM surface it drew
    /// into is -- that one outlives it until the first frame of the new size
    /// is on the panel.
    fn follow_surface(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let generation = self.display.generation.get();
        if self.generation.get() == generation {
            return Ok(());
        }
        let surface = window_surface(&self.config, &self.display).map_err(|e| e.to_string())?;
        self.context.make_current(&surface)?;
        drop(self.surface.replace(surface));
        self.generation.set(generation);
        Ok(())
    }
}

fn window_surface(
    config: &glutin::config::Config,
    display: &SplitDisplay,
) -> Result<glutin::surface::Surface<WindowSurface>, PlatformError> {
    let window_handle = display
        .window_handle()
        .map_err(|e| format!("GBM window handle: {e}"))?;
    let size = display.size();
    let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        window_handle.as_raw(),
        NonZeroU32::new(size.width).unwrap(),
        NonZeroU32::new(size.height).unwrap(),
    );
    unsafe {
        config
            .display()
            .create_window_surface(config, &surface_attrs)
    }
    .map_err(|e| format!("create Mali EGL window surface: {e}").into())
}

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglQueryString(display: *const std::ffi::c_void, name: i32) -> *const libc::c_char;
}

fn egl_string(display: &glutin::display::Display, name: i32) -> Option<String> {
    let RawDisplay::Egl(raw) = display.raw_display();
    let value = unsafe { eglQueryString(raw, name) };
    (!value.is_null()).then(|| {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    })
}

fn gl_string(display: &glutin::display::Display, name: u32) -> Option<String> {
    type GlGetString = unsafe extern "C" fn(u32) -> *const u8;
    let symbol = CString::new("glGetString").unwrap();
    let address = display.get_proc_address(&symbol);
    if address.is_null() {
        return None;
    }
    let get_string: GlGetString = unsafe { std::mem::transmute(address) };
    let value = unsafe { get_string(name) };
    (!value.is_null()).then(|| {
        unsafe { CStr::from_ptr(value.cast()) }
            .to_string_lossy()
            .into_owned()
    })
}

impl GlContext {
    /// Reads the frame back off the GPU and writes it as a PNG.
    ///
    /// Slow and deliberately so — a 4K readback is tens of megabytes — which is
    /// why it happens once, when asked, and never on an ordinary frame.
    fn capture(&self, path: &Path) {
        const GL_RGBA: u32 = 0x1908;
        const GL_UNSIGNED_BYTE: u32 = 0x1401;
        type ReadPixels = unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut u8);

        let symbol = CString::new("glReadPixels").expect("literal");
        let address = self.context.display().get_proc_address(&symbol);
        if address.is_null() {
            eprintln!("mediabox-tv.snapshot glReadPixels unavailable");
            return;
        }
        let read: ReadPixels = unsafe { std::mem::transmute(address) };

        let size = self.display.size();
        let (width, height) = (size.width, size.height);
        let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        unsafe {
            read(
                0,
                0,
                width as i32,
                height as i32,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                pixels.as_mut_ptr(),
            );
        }

        // GL's origin is the bottom-left corner and a PNG's is the top-left, so
        // the rows come back upside down. The surface is XRGB, so whatever the
        // driver left in the fourth channel is not alpha and is overwritten.
        let stride = (width as usize) * 4;
        let mut image = vec![0u8; pixels.len()];
        for row in 0..height as usize {
            let from = (height as usize - 1 - row) * stride;
            let to = row * stride;
            image[to..to + stride].copy_from_slice(&pixels[from..from + stride]);
            for pixel in image[to..to + stride].chunks_exact_mut(4) {
                pixel[3] = 0xff;
            }
        }

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match image::RgbaImage::from_raw(width, height, image) {
            Some(buffer) => match buffer.save(path) {
                Ok(()) => eprintln!(
                    "mediabox-tv.snapshot wrote {} {}x{}",
                    path.display(),
                    width,
                    height
                ),
                Err(e) => eprintln!("mediabox-tv.snapshot {} failed: {e}", path.display()),
            },
            None => eprintln!("mediabox-tv.snapshot buffer did not fit the frame"),
        }
    }
}

unsafe impl OpenGLInterface for GlContext {
    fn ensure_current(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.follow_surface()?;
        if !self.context.is_current() {
            self.context.make_current(&*self.surface.borrow())?;
        }
        Ok(())
    }

    fn swap_buffers(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Only wait when there is nothing left to draw into. With a surface
        // that allocates a third buffer this lets the GPU start the next frame
        // while the panel is still showing the last one; with two it is the
        // same wait as before, in the same place.
        let at = std::time::Instant::now();
        if !self.display.gbm_surface.borrow().has_free_buffers() {
            self.display.wait_for_page_flip()?;
        }
        let waited = at.elapsed();

        // Before the swap, while the finished frame is still the one the
        // context will read from.
        if let Some(path) = SNAPSHOT.with(|cell| cell.borrow_mut().take()) {
            self.capture(&path);
        }

        let at = std::time::Instant::now();
        self.surface.borrow().swap_buffers(&self.context)?;
        let swapped = at.elapsed();

        let at = std::time::Instant::now();
        let answer = self.display.present();
        let presented = at.elapsed();

        add_phase(|phases| {
            phases.flip_wait_us += waited.as_micros() as u64;
            phases.swap_us += swapped.as_micros() as u64;
            phases.present_us += presented.as_micros() as u64;
        });
        answer
    }

    fn resize(
        &self,
        _width: NonZeroU32,
        _height: NonZeroU32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    fn get_proc_address(&self, name: &CStr) -> *const std::ffi::c_void {
        self.context.display().get_proc_address(name)
    }
}

struct SplitWindow {
    window: slint::Window,
    renderer: FemtoVGRenderer,
    display: Rc<SplitDisplay>,
    redraw: Cell<bool>,
    /// Draws every frame the panel will take, whether anything changed or not.
    ///
    /// The interface is event-driven and idles at nothing, which is right for a
    /// television and useless for measuring throughput: a run that is asleep
    /// reports the frame rate of the last thing that moved. Set
    /// `MEDIABOX_TV_BENCH=1` to make the loop draw continuously, which is what
    /// the frame-pacing figures in the acceptance report are taken from.
    bench: bool,
}

impl SplitWindow {
    fn new(display: Rc<SplitDisplay>) -> Result<Rc<Self>, PlatformError> {
        let renderer = FemtoVGRenderer::new(GlContext::new(display.clone())?)?;
        Ok(Rc::new_cyclic(|weak: &std::rc::Weak<Self>| Self {
            window: slint::Window::new(weak.clone()),
            renderer,
            display,
            redraw: Cell::new(true),
            bench: std::env::var_os("MEDIABOX_TV_BENCH").is_some(),
        }))
    }

    /// Tells Slint the panel's size and scale, when they are not what it was
    /// last told: after a mode of another size, or back from one the kernel
    /// refused. The scale first, then the size in the logical pixels that
    /// scale defines.
    fn sync_size(&self) {
        let scale = self.display.scale_factor();
        if (self.window.scale_factor() - scale).abs() > f32::EPSILON {
            self.window
                .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        }
        let logical = self.size().to_logical(self.window.scale_factor());
        if self.window.size().to_logical(self.window.scale_factor()) != logical {
            self.window.dispatch_event(WindowEvent::Resized { size: logical });
        }
        self.redraw.set(true);
    }

    /// A setting from the daemon: the mode and colour it resolves to on this
    /// display, from the next frame.
    fn switch_output(&self, setting: mediabox_core::OutputSetting, trial: Option<u64>) {
        let display = &self.display;
        let Some(offer) = display.offer.as_ref() else {
            eprintln!("mediabox-tv.platform no offer: this display's EDID could not be read");
            return;
        };
        let setting = setting.for_sink(&offer.edid_sha256);
        let Some(target) = offer.resolve(setting.resolution) else { return };
        let colour = offer.colour(&setting, target);
        let Some(mode) = display
            .connector
            .modes()
            .iter()
            .copied()
            .find(|mode| same_mode(mode, target))
        else {
            eprintln!("mediabox-tv.platform {} is not in the mode list", target.label);
            return;
        };
        // The same mode with the crystal's error moved on since it was set
        // (a kHz is 1.7 ppm at 594 MHz) is set again: here, at a channel's
        // opening, on a dark panel -- never while a film plays.
        // Nothing measured yet (a boot's first process, its first seconds):
        // the panel keeps what it shows, the film too, until it is.
        let ppm = match crystal_ppm() {
            Some(ppm) => Some(ppm),
            None => {
                let _held = display.hold();
                crystal_ppm_settled()
            }
        };
        let wire = on_the_wire(mode, ppm).clock();
        if mode == display.mode.get() && colour == display.colour.get() && wire == display.wire_khz.get() {
            *display.setting.borrow_mut() = setting;
            display.trial.set(trial);
            display.report();
            return;
        }
        eprintln!(
            "mediabox-tv.platform switching to {} {}",
            target.label,
            colour.map(|mode| mode.label()).unwrap_or_default()
        );
        match display.begin_switch(mode, colour, setting, trial) {
            Ok(_) => self.sync_size(),
            Err(error) => eprintln!("mediabox-tv.platform switch not started: {error}"),
        }
    }

    fn render_if_needed(&self) -> Result<(), PlatformError> {
        if self.redraw.replace(false) {
            let at = std::time::Instant::now();
            self.renderer.render()?;
            let total = at.elapsed();
            add_phase(|phases| {
                phases.frames += 1;
                phases.total_us += total.as_micros() as u64;
            });
            let moving = DRAW_UNTIL.with(|until| until.get().is_some_and(|until| until > std::time::Instant::now()));
            if self.bench || moving || self.window.has_active_animations() {
                self.redraw.set(true);
            }
        }
        Ok(())
    }
}

thread_local! {
    static DRAW_UNTIL: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// Draw every frame for `span`: for a transition the loop must not wait on
/// Slint's own "animation running" signal to notice, which left gaps of
/// tens of milliseconds in the Bluetooth card's slide (measured 2026-10-08).
pub fn keep_drawing(span: Duration) {
    DRAW_UNTIL.with(|until| until.set(Some(std::time::Instant::now() + span)));
}

impl WindowAdapter for SplitWindow {
    fn window(&self) -> &slint::Window {
        &self.window
    }
    fn size(&self) -> slint::PhysicalSize {
        self.display.size()
    }
    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }
    fn request_redraw(&self) {
        self.redraw.set(true);
    }
    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if visible {
            // The scale first, then the size in the logical pixels that scale
            // defines. The other order sizes the interface in the panel's own
            // pixels for one frame, which is the whole home screen laid out
            // against a viewport twice the width it was designed for.
            self.window.dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: self.display.scale_factor(),
            });
            self.window.dispatch_event(WindowEvent::Resized {
                size: self.size().to_logical(self.window.scale_factor()),
            });
            self.redraw.set(true);
        }
        Ok(())
    }
}

enum LoopMessage {
    Invoke(Box<dyn FnOnce() + Send>),
    Quit,
}

#[derive(Clone)]
struct Proxy {
    sender: mpsc::Sender<LoopMessage>,
    wake: Arc<OwnedFd>,
}

impl Proxy {
    fn send(&self, message: LoopMessage) -> Result<(), slint::EventLoopError> {
        self.sender
            .send(message)
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)?;
        let one: u64 = 1;
        unsafe {
            libc::write(self.wake.as_raw_fd(), (&one as *const u64).cast(), 8);
        }
        Ok(())
    }
}

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.send(LoopMessage::Quit)
    }
    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.send(LoopMessage::Invoke(event))
    }
}

struct DirectInput;

/// `EVIOCGRAB`, which is `_IOW('E', 0x90, int)`.
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;

impl LibinputInterface for DirectInput {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let access = flags & libc::O_ACCMODE;
        let file = OpenOptions::new()
            .read(access == libc::O_RDONLY || access == libc::O_RDWR)
            .write(access == libc::O_WRONLY || access == libc::O_RDWR)
            .custom_flags(flags & !libc::O_ACCMODE)
            .open(path)
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        // Take the device exclusively, so the kernel's own console handler
        // stops seeing it.
        //
        // Without this every press also went to the virtual terminal, which is
        // still sitting behind the television with a login prompt on it. It is
        // invisible while this process holds DRM master — and then Kodi hands
        // the display back, there is a moment before the mode is set again, and
        // the panel shows a console with an evening's worth of remote presses
        // typed into it. Reported from the appliance, exactly that way.
        //
        // EVIOCGRAB is an input-core grab rather than an evdev one: every other
        // handler of the device, the console's included, stops receiving from
        // it. It does not touch the CEC adapter, which the daemon owns and reads by
        // another road entirely, so the normalised remote is unaffected.
        //
        // A refusal is not fatal. Another process holding the grab means keys
        // reach the console again, which is untidy rather than broken, and a
        // television with no input at all would be worse.
        //
        // The board's power button is left to everyone: the control plane
        // reads it to turn the box off, and a grab here took every press of
        // it (measured on the Plus: the PMIC's interrupts counted each press,
        // the daemon saw none). It types nothing on a console.
        let name = path
            .file_name()
            .and_then(|node| std::fs::read_to_string(Path::new("/sys/class/input").join(node).join("device/name")).ok())
            .unwrap_or_default();
        if !crate::input::should_grab(&name) {
            return Ok(file.into());
        }
        // SAFETY: one ioctl on a file descriptor this function owns.
        if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB, 1) } != 0 {
            eprintln!(
                "mediabox-tv.input could not take {} exclusively: {}",
                path.display(),
                std::io::Error::last_os_error()
            );
        }

        Ok(file.into())
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        // Released explicitly rather than left to the close: the appliance
        // hands the display and the remote to Kodi, and a device still grabbed
        // by a process that is going away is a remote that does nothing.
        // SAFETY: one ioctl on a descriptor this call owns.
        unsafe {
            libc::ioctl(fd.as_raw_fd(), EVIOCGRAB, 0);
        }
        drop(File::from(fd));
    }
}

struct SplitPlatform {
    window: Rc<SplitWindow>,
    receiver: RefCell<mpsc::Receiver<LoopMessage>>,
    proxy: Proxy,
    /// Which of the seat's devices are remote controls, decided once each.
    ///
    /// Keyed by the kernel's own name for the device ("event3"), because that
    /// is what libinput hands back and what /sys is organised by.
    remotes: RefCell<std::collections::HashMap<String, crate::input::Device>>,
    /// Whether a shift key is down.
    ///
    /// libinput hands over key codes, not characters: there is no xkb here and
    /// nothing else is tracking the modifiers, so without this every letter
    /// arrived lower case and half of ASCII could not be typed at all. A film
    /// title did not care. A password does.
    shift: std::cell::Cell<bool>,
    /// The key being held down, repeated at its own device's rate.
    ///
    /// libinput hands over the press and the release and never the kernel's
    /// repeats: repeating a held key is the job of whatever owns the seat --
    /// a Wayland compositor, Kodi -- and on this appliance that is this
    /// process. Without it a held arrow moved one row, and a held volume key
    /// moved the volume one step.
    held: RefCell<Option<HeldKey>>,
    /// Each device's own repeat delay and period (`EVIOCGREP`), read once:
    /// the same numbers Kodi reads, so a remote held down behaves alike in
    /// both.
    rates: RefCell<std::collections::HashMap<String, (Duration, Duration)>>,
    /// When this process started: libinput hands over every device already
    /// there as "added" at start, and those are not arrivals.
    started: std::time::Instant,
    /// Bluetooth devices announced in the last two seconds, by name: one
    /// keyboard registers two or three input nodes and is one arrival.
    announced: RefCell<std::collections::HashMap<String, std::time::Instant>>,
    /// The Bluetooth input nodes on the seat, by sysname, with the device's
    /// name: on removal sysfs is already gone, so this is how a node that
    /// leaves is known to have been a Bluetooth device's.
    bluetooth: RefCell<std::collections::HashMap<String, String>>,
}

/// A press being repeated.
struct HeldKey {
    code: u32,
    sysname: String,
    press: HeldPress,
    next: std::time::Instant,
    period: Duration,
}

enum HeldPress {
    Action(mediabox_core::InputAction, crate::input::Origin),
    Text(slint::SharedString),
}

/// What a device does not say, Android's: a key held for 400 ms repeats every
/// 100 ms.
const REPEAT_WHEN_UNSAID: (Duration, Duration) = (Duration::from_millis(400), Duration::from_millis(100));

/// `EVIOCGREP`: the device's repeat delay and period, in milliseconds.
fn device_repeat(sysname: &str) -> Option<(Duration, Duration)> {
    const EVIOCGREP: libc::c_ulong = 0x8008_4503;
    let file = File::open(format!("/dev/input/{sysname}")).ok()?;
    let mut rate = [0u32; 2];
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGREP as _, rate.as_mut_ptr()) };
    if rc < 0 || rate[0] == 0 || rate[1] == 0 {
        return None;
    }
    Some((Duration::from_millis(rate[0].into()), Duration::from_millis(rate[1].into())))
}

impl SplitPlatform {
    fn new() -> Result<Self, PlatformError> {
        let display = SplitDisplay::new()?;
        let window = SplitWindow::new(display)?;
        let (sender, receiver) = mpsc::channel();
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 {
            return Err(format!(
                "create event-loop eventfd: {}",
                std::io::Error::last_os_error()
            )
            .into());
        }
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        Ok(Self {
            window,
            receiver: RefCell::new(receiver),
            proxy: Proxy { sender, wake },
            remotes: RefCell::new(std::collections::HashMap::new()),
            held: RefCell::new(None),
            rates: RefCell::new(std::collections::HashMap::new()),
            shift: std::cell::Cell::new(false),
            started: std::time::Instant::now(),
            announced: RefCell::new(std::collections::HashMap::new()),
            bluetooth: RefCell::new(std::collections::HashMap::new()),
        })
    }

    /// What a device on the seat is: the CEC adapter's rc-core double, an
    /// infrared receiver, or anything else.
    ///
    /// The daemon owns the television's remote: it holds the CEC adapter,
    /// decodes the user-control codes and publishes them as normalised
    /// actions, and this interface listens to that. The kernel *also*
    /// registers each HDMI transmitter's CEC as an rc-core input device, so
    /// without this every press of the television remote reaches the
    /// interface twice.
    ///
    /// That used to be decided by "the event node is under /rc/", which was
    /// right while CEC was the only rc device on the board. The board's own
    /// infrared receiver is an rc-core device too (gpio-ir-receiver, see
    /// packaging/overlays/mediabox-ir-opi5plus.dts), and that rule silenced
    /// it. The rc device says which it is: CEC's protocol is `cec` and its
    /// parent is an HDMI transmitter; see `input::classify`.
    fn device_kind(&self, device: &input::Device) -> crate::input::Device {
        let sysname = device.sysname().to_string();
        if let Some(answer) = self.remotes.borrow().get(&sysname) {
            return *answer;
        }

        // /sys/class/input/event0 -> ../../devices/platform/fdea0000.hdmi/rc/rc0/input0/event0
        let path = std::fs::canonicalize(format!("/sys/class/input/{sysname}"))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rc_dir = path.find("/rc/rc").map(|at| {
            let rest = &path[at + 4..];
            let end = rest.find('/').map_or(path.len(), |slash| at + 4 + slash);
            std::path::PathBuf::from(&path[..end])
        });
        let protocols = rc_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join("protocols")).ok());
        let driver = rc_dir.as_ref().and_then(|dir| {
            std::fs::read_link(dir.join("device/driver"))
                .ok()
                .and_then(|link| link.file_name().map(|name| name.to_string_lossy().into_owned()))
        });
        let answer = crate::input::classify(&path, &device.name(), protocols.as_deref(), driver.as_deref());

        eprintln!(
            "mediabox-tv.input device {} name={:?} kind={:?} rc={:?} driver={:?}",
            sysname,
            device.name(),
            answer,
            protocols.as_deref().map(str::trim),
            driver
        );
        self.remotes.borrow_mut().insert(sysname, answer);
        answer
    }

    /// A Bluetooth input device -- a keyboard, a mouse, a remote -- that
    /// has just connected, said on the panel. The kernel registers it the
    /// moment the link is up, so this is as soon as it can be known.
    fn device_added(&self, device: &input::Device) {
        let path = std::fs::canonicalize(format!("/sys/class/input/{}", device.sysname()))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bustype = std::fs::read_to_string(format!("/sys/class/input/{}/device/id/bustype", device.sysname())).ok();
        if !crate::input::is_bluetooth(bustype.as_deref(), &path) {
            return;
        }
        let name = crate::input::device_title(&device.name());
        self.bluetooth.borrow_mut().insert(device.sysname().to_string(), name.clone());
        self.announce(name, true);
    }

    fn device_removed(&self, device: &input::Device) {
        if let Some(name) = self.bluetooth.borrow_mut().remove(device.sysname()) {
            self.announce(name, false);
        }
    }

    /// Said once per device per change: one keyboard is two or three input
    /// nodes. Nothing at start, where libinput lists what is already there.
    fn announce(&self, name: String, connected: bool) {
        if self.started.elapsed() < Duration::from_secs(5) {
            return;
        }
        let key = format!("{connected}{name}");
        let now = std::time::Instant::now();
        let mut announced = self.announced.borrow_mut();
        announced.retain(|_, at| now.duration_since(*at) < Duration::from_secs(2));
        if announced.contains_key(&key) {
            return;
        }
        announced.insert(key, now);
        drop(announced);
        eprintln!("mediabox-tv.input bluetooth {} {name:?}", if connected { "connected" } else { "disconnected" });
        crate::with_app(|app| app.device_changed(&name, connected));
    }

    fn drain_messages(&self) -> bool {
        let mut quit = false;
        while let Ok(message) = self.receiver.borrow_mut().try_recv() {
            match message {
                LoopMessage::Invoke(event) => event(),
                LoopMessage::Quit => quit = true,
            }
        }
        quit
    }

    fn dispatch_input(&self, libinput: &mut input::Libinput) -> Result<(), PlatformError> {
        libinput
            .dispatch()
            .map_err(|e| format!("libinput dispatch: {e}"))?;
        for event in libinput {
            match &event {
                input::Event::Device(input::event::DeviceEvent::Added(added)) => {
                    self.device_added(&added.device());
                    continue;
                }
                input::Event::Device(input::event::DeviceEvent::Removed(removed)) => {
                    self.device_removed(&removed.device());
                    continue;
                }
                _ => {}
            }
            // A remote in its pointer mode -- the UR-02's, with the gyroscope
            // -- clicks where it would otherwise press Ok. There is no pointer
            // on this interface, so the click is the press of what has focus,
            // as Android takes it. Moving the pointer means nothing here.
            if let input::Event::Pointer(input::event::PointerEvent::Button(button)) = &event {
                if button.button() == BTN_LEFT {
                    let pressed = matches!(button.button_state(), input::event::pointer::ButtonState::Pressed);
                    crate::with_app(|app| app.remote_key(mediabox_core::InputAction::Ok, pressed, crate::input::Origin::Keyboard));
                }
                continue;
            }
            let input::Event::Keyboard(input::event::KeyboardEvent::Key(key)) = event else {
                continue;
            };
            let kind = self.device_kind(&key.device());
            if kind == crate::input::Device::CecDuplicate {
                continue;
            }
            let pressed = matches!(key.key_state(), KeyState::Pressed);
            let sysname = key.device().sysname().to_string();
            if !pressed
                && self
                    .held
                    .borrow()
                    .as_ref()
                    .is_some_and(|held| held.code == key.key() && held.sysname == sysname)
            {
                self.held.replace(None);
            } else if pressed {
                // Another key down ends the repeat of the last one.
                self.held.replace(None);
            }
            // A remote's keys -- all of the receiver's, and the Back, Home,
            // Menu, transport and volume keys of anything else -- are
            // actions, not text.
            let keyboard = kind == crate::input::Device::Other;
            if let Some(action) = crate::input::action_for_evdev(key.key(), keyboard) {
                let origin = if keyboard {
                    crate::input::Origin::Keyboard
                } else {
                    crate::input::Origin::Infrared
                };
                if pressed && crate::input::repeats(action) {
                    self.hold(key.key(), sysname, HeldPress::Action(action, origin));
                }
                crate::with_app(|app| app.remote_key(action, pressed, origin));
                continue;
            }
            if !keyboard {
                continue;
            }
            // Either shift, held. Tracked before anything else looks at the
            // code, and not forwarded: a modifier on its own is not a press
            // this interface has anything to do with.
            if matches!(key.key(), KEY_LEFTSHIFT | KEY_RIGHTSHIFT) {
                self.shift.set(matches!(key.key_state(), KeyState::Pressed));
                continue;
            }
            let Some(text) = key_text(key.key(), self.shift.get()) else {
                continue;
            };
            if pressed && crate::input::text_repeats(text.as_str()) {
                self.hold(key.key(), sysname, HeldPress::Text(text.clone()));
            }
            let event = match key.key_state() {
                KeyState::Pressed => WindowEvent::KeyPressed { text },
                KeyState::Released => WindowEvent::KeyReleased { text },
            };
            self.window.window.dispatch_event(event);
        }
        Ok(())
    }

    /// Start repeating a press after its device's delay.
    fn hold(&self, code: u32, sysname: String, press: HeldPress) {
        let (delay, period) = *self
            .rates
            .borrow_mut()
            .entry(sysname.clone())
            .or_insert_with(|| device_repeat(&sysname).unwrap_or(REPEAT_WHEN_UNSAID));
        self.held.replace(Some(HeldKey {
            code,
            sysname,
            press,
            next: std::time::Instant::now() + delay,
            period,
        }));
    }

    /// The held key's next repeat, if it is due; and how long until the one
    /// after, for the loop to wake for it.
    fn repeat_held(&self) -> Option<Duration> {
        let now = std::time::Instant::now();
        let due = {
            let mut held = self.held.borrow_mut();
            let held = held.as_mut()?;
            if held.next > now {
                return Some(held.next - now);
            }
            held.next = now + held.period;
            match &held.press {
                HeldPress::Action(action, origin) => Ok((*action, *origin)),
                HeldPress::Text(text) => Err(text.clone()),
            }
        };
        match due {
            Ok((action, origin)) => crate::with_app(|app| app.remote_key(action, true, origin)),
            Err(text) => self
                .window
                .window
                .dispatch_event(WindowEvent::KeyPressed { text }),
        }
        self.held.borrow().as_ref().map(|held| held.period)
    }
}

impl Platform for SplitPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(self.proxy.clone()))
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        /// The interface's own nice: above ordinary work, below the
        /// presentation thread (SCHED_FIFO, `presenter`), which puts the film
        /// up whatever this loop is drawing.
        const INTERFACE_NICE: libc::c_int = -5;
        // SAFETY: gettid and setpriority on this thread only.
        let set = unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, INTERFACE_NICE) };
        mediabox_core::timeline!("sched", "thread=interface nice={:?}", (set == 0).then_some(INTERFACE_NICE));
        let mut libinput = input::Libinput::new_with_udev(DirectInput);
        libinput
            .udev_assign_seat("seat0")
            .map_err(|_| PlatformError::from("libinput could not assign seat0".to_string()))?;
        // Passes that drew for longer than a vblank while a film was on the
        // panel, said in the journal once a second at most: the film no
        // longer waits for them (`presenter`), and this is how that is seen.
        let mut long_draws: (u32, u64, Option<std::time::Instant>) = (0, 0, None);
        loop {
            // Where a pass of the loop spends its time, on the timeline when
            // one phase holds it past a commit's margin: a film frame due at
            // a vblank waits for this loop (`loop-slow`).
            let started = mediabox_core::timeline::now_ns();
            slint::platform::update_timers_and_animations();
            if self.drain_messages() {
                break;
            }
            if let Some((setting, trial)) = OUTPUT_WANTED.with(|wanted| wanted.borrow_mut().take()) {
                self.window.switch_output(setting, trial);
            }
            if OUTPUT_REPORT_DUE.with(|due| due.replace(false))
                && self.window.display.presentation.borrow().current.is_some()
            {
                self.window.display.report();
            }
            let before_render = mediabox_core::timeline::now_ns();
            self.window.render_if_needed()?;
            let rendered = mediabox_core::timeline::now_ns();
            let drew = rendered - before_render;
            if drew > self.window.display.frame_period().as_nanos() as u64 && video_showing() {
                long_draws.0 += 1;
                long_draws.1 = long_draws.1.max(drew);
                let now = std::time::Instant::now();
                if long_draws.2.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1)) {
                    eprintln!(
                        "mediabox-tv.loop: arayüz {} kez bir vblank'ten uzun çizdi (en uzun {:.1} ms); film sunum iş parçacığında",
                        long_draws.0,
                        long_draws.1 as f64 / 1e6
                    );
                    long_draws = (0, 0, Some(now));
                }
            }
            // A switch the kernel refused has put the old size back.
            if self.window.display.switch.borrow().is_none()
                && self.window.window.size() != self.window.size()
            {
                self.window.sync_size();
            }

            let repeat = self.repeat_held();
            // The film is the presentation thread's; what it has for the
            // interface (a subtitle, HDR) is taken here.
            self.window.display.sync_presented();
            let timeout = if self.window.redraw.get() {
                0
            } else {
                slint::platform::duration_until_next_timer_update()
                    .unwrap_or(Duration::from_millis(250))
                    .min(Duration::from_millis(250))
                    .min(repeat.unwrap_or(Duration::MAX))
                    .as_millis() as i32
            };
            let mut pollfds = [
                libc::pollfd {
                    fd: self.proxy.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: libinput.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let polling = mediabox_core::timeline::now_ns();
            let rc = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, timeout) };
            let polled = mediabox_core::timeline::now_ns();
            if rc < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("event loop poll: {}", std::io::Error::last_os_error()).into());
            }
            if pollfds[0].revents & libc::POLLIN != 0 {
                let mut value: u64 = 0;
                unsafe {
                    libc::read(
                        self.proxy.wake.as_raw_fd(),
                        (&mut value as *mut u64).cast(),
                        8,
                    );
                }
            }
            if pollfds[1].revents & libc::POLLIN != 0 {
                self.dispatch_input(&mut libinput)?;
            }
            if mediabox_core::timeline::enabled() {
                let done = mediabox_core::timeline::now_ns();
                let ms = |ns: u64| ns as f64 / 1e6;
                let overslept = polled.saturating_sub(polling).saturating_sub(timeout.max(0) as u64 * 1_000_000);
                if before_render - started > 2_000_000
                    || rendered - before_render > 2_000_000
                    || polling - rendered > 2_000_000
                    || done - polled > 2_000_000
                    || overslept > 2_000_000
                {
                    mediabox_core::timeline::write(
                        "loop-slow",
                        format_args!(
                            "timers={:.2} render={:.2} between={:.2} poll={:.2} timeout={timeout} after={:.2} rc={rc} revents={},{}",
                            ms(before_render - started),
                            ms(rendered - before_render),
                            ms(polling - rendered),
                            ms(polled - polling),
                            ms(done - polled),
                            pollfds[0].revents,
                            pollfds[1].revents
                        ),
                    );
                }
            }
        }
        self.window.display.release_display();
        Ok(())
    }
}

/// What a USB or Bluetooth keyboard's key code means to this interface.
///
/// Two vocabularies, deliberately separate. The named keys are navigation and
/// are answered by the focus model; the printable ones are text, and exist so
/// the search screen can be typed into by somebody who would rather not spell
/// a film out on a grid with a remote.
///
/// Nothing here maps to power. KEY_POWER (116), KEY_POWER2 (356), KEY_RESTART
/// (408) and KEY_SLEEP (142) are not in either table and never will be: this
/// interface has no business restarting the appliance because a key was
/// pressed, and a television remote that emits one of those through a HID
/// endpoint must reach a dead end here. See `actions.rs`, and the udev rule
/// that stops systemd-logind from acting on them first.
/// The two shift keys, by the kernel's code.
const KEY_LEFTSHIFT: u32 = 42;
const BTN_LEFT: u32 = 0x110;
const KEY_RIGHTSHIFT: u32 = 54;

fn key_text(code: u32, shift: bool) -> Option<slint::SharedString> {
    use slint::platform::Key;
    let key = match code {
        1 => Key::Escape,
        14 => Key::Backspace,
        28 | 96 => Key::Return,
        102 => Key::Home,
        103 => Key::UpArrow,
        105 => Key::LeftArrow,
        106 => Key::RightArrow,
        108 => Key::DownArrow,
        111 => Key::Delete,
        _ => return printable(code, shift).map(|c| c.to_string().into()),
    };
    Some(char::from(key).to_string().into())
}

/// The letters, digits and the few marks a title can contain, in the US layout
/// the appliance assumes. There is no xkb here and no compose: a keyboard on a
/// television is for typing a film's name into a search box, and anything more
/// belongs to the browser application.
fn printable(code: u32, shift: bool) -> Option<char> {
    const ROW_NUMBERS: [char; 10] = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'];
    const ROW_NUMBERS_SHIFTED: [char; 10] = ['!', '@', '#', '$', '%', '^', '&', '*', '(', ')'];
    const ROW_Q: [char; 10] = ['q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p'];
    const ROW_A: [char; 9] = ['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'];
    const ROW_Z: [char; 7] = ['z', 'x', 'c', 'v', 'b', 'n', 'm'];

    let letter = |c: char| {
        if shift { c.to_ascii_uppercase() } else { c }
    };
    // The marks, in the order the kernel numbers them: unshifted, shifted.
    let mark = |plain: char, shifted: char| if shift { shifted } else { plain };

    Some(match code {
        2..=11 => {
            let at = (code - 2) as usize;
            if shift {
                ROW_NUMBERS_SHIFTED[at]
            } else {
                ROW_NUMBERS[at]
            }
        }
        16..=25 => letter(ROW_Q[(code - 16) as usize]),
        30..=38 => letter(ROW_A[(code - 30) as usize]),
        44..=50 => letter(ROW_Z[(code - 44) as usize]),
        // The rest of the US layout's printable keys. They were missing, and
        // with them so were the full stop and the underscore — which is to say
        // most e-mail addresses could not be typed on a keyboard at all.
        12 => mark('-', '_'),
        13 => mark('=', '+'),
        26 => mark('[', '{'),
        27 => mark(']', '}'),
        39 => mark(';', ':'),
        40 => mark('\'', '"'),
        41 => mark('`', '~'),
        43 => mark('\\', '|'),
        51 => mark(',', '<'),
        52 => mark('.', '>'),
        53 => mark('/', '?'),
        57 => ' ',
        // The keypad's digits, so a value can be typed on a number pad: the
        // fan curve's temperatures and percents. Kernel order, 7 8 9 over
        // 4 5 6 over 1 2 3 over 0, whatever Num Lock says -- there is no
        // keypad navigation here to give way to.
        71 => '7',
        72 => '8',
        73 => '9',
        75 => '4',
        76 => '5',
        77 => '6',
        79 => '1',
        80 => '2',
        81 => '3',
        82 => '0',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keyboard must not have a key that can turn the appliance off.
    #[test]
    fn no_key_code_maps_to_power() {
        for code in [116u32, 142, 356, 408, 0x198, 0x1ae] {
            for shift in [false, true] {
                assert!(
                    key_text(code, shift).is_none(),
                    "key code {code} reached the interface"
                );
            }
        }
    }

    #[test]
    fn the_keypad_types_digits() {
        let typed: String = [82, 79, 80, 81, 75, 76, 77, 71, 72, 73]
            .into_iter()
            .map(|code| key_text(code, false).unwrap().to_string())
            .collect();
        assert_eq!(typed, "0123456789");
    }

    #[test]
    fn a_film_can_be_typed() {
        let word: String = [30u32, 18, 19, 20, 57, 50, 50]
            .into_iter()
            .filter_map(|code| printable(code, false))
            .collect();
        assert_eq!(word, "aert mm");
    }

    /// There is no xkb here, so the modifier is this table's business. Without
    /// it every letter arrived lower case and half of ASCII could not be
    /// entered — which a film title survives and a password does not.
    #[test]
    fn shift_reaches_the_other_half_of_the_keyboard() {
        let typed = |codes: &[u32], shift: bool| -> String {
            codes
                .iter()
                .filter_map(|code| printable(*code, shift))
                .collect()
        };
        assert_eq!(typed(&[30, 31, 32], false), "asd");
        assert_eq!(typed(&[30, 31, 32], true), "ASD");
        // The number row carries the marks a password is made of.
        assert_eq!(
            typed(&[2, 3, 4, 5, 6, 7, 8, 9, 10, 11], false),
            "1234567890"
        );
        assert_eq!(typed(&[2, 3, 4, 5, 6, 7, 8, 9, 10, 11], true), "!@#$%^&*()");
    }

    /// The fault this was found by: an address could not be typed on a
    /// keyboard. The full stop and the underscore were not in the table at
    /// all, and the at sign needed a modifier nothing was tracking.
    #[test]
    fn an_address_can_be_typed_on_a_keyboard() {
        //          a      .      b      @      c      _      d
        let keys = [
            (30u32, false),
            (52, false),
            (48, false),
            (3, true),
            (46, false),
            (12, true),
            (32, false),
            (12, false),
            (18, false),
            (52, false),
            (46, false),
            (24, false),
            (50, false),
        ];
        let typed: String = keys
            .into_iter()
            .map(|(code, shift)| printable(code, shift).unwrap_or('?'))
            .collect();
        assert_eq!(typed, "a.b@c_d-e.com");
    }
}

pub fn install() -> Result<(), PlatformError> {
    slint::platform::set_platform(Box::new(SplitPlatform::new()?))
        .map_err(|e| PlatformError::from(e.to_string()))
}

/// A property of a KMS object, by name.
fn find_property<H: control::ResourceHandle>(
    kms: &SharedKms,
    object: H,
    name: &str,
) -> Option<control::property::Handle> {
    let properties = kms.get_properties(object).ok()?;
    properties
        .as_props_and_values()
        .0
        .iter()
        .copied()
        .find(|handle| {
            kms.get_property(*handle)
                .is_ok_and(|info| info.name().to_str() == Ok(name))
        })
}

/// Who hears about the display after every mode set: the application, which
/// tells the daemon.
static REPORTER: std::sync::OnceLock<Box<dyn Fn(mediabox_core::OwnerReport) + Send + Sync>> =
    std::sync::OnceLock::new();

pub fn on_output_report(reporter: impl Fn(mediabox_core::OwnerReport) + Send + Sync + 'static) {
    let _ = REPORTER.set(Box::new(reporter));
}

fn report(report: mediabox_core::OwnerReport) {
    if let Some(reporter) = REPORTER.get() {
        reporter(report);
    }
}

thread_local! {
    /// A setting to put on the wire, from the daemon, and its trial.
    static OUTPUT_WANTED: RefCell<Option<(mediabox_core::OutputSetting, Option<u64>)>> = const { RefCell::new(None) };
    /// The daemon asked to hear about the display again: it has started since
    /// the last report and knows nothing.
    static OUTPUT_REPORT_DUE: Cell<bool> = const { Cell::new(false) };
    /// Why the last mode asked for is not on the wire, when the kernel said no.
    static OUTPUT_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The setting on the wire and its trial, as last reported.
    static ON_WIRE: RefCell<Option<(mediabox_core::OutputSetting, Option<u64>)>> = const { RefCell::new(None) };
    /// The output and sink this process drives.
    static IDENTITY: RefCell<Option<mediabox_core::DisplayIdentity>> = const { RefCell::new(None) };
}

/// Put `setting` (trial `trial`, if it is one) on the wire from the next pass
/// of the event loop.
pub fn apply_output(setting: mediabox_core::OutputSetting, trial: Option<u64>) {
    OUTPUT_WANTED.with(|wanted| *wanted.borrow_mut() = Some((setting, trial)));
}

/// Tell the daemon about the display again, from the next pass of the event
/// loop.
pub fn report_output_again() {
    OUTPUT_REPORT_DUE.with(|due| due.set(true));
}

/// The setting on the wire and its trial, as last reported to the daemon.
pub fn on_wire() -> Option<(mediabox_core::OutputSetting, Option<u64>)> {
    ON_WIRE.with(|cell| cell.borrow().clone())
}

/// The output and sink this process drives; an `Apply` for any other is not
/// for it.
pub fn identity() -> Option<mediabox_core::DisplayIdentity> {
    IDENTITY.with(|cell| cell.borrow().clone())
}

pub fn output_error() -> Option<String> {
    OUTPUT_ERROR.with(|cell| cell.borrow().clone())
}

/// Where the daemon keeps the display setting. Read-only here; the daemon
/// writes it.
const SETTING_FILE: &str = "/var/lib/mediabox/output.json";

fn read_setting() -> Option<mediabox_core::StoredSetting> {
    std::fs::read_to_string(SETTING_FILE)
        .ok()
        .and_then(|text| mediabox_core::StoredSetting::parse(&text))
}

/// A KMS mode in the terms the HDMI rules are written in: the whole timing,
/// flags and all -- pixel repetition and interlace are in the flags, and a
/// mode known by its size and totals alone is not the mode the kernel listed.
fn timing_of(mode: &control::Mode) -> mediabox_platform::video::Timing {
    let (width, height) = mode.size();
    let (hsync_start, hsync_end, htotal) = mode.hsync();
    let (vsync_start, vsync_end, vtotal) = mode.vsync();
    mediabox_platform::video::Timing::from_mode(
        mediabox_core::ModeTiming::new(
            mode.clock(),
            width,
            hsync_start,
            hsync_end,
            htotal,
            mode.hskew(),
            height,
            vsync_start,
            vsync_end,
            vtotal,
            mode.vscan(),
            mode.flags().bits(),
        ),
        mode.mode_type().contains(control::ModeTypeFlags::PREFERRED),
    )
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    fn mode_4k50() -> control::Mode {
        let mut info: drm_ffi::drm_mode_modeinfo = unsafe { std::mem::zeroed() };
        info.clock = 594_000;
        (info.hdisplay, info.hsync_start, info.hsync_end, info.htotal) = (3840, 4896, 4984, 5280);
        (info.vdisplay, info.vsync_start, info.vsync_end, info.vtotal) = (2160, 2168, 2178, 2250);
        info.vrefresh = 50;
        control::Mode::from(info)
    }

    #[test]
    fn the_crystal_error_moves_the_pixel_clock_the_other_way() {
        // NTP's freq -11.403 ppm: the crystal 11.403 ppm fast, the clock
        // asked for that much slower.
        let wire = on_the_wire(mode_4k50(), Some(11.403));
        assert_eq!(wire.clock(), 593_993);
        assert_eq!(wire.size(), (3840, 2160));
        assert_eq!(on_the_wire(mode_4k50(), Some(-11.403)).clock(), 594_007);
        // Unsynchronised, or under a kHz: the EDID's.
        assert_eq!(on_the_wire(mode_4k50(), None).clock(), 594_000);
        assert_eq!(on_the_wire(mode_4k50(), Some(0.5)).clock(), 594_000);
        let period = period_of(&wire) as f64;
        assert!((period / 20e6 - 1.0 - 11.4e-6).abs() < 1e-6, "{period}");
    }
}
