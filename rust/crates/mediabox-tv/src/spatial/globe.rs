//! Level 1: the Tarayıcı scene's globe (ui/quiet/scenes.slint, `GlobeScene`),
//! the same curves and the same motion, with every point worked out on the
//! GPU. The CPU says where the three planes are, how far it has turned and
//! how visible each plane is -- forty-odd numbers a frame -- and draws one
//! instanced strip: an instance is one curve, its parameters never change.

use super::gl::{Drawn, Error, Mesh, MeshData, program};
use super::{Cam, Place};
use glow::{Context, HasContext, NativeProgram};

/// Steps along a curve. The largest is a circle of 305 units: at 4K (1300
/// pixels of box) a chord is 0.08 px off it.
const STEPS: usize = 128;

// Curve kinds, as the vertex shader reads them.
const ELLIPSE: f32 = 0.0;
const CHORD: f32 = 1.0;
const MERIDIAN: f32 = 2.0;
const LINK: f32 = 3.0;
const DISC: f32 = 4.0;
const NODE: f32 = 5.0;
const ARC: f32 = 6.0;

// Planes, line weights and colours, as the uniforms are numbered.
const FAR: f32 = 0.0;
const MAIN: f32 = 1.0;
const NEAR: f32 = 2.0;
const FINE: f32 = 0.0;
const LINE: f32 = 1.0;
const RIM: f32 = 2.0;
const INK: f32 = 0.0;
const GLOW: f32 = 1.0;
const TINT: f32 = 2.0;

const PLACES: [(f32, f32); 5] = [(41.0, 29.0), (52.0, -10.0), (35.0, -60.0), (1.0, 70.0), (-30.0, 20.0)];
const LINKS: [(usize, usize); 6] = [(0, 1), (1, 2), (0, 3), (3, 4), (4, 0), (2, 4)];
/// The near plane's arc: a quadratic from, by and to.
const ARC_POINTS: [(f32, f32); 3] = [(40.0, 520.0), (300.0, 40.0), (640.0, 250.0)];

/// Each curve in the order it is drawn: kind, plane, weight, colour, alpha
/// and four parameters.
fn curves() -> Vec<[f32; 9]> {
    let mut all = vec![
        [ELLIPSE, FAR, FINE, INK, 0.18, 305.0, 120.0, -18f32.to_radians(), 0.0],
        [ELLIPSE, FAR, FINE, INK, 0.18, 275.0, 275.0, 0.0, 0.0],
        [DISC, MAIN, RIM, TINT, 0.04, 190.0, 0.0, 0.0, 0.0],
        [ELLIPSE, MAIN, RIM, GLOW, 0.55, 190.0, 190.0, 0.0, 0.0],
    ];
    for [x0, y0, x1, y1] in [
        [135.0, 325.0, 515.0, 325.0],
        [160.5, 230.0, 489.5, 230.0],
        [160.5, 420.0, 489.5, 420.0],
        [230.0, 160.5, 420.0, 160.5],
        [230.0, 489.5, 420.0, 489.5],
    ] {
        all.push([CHORD, MAIN, RIM, GLOW, 0.55, x0, y0, x1, y1]);
    }
    for side in [1.0, -1.0] {
        for k in 0..6 {
            all.push([MERIDIAN, MAIN, LINE, GLOW, 1.0, k as f32, side, 0.0, 0.0]);
        }
    }
    for (a, b) in LINKS {
        let (a, b) = (PLACES[a], PLACES[b]);
        all.push([LINK, MAIN, FINE, GLOW, 0.7, a.0, a.1, b.0, b.1]);
    }
    for (lat, lon) in PLACES {
        all.push([NODE, MAIN, LINE, GLOW, 1.0, lat, lon, 0.0, 0.0]);
    }
    let [a, b, c] = ARC_POINTS;
    debug_assert!(a.0 < c.0 && b.1 < a.1);
    all.push([ARC, NEAR, LINE, GLOW, 0.2, 0.0, 0.0, 0.0, 0.0]);
    all
}

const VERTEX: &str = r"#version 300 es
precision highp float;
layout(location = 0) in vec2 a_unit;
layout(location = 1) in vec4 a_what;
layout(location = 2) in float a_alpha;
layout(location = 3) in vec4 a_p;
uniform vec3 u_box;
uniform vec2 u_view;
uniform vec4 u_plane[3];
uniform vec3 u_fade;
uniform vec4 u_weight;
uniform vec4 u_colour[3];
uniform float u_spin;
uniform float u_slip;
flat out vec4 v_colour;
flat out vec2 v_half;
out vec2 v_at;
const float PI = 3.14159265;
const float R = 190.0;

vec2 sphere(float lat, float lon) {
    float a = radians(lon + u_spin);
    return vec2(R * cos(radians(lat)) * sin(a), -R * sin(radians(lat)));
}
float face(float lat, float lon) {
    return clamp(cos(radians(lat)) * cos(radians(lon + u_spin)) * 3.0, 0.0, 1.0);
}
// The curve, about the middle of the box.
vec2 curve(float t) {
    if (a_what.x < 0.5) {
        vec2 e = a_p.xy * vec2(cos(t * 2.0 * PI), sin(t * 2.0 * PI));
        float c = cos(a_p.z), s = sin(a_p.z);
        return vec2(e.x * c - e.y * s, e.x * s + e.y * c);
    }
    if (a_what.x < 1.5) return mix(a_p.xy, a_p.zw, t) - 325.0;
    if (a_what.x < 2.5) {
        float phi = radians(mod(a_p.x * 30.0 + u_spin, 180.0));
        float th = (t - 0.5) * PI;
        return R * vec2(a_p.y * sin(phi) * cos(th), sin(th));
    }
    if (a_what.x < 3.5) return mix(sphere(a_p.x, a_p.y), sphere(a_p.z, a_p.w), t);
    vec2 a = vec2(40.0, 520.0), b = vec2(300.0, 40.0), c = vec2(640.0, 250.0);
    return mix(mix(a, b, t), mix(b, c, t), t) - 325.0;
}
void main() {
    int plane = int(a_what.y);
    vec4 at = u_plane[plane];
    float c = cos(at.z), s = sin(at.z);
    mat2 turn = mat2(c, s, -s, c);
    float unit = u_box.z;
    float alpha = a_alpha * u_fade[plane];
    vec2 p;
    bool disc = a_what.x > 3.5 && a_what.x < 5.5;
    // Slint fits a Path's viewbox inside half its stroke all round.
    // And FemtoVG puts it back by half its stroke in logical pixels where it
    // means device ones (Slint 1.17.1, `draw_path`): up and to the left, on a
    // panel that has more than one. The same here, so the picture is the same.
    vec2 slip = vec2(-u_slip * u_weight[int(a_what.z)] / unit);
    float fit = 1.0 - u_weight[int(a_what.z)] / (unit * 650.0);
    if (disc) {
        bool node = a_what.x > 4.5;
        float r = (node ? u_weight.w : a_p.x * unit * fit) * at.x;
        vec2 local = vec2(a_unit.x * 2.0 - 1.0, a_unit.y) * (r + 1.0);
        p = (turn * (node ? sphere(a_p.x, a_p.y) : slip)) * at.x * unit + local;
        v_at = local;
        v_half = vec2(r, 1.0);
        if (node) alpha *= face(a_p.x, a_p.y);
    } else {
        vec2 here = turn * (curve(a_unit.x) * fit + slip);
        vec2 way = turn * (curve(a_unit.x + 0.004) - curve(a_unit.x - 0.004));
        float n = length(way);
        way = n > 1e-5 ? way / n : vec2(0.0, 1.0);
        float half_w = 0.5 * u_weight[int(a_what.z)] * at.x;
        float d = a_unit.y * (half_w + 1.0);
        p = here * at.x * unit + vec2(-way.y, way.x) * d;
        v_at = vec2(d, 0.0);
        v_half = vec2(half_w, 0.0);
        if (a_what.x > 1.5 && a_what.x < 2.5) {
            float phi = radians(mod(a_p.x * 30.0 + u_spin, 180.0));
            alpha *= 0.12 + 0.48 * max(0.0, a_p.y * cos(phi));
        } else if (a_what.x > 2.5 && a_what.x < 3.5) {
            alpha *= min(face(a_p.x, a_p.y), face(a_p.z, a_p.w));
        }
    }
    p += u_box.xy + vec2(325.0, 325.0 + at.y) * unit;
    v_colour = vec4(u_colour[int(a_what.w)].rgb * alpha, alpha);
    // Nothing to see: nothing to rasterise.
    gl_Position = alpha < 0.002 ? vec4(2.0, 2.0, 2.0, 1.0) : vec4(p / u_view * 2.0 - 1.0, 0.0, 1.0);
}
";

// Blended by the shader where it can read the pixel under it: the picture is
// straight alpha as it is made, and no pixel without a line in it is shaded.
// Elsewhere GL blends premultiplied and the pass after it divides.
const FRAGMENT_HEAD: [&str; 2] = [
    "#version 300 es\nprecision highp float;\nout vec4 colour;\nvoid put(vec4 s) { colour = s; }\n",
    "#version 300 es\n#extension GL_EXT_shader_framebuffer_fetch : require\nprecision highp float;\n\
     inout vec4 colour;\nvoid put(vec4 s) {\n\
     float a = s.a + colour.a * (1.0 - s.a);\n\
     colour = a > 0.0 ? vec4((s.rgb + colour.rgb * colour.a * (1.0 - s.a)) / a, a) : vec4(0.0);\n}\n",
];
const FRAGMENT: &str = r"
flat in vec4 v_colour;
flat in vec2 v_half;
in vec2 v_at;
void main() {
    // How much of the pixel the line, or the disc, covers.
    float d = v_half.y > 0.5 ? length(v_at) : abs(v_at.x);
    float cover = max(0.0, min(d + 0.5, v_half.x) - max(d - 0.5, -v_half.x));
    put(v_colour * cover);
}
";

pub struct Globe {
    program: NativeProgram,
    /// Straight alpha as it is drawn.
    pub fetch: bool,
    mesh: Mesh,
    at: [Option<glow::NativeUniformLocation>; 8],
}

/// One plane of the scene, by the camera's offset from it (`Plane`): its
/// scale and how far down it is, in box units.
fn plane(cam: &Cam, d: f32, layer: f32) -> (f32, f32) {
    let spread = (1.0 + (layer - 1.0) * cam.parallax).max(0.0);
    let k = cam.depth * spread;
    let scale = if d >= 0.0 { (1.0 - d * 0.32 * k).max(0.25) } else { 1.0 - d * 0.42 * k };
    (scale, 650.0 * d * 0.34 * spread * cam.travel)
}

/// Far, main, near: scale, drop, turn.
fn planes(cam: &Cam) -> [[f32; 4]; 3] {
    let of = |d: f32, layer: f32, turn: f32| {
        let (scale, drop) = plane(cam, d, layer);
        [scale, drop, turn.to_radians(), 0.0]
    };
    [of(cam.d_far, 0.5, cam.d_far * 18.0), of(cam.d, 1.0, -16.0), of(cam.d_near, 1.7, cam.d_near * 30.0)]
}

/// The part of the panel this scene touches now.
pub fn place(cam: &Cam) -> Place {
    let unit = cam.size / 650.0;
    let [far, main, near] = planes(cam);
    let mut lo = (f32::MAX, f32::MAX);
    let mut hi = (f32::MIN, f32::MIN);
    let mut take = |plane: &[f32; 4], x: f32, y: f32| {
        let (x, y) = (325.0 + x * plane[0], 325.0 + y * plane[0] + plane[1]);
        lo = (lo.0.min(x), lo.1.min(y));
        hi = (hi.0.max(x), hi.1.max(y));
    };
    if cam.f_far > 0.0 {
        let a = (-18.0 + cam.d_far * 18.0f32).to_radians();
        let wide = (305.0 * a.cos()).hypot(120.0 * a.sin()).max(275.0);
        let tall = (305.0 * a.sin()).hypot(120.0 * a.cos()).max(275.0);
        take(&far, -wide, -tall);
        take(&far, wide, tall);
    }
    if cam.f_main > 0.0 {
        take(&main, -194.0, -194.0);
        take(&main, 194.0, 194.0);
    }
    if cam.f_near > 0.0 {
        let (s, c) = near[2].sin_cos();
        let [a, b, e] = ARC_POINTS;
        for i in 0..=16 {
            let t = i as f32 / 16.0;
            let at = |p: f32, q: f32, r: f32| (1.0 - t) * (1.0 - t) * p + 2.0 * t * (1.0 - t) * q + t * t * r - 325.0;
            let (x, y) = (at(a.0, b.0, e.0), at(a.1, b.1, e.1));
            take(&near, x * c - y * s, x * s + y * c);
        }
    }
    if lo.0 >= hi.0 {
        return Place::default();
    }
    // Half a line, the fringe, and what sixteen chords of the arc cut off.
    let pad = 4.0 + 2.0 * unit;
    Place::covering(
        cam,
        [
            cam.x + lo.0 * unit - pad,
            cam.y + lo.1 * unit - pad,
            cam.x + hi.0 * unit + pad,
            cam.y + hi.1 * unit + pad,
        ],
    )
}

impl Globe {
    pub fn new(gl: &Context, fetch: bool) -> Result<Self, Error> {
        let program = program(gl, VERTEX, &format!("{}{FRAGMENT}", FRAGMENT_HEAD[usize::from(fetch)]))?;
        let mut vertices = Vec::new();
        for i in 0..=STEPS {
            for side in [-1.0, 1.0] {
                vertices.extend([i as f32 / STEPS as f32, side]);
            }
        }
        let indices: Vec<u32> = (0..(STEPS as u32 + 1) * 2).collect();
        let instances: Vec<f32> = curves().concat();
        let mesh = Mesh::new(
            gl,
            &MeshData {
                vertices: &vertices,
                vertex: &[2],
                indices: &indices,
                instances: &instances,
                instance: &[4, 1, 4],
                mode: glow::TRIANGLE_STRIP,
            },
        )?;
        let at = ["u_box", "u_view", "u_plane", "u_fade", "u_weight", "u_colour", "u_spin", "u_slip"]
            // SAFETY: the program was just linked on this context.
            .map(|name| unsafe { gl.get_uniform_location(program, name) });
        Ok(Self { program, mesh, at, fetch })
    }

    pub fn draw(&self, gl: &Context, cam: &Cam, place: &Place, drawn: &mut Drawn) {
        let [box_at, view, plane, fade, weight, colour, spin, slip] = &self.at;
        // SAFETY: the context is current and the pass is begun.
        unsafe {
            gl.use_program(Some(self.program));
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            if self.fetch {
                gl.disable(glow::BLEND);
            } else {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            }
            let unit = cam.size / 650.0;
            gl.uniform_3_f32(box_at.as_ref(), cam.x - place.x as f32, cam.y - place.y as f32, unit);
            gl.uniform_2_f32(view.as_ref(), place.w as f32, place.h as f32);
            gl.uniform_4_f32_slice(plane.as_ref(), &planes(cam).concat());
            gl.uniform_3_f32(fade.as_ref(), cam.f_far, cam.f_main, cam.f_near);
            let node = (3.0 * cam.scale).max(8.0 * unit) / 2.0;
            gl.uniform_4_f32(weight.as_ref(), cam.fine, cam.line, cam.line * 1.2, node);
            gl.uniform_4_f32_slice(colour.as_ref(), &[cam.ink, cam.glow, cam.tint].concat());
            gl.uniform_1_f32(slip.as_ref(), 0.5 * (1.0 - 1.0 / cam.scale));
            gl.uniform_1_f32(spin.as_ref(), 30.0 + (cam.d + 3.0) * 70.0);
        }
        drawn.objects += self.mesh.instances as u64;
        self.mesh.draw(gl, drawn);
    }

    pub fn delete(self, gl: &Context) {
        // SAFETY: see `draw`.
        unsafe { gl.delete_program(self.program) };
        self.mesh.delete(gl);
    }
}
