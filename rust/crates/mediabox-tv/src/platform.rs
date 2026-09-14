//! RK3588 split render/display Slint platform.
//!
//! The vendor Mali GBM implementation works on `renderD128`; HDMI modesetting
//! belongs to `card0`. Every frame is therefore rendered into a GBM BO on the
//! former, exported as dma-buf, PRIME-imported on the latter and kept alive
//! until its KMS page-flip event arrives.

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
use slint::platform::{
    EventLoopProxy, Platform, PlatformError, WindowAdapter, WindowEvent,
};

const DEFAULT_RENDER_NODE: &str = "/dev/dri/renderD128";
const DEFAULT_KMS_NODE: &str = "/dev/dri/card0";

/// The size the interface is laid out for, seen from a sofa. Every metric in
/// the theme derives from the viewport, and its `min()` bounds are written
/// against this: handed a 3840x2560 logical viewport instead, every one of them
/// saturates at its cap and the home screen is no longer the home screen.
const DESIGN: (f32, f32) = (1920.0, 1080.0);

#[derive(Clone)]
struct SharedKms(Rc<OwnedFd>);

impl AsFd for SharedKms {
    fn as_fd(&self) -> BorrowedFd<'_> { self.0.as_fd() }
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
    fn size(&self) -> (u32, u32) { self.size }
    fn format(&self) -> DrmFourcc { DrmFourcc::Xrgb8888 }
    fn pitch(&self) -> u32 { self.pitch }
    fn handle(&self) -> BufferHandle { self.handle }
}

impl PlanarBuffer for ImportedPlane {
    fn size(&self) -> (u32, u32) { self.size }
    fn format(&self) -> DrmFourcc { DrmFourcc::Xrgb8888 }
    fn modifier(&self) -> Option<DrmModifier> {
        (!matches!(self.modifier, DrmModifier::Invalid)).then_some(self.modifier)
    }
    fn pitches(&self) -> [u32; 4] { [self.pitch, 0, 0, 0] }
    fn handles(&self) -> [Option<BufferHandle>; 4] { [Some(self.handle), None, None, None] }
    fn offsets(&self) -> [u32; 4] { [self.offset, 0, 0, 0] }
}

struct PresentedFrame {
    kms: SharedKms,
    framebuffer: control::framebuffer::Handle,
    imported_handle: BufferHandle,
    _dma_buf: OwnedFd,
    _bo: gbm::BufferObject<()>,
}

impl Drop for PresentedFrame {
    fn drop(&mut self) {
        if let Err(e) = self.kms.destroy_framebuffer(self.framebuffer) {
            eprintln!("mediabox-tv.kms cleanup rmfb failed: {e}");
        }
        if let Err(e) = self.kms.close_buffer(self.imported_handle) {
            eprintln!("mediabox-tv.kms cleanup gem-close failed: {e}");
        }
    }
}

#[derive(Default)]
struct Presentation {
    current: Option<PresentedFrame>,
    pending_previous: Option<PresentedFrame>,
    waiting_for_flip: bool,
    first_frame_logged: bool,
}

struct SplitDisplay {
    kms: SharedKms,
    connector: control::connector::Info,
    crtc: control::crtc::Handle,
    mode: control::Mode,
    gbm_device: gbm::Device<OwnedFd>,
    gbm_surface: gbm::Surface<()>,
    presentation: RefCell<Presentation>,
    released: Cell<bool>,
}

impl SplitDisplay {
    fn new() -> Result<Rc<Self>, PlatformError> {
        let render_path = std::env::var("MEDIABOX_RENDER_NODE")
            .unwrap_or_else(|_| DEFAULT_RENDER_NODE.into());
        let kms_path = std::env::var("MEDIABOX_KMS_NODE")
            .unwrap_or_else(|_| DEFAULT_KMS_NODE.into());

        let render_file = OpenOptions::new().read(true).write(true).open(&render_path)
            .map_err(|e| format!("open render node {render_path}: {e}"))?;
        let kms_file = OpenOptions::new().read(true).write(true).open(&kms_path)
            .map_err(|e| format!("open KMS node {kms_path}: {e}"))?;
        let kms = SharedKms(Rc::new(kms_file.into()));
        kms.acquire_master_lock().or_else(|e| {
            if e.raw_os_error() == Some(libc::EINVAL) { Ok(()) } else { Err(e) }
        }).map_err(|e| format!("acquire DRM master on {kms_path}: {e}"))?;

        let (connector, crtc, mode) = find_output(&kms)?;
        let gbm_device = gbm::Device::new(OwnedFd::from(render_file))
            .map_err(|e| format!("create GBM device on {render_path}: {e}"))?;
        if gbm_device.backend_name() != "armsoc" {
            return Err(format!(
                "software/non-vendor GBM rejected: backend={}", gbm_device.backend_name()
            ).into());
        }
        let (width, height) = mode.size();
        let gbm_surface = gbm_device.create_surface::<()>(
            width.into(), height.into(), gbm::Format::Xrgb8888,
            gbm::BufferObjectFlags::RENDERING
                | gbm::BufferObjectFlags::SCANOUT
                | gbm::BufferObjectFlags::LINEAR,
        ).map_err(|e| format!("create Mali GBM XRGB8888 surface: {e}"))?;

        eprintln!(
            "mediabox-tv.platform split-kms render={} gbm={} display={} output={}-{} mode={}x{}@{}",
            render_path, gbm_device.backend_name(), kms_path,
            connector.interface().as_str(), connector.interface_id(),
            width, height, mode.vrefresh()
        );

        let display = Rc::new(Self {
            kms, connector, crtc, mode, gbm_device, gbm_surface,
            presentation: RefCell::new(Presentation::default()),
            released: Cell::new(false),
        });
        eprintln!(
            "mediabox-tv.platform scale={:.3} logical={:.0}x{:.0} design={:.0}x{:.0}",
            display.scale_factor(),
            f32::from(width) / display.scale_factor(),
            f32::from(height) / display.scale_factor(),
            DESIGN.0, DESIGN.1
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
        if !self.presentation.borrow().waiting_for_flip { return Ok(()); }
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

    fn import_front_buffer(&self) -> Result<PresentedFrame, Box<dyn std::error::Error + Send + Sync>> {
        let bo = unsafe { self.gbm_surface.lock_front_buffer() }
            .map_err(|e| format!("lock Mali GBM front buffer: {e}"))?;
        if bo.format() != gbm::Format::Xrgb8888 {
            return Err(format!("unexpected GBM format: {:?}", bo.format()).into());
        }
        let dma_buf = bo.fd_for_plane(0)
            .map_err(|e| format!("export GBM BO as dma-buf: {e}"))?;
        let imported_handle = self.kms.prime_fd_to_buffer(dma_buf.as_fd())
            .map_err(|e| format!("DRM_IOCTL_PRIME_FD_TO_HANDLE(card0): {e}"))?;
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
                return Err(format!("DRM_IOCTL_MODE_ADDFB2(card0 PRIME buffer): {e}").into());
            }
        };
        Ok(PresentedFrame {
            kms: self.kms.clone(), framebuffer, imported_handle,
            _dma_buf: dma_buf, _bo: bo,
        })
    }

    fn present(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let frame = self.import_front_buffer()?;
        let mut state = self.presentation.borrow_mut();
        if state.current.is_none() {
            self.kms.set_crtc(
                self.crtc, Some(frame.framebuffer), (0, 0),
                &[self.connector.handle()], Some(self.mode),
            ).map_err(|e| format!("DRM_IOCTL_MODE_SETCRTC(first frame): {e}"))?;
            state.current = Some(frame);
        } else {
            self.kms.page_flip(
                self.crtc, frame.framebuffer, control::PageFlipFlags::EVENT, None,
            ).map_err(|e| format!("DRM_IOCTL_MODE_PAGE_FLIP: {e}"))?;
            state.pending_previous = state.current.replace(frame);
            state.waiting_for_flip = true;
        }
        if !state.first_frame_logged {
            let current = state.current.as_ref().unwrap();
            eprintln!(
                "mediabox-tv.present dma-buf=active card0_fb={} native={}x{}",
                u32::from(current.framebuffer), self.size().width, self.size().height
            );
            state.first_frame_logged = true;
        }
        Ok(())
    }

    fn release_display(&self) {
        if self.released.replace(true) { return; }
        let _ = self.wait_for_page_flip();
        let _ = self.kms.set_crtc(self.crtc, None, (0, 0), &[], None);
        let (pending, current) = {
            let mut state = self.presentation.borrow_mut();
            (state.pending_previous.take(), state.current.take())
        };
        drop(pending);
        drop(current);
        let _ = self.kms.release_master_lock();
        eprintln!("mediabox-tv.platform DRM released");
    }
}

impl Drop for SplitDisplay {
    fn drop(&mut self) { self.release_display(); }
}

fn find_output(kms: &SharedKms) -> Result<(control::connector::Info, control::crtc::Handle, control::Mode), PlatformError> {
    let resources = kms.resource_handles()
        .map_err(|e| format!("read KMS resources: {e}"))?;
    let connector = resources.connectors().iter().find_map(|handle| {
        let connector = kms.get_connector(*handle, false).ok()?;
        (connector.state() == control::connector::State::Connected
            && connector.interface().as_str() == "HDMI-A"
            && !connector.modes().is_empty()).then_some(connector)
    }).ok_or_else(|| PlatformError::from("no connected HDMI-A output".to_string()))?;

    let mode = connector.modes().iter().max_by_key(|mode| {
        let preferred = mode.mode_type().contains(control::ModeTypeFlags::PREFERRED);
        let (w, h) = mode.size();
        (preferred, u32::from(w) * u32::from(h), mode.vrefresh())
    }).copied().ok_or_else(|| PlatformError::from("HDMI output has no mode".to_string()))?;

    let current = connector.current_encoder()
        .filter(|handle| connector.encoders().contains(handle))
        .and_then(|handle| kms.get_encoder(handle).ok())
        .and_then(|encoder| encoder.crtc());
    let crtc = current.or_else(|| {
        connector.encoders().iter()
            .filter_map(|handle| kms.get_encoder(*handle).ok())
            .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
            .next()
    }).ok_or_else(|| PlatformError::from("HDMI output has no compatible CRTC".to_string()))?;
    Ok((connector, crtc, mode))
}

impl HasWindowHandle for SplitDisplay {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe {
            let handle = raw_window_handle::GbmWindowHandle::new(
                std::ptr::NonNull::from(&*self.gbm_surface.as_raw()).cast()
            );
            raw_window_handle::WindowHandle::borrow_raw(
                raw_window_handle::RawWindowHandle::Gbm(handle)
            )
        })
    }
}

impl HasDisplayHandle for SplitDisplay {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe {
            let handle = raw_window_handle::GbmDisplayHandle::new(
                std::ptr::NonNull::from(&*self.gbm_device.as_raw()).cast()
            );
            raw_window_handle::DisplayHandle::borrow_raw(
                raw_window_handle::RawDisplayHandle::Gbm(handle)
            )
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
        let display_handle = display.display_handle()
            .map_err(|e| format!("GBM display handle: {e}"))?;
        let window_handle = display.window_handle()
            .map_err(|e| format!("GBM window handle: {e}"))?;
        let gl_display = unsafe {
            glutin::display::Display::new(display_handle.as_raw(), DisplayApiPreference::Egl)
        }.map_err(|e| format!("create EGL display on Mali GBM: {e}"))?;
        let template = ConfigTemplateBuilder::new()
            .with_transparency(false).with_alpha_size(0).build();
        let config = unsafe { gl_display.find_configs(template) }
            .map_err(|e| format!("enumerate EGL configs: {e}"))?
            .filter(|config| matches!(config, glutin::config::Config::Egl(egl) if egl.native_visual() as u32 == gbm::Format::Xrgb8888 as u32))
            .min_by_key(|config| config.num_samples())
            .ok_or_else(|| PlatformError::from("no EGL XRGB8888 window config".to_string()))?;
        let attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(glutin::context::Version { major: 2, minor: 0 })))
            .build(Some(window_handle.as_raw()));
        let context = unsafe { gl_display.create_context(&config, &attrs) }
            .map_err(|e| format!("create Mali EGL GLES context: {e}"))?;
        let size = display.size();
        let width = NonZeroU32::new(size.width).unwrap();
        let height = NonZeroU32::new(size.height).unwrap();
        let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new()
            .build(window_handle.as_raw(), width, height);
        let surface = unsafe { config.display().create_window_surface(&config, &surface_attrs) }
            .map_err(|e| format!("create Mali EGL window surface: {e}"))?;
        let context = context.make_current(&surface)
            .map_err(|e| format!("make Mali EGL context current: {e}"))?;

        let egl_vendor = egl_string(&gl_display, 0x3053).unwrap_or_else(|| "unknown".into());
        let egl_version = egl_string(&gl_display, 0x3054).unwrap_or_else(|| gl_display.version_string());
        let gl_vendor = gl_string(&gl_display, 0x1F00).unwrap_or_else(|| "unknown".into());
        let gl_renderer = gl_string(&gl_display, 0x1F01).unwrap_or_else(|| "unknown".into());
        if egl_vendor != "ARM" || !gl_renderer.contains("Mali") {
            return Err(format!(
                "software/non-Mali EGL rejected: EGL_VENDOR={egl_vendor} GL_RENDERER={gl_renderer}"
            ).into());
        }
        eprintln!(
            "mediabox-tv.gpu EGL_VENDOR={} EGL_VERSION={} GL_VENDOR={} GL_RENDERER={}",
            egl_vendor, egl_version, gl_vendor, gl_renderer
        );
        Ok(Self { context, surface, display })
    }
}

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglQueryString(display: *const std::ffi::c_void, name: i32) -> *const libc::c_char;
}

fn egl_string(display: &glutin::display::Display, name: i32) -> Option<String> {
    let RawDisplay::Egl(raw) = display.raw_display();
    let value = unsafe { eglQueryString(raw, name) };
    (!value.is_null()).then(|| unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned())
}

fn gl_string(display: &glutin::display::Display, name: u32) -> Option<String> {
    type GlGetString = unsafe extern "C" fn(u32) -> *const u8;
    let symbol = CString::new("glGetString").unwrap();
    let address = display.get_proc_address(&symbol);
    if address.is_null() { return None; }
    let get_string: GlGetString = unsafe { std::mem::transmute(address) };
    let value = unsafe { get_string(name) };
    (!value.is_null()).then(|| unsafe { CStr::from_ptr(value.cast()) }.to_string_lossy().into_owned())
}

unsafe impl OpenGLInterface for GlContext {
    fn ensure_current(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.context.is_current() {
            self.context.make_current(&self.surface)?;
        }
        Ok(())
    }

    fn swap_buffers(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.display.wait_for_page_flip()?;
        self.surface.swap_buffers(&self.context)?;
        self.display.present()
    }

    fn resize(&self, _width: NonZeroU32, _height: NonZeroU32)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>> { Ok(()) }

    fn get_proc_address(&self, name: &CStr) -> *const std::ffi::c_void {
        self.context.display().get_proc_address(name)
    }
}

struct SplitWindow {
    window: slint::Window,
    renderer: FemtoVGRenderer,
    display: Rc<SplitDisplay>,
    redraw: Cell<bool>,
}

impl SplitWindow {
    fn new(display: Rc<SplitDisplay>) -> Result<Rc<Self>, PlatformError> {
        let renderer = FemtoVGRenderer::new(GlContext::new(display.clone())?)?;
        Ok(Rc::new_cyclic(|weak: &std::rc::Weak<Self>| Self {
            window: slint::Window::new(weak.clone()),
            renderer,
            display,
            redraw: Cell::new(true),
        }))
    }

    fn render_if_needed(&self) -> Result<(), PlatformError> {
        if self.redraw.replace(false) {
            self.renderer.render()?;
            if self.window.has_active_animations() { self.redraw.set(true); }
        }
        Ok(())
    }
}

impl WindowAdapter for SplitWindow {
    fn window(&self) -> &slint::Window { &self.window }
    fn size(&self) -> slint::PhysicalSize { self.display.size() }
    fn renderer(&self) -> &dyn slint::platform::Renderer { &self.renderer }
    fn request_redraw(&self) { self.redraw.set(true); }
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

enum LoopMessage { Invoke(Box<dyn FnOnce() + Send>), Quit }

#[derive(Clone)]
struct Proxy {
    sender: mpsc::Sender<LoopMessage>,
    wake: Arc<OwnedFd>,
}

impl Proxy {
    fn send(&self, message: LoopMessage) -> Result<(), slint::EventLoopError> {
        self.sender.send(message).map_err(|_| slint::EventLoopError::EventLoopTerminated)?;
        let one: u64 = 1;
        unsafe { libc::write(self.wake.as_raw_fd(), (&one as *const u64).cast(), 8); }
        Ok(())
    }
}

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> { self.send(LoopMessage::Quit) }
    fn invoke_from_event_loop(&self, event: Box<dyn FnOnce() + Send>)
        -> Result<(), slint::EventLoopError> { self.send(LoopMessage::Invoke(event)) }
}

struct DirectInput;

impl LibinputInterface for DirectInput {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let access = flags & libc::O_ACCMODE;
        OpenOptions::new()
            .read(access == libc::O_RDONLY || access == libc::O_RDWR)
            .write(access == libc::O_WRONLY || access == libc::O_RDWR)
            .custom_flags(flags & !libc::O_ACCMODE)
            .open(path).map(Into::into)
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
    }
    fn close_restricted(&mut self, fd: OwnedFd) { drop(File::from(fd)); }
}

struct SplitPlatform {
    window: Rc<SplitWindow>,
    receiver: RefCell<mpsc::Receiver<LoopMessage>>,
    proxy: Proxy,
}

impl SplitPlatform {
    fn new() -> Result<Self, PlatformError> {
        let display = SplitDisplay::new()?;
        let window = SplitWindow::new(display)?;
        let (sender, receiver) = mpsc::channel();
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 { return Err(format!("create event-loop eventfd: {}", std::io::Error::last_os_error()).into()); }
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        Ok(Self { window, receiver: RefCell::new(receiver), proxy: Proxy { sender, wake } })
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
        libinput.dispatch().map_err(|e| format!("libinput dispatch: {e}"))?;
        for event in libinput {
            let input::Event::Keyboard(input::event::KeyboardEvent::Key(key)) = event else { continue };
            if key.device().name().to_ascii_lowercase().contains("cec") { continue; }
            let Some(text) = key_text(key.key()) else { continue };
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
        libinput.udev_assign_seat("seat0")
            .map_err(|_| PlatformError::from("libinput could not assign seat0".to_string()))?;
        loop {
            slint::platform::update_timers_and_animations();
            if self.drain_messages() { break; }
            self.window.render_if_needed()?;

            let timeout = if self.window.redraw.get() {
                0
            } else {
                slint::platform::duration_until_next_timer_update()
                    .unwrap_or(Duration::from_millis(250))
                    .min(Duration::from_millis(250)).as_millis() as i32
            };
            let mut pollfds = [
                libc::pollfd { fd: self.proxy.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: libinput.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            let rc = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, timeout) };
            if rc < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("event loop poll: {}", std::io::Error::last_os_error()).into());
            }
            if pollfds[0].revents & libc::POLLIN != 0 {
                let mut value: u64 = 0;
                unsafe { libc::read(self.proxy.wake.as_raw_fd(), (&mut value as *mut u64).cast(), 8); }
            }
            if pollfds[1].revents & libc::POLLIN != 0 { self.dispatch_input(&mut libinput)?; }
        }
        self.window.display.release_display();
        Ok(())
    }
}

fn key_text(code: u32) -> Option<slint::SharedString> {
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
        _ => return None,
    };
    Some(char::from(key).to_string().into())
}

pub fn install() -> Result<(), PlatformError> {
    slint::platform::set_platform(Box::new(SplitPlatform::new()?))
        .map_err(|e| PlatformError::from(e.to_string()))
}
