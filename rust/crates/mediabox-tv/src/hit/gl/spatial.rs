//! GPU-resident scenes in the product's interface, on its renderer
//! (src/spatial): that they are the drawn ones, that they do not write over
//! one another, that what they hold is bounded and given back, and that
//! whatever goes wrong there is a scene on the panel.

use super::*;
use crate::spatial::tests::{Fault, State, budget, fault, on_picture, start_product, start_with, state};

/// A launcher of `ids`.
fn tiles(ids: &[&str]) -> Rc<VecModel<crate::AppTile>> {
    Rc::new(VecModel::from(
        ids.iter()
            .enumerate()
            .map(|(n, id)| crate::AppTile { id: (*id).into(), name: format!("Uygulama {n}").into(), ready: true, ..Default::default() })
            .collect::<Vec<_>>(),
    ))
}

fn row_of(apps: &VecModel<crate::AppTile>, id: &str) -> i32 {
    (0..slint::Model::row_count(apps)).find(|&i| slint::Model::row_data(apps, i).unwrap().id == id).expect("the tile") as i32
}

/// Until nothing moves -- however busy the machine -- and past what is kept
/// for the next scene.
fn settled(drawn: &Drawn) -> State {
    let least = std::time::Instant::now() + std::time::Duration::from_millis(400);
    let most = least + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < least || (drawn.ui.window().has_active_animations() && std::time::Instant::now() < most) {
        drawn.frame();
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    drawn.frame();
    std::thread::sleep(std::time::Duration::from_millis(1600));
    step(&drawn.ui);
    state()
}

/// The same moment, Slint's scenes or the GPU's.
fn show(drawn: &Drawn, on: bool) -> Picture {
    drawn.ui.global::<crate::Spatial>().set_on(on);
    // The element comes into being on one frame and is drawn on the next.
    drawn.draw();
    drawn.draw();
    drawn.read()
}

/// How alike two pictures are, in decibels.
fn psnr(a: &Picture, b: &Picture, what: &str) -> f64 {
    let (mut sum, mut worst, mut far, mut n) = (0u64, 0u8, 0usize, 0usize);
    for (p, q) in a.pixels.as_chunks::<4>().0.iter().zip(b.pixels.as_chunks::<4>().0) {
        for i in 0..3 {
            let by = p[i].abs_diff(q[i]);
            sum += u64::from(by) * u64::from(by);
            worst = worst.max(by);
            far += usize::from(by > 16);
            n += 1;
        }
    }
    let psnr = 10.0 * (255.0f64 * 255.0 / (sum as f64 / n as f64).max(1e-9)).log10();
    eprintln!("{what}: PSNR {psnr:.1} dB, worst {worst}, over 16: {:.4}%", 100.0 * far as f64 / n as f64);
    a.keep(&format!("{what}-slint"));
    b.keep(&format!("{what}-gpu"));
    psnr
}

/// The GPU-resident globe is the drawn one: at rest and half way through a
/// move, at the same instant, what Slint's own paths make and what the GPU's
/// curves make differ by no more than two rasterisers' edges do.
#[test]
fn the_gpu_globe_is_the_drawn_globe() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(6)));
    let row = row_of(&apps, "browser");
    start_with(&drawn.ui, "globe");
    for &(w, h, scale) in PANELS {
        drawn.panel(w, h, scale);
        drawn.vitals(false, true, 3);
        drawn.home(&apps, row);
        drawn.rest(1200);
        let alike = psnr(&show(&drawn, false), &show(&drawn, true), &format!("globe-{h}-rest"));
        assert!(alike > 42.0, "{h}: {alike}");
        for (to, ms) in [(row - 1, 90), (row, 260), (row + 1, 180), (row, 120), (row, 700)] {
            drawn.ui.set_focus_col(to);
            drawn.rest(ms);
            let alike = psnr(&show(&drawn, false), &show(&drawn, true), &format!("globe-{h}-{to}-{ms}"));
            assert!(alike > 40.0, "{h} {to} {ms}: {alike}");
        }
    }
    assert_eq!((state().failed, state().refused), (0, 0));
}

/// A scene at rest is not drawn: not by a frame that something else asked
/// for, and not by the launcher's model being replaced under it; and calling
/// the roll asks for no frame. And what the foundation draws beyond the globe
/// is there, at every size of panel.
#[test]
fn a_gpu_scene_at_rest_is_not_drawn_again() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(6)));
    let row = row_of(&apps, "browser");
    start_with(&drawn.ui, "stress objects=60 particles=800 msaa=4");
    for &(w, h, scale) in PANELS {
        drawn.panel(w, h, scale);
        drawn.home(&apps, row);
        drawn.rest(1200);
        let there = drawn.picture();
        there.keep(&format!("{h}-advanced"));
        assert!(!drawn.window.redraw.get(), "{h}: a frame at rest asks for another");
        let before = state().drawn;
        assert!(before > 0);
        drawn.vitals(true, false, 0);
        drawn.rest(300);
        drawn.ui.set_apps(ModelRc::from(Rc::new(VecModel::from(launcher(6)))));
        drawn.rest(300);
        assert_eq!(state().drawn, before, "{h}: drawn at rest");
        let without = show(&drawn, false);
        assert!(there.unlike(&without, 16) > (w * h / 100) as usize, "{h}: nothing to see");
        show(&drawn, true);
    }
}

/// The same instant, the GPU's scenes on the tiles of `on` and Slint's on the
/// rest.
fn moment(drawn: &Drawn, scene: &str, on: &str) -> Picture {
    start_with(&drawn.ui, &format!("{scene} on={on}"));
    drawn.draw();
    drawn.draw();
    drawn.read()
}

/// Scenes shown together are each their own. Between two applications the
/// GPU shows both; with the globe's drawn by Slint instead, the globe is what
/// Slint's own is, and the other is the very same.
#[test]
fn scenes_shown_together_do_not_write_over_one_another() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = tiles(&["a", "b", "browser", "c", "d"]);
    drawn.home(&apps, 0);
    drawn.rest(1200);
    let mut most = 0;
    for (to, ms) in [(1, 200), (2, 260), (1, 120), (3, 160), (2, 200), (4, 120), (2, 150), (0, 130)] {
        drawn.ui.set_focus_col(to);
        drawn.rest(ms);
        let all = moment(&drawn, "globe", "*");
        most = most.max(state().live);
        assert_eq!((state().failed, state().refused), (0, 0));
        let others = moment(&drawn, "globe", "a+b+c+d");
        let alike = psnr(&others, &all, &format!("together-{to}-{ms}"));
        assert!(alike > 40.0, "{to} {ms}: {alike}");
        // And again: the same, to the pixel.
        assert_eq!(moment(&drawn, "globe", "*").unlike(&all, 1), 0);
    }
    assert!(most >= 2, "never more than one at once");
}

/// Samples and a depth buffer are one target's, drawn in by each scene in
/// turn: two at once are what each is alone.
#[test]
fn scenes_that_share_a_depth_buffer_do_not_show_in_one_another() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = tiles(&["a", "b", "c", "d"]);
    drawn.home(&apps, 0);
    drawn.rest(1200);
    drawn.ui.set_focus_col(1);
    drawn.rest(230);
    let both = moment(&drawn, "advanced", "a+b");
    assert!(state().live >= 2, "{:?}", state());
    let (a, b) = (moment(&drawn, "advanced", "a"), moment(&drawn, "advanced", "b"));
    let again = moment(&drawn, "advanced", "a+b");
    assert!(both.unlike(&again, 1) == 0, "the same moment twice");
    // Alone, the other is Slint's own scene: so the two are unlike, and unlike
    // the two together.
    assert!(a.unlike(&b, 8) > 1000 && a.unlike(&both, 8) > 1000 && b.unlike(&both, 8) > 1000);
}

/// How many scenes there are at once, whatever is done to the launcher: every
/// speed of a held key, turned round, sent across it and back. All of them
/// GPU scenes, and none of them refused.
#[test]
fn no_way_of_moving_shows_more_scenes_than_the_budget_has() {
    let Some(drawn) = Drawn::open() else { return };
    let names: Vec<String> = (0..24).map(|n| format!("app-{n}")).collect();
    let apps = tiles(&names.iter().map(String::as_str).collect::<Vec<_>>());
    start_with(&drawn.ui, "globe on=* bounded");
    drawn.home(&apps, 0);
    drawn.rest(1200);
    let frames = |ms: u64| {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < until {
            drawn.frame();
        }
    };
    let mut reach = 0;
    for every in [16, 25, 33, 40, 60, 80, 110, 150, 220] {
        let mut go = |to: i32| {
            drawn.ui.set_focus_col(to);
            frames(every);
            reach = reach.max(scenes(&drawn.ui, 24));
        };
        (1..20).for_each(&mut go);
        (0..19).rev().for_each(&mut go);
        // There and back before either is done.
        [3, 0, 4, 1, 5, 2, 23, 0, 23, 12, 13, 11, 14, 10, 0].into_iter().for_each(&mut go);
        eprintln!("every {every} ms: {reach} in reach, {:?}", state());
    }
    let state = state();
    assert!(reach >= 3 && state.peak <= crate::spatial::Budget::PRODUCT.scenes, "{reach} {state:?}");
    assert_eq!((state.failed, state.refused), (0, 0), "{state:?}");
}

/// What is held goes up with what is shown and down again with it: round and
/// round the launcher, across panels, to the other face and out of the home
/// screen, nothing is left behind.
#[test]
fn what_scenes_hold_is_given_back() {
    let Some(drawn) = Drawn::open() else { return };
    let names: Vec<String> = (0..12).map(|n| format!("app-{n}")).collect();
    let apps = tiles(&names.iter().map(String::as_str).collect::<Vec<_>>());
    start_with(&drawn.ui, "advanced on=* objects=8 detail=6 globe=12 bounded");
    let round = || -> State {
        for (w, h, scale) in [(1920, 1080, 1.0), (3840, 2160, 2.0)] {
            drawn.panel(w, h, scale);
            drawn.home(&apps, 0);
            for to in [1, 2, 3, 4, 5, 4, 3, 2, 11, 0, 6] {
                drawn.ui.set_focus_col(to);
                drawn.rest(45);
                assert!(state().slots <= crate::spatial::Budget::PRODUCT.scenes);
                assert!(state().bytes <= crate::spatial::Budget::PRODUCT.bytes);
            }
        }
        drawn.panel(1920, 1080, 1.0);
        settled(&drawn)
    };
    round();
    let (once, textures) = (round(), TEXTURES.get());
    assert_eq!((once.live, once.slots), (1, 1), "{once:?}");
    for _ in 0..4 {
        round();
    }
    let often = round();
    assert_eq!((often.slots, often.bytes, often.objects), (once.slots, once.bytes, once.objects));
    assert!(TEXTURES.get() <= textures, "{} FemtoVG textures, {textures} before", TEXTURES.get());
    assert_eq!((often.failed, often.refused), (0, 0), "{often:?}");
    // The other face has none of them; a little later nothing is held for it.
    wear(crate::layout::Layout::Classic);
    let classic = settled(&drawn);
    assert_eq!((classic.live, classic.slots), (0, 0), "{classic:?}");
    assert!(classic.bytes < once.bytes / 2, "{} bytes of {}", classic.bytes, once.bytes);
    // And back: a scene, drawn again.
    wear(crate::layout::Layout::Quiet);
    drawn.home(&apps, 6);
    drawn.rest(1200);
    assert_eq!((state().live, state().failed), (1, 0));
    // Over the screens: the settings have no GPU scene.
    drawn.settings("Sistem");
    let settings = settled(&drawn);
    assert_eq!(settings.slots, 0, "{settings:?}");
}

/// Whatever fails -- before anything was drawn, or half way through a move --
/// Slint's own scene is on the panel in the very frame, and not one without a
/// scene was presented.
#[test]
fn what_cannot_be_drawn_is_drawn_by_the_interface() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(6)));
    let row = row_of(&apps, "browser");
    drawn.home(&apps, row);
    drawn.rest(1200);
    let slint = drawn.picture();
    for (what, scene) in [
        (Fault::Shader, "globe"),
        (Fault::Buffer, "globe"),
        (Fault::Texture, "globe"),
        (Fault::Framebuffer, "globe"),
        (Fault::Draw, "globe"),
        (Fault::Context, "globe"),
        (Fault::Shader, "advanced"),
        (Fault::Buffer, "advanced"),
        (Fault::Texture, "advanced"),
        (Fault::Framebuffer, "advanced"),
        (Fault::Draw, "advanced"),
    ] {
        // From the start.
        fault(Some(what));
        start_with(&drawn.ui, scene);
        let presented = PRESENTED.get();
        drawn.draw();
        assert_eq!(PRESENTED.get(), presented + 1);
        let unlike = drawn.read().unlike(&slint, 2);
        assert_eq!(unlike, 0, "{what:?} {scene}: the first frame is not the interface's own");
        assert_eq!((state().failed, state().slots == 0 || state().live == 0), (1, true), "{what:?} {scene} {:?}", state());
        for _ in 0..5 {
            drawn.frame();
        }
        assert!(!drawn.window.redraw.get(), "{what:?} {scene}: never at rest again");
        let drawn_once = WITHHELD.get();
        drawn.rest(50);
        assert_eq!(WITHHELD.get(), drawn_once, "{what:?} {scene}: tried again and again");

        // Half way through a life.
        fault(None);
        start_with(&drawn.ui, scene);
        drawn.draw();
        assert_eq!((state().live, state().failed), (1, 0), "{what:?} {scene} {:?}", state());
        assert!(drawn.read().unlike(&slint, 8) > 0 || scene == "globe");
        fault(Some(what));
        let held = state();
        // Something to draw, and something to allocate.
        drawn.panel(2560, 1440, 2.0);
        drawn.ui.set_focus_col(row + 1);
        drawn.rest(120);
        let there = drawn.read();
        // Shaders and buffers are long made, but a context that is gone is
        // gone; the rest fail at the next allocation or the next draw.
        let survives = matches!(what, Fault::Shader | Fault::Buffer);
        assert_eq!(state().failed, usize::from(!survives), "{what:?} {scene} {:?} {held:?}", state());
        drawn.ui.global::<crate::Spatial>().set_on(false);
        drawn.draw();
        drawn.draw();
        let own = drawn.read();
        if !survives {
            assert_eq!(there.unlike(&own, 2), 0, "{what:?} {scene}: not the interface's own");
        }
        // Over, and nothing left behind that the fault had half made.
        fault(None);
        drawn.panel(W, H, 1.0);
        drawn.ui.set_focus_col(row);
        drawn.rest(1200);
        drawn.ui.global::<crate::Spatial>().set_on(false);
        drawn.rest(60);
        assert_eq!(drawn.picture().unlike(&slint, 2), 0, "{what:?} {scene}: the panel is not what it was");
    }
    // A texture that cannot be had at one size may be at another.
    start_with(&drawn.ui, "globe");
    fault(Some(Fault::Texture));
    drawn.draw();
    assert_eq!(state().failed, 1);
    fault(None);
    drawn.panel(1366, 768, 1.0);
    drawn.rest(60);
    assert_eq!((state().failed, state().live), (0, 1), "{:?}", state());
    // A shader that will not compile will not at any.
    fault(Some(Fault::Shader));
    start_with(&drawn.ui, "globe");
    drawn.draw();
    fault(None);
    drawn.panel(W, H, 1.0);
    drawn.rest(60);
    assert_eq!((state().failed, state().live), (1, 0), "{:?}", state());
}

/// No frame without a scene, all the way through a move in which the GPU
/// fails: what is presented has the interface's ink where the scene is.
#[test]
fn a_failure_in_the_middle_of_a_move_is_never_seen() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = tiles(&["browser", "a", "b"]);
    start_with(&drawn.ui, "globe");
    drawn.home(&apps, 0);
    drawn.rest(1200);
    drawn.ui.set_focus_col(1);
    drawn.rest(100);
    fault(Some(Fault::Draw));
    let (presented, withheld) = (PRESENTED.get(), WITHHELD.get());
    drawn.frame();
    // One frame was asked for and one was presented; the one that failed was
    // not it.
    assert_eq!((PRESENTED.get(), WITHHELD.get()), (presented + 1, withheld + 1));
    let there = drawn.read();
    drawn.ui.global::<crate::Spatial>().set_on(false);
    drawn.draw();
    drawn.draw();
    assert!(psnr(&drawn.read(), &there, "failed-mid-move") > 35.0);
    fault(None);
}

/// NVIDIA's driver does not survive the process's end with framebuffers left
/// in a context that never ended (620.73): what was forgotten on purpose,
/// deleted after all.
fn delete_what_was_forgotten() {
    crate::spatial::stop();
    let gpu = crate::spatial::gl::Gpu::new(0).expect("GLES 3");
    for n in (1..4000).map(|n| NonZeroU32::new(n).unwrap()) {
        use glow::HasContext;
        // SAFETY: nothing is drawn after this.
        unsafe { gpu.gl.delete_framebuffer(glow::NativeFramebuffer(n)) };
    }
}

/// Another context: nothing of the old one is used, nothing of it deleted by
/// a name that is another's by now, and the scene is there.
#[test]
fn another_context_begins_again() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(6)));
    let row = row_of(&apps, "browser");
    start_with(&drawn.ui, "advanced objects=8 detail=6 globe=12");
    drawn.home(&apps, row);
    drawn.rest(1200);
    let (before, was) = (drawn.picture(), state());
    // To this module a context is who made it: the platform says so.
    crate::spatial::gl::context_made();
    drawn.draw();
    let now = state();
    assert_eq!((now.live, now.slots, now.failed), (1, 1, 0));
    assert_eq!((now.bytes, now.objects), (was.bytes, was.objects));
    assert_eq!(now.drawn, was.drawn + 1, "drawn again, once");
    assert_eq!(drawn.read().unlike(&before, 1), 0);
    // What failed in one may not in the next.
    fault(Some(Fault::Shader));
    crate::spatial::gl::context_made();
    drawn.draw();
    assert_eq!(state().failed, 1);
    fault(None);
    crate::spatial::gl::context_made();
    drawn.draw();
    assert_eq!((state().failed, state().live), (0, 1), "{:?}", state());
    assert_eq!(drawn.read().unlike(&before, 1), 0);
    delete_what_was_forgotten();
}

/// Two real contexts: what was made in one and let go of in the other leaves
/// the other's things alone, and is let go of in its own.
#[test]
fn a_name_is_only_deleted_where_it_means_something() {
    use crate::spatial::gl::{Gpu, Target};
    use glow::HasContext;
    let Ok(first) = Pbuffer::new() else { return };
    let gpu = Gpu::new(1 << 30).expect("GLES 3");
    let one = Target::new(&gpu, (64, 64), 1, true).expect("a target");
    let two = Target::new(&gpu, (64, 64), 4, false).expect("a target");
    let name = one.texture.name;
    assert_eq!(gpu.held(), 2 * 64 * 64 * 4 + 64 * 64 * 4);
    let second = Pbuffer::new().expect("another");
    let other = Gpu::new(1 << 30).expect("GLES 3");
    // SAFETY: the second context is current.
    unsafe {
        // The same name, another thing.
        let theirs: Vec<_> = (0..8).map(|_| other.gl.create_texture().unwrap()).collect();
        assert!(theirs.contains(&name), "contexts share no names");
        for texture in &theirs {
            other.gl.bind_texture(glow::TEXTURE_2D, Some(*texture));
        }
        drop(one);
        assert!(theirs.iter().all(|texture| other.gl.is_texture(*texture)));
    }
    drop(two);
    assert_eq!((gpu.held(), gpu.objects()), (0, 0));
    first.ensure_current().expect("the first again");
    // SAFETY: the first context is current, and these are its own.
    unsafe {
        assert!(gpu.gl.is_texture(name), "the first context's own is gone");
        for n in 1..=8 {
            gpu.gl.delete_framebuffer(glow::NativeFramebuffer(NonZeroU32::new(n).unwrap()));
            gpu.gl.delete_renderbuffer(glow::NativeRenderbuffer(NonZeroU32::new(n).unwrap()));
            gpu.gl.delete_texture(glow::NativeTexture(NonZeroU32::new(n).unwrap()));
        }
    }
    drop((first, second));
}

/// Whatever state FemtoVG is in when a scene is asked for, it is in after.
#[test]
fn the_state_of_gl_is_left_as_it_was() {
    use crate::spatial::gl::{Gpu, Kept, Target};
    use glow::HasContext;
    let Ok(_pbuffer) = Pbuffer::new() else { return };
    let gpu = Gpu::new(1 << 30).expect("GLES 3");
    let gl = &gpu.gl;
    let names = [
        glow::DRAW_FRAMEBUFFER_BINDING, glow::READ_FRAMEBUFFER_BINDING, glow::RENDERBUFFER_BINDING, glow::CURRENT_PROGRAM,
        glow::VERTEX_ARRAY_BINDING, glow::ARRAY_BUFFER_BINDING, glow::ACTIVE_TEXTURE, glow::BLEND_SRC_RGB, glow::BLEND_DST_RGB,
        glow::BLEND_SRC_ALPHA, glow::BLEND_DST_ALPHA, glow::BLEND_EQUATION_RGB, glow::BLEND_EQUATION_ALPHA, glow::DEPTH_FUNC,
        glow::DEPTH_WRITEMASK, glow::CULL_FACE_MODE, glow::FRONT_FACE, glow::SCISSOR_TEST, glow::STENCIL_TEST, glow::DEPTH_TEST,
        glow::CULL_FACE, glow::BLEND, glow::TEXTURE_BINDING_2D,
    ];
    let all = || {
        // SAFETY: the context is current.
        unsafe {
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut clear = [0.0; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear);
            let mask = gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK);
            // Texture unit 0 and the one that is active.
            let unit = gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32;
            let here: Vec<i32> = names.iter().map(|&name| gl.get_parameter_i32(name)).collect();
            gl.active_texture(glow::TEXTURE0);
            let zero = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
            gl.active_texture(unit);
            (here, zero, viewport, clear, mask, gl.get_parameter_f32(glow::DEPTH_CLEAR_VALUE))
        }
    };
    let target = Target::new(&gpu, (32, 32), 1, true).expect("a target");
    let other = Target::new(&gpu, (16, 16), 1, false).expect("a target");
    // SAFETY: the context is current.
    unsafe {
        other.begin(gl, (7, 5));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(other.texture.name));
        gl.active_texture(glow::TEXTURE3);
        gl.bind_texture(glow::TEXTURE_2D, Some(target.texture.name));
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        gl.blend_func_separate(glow::DST_COLOR, glow::ZERO, glow::ONE, glow::SRC_ALPHA);
        gl.blend_equation_separate(glow::FUNC_SUBTRACT, glow::MAX);
        gl.depth_func(glow::GEQUAL);
        gl.depth_mask(false);
        gl.clear_depth_f32(0.25);
        gl.cull_face(glow::FRONT);
        gl.front_face(glow::CW);
        gl.color_mask(true, false, true, false);
        gl.clear_color(0.1, 0.2, 0.3, 0.4);
        for on in [glow::SCISSOR_TEST, glow::STENCIL_TEST, glow::CULL_FACE, glow::BLEND] {
            gl.enable(on);
        }
    }
    let before = all();
    {
        let _kept = Kept::of(gl);
        target.begin(gl, (32, 32));
        target.end(gl);
        let _made = Target::new(&gpu, (8, 8), 4, true);
    }
    assert_eq!(all(), before);
    gpu.well().expect("no error on the way");
}

/// A scene is drawn before the panel's pass begins, not in the middle of it:
/// when a picture is asked for FemtoVG has issued nothing of the frame. And a
/// frame that has one is the clear and the drawing Slint makes without.
#[test]
fn a_picture_is_drawn_before_the_frame_is_issued() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = tiles(&["a", "b", "c", "d"]);
    start_with(&drawn.ui, "globe on=*");
    drawn.home(&apps, 0);
    drawn.rest(1200);
    let issued = Rc::new(Cell::new(0u32));
    let pictures = Rc::new(Cell::new(0u32));
    let begun = Rc::new(Cell::new(0u32));
    on_picture({
        let (issued, pictures, begun) = (issued.clone(), pictures.clone(), begun.clone());
        move || {
            pictures.set(pictures.get() + 1);
            issued.set(issued.get() + CLEARS.get() - begun.get());
        }
    });
    drawn.ui.set_focus_col(1);
    for _ in 0..20 {
        begun.set(CLEARS.get());
        drawn.frame();
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    assert!(pictures.get() >= 10, "{} pictures", pictures.get());
    assert_eq!(issued.get(), 0, "FemtoVG had begun its frame");
}

/// Past the budget an element is Slint's, whole, and the others are not
/// touched; with room again, it is the GPU's again.
#[test]
fn what_is_over_the_budget_is_drawn_by_the_interface() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = tiles(&["browser", "a", "b", "c"]);
    start_with(&drawn.ui, "globe on=*");
    // One scene, where a move shows two.
    budget(|budget| budget.scenes = 1);
    drawn.home(&apps, 0);
    drawn.rest(1200);
    assert_eq!((state().live, state().refused), (1, 0));
    drawn.ui.set_focus_col(1);
    let mut refused = false;
    for _ in 0..30 {
        let presented = PRESENTED.get();
        drawn.frame();
        assert_eq!(PRESENTED.get(), presented + 1, "a frame was lost, or one shown twice");
        assert!(state().slots <= 1, "{:?}", state());
        refused |= state().refused > 0;
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    assert!(refused, "{:?}", state());
    settled(&drawn);
    drawn.rest(100);
    let state_after = state();
    assert_eq!((state_after.live + state_after.refused, state_after.failed), (1, 0), "{state_after:?}");
    // Too much geometry for the product: refused as it is made, whatever the
    // scene, and Slint's is there.
    drawn.ui.global::<crate::Spatial>().set_on(false);
    drawn.home(&apps, 0);
    drawn.rest(1200);
    let slint = drawn.picture();
    start_with(&drawn.ui, "stress particles=100000 objects=2000 bounded");
    drawn.draw();
    assert_eq!(drawn.read().unlike(&slint, 2), 0);
    assert_eq!((state().failed, state().slots), (1, 0), "{:?}", state());
    // And memory: no texture where there is no room for one.
    start_with(&drawn.ui, "advanced");
    budget(|budget| budget.bytes = 1 << 20);
    drawn.draw();
    assert_eq!(drawn.read().unlike(&slint, 2), 0);
    assert_eq!(state().slots, 0, "{:?}", state());
}

/// Reduced motion lands a GPU scene where it lands Slint's.
#[test]
fn reduced_motion_lands_a_gpu_scene() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = tiles(&["a", "browser", "b", "c"]);
    start_with(&drawn.ui, "globe");
    drawn.home(&apps, 0);
    drawn.rest(1200);
    drawn.ui.set_focus_col(1);
    drawn.rest(150);
    drawn.ui.global::<crate::QuietMotion>().set_reduced(true);
    drawn.rest(30);
    drawn.draw();
    assert_eq!(state().live, 1, "{:?}", state());
    let before = state().drawn;
    drawn.rest(200);
    assert_eq!(state().drawn, before, "still moving");
    assert!(psnr(&show(&drawn, false), &show(&drawn, true), "reduced") > 42.0);
    drawn.ui.set_focus_col(3);
    drawn.frame();
    drawn.frame();
    assert_eq!(state().live, 0, "gone at once");
    drawn.ui.global::<crate::QuietMotion>().set_reduced(false);
}

/// An emblem is whole, all along its way: what it draws is inside the place
/// it says it is in. And at rest there is something to see of each.
#[test]
fn an_emblem_is_not_cut_by_its_place() {
    let Ok(_pbuffer) = Pbuffer::new() else { return };
    let mut cut = std::collections::BTreeMap::<String, (u8, f32)>::new();
    crate::spatial::tests::emblem_edges(|id, d, edge, inside| {
        assert!(d != 0.0 || inside > 2000, "{id}: {inside} pixels at rest");
        let worst = cut.entry(id.into()).or_default();
        // Fainter than a level of the panel's is nothing.
        if edge > worst.0.max(6) {
            *worst = (edge, d);
        }
    });
    cut.retain(|_, worst| worst.0 > 0);
    assert!(cut.is_empty(), "cut: {cut:?}");
}

/// The settings' scenes, by their own ids.
const EMBLEMS: &[&str] = &[
    "media", "picture", "links", "remote", "live", "device", "system", "diagnostics", "playback", "account", "cec", "output", "cooling",
    "audio", "ethernet", "wifi", "storage", "services",
];

/// Pictures of every emblem, at rest and on the way, the GPU's and Slint's.
/// As `shots_family`, only with MEDIABOX_TV_SHOTS set.
#[test]
#[ignore]
fn shots_emblems() {
    let Some(out) = std::env::var_os("MEDIABOX_TV_SHOTS").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&out).unwrap();
    let drawn = Drawn::open().expect("a GLES device to draw with");
    start_product(&drawn.ui);
    drawn.panel(3840, 2160, 2.0);
    let crop = |name: &str| {
        let picture = drawn.picture();
        let (x, y, side) = (3100, 340, 620);
        let pixels = (y..y + side).flat_map(|row| (x..x + side).flat_map(move |col| [col, row])).collect::<Vec<u32>>();
        let pixels = pixels.as_chunks::<2>().0.iter().flat_map(|&[col, row]| {
            let [r, g, b] = picture.at(col, row);
            [r, g, b, 255]
        });
        Picture { size: (side, side), pixels: pixels.collect() }.save(&out.join(format!("{name}.png")));
    };
    wear(crate::layout::Layout::Quiet);
    drawn.settings("Sistem");
    drawn.rest(1500);
    for id in EMBLEMS {
        // Slowly: a picture takes a while to keep.
        drawn.ui.global::<crate::QuietMotion>().set_pace(12.0);
        drawn.ui.set_settings_scene((*id).into());
        for (n, ms) in [(0, 1500), (1, 300), (2, 300)] {
            drawn.rest(ms);
            crop(&format!("emblem-{id}-{n}"));
        }
        drawn.rest(9000);
        crop(&format!("emblem-{id}-3"));
        drawn.ui.global::<crate::Spatial>().set_on(false);
        drawn.rest(60);
        crop(&format!("emblem-{id}-slint"));
        drawn.ui.global::<crate::Spatial>().set_on(true);
        drawn.rest(60);
    }
    eprintln!("{:?}", state());
    // Down the column, as it moves: each frame named by its moment.
    const PACE: f32 = 12.0;
    let start = std::time::Instant::now();
    for id in &EMBLEMS[..8] {
        drawn.ui.set_settings_scene((*id).into());
        let until = std::time::Instant::now() + std::time::Duration::from_millis((1100.0 * PACE) as u64);
        while std::time::Instant::now() < until {
            drawn.frame();
            crop(&format!("seq-{:06}", (start.elapsed().as_millis() as f32 / PACE) as u32));
        }
    }
}

/// Pictures of the launcher's marks on the way, as `shots_emblems`.
#[test]
#[ignore]
fn shots_marks() {
    let Some(out) = std::env::var_os("MEDIABOX_TV_SHOTS").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&out).unwrap();
    let drawn = Drawn::open().expect("a GLES device to draw with");
    drawn.panel(3840, 2160, 2.0);
    let display = serde_json::json!({ "applications": [{ "id": "kodi", "name": "KODI", "installed": true }] });
    let tiles: Vec<crate::AppTile> = app(|app| {
        app.home.set_apps(crate::state::app_tiles_from(&serde_json::from_value(display).unwrap()));
        slint::Model::iter(&*app.home.app_tiles).collect()
    });
    let apps = Rc::new(VecModel::from(tiles));
    drawn.home(&apps, 4);
    drawn.rest(1200);
    drawn.ui.global::<crate::QuietMotion>().set_pace(16.0);
    for row in 0..5 {
        drawn.ui.set_focus_col(row);
        for n in 0..6 {
            drawn.rest(900);
            let picture = drawn.picture();
            let (x, y, side) = (270u32, 520 + 166 * row as u32 + 40, 72u32);
            let pixels = (y..y + side).flat_map(|row| (x..x + side).map(move |col| (col, row)));
            let pixels = pixels.flat_map(|(col, row)| { let [r, g, b] = picture.at(col, row); [r, g, b, 255] }).collect();
            Picture { size: (side, side), pixels }.save(&out.join(format!("mark-{row}-{n}.png")));
        }
    }
}

/// The settings' emblems, as the product has them: the GPU's and Slint's on
/// one deck, one at rest, drawn only while it moves, whatever is done to the
/// column; landed by reduced motion; Slint's where the GPU fails; and nothing
/// held of them on another screen.
#[test]
fn the_emblems_change_with_what_is_on_screen() {
    let Some(drawn) = Drawn::open() else { return };
    start_product(&drawn.ui);
    let apps = Rc::new(VecModel::from(launcher(6)));
    drawn.home(&apps, 3);
    drawn.rest(1200);
    assert_eq!((state().live, state().drawn), (0, 0), "nothing of it on the home screen");
    let there = || {
        let deck = drawn.ui.get_quiet_deck();
        [(deck.at0, deck.id0), (deck.at1, deck.id1), (deck.at2, deck.id2)].into_iter().find(|(at, _)| *at == deck.to).unwrap().1.to_string()
    };
    // In from the home screen: the mechanism first, then the category's own.
    drawn.ui.global::<crate::QuietMotion>().set_pace(6.0);
    drawn.settings("Sistem");
    drawn.rest(40);
    assert_eq!((there().as_str(), state().live), ("settings", 0));
    drawn.rest(9000);
    drawn.ui.global::<crate::QuietMotion>().set_pace(1.0);
    assert_eq!((there().as_str(), state().live, state().slots <= 2), ("system", 1, true));
    // At rest: not for a frame something else asked for.
    let before = state().drawn;
    drawn.vitals(true, false, 0);
    drawn.rest(300);
    assert!(!drawn.window.redraw.get() && state().drawn == before);
    // Every category and back, at every speed, turned round half way.
    let groups = ["Medya", "Görüntü ve Ses", "Bağlantılar", "TV ve Kumanda", "Canlı TV", "Cihaz", "Sistem", "Tanılama"];
    for every in [20, 60, 110, 250] {
        for group in groups.iter().chain(groups.iter().rev()).chain(["Cihaz", "Sistem", "Cihaz", "Sistem"].iter()) {
            drawn.settings(group);
            drawn.rest(every);
            assert!(state().live <= 3, "{:?}", state());
        }
    }
    drawn.rest(1200);
    assert_eq!((there().as_str(), state().live, state().failed, state().refused), ("system", 1, 0, 0));
    // Pages: the small emblem, and Slint's own.
    for (name, id, live) in [("cooling", "cooling", 1), ("ethernet", "ethernet", 0), ("cec", "cec", 0), ("audio", "audio", 1), ("output", "output", 1)] {
        family_show(&drawn.ui, name);
        drawn.rest(1500);
        assert_eq!((there().as_str(), state().live), (id, live), "{name}");
        drawn.is_what_it_is(&format!("emblem-page-{name}"));
    }
    // Reduced motion: there, now.
    drawn.ui.global::<crate::QuietMotion>().set_reduced(true);
    drawn.settings("Cihaz");
    drawn.frame();
    drawn.frame();
    assert_eq!((there().as_str(), state().live), ("device", 1));
    let before = state().drawn;
    drawn.rest(200);
    assert_eq!(state().drawn, before, "still moving");
    drawn.ui.global::<crate::QuietMotion>().set_reduced(false);
    // What the GPU cannot draw is there all the same.
    let emblem = |picture: &Picture| {
        let ground = picture.at(1590, 190)[1];
        (1592..1820).flat_map(|x| (196..424).map(move |y| (x, y))).filter(|&(x, y)| picture.at(x, y)[1] > ground.saturating_add(30)).count()
    };
    for what in [Fault::Shader, Fault::Texture, Fault::Framebuffer, Fault::Draw] {
        fault(Some(what));
        crate::spatial::gl::context_made();
        let presented = PRESENTED.get();
        drawn.draw();
        assert_eq!(PRESENTED.get(), presented + 1, "{what:?}");
        assert_eq!(state().live, 0, "{what:?} {:?}", state());
        let seen = emblem(&drawn.read());
        assert!(seen > 300, "{what:?}: {seen}");
        fault(None);
        crate::spatial::gl::context_made();
        drawn.draw();
        assert_eq!(state().live, 1, "{what:?} {:?}", state());
    }
    // Home, half way through the change: the mechanism, and nothing left.
    drawn.settings("Sistem");
    drawn.rest(120);
    drawn.home(&apps, 3);
    drawn.rest(1500);
    assert_eq!(there(), "settings");
    let home = settled(&drawn);
    assert_eq!((home.live, home.slots), (0, 0), "{home:?}");
    wear(crate::layout::Layout::Classic);
    drawn.settings("Sistem");
    assert_eq!(settled(&drawn).live, 0);
    delete_what_was_forgotten();
}

/// The marks end where they began, however fast the focus goes and wherever
/// it is turned round; with no motion they never leave; and the list they
/// stand in is not drawn for them.
#[test]
fn a_mark_at_rest_is_the_mark() {
    let Some(drawn) = Drawn::open() else { return };
    let apps = Rc::new(VecModel::from(launcher(6)));
    drawn.home(&apps, 0);
    drawn.rest(1200);
    let column = |picture: &Picture| {
        let pixels = (268..780).flat_map(|y| (135..180).map(move |x| (x, y)));
        Picture { size: (45, 512), pixels: pixels.flat_map(|(x, y)| { let [r, g, b] = picture.at(x, y); [r, g, b, 255] }).collect() }
    };
    let still: Vec<Picture> = (0..6)
        .map(|row| {
            drawn.ui.set_focus_col(row);
            drawn.rest(1200);
            column(&drawn.picture())
        })
        .collect();
    for every in [10, 40, 90, 200] {
        for row in [0, 1, 2, 3, 4, 5, 4, 3, 2, 3, 2, 1, 2, 1, 0, 5, 0, 2] {
            drawn.ui.set_focus_col(row);
            drawn.rest(every);
        }
        drawn.rest(1200);
        assert_eq!(column(&drawn.picture()).unlike(&still[2], 2), 0, "every {every}");
        drawn.is_what_it_is(&format!("marks-{every}"));
    }
    // It moves, and is drawn over the list, which is not drawn again.
    drawn.ui.global::<crate::QuietMotion>().set_pace(4.0);
    drawn.ui.set_focus_col(3);
    drawn.rest(1000);
    assert!(column(&drawn.picture()).unlike(&still[3], 8) > 20, "the mark does not move");
    for _ in 0..5 {
        let was = PASSES.get();
        drawn.frame();
        assert_eq!(PASSES.get(), was, "a pass for a mark that moves");
    }
    // The same application under another row: nothing moves.
    let rows: Vec<crate::AppTile> = slint::Model::iter(&*apps).collect();
    drawn.rest(4000);
    let mut moved = rows.clone();
    moved.remove(0);
    apps.set_vec(moved);
    drawn.ui.set_focus_col(2);
    drawn.rest(60);
    let there = column(&drawn.picture());
    drawn.rest(1200);
    assert!(there.unlike(&column(&drawn.picture()), 40) == 0, "a mark moved under an application that did not");
    apps.set_vec(rows);
    // Reduced motion.
    drawn.ui.global::<crate::QuietMotion>().set_reduced(true);
    for row in [0, 3, 4, 2] {
        drawn.ui.set_focus_col(row);
        drawn.frame();
        drawn.frame();
        let at_once = column(&drawn.picture());
        drawn.rest(200);
        assert_eq!(at_once.unlike(&column(&drawn.picture()), 2), 0, "row {row}: still moving");
    }
}
