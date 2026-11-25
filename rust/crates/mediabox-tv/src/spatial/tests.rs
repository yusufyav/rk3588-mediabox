//! What needs no interface; and what the tests that have one
//! (src/hit/gl/spatial.rs) ask of this.

use super::gl::FAULT;
use super::*;

pub use super::gl::Fault;

thread_local! {
    /// Called as a picture has been drawn, inside the callback.
    static ON_PICTURE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

pub fn on_picture(told: impl Fn() + 'static) {
    ON_PICTURE.set(Some(Box::new(told)));
}

pub fn picture_drawn() {
    ON_PICTURE.with_borrow(|told| {
        if let Some(told) = told {
            told();
        }
    });
}

/// The laboratory's words, or the product's (`product`), on this window.
pub fn start_with(ui: &MediaBoxWindow, text: &str) {
    start(ui, lab::parse(text).expect("a scene"));
}

/// The product's own.
pub fn start_product(ui: &MediaBoxWindow) {
    start(ui, Shown::product());
}

pub fn fault(fault: Option<Fault>) {
    FAULT.set(fault);
}

/// What is held, and what has been done.
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub drawn: u64,
    pub peak: usize,
    /// Textures, shown in the last frame, and all.
    pub live: usize,
    pub slots: usize,
    pub bytes: u64,
    pub objects: u32,
    /// Scenes whose shaders and geometry are there.
    pub made: usize,
    pub failed: usize,
    pub refused: usize,
}

pub fn state() -> State {
    stage(|stage| {
        let on = stage.on.as_ref();
        State {
            drawn: stage.drawn,
            peak: stage.peak_slots,
            live: on.map_or(0, |on| on.slots.iter().filter(|s| s.asked == stage.frame).count()),
            slots: on.map_or(0, |on| on.slots.len()),
            bytes: on.map_or(0, |on| on.gpu.held()),
            objects: on.map_or(0, |on| on.gpu.objects()),
            made: on.map_or(0, |on| on.scenes.len()),
            failed: stage.failed.len(),
            refused: stage.refused.len(),
        }
    })
    .expect("started")
}

/// Another budget than it was started with.
pub fn budget(change: impl FnOnce(&mut Budget)) {
    stage(|stage| change(&mut stage.shown.budget)).expect("started");
}

/// Every emblem all along its way, each plane at its own offset: the most that
/// is drawn at the edge of its place, and how much of it there is inside, in
/// levels. In the context that is current.
pub fn emblem_edges(mut told: impl FnMut(&str, f32, u8, usize)) {
    use glow::HasContext;
    let gpu = Gpu::new(1 << 30).expect("GLES 3");
    let gl = &gpu.gl;
    let mut tools = None;
    let scratch = Scratch::default();
    for model in emblem::Model::ALL {
        let scene = Kind::Emblem(model).make(&gpu, &mut tools).expect("a model").expect("a scene");
        for step in -8..=8 {
            let d = step as f32 / 10.0;
            for (far, near) in [(d, d), (d * 1.2, d * 0.6), (d * 0.7, d * 1.2)] {
                let fade = |d: f32, reach: f32| (1.0 - d.abs() / reach).max(0.0);
                let cam = Cam {
                    d,
                    d_far: far,
                    d_near: near,
                    f_main: fade(d, 0.9),
                    f_far: fade(far, 1.0),
                    f_near: fade(near, 0.65),
                    tint: [0.66, 0.71, 0.75, 1.0],
                    ink: [0.8, 0.8, 0.8, 1.0],
                    glow: [0.9, 0.9, 0.9, 1.0],
                    x: 800.0,
                    y: 800.0,
                    size: 400.0,
                    line: 2.0,
                    fine: 2.0,
                    depth: 1.0,
                    parallax: 1.0,
                    travel: 1.0,
                    panel: (2000.0, 2000.0),
                    scale: 2.0,
                };
                let place = Kind::Emblem(model).place(&cam);
                let size = (place.w, place.h);
                scratch
                    .with(&gpu, size, (1, true), |target| {
                        target.begin(gl, size);
                        scene.draw(&gpu, &cam, &place, &mut Drawn::default());
                        let mut pixels = vec![0u8; (size.0 * size.1 * 4) as usize];
                        // SAFETY: the context is current, the buffer as large as what is read.
                        unsafe {
                            gl.read_pixels(0, 0, size.0, size.1, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut pixels)));
                        }
                        let at = |x: i32, y: i32| pixels[((y * size.0 + x) * 4 + 3) as usize];
                        let across = (0..size.0).map(|x| at(x, 0).max(at(x, size.1 - 1)));
                        let down = (0..size.1).map(|y| at(0, y).max(at(size.0 - 1, y)));
                        let inside = pixels.as_chunks::<4>().0.iter().filter(|p| p[3] > 8).count();
                        told(model.id(), d, across.chain(down).max().unwrap_or(0), inside);
                        Ok(())
                    })
                    .expect("drawn");
            }
        }
    }
    gpu.well().expect("no error on the way");
}

#[test]
fn a_scene_is_asked_for_in_words_and_bounded() {
    let config = lab::Config::parse("stress,particles=20000 objects=64,msaa=1 full run on=a+b\n").unwrap();
    assert!(config.run && config.samples == 1);
    assert!(lab::Config::parse("").is_none() && lab::Config::parse("advanced nonsense=1").is_none());
    assert!(lab::Config::parse("stress particles=1e12").is_some());
    // The product knows its own scenes and nothing of the laboratory's.
    assert_eq!(Shown::parse("globe").unwrap().kind("browser"), Some(Kind::Globe));
    assert_eq!(Shown::parse("globe").unwrap().kind("kodi"), None);
    let cooling = Some(Kind::Emblem(emblem::Model::Cooling));
    assert_eq!((Shown::product().kind("emblem/cooling"), Shown::product().kind("browser")), (cooling, None));
    assert_eq!((Shown::product().kind("cooling"), Shown::parse("off").unwrap().kind("emblem/cooling")), (None, None));
    // Another's name for all of them is the launcher's.
    assert_eq!(lab::parse("globe on=*").unwrap().kind("emblem/cooling"), None);
    assert!(Shown::parse("stress").is_none());
}

/// What was counted on the televisions, and what the budget takes it for.
#[test]
fn the_budget_takes_a_scene_for_what_it_was_measured_to_be() {
    let of = |triangles, instances, layers| Cost { triangles, instances, layers };
    // Plus, 3840x2160: whole frame's cycles less the interface's own 1.6 M.
    for (what, cost, pixels, counted) in [
        ("L2", of(84_481, 32, 3), 2_761_403u64, 2.0e6),
        ("L3", of(195_857, 5208, 3), 2_761_403, 3.1e6),
        ("L2 full", of(84_481, 32, 3), 8_294_400, 4.3e6),
        ("L2 full, 8 shells", of(109_057, 38, 9), 8_294_400, 6.7e6),
        ("1000 objects", of(656_657, 6008, 3), 2_761_403, 5.5e6),
        ("2000 objects", of(1_232_657, 7008, 3), 2_761_403, 8.6e6),
        ("50000 particles", of(285_857, 50_208, 3), 2_761_403, 4.6e6),
        // Ultra, 2560x1440, less 0.54 M.
        ("Ultra L3", of(195_857, 5208, 3), 1_230_000, 2.35e6),
        ("Ultra 100000 particles", of(284_000, 100_032, 3), 1_230_000, 5.15e6),
    ] {
        let taken = cost.cycles(pixels) as f64;
        eprintln!("{what}: {taken:.2e} for {counted:.2e}");
        assert!(taken > 0.89 * counted && taken < 1.34 * counted, "{what}: {taken:.2e} for {counted:.2e}");
    }
    // A frame's: three of the heaviest at 60 Hz, one at 144.
    let (sixty, fast) = (Duration::from_nanos(16_666_667), Duration::from_nanos(6_944_444));
    let l3 = of(195_857, 5208, 3);
    assert_eq!(Budget::PRODUCT.holds(3 * l3.cycles(2_761_403), sixty), Ok(()));
    assert_eq!(Budget::PRODUCT.holds(l3.cycles(1_230_000), fast), Ok(()));
    assert!(Budget::PRODUCT.holds(of(2_000_000, 0, 1).cycles(0), sixty).is_err());
    assert!(Budget::PRODUCT.holds(of(1_000_000, 0, 1).cycles(0), fast).is_err());
    assert!(Budget::PRODUCT.holds(of(u64::MAX, u64::MAX, u32::MAX).cycles(u64::MAX), sixty).is_err());
    assert_eq!(Budget::NONE.holds(u64::MAX, fast), Ok(()));
}

#[test]
fn a_kind_is_its_number() {
    for kind in [Kind::Globe, Kind::Nothing, Kind::Advanced].into_iter().chain(emblem::Model::ALL.map(Kind::Emblem)) {
        assert_eq!(Kind::of(kind.number()), Some(kind));
    }
    assert_eq!((Kind::of(0), Kind::of(Kind::EMBLEMS + emblem::Model::ALL.len() as i32)), (None, None));
}
