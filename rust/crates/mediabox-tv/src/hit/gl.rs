//! Quiet OS as the appliance draws it: FemtoVG on GLES, off screen.
//!
//! The software renderer the other tests draw with has no transform-scale
//! and keeps no layer, so it can say nothing of a ground drawn small and
//! scaled back, nor of a drawing kept in a texture (ui/quiet/materials.slint).
//! Here the product's own renderer draws into an EGL pbuffer, and what it
//! asks of GL on the way is counted: the textures it holds and the passes it
//! makes into one.
//!
//! The frame is the GL's, so nothing is compared with a picture from another
//! machine. What is asked is that a kept drawing is the drawing: a panel that
//! arrived at a state through changes shows what one drawn afresh shows.
//!
//! A machine with no EGL device that gives a GLES context has nothing to
//! draw with; the tests say so and pass. MEDIABOX_TV_GL=require makes that a
//! failure.

use super::*;
use std::cell::Cell;
use std::ffi::{CStr, CString, c_void};
use std::num::NonZeroU32;

use glutin::api::egl::{device::Device, display::Display};
use glutin::config::{ConfigSurfaceTypes, ConfigTemplateBuilder};
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::prelude::*;
use glutin::surface::{PbufferSurface, SurfaceAttributesBuilder};
use slint::platform::femtovg_renderer::{FemtoVGRenderer, OpenGLInterface};

/// The pbuffer: the largest panel the appliance drives.
const MOST: (u32, u32) = (3840, 2160);

/// The GPU's own scenes among Slint's (src/spatial).
mod spatial;

// ------------------------------------------------------- what GL was asked

type Gen = unsafe extern "C" fn(i32, *mut u32);
type Delete = unsafe extern "C" fn(i32, *const u32);
type Bind = unsafe extern "C" fn(u32, u32);
type Read = unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut u8);
type Clear = unsafe extern "C" fn(u32);
type Image = unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);

thread_local! {
    static REAL: Cell<[*const c_void; 5]> = const { Cell::new([std::ptr::null(); 5]) };
    /// Textures alive.
    static TEXTURES: Cell<i64> = const { Cell::new(0) };
    /// Passes into a texture rather than the panel.
    static PASSES: Cell<u32> = const { Cell::new(0) };
    /// Times FemtoVG cleared, which is how it begins to issue a frame.
    static CLEARS: Cell<u32> = const { Cell::new(0) };
    /// Frames presented, and frames drawn that were not.
    static PRESENTED: Cell<u32> = const { Cell::new(0) };
    static WITHHELD: Cell<u32> = const { Cell::new(0) };
    /// The longest side of any texture made.
    static LONGEST: Cell<i32> = const { Cell::new(0) };
}

unsafe extern "C" fn gen_textures(count: i32, out: *mut u32) {
    TEXTURES.set(TEXTURES.get() + i64::from(count));
    let real: Gen = unsafe { std::mem::transmute(REAL.get()[0]) };
    unsafe { real(count, out) }
}

unsafe extern "C" fn delete_textures(count: i32, which: *const u32) {
    TEXTURES.set(TEXTURES.get() - i64::from(count));
    let real: Delete = unsafe { std::mem::transmute(REAL.get()[1]) };
    unsafe { real(count, which) }
}

unsafe extern "C" fn bind_framebuffer(target: u32, which: u32) {
    if which != 0 {
        PASSES.set(PASSES.get() + 1);
    }
    let real: Bind = unsafe { std::mem::transmute(REAL.get()[2]) };
    unsafe { real(target, which) }
}

unsafe extern "C" fn clear(what: u32) {
    CLEARS.set(CLEARS.get() + 1);
    let real: Clear = unsafe { std::mem::transmute(REAL.get()[4]) };
    unsafe { real(what) }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn tex_image(t: u32, l: i32, i: i32, w: i32, h: i32, b: i32, f: u32, k: u32, p: *const c_void) {
    LONGEST.set(LONGEST.get().max(w).max(h));
    let real: Image = unsafe { std::mem::transmute(REAL.get()[3]) };
    unsafe { real(t, l, i, w, h, b, f, k, p) }
}

struct Pbuffer {
    display: Display,
    context: glutin::api::egl::context::PossiblyCurrentContext,
    surface: glutin::api::egl::surface::Surface<PbufferSurface>,
}

impl Pbuffer {
    fn new() -> Result<Self, String> {
        let mut last = "no EGL device".to_string();
        for device in Device::query_devices().map_err(|e| e.to_string())? {
            match Self::on(&device) {
                Ok(made) => return Ok(made),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    fn on(device: &Device) -> Result<Self, String> {
        let told = |e: glutin::error::Error| e.to_string();
        // SAFETY: a device EGL itself listed, no native display.
        let display = unsafe { Display::with_device(device, None) }.map_err(told)?;
        let template = ConfigTemplateBuilder::new()
            .with_surface_type(ConfigSurfaceTypes::PBUFFER)
            .with_alpha_size(8)
            .with_stencil_size(8)
            .build();
        // SAFETY: no native window is named.
        let config = unsafe { display.find_configs(template) }
            .map_err(told)?
            .min_by_key(|config| config.num_samples())
            .ok_or("no pbuffer config")?;
        let attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);
        // SAFETY: the same.
        let context = unsafe { display.create_context(&config, &attributes) }.map_err(told)?;
        let size = SurfaceAttributesBuilder::<PbufferSurface>::new()
            .build(NonZeroU32::new(MOST.0).unwrap(), NonZeroU32::new(MOST.1).unwrap());
        // SAFETY: the same.
        let surface = unsafe { display.create_pbuffer_surface(&config, &size) }.map_err(told)?;
        let context = context.make_current(&surface).map_err(told)?;
        crate::spatial::gl::context_made();
        Ok(Self { display, context, surface })
    }
}

// SAFETY: the context is made current where it is asked for, and the
// functions are its own display's.
unsafe impl OpenGLInterface for Pbuffer {
    fn ensure_current(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.context.make_current(&self.surface)?)
    }

    fn swap_buffers(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let count = if crate::spatial::withheld() { &WITHHELD } else { &PRESENTED };
        count.set(count.get() + 1);
        Ok(())
    }

    fn resize(&self, _: NonZeroU32, _: NonZeroU32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    fn get_proc_address(&self, name: &CStr) -> *const c_void {
        let real = self.display.get_proc_address(name);
        let counted = |at: usize, instead: *const c_void| {
            let mut all = REAL.get();
            all[at] = real;
            REAL.set(all);
            instead
        };
        match name.to_bytes() {
            b"glGenTextures" => counted(0, gen_textures as *const c_void),
            b"glDeleteTextures" => counted(1, delete_textures as *const c_void),
            b"glBindFramebuffer" => counted(2, bind_framebuffer as *const c_void),
            b"glClear" => counted(4, clear as *const c_void),
            b"glTexImage2D" => counted(3, tex_image as *const c_void),
            _ => real,
        }
    }
}

struct GlWindow {
    window: slint::Window,
    renderer: FemtoVGRenderer,
    size: Cell<slint::PhysicalSize>,
    redraw: Cell<bool>,
    read: Read,
}

impl WindowAdapter for GlWindow {
    fn window(&self) -> &slint::Window {
        &self.window
    }
    fn size(&self) -> slint::PhysicalSize {
        self.size.get()
    }
    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }
    fn request_redraw(&self) {
        self.redraw.set(true);
    }
}

struct Screen(Rc<GlWindow>);

impl Platform for Screen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

/// The application on the product's renderer.
pub(super) struct Drawn {
    // Before the window whose renderer its items' textures are of.
    pub(super) ui: MediaBoxWindow,
    window: Rc<GlWindow>,
    _scratch: Scratch,
}

/// A picture, and how it differs from another.
pub(super) struct Picture {
    size: (u32, u32),
    pixels: Vec<u8>,
}

impl Picture {
    /// How many pixels are further from `other` than `by` levels of 255.
    fn unlike(&self, other: &Picture, by: u8) -> usize {
        assert_eq!(self.size, other.size);
        self.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(other.pixels.as_chunks::<4>().0)
            .filter(|(a, b)| (0..3).any(|i| a[i].abs_diff(b[i]) > by))
            .count()
    }

    fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * self.size.0 + x) * 4) as usize;
        [self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]]
    }

    /// Kept beside MEDIABOX_TV_SHOTS's other pictures, where one asks.
    fn keep(&self, name: &str) {
        if let Some(out) = std::env::var_os("MEDIABOX_TV_SHOTS").map(std::path::PathBuf::from) {
            std::fs::create_dir_all(&out).unwrap();
            self.save(&out.join(format!("gl-{name}.png")));
        }
    }

    pub(super) fn save(&self, path: &std::path::Path) {
        // Opaque, as the panel is, whatever is left in the fourth channel.
        let rgb = self.pixels.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        image::RgbImage::from_raw(self.size.0, self.size.1, rgb).unwrap().save(path).unwrap();
    }
}

impl Drop for Drawn {
    fn drop(&mut self) {
        crate::spatial::stop();
    }
}

impl Drawn {
    /// None where this machine has no GLES to draw with.
    pub(super) fn open() -> Option<Self> {
        let pbuffer = match Pbuffer::new() {
            Ok(made) => made,
            Err(error) => {
                assert!(std::env::var("MEDIABOX_TV_GL").as_deref() != Ok("require"), "no GLES: {error}");
                eprintln!("no GLES on this machine, nothing drawn: {error}");
                return None;
            }
        };
        let name = CString::new("glReadPixels").unwrap();
        let read = pbuffer.display.get_proc_address(&name);
        assert!(!read.is_null());
        let renderer = FemtoVGRenderer::new(pbuffer).expect("FemtoVG");
        let window = Rc::new_cyclic(|weak: &std::rc::Weak<GlWindow>| GlWindow {
            window: slint::Window::new(weak.clone()),
            renderer,
            size: Cell::new(slint::PhysicalSize::new(W, H)),
            redraw: Cell::new(true),
            // SAFETY: glReadPixels, by its own name.
            read: unsafe { std::mem::transmute::<*const c_void, Read>(read) },
        });
        slint::platform::set_platform(Box::new(Screen(window.clone()))).expect("one platform");
        let (ui, scratch) = driven_in_place();
        let drawn = Self { ui, window, _scratch: scratch };
        drawn.panel(W, H, 1.0);
        Some(drawn)
    }

    /// A panel of this mode, as the platform tells Slint of one
    /// (`SplitWindow::sync_size`): the scale first.
    pub(super) fn panel(&self, width: u32, height: u32, scale: f32) {
        let window = &self.window.window;
        let size = slint::PhysicalSize::new(width, height);
        self.window.size.set(size);
        if window.scale_factor() != scale {
            window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        }
        window.dispatch_event(WindowEvent::Resized { size: size.to_logical(scale) });
        self.frame();
        self.frame();
    }

    /// One pass of the platform's loop, drawn.
    fn frame(&self) {
        step(&self.ui);
        self.draw();
    }

    /// A frame of the moment Slint's clock is at.
    fn draw(&self) {
        crate::spatial::frame(std::time::Duration::from_nanos(16_666_667), || {
            self.window.redraw.set(false);
            self.window.renderer.render()
        })
        .expect("a frame");
    }

    /// Drawn as it goes for `ms`, then at rest.
    pub(super) fn rest(&self, ms: u64) {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < until {
            self.frame();
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        self.frame();
    }

    pub(super) fn picture(&self) -> Picture {
        self.frame();
        self.read()
    }

    fn read(&self) -> Picture {
        let size = self.window.size.get();
        let mut pixels = vec![0u8; (size.width * size.height * 4) as usize];
        // SAFETY: the context is current (it just drew), and the buffer is
        // the size asked for.
        unsafe {
            (self.window.read)(0, 0, size.width as i32, size.height as i32, 0x1908, 0x1401, pixels.as_mut_ptr());
        }
        // GL's first row is the bottom one.
        let row = (size.width * 4) as usize;
        let pixels = pixels.chunks_exact(row).rev().flatten().copied().collect();
        Picture { size: (size.width, size.height), pixels }
    }

    /// The same panel with nothing kept: every layer drawn over again.
    /// (A scale factor that changed is when the renderer drops them.)
    fn afresh(&self) -> Picture {
        let window = &self.window.window;
        let scale = window.scale_factor();
        let size = self.window.size.get();
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale + 1.0 });
        self.frame();
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        window.dispatch_event(WindowEvent::Resized { size: size.to_logical(scale) });
        self.rest(1000);
        self.picture()
    }

    /// What is on the panel is what would be drawn afresh.
    fn is_what_it_is(&self, what: &str) {
        let kept = self.picture();
        let fresh = self.afresh();
        let unlike = kept.unlike(&fresh, 2);
        if unlike > 0 {
            kept.keep(&format!("{what}-kept"));
            fresh.keep(&format!("{what}-fresh"));
        }
        assert_eq!(unlike, 0, "{what}: pixels that are not what they would be drawn afresh");
    }

    fn vitals(&self, eth: bool, wifi: bool, bars: i32) {
        self.ui.set_vitals(crate::VitalsModel {
            time: "13:34".into(),
            storage_free: "51.2 GB".into(),
            temperature: "56 °C".into(),
            eth_up: eth,
            wifi_up: wifi,
            wifi_bars: bars,
            wifi_ssid: "Ağ".into(),
            ..Default::default()
        });
    }

    fn home(&self, apps: &Rc<VecModel<crate::AppTile>>, row: i32) {
        wear(crate::layout::Layout::Quiet);
        // From the other face, once its scenes are made (`spatial::ready_or`):
        // the launcher below is put in after the application has painted.
        for _ in 0..3 {
            if self.ui.get_layout() == "quiet" {
                break;
            }
            self.frame();
        }
        assert_eq!(self.ui.get_layout(), "quiet");
        app(|app| app.go_home());
        self.ui.set_apps(ModelRc::from(apps.clone()));
        self.ui.set_focus_col(row);
    }

    fn settings(&self, group: &str) {
        app(|app| {
            app.open(crate::route::Route::Settings);
            while app.settings.back() {}
            app.settings.compose(Some(&family_status()), None, None);
            let at = app.settings.groups.iter().position(|g| g.title == group).expect("a category");
            app.settings.point_section(at);
            app.paint();
        });
    }
}

/// The panels the appliance drives, each as `platform::scale_factor` scales
/// it, and one that neither its scale nor the ground's grid divides.
const PANELS: &[(u32, u32, f32)] = &[(1920, 1080, 1.0), (2560, 1440, 2.0), (3840, 2160, 2.0), (1366, 768, 1.0)];

/// What is kept is drawn again when something in it changes, and only then
/// looks any different: through a change of application, of the launcher, of
/// the network, of the category and of the panel's mode, the panel is what it
/// would be drawn afresh.
#[test]
fn what_is_kept_is_what_is_drawn() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(6)));
    for &(w, h, scale) in PANELS {
        drawn.panel(w, h, scale);
        drawn.vitals(false, true, 3);
        drawn.home(&apps, 0);
        drawn.rest(1200);
        drawn.is_what_it_is(&format!("{h}-home"));
        for row in [1, 3, 2] {
            drawn.ui.set_focus_col(row);
            drawn.rest(1200);
        }
        drawn.is_what_it_is(&format!("{h}-home-moved"));
        // The network, both ways and gone: the glyph, the word and the mark.
        for (eth, wifi, bars) in [(true, false, 0), (false, true, 1), (false, false, 0), (true, true, 4)] {
            drawn.vitals(eth, wifi, bars);
            drawn.rest(300);
            drawn.is_what_it_is(&format!("{h}-net-{eth}-{wifi}-{bars}"));
        }
        // The launcher under the focus: one renamed, one not ready, one gone.
        let mut tile = slint::Model::row_data(&*apps, 2).unwrap();
        tile.name = "Başka Adlı Bir Uygulama".into();
        tile.about = "Ne olduğu".into();
        slint::Model::set_row_data(&*apps, 2, tile);
        let mut tile = slint::Model::row_data(&*apps, 4).unwrap();
        tile.ready = false;
        slint::Model::set_row_data(&*apps, 4, tile);
        apps.remove(5);
        drawn.rest(1200);
        drawn.is_what_it_is(&format!("{h}-launcher"));
        apps.set_vec(launcher(6));

        for group in ["Sistem", "Bağlantılar", "Sistem"] {
            drawn.settings(group);
            drawn.rest(1200);
        }
        drawn.is_what_it_is(&format!("{h}-settings"));
    }
}

/// The settings' emblem is the size its place is, whatever became of the
/// panel while it stood there: the mode changed (a trial of another
/// resolution, from the editor whose emblem is the small one), and reduced
/// motion, where it goes from one size to the other with nothing between.
#[test]
fn the_emblem_is_the_size_of_its_place() {
    let Some(drawn) = Drawn::open() else { return };
    // The emblem's part of the panel, and nothing of what is under it.
    let emblem = |picture: &Picture, small: bool| {
        let u = picture.size.1 as f32 / 1080.0;
        let (x, y, side) = if small { (1724.0, 116.0, 96.0) } else { (1592.0, 196.0, 228.0) };
        let (x, y, side) = ((x * u) as u32, (y * u) as u32, (side * u) as u32);
        let mut pixels = Vec::new();
        for row in y..(y + side).min(picture.size.1) {
            for col in x..(x + side).min(picture.size.0) {
                pixels.extend(picture.at(col, row));
                pixels.push(255);
            }
        }
        Picture { size: (side, pixels.len() as u32 / 4 / side), pixels }
    };
    let panels = [(1920, 1080, 1.0), (2560, 1440, 2.0), (3840, 2160, 2.0), (1920, 1080, 1.0)];
    for small in [false, true] {
        let show = || {
            if small {
                family_show(&drawn.ui, "output");
            } else {
                drawn.settings("Sistem");
            }
        };
        wear(crate::layout::Layout::Quiet);
        app(|app| app.go_home());
        drawn.rest(50);
        show();
        drawn.rest(1200);
        for (w, h, scale) in panels {
            // The mode, under the screen.
            drawn.panel(w, h, scale);
            drawn.rest(800);
            let stood = emblem(&drawn.picture(), small);
            // The screen, under the mode.
            app(|app| app.go_home());
            drawn.rest(100);
            show();
            drawn.rest(1200);
            let opened = emblem(&drawn.picture(), small);
            stood.keep(&format!("emblem-{h}-{small}-stood"));
            opened.keep(&format!("emblem-{h}-{small}-opened"));
            assert_eq!(stood.unlike(&opened, 6), 0, "{w}x{h}, small {small}");
        }
    }
    // No motion: the editor opened and closed with nothing between.
    drawn.panel(1920, 1080, 1.0);
    drawn.ui.global::<crate::QuietMotion>().set_reduced(true);
    let shown = |name: &str, small: bool| {
        if small {
            family_show(&drawn.ui, name);
        } else {
            drawn.settings("Sistem");
        }
        drawn.rest(60);
        emblem(&drawn.picture(), small)
    };
    for small in [false, true, false] {
        let stood = shown("output", small);
        app(|app| app.go_home());
        drawn.rest(60);
        let opened = shown("output", small);
        assert_eq!(stood.unlike(&opened, 6), 0, "reduced motion, small {small}");
    }
}

/// A panel whose mode changed under a screen shows what the screen shows
/// opened in that mode, and one whose face changed under it likewise: no
/// place, size or texture of the old one is left anywhere in the family.
#[test]
fn a_mode_changed_under_a_screen_leaves_nothing_of_the_old_one() {
    let Some(drawn) = Drawn::open() else { return };
    // Everything lands at once: it is where things are that is asked.
    drawn.ui.global::<crate::QuietMotion>().set_reduced(true);
    let show = |name: &str| {
        app(|app| app.go_home());
        drawn.rest(40);
        family_show(&drawn.ui, name);
        drawn.vitals(true, false, 0);
        drawn.rest(350);
    };
    let mut wrong = Vec::new();
    let mut same = |what: String, stood: &Picture, opened: &Picture| {
        let unlike = stood.unlike(opened, 12);
        if unlike > 0 {
            stood.keep(&format!("{what}-stood"));
            opened.keep(&format!("{what}-opened"));
            wrong.push((what, unlike));
        }
    };
    for name in ["output", "output-advanced", "audio", "ethernet-static", "cooling", "cec", "system", "wifi", "account", "diagnostics-0"] {
        wear(crate::layout::Layout::Classic);
        show(name);
        wear(crate::layout::Layout::Quiet);
        drawn.rest(350);
        let stood = drawn.picture();
        show(name);
        same(format!("face-{name}"), &stood, &drawn.picture());
        for (w, h, scale) in [(1920, 1080, 1.0), (2560, 1440, 2.0), (3840, 2160, 2.0), (1920, 1080, 1.0)] {
            drawn.panel(w, h, scale);
            drawn.rest(350);
            let stood = drawn.picture();
            show(name);
            same(format!("mode-{h}-{name}"), &stood, &drawn.picture());
        }
    }
    assert!(wrong.is_empty(), "{wrong:?}");
}

/// The light's way to another application is one colour at every moment,
/// however often it is turned round on the way: no frame of it is darker,
/// lighter or further from where it goes than the ones beside it allow, and
/// it ends where it was going.
#[test]
fn the_light_never_jumps() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(6)));
    drawn.home(&apps, 0);
    drawn.vitals(true, false, 0);
    drawn.rest(1200);
    // A place with the light on it, and nothing else ever.
    let light = || drawn.picture().at(1900, 1000);
    let mut last = light();
    let mut seen = last;
    for row in [1, 2, 1, 0, 3, 4, 3, 2, 5, 0, 1] {
        drawn.ui.set_focus_col(row);
        for _ in 0..6 {
            let at = std::time::Instant::now();
            std::thread::sleep(std::time::Duration::from_millis(15));
            let now = light();
            // The whole way is some thirty levels in 900 ms, quickest first.
            let may = 3 + at.elapsed().as_millis() as u8 / 8;
            for i in 0..3 {
                assert!(now[i].abs_diff(last[i]) <= may, "row {row}: {last:?} then {now:?}");
                seen[i] = seen[i].max(now[i]);
            }
            last = now;
        }
    }
    assert_ne!(seen, [last[0], last[1], last[2]], "the light moved");
    drawn.rest(1200);
    let arrived = light();
    let fresh = drawn.afresh().at(1900, 1000);
    for i in 0..3 {
        assert!(arrived[i].abs_diff(fresh[i]) <= 1, "{arrived:?} is not {fresh:?}");
    }
    // Reduced motion, half way: there, now.
    drawn.ui.set_focus_col(4);
    drawn.rest(150);
    drawn.ui.global::<crate::QuietMotion>().set_reduced(true);
    drawn.rest(30);
    let there = light();
    drawn.rest(1200);
    let later = light();
    for i in 0..3 {
        assert!(there[i].abs_diff(later[i]) <= 1, "{there:?} then {later:?}");
    }
}

/// What is kept is not drawn again by what moves past it: a scene's whole
/// way to the next makes passes into a texture for the first frames only
/// (the list's focus and the new words), and a panel at rest none. `Kept`
/// is Slint's opacity layer, and this is what says it still is one.
#[test]
fn a_kept_drawing_is_not_drawn_for_every_frame() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(6)));
    drawn.vitals(true, false, 0);
    drawn.home(&apps, 0);
    drawn.rest(1200);
    let passes = || {
        let was = PASSES.get();
        drawn.frame();
        PASSES.get() - was
    };
    assert_eq!(passes(), 0, "home, at rest");
    drawn.ui.set_focus_col(1);
    // The list, and what `Kept` holds: the light it goes to and the new name
    // (the list alone is four).
    let first = passes();
    assert!(first >= 5, "{first} passes: what is to be kept is not a layer");
    let settled = std::time::Instant::now() + std::time::Duration::from_millis(320);
    while std::time::Instant::now() < settled {
        drawn.frame();
    }
    for n in 0..20 {
        assert!(drawn.ui.window().has_active_animations(), "still on its way");
        assert_eq!(passes(), 0, "home, frame {n} of the rest of the way");
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    drawn.rest(1200);
    drawn.settings("Sistem");
    drawn.rest(1200);
    assert_eq!(passes(), 0, "settings, at rest");
    drawn.settings("Bağlantılar");
    // The heading and the rows are in after 320 ms, at this pace 1.6 s; the
    // emblem's notch takes 560, at this pace 2.8.
    drawn.ui.global::<crate::QuietMotion>().set_pace(5.0);
    drawn.frame();
    std::thread::sleep(std::time::Duration::from_millis(1800));
    drawn.frame();
    drawn.frame();
    for n in 0..10 {
        assert!(drawn.ui.window().has_active_animations(), "the emblem still turns");
        assert_eq!(passes(), 0, "settings, frame {n} of the notch");
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

/// Going round and round the family holds no more of the GPU than going
/// round it once did.
#[test]
fn going_round_and_round_holds_no_more_textures() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(8)));
    drawn.ui.global::<crate::QuietMotion>().set_pace(0.05);
    let round = || {
        drawn.home(&apps, 0);
        for row in [1, 2, 3, 4, 5, 6, 7, 3] {
            drawn.ui.set_focus_col(row);
            drawn.rest(25);
        }
        for group in ["Sistem", "Bağlantılar", "Sistem"] {
            drawn.settings(group);
            drawn.rest(25);
        }
        for name in ["output", "cooling", "audio", "wifi", "bluetooth", "account", "diagnostics-0"] {
            family_show(&drawn.ui, name);
            drawn.rest(25);
        }
        // The other face, and back; a reset model, which builds every row.
        wear(crate::layout::Layout::Classic);
        drawn.rest(25);
        apps.set_vec(launcher(8));
        drawn.home(&apps, 3);
        drawn.rest(120);
        TEXTURES.get()
    };
    round();
    let once = round();
    for _ in 0..6 {
        round();
    }
    let often = round();
    assert!(often <= once, "{once} textures after two rounds, {often} after nine");
}

/// A launcher however long is drawn, scrolled and pointed at, and no
/// texture grows with it: the list's is of every row, not of those that show.
#[test]
fn a_long_launcher_is_drawn_whole_and_its_texture_is_bounded() {
    let Some(drawn) = Drawn::open() else { return };
    for &(w, h, scale) in PANELS {
        drawn.panel(w, h, scale);
        drawn.vitals(true, false, 0);
        for count in [12, 60, 400] {
            let apps = Rc::new(VecModel::from(launcher(count)));
            LONGEST.set(0);
            drawn.home(&apps, 0);
            drawn.rest(300);
            drawn.ui.set_focus_col(count as i32 - 1);
            drawn.rest(1200);
            assert!(LONGEST.get() <= 4096, "{count} applications at {h}: a texture of {}", LONGEST.get());
            drawn.is_what_it_is(&format!("{h}-list-{count}"));
            // The last row's name is there: ink, on the column's lowest row.
            let picture = drawn.picture();
            let u = h as f32 / 1080.0;
            let inked = ((190.0 * u) as u32..(420.0 * u) as u32)
                .any(|x| ((785.0 * u) as u32..(845.0 * u) as u32).any(|y| picture.at(x, y)[0] > 180));
            assert!(inked, "{count} applications at {h}: no last row");
        }
    }
    // And what a pointer is over, at the list's end, is what is drawn there.
    drawn.panel(1920, 1080, 1.0);
    let apps = Rc::new(VecModel::from(launcher(60)));
    drawn.home(&apps, 59);
    drawn.rest(1200);
    let over: Rc<RefCell<Vec<i32>>> = Rc::default();
    let heard = over.clone();
    drawn.ui.global::<PointerHub>().on_hover(move |zone, index, on| {
        if on && zone == "quiet-home" {
            heard.borrow_mut().push(index);
        }
    });
    drawn.ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(300.0, 815.0) });
    assert_eq!(*over.borrow(), [59]);
}
