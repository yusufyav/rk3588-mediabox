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

#[test]
fn a_frame_is_within_its_budget_or_says_what_it_is_not() {
    let budget = Budget::PRODUCT;
    let scene = Cost { triangles: budget.triangles / 3, instances: 10, layers: 2 };
    let mut frame = Cost::default();
    for _ in 0..3 {
        frame.add(&scene, 1_000_000);
        assert_eq!(budget.holds(&frame), Ok(()));
    }
    assert_eq!(frame.layers, 600);
    frame.add(&scene, 0);
    assert_eq!(budget.holds(&frame), Err(Error::Budget("triangles")));
    let mut full = Cost::default();
    full.add(&Cost { layers: 4, ..Cost::default() }, 3840 * 2160);
    full.add(&Cost { layers: u32::MAX / 10_000, ..Cost::default() }, 100);
    assert_eq!(budget.holds(&full), Err(Error::Budget("fill")));
    assert_eq!(budget.holds(&Cost { instances: u64::MAX, ..Cost::default() }), Err(Error::Budget("instances")));
}

#[test]
fn a_kind_is_its_number() {
    for kind in [Kind::Globe, Kind::Nothing, Kind::Advanced] {
        assert_eq!(Kind::of(kind.number()), Some(kind));
    }
    assert_eq!(Kind::of(0), None);
}
