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

#[test]
fn a_scene_is_asked_for_in_words_and_bounded() {
    let config = lab::Config::parse("stress,particles=20000 objects=64,msaa=1 full run on=a+b\n").unwrap();
    assert!(config.run && config.samples == 1);
    assert!(lab::Config::parse("").is_none() && lab::Config::parse("advanced nonsense=1").is_none());
    assert!(lab::Config::parse("stress particles=1e12").is_some());
    // The product knows its own scenes and nothing of the laboratory's.
    assert_eq!(Shown::parse("globe").unwrap().kind("browser"), Some(Kind::Globe));
    assert_eq!(Shown::parse("globe").unwrap().kind("kodi"), None);
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
    for kind in [Kind::Globe, Kind::Nothing, Kind::Advanced] {
        assert_eq!(Kind::of(kind.number()), Some(kind));
    }
    assert_eq!(Kind::of(0), None);
}
