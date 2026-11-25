//! GPU-resident spatial scenes: geometry that stays on the GPU, under a
//! camera Slint animates. An experiment (docs/quietos-advanced-graphics.md),
//! built only with the `spatial` feature and inert without
//! MEDIABOX_TV_SPATIAL.
//!
//! How a frame goes. The scene's element in ui/quiet/spatial.slint binds an
//! image to a pure callback of its camera. Slint asks for that image while it
//! draws, with the product's GL context current and its own drawing not yet
//! submitted (FemtoVG queues a frame and issues it at the end), so the scene
//! is drawn into its texture there and then, in a pass of its own *before*
//! the frame's: no rendering notifier, no second context, no extra frame of
//! latency, and the frame's pass is not cut in two -- on a tiled GPU a cut
//! means storing and loading the whole panel again. The texture is what
//! Slint calls a borrowed one: it is shown, not copied.
//!
//! Nothing is drawn unless the camera, the place or the colours changed,
//! because then the binding does not run. A panel of another size only makes
//! the next texture larger; another GL context makes everything again.

mod advanced;
mod gl;
mod globe;
mod math;

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use glow::HasContext;
use slint::ComponentHandle;

use crate::{MediaBoxWindow, Spatial, SpatialCam, SpatialPlace};

/// What to draw, and how much of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    scene: Scene,
    /// Quads from pole to pole of the globe.
    globe: u32,
    rings: usize,
    objects: usize,
    /// Quads from pole to pole of a satellite; a ring's are as fine.
    detail: u32,
    layers: usize,
    particles: usize,
    /// A particle's size, relative to its own.
    dust: f32,
    samples: i32,
    /// Straight alpha by a pass of its own rather than in the tile it was
    /// drawn in. With samples, reading the pixel under the shader is done for
    /// every one of them: 2.4 million cycles of the GPU's a frame here, where
    /// a second pass is cheaper than it can be measured (2026-11-25).
    second: bool,
    /// The whole panel rather than the scene's place in it.
    full: bool,
    /// Moving for as long as it is shown, for a measurement: not of the
    /// camera's, and never at rest.
    run: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scene {
    /// The element is there and nothing is drawn: what a frame costs without.
    Nothing,
    Globe,
    Advanced,
}

impl Config {
    /// `globe`, `advanced`, `stress` or `nothing`, then `name=value`s.
    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split([' ', ',', '\n']).filter(|word| !word.is_empty());
        let base = Self {
            scene: Scene::Advanced,
            globe: 64,
            rings: 5,
            objects: 24,
            detail: 12,
            layers: 2,
            particles: 0,
            dust: 1.0,
            samples: 4,
            second: true,
            full: false,
            run: false,
        };
        let mut config = match words.next()? {
            "nothing" => Self { scene: Scene::Nothing, ..base },
            "globe" => Self { scene: Scene::Globe, samples: 1, second: false, ..base },
            "advanced" => base,
            "stress" => Self { objects: 200, particles: 5000, ..base },
            _ => return None,
        };
        for word in words {
            let (name, value) = word.split_once('=').unwrap_or((word, "1"));
            let n: f32 = value.parse().ok()?;
            // Bounded: this is a television, not a place to run out of memory.
            let whole = |most: usize| (n.max(0.0) as usize).min(most);
            match name {
                "globe" => config.globe = whole(1024).max(3) as u32,
                "rings" => config.rings = whole(512),
                "objects" => config.objects = whole(200_000),
                "detail" => config.detail = whole(256).max(3) as u32,
                "layers" => config.layers = whole(64),
                "particles" => config.particles = whole(2_000_000),
                "dust" => config.dust = n,
                "msaa" => config.samples = whole(16).max(1) as i32,
                "second" => config.second = n != 0.0,
                "full" => config.full = n != 0.0,
                "run" => config.run = n != 0.0,
                _ => return None,
            }
        }
        Some(config)
    }
}

/// `SpatialCam`, in the scenes' own terms.
struct Cam {
    d: f32,
    d_far: f32,
    d_near: f32,
    f_main: f32,
    f_far: f32,
    f_near: f32,
    tint: [f32; 4],
    ink: [f32; 4],
    glow: [f32; 4],
    x: f32,
    y: f32,
    size: f32,
    line: f32,
    fine: f32,
    depth: f32,
    parallax: f32,
    travel: f32,
    panel: (f32, f32),
    scale: f32,
    seconds: f32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Place {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Place {
    /// The whole pixels of the panel under `[left, top, right, bottom]`.
    fn covering(cam: &Cam, at: [f32; 4]) -> Self {
        let (left, top) = (at[0].max(0.0).floor() as i32, at[1].max(0.0).floor() as i32);
        let (right, bottom) = (at[2].min(cam.panel.0).ceil() as i32, at[3].min(cam.panel.1).ceil() as i32);
        if right <= left || bottom <= top {
            return Self::default();
        }
        Self { x: left, y: top, w: right - left, h: bottom - top }
    }
}

impl From<&SpatialCam> for Cam {
    fn from(c: &SpatialCam) -> Self {
        let colour = |c: slint::Color| {
            let c = c.to_argb_f32();
            [c.red, c.green, c.blue, c.alpha]
        };
        Self {
            d: c.d,
            d_far: c.d_far,
            d_near: c.d_near,
            f_main: c.f_main,
            f_far: c.f_far,
            f_near: c.f_near,
            tint: colour(c.tint),
            ink: colour(c.ink),
            glow: colour(c.glow),
            x: c.x,
            y: c.y,
            size: c.size,
            line: c.line,
            fine: c.fine,
            depth: c.depth,
            parallax: c.parallax,
            travel: c.travel,
            panel: (c.panel_w, c.panel_h),
            scale: c.scale.max(1.0),
            seconds: started().elapsed().as_secs_f32(),
        }
    }
}

fn started() -> Instant {
    static AT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *AT.get_or_init(Instant::now)
}

thread_local! {
    static STAGE: RefCell<Option<Stage>> = const { RefCell::new(None) };
    /// The platform is inside a frame: its context is current and Slint's own
    /// drawing has not been submitted.
    static DRAWING: Cell<bool> = const { Cell::new(false) };
    /// What was asked for outside one, drawn at the next.
    static OWED: RefCell<Option<(SpatialCam, SpatialPlace)>> = const { RefCell::new(None) };
    static UI: RefCell<Option<slint::Weak<MediaBoxWindow>>> = const { RefCell::new(None) };
}

/// Everything on the GPU, and of which context it is.
struct Stage {
    config: Config,
    context: usize,
    on: Option<OnGpu>,
    image: slint::Image,
    /// What `image` is a picture of. An element made anew (the launcher's
    /// model replaced under it) asks again for the picture there is.
    of: Option<(SpatialCam, SpatialPlace)>,
    report: Report,
    /// GL said no: nothing more is tried, and the element shows nothing.
    failed: bool,
}

struct OnGpu {
    gl: glow::Context,
    caps: gl::Caps,
    target: Option<gl::Target>,
    globe: Option<globe::Globe>,
    advanced: Option<advanced::Advanced>,
    finish: Finish,
}

#[derive(Default)]
struct Report {
    /// Ever, for the tests.
    all: u64,
    frames: u32,
    cpu: Duration,
    drawn: gl::Drawn,
    since: Option<Instant>,
}

/// Straight alpha out of premultiplied, in the pass that drew it: Slint takes
/// a borrowed texture as straight, and blending makes premultiplied.
struct Finish {
    /// In the tile, where the pixel under the shader can be read; and by a
    /// pass of its own.
    in_tile: Option<glow::NativeProgram>,
    program: glow::NativeProgram,
    vao: glow::NativeVertexArray,
    /// No framebuffer fetch here: the picture is drawn into this and put
    /// through a second pass.
    second: Option<gl::Target>,
}

const WHOLE: &str = r"#version 300 es
void main() {
    vec2 p = vec2(gl_VertexID == 1 ? 3.0 : -1.0, gl_VertexID == 2 ? 3.0 : -1.0);
    gl_Position = vec4(p, 0.0, 1.0);
}
";
const STRAIGHT_IN_TILE: &str = r"#version 300 es
#extension GL_EXT_shader_framebuffer_fetch : require
precision mediump float;
inout vec4 colour;
void main() { colour = colour.a > 0.0 ? vec4(colour.rgb / colour.a, colour.a) : vec4(0.0); }
";
const STRAIGHT: &str = r"#version 300 es
precision mediump float;
uniform sampler2D u_from;
out vec4 colour;
void main() {
    vec4 c = texelFetch(u_from, ivec2(gl_FragCoord.xy), 0);
    colour = c.a > 0.0 ? vec4(c.rgb / c.a, c.a) : vec4(0.0);
}
";

impl Finish {
    fn new(gl: &glow::Context, caps: &gl::Caps) -> Result<Self, gl::Error> {
        let program = gl::program(gl, WHOLE, STRAIGHT)?;
        let in_tile = caps.fetch.then(|| gl::program(gl, WHOLE, STRAIGHT_IN_TILE)).transpose()?;
        // SAFETY: the context is current on this thread.
        let vao = unsafe { gl.create_vertex_array()? };
        Ok(Self { in_tile, program, vao, second: None })
    }

    fn delete(&self, gl: &glow::Context) {
        // SAFETY: the context is the one they were made in.
        unsafe {
            gl.delete_program(self.program);
            if let Some(program) = self.in_tile {
                gl.delete_program(program);
            }
            gl.delete_vertex_array(self.vao);
        }
    }

    fn draw(&self, gl: &glow::Context, from: Option<&gl::Target>, drawn: &mut gl::Drawn) {
        // SAFETY: the same; the pass is begun.
        unsafe {
            gl.use_program(Some(if from.is_some() { self.program } else { self.in_tile.unwrap_or(self.program) }));
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.bind_vertex_array(Some(self.vao));
            if let Some(from) = from {
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(from.texture));
            }
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
        drawn.draws += 1;
        drawn.vertices += 3;
        drawn.triangles += 1;
    }
}

impl OnGpu {
    fn new() -> Result<Self, gl::Error> {
        // SAFETY: the loader is EGL's, the context current.
        let gl = unsafe {
            glow::Context::from_loader_function_cstr(|name| gl::load(name.to_str().expect("a GL function's name")))
        };
        let caps = gl::Caps::of(&gl);
        let finish = Finish::new(&gl, &caps)?;
        Ok(Self { gl, caps, target: None, globe: None, advanced: None, finish })
    }

    /// What the scene needs that is not on the GPU yet.
    fn prepare(&mut self, config: &Config) -> Result<(), gl::Error> {
        if config.scene != Scene::Globe && let Some(old) = self.globe.take() {
            old.delete(&self.gl);
        }
        if config.scene != Scene::Advanced && let Some(old) = self.advanced.take() {
            old.delete(&self.gl);
        }
        match config.scene {
            Scene::Nothing => {}
            Scene::Globe => {
                if self.globe.is_none() {
                    self.globe = Some(globe::Globe::new(&self.gl, self.caps.fetch)?);
                }
            }
            Scene::Advanced => {
                if !self.advanced.as_ref().is_some_and(|have| have.is(config)) {
                    if let Some(old) = self.advanced.take() {
                        old.delete(&self.gl);
                    }
                    self.advanced = Some(advanced::Advanced::new(&self.gl, config)?);
                }
            }
        }
        Ok(())
    }

    fn target(&mut self, config: &Config, cam: &Cam, place: &Place) -> Result<(), gl::Error> {
        let depth = config.scene == Scene::Advanced;
        let fits = |t: &gl::Target| t.size.0 >= place.w && t.size.1 >= place.h;
        if self.target.as_ref().is_some_and(fits) {
            return Ok(());
        }
        // Room for the scene at its largest, so a move does not make another:
        // passed by the camera it is half as large again as at rest.
        let room = |used: i32, panel: f32| {
            let most = if config.full { panel } else { (cam.size * 1.35).min(panel) };
            (used.max(most.ceil() as i32) + 15) & !15
        };
        let size = (room(place.w, cam.panel.0), room(place.h, cam.panel.1));
        self.drop_target();
        self.target = Some(gl::Target::new(&self.gl, &self.caps, size, config.samples, depth)?);
        if !self.caps.fetch || config.second {
            self.finish.second = Some(gl::Target::new(&self.gl, &self.caps, size, 1, false)?);
        }
        Ok(())
    }

    fn drop_target(&mut self) {
        for old in self.target.take().into_iter().chain(self.finish.second.take()) {
            old.delete(&self.gl);
        }
    }

    fn draw(&mut self, config: &Config, cam: &Cam, place: &Place) -> Result<(slint::Image, gl::Drawn), gl::Error> {
        self.prepare(config)?;
        let kept = gl::Kept::of(&self.gl);
        let made = self.target(config, cam, place);
        let mut drawn = gl::Drawn { pixels: place.w as u64 * place.h as u64, ..gl::Drawn::default() };
        drawn.samples = self.target.as_ref().map_or(0, |target| target.samples);
        if made.is_ok() && let Some(target) = &self.target {
            let used = (place.w, place.h);
            target.begin(&self.gl, used);
            drawn.passes += 1;
            match config.scene {
                Scene::Nothing => {}
                Scene::Globe => {
                    if let Some(globe) = &self.globe {
                        globe.draw(&self.gl, cam, place, &mut drawn);
                    }
                }
                Scene::Advanced => {
                    if let Some(advanced) = &self.advanced {
                        advanced.draw(&self.gl, cam, place, config, &mut drawn);
                    }
                }
            }
            // The globe blends straight in its own shader.
            if self.finish.second.is_none() && config.scene == Scene::Advanced {
                self.finish.draw(&self.gl, None, &mut drawn);
            }
            target.end(&self.gl);
            if let Some(second) = &self.finish.second {
                second.begin(&self.gl, used);
                drawn.passes += 1;
                self.finish.draw(&self.gl, Some(target), &mut drawn);
            }
        }
        kept.put_back(&self.gl);
        made?;
        let shown = self.finish.second.as_ref().or(self.target.as_ref()).ok_or("no target")?;
        // SAFETY: an RGBA texture of this context, the one Slint draws with,
        // that outlives every frame that names it: it is deleted only where
        // another takes its place, and the image with it.
        let image = unsafe {
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                shown.texture.0,
                (shown.size.0 as u32, shown.size.1 as u32).into(),
            )
        }
        .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
        .build();
        Ok((image, drawn))
    }
}

impl Stage {
    /// This context's, made again if there was another before it.
    fn gpu(&mut self) -> Result<&mut OnGpu, gl::Error> {
        let context = gl::current();
        if context == 0 {
            return Err("no GL context".into());
        }
        if self.context != context {
            // The old context's names mean nothing in this one: forgotten,
            // not deleted.
            self.on = None;
            self.context = context;
        }
        if self.on.is_none() {
            let at = Instant::now();
            let mut on = OnGpu::new()?;
            on.prepare(&self.config)?;
            eprintln!(
                "mediabox-tv.spatial on the GPU in {:.1} ms: {:?} fetch={}",
                at.elapsed().as_secs_f64() * 1e3,
                self.config,
                on.caps.fetch
            );
            self.on = Some(on);
        }
        self.on.as_mut().ok_or_else(|| "nothing on the GPU".into())
    }

    fn draw(&mut self, cam: &SpatialCam, place: &SpatialPlace) {
        if self.failed {
            return;
        }
        let asked = Some((cam.clone(), place.clone()));
        if self.of == asked && self.context == gl::current() {
            return;
        }
        let at = Instant::now();
        let config = self.config.clone();
        let cam = Cam::from(cam);
        let place = Place { x: place.x, y: place.y, w: place.w, h: place.h };
        match self.gpu().and_then(|on| on.draw(&config, &cam, &place)) {
            Ok((image, drawn)) => {
                self.image = image;
                self.of = asked;
                self.report.add(at.elapsed(), drawn, &self.config);
            }
            Err(error) => {
                eprintln!("mediabox-tv.spatial not drawn, and not tried again: {error}");
                self.failed = true;
                self.image = slint::Image::default();
            }
        }
    }

    fn change(&mut self, config: Config) {
        if let Some(on) = &mut self.on {
            let kept = gl::Kept::of(&on.gl);
            on.drop_target();
            kept.put_back(&on.gl);
        }
        eprintln!("mediabox-tv.spatial now {config:?}");
        self.config = config;
        self.of = None;
        self.report = Report::default();
        self.failed = false;
    }
}

impl Report {
    fn add(&mut self, cpu: Duration, drawn: gl::Drawn, config: &Config) {
        self.all += 1;
        self.frames += 1;
        self.cpu += cpu;
        self.drawn = drawn;
        let now = Instant::now();
        let since = *self.since.get_or_insert(now);
        if now.duration_since(since) >= Duration::from_secs(1) {
            let d = &self.drawn;
            eprintln!(
                "mediabox-tv.spatial {:?} frames={} cpu_us={:.1} passes={} draws={} objects={} instances={} vertices={} triangles={} pixels={} samples={}",
                config.scene,
                self.frames,
                self.cpu.as_secs_f64() * 1e6 / f64::from(self.frames),
                d.passes,
                d.draws,
                d.objects,
                d.instances,
                d.vertices,
                d.triangles,
                d.pixels,
                d.samples
            );
            *self = Self { all: self.all, since: Some(now), ..Self::default() };
        }
    }
}

/// Turns the experiment on for this window, if it was asked for.
pub fn attach(ui: &MediaBoxWindow) {
    let Some(config) = std::env::var("MEDIABOX_TV_SPATIAL").ok().and_then(|text| Config::parse(&text)) else {
        return;
    };
    start(ui, config);
    // A measurement's own knob: another scene without another start.
    if let Some(path) = std::env::var_os("MEDIABOX_TV_SPATIAL_CONTROL") {
        let timer = slint::Timer::default();
        let last = RefCell::new(String::new());
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if *last.borrow() != text {
                if let Some(config) = Config::parse(&text) {
                    let run = config.run;
                    STAGE.with(|stage| {
                        if let Some(stage) = stage.borrow_mut().as_mut() {
                            stage.change(config);
                        }
                    });
                    if run {
                        crate::platform::keep_drawing(Duration::from_secs(1));
                    }
                    again();
                }
                *last.borrow_mut() = text;
            }
        });
        // For as long as the process.
        std::mem::forget(timer);
    }
}

fn start(ui: &MediaBoxWindow, config: Config) {
    STAGE.with(|stage| {
        *stage.borrow_mut() = Some(Stage {
            config,
            context: 0,
            on: None,
            image: slint::Image::default(),
            of: None,
            report: Report::default(),
            failed: false,
        });
    });
    UI.with(|cell| *cell.borrow_mut() = Some(ui.as_weak()));
    let spatial = ui.global::<Spatial>();
    spatial.on_place(|cam| {
        let config = STAGE.with(|stage| stage.borrow().as_ref().map(|stage| stage.config.clone()));
        let cam = Cam::from(&cam);
        let place = match config {
            Some(Config { scene: Scene::Globe, .. }) => globe::place(&cam),
            Some(Config { scene: Scene::Advanced, full, .. }) if cam.f_main > 0.0 => advanced::place(&cam, full),
            _ => Place::default(),
        };
        SpatialPlace { x: place.x, y: place.y, w: place.w, h: place.h }
    });
    spatial.on_picture(|cam, place| {
        STAGE.with(|stage| {
            let Ok(mut stage) = stage.try_borrow_mut() else { return slint::Image::default() };
            let Some(stage) = stage.as_mut() else { return slint::Image::default() };
            if DRAWING.get() {
                stage.draw(&cam, &place);
            } else {
                OWED.with(|owed| *owed.borrow_mut() = Some((cam, place)));
            }
            stage.image.clone()
        })
    });
    spatial.set_on(true);
}

/// The element asks again, and is drawn.
fn again() {
    let ui = UI.with(|cell| cell.borrow().as_ref().and_then(slint::Weak::upgrade));
    if let Some(ui) = ui {
            let spatial = ui.global::<Spatial>();
        spatial.set_tick(spatial.get_tick().wrapping_add(1));
        ui.window().request_redraw();
    }
}

/// Everything given back, while its context is still there.
pub fn stop() {
    let Some(mut stage) = STAGE.with(|stage| stage.borrow_mut().take()) else { return };
    if let Some(mut on) = stage.on.take().filter(|_| stage.context != 0 && stage.context == gl::current()) {
        on.drop_target();
        on.finish.delete(&on.gl);
        if let Some(globe) = on.globe.take() {
            globe.delete(&on.gl);
        }
        if let Some(advanced) = on.advanced.take() {
            advanced.delete(&on.gl);
        }
    }
}

/// A frame: `draw` is Slint's renderer, the context is current.
pub fn frame<T>(draw: impl FnOnce() -> T) -> T {
    let run = STAGE.with(|stage| {
        let mut stage = stage.borrow_mut();
        let stage = stage.as_mut()?;
        // On the GPU before it is first wanted: the shaders are compiled on
        // the first frame of all, not on the first of a move.
        if !stage.failed
            && stage.on.is_none()
            && let Err(error) = stage.gpu().map(|_| ())
        {
            eprintln!("mediabox-tv.spatial not on the GPU: {error}");
            stage.failed = true;
        }
        if let Some((cam, place)) = OWED.with(|owed| owed.borrow_mut().take()) {
            stage.draw(&cam, &place);
        }
        Some(stage.config.run)
    });
    if run == Some(true) {
        crate::platform::keep_drawing(Duration::from_secs(1));
        again();
    }
    DRAWING.set(run.is_some());
    let answer = draw();
    DRAWING.set(false);
    answer
}

/// For the tests on the product's renderer (src/hit/gl.rs): the experiment on
/// this window, as if started with it.
#[cfg(test)]
pub fn start_with(ui: &MediaBoxWindow, text: &str) {
    start(ui, Config::parse(text).expect("a scene"));
}

/// How many times a scene has been drawn.
#[cfg(test)]
pub fn times_drawn() -> u64 {
    STAGE.with(|stage| stage.borrow().as_ref().map_or(0, |stage| stage.report.all))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scene_is_asked_for_in_words_and_bounded() {
        assert_eq!(Config::parse("globe").unwrap().scene, Scene::Globe);
        let config = Config::parse("stress,particles=20000 objects=64,msaa=1 full run\n").unwrap();
        assert_eq!((config.particles, config.objects, config.samples, config.full, config.run), (20000, 64, 1, true, true));
        assert_eq!(Config::parse("stress particles=1e12").unwrap().particles, 2_000_000);
        assert!(Config::parse("").is_none() && Config::parse("advanced nonsense=1").is_none());
    }
}
