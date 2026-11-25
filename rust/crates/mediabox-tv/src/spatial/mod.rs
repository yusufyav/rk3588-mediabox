//! GPU-resident spatial scenes: geometry that stays on the GPU, under a
//! camera Slint animates (docs/quietos-advanced-graphics.md).
//!
//! A scene is an element of the interface (ui/quiet/spatial.slint). Slint
//! keeps the camera, the timing and the place on the panel; the element's
//! image is a pure callback of those, and what answers it draws the scene
//! into a texture Slint then shows without copying it (a *borrowed* one).
//! Nothing draws unless MEDIABOX_TV_SPATIAL names a scene: the interface's
//! own scenes are Slint's, and cheaper drawn as they are.
//!
//! # A frame
//!
//! The platform draws a frame inside `frame`. Slint asks for the pictures
//! while it walks the interface, when FemtoVG has only queued its own
//! drawing, so a scene's pass is issued before the panel's and the panel's is
//! not cut in two. That is an order of cost, not of correctness: a picture is
//! drawn before the callback returns, so before anything that shows it can be
//! issued; no texture is drawn twice in a frame, so nothing queued can show
//! another's; and everything of GL's state is put back (`gl::Kept`), wherever
//! in FemtoVG's work the callback came. Asked outside a frame, nothing is
//! drawn: the picture there is is the answer, and the next frame asks again.
//!
//! # Who still shows a texture
//!
//! Slint says nothing when an element goes, so every frame calls the roll
//! (`Spatial.tick`): each scene still shown asks for its picture again and is
//! given the one there is, without GL, unless it moved. A texture nobody
//! asked for is nobody's; it is kept a moment for the next scene -- a held
//! key makes none -- and given back when the interface has been at rest.
//! A texture that is replaced is deleted after the frame it was last shown in
//! was issued, and by then its element has been given another.
//!
//! # When it cannot be done
//!
//! Shaders, textures, contexts and budgets fail. The frame that found out is
//! not presented: the element it was for is told (`Spatial.kind`), Slint
//! draws its own scene in that place, and the frame is drawn again.
//!
//! # A scene of one's own
//!
//! Implement `Scene`, name it in `Kind`, and bind a `SpatialScene` to it. Do
//! not put one under `cache-rendering-hint`: the layer would keep a picture
//! of a texture that has moved on.

mod budget;
pub mod gl;
mod globe;
#[cfg(any(test, feature = "spatial-lab"))]
mod lab;
// For scenes with a lens; the product's first one takes the condition off.
#[cfg(any(test, feature = "spatial-lab"))]
mod math;
#[cfg(test)]
pub mod tests;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::ComponentHandle;

pub use budget::{Budget, Cost};
use gl::{Drawn, Error, Gpu, Kept, Scratch, Straight, Target};

use crate::{MediaBoxWindow, Spatial, SpatialCam, SpatialPlace};

/// How a scene is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    pub samples: i32,
    pub depth: bool,
    /// Its picture is straight alpha as it draws it; otherwise premultiplied,
    /// and made straight by a pass after it.
    pub straight: bool,
}

impl Pass {
    /// Drawn into its own texture, with nothing between.
    fn alone(&self) -> bool {
        self.straight && self.samples <= 1 && !self.depth
    }
}

/// What draws one kind of scene. One per context, however many elements show
/// it: its shaders and geometry are theirs together.
pub trait Scene {
    fn pass(&self) -> Pass;
    fn cost(&self) -> Cost;
    /// The pass is begun; the state is `Target::begin`'s.
    fn draw(&self, gpu: &Gpu, cam: &Cam, place: &Place, drawn: &mut Drawn);
}

/// The scenes there are. `Spatial.kind` says them as numbers, 0 for none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Globe,
    /// The element is there and nothing is drawn: what a frame costs without.
    #[cfg(any(test, feature = "spatial-lab"))]
    Nothing,
    #[cfg(any(test, feature = "spatial-lab"))]
    Advanced,
}

impl Kind {
    fn number(self) -> i32 {
        match self {
            Self::Globe => 1,
            #[cfg(any(test, feature = "spatial-lab"))]
            Self::Nothing => 2,
            #[cfg(any(test, feature = "spatial-lab"))]
            Self::Advanced => 3,
        }
    }

    fn of(number: i32) -> Option<Self> {
        match number {
            1 => Some(Self::Globe),
            #[cfg(any(test, feature = "spatial-lab"))]
            2 => Some(Self::Nothing),
            #[cfg(any(test, feature = "spatial-lab"))]
            3 => Some(Self::Advanced),
            _ => None,
        }
    }

    /// The part of the panel it covers now; empty when there is nothing to
    /// see.
    fn place(self, cam: &Cam) -> Place {
        match self {
            Self::Globe => globe::place(cam),
            #[cfg(any(test, feature = "spatial-lab"))]
            Self::Nothing => Place::default(),
            #[cfg(any(test, feature = "spatial-lab"))]
            Self::Advanced => lab::advanced::place(cam),
        }
    }

    /// The most of the panel it will cover, so a move makes no other texture:
    /// passed by the camera it is half as large again as at rest.
    fn room(self, cam: &Cam) -> (f32, f32) {
        #[cfg(any(test, feature = "spatial-lab"))]
        if self == Self::Advanced && lab::full() {
            return cam.panel;
        }
        let most = cam.size * 1.35;
        (most.min(cam.panel.0), most.min(cam.panel.1))
    }

    fn make(self, gpu: &Rc<Gpu>) -> Result<Option<Box<dyn Scene>>, Error> {
        Ok(match self {
            Self::Globe => Some(Box::new(globe::Globe::new(gpu)?)),
            #[cfg(any(test, feature = "spatial-lab"))]
            Self::Nothing => None,
            #[cfg(any(test, feature = "spatial-lab"))]
            Self::Advanced => Some(Box::new(lab::advanced::Advanced::new(gpu, &lab::config())?)),
        })
    }
}

/// `SpatialCam`, in the scenes' own terms.
pub struct Cam {
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
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Place {
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

impl From<&SpatialPlace> for Place {
    fn from(p: &SpatialPlace) -> Self {
        Self { x: p.x, y: p.y, w: p.w, h: p.h }
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
        }
    }
}

/// Which elements are whose, and what may be spent on them.
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    /// An element's name (a launcher tile's id; "*" for any), and its scene.
    by: Vec<(String, Kind)>,
    budget: Budget,
}

impl Shown {
    fn kind(&self, name: &str) -> Option<Kind> {
        self.by.iter().find(|(who, _)| who == "*" || who == name).map(|&(_, kind)| kind)
    }

    /// `globe`: the product's own scenes, by their names.
    #[cfg(any(test, not(feature = "spatial-lab")))]
    fn parse(text: &str) -> Option<Self> {
        (text.trim() == "globe").then(|| Self { by: vec![("browser".into(), Kind::Globe)], budget: Budget::PRODUCT })
    }
}

/// One element's picture.
struct Slot {
    name: String,
    kind: Kind,
    target: Target,
    image: slint::Image,
    /// What it is a picture of.
    of: Option<(SpatialCam, SpatialPlace)>,
    /// The last frame it was asked for in.
    asked: u64,
}

/// What is on the GPU, and of which context. Fields in the order they go.
struct OnGpu {
    slots: Vec<Slot>,
    /// Replaced in this frame, shown in it perhaps: gone after it.
    retired: Vec<Target>,
    scratch: Scratch,
    scenes: Vec<(Kind, Option<Box<dyn Scene>>)>,
    straight: Straight,
    gpu: Rc<Gpu>,
}

struct Stage {
    shown: Shown,
    /// The context of the last frame, whatever could be made of it.
    context: Option<gl::ContextId>,
    on: Option<OnGpu>,
    /// Kinds that cannot be drawn in this context, and whether ever.
    failed: Vec<(Kind, bool)>,
    /// Elements there was no room for, and how many there was room for.
    refused: Vec<(String, usize)>,
    panel: slint::PhysicalSize,
    frame: u64,
    spent: Cost,
    /// Answered outside a frame, or with what is gone: to be asked again.
    asked_outside: bool,
    /// In this frame something fell back to Slint's own drawing.
    fell: bool,
    /// What draws an element has changed: `Spatial.epoch` is behind.
    changed: bool,
    /// Pictures drawn, ever.
    #[cfg_attr(not(test), allow(dead_code))]
    drawn: u64,
    /// The most shown in one frame, ever.
    #[cfg_attr(not(test), allow(dead_code))]
    peak_slots: usize,
}

thread_local! {
    static STAGE: RefCell<Option<Stage>> = const { RefCell::new(None) };
    static UI: RefCell<Option<slint::Weak<MediaBoxWindow>>> = const { RefCell::new(None) };
    static REST: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    /// The platform is inside a frame: the context is its own.
    static DRAWING: Cell<bool> = const { Cell::new(false) };
    /// The frame being drawn is not to be presented: it lacks something, and
    /// is drawn again.
    static WITHHELD: Cell<bool> = const { Cell::new(false) };
    static MAY_WITHHOLD: Cell<bool> = const { Cell::new(false) };
}

/// How long a texture nobody shows is kept for the next that wants one.
const KEPT_FOR: Duration = Duration::from_millis(1500);
const KEPT_FRAMES: u64 = 600;
/// How often a frame is drawn again for what fell back in it. Each time at
/// least one element or one kind has; there are never this many.
const AGAIN: u32 = 12;

impl OnGpu {
    fn new(budget: &Budget) -> Result<Self, Error> {
        let gpu = Gpu::new(budget.bytes)?;
        let _kept = Kept::of(&gpu.gl);
        let straight = Straight::new(&gpu)?;
        Ok(Self { slots: Vec::new(), retired: Vec::new(), scratch: Scratch::default(), scenes: Vec::new(), straight, gpu: gpu.clone() })
    }

    /// The shaders and the geometry of `kind`, once.
    fn prepare(&mut self, kind: Kind, budget: &Budget) -> Result<(), Error> {
        if self.scenes.iter().any(|(have, _)| *have == kind) {
            return Ok(());
        }
        let _kept = Kept::of(&self.gpu.gl);
        let scene = kind.make(&self.gpu)?;
        if let Some(scene) = &scene {
            budget.holds(&scene.cost())?;
        }
        self.scenes.push((kind, scene));
        Ok(())
    }

    /// A slot to draw `name` into: its own, one nobody has asked for lately,
    /// or a new one.
    fn slot(&mut self, name: &str, kind: Kind, used: (i32, i32), room: (i32, i32), frame: u64, most: usize) -> Result<usize, Error> {
        let own = self.slots.iter().position(|s| s.name == name && s.kind == kind && s.asked < frame);
        if let Some(at) = own.filter(|&at| self.slots[at].target.holds(used)) {
            return Ok(at);
        }
        // Not asked for in the last frame either: in this one it may be yet.
        let idle = |s: &Slot| s.asked + 1 < frame;
        let area = |size: (i32, i32)| size.0 as i64 * size.1 as i64;
        let at = self.slots.iter().position(|s| idle(s) && s.target.holds(room) && area(s.target.size) <= 2 * area(room));
        if own.is_none() && let Some(at) = at {
            let slot = &mut self.slots[at];
            (slot.name, slot.kind, slot.of) = (name.into(), kind, None);
            return Ok(at);
        }
        // Room for another, from those nobody shows.
        let bytes = Target::bytes(room, false);
        while (own.is_none() && self.slots.len() >= most) || !self.gpu.room_for(bytes) {
            let Some(oldest) = (0..self.slots.len()).filter(|&i| idle(&self.slots[i])).min_by_key(|&i| self.slots[i].asked) else {
                return Err(Error::Budget(if self.slots.len() >= most { "scenes at once" } else { "memory" }));
            };
            self.slots.swap_remove(oldest);
        }
        let target = Target::new(&self.gpu, room, 1, false)?;
        // SAFETY: an RGBA texture of this context, the one Slint draws with.
        // It outlives every frame that names it: `OnGpu` lets go of one only
        // after the frame in which it was last shown, or when nobody has
        // asked for it in a whole frame (see the module's head).
        let image = unsafe {
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                target.texture.name.0,
                (target.size.0 as u32, target.size.1 as u32).into(),
            )
        }
        .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
        .build();
        let slot = Slot { name: name.into(), kind, target, image, of: None, asked: 0 };
        // Its own, outgrown (the panel's mode, the scale).
        if let Some(at) = self.slots.iter().position(|s| s.name == name && s.kind == kind && s.asked < frame) {
            let old = std::mem::replace(&mut self.slots[at], slot);
            self.retired.push(old.target);
            return Ok(at);
        }
        self.slots.push(slot);
        Ok(self.slots.len() - 1)
    }

    fn draw(&self, at: usize, cam: &Cam, place: &Place) -> Result<Drawn, Error> {
        let (gpu, slot) = (&self.gpu, &self.slots[at]);
        let gl = &gpu.gl;
        let Some((_, Some(scene))) = self.scenes.iter().find(|(kind, _)| *kind == slot.kind) else {
            return Err(Error::Context);
        };
        let pass = scene.pass();
        let used = (place.w, place.h);
        let mut drawn = Drawn::default();
        if pass.alone() {
            slot.target.begin(gl, used);
            scene.draw(gpu, cam, place, &mut drawn);
            drawn.passes = 1;
            return gpu.well().map(|()| drawn);
        }
        self.scratch.with(gpu, slot.target.size, (pass.samples, pass.depth), |target| {
            target.begin(gl, used);
            scene.draw(gpu, cam, place, &mut drawn);
            target.end(gl);
            slot.target.begin(gl, used);
            self.straight.draw(gl, target, &mut drawn);
            drawn.passes = 2;
            Ok(())
        })?;
        gpu.well().map(|()| drawn)
    }
}

impl Stage {
    fn new(shown: Shown) -> Self {
        Self {
            shown,
            context: None,
            on: None,
            failed: Vec::new(),
            refused: Vec::new(),
            panel: slint::PhysicalSize::default(),
            frame: 0,
            spent: Cost::default(),
            asked_outside: false,
            fell: false,
            changed: false,
            drawn: 0,
            peak_slots: 0,
        }
    }

    /// What draws this element now.
    fn kind(&self, name: &str) -> Option<Kind> {
        let kind = self.shown.kind(name)?;
        let not = self.failed.iter().any(|(k, _)| *k == kind) || self.refused.iter().any(|(who, _)| who == name);
        (!not).then_some(kind)
    }

    /// A frame begins, in the platform's context. Whether the roll is to be
    /// called.
    fn open(&mut self, panel: slint::PhysicalSize) -> bool {
        self.frame += 1;
        self.spent = Cost::default();
        let context = gl::current();
        if context != self.context {
            // The old one's names mean nothing in this: forgotten.
            self.on = None;
            self.context = context;
            self.changed |= !self.failed.is_empty() || !self.refused.is_empty();
            self.failed.clear();
            self.refused.clear();
            // What was shown of it is to be shown again.
            self.asked_outside = true;
        }
        if panel != self.panel {
            // What could not be had at that size may be at this one.
            self.panel = panel;
            self.changed |= !self.refused.is_empty() || self.failed.iter().any(|(_, lasts)| !lasts);
            self.refused.clear();
            self.failed.retain(|(_, lasts)| *lasts);
        }
        // On the GPU before it is first wanted: shaders are compiled on the
        // first frame of all, not on the first of a move.
        let mut kinds: Vec<Kind> = Vec::new();
        for &(_, kind) in &self.shown.by {
            if !kinds.contains(&kind) && !self.failed.iter().any(|(k, _)| *k == kind) {
                kinds.push(kind);
            }
        }
        for kind in kinds {
            if let Err(error) = self.make(kind) {
                self.fail(kind, &error);
            }
        }
        // The interface is told of those before it draws.
        self.fell = false;
        std::mem::take(&mut self.asked_outside) || self.on.as_ref().is_some_and(|on| !on.slots.is_empty())
    }

    fn make(&mut self, kind: Kind) -> Result<(), Error> {
        if self.on.is_none() {
            if self.context.is_none() {
                return Err(Error::Context);
            }
            let at = std::time::Instant::now();
            let on = OnGpu::new(&self.shown.budget)?;
            eprintln!("mediabox-tv.spatial context ready in {:.1} ms fetch={}", at.elapsed().as_secs_f64() * 1e3, on.gpu.fetch);
            self.on = Some(on);
        }
        let Some(on) = &mut self.on else { return Err(Error::Context) };
        if on.scenes.iter().any(|(have, _)| *have == kind) {
            return Ok(());
        }
        let at = std::time::Instant::now();
        on.prepare(kind, &self.shown.budget)?;
        eprintln!("mediabox-tv.spatial {kind:?} on the GPU in {:.1} ms held={}", at.elapsed().as_secs_f64() * 1e3, on.gpu.held());
        Ok(())
    }

    /// No element is drawn as `kind` from here on: for this context's life,
    /// or until the panel is another.
    fn fail(&mut self, kind: Kind, error: &Error) {
        eprintln!("mediabox-tv.spatial {kind:?} given back to the interface: {error}");
        self.failed.push((kind, error.lasts()));
        self.fell = true;
        self.changed = true;
    }

    fn picture(&mut self, name: &str, kind: Kind, cam: &SpatialCam, place: &SpatialPlace) -> slint::Image {
        match self.drawn_picture(name, kind, cam, place) {
            Ok(image) => image,
            Err(error @ Error::Budget(_)) => {
                let shown = self.on.as_ref().map_or(0, |on| on.slots.iter().filter(|s| s.asked == self.frame).count());
                eprintln!("mediabox-tv.spatial {name} given back to the interface, {shown} scenes drawn: {error}");
                self.refused.push((name.into(), shown));
                self.fell = true;
                self.changed = true;
                slint::Image::default()
            }
            Err(error) => {
                if !self.failed.iter().any(|(k, _)| *k == kind) {
                    self.fail(kind, &error);
                }
                slint::Image::default()
            }
        }
    }

    fn drawn_picture(&mut self, name: &str, kind: Kind, cam: &SpatialCam, place: &SpatialPlace) -> Result<slint::Image, Error> {
        let frame = self.frame;
        let on = self.on.as_mut().ok_or(Error::Context)?;
        if !on.gpu.is_current() {
            return Err(Error::Context);
        }
        let asked = Some((cam.clone(), place.clone()));
        if !running(kind) && let Some(slot) = on.slots.iter_mut().find(|s| s.name == name && s.kind == kind && s.of == asked) {
            slot.asked = frame;
            return Ok(slot.image.clone());
        }
        let Some((_, Some(scene))) = on.scenes.iter().find(|(k, _)| *k == kind) else {
            return Err(Error::Context);
        };
        let (cam, place) = (Cam::from(cam), Place::from(place));
        let mut spent = self.spent;
        spent.add(&scene.cost(), place.w as u64 * place.h as u64);
        self.shown.budget.holds(&spent)?;
        #[cfg(feature = "spatial-lab")]
        let at = std::time::Instant::now();
        let gpu = on.gpu.clone();
        let _kept = Kept::of(&gpu.gl);
        gpu.listen();
        let used = (place.w, place.h);
        let room = kind.room(&cam);
        let whole = |used: i32, room: f32| (used.max(room.ceil() as i32) + 15) & !15;
        let room = (whole(used.0, room.0), whole(used.1, room.1));
        let at_slot = on.slot(name, kind, used, room, frame, self.shown.budget.scenes)?;
        on.slots[at_slot].of = None;
        let drawn = on.draw(at_slot, &cam, &place);
        // Asked for, whatever came of it: not another's in this frame.
        let slot = &mut on.slots[at_slot];
        slot.asked = frame;
        let drawn = drawn?;
        slot.of = asked;
        let image = slot.image.clone();
        self.spent = spent;
        self.drawn += 1;
        self.peak_slots = self.peak_slots.max(on.slots.iter().filter(|s| s.asked == frame).count());
        #[cfg(feature = "spatial-lab")]
        lab::drew(kind, at.elapsed(), drawn, (used, gpu.held(), on.slots.len()));
        let _ = drawn;
        #[cfg(test)]
        tests::picture_drawn();
        Ok(image)
    }

    /// The frame has been issued. Whether something is nobody's now.
    fn close(&mut self) -> bool {
        let frame = self.frame;
        let Some(on) = &mut self.on else { return false };
        on.retired.clear();
        // However long it moves.
        on.slots.retain(|s| s.asked + KEPT_FRAMES > frame);
        on.slots.iter().any(|s| s.asked < frame)
    }

    /// The interface has been at rest: what nobody shows is given back, and
    /// an element there was no room for may be tried again.
    fn rest(&mut self) {
        let frame = self.frame;
        let Some(on) = &mut self.on else { return };
        on.slots.retain(|s| s.asked >= frame);
        let scratch = |s: &Slot| on.scenes.iter().any(|(k, scene)| *k == s.kind && scene.as_ref().is_some_and(|scene| !scene.pass().alone()));
        if !on.slots.iter().any(scratch) {
            on.scratch.clear();
        }
        if on.gpu.is_current() {
            use glow::HasContext;
            // SAFETY: the context is current; what was deleted is given back
            // now rather than with the next frame, which may be long in
            // coming.
            unsafe { on.gpu.gl.flush() };
        }
        let live = on.slots.len();
        let before = self.refused.len();
        self.refused.retain(|&(_, shown)| live >= shown);
        self.changed |= self.refused.len() != before;
    }
}

/// Moving whether the camera is or not: a measurement's.
fn running(_kind: Kind) -> bool {
    #[cfg(any(test, feature = "spatial-lab"))]
    return _kind == Kind::Advanced && lab::config().run;
    #[cfg(not(any(test, feature = "spatial-lab")))]
    false
}

fn ui() -> Option<MediaBoxWindow> {
    UI.with(|cell| cell.borrow().as_ref().and_then(slint::Weak::upgrade))
}

fn stage<T>(f: impl FnOnce(&mut Stage) -> T) -> Option<T> {
    STAGE.with(|stage| stage.borrow_mut().as_mut().map(f))
}

/// Says to the interface that what draws its elements has changed, if it has.
fn tell() {
    if stage(|stage| std::mem::take(&mut stage.changed)) == Some(true)
        && let Some(ui) = ui()
    {
        let spatial = ui.global::<Spatial>();
        spatial.set_epoch(spatial.get_epoch().wrapping_add(1));
    }
}

/// The scenes MEDIABOX_TV_SPATIAL names, for this window; none, and nothing
/// here ever runs.
pub fn attach(ui: &MediaBoxWindow) {
    let Ok(text) = std::env::var("MEDIABOX_TV_SPATIAL") else { return };
    #[cfg(feature = "spatial-lab")]
    let shown = lab::parse(&text);
    #[cfg(not(feature = "spatial-lab"))]
    let shown = Shown::parse(&text);
    match shown {
        Some(shown) => start(ui, shown),
        None => eprintln!("mediabox-tv.spatial no such scene: {text}"),
    }
    #[cfg(feature = "spatial-lab")]
    lab::listen();
}

fn start(ui: &MediaBoxWindow, shown: Shown) {
    STAGE.with(|stage| *stage.borrow_mut() = Some(Stage::new(shown)));
    UI.with(|cell| *cell.borrow_mut() = Some(ui.as_weak()));
    let spatial = ui.global::<Spatial>();
    spatial.on_kind(|name, _epoch| stage(|stage| stage.kind(&name)).flatten().map_or(0, Kind::number));
    spatial.on_place(|kind, cam| {
        let place = Kind::of(kind).map(|kind| kind.place(&Cam::from(&cam))).unwrap_or_default();
        SpatialPlace { x: place.x, y: place.y, w: place.w, h: place.h }
    });
    spatial.on_picture(|name, kind, cam, place, _tick| {
        let Some(kind) = Kind::of(kind) else { return slint::Image::default() };
        stage(|stage| {
            if !DRAWING.get() {
                stage.asked_outside = true;
                let of = |s: &&Slot| s.name == name.as_str() && s.kind == kind;
                return stage.on.as_ref().and_then(|on| on.slots.iter().find(of)).map(|s| s.image.clone()).unwrap_or_default();
            }
            let image = stage.picture(&name, kind, &cam, &place);
            if stage.fell && MAY_WITHHOLD.get() {
                WITHHELD.set(true);
            }
            image
        })
        .unwrap_or_default()
    });
    spatial.set_on(true);
}

/// Everything given back, while its context is still there.
pub fn stop() {
    REST.with(|timer| timer.borrow_mut().take());
    if let Some(ui) = ui() {
        ui.global::<Spatial>().set_on(false);
    }
    drop(STAGE.with(|stage| stage.borrow_mut().take()));
}

/// Whether the frame being drawn is not to be presented.
pub fn withheld() -> bool {
    WITHHELD.get()
}

/// Ends the drawing however it ends.
struct Drawing;

impl Drawing {
    fn begin(may_withhold: bool) -> Self {
        WITHHELD.set(false);
        MAY_WITHHOLD.set(may_withhold);
        DRAWING.set(true);
        Self
    }
}

impl Drop for Drawing {
    fn drop(&mut self) {
        DRAWING.set(false);
        MAY_WITHHOLD.set(false);
        WITHHELD.set(false);
    }
}

/// A frame: `draw` is Slint's renderer, the context is current.
pub fn frame<T>(mut draw: impl FnMut() -> T) -> T {
    if STAGE.with(|stage| stage.borrow().is_none()) {
        return draw();
    }
    let mut again = AGAIN;
    loop {
        let Some(ui) = ui() else { return draw() };
        let panel = ui.window().size();
        if stage(|stage| stage.open(panel)) == Some(true) {
            let spatial = ui.global::<Spatial>();
            spatial.set_tick(spatial.get_tick().wrapping_add(1));
        }
        tell();
        let drawing = Drawing::begin(again > 0);
        let answer = draw();
        let withheld = withheld();
        drop(drawing);
        let idle = stage(Stage::close).unwrap_or_default();
        tell();
        if withheld && again > 0 {
            again -= 1;
            continue;
        }
        if idle {
            settle();
        }
        #[cfg(feature = "spatial-lab")]
        lab::keep_running();
        return answer;
    }
}

/// What nobody shows, given back once the interface has been at rest. Not a
/// frame: nothing changes on the panel.
fn settle() {
    REST.with(|timer| {
        timer.borrow_mut().get_or_insert_default().start(slint::TimerMode::SingleShot, KEPT_FOR, || {
            stage(Stage::rest);
            tell();
        });
    });
}
