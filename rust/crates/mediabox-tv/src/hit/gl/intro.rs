//! The opening (ui/quiet/intro.slint) on the product's renderer.
//!
//! Its clock is stood at a moment (`QuietIntro.scrub`) and FemtoVG draws
//! that moment into the pbuffer, so what is checked -- and, with
//! MEDIABOX_TV_INTRO_FRAMES set to a directory, what is recorded frame by
//! frame for a film of it -- is what the appliance draws, not a mock-up.
//!
//! MEDIABOX_TV_INTRO_FRAMES=<dir> [MEDIABOX_TV_INTRO_PANEL=2560x1440@2]
//! [MEDIABOX_TV_INTRO_FPS=60] cargo test intro_on_film -- --nocapture

use super::*;

/// Ayarlar, as the preview is started from: what the opening opens onto.
fn under_it(drawn: &Drawn) {
    wear(crate::layout::Layout::Quiet);
    drawn.settings("Sistem");
    drawn.rest(800);
}

fn at(drawn: &Drawn, second: f32) -> Picture {
    let intro = drawn.ui.global::<crate::QuietIntro>();
    intro.set_shown(true);
    intro.set_scrub(second);
    drawn.picture()
}

fn luma(p: [u8; 3]) -> u32 {
    (u32::from(p[0]) * 3 + u32::from(p[1]) * 6 + u32::from(p[2])) / 10
}

/// Pixels drawn brighter than the ground ever is: the film's lines.
fn lit(picture: &Picture) -> usize {
    picture.pixels.as_chunks::<4>().0.iter().filter(|p| luma([p[0], p[1], p[2]]) > 70).count()
}

/// The opening is the home's film scene on its ground, moved by its
/// cameras: dark at first; the strips lit and running while the camera
/// travels; standing at rest; gone through at the end, leaving the
/// interface itself, untouched. At every panel the appliance drives.
#[test]
fn the_opening_is_the_film_scene_and_ends_on_the_interface() {
    let Some(drawn) = Drawn::open() else { return };
    for &(w, h, scale) in PANELS {
        drawn.panel(w, h, scale);
        under_it(&drawn);
        drawn.ui.global::<crate::QuietIntro>().set_shown(false);
        let interface = drawn.picture();
        let (cx, cy) = (w / 2, h / 2);
        assert!(luma(at(&drawn, 0.02).at(cx, cy)) < 12, "{w}x{h}: the dark");
        let travelling = lit(&at(&drawn, 1.6));
        let standing = at(&drawn, 3.05);
        assert!(travelling > 0, "{w}x{h}: the strips lit while the camera travels");
        assert!(lit(&standing) > 0, "{w}x{h}: the scene standing at rest");
        assert!(standing.unlike(&at(&drawn, 3.25), 2) == 0, "{w}x{h}: at rest, nothing moves");
        let length = drawn.ui.global::<crate::QuietIntro>().get_seconds();
        let after = at(&drawn, length);
        assert_eq!(after.unlike(&interface, 2), 0, "{w}x{h}: the opening leaves the interface as it was");
        drawn.ui.global::<crate::QuietIntro>().set_shown(false);
    }
}

/// A film of it, for looking at: every frame drawn by the product's renderer
/// at the stood clock, written as PNGs (ffmpeg makes the film). Only with
/// MEDIABOX_TV_INTRO_FRAMES.
#[test]
fn intro_on_film() {
    let Some(out) = std::env::var_os("MEDIABOX_TV_INTRO_FRAMES").map(std::path::PathBuf::from) else { return };
    let Some(drawn) = Drawn::open() else { return };
    let panel = std::env::var("MEDIABOX_TV_INTRO_PANEL").unwrap_or_else(|_| "1920x1080@1".into());
    let (size, scale) = panel.split_once('@').unwrap_or((panel.as_str(), "1"));
    let (w, h) = size.split_once('x').expect("WxH");
    let (w, h, scale): (u32, u32, f32) = (w.parse().unwrap(), h.parse().unwrap(), scale.parse().unwrap());
    let fps: f32 = std::env::var("MEDIABOX_TV_INTRO_FPS").ok().and_then(|v| v.parse().ok()).unwrap_or(60.0);
    let only: Option<Vec<f32>> = std::env::var("MEDIABOX_TV_INTRO_AT")
        .ok()
        .map(|list| list.split(',').filter_map(|v| v.trim().parse().ok()).collect());
    std::fs::create_dir_all(&out).unwrap();
    drawn.panel(w, h, scale);
    under_it(&drawn);
    let length = drawn.ui.global::<crate::QuietIntro>().get_seconds();
    let moments: Vec<(String, f32)> = match only {
        Some(list) => list.into_iter().map(|s| (format!("at-{s:05.2}"), s)).collect(),
        None => (0..=(length * fps).ceil() as u32).map(|n| (format!("frame-{n:04}"), n as f32 / fps)).collect(),
    };
    for (name, second) in moments {
        at(&drawn, second.min(length)).save(&out.join(format!("{name}.png")));
    }
}
