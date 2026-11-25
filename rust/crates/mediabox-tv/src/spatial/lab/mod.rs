//! The laboratory: scenes that load the foundation, and the knobs a
//! measurement turns. Not in the product's binary (`spatial-lab`).

pub mod advanced;

use std::cell::RefCell;

use super::{Budget, Kind, Shown};

/// What the laboratory's scenes are made of.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    kind: Kind,
    /// Quads from pole to pole of the globe.
    globe: u32,
    rings: usize,
    objects: usize,
    /// Quads from pole to pole of a satellite; a ring's are as fine.
    detail: u32,
    pub layers: usize,
    particles: usize,
    /// A particle's size, relative to its own.
    dust: f32,
    pub samples: i32,
    /// The whole panel rather than the scene's place in it.
    full: bool,
    /// Moving for as long as it is shown, for a measurement: not of the
    /// camera's, and never at rest.
    pub run: bool,
    /// The product's budget, or none.
    bounded: bool,
    /// Launcher tiles, by id; "*" for all of them.
    on: Vec<String>,
    #[cfg_attr(not(feature = "spatial-lab"), allow(dead_code))]
    fault: Option<super::gl::Fault>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            kind: Kind::Advanced,
            globe: 64,
            rings: 5,
            objects: 24,
            detail: 12,
            layers: 2,
            particles: 0,
            dust: 1.0,
            samples: 4,
            full: false,
            run: false,
            bounded: false,
            on: vec!["browser".into()],
            fault: None,
        }
    }
}

impl Config {
    /// `globe`, `advanced`, `stress` or `nothing`, then `name=value`s.
    pub fn parse(text: &str) -> Option<Self> {
        use super::gl::Fault;
        let mut words = text.split([' ', ',', '\n']).filter(|word| !word.is_empty());
        let base = Self::default();
        let mut config = match words.next()? {
            "nothing" => Self { kind: Kind::Nothing, ..base },
            "globe" => Self { kind: Kind::Globe, ..base },
            "advanced" => base,
            "stress" => Self { objects: 200, particles: 5000, ..base },
            _ => return None,
        };
        for word in words {
            let (name, value) = word.split_once('=').unwrap_or((word, "1"));
            match name {
                "on" => {
                    config.on = value.split('+').map(str::to_string).collect();
                    continue;
                }
                "fault" => {
                    config.fault = match value {
                        "shader" => Some(Fault::Shader),
                        "buffer" => Some(Fault::Buffer),
                        "texture" => Some(Fault::Texture),
                        "framebuffer" => Some(Fault::Framebuffer),
                        "draw" => Some(Fault::Draw),
                        "context" => Some(Fault::Context),
                        _ => None,
                    };
                    continue;
                }
                _ => {}
            }
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
                "full" => config.full = n != 0.0,
                "run" => config.run = n != 0.0,
                "bounded" => config.bounded = n != 0.0,
                _ => return None,
            }
        }
        Some(config)
    }
}

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
}

pub fn config() -> Config {
    CONFIG.with_borrow(Clone::clone)
}

pub fn full() -> bool {
    CONFIG.with_borrow(|config| config.full)
}

/// `text`'s scenes, its knobs turned.
pub fn parse(text: &str) -> Option<Shown> {
    let config = Config::parse(text)?;
    let shown = Shown {
        by: config.on.iter().map(|name| (name.clone(), config.kind)).collect(),
        budget: if config.bounded { Budget::PRODUCT } else { Budget::NONE },
    };
    CONFIG.set(config);
    Some(shown)
}

#[cfg(feature = "spatial-lab")]
pub use live::{drew, keep_running, listen};

/// On a television: turned while it runs, and what it costs said every second.
#[cfg(feature = "spatial-lab")]
mod live {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use slint::ComponentHandle;

    use super::super::gl::{Drawn, FAULT};
    use super::super::{Kind, Spatial, Stage, stage, ui};
    use super::{config, parse};

    /// Another scene without another start: the file's words, as they change.
    pub fn listen() {
        FAULT.set(config().fault);
        let Some(path) = std::env::var_os("MEDIABOX_TV_SPATIAL_CONTROL") else { return };
        let timer = slint::Timer::default();
        let last = RefCell::new(String::new());
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if *last.borrow() == text {
                return;
            }
            if let Some(shown) = parse(&text) {
                eprintln!("mediabox-tv.spatial now {:?}", config());
                FAULT.set(config().fault);
                stage(|stage| {
                    *stage = Stage::new(shown);
                    stage.asked_outside = true;
                });
                if let Some(ui) = ui() {
                    let spatial = ui.global::<Spatial>();
                    spatial.set_epoch(spatial.get_epoch().wrapping_add(1));
                    ui.window().request_redraw();
                }
            }
            *last.borrow_mut() = text;
        });
        // For as long as the process.
        std::mem::forget(timer);
    }

    /// Frames for as long as a running scene is shown.
    pub fn keep_running() {
        let shown = stage(|stage| stage.on.as_ref().is_some_and(|on| on.slots.iter().any(|s| s.asked == stage.frame)));
        if config().run && shown == Some(true) {
            crate::platform::keep_drawing(Duration::from_secs(1));
        }
    }

    #[derive(Default)]
    struct Report {
        pictures: u32,
        cpu: Duration,
        drawn: Drawn,
        since: Option<Instant>,
    }

    thread_local! {
        static REPORT: RefCell<Report> = RefCell::new(Report::default());
    }

    pub fn drew(kind: Kind, cpu: Duration, d: Drawn, (used, held, slots): ((i32, i32), u64, usize)) {
        REPORT.with_borrow_mut(|report| {
            report.pictures += 1;
            report.cpu += cpu;
            report.drawn = d;
            let now = Instant::now();
            let since = *report.since.get_or_insert(now);
            if now.duration_since(since) < Duration::from_secs(1) {
                return;
            }
            let frames = report.pictures;
            eprintln!(
                "mediabox-tv.spatial {kind:?} frames={frames} cpu_us={:.1} passes={} draws={} objects={} instances={} vertices={} triangles={} pixels={} samples={} slots={slots} held={held}",
                report.cpu.as_secs_f64() * 1e6 / f64::from(frames),
                d.passes,
                d.draws,
                d.instances,
                d.instances,
                d.vertices,
                d.triangles,
                used.0 as u64 * used.1 as u64,
                config().samples,
            );
            *report = Report { since: Some(now), ..Report::default() };
        });
    }
}
