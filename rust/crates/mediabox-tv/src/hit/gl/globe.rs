//! The frames src/globe.rs shows: Tarayıcı's sphere as the product's renderer
//! draws it, as the light it adds to the ground it stands on.
//!
//! Not a check: `tools/globe-offload/make-frames.sh` runs it and encodes what
//! it writes.

use super::*;

mod bake {
    slint::slint! {
        import { Quiet } from "ui/quiet/core.slint";
        import { QuietGround } from "ui/quiet/materials.slint";
        import { GlobeScene } from "ui/quiet/scenes.slint";
        import { Metrics } from "ui/theme.slint";
        export { Metrics }
        export component Bake inherits Window {
            in property <float> turn;
            in property <bool> sphere;
            in property <float> presence;
            QuietGround {
                width: 100%;
                height: 100%;
                aura: Quiet.aura("browser");
            }
            if sphere: GlobeScene {
                x: 1201px * Quiet.u;
                y: 150px * Quiet.u;
                width: 650px * Quiet.u;
                height: 650px * Quiet.u;
                bare: true;
                turn: root.turn;
                tint: Quiet.aura("browser");
                presence: root.presence;
            }
        }
    }
}

#[test]
#[ignore]
fn frames() {
    let out = std::path::PathBuf::from(std::env::var_os("MEDIABOX_TV_GLOBE_OUT").expect("MEDIABOX_TV_GLOBE_OUT"));
    let (panel, scale) = ((2560u32, 1440u32), 2.0f32);
    std::fs::create_dir_all(&out).unwrap();
    let pbuffer = Pbuffer::new().expect("a GLES device to draw with");
    let read = pbuffer.display.get_proc_address(&CString::new("glReadPixels").unwrap());
    let window = Rc::new_cyclic(|weak: &std::rc::Weak<GlWindow>| GlWindow {
        window: slint::Window::new(weak.clone()),
        renderer: FemtoVGRenderer::new(pbuffer).expect("FemtoVG"),
        size: Cell::new(slint::PhysicalSize::new(panel.0, panel.1)),
        redraw: Cell::new(true),
        // SAFETY: glReadPixels, by its own name.
        read: unsafe { std::mem::transmute::<*const c_void, Read>(read) },
    });
    slint::platform::set_platform(Box::new(Screen(window.clone()))).expect("one platform");
    let ui = bake::Bake::new().expect("the scene");
    ui.show().unwrap();
    window.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
    window.window.dispatch_event(WindowEvent::Resized { size: window.size.get().to_logical(scale) });
    let metrics = ui.global::<bake::Metrics>();
    metrics.set_vw(panel.0 as f32 / scale);
    metrics.set_vh(panel.1 as f32 / scale);
    ui.set_presence(crate::globe::PRESENCE);
    let side = crate::globe::drawn_at(panel);
    let rest = crate::globe::pose(0.0, panel).unwrap().dst;
    // The box as it stands at rest, RGB, top row first.
    let shot = || {
        for _ in 0..2 {
            slint::platform::update_timers_and_animations();
            window.renderer.render().expect("a frame");
        }
        let mut p = vec![0u8; (side * side * 4) as usize];
        // SAFETY: the context is current, and the buffer is the size asked for.
        unsafe {
            (window.read)(rest.x, panel.1 as i32 - rest.y - side as i32, side as i32, side as i32, 0x1908, 0x1401, p.as_mut_ptr());
        }
        p.chunks_exact((side * 4) as usize).rev().flat_map(|row| row.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]])).collect::<Vec<u8>>()
    };
    let ground = shot();
    ui.set_sphere(true);
    let mut raw = Vec::new();
    for index in 0..crate::globe::FRAMES {
        ui.set_turn(crate::globe::offset_of(index));
        let frame = shot();
        if index == crate::globe::FRAMES / 2 {
            std::fs::write(out.join("rest-drawn.rgb"), &frame).unwrap();
        }
        raw.extend(frame.iter().zip(&ground).map(|(a, b)| a.saturating_sub(*b)));
    }
    std::fs::write(out.join("ground.rgb"), &ground).unwrap();
    std::fs::write(out.join("frames.rgb"), &raw).unwrap();
    std::fs::write(out.join("side"), side.to_string()).unwrap();
}
