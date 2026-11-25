//! The Skia experiment: Slint's Skia renderer on this platform's own EGL
//! context, GBM surface and page flips. Not the product's renderer
//! (docs/quietos-advanced-graphics.md says why); built only with the `skia`
//! feature and used only when MEDIABOX_TV_RENDERER asks.
//!
//! Slint's ready-made OpenGL surface makes an EGL display, context and swap
//! of its own. This one draws through the context the product has, so the
//! presentation -- DMA-BUF export, PRIME import, the atomic commit -- is the
//! same code FemtoVG's frames go through, and there is no software fallback
//! to fall into: the feature set has none.

use std::cell::RefCell;
use std::sync::Arc;

use i_slint_core::api::GraphicsAPI;
use i_slint_core::graphics::RequestedGraphicsAPI;
use i_slint_core::partial_renderer::DirtyRegion;
use i_slint_core::renderer::DrawOutcome;
use i_slint_renderer_skia::skia_safe::{self, gpu};
use i_slint_renderer_skia::{SkiaRenderer, SkiaSharedContext, Surface};
use slint::platform::PlatformError;
use slint::platform::femtovg_renderer::OpenGLInterface;
use slint::{PhysicalSize, Window};

use crate::platform::GlContext;

/// Whether Skia was asked for, and if so whether only what changed is drawn.
pub fn asked() -> Option<bool> {
    match std::env::var("MEDIABOX_TV_RENDERER").as_deref() {
        Ok("skia") => Some(false),
        Ok("skia-partial") => Some(true),
        _ => None,
    }
}

pub fn renderer(gl: GlContext, partial: bool) -> Result<SkiaRenderer, PlatformError> {
    let surface = KmsSurface::new(gl, partial)?;
    eprintln!("mediabox-tv.skia on the platform's context, partial={partial}");
    Ok(SkiaRenderer::new_with_surface(&SkiaSharedContext::default(), Box::new(surface)))
}

struct KmsSurface {
    partial: bool,
    /// Before the context it was made with.
    drawn: RefCell<Drawn>,
    gl: GlContext,
}

struct Drawn {
    surface: skia_safe::Surface,
    size: PhysicalSize,
    gr: gpu::DirectContext,
    log: Log,
}

/// What was drawn, for the measurement: said once a second while it draws.
#[derive(Default)]
struct Log {
    frames: u32,
    ages: [u32; 5],
    area: f64,
    full: u32,
    since: Option<std::time::Instant>,
}

fn told(e: impl std::fmt::Display) -> PlatformError {
    e.to_string().into()
}

fn target(gr: &mut gpu::DirectContext, size: PhysicalSize, stencil: u8) -> Result<skia_safe::Surface, PlatformError> {
    let info = gpu::gl::FramebufferInfo { fboid: 0, format: gpu::gl::Format::RGBA8.into(), ..Default::default() };
    let target = gpu::backend_render_targets::make_gl((size.width as i32, size.height as i32), 0, stencil.into(), info);
    // Skia's own answer to a target without samples: multisampled only where
    // a path needs it, in the tile. MEDIABOX_TV_SKIA_DMSAA asks.
    let dmsaa = std::env::var_os("MEDIABOX_TV_SKIA_DMSAA").map(|_| {
        skia_safe::SurfaceProps::new(skia_safe::SurfacePropsFlags::DYNAMIC_MSAA, skia_safe::PixelGeometry::Unknown)
    });
    gpu::surfaces::wrap_backend_render_target(
        gr,
        &target,
        gpu::SurfaceOrigin::BottomLeft,
        skia_safe::ColorType::RGBA8888,
        None,
        dmsaa.as_ref(),
    )
    .ok_or_else(|| told("Skia could not wrap the window's framebuffer"))
}

impl KmsSurface {
    fn new(gl: GlContext, partial: bool) -> Result<Self, PlatformError> {
        gl.ensure_current().map_err(told)?;
        let interface = gpu::gl::Interface::new_load_with_cstr(|name| gl.get_proc_address(name))
            .ok_or_else(|| told("Skia found no GL interface"))?;
        let mut gr =
            gpu::direct_contexts::make_gl(interface, None).ok_or_else(|| told("Skia made no GPU context"))?;
        let size = gl.size();
        let surface = target(&mut gr, size, gl.stencil())?;
        Ok(Self { partial, drawn: RefCell::new(Drawn { surface, size, gr, log: Log::default() }), gl })
    }
}

impl Surface for KmsSurface {
    fn new(
        _: &SkiaSharedContext,
        _: Arc<dyn raw_window_handle::HasWindowHandle + Sync + Send>,
        _: Arc<dyn raw_window_handle::HasDisplayHandle + Sync + Send>,
        _: PhysicalSize,
        _: Option<RequestedGraphicsAPI>,
    ) -> Result<Self, PlatformError> {
        Err(told("made from the platform's context only"))
    }

    fn name(&self) -> &'static str {
        "opengl (mediabox split-kms)"
    }

    fn with_graphics_api(&self, callback: &mut dyn FnMut(GraphicsAPI<'_>)) {
        callback(GraphicsAPI::NativeOpenGL { get_proc_address: &|name| self.gl.get_proc_address(name) });
    }

    fn with_active_surface(&self, callback: &mut dyn FnMut()) -> Result<(), PlatformError> {
        self.gl.ensure_current().map_err(told)?;
        callback();
        Ok(())
    }

    fn render(
        &self,
        _: &Window,
        size: PhysicalSize,
        callback: &dyn Fn(&skia_safe::Canvas, Option<&mut gpu::DirectContext>, u8) -> Option<DirtyRegion>,
        before_present: &RefCell<Option<Box<dyn FnMut()>>>,
    ) -> Result<DrawOutcome, PlatformError> {
        // A mode of another size: the same context, over a new surface.
        self.gl.ensure_current().map_err(told)?;
        self.gl.wait_for_buffer().map_err(told)?;
        let mut drawn = self.drawn.borrow_mut();
        let Drawn { surface, size: had, gr, log } = &mut *drawn;
        if *had != size {
            *surface = target(gr, size, self.gl.stencil())?;
            *had = size;
        }
        let age = self.gl.buffer_age();
        let canvas = surface.canvas();
        canvas.save();
        let region = callback(canvas, Some(gr), u8::try_from(age).unwrap_or_default());
        canvas.restore();
        // Slint flushes into GL's queue; this makes sure of it before the swap.
        gr.flush_and_submit();
        if let Some(callback) = before_present.borrow_mut().as_mut() {
            callback();
        }
        log.add(age, region);
        drop(drawn);
        self.gl.swap_buffers().map_err(told)?;
        Ok(DrawOutcome::Success)
    }

    fn resize_event(&self, _: PhysicalSize) -> Result<(), PlatformError> {
        Ok(())
    }

    fn bits_per_pixel(&self) -> Result<u8, PlatformError> {
        Ok(32)
    }

    fn use_partial_rendering(&self) -> bool {
        self.partial
    }
}

impl Log {
    fn add(&mut self, age: u32, region: Option<DirtyRegion>) {
        self.frames += 1;
        self.ages[(age as usize).min(4)] += 1;
        if let Some(region) = region {
            let boxes = region.iter().map(|b| f64::from(b.area())).sum::<f64>();
            self.area += boxes;
            if self.frames % 48 == 1 {
                eprintln!("mediabox-tv.skia region age={age} {region:?}");
            }
            self.full += u32::from(region.bounding_rect().area() >= 1280.0 * 720.0 * 0.95);
        }
        let now = std::time::Instant::now();
        let since = *self.since.get_or_insert(now);
        if now.duration_since(since).as_secs() >= 1 {
            eprintln!(
                "mediabox-tv.skia frames={} age0..4={:?} dirty_px_mean={:.0} full={}",
                self.frames,
                self.ages,
                self.area / f64::from(self.frames),
                self.full
            );
            *self = Self { since: Some(now), ..Self::default() };
        }
    }
}

impl Drop for KmsSurface {
    fn drop(&mut self) {
        if self.gl.ensure_current().is_err() {
            self.drawn.borrow_mut().gr.abandon();
        }
    }
}
