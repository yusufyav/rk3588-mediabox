//! The settings' solid emblems (ui/quiet/emblems.slint): metal and glass under
//! a lens, one small model for each subject that has a body -- a screen's
//! layers, a loudspeaker's rings, a fan, a board, a processor, platters.
//!
//! One pair of shaders draws them all and is made once for the context
//! (`Tools`); a model is its geometry, resident, and where its parts are for a
//! camera (`pose`): a frame hands the GPU half a dozen matrices. Everything a
//! part does is a function of the camera's three offsets, so a model at rest
//! is not drawn again. What runs along a line or lights up does so only on the
//! way; none of it is a reading of the machine.

use std::f32::consts::PI;
use std::rc::Rc;

use super::gl::{Drawn, Error, Gpu, Mesh, MeshData, Owned, program};
use super::math::{self, Mat4, mul, rotate_x, rotate_y, rotate_z, scale, translate};
use super::{Cam, Cost, Pass, Place, Scene};
use glow::{HasContext, NativeProgram, NativeUniformLocation};

/// The most parts a model has.
const PARTS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Model {
    Media,
    Links,
    Remote,
    Live,
    Diagnostics,
    Picture,
    Output,
    Audio,
    Cooling,
    Device,
    System,
    Storage,
}

impl Model {
    pub const ALL: [Self; 12] = [
        Self::Media,
        Self::Links,
        Self::Remote,
        Self::Live,
        Self::Diagnostics,
        Self::Picture,
        Self::Output,
        Self::Audio,
        Self::Cooling,
        Self::Device,
        Self::System,
        Self::Storage,
    ];

    /// The scene it is drawn for: a category's id or a page's
    /// (src/screens/settings.rs), never a name on the screen.
    pub fn id(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Links => "links",
            Self::Remote => "remote",
            Self::Live => "live",
            Self::Diagnostics => "diagnostics",
            Self::Picture => "picture",
            Self::Output => "output",
            Self::Audio => "audio",
            Self::Cooling => "cooling",
            Self::Device => "device",
            Self::System => "system",
            Self::Storage => "storage",
        }
    }
}

const SOLID_VERTEX: &str = r"#version 300 es
precision highp float;
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_normal;
layout(location = 2) in vec4 a_at;
layout(location = 3) in vec4 a_size;
layout(location = 4) in vec4 a_look;
uniform mat4 u_vp;
uniform mat4 u_part[6];
uniform vec2 u_lit[6];
out vec3 v_normal;
out vec3 v_world;
flat out vec4 v_look;
void main() {
    int part = int(a_at.w);
    float c = cos(a_size.w), s = sin(a_size.w);
    mat3 turn = mat3(c, s, 0.0, -s, c, 0.0, 0.0, 0.0, 1.0);
    vec4 world = u_part[part] * vec4(turn * (a_pos * a_size.xyz) + a_at.xyz, 1.0);
    v_normal = mat3(u_part[part]) * (turn * (a_normal / a_size.xyz));
    v_world = world.xyz;
    // shade, opacity, light of its own; what is seen of it.
    v_look = vec4(a_look.xy, a_look.z * (0.45 + u_lit[part].y), u_lit[part].x);
    gl_Position = u_vp * world;
}
";

const SOLID_FRAGMENT: &str = r"#version 300 es
precision highp float;
in vec3 v_normal;
in vec3 v_world;
flat in vec4 v_look;
uniform vec3 u_eye;
uniform vec3 u_tint;
uniform vec3 u_glow;
out vec4 colour;
void main() {
    vec3 e = normalize(u_eye - v_world), n = normalize(v_normal);
    if (dot(n, e) < 0.0) n = -n;
    vec3 key = normalize(vec3(-0.55, 0.75, 0.6)), fill = normalize(vec3(0.8, -0.1, 0.35));
    float facing = dot(n, e);
    float rim = pow(1.0 - facing, 3.0);
    float lit = max(dot(n, key), 0.0) + 0.3 * max(dot(n, fill), 0.0);
    float shine = pow(max(dot(n, normalize(key + e)), 0.0), 36.0);
    float glint = pow(max(dot(n, normalize(fill + e)), 0.0), 24.0);
    // Dark slate, and the metal's light on it.
    vec3 body = mix(vec3(0.035, 0.049, 0.064), u_tint, v_look.x * (0.10 + 0.42 * lit));
    vec3 light = u_glow * (shine * 0.45 + glint * 0.16 + rim * 0.22) + u_glow * v_look.z;
    float a = min(1.0, v_look.y + (1.0 - v_look.y) * (rim * 0.5 + shine * 0.3));
    colour = vec4(body * a + light * mix(0.55, 1.0, a), a) * v_look.w;
}
";

const LINE_VERTEX: &str = r"#version 300 es
precision highp float;
layout(location = 0) in vec2 a_unit;
layout(location = 1) in vec4 a_before;
layout(location = 2) in vec4 a_from;
layout(location = 3) in vec4 a_to;
layout(location = 4) in vec4 a_after;
layout(location = 5) in vec2 a_run;
uniform mat4 u_vp;
uniform mat4 u_part[6];
uniform vec2 u_lit[6];
uniform vec2 u_view;
uniform vec2 u_weight;
uniform float u_far;
uniform vec3 u_colour[2];
flat out vec4 v_colour;
flat out float v_half;
out float v_run;
out float v_at;
vec4 clip(vec3 p, int part) { return u_vp * (u_part[part] * vec4(p, 1.0)); }
vec2 pixel(vec4 c) { return (c.xy / c.w * 0.5 + 0.5) * u_view; }
vec2 across(vec2 a, vec2 b) {
    vec2 d = b - a;
    float n = length(d);
    return n > 1e-3 ? vec2(-d.y, d.x) / n : vec2(0.0);
}
void main() {
    int part = int(a_before.w);
    vec4 here = clip(a_unit.x < 0.5 ? a_from.xyz : a_to.xyz, part);
    vec2 from = pixel(clip(a_from.xyz, part)), to = pixel(clip(a_to.xyz, part));
    vec2 side = across(from, to);
    // Mitred with its neighbour: a line of segments nowhere covers itself.
    vec2 other = a_unit.x < 0.5 ? across(pixel(clip(a_before.xyz, part)), from) : across(to, pixel(clip(a_after.xyz, part)));
    vec2 sum = side + (dot(other, other) > 0.0 ? other : side);
    float n = length(sum);
    vec2 mitre = n > 0.2 ? sum / n : side;
    float stretch = min(1.0 / max(dot(mitre, side), 0.3), 2.0);
    v_half = 0.5 * (a_from.w < 0.5 ? u_weight.x : u_weight.y);
    v_at = a_unit.y * (v_half + 1.0);
    vec2 p = pixel(here) + mitre * stretch * v_at;
    // Dimmer the further off.
    float near = clamp(1.0 - 0.32 * (here.w - u_far), 0.35, 1.0);
    float alpha = a_to.w * near * u_lit[part].x;
    v_colour = vec4(u_colour[int(a_after.w)], alpha);
    v_run = mix(a_run.x, a_run.y, a_unit.x);
    gl_Position = vec4(p / u_view * 2.0 - 1.0, here.z / here.w - 0.0015, 1.0);
}
";

const LINE_FRAGMENT: &str = r"#version 300 es
precision highp float;
flat in vec4 v_colour;
flat in float v_half;
in float v_run;
in float v_at;
uniform vec2 u_pulse;
uniform vec3 u_light;
out vec4 colour;
void main() {
    float d = abs(v_at);
    float cover = max(0.0, min(d + 0.5, v_half) - max(d - 0.5, -v_half));
    float on = v_run < 0.0 ? 0.0 : u_pulse.y * exp(-pow((v_run - u_pulse.x) * 5.0, 2.0));
    float a = min(1.0, v_colour.a * (1.0 + 2.5 * on)) * cover;
    colour = vec4(mix(v_colour.rgb, u_light, min(1.0, on)) * a, a);
}
";

/// The shaders every model is drawn with.
pub struct Tools {
    solid: Owned<NativeProgram>,
    solid_at: [Option<NativeUniformLocation>; 6],
    line: Owned<NativeProgram>,
    line_at: [Option<NativeUniformLocation>; 9],
}

impl Tools {
    pub fn new(gpu: &Rc<Gpu>) -> Result<Self, Error> {
        let solid = program(gpu, SOLID_VERTEX, SOLID_FRAGMENT)?;
        let line = program(gpu, LINE_VERTEX, LINE_FRAGMENT)?;
        // SAFETY: programs just linked on this context.
        let at = |program: &Owned<NativeProgram>, name: &str| unsafe { gpu.gl.get_uniform_location(program.name, name) };
        Ok(Self {
            solid_at: ["u_vp", "u_part", "u_lit", "u_eye", "u_tint", "u_glow"].map(|name| at(&solid, name)),
            line_at: ["u_vp", "u_part", "u_lit", "u_view", "u_weight", "u_far", "u_colour", "u_pulse", "u_light"]
                .map(|name| at(&line, name)),
            solid,
            line,
        })
    }
}

// ------------------------------------------------------------- geometry

/// Positions with their normals, and the triangles between them.
type Shape = (Vec<f32>, Vec<u32>);

/// A grid of `across` by `along` quads over `at(u, v)`, its normals the
/// surface's own.
fn surface(across: u32, along: u32, at: impl Fn(f32, f32) -> [f32; 3]) -> Shape {
    let (mut v, mut i) = (Vec::new(), Vec::new());
    for y in 0..=along {
        for x in 0..=across {
            let (s, t) = (x as f32 / across as f32, y as f32 / along as f32);
            let e = 1e-3;
            let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
            let (p, q) = (sub(at(s + e, t), at(s - e, t)), sub(at(s, t + e), at(s, t - e)));
            let n = [p[1] * q[2] - p[2] * q[1], p[2] * q[0] - p[0] * q[2], p[0] * q[1] - p[1] * q[0]];
            let long = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-9);
            v.extend(at(s, t));
            v.extend(n.map(|n| n / long));
        }
    }
    for y in 0..along {
        for x in 0..across {
            let a = y * (across + 1) + x;
            let b = a + across + 1;
            i.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    (v, i)
}

/// A cube of half-side 1, its faces flat.
fn cube() -> Shape {
    let (mut v, mut i) = (Vec::new(), Vec::new());
    for axis in 0..3 {
        for side in [-1.0f32, 1.0] {
            let first = v.len() as u32 / 6;
            for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let mut p = [0.0; 3];
                (p[axis], p[(axis + 1) % 3], p[(axis + 2) % 3]) = (side, a, b);
                let mut n = [0.0; 3];
                n[axis] = side;
                v.extend(p);
                v.extend(n);
            }
            i.extend([0, 1, 2, 0, 2, 3].map(|k| first + k));
        }
    }
    (v, i)
}

/// A cylinder of radius 1 about z, from -1 to 1, closed.
fn drum() -> Shape {
    let round = 64;
    let turn = |s: f32| (2.0 * PI * s).sin_cos();
    let (mut v, mut i) = surface(round, 1, |s, t| [turn(s).1, turn(s).0, 2.0 * t - 1.0]);
    for end in [-1.0f32, 1.0] {
        let first = v.len() as u32 / 6;
        v.extend([0.0, 0.0, end, 0.0, 0.0, end]);
        for k in 0..=round {
            let (s, c) = turn(k as f32 / round as f32);
            v.extend([c, s, end, 0.0, 0.0, end]);
        }
        i.extend((0..round).flat_map(|k| [first, first + 1 + k, first + 2 + k]));
    }
    (v, i)
}

/// A ring of radius 1 about z, `tube` thick.
fn torus(tube: f32) -> Shape {
    surface(80, 10, |s, t| {
        let ((a, b), (c, d)) = ((2.0 * PI * s).sin_cos(), (2.0 * PI * t).sin_cos());
        [b * (1.0 + tube * d), a * (1.0 + tube * d), tube * c]
    })
}

/// The front half of a ball.
fn dome() -> Shape {
    surface(32, 8, |s, t| {
        let ((a, b), (c, d)) = ((2.0 * PI * s).sin_cos(), (0.5 * PI * t.max(0.001)).sin_cos());
        [b * c, a * c, d]
    })
}

/// A ball of radius 1.
fn ball() -> Shape {
    surface(24, 14, |s, t| {
        let ((a, b), (c, d)) = ((2.0 * PI * s).sin_cos(), (PI * t.clamp(0.002, 0.998)).sin_cos());
        [b * c, a * c, d]
    })
}

/// A dish looking along z.
fn dish() -> Shape {
    surface(48, 8, |s, t| {
        let ((a, b), r) = ((2.0 * PI * s).sin_cos(), t.max(0.002));
        [r * b, r * a, 0.42 * r * r]
    })
}

/// Where a fan's blade is: from the hub (u 0) to the tip, across its width.
fn blade_at(u: f32, v: f32) -> [f32; 3] {
    let r = 0.24 + 0.66 * u;
    let a = -0.5 * u * u + (v - 0.5) * (0.5 + 0.1 * u);
    [r * a.cos(), r * a.sin(), -(v - 0.5) * 0.34 * (1.0 - 0.3 * u)]
}

/// A model as it is put together.
#[derive(Default)]
struct Build {
    /// Opaque, and glass, back to front.
    solid: Vec<(Shape, Vec<f32>)>,
    glass: Vec<(Shape, Vec<f32>)>,
    lines: Vec<f32>,
}

/// How a line is drawn: its plane's weight, its colour and how strong.
#[derive(Clone, Copy)]
struct Pen {
    part: usize,
    heavy: bool,
    glow: bool,
    alpha: f32,
    /// A pulse runs along it.
    run: bool,
}

impl Pen {
    fn fine(part: usize, alpha: f32) -> Self {
        Self { part, heavy: false, glow: false, alpha, run: false }
    }
    fn line(part: usize, alpha: f32) -> Self {
        Self { part, heavy: true, glow: true, alpha, run: false }
    }
}

/// One thing of a shape: where, how large, turned about z.
fn one(part: usize, at: [f32; 3], size: [f32; 3], turn: f32, look: [f32; 3]) -> [f32; 12] {
    [at[0], at[1], at[2], part as f32, size[0], size[1], size[2], turn, look[0], look[1], look[2], 0.0]
}

const METAL: [f32; 3] = [0.8, 1.0, 0.0];
const DARK: [f32; 3] = [0.35, 1.0, 0.0];
const GLASS: [f32; 3] = [0.6, 0.12, 0.0];

impl Build {
    fn solid(&mut self, shape: Shape, all: &[[f32; 12]]) {
        self.solid.push((shape, all.concat()));
    }

    fn glass(&mut self, shape: Shape, all: &[[f32; 12]]) {
        self.glass.push((shape, all.concat()));
    }

    fn line(&mut self, pen: Pen, points: &[[f32; 3]]) {
        let closed = points.len() > 2 && points.first() == points.last();
        let last = points.len() - 1;
        for k in 0..last {
            let before = if k > 0 { points[k - 1] } else if closed { points[last - 1] } else { points[0] };
            let after = if k + 1 < last { points[k + 2] } else if closed { points[1] } else { points[last] };
            let (from, to) = (points[k], points[k + 1]);
            for (p, w) in [
                (before, pen.part as f32),
                (from, f32::from(u8::from(pen.heavy))),
                (to, pen.alpha),
                (after, f32::from(u8::from(pen.glow))),
            ] {
                self.lines.extend([p[0], p[1], p[2], w]);
            }
            let run = |k: usize| if pen.run { k as f32 / last as f32 } else { -1.0 };
            self.lines.extend([run(k), run(k + 1)]);
        }
    }

    /// Part of a circle about z.
    fn arc(&mut self, pen: Pen, r: f32, z: f32, from: f32, to: f32) {
        let steps = ((to - from).abs() / (2.0 * PI) * 72.0).ceil().max(2.0) as usize;
        let mut points: Vec<[f32; 3]> = (0..=steps)
            .map(|k| {
                let (s, c) = (from + (to - from) * k as f32 / steps as f32).sin_cos();
                [r * c, r * s, z]
            })
            .collect();
        if (to - from).abs() > 2.0 * PI - 1e-3 {
            points[steps] = points[0];
        }
        self.line(pen, &points);
    }

    fn circle(&mut self, pen: Pen, r: f32, z: f32) {
        self.arc(pen, r, z, 0.0, 2.0 * PI);
    }

    /// A rectangle facing z.
    fn frame(&mut self, pen: Pen, at: [f32; 3], half: [f32; 2]) {
        let [x, y, z] = at;
        let corner = |a: f32, b: f32| [x + a * half[0], y + b * half[1], z];
        self.line(pen, &[corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0), corner(-1.0, -1.0)]);
    }

    /// The same lying down: facing y.
    fn floor(&mut self, pen: Pen, at: [f32; 3], half: [f32; 2]) {
        let [x, y, z] = at;
        let corner = |a: f32, b: f32| [x + a * half[0], y, z + b * half[1]];
        self.line(pen, &[corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0), corner(-1.0, -1.0)]);
    }
}

fn build(model: Model) -> Build {
    let mut b = Build::default();
    match model {
        // Two reels and the film between them.
        Model::Media => {
            for (part, r) in [(0, 0.4f32), (1, 0.72)] {
                b.solid(torus(0.05), &[one(part, [0.0; 3], [r, r, r * 1.4], 0.0, METAL)]);
                b.solid(
                    drum(),
                    &[
                        one(part, [0.0, 0.0, -0.05 * r], [0.62 * r, 0.62 * r, 0.07 * r], 0.0, DARK),
                        one(part, [0.0, 0.0, 0.05 * r], [0.2 * r, 0.2 * r, 0.09 * r], 0.0, METAL),
                    ],
                );
                let spoke = |k: usize| 2.0 * PI * k as f32 / 5.0;
                let spokes: Vec<[f32; 12]> = (0..5)
                    .map(|k| {
                        let (s, c) = spoke(k).sin_cos();
                        one(part, [0.56 * r * c, 0.56 * r * s, 0.05 * r], [0.4 * r, 0.05 * r, 0.02 * r], spoke(k), METAL)
                    })
                    .collect();
                b.solid(cube(), &spokes);
                for k in 0..5 {
                    let (s, c) = (spoke(k) + PI / 5.0).sin_cos();
                    let (x, y) = (0.64 * r * c, 0.64 * r * s);
                    let points: Vec<[f32; 3]> = (0..=24)
                        .map(|n| {
                            let (s, c) = (2.0 * PI * (n % 24) as f32 / 24.0).sin_cos();
                            [x + 0.24 * r * c, y + 0.24 * r * s, 0.05 * r]
                        })
                        .collect();
                    b.line(Pen::fine(part, 0.7), &points);
                }
                b.circle(Pen::line(part, 0.75), r * 1.04, 0.05 * r);
                b.circle(Pen::fine(part, 0.5), 0.2 * r, 0.14 * r);
            }
            // Off the large one's foot and away, with its frames.
            let film = |t: f32, side: f32| {
                let a = -2.2 + 2.3 * t;
                let r = 0.92 + 0.2 * t + 0.07 * side;
                [-0.22 + r * a.cos(), -0.2 + r * a.sin(), 0.18]
            };
            for side in [-1.0, 1.0] {
                let edge: Vec<[f32; 3]> = (0..=40).map(|k| film(k as f32 / 40.0, side)).collect();
                b.line(Pen { run: true, ..Pen::line(2, 0.6) }, &edge);
            }
            for k in 0..=16 {
                let t = k as f32 / 16.0;
                b.line(Pen::fine(2, 0.5), &[film(t, -1.0), film(t, 1.0)]);
            }
        }
        // Nodes about a hub, and the ways between them.
        Model::Links => {
            let nodes = [[-0.82, 0.5, 0.1], [0.62, 0.7, -0.35], [0.9, -0.28, 0.25], [-0.1, -0.86, 0.35], [-0.86, -0.42, -0.4], [0.1, 0.35, 0.85]];
            let mut balls = vec![one(1, [0.0; 3], [0.2; 3], 0.0, METAL)];
            balls.extend(nodes.iter().enumerate().map(|(k, &at)| one(1, at, [0.085 + 0.02 * (k % 2) as f32; 3], 0.0, METAL)));
            b.solid(ball(), &balls);
            let way = Pen { run: true, ..Pen::line(1, 0.55) };
            let between = |a: [f32; 3], b: [f32; 3]| -> Vec<[f32; 3]> {
                (0..=8).map(|k| k as f32 / 8.0).map(|t| std::array::from_fn(|n| a[n] + (b[n] - a[n]) * t)).collect()
            };
            for at in nodes {
                b.line(way, &between([0.0; 3], at));
            }
            for (p, q) in [(0, 1), (1, 5), (2, 3), (3, 4), (4, 0), (5, 2)] {
                b.line(Pen { glow: false, alpha: 0.4, heavy: false, ..way }, &between(nodes[p], nodes[q]));
            }
            b.circle(Pen::fine(0, 0.5), 1.15, 0.0);
            b.circle(Pen::fine(2, 0.5), 1.02, 0.0);
            b.arc(Pen::line(2, 0.7), 1.02, 0.0, 0.3, 1.2);
        }
        // A remote, the television it is aimed at, and what goes between.
        Model::Remote => {
            b.solid(cube(), &[one(1, [0.0; 3], [0.21, 0.66, 0.055], 0.0, DARK)]);
            b.frame(Pen::line(1, 0.85), [0.0, 0.0, 0.056], [0.21, 0.66]);
            b.solid(torus(0.16), &[one(1, [0.0, 0.34, 0.06], [0.12; 3], 0.0, METAL)]);
            let keys: Vec<[f32; 12]> = (0..10)
                .map(|k| one(1, [if k % 2 == 0 { -0.085 } else { 0.085 }, 0.06 - 0.13 * (k / 2) as f32, 0.06], [0.04, 0.04, 0.02], 0.0, METAL))
                .chain([one(1, [0.0, 0.34, 0.06], [0.05, 0.05, 0.025], 0.0, METAL)])
                .collect();
            b.solid(drum(), &keys);
            b.solid(cube(), &[one(0, [0.0; 3], [0.66, 0.4, 0.02], 0.0, DARK)]);
            b.frame(Pen::line(0, 0.75), [0.0, 0.0, 0.021], [0.66, 0.4]);
            b.frame(Pen::fine(0, 0.4), [0.0, 0.0, 0.021], [0.6, 0.34]);
            b.line(Pen::fine(0, 0.6), &[[-0.2, -0.52, 0.0], [0.2, -0.52, 0.0]]);
            b.line(Pen::fine(0, 0.6), &[[0.0, -0.52, 0.0], [0.0, -0.4, 0.0]]);
            for (part, alpha) in [(2, 0.9), (3, 0.65), (4, 0.45)] {
                b.arc(Pen::line(part, alpha), 1.0, 0.0, 0.1, 1.35);
            }
        }
        // A satellite on its orbit, and the dish under it.
        Model::Live => {
            b.solid(cube(), &[one(1, [0.0; 3], [0.15, 0.17, 0.15], 0.0, METAL)]);
            b.solid(
                cube(),
                &[-1.0f32, 1.0].map(|side| one(1, [0.52 * side, 0.0, 0.0], [0.28, 0.15, 0.01], 0.0, [0.5, 1.0, 0.0])),
            );
            b.solid(drum(), &[one(1, [0.0, 0.0, 0.17], [0.09, 0.09, 0.02], 0.0, METAL)]);
            for side in [-1.0f32, 1.0] {
                b.frame(Pen::line(1, 0.8), [0.52 * side, 0.0, 0.011], [0.28, 0.15]);
                for k in 1..4 {
                    let x = side * (0.24 + 0.14 * k as f32);
                    b.line(Pen::fine(1, 0.6), &[[x, -0.15, 0.012], [x, 0.15, 0.012]]);
                }
                b.line(Pen::fine(1, 0.6), &[[side * 0.24, 0.0, 0.012], [side * 0.8, 0.0, 0.012]]);
                b.line(Pen::line(1, 0.6), &[[side * 0.15, 0.0, 0.0], [side * 0.24, 0.0, 0.0]]);
            }
            b.circle(Pen::fine(0, 0.65), 1.2, 0.0);
            b.circle(Pen::fine(0, 0.35), 1.06, 0.0);
            b.solid(dish(), &[one(3, [0.0; 3], [0.4, 0.4, 0.4], 0.0, METAL)]);
            b.circle(Pen::line(3, 0.9), 0.4, 0.168);
            for a in [0.5f32, 2.6, 4.7] {
                b.line(Pen::fine(3, 0.7), &[[0.4 * a.cos(), 0.4 * a.sin(), 0.168], [0.0, 0.0, 0.4]]);
            }
            b.solid(ball(), &[one(3, [0.0, 0.0, 0.4], [0.04; 3], 0.0, METAL)]);
            b.solid(cube(), &[one(4, [0.0, -0.22, 0.0], [0.025, 0.22, 0.025], 0.0, METAL), one(4, [0.0, -0.44, 0.0], [0.2, 0.015, 0.2], 0.0, DARK)]);
            // Down, and across to the feed.
            let (from, to) = ([0.3, 0.42, 0.12], [-0.46, -0.3, 0.3]);
            for side in [-0.06, 0.0, 0.06] {
                let points: Vec<[f32; 3]> = (0..=8)
                    .map(|k| {
                        let t = k as f32 / 8.0;
                        [from[0] + (to[0] - from[0]) * t + side * t, from[1] + (to[1] - from[1]) * t - side * t, from[2] + (to[2] - from[2]) * t]
                    })
                    .collect();
                b.line(Pen { run: true, ..Pen::line(5, 0.3) }, &points);
            }
        }
        // A lens over a lattice.
        Model::Diagnostics => {
            b.solid(torus(0.075), &[one(1, [0.0; 3], [0.56; 3], 0.0, METAL)]);
            b.glass(drum(), &[one(1, [0.0; 3], [0.54, 0.54, 0.015], 0.0, [0.9, 0.1, 0.0])]);
            b.solid(
                cube(),
                &[
                    one(1, [0.52, -0.52, 0.0], [0.15, 0.03, 0.03], -PI / 4.0, METAL),
                    one(1, [0.82, -0.82, 0.0], [0.3, 0.07, 0.06], -PI / 4.0, DARK),
                ],
            );
            b.circle(Pen::line(1, 0.8), 0.6, 0.0);
            for k in 0..9 {
                let at = -1.0 + 0.25 * k as f32;
                b.line(Pen { run: true, ..Pen::fine(0, 0.34) }, &[[at, -1.0, 0.0], [at, -0.3, 0.0], [at, 0.3, 0.0], [at, 1.0, 0.0]]);
                b.line(Pen { run: true, ..Pen::fine(0, 0.34) }, &[[-1.0, at, 0.0], [-0.3, at, 0.0], [0.3, at, 0.0], [1.0, at, 0.0]]);
            }
            let chips: Vec<[f32; 12]> = [(-0.5, 0.5), (0.5, 0.25), (-0.25, -0.5), (0.25, -0.75), (0.75, 0.75), (-0.75, -0.25)]
                .map(|(x, y)| one(0, [x, y, 0.02], [0.1, 0.1, 0.03], 0.0, METAL))
                .to_vec();
            b.solid(cube(), &chips);
            for (from, r) in [(0.2, 0.4), (PI + 0.2, 0.4), (1.8, 0.26), (PI + 1.8, 0.26)] {
                b.arc(Pen::line(2, 0.7), r, 0.03, from, from + 1.2);
            }
            b.line(Pen::fine(2, 0.8), &[[-0.07, 0.0, 0.03], [0.07, 0.0, 0.03]]);
            b.line(Pen::fine(2, 0.8), &[[0.0, -0.07, 0.03], [0.0, 0.07, 0.03]]);
            for (x, y) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                b.line(Pen::line(3, 0.6), &[[1.08 * x, 0.82 * y, 0.0], [1.08 * x, 1.08 * y, 0.0], [0.82 * x, 1.08 * y, 0.0]]);
            }
        }
        // The screen and what is behind it; the sound that leaves it.
        Model::Picture => {
            b.solid(cube(), &[one(1, [0.0; 3], [0.92, 0.55, 0.025], 0.0, DARK)]);
            b.glass(cube(), &[one(0, [0.0; 3], [0.84, 0.5, 0.01], 0.0, GLASS)]);
            b.frame(Pen::fine(0, 0.5), [0.0; 3], [0.84, 0.5]);
            for k in 1..6 {
                let x = -0.84 + 0.28 * k as f32;
                b.line(Pen::fine(0, 0.2), &[[x, -0.5, 0.0], [x, 0.5, 0.0]]);
            }
            b.frame(Pen::line(1, 0.85), [0.0, 0.0, 0.026], [0.92, 0.55]);
            b.frame(Pen::fine(1, 0.4), [0.0, 0.0, 0.026], [0.84, 0.47]);
            b.line(Pen::line(1, 0.6), &[[-0.3, -0.78, 0.0], [0.3, -0.78, 0.0]]);
            b.line(Pen::line(1, 0.6), &[[0.0, -0.78, 0.0], [0.0, -0.55, 0.0]]);
            for (part, r, alpha) in [(2, 0.2, 0.85), (3, 0.4, 0.65), (4, 0.6, 0.45)] {
                b.circle(Pen::line(part, alpha), r, 0.0);
            }
        }
        // An optical stack: the lattice, the filter, the front; light through.
        Model::Output => {
            b.solid(cube(), &[one(0, [0.0; 3], [0.86, 0.52, 0.015], 0.0, DARK)]);
            b.glass(cube(), &[one(1, [0.0; 3], [0.86, 0.52, 0.01], 0.0, [0.7, 0.2, 0.0]), one(2, [0.0; 3], [0.86, 0.52, 0.008], 0.0, GLASS)]);
            for k in 1..10 {
                let x = -0.86 + 0.172 * k as f32;
                b.line(Pen::fine(0, 0.42), &[[x, -0.52, 0.02], [x, 0.52, 0.02]]);
            }
            for k in 1..6 {
                let y = -0.52 + 0.1733 * k as f32;
                b.line(Pen::fine(0, 0.42), &[[-0.86, y, 0.02], [0.86, y, 0.02]]);
            }
            b.frame(Pen::line(0, 0.5), [0.0, 0.0, 0.02], [0.86, 0.52]);
            b.frame(Pen::line(1, 0.7), [0.0; 3], [0.86, 0.52]);
            b.frame(Pen::line(2, 0.9), [0.0; 3], [0.86, 0.52]);
            for (x, y) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                b.line(
                    Pen::fine(2, 0.8),
                    &[[0.74 * x, 0.46 * y, 0.0], [0.8 * x, 0.46 * y, 0.0], [0.8 * x, 0.4 * y, 0.0]],
                );
            }
            let beam = |alpha| Pen { run: true, ..Pen::line(3, alpha) };
            b.line(beam(0.5), &[[0.0, 0.0, -1.1], [0.0, 0.0, -0.55], [0.0, 0.0, 0.0]]);
            for x in [-0.34, 0.0, 0.34] {
                b.line(beam(0.4), &[[0.0, 0.0, 0.0], [x * 0.5, x * 0.2, 0.55], [x, x * 0.4, 1.1]]);
            }
        }
        // A loudspeaker: rings on a cone, and the wave that goes through them.
        Model::Audio => {
            for k in 0..5 {
                let r = 0.28 + 0.19 * k as f32;
                b.solid(torus(0.034 - 0.003 * k as f32), &[one(k, [0.0; 3], [r, r, r], 0.0, METAL)]);
                if k > 0 {
                    b.circle(Pen::fine(k, 0.35), r - 0.095, -0.05);
                }
            }
            b.solid(dome(), &[one(0, [0.0, 0.0, -0.02], [0.2, 0.2, 0.12], 0.0, METAL)]);
            b.circle(Pen::line(5, 0.7), 1.14, 0.0);
            for k in 0..24 {
                let (s, c) = (2.0 * PI * k as f32 / 24.0).sin_cos();
                let far = if k % 6 == 0 { 1.26 } else { 1.21 };
                b.line(Pen::fine(5, 0.5), &[[1.17 * c, 1.17 * s, 0.0], [far * c, far * s, 0.0]]);
            }
        }
        // A fan in its frame.
        Model::Cooling => {
            b.solid(torus(0.055), &[one(0, [0.0; 3], [1.0, 1.0, 1.6], 0.0, METAL)]);
            b.solid(drum(), &[one(1, [0.0; 3], [0.27, 0.27, 0.16], 0.0, METAL), one(1, [0.0, 0.0, 0.16], [0.17, 0.17, 0.03], 0.0, METAL)]);
            let turns = (0..9).map(|k| 2.0 * PI * k as f32 / 9.0);
            let blades: Vec<[f32; 12]> = turns.clone().map(|turn| one(1, [0.0; 3], [1.0; 3], turn, [0.75, 1.0, 0.0])).collect();
            b.solid(surface(10, 6, blade_at), &blades);
            for turn in turns {
                let (s, c) = turn.sin_cos();
                let at = |u: f32, v: f32| {
                    let [x, y, z] = blade_at(u, v);
                    [x * c - y * s, x * s + y * c, z]
                };
                let mut edge: Vec<[f32; 3]> = (0..=8).map(|k| at(k as f32 / 8.0, 1.0)).collect();
                edge.extend((0..=3).rev().map(|k| at(1.0, k as f32 / 4.0)));
                b.line(Pen::line(1, 0.55), &edge);
            }
            let strut: Vec<[f32; 12]> = (0..4)
                .map(|k| {
                    let turn = PI / 4.0 + PI / 2.0 * k as f32;
                    let (s, c) = turn.sin_cos();
                    one(2, [0.62 * c, 0.62 * s, -0.2], [0.4, 0.035, 0.02], turn, DARK)
                })
                .collect();
            b.solid(cube(), &strut);
            b.circle(Pen::line(1, 0.6), 0.27, 0.16);
            for (part, z) in [(2, -0.18), (3, 0.18)] {
                let (r, side) = (0.16, 1.12f32);
                let mut points = Vec::new();
                for (q, (x, y)) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)].into_iter().enumerate() {
                    for k in 0..=6 {
                        let (s, c) = (PI / 2.0 * (q as f32 + k as f32 / 6.0)).sin_cos();
                        points.push([x * (side - r) + r * c, y * (side - r) + r * s, z]);
                    }
                }
                points.push(points[0]);
                b.line(Pen { glow: part == 3, ..Pen::line(part, if part == 3 { 0.7 } else { 0.45 }) }, &points);
            }
        }
        // A board: its heat sink, its chips, its ports, its lights.
        Model::Device => {
            b.solid(
                cube(),
                &[
                    one(0, [0.0, -0.22, 0.0], [1.02, 0.035, 0.74], 0.0, DARK),
                    one(0, [-0.6, -0.16, -0.34], [0.2, 0.03, 0.16], 0.0, METAL),
                    one(0, [-0.62, -0.16, 0.22], [0.14, 0.03, 0.14], 0.0, METAL),
                    one(0, [0.86, -0.07, 0.32], [0.16, 0.11, 0.17], 0.0, METAL),
                    one(0, [0.86, -0.1, -0.2], [0.16, 0.08, 0.22], 0.0, METAL),
                ],
            );
            let mut sink = vec![one(1, [0.1, -0.12, -0.14], [0.46, 0.045, 0.42], 0.0, METAL)];
            sink.extend((0..9).map(|k| one(1, [-0.32 + 0.105 * k as f32, 0.07, -0.14], [0.018, 0.16, 0.42], 0.0, METAL)));
            b.solid(cube(), &sink);
            let lights: Vec<[f32; 12]> =
                (0..3).map(|k| one(2 + k, [-0.28 + 0.2 * k as f32, -0.15, 0.56], [0.045; 3], 0.0, [0.9, 1.0, 0.9])).collect();
            b.solid(drum(), &lights);
            b.floor(Pen::line(0, 0.7), [0.0, -0.184, 0.0], [1.02, 0.74]);
            b.floor(Pen::line(1, 0.55), [0.1, -0.074, -0.14], [0.46, 0.42]);
            let trace = Pen { run: true, ..Pen::line(0, 0.35) };
            for (a, z) in [(0.0, 0.34), (0.06, 0.42)] {
                b.line(trace, &[[-0.46, -0.183, 0.2 + a], [-0.3, -0.183, z], [0.3, -0.183, z], [0.66, -0.183, 0.3 + a]]);
            }
            b.line(trace, &[[-0.4, -0.183, -0.36], [-0.36, -0.183, -0.36]]);
        }
        // A processor on its package, and the circuit it stands in.
        Model::System => {
            b.solid(cube(), &[one(1, [0.0, -0.1, 0.0], [0.62, 0.05, 0.62], 0.0, DARK)]);
            b.solid(cube(), &[one(2, [0.0; 3], [0.4, 0.055, 0.4], 0.0, METAL)]);
            b.floor(Pen::line(1, 0.8), [0.0, -0.048, 0.0], [0.62, 0.62]);
            b.floor(Pen::line(2, 0.85), [0.0, 0.057, 0.0], [0.4, 0.4]);
            b.floor(Pen::fine(2, 0.55), [0.0, 0.057, 0.0], [0.26, 0.26]);
            b.line(Pen::line(2, 0.6), &[[-0.33, 0.058, 0.33], [-0.27, 0.058, 0.33], [-0.33, 0.058, 0.27], [-0.33, 0.058, 0.33]]);
            let pins: Vec<[f32; 12]> = (0..8)
                .flat_map(|k| {
                    let at = -0.49 + 0.14 * k as f32;
                    [(at, 0.69), (at, -0.69), (0.69, at), (-0.69, at)].map(|(x, z)| {
                        let size = if x.abs() > z.abs() { [0.07, 0.012, 0.022] } else { [0.022, 0.012, 0.07] };
                        one(1, [x, -0.13, z], size, 0.0, METAL)
                    })
                })
                .collect();
            b.solid(cube(), &pins);
            let trace = Pen { run: true, ..Pen::line(0, 0.5) };
            for k in 0..6 {
                let (at, out) = (-0.49 + 0.196 * k as f32, 0.94 + 0.06 * (k % 2) as f32);
                let off = at * 1.35;
                for (x, z) in [(1.0f32, 0.0f32), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                    let put = |along: f32, side: f32| [x * along + z * side, -0.17, z * along + x * side];
                    b.line(trace, &[put(0.78, at), put(0.86, at), put(out, off), put(1.14, off)]);
                }
            }
            b.floor(Pen::fine(0, 0.35), [0.0, -0.17, 0.0], [1.16, 1.16]);
        }
        // Platters on a spindle, and the arm between them.
        Model::Storage => {
            let platters: Vec<[f32; 12]> = (0..3).map(|k| one(k, [0.0; 3], [0.95, 0.95, 0.022], 0.0, [0.55, 1.0, 0.0])).collect();
            b.solid(drum(), &platters);
            let hubs: Vec<[f32; 12]> = (0..3).map(|k| one(k, [0.0, 0.0, 0.04], [0.2, 0.2, 0.06], 0.0, METAL)).collect();
            b.solid(drum(), &hubs);
            for k in 0..3 {
                b.circle(Pen::line(k, 0.75), 0.95, 0.023);
                for r in [0.36, 0.5, 0.64, 0.78] {
                    b.circle(Pen { run: true, ..Pen::fine(k, 0.34) }, r, 0.024);
                }
                b.line(Pen::fine(k, 0.5), &[[0.36, 0.0, 0.024], [0.9, 0.0, 0.024]]);
            }
            b.solid(
                cube(),
                &[one(3, [-0.36, 0.0, 0.0], [0.4, 0.04, 0.014], 0.0, METAL), one(3, [-0.74, 0.0, 0.0], [0.05, 0.075, 0.016], 0.0, METAL)],
            );
            b.solid(drum(), &[one(3, [0.1, 0.0, 0.0], [0.13, 0.13, 0.07], 0.0, METAL)]);
        }
    }
    b
}

// ----------------------------------------------------------------- motion

/// Where a model is, for a camera: its parts, how much of each is seen and
/// how much light it has of its own, and the pulse along its lines.
struct Pose {
    part: [Mat4; PARTS],
    lit: [[f32; 2]; PARTS],
    pulse: [f32; 2],
}

fn pose(model: Model, cam: &Cam) -> Pose {
    let (d, far, near) = (cam.d, cam.d_far, cam.d_near);
    let (f, f_far, f_near) = (cam.f_main, cam.f_far, cam.f_near);
    // 0 at rest, and how far what is in layers stands apart on the way.
    let away = d.abs().min(1.0);
    let open = 1.0 + 1.1 * far.abs().max(near.abs()).min(1.0) * cam.depth;
    let go = (1.0 - away * 1.15).clamp(0.0, 1.0);
    let swell = 4.0 * go * (1.0 - go);
    let mut part = [math::IDENTITY; PARTS];
    let mut lit = [[f, 0.0]; PARTS];
    let view;
    match model {
        Model::Media => {
            view = mul(&rotate_x(0.14), &rotate_y(-0.42 + 0.45 * d));
            part[0] = mul(&translate([0.6, 0.58, -0.22 * open]), &rotate_z(-3.2 * far));
            part[1] = mul(&translate([-0.22, -0.2, 0.0]), &rotate_z(0.3 + 2.2 * d));
            part[2] = mul(&translate([0.0, 0.0, 0.25 * (open - 1.0)]), &rotate_z(0.3 * near));
            lit[0][0] = f_far;
            lit[2][0] = f_near;
        }
        Model::Links => {
            view = mul(&rotate_x(0.22), &rotate_y(0.25));
            part[0] = mul(&rotate_x(1.0 + 0.4 * far), &rotate_y(0.5));
            part[1] = mul(&rotate_y(1.5 * d), &scale(1.0 - 0.25 * away));
            part[2] = mul(&mul(&rotate_x(0.9), &rotate_y(-0.75 - 0.5 * near)), &rotate_z(1.8 * near));
            lit[0][0] = f_far;
            lit[2][0] = f_near;
        }
        Model::Remote => {
            view = mul(&rotate_x(0.12), &rotate_y(0.3 - 0.4 * d));
            part[0] = mul(&translate([0.36, 0.46, -0.45 * open]), &rotate_y(-0.35));
            part[1] = mul(&mul(&translate([-0.46, -0.28, 0.2]), &rotate_z(-0.42)), &rotate_x(-0.45 - 0.6 * d));
            lit[0][0] = f_far;
            for k in 0..3 {
                let r = 0.26 + 0.17 * k as f32;
                part[2 + k] = mul(&translate([-0.2, 0.16, 0.0]), &scale(r * (1.0 - 0.55 * near.abs().min(1.0))));
                lit[2 + k][0] = f_near;
            }
        }
        Model::Live => {
            view = mul(&rotate_x(0.16), &rotate_y(-0.18 + 0.3 * d));
            part[0] = mul(&mul(&rotate_z(0.42), &rotate_x(1.2)), &rotate_z(far));
            part[1] = mul(&mul(&translate([0.36 + 0.5 * d, 0.5 + 0.2 * d, 0.1]), &rotate_z(0.42)), &mul(&rotate_x(0.5), &rotate_y(-0.35 + 1.4 * d)));
            part[3] = mul(&mul(&translate([-0.62, -0.46, 0.15]), &rotate_y(0.85 + 0.5 * near)), &rotate_x(-0.6));
            part[4] = translate([-0.62, -0.52, 0.15]);
            lit[0][0] = f_far;
            lit[3][0] = f_near.max(f * 0.7);
            lit[4][0] = lit[3][0];
            lit[5][0] = f * go;
        }
        Model::Diagnostics => {
            view = mul(&rotate_x(0.3), &rotate_y(0.42 - 0.35 * d));
            part[0] = translate([0.0, 0.0, -0.4 * open]);
            let lens = translate([-0.16 + 0.7 * near, 0.18 + 0.25 * near, 0.22 * open]);
            part[1] = lens;
            part[2] = mul(&lens, &rotate_z(2.6 * d));
            part[3] = mul(&translate([0.0, 0.0, 0.3]), &scale(1.0 + 0.35 * near.abs().min(1.0)));
            part[3] = mul(&part[3], &scale(0.92));
            lit[0][0] = f_far;
            lit[3][0] = f_near;
        }
        Model::Picture => {
            view = mul(&rotate_x(0.16), &rotate_y(-0.4 + 0.5 * d));
            part[0] = translate([-0.2, 0.24, -0.42 * open]);
            part[1] = translate([0.04, 0.0, 0.0]);
            lit[0][0] = f_far;
            for k in 0..3 {
                let on = 0.1 + 0.12 * k as f32;
                part[2 + k] = mul(&translate([0.0, 0.0, on * open]), &scale(1.0 + (0.3 + 0.2 * k as f32) * near.abs().min(1.0)));
                lit[2 + k][0] = f_near;
            }
        }
        Model::Output => {
            view = mul(&rotate_x(0.2), &rotate_y(0.68 - 0.55 * d));
            part[0] = translate([0.0, 0.0, -0.52 * open]);
            part[2] = translate([0.0, 0.0, 0.52 * open]);
            lit[0][0] = f_far;
            lit[2][0] = f_near;
        }
        Model::Audio => {
            view = mul(&rotate_x(0.18), &rotate_y(-0.6 + 0.35 * d));
            for (k, part) in part.iter_mut().take(5).enumerate() {
                let k = k as f32;
                let ahead = d + (near - d) * k / 4.0;
                let wave = 0.26 * (7.0 * ahead - 1.15 * k).sin() * (2.2 * ahead.abs()).min(1.0);
                *part = translate([0.0, 0.0, -0.34 + 0.15 * k + wave]);
            }
            lit[4][0] = f_near;
            part[5] = mul(&translate([0.0, 0.0, 0.3 * open]), &rotate_z(0.9 * near));
            lit[5][0] = f_near;
        }
        Model::Cooling => {
            view = mul(&rotate_x(0.22), &rotate_y(0.5 - 0.3 * d));
            part[1] = rotate_z(0.2 - 4.2 * d);
            part[2] = translate([0.0, 0.0, -0.22 * (open - 1.0)]);
            part[3] = translate([0.0, 0.0, 0.3 * (open - 1.0)]);
            lit[2][0] = f_far;
            lit[3][0] = f_near;
        }
        Model::Device => {
            view = mul(&mul(&rotate_x(0.62), &rotate_y(-0.62 + 0.7 * d)), &scale(1.04));
            part[1] = translate([0.0, 0.55 * near.abs().min(1.0), 0.0]);
            lit[1][0] = f_near.max(f * 0.6);
            for k in 0..3 {
                lit[2 + k][1] = swell * (1.0 - (go * 3.0 - 0.5 - k as f32).abs()).max(0.0);
            }
        }
        Model::System => {
            view = mul(&mul(&rotate_x(0.66), &rotate_y(-0.72 + 0.8 * d)), &scale(0.82));
            part[0] = translate([0.0, -0.4 * far.abs().min(1.0), 0.0]);
            part[2] = translate([0.0, 0.08 + 0.6 * near.abs().min(1.0), 0.0]);
            lit[0][0] = f_far;
            lit[2][0] = f_near;
        }
        Model::Storage => {
            // The spindle stands up.
            view = mul(&mul(&mul(&rotate_x(0.56), &rotate_y(-0.4 + 0.3 * d)), &rotate_x(-PI / 2.0)), &scale(0.9));
            let spin = 2.6 * d;
            for (k, (part, way)) in part.iter_mut().zip([far, d, near]).enumerate() {
                *part = mul(&translate([0.0, 0.0, (k as f32 - 1.0) * 0.4 * open]), &rotate_z(spin + 0.6 * k as f32 + way));
            }
            lit[0][0] = f_far;
            lit[2][0] = f_near;
            part[3] = mul(&translate([1.08, -0.2, 0.2 * open]), &rotate_z(0.28 + 0.7 * near));
        }
    }
    for part in &mut part {
        *part = mul(&view, part);
    }
    Pose { part, lit, pulse: [go, swell] }
}

/// How far off the lens is, and what it takes in: a little more than the box,
/// as a model's corners are nearer than its middle.
const EYE: f32 = 4.0;
const REACH: f32 = 1.36;

/// The whole scene's scale and how far it has dropped, in pixels: as a main
/// plane's (ui/quiet/scenes.slint, `Plane`).
fn carried(cam: &Cam) -> (f32, f32) {
    let d = cam.d;
    let scale = if d >= 0.0 { (1.0 - d * 0.32 * cam.depth).max(0.25) } else { 1.0 - d * 0.42 * cam.depth };
    (scale, d * 0.34 * cam.travel * cam.size)
}

/// The room for one, whichever of its two sizes it stands in: the larger one's
/// (ui/quiet/home.slint, `QuietPlaces`), and all of its way out (`place`). One
/// size of texture, then, however the emblem shrinks and whatever it is of.
pub fn room(cam: &Cam) -> f32 {
    1.9 * (228.0 * (cam.panel.0 / 1920.0).min(cam.panel.1 / 1080.0)).max(cam.size)
}

/// The part of the panel it may touch now.
pub fn place(cam: &Cam) -> Place {
    if cam.f_main.max(cam.f_far).max(cam.f_near) <= 0.0 {
        return Place::default();
    }
    let (scale, drop) = carried(cam);
    // What stands apart on the way is outside the box.
    let half = cam.size * (0.53 * scale + 0.24 * cam.d_far.abs().max(cam.d_near.abs()).min(1.0)) + 2.0;
    let (x, y) = (cam.x + cam.size / 2.0, cam.y + cam.size / 2.0 + drop);
    Place::covering(cam, [x - half, y - half, x + half, y + half])
}

pub struct Emblem {
    model: Model,
    tools: Rc<Tools>,
    solid: Vec<Mesh>,
    glass: Vec<Mesh>,
    lines: Mesh,
}

impl Emblem {
    pub fn new(gpu: &Rc<Gpu>, tools: Rc<Tools>, model: Model) -> Result<Self, Error> {
        let made = build(model);
        let meshes = |all: &[(Shape, Vec<f32>)]| -> Result<Vec<Mesh>, Error> {
            all.iter()
                .map(|((vertices, indices), instances)| {
                    let instance = &[4, 4, 4];
                    Mesh::new(gpu, &MeshData { vertices, vertex: &[3, 3], indices, instances, instance, mode: glow::TRIANGLES })
                })
                .collect()
        };
        let lines = Mesh::new(
            gpu,
            &MeshData {
                vertices: &[0.0, -1.0, 0.0, 1.0, 1.0, -1.0, 1.0, 1.0],
                vertex: &[2],
                indices: &[0, 1, 2, 3],
                instances: &made.lines,
                instance: &[4, 4, 4, 4, 2],
                mode: glow::TRIANGLE_STRIP,
            },
        )?;
        Ok(Self { model, tools, solid: meshes(&made.solid)?, glass: meshes(&made.glass)?, lines })
    }
}

impl Scene for Emblem {
    fn pass(&self) -> Pass {
        Pass { samples: 4, depth: true, straight: false }
    }

    fn cost(&self) -> Cost {
        let all = || self.solid.iter().chain(&self.glass).chain([&self.lines]);
        Cost {
            triangles: all().map(Mesh::triangles).sum(),
            instances: all().map(|mesh| mesh.instances as u64).sum(),
            layers: 1 + self.glass.iter().map(|mesh| mesh.instances as u32).sum::<u32>(),
        }
    }

    fn draw(&self, gpu: &Gpu, cam: &Cam, place: &Place, drawn: &mut Drawn) {
        let gl = &gpu.gl;
        let Pose { part, lit, pulse } = pose(self.model, cam);
        let (by, drop) = carried(cam);
        // The lens over the box, then the box into its place of the panel.
        let (w, h) = (place.w as f32, place.h as f32);
        let mut post = math::IDENTITY;
        (post[0], post[5]) = (cam.size * by / w, cam.size * by / h);
        post[12] = 2.0 * (cam.x + cam.size / 2.0 - place.x as f32) / w - 1.0;
        post[13] = 2.0 * (cam.y + cam.size / 2.0 + drop - place.y as f32) / h - 1.0;
        let eye = [0.0, 0.0, EYE];
        let lens = mul(&math::perspective(2.0 * (REACH / EYE).atan(), 1.0, 0.5, 12.0), &math::look_at(eye, [0.0; 3]));
        let vp = mul(&post, &lens);
        let (parts, lit) = (part.concat(), lit.concat());
        let rgb = |c: [f32; 4]| [c[0], c[1], c[2]];
        let (tint, glow, ink) = (rgb(cam.tint), rgb(cam.glow), rgb(cam.ink));
        let [u_vp, u_part, u_lit, u_eye, u_tint, u_glow] = &self.tools.solid_at;
        // SAFETY: the context is current and the pass is begun.
        unsafe {
            gl.use_program(Some(self.tools.solid.name));
            gl.uniform_matrix_4_f32_slice(u_vp.as_ref(), false, &vp);
            gl.uniform_matrix_4_f32_slice(u_part.as_ref(), false, &parts);
            gl.uniform_2_f32_slice(u_lit.as_ref(), &lit);
            gl.uniform_3_f32_slice(u_eye.as_ref(), &eye);
            gl.uniform_3_f32_slice(u_tint.as_ref(), &tint);
            gl.uniform_3_f32_slice(u_glow.as_ref(), &glow);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
        }
        for mesh in &self.solid {
            mesh.draw(gl, drawn);
        }
        // SAFETY: see above.
        unsafe { gl.depth_mask(false) };
        for mesh in &self.glass {
            mesh.draw(gl, drawn);
        }
        let [u_vp, u_part, u_lit, u_view, u_weight, u_far, u_colour, u_pulse, u_light] = &self.tools.line_at;
        // SAFETY: see above.
        unsafe {
            gl.use_program(Some(self.tools.line.name));
            gl.uniform_matrix_4_f32_slice(u_vp.as_ref(), false, &vp);
            gl.uniform_matrix_4_f32_slice(u_part.as_ref(), false, &parts);
            gl.uniform_2_f32_slice(u_lit.as_ref(), &lit);
            gl.uniform_2_f32(u_view.as_ref(), w, h);
            gl.uniform_2_f32(u_weight.as_ref(), cam.fine, cam.line);
            gl.uniform_1_f32(u_far.as_ref(), EYE - 0.4);
            gl.uniform_3_f32_slice(u_colour.as_ref(), &[ink, glow].concat());
            gl.uniform_2_f32_slice(u_pulse.as_ref(), &pulse);
            gl.uniform_3_f32_slice(u_light.as_ref(), &[1.0, 0.97, 0.92]);
        }
        self.lines.draw(gl, drawn);
    }
}
