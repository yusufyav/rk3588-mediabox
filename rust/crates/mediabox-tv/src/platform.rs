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
use std::sync::{Arc, mpsc};
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
struct SharedKms(Rc<OwnedFd>);

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

#[derive(Default)]
struct Presentation {
    current: Option<PresentedFrame>,
    pending_previous: Option<PresentedFrame>,
    waiting_for_flip: bool,
    first_frame_logged: bool,
    /// How many buffers have needed a framebuffer built for them. On a healthy
    /// run this stops at the surface's buffer count — two or three — and the
    /// per-frame import work is gone. A number that keeps climbing means the
    /// cache is not working and is worth seeing in the journal.
    imports: u32,
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

thread_local! {
    static VIDEO: Cell<bool> = const { Cell::new(false) };
}

/// Whether a film is on the video plane at this moment.
///
/// The interface is still drawn underneath it — the film is a window of this
/// process, not another application — but the plane sits above the primary, so
/// while this is true the panel is showing the film and not the catalogue.
pub fn video_showing() -> bool {
    VIDEO.with(|cell| cell.get())
}

struct SplitDisplay {
    kms: SharedKms,
    /// The overlays the appliance's own player can draw into, best first.
    /// Enumerated at startup and held for the life of the process: asking the
    /// display controller for a window while a film is waiting to start is a
    /// question with a worse moment to ask it.
    video: Vec<crate::video::VideoPlane>,
    /// The socket the player hands decoded frames down, and the frames the
    /// display controller has not finished with.
    sink: RefCell<crate::video::Sink<SharedKms>>,
    connector: control::connector::Info,
    crtc: control::crtc::Handle,
    mode: control::Mode,
    gbm_device: gbm::Device<OwnedFd>,
    gbm_surface: gbm::Surface<Scanout>,
    presentation: RefCell<Presentation>,
    released: Cell<bool>,
    /// Whether the television is currently away, so the journal says so once
    /// rather than sixty times a second.
    dark: Cell<bool>,
    /// What the television said it can be sent, read from its EDID once. The
    /// HDR decision is made against this and the mode in use, never against
    /// the film alone: a set that never declared ST 2084, or a link with no
    /// room for ten bits at this timing, cannot be sent HDR worth having.
    sink_video: Option<mediabox_platform::video::SinkVideo>,
    /// The mode in use, in the terms the colour rules are written in.
    timing: mediabox_platform::video::Timing,
    /// The colour mode a person chose in the settings, `Auto` otherwise.
    colour_choice: Cell<mediabox_core::ColorChoice>,
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
        let kms = SharedKms(Rc::new(kms_file.into()));
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

        let (connector, crtc, mode) = find_output(&kms, &wanted)?;
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
                .and_then(|output| output.audio.as_ref())
                .map(|audio| audio.card_id.as_str())
                .unwrap_or("-"),
            discovered
                .selected_output()
                .and_then(|output| output.cec.as_ref())
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

        // What the television declared about itself, kept for the HDR
        // decision: whether it says ST 2084 at all, and how fast its link is.
        let sink_video = connector_edid(&kms, connector.handle())
            .and_then(|edid| mediabox_platform::video::parse_sink_video(&edid));
        if let Some(sink) = &sink_video {
            eprintln!(
                "mediabox-tv.platform sink: ST2084={} HLG={} link {} kHz, HDR10 at this mode: {}",
                sink.st2084,
                sink.hlg,
                sink.max_character_rate_khz,
                sink.hdr10_fits(&timing_of(&mode))
            );
        }

        let display = Rc::new(Self {
            kms,
            connector,
            crtc,
            mode,
            sink_video,
            timing: timing_of(&mode),
            colour_choice: Cell::new(
                read_choice::<mediabox_core::ColorChoice>(COLOUR_FILE).unwrap_or_default(),
            ),
            applied: Cell::new(None),
            signalled: Cell::new(None),
            asked: Cell::new(None),
            gbm_device,
            gbm_surface,
            video,
            sink: RefCell::new(sink),
            presentation: RefCell::new(Presentation::default()),
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
        let (width, height) = self.mode.size();
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
        let (width, height) = self.mode.size();
        let fit = (f32::from(width) / DESIGN.0).min(f32::from(height) / DESIGN.1);
        // Rounded up rather than down: on a panel between two whole scales the
        // tighter one is the television-shaped answer.
        (fit - 1e-6).ceil().clamp(1.0, 4.0)
    }

    fn wait_for_page_flip(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.presentation.borrow().waiting_for_flip {
            return Ok(());
        }
        loop {
            let mut events = self.kms.receive_events()?;
            if events.any(|event| matches!(event, control::Event::PageFlip(_))) {
                let old = {
                    let mut state = self.presentation.borrow_mut();
                    state.waiting_for_flip = false;
                    state.pending_previous.take()
                };
                drop(old);
                return Ok(());
            }
        }
    }

    /// Locks whatever the renderer just finished and returns it ready to flip.
    ///
    /// The expensive half — dma-buf export, PRIME import, ADDFB2 — happens only
    /// the first time each of the surface's buffers is seen. After that this is
    /// one `gbm_surface_lock_front_buffer` and a handle read.
    fn import_front_buffer(
        &self,
    ) -> Result<PresentedFrame, Box<dyn std::error::Error + Send + Sync>> {
        let mut bo = unsafe { self.gbm_surface.lock_front_buffer() }
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
        // A colour mode chosen in the settings is put on the wire here, on the
        // next frame, rather than waiting for the next film or restart.
        if let Some(choice) = COLOUR_WANTED.with(Cell::take) {
            self.colour_choice.set(choice);
            self.signal_output_colour(self.asked.get());
        }
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
                let mut state = self.presentation.borrow_mut();
                state.waiting_for_flip = false;
                state.pending_previous = None;
                state.current = None;
                Ok(())
            }
        }
    }

    fn present_frame(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let frame = self.import_front_buffer()?;
        let mut state = self.presentation.borrow_mut();
        if state.current.is_none() {
            self.kms
                .set_crtc(
                    self.crtc,
                    Some(frame.framebuffer),
                    (0, 0),
                    &[self.connector.handle()],
                    Some(self.mode),
                )
                .map_err(|e| format!("DRM_IOCTL_MODE_SETCRTC(first frame): {e}"))?;
            state.current = Some(frame);
        } else {
            // Only one flip may be outstanding on a CRTC. When the wait in
            // `swap_buffers` was skipped because a free buffer was available,
            // this is where the pace is kept.
            if state.waiting_for_flip {
                drop(state);
                self.wait_for_page_flip()?;
                state = self.presentation.borrow_mut();
            }
            self.kms
                .page_flip(
                    self.crtc,
                    frame.framebuffer,
                    control::PageFlipFlags::EVENT,
                    None,
                )
                .map_err(|e| format!("DRM_IOCTL_MODE_PAGE_FLIP: {e}"))?;
            state.pending_previous = state.current.replace(frame);
            state.waiting_for_flip = true;
        }
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
            return Ok(());
        }
        Ok(())
    }

    /// Everything waiting on the player's socket, put on the panel.
    ///
    /// Called from the event loop rather than from a thread of its own: the
    /// descriptor is polled beside libinput and the wake pipe, so a frame
    /// arriving wakes the interface exactly the way a key press does, and the
    /// ioctls that import and show it happen on the thread that holds master.
    fn pump_video(&self) {
        if self.released.get() {
            return;
        }
        let (width, height) = self.mode.size();
        let into = crate::video::Rect {
            x: 0,
            y: 0,
            width: width.into(),
            height: height.into(),
        };
        let mut sink = self.sink.borrow_mut();
        sink.pump(&self.kms, self.crtc, &self.video, into);
        self.signal_output_colour(sink.hdr());
        VIDEO.with(|cell| cell.set(sink.showing()));
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
    /// And one decision before them: whether this link can carry it at the
    /// timing that is actually set. It is the same rule the rest of the
    /// product already uses -- a 4K film at 23.976 fits ten-bit RGB in
    /// 371 MHz, at 60 Hz it does not -- and when it does not fit, HDR is
    /// given up rather than asked for and silently subsampled.
    fn signal_output_colour(&self, film: Option<crate::video::HdrStatic>) {
        let fits = self
            .sink_video
            .as_ref()
            .is_some_and(|sink| sink.hdr10_fits(&self.timing));
        let wanted = match film {
            Some(hdr) if hdr.is_hdr() && fits => Some(hdr),
            _ => None,
        };
        if self.asked.replace(film) != film {
            match film {
                Some(hdr) if hdr.is_hdr() && !fits => eprintln!(
                    "mediabox-tv.platform the film asks for HDR (eotf {}) and this link \
                     cannot carry it at {} kHz: sending SDR, the plane is tone-mapped",
                    hdr.eotf,
                    self.mode.clock()
                ),
                Some(hdr) if hdr.is_hdr() => eprintln!(
                    "mediabox-tv.platform the film asks for HDR (eotf {}), and it fits",
                    hdr.eotf
                ),
                Some(_) => eprintln!("mediabox-tv.platform the film is SDR"),
                None => {}
            }
        }
        // The format and depth on the wire, by the same rules as the list in
        // the settings: the one a person chose if it can be sent at this mode
        // (and carries HDR, when HDR is wanted), otherwise what `Auto` gives.
        let format = self.sink_video.as_ref().and_then(|sink| {
            let allowed = sink.modes_for(&self.timing);
            let fixed = self.colour_choice.get().mode().filter(|mode| allowed.contains(mode));
            match wanted {
                Some(_) => fixed
                    .filter(|mode| mode.carries_hdr())
                    .or_else(|| sink.best_for(&self.timing, true)),
                None => fixed.or_else(|| sink.best_for(&self.timing, false)),
            }
        });
        if self.signalled.get() == wanted && self.applied.get() == format {
            return;
        }

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

        // As this driver enumerates them: `Colorspace` Default=0,
        // BT2020_RGB=9, BT2020_YCC=10; `color_depth` Automatic=0, 24bit=8,
        // 30bit=10; `color_format` rgb=0, ycbcr444=1, ycbcr422=2, ycbcr420=3.
        use mediabox_core::ColorFormat;
        let (space, bits, layout) = match format {
            Some(mode) => (
                match (wanted, mode.format) {
                    (Some(_), ColorFormat::Rgb) => 9,
                    (Some(_), _) => 10,
                    (None, _) => 0,
                },
                u64::from(mode.bits),
                Some(match mode.format {
                    ColorFormat::Rgb => 0u64,
                    ColorFormat::Ycbcr444 => 1,
                    ColorFormat::Ycbcr422 => 2,
                    ColorFormat::Ycbcr420 => 3,
                }),
            ),
            // No EDID to reason from: the driver's own choice.
            None => (0, 0, None),
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
        if let Some(property) = depth
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
                    self.mode.clock()
                ),
                None => "SDR".to_string(),
            }
        );
        eprintln!(
            "mediabox-tv.platform output format: {} at {}",
            format.map(|mode| mode.label()).unwrap_or_else(|| "sürücünün seçimi".into()),
            self.timing.label()
        );
        self.signalled.set(wanted);
    }

    fn release_display(&self) {
        if self.released.replace(true) {
            return;
        }
        // The film first: a plane left enabled on a CRTC that is about to be
        // turned off is how the next master inherits a picture it did not draw.
        self.sink
            .borrow_mut()
            .clear(&self.kms, self.crtc, &self.video);
        // And the stacking order, which belongs to the connector rather than to
        // whoever set it: Kodi assigns this port's windows for itself and must
        // not inherit a primary this process raised.
        crate::video::lower_interface(&self.kms, self.crtc);
        let _ = self.wait_for_page_flip();
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

/// The connector discovery chose, found again through the open KMS device.
///
/// Discovery works from sysfs and names an output; this needs the DRM objects
/// behind that name. They are matched by connector *name* — type plus type
/// index, which is what sysfs publishes — and never by DRM object id: object
/// ids are allocated per boot and a different kernel hands out different ones.
/// The sink's EDID, as the connector publishes it.
///
/// Read here rather than from `/sys/class/drm/*/edid`, which this vendor
/// driver leaves empty -- measured, zero bytes on a television that is plainly
/// connected and answering.
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

fn find_output(
    kms: &SharedKms,
    wanted: &str,
) -> Result<
    (
        control::connector::Info,
        control::crtc::Handle,
        control::Mode,
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

    // The biggest mode this link can actually carry -- not the one the sink
    // puts first.
    //
    // Ranking `preferred` above size looks harmless until a television says
    // this, which one of ours does on one of its inputs:
    //
    //     #0  1920x1080 60.00  type: preferred, driver
    //     #6  3840x2160 60.00  594000 kHz
    //     #13 3840x2160 23.98  296703 kHz
    //
    // The appliance then ran its whole interface at 1080p on a 4K panel, and
    // every 4K film with it, because a film is composited into the mode the
    // interface set rather than given one of its own.
    //
    // So: size first, then the fastest refresh, and `preferred` only to settle
    // a tie. And a timing the link cannot carry is not a candidate at all --
    // that sink's link stops at 300 MHz, where 4K60 wants 594, and asking for
    // it anyway is how this driver ends up quietly subsampling a picture
    // nobody asked it to touch.
    let sink = connector_edid(kms, connector.handle())
        .and_then(|edid| mediabox_platform::video::parse_sink_video(&edid));
    // Carried means carried in any format the sink takes, as every HDMI source
    // does it.
    //
    // This used to demand RGB, so on the Sony's 300 MHz input the interface
    // ran at 3840x2160@30. The reference Android box on the same input, same
    // afternoon, runs 2160p60 in YCbCr 4:2:0 8-bit -- the only format that fits
    // 594 MHz of pixels into 300 -- and offers the other formats only at modes
    // where they fit. The kernel already listed 4K60 for that input and falls
    // back to 4:2:0 on its own; the interface was the only thing refusing it.
    // Which mode, by the product's one rule set: the modes the kernel lists
    // for this sink, the resolution a person chose if this sink lists it and
    // the link carries it, `Auto` otherwise. See mediabox_platform::video.
    let modes: Vec<control::Mode> = connector.modes().to_vec();
    let timings: Vec<mediabox_platform::video::Timing> = modes.iter().map(timing_of).collect();
    let choice = read_choice::<mediabox_core::ResolutionChoice>(RESOLUTION_FILE).unwrap_or_default();
    let chosen = mediabox_platform::video::choose_timing(choice, &timings, sink.as_ref())
        .ok_or_else(|| PlatformError::from(format!("{wanted} has no mode")))?;
    let auto = mediabox_platform::video::auto_timing(&timings, sink.as_ref());
    let mode = modes[timings.iter().position(|timing| *timing == chosen).unwrap_or(0)];
    publish_offer(&timings, chosen, auto, choice, sink.as_ref());
    if let Some(sink) = &sink {
        let (w, h) = mode.size();
        eprintln!(
            "mediabox-tv.platform {wanted} {w}x{h}@{} chosen against a {} kHz link{}",
            mode.vrefresh(),
            sink.max_character_rate_khz,
            if sink.rate_is_declared {
                ""
            } else {
                " (assumed: the sink declared none)"
            }
        );
    }

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
    Ok((connector, crtc, mode))
}

impl HasWindowHandle for SplitDisplay {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe {
            let handle = raw_window_handle::GbmWindowHandle::new(
                std::ptr::NonNull::from(&*self.gbm_surface.as_raw()).cast(),
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
    surface: glutin::surface::Surface<WindowSurface>,
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
        let size = display.size();
        let width = NonZeroU32::new(size.width).unwrap();
        let height = NonZeroU32::new(size.height).unwrap();
        let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            window_handle.as_raw(),
            width,
            height,
        );
        let surface = unsafe {
            config
                .display()
                .create_window_surface(&config, &surface_attrs)
        }
        .map_err(|e| format!("create Mali EGL window surface: {e}"))?;
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
        Ok(Self {
            context,
            surface,
            display,
        })
    }
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
        if !self.context.is_current() {
            self.context.make_current(&self.surface)?;
        }
        Ok(())
    }

    fn swap_buffers(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Only wait when there is nothing left to draw into. With a surface
        // that allocates a third buffer this lets the GPU start the next frame
        // while the panel is still showing the last one; with two it is the
        // same wait as before, in the same place.
        let at = std::time::Instant::now();
        if !self.display.gbm_surface.has_free_buffers() {
            self.display.wait_for_page_flip()?;
        }
        let waited = at.elapsed();

        // Before the swap, while the finished frame is still the one the
        // context will read from.
        if let Some(path) = SNAPSHOT.with(|cell| cell.borrow_mut().take()) {
            self.capture(&path);
        }

        let at = std::time::Instant::now();
        self.surface.swap_buffers(&self.context)?;
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

    fn render_if_needed(&self) -> Result<(), PlatformError> {
        if self.redraw.replace(false) {
            let at = std::time::Instant::now();
            self.renderer.render()?;
            let total = at.elapsed();
            add_phase(|phases| {
                phases.frames += 1;
                phases.total_us += total.as_micros() as u64;
            });
            if self.bench || self.window.has_active_animations() {
                self.redraw.set(true);
            }
        }
        Ok(())
    }
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
    remotes: RefCell<std::collections::HashMap<String, bool>>,
    /// Whether a shift key is down.
    ///
    /// libinput hands over key codes, not characters: there is no xkb here and
    /// nothing else is tracking the modifiers, so without this every letter
    /// arrived lower case and half of ASCII could not be typed at all. A film
    /// title did not care. A password does.
    shift: std::cell::Cell<bool>,
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
            shift: std::cell::Cell::new(false),
        })
    }

    /// Whether a device is a remote control rather than a keyboard.
    ///
    /// The daemon owns the remote: it holds the CEC adapter, decodes the user-control
    /// codes and publishes them as normalised actions, and this interface
    /// listens to that. The kernel *also* registers the same remote as an
    /// rc-core input device, so without this every press of the television
    /// remote reaches the interface twice — once here as an ordinary key and
    /// once as an action. A compositor used to switch that device off; there is
    /// no compositor any more.
    ///
    /// Decided on what the device is attached to rather than on its name. The
    /// CEC remote on this board is called "dw_hdmi_qp", which contains neither
    /// "cec" nor "remote", and the name check that came before this let every
    /// press through.
    fn remote_control(&self, device: &input::Device) -> bool {
        let sysname = device.sysname().to_string();
        if let Some(answer) = self.remotes.borrow().get(&sysname) {
            return *answer;
        }

        // /sys/class/input/event0 -> ../../devices/platform/fdea0000.hdmi/rc/rc0/input0/event0
        let path = std::fs::canonicalize(format!("/sys/class/input/{sysname}"))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let answer = path.contains("/rc/rc")
            || device.name().to_ascii_lowercase().contains("cec")
            || device.name() == "dw_hdmi_qp";

        eprintln!(
            "mediabox-tv.input device {} name={:?} remote={}",
            sysname,
            device.name(),
            answer
        );
        self.remotes.borrow_mut().insert(sysname, answer);
        answer
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
            let input::Event::Keyboard(input::event::KeyboardEvent::Key(key)) = event else {
                continue;
            };
            if self.remote_control(&key.device()) {
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
            let event = match key.key_state() {
                KeyState::Pressed => WindowEvent::KeyPressed { text },
                KeyState::Released => WindowEvent::KeyReleased { text },
            };
            self.window.window.dispatch_event(event);
        }
        Ok(())
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
        let mut libinput = input::Libinput::new_with_udev(DirectInput);
        libinput
            .udev_assign_seat("seat0")
            .map_err(|_| PlatformError::from("libinput could not assign seat0".to_string()))?;
        loop {
            slint::platform::update_timers_and_animations();
            if self.drain_messages() {
                break;
            }
            self.window.render_if_needed()?;

            let timeout = if self.window.redraw.get() {
                0
            } else {
                slint::platform::duration_until_next_timer_update()
                    .unwrap_or(Duration::from_millis(250))
                    .min(Duration::from_millis(250))
                    .as_millis() as i32
            };
            // The third is the player's socket. Its descriptor is the door
            // while nothing is playing and the player itself once something is,
            // so it is read again on every pass rather than kept.
            let video = self.window.display.sink.borrow().poll_fd();
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
                libc::pollfd {
                    fd: video,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let rc = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, timeout) };
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
            if pollfds[2].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                self.window.display.pump_video();
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

/// Where the daemon remembers the choices made in the settings. Read-only
/// here; the daemon writes them.
const RESOLUTION_FILE: &str = "/var/lib/mediabox/resolution";
const COLOUR_FILE: &str = "/var/lib/mediabox/color-mode";

fn read_choice<T: serde::de::DeserializeOwned>(path: &str) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim()).ok()
}

/// A KMS mode in the terms the HDMI rules are written in.
fn timing_of(mode: &control::Mode) -> mediabox_platform::video::Timing {
    let (width, height) = mode.size();
    mediabox_platform::video::Timing::new(
        width,
        height,
        mode.clock(),
        mode.hsync().2,
        mode.vsync().2,
        mode.flags().contains(control::ModeFlags::INTERLACE),
        mode.mode_type().contains(control::ModeTypeFlags::PREFERRED),
    )
}

/// What the settings screen offers: every mode the kernel lists for this
/// sink, which one is on, what `Auto` would be, and the colour modes each
/// can be sent in.
static OFFER: std::sync::Mutex<Option<serde_json::Value>> = std::sync::Mutex::new(None);

pub fn display_offer() -> Option<serde_json::Value> {
    OFFER.lock().ok()?.clone()
}

fn publish_offer(
    timings: &[mediabox_platform::video::Timing],
    current: mediabox_platform::video::Timing,
    auto: Option<mediabox_platform::video::Timing>,
    choice: mediabox_core::ResolutionChoice,
    sink: Option<&mediabox_platform::video::SinkVideo>,
) {
    let mut seen = std::collections::HashSet::new();
    let list: Vec<serde_json::Value> = timings
        .iter()
        .filter(|timing| seen.insert((timing.width, timing.height, timing.refresh_mhz, timing.interlaced)))
        .map(|timing| {
            let allowed = sink.map(|sink| sink.modes_for(timing)).unwrap_or_default();
            serde_json::json!({
                "label": timing.label(),
                "width": timing.width,
                "height": timing.height,
                "refresh_mhz": timing.refresh_mhz,
                "interlaced": timing.interlaced,
                "allowed": allowed,
                "hdr10": sink.is_some_and(|sink| sink.hdr10_fits(timing)),
                "auto_sdr": sink.and_then(|sink| sink.best_for(timing, false)),
            })
        })
        .collect();
    let offer = serde_json::json!({
        "timings": list,
        "current": current.label(),
        "auto": auto.map(|timing| timing.label()),
        "choice": choice,
    });
    if let Ok(mut slot) = OFFER.lock() {
        *slot = Some(offer);
    }
}

thread_local! {
    static COLOUR_WANTED: Cell<Option<mediabox_core::ColorChoice>> = const { Cell::new(None) };
}

/// Put a colour mode chosen in the settings on the wire, from the next frame.
pub fn set_colour_choice(choice: mediabox_core::ColorChoice) {
    COLOUR_WANTED.with(|wanted| wanted.set(Some(choice)));
}
