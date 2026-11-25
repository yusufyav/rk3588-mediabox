//! Levels 2 and 3: what the foundation can carry, not a design. A globe with
//! a shader material, rings turning in one another, satellites on orbits of
//! their own, glass shells and particles, under a moving pinhole camera with
//! a depth buffer.
//!
//! Every transform is the GPU's: an instance holds its orbit, the vertex
//! shader puts it where it is now. A frame costs the CPU two matrices, a
//! handful of numbers and one draw per kind of thing, however many there are.

use std::rc::Rc;

use super::super::gl::{Drawn, Error, Gpu, Mesh, MeshData, Owned, program};
use super::super::{Cam, Cost, Pass, Place, Scene};
use super::super::math;
use super::Config;
use glow::{HasContext, NativeProgram};

const PI: f32 = std::f32::consts::PI;

const VERTEX: &str = r"#version 300 es
precision highp float;
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_normal;
layout(location = 2) in vec4 a_orbit;
layout(location = 3) in vec4 a_look;
uniform mat4 u_vp;
uniform float u_phase;
out vec3 v_normal;
out vec3 v_world;
out vec3 v_local;
flat out vec2 v_look;
mat3 about_x(float a) { float c = cos(a), s = sin(a); return mat3(1.,0.,0., 0.,c,s, 0.,-s,c); }
mat3 about_y(float a) { float c = cos(a), s = sin(a); return mat3(c,0.,-s, 0.,1.,0., s,0.,c); }
void main() {
    // orbit: radius, inclination, speed, start. look: size, material, shade, own spin.
    float a = a_orbit.w + a_orbit.z * u_phase;
    mat3 turn = about_x(a_orbit.y) * about_y(a);
    mat3 own = about_y(a_look.w * u_phase);
    vec3 local = own * a_pos * a_look.x;
    vec3 world = turn * (local + vec3(a_orbit.x, 0.0, 0.0));
    v_normal = turn * (own * a_normal);
    v_local = a_pos;
    v_world = world;
    v_look = a_look.yz;
    gl_Position = u_vp * vec4(world, 1.0);
}
";

const FRAGMENT: &str = r"#version 300 es
precision highp float;
in vec3 v_normal;
in vec3 v_world;
in vec3 v_local;
flat in vec2 v_look;
uniform vec3 u_eye;
uniform vec3 u_tint;
uniform vec3 u_glow;
uniform float u_fade;
out vec4 colour;
float lines(float at) {
    float w = fwidth(at);
    return 1.0 - smoothstep(0.0, 1.5 * w, abs(fract(at - 0.5) - 0.5));
}
void main() {
    vec3 n = normalize(v_normal), e = normalize(u_eye - v_world);
    vec3 l = normalize(vec3(-0.4, 0.8, 0.6));
    float facing = abs(dot(n, e));
    float rim = pow(1.0 - facing, 3.0);
    float lit = 0.15 + 0.85 * max(dot(n, l), 0.0);
    float shine = pow(max(dot(n, normalize(l + e)), 0.0), 48.0);
    if (v_look.x < 0.5) {
        // The globe: dark glass, its graticule and a lit rim.
        float lat = asin(clamp(v_local.y, -1.0, 1.0)) * 5.7296;
        float lon = atan(v_local.z, v_local.x) * 5.7296 / 1.5;
        float grid = max(lines(lat / 1.5), lines(lon));
        vec3 base = u_tint * 0.12 * lit + u_tint * grid * 0.5 * (0.3 + 0.7 * lit) + u_glow * rim * 0.9 + shine * 0.4;
        colour = vec4(base, 1.0);
    } else if (v_look.x < 1.5) {
        // Metal: rings and satellites.
        vec3 base = mix(u_tint, u_glow, v_look.y);
        colour = vec4(base * lit * 0.7 + u_glow * shine + u_glow * rim * 0.25, 1.0);
    } else {
        // Glass: seen at its edges.
        float a = 0.03 + 0.4 * rim;
        colour = vec4((mix(u_tint, u_glow, rim) + shine * 2.0) * a, a);
    }
    colour *= u_fade;
}
";

const DUST_VERTEX: &str = r"#version 300 es
precision highp float;
layout(location = 0) in vec2 a_corner;
layout(location = 1) in vec4 a_orbit;
uniform mat4 u_vp;
uniform float u_phase;
uniform vec2 u_size;
out vec2 v_at;
flat out float v_bright;
void main() {
    // radius, height, speed, start
    float a = a_orbit.w + a_orbit.z * u_phase;
    vec3 world = vec3(a_orbit.x * cos(a), a_orbit.y + 0.12 * sin(a * 3.0 + a_orbit.x * 9.0), a_orbit.x * sin(a));
    vec4 clip = u_vp * vec4(world, 1.0);
    v_at = a_corner;
    v_bright = 0.35 + 0.65 * fract(a_orbit.w * 7.31);
    gl_Position = clip + vec4(a_corner * u_size, 0.0, 0.0);
}
";

const DUST_FRAGMENT: &str = r"#version 300 es
precision mediump float;
in vec2 v_at;
flat in float v_bright;
uniform vec3 u_glow;
uniform float u_fade;
out vec4 colour;
void main() {
    float a = max(0.0, 1.0 - dot(v_at, v_at));
    a *= a * 0.5 * v_bright * u_fade;
    colour = vec4(u_glow * a, a);
}
";

/// A sphere of `n` by `2n` quads: its position is its normal.
fn sphere(n: u32) -> (Vec<f32>, Vec<u32>) {
    let (mut v, mut i) = (Vec::new(), Vec::new());
    for y in 0..=n {
        for x in 0..=2 * n {
            let (th, ph) = (PI * y as f32 / n as f32, PI * x as f32 / n as f32);
            let p = [th.sin() * ph.cos(), th.cos(), th.sin() * ph.sin()];
            v.extend(p);
            v.extend(p);
        }
    }
    let row = 2 * n + 1;
    for y in 0..n {
        for x in 0..2 * n {
            let a = y * row + x;
            i.extend([a, a + 1, a + row, a + 1, a + row + 1, a + row]);
        }
    }
    (v, i)
}

/// A ring of radius 1 in the x-z plane, `tube` thick.
fn torus(around: u32, across: u32, tube: f32) -> (Vec<f32>, Vec<u32>) {
    let (mut v, mut i) = (Vec::new(), Vec::new());
    for a in 0..=around {
        for b in 0..=across {
            let (p, q) = (2.0 * PI * a as f32 / around as f32, 2.0 * PI * b as f32 / across as f32);
            let n = [q.cos() * p.cos(), q.sin(), q.cos() * p.sin()];
            v.extend([p.cos() + tube * n[0], tube * n[1], p.sin() + tube * n[2]]);
            v.extend(n);
        }
    }
    let row = across + 1;
    for a in 0..around {
        for b in 0..across {
            let at = a * row + b;
            i.extend([at, at + 1, at + row, at + 1, at + row + 1, at + row]);
        }
    }
    (v, i)
}

/// The same scatter every time: a golden-ratio sequence.
fn scatter(i: usize, k: f32) -> f32 {
    (i as f32 * k).fract()
}

pub struct Advanced {
    solid: Owned<NativeProgram>,
    solid_at: [Option<glow::NativeUniformLocation>; 6],
    dust: Owned<NativeProgram>,
    dust_at: [Option<glow::NativeUniformLocation>; 5],
    opaque: Vec<Mesh>,
    glass: Option<Mesh>,
    particles: Option<Mesh>,
    of: Config,
}

fn solid(gpu: &Rc<Gpu>, (v, i): (Vec<f32>, Vec<u32>), instances: &[f32]) -> Result<Mesh, Error> {
    Mesh::new(
        gpu,
        &MeshData {
            vertices: &v,
            vertex: &[3, 3],
            indices: &i,
            instances,
            instance: &[4, 4],
            mode: glow::TRIANGLES,
        },
    )
}

pub fn place(cam: &Cam) -> Place {
    if cam.f_main <= 0.0 {
        return Place::default();
    }
    if super::full() {
        return Place::covering(cam, [0.0, 0.0, cam.panel.0, cam.panel.1]);
    }
    let over = cam.size * 0.15;
    Place::covering(cam, [cam.x - over, cam.y - over, cam.x + cam.size + over, cam.y + cam.size + over])
}

/// For a measurement: moving for as long as it is shown.
fn seconds() -> f32 {
    static AT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    AT.get_or_init(std::time::Instant::now).elapsed().as_secs_f32()
}

impl Advanced {
    pub fn new(gpu: &Rc<Gpu>, of: &Config) -> Result<Self, Error> {
        let solid_program = program(gpu, VERTEX, FRAGMENT)?;
        let dust = program(gpu, DUST_VERTEX, DUST_FRAGMENT)?;
        // SAFETY: programs just linked on this context.
        let (solid_at, dust_at) = unsafe {
            (
                ["u_vp", "u_phase", "u_eye", "u_tint", "u_glow", "u_fade"].map(|n| gpu.gl.get_uniform_location(solid_program.name, n)),
                ["u_vp", "u_phase", "u_size", "u_glow", "u_fade"].map(|n| gpu.gl.get_uniform_location(dust.name, n)),
            )
        };
        let mut opaque = vec![solid(gpu, sphere(of.globe), &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.35])?];
        if of.rings > 0 {
            // Inside one another, each tilted and turning its own way. A ring
            // turns about its own axis, so it is its inclination that is seen
            // to move: `a_orbit.z` swings it through the vertical.
            let instances: Vec<f32> = (0..of.rings)
                .flat_map(|i| {
                    let way = if i % 2 == 0 { 1.0 } else { -1.0 };
                    [0.0, 0.25 + 0.5 * i as f32, 0.5 * way, i as f32, 1.18 + 0.14 * i as f32, 1.0, 0.35, 0.3 * way]
                })
                .collect();
            opaque.push(solid(gpu, torus(of.detail * 16, of.detail * 2, 0.012), &instances)?);
        }
        if of.objects > 0 {
            let instances: Vec<f32> = (0..of.objects)
                .flat_map(|i| {
                    [
                        1.25 + 1.0 * scatter(i, 0.618_034),
                        PI * scatter(i, 0.754_877),
                        (0.3 + 0.9 * scatter(i, 0.569_84)) * if i % 2 == 0 { 1.0 } else { -1.0 },
                        2.0 * PI * scatter(i, 0.324_718),
                        0.02 + 0.05 * scatter(i, 0.437_01),
                        1.0,
                        scatter(i, 0.181_39),
                        2.0,
                    ]
                })
                .collect();
            opaque.push(solid(gpu, sphere(of.detail), &instances)?);
        }
        let glass = (of.layers > 0)
            .then(|| {
                // Furthest out first: drawn back to front.
                let instances: Vec<f32> = (0..of.layers)
                    .rev()
                    .flat_map(|i| [0.0, 0.0, 0.0, 0.0, 1.06 + 0.05 * i as f32, 2.0, 0.0, 0.0])
                    .collect();
                solid(gpu, sphere(32), &instances)
            })
            .transpose()?;
        let particles = (of.particles > 0)
            .then(|| {
                let instances: Vec<f32> = (0..of.particles)
                    .flat_map(|i| {
                        [
                            1.15 + 1.5 * scatter(i, 0.618_034),
                            1.8 * (scatter(i, 0.754_877) - 0.5),
                            0.15 + 0.5 * scatter(i, 0.569_84),
                            2.0 * PI * scatter(i, 0.324_718),
                        ]
                    })
                    .collect();
                Mesh::new(
                    gpu,
                    &MeshData {
                        vertices: &[-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0],
                        vertex: &[2],
                        indices: &[0, 1, 2, 3],
                        instances: &instances,
                        instance: &[4],
                        mode: glow::TRIANGLE_STRIP,
                    },
                )
            })
            .transpose()?;
        Ok(Self { solid: solid_program, solid_at, dust, dust_at, opaque, glass, particles, of: of.clone() })
    }
}

impl Scene for Advanced {
    fn pass(&self) -> Pass {
        Pass { samples: self.of.samples, depth: true, straight: false }
    }

    fn cost(&self) -> Cost {
        let all = || self.opaque.iter().chain(&self.glass).chain(&self.particles);
        Cost {
            triangles: all().map(Mesh::triangles).sum(),
            instances: all().map(|mesh| mesh.instances as u64).sum(),
            // A shell has a far side and a near one only where it is not
            // culled: one layer each. The dust is small.
            layers: 1 + self.of.layers as u32,
        }
    }

    fn draw(&self, gpu: &Gpu, cam: &Cam, place: &Place, drawn: &mut Drawn) {
        let (gl, of) = (&gpu.gl, &self.of);
        // The camera travels with the launcher's: nearer and round as it passes.
        let phase = if of.run { seconds() } else { -2.0 * cam.d };
        let (distance, turn) = if of.run {
            (4.6 + 0.5 * (phase * 0.7).sin(), phase * 0.25)
        } else {
            (4.6 + 1.6 * cam.d, 0.6 + 0.9 * cam.d)
        };
        let eye = [distance * turn.sin(), 1.1, distance * turn.cos()];
        let aspect = place.w as f32 / place.h as f32;
        let vp = math::mul(&math::perspective(0.95, aspect, 0.2, 40.0), &math::look_at(eye, [0.0; 3]));
        let fade = cam.f_main;
        let [u_vp, u_phase, u_eye, u_tint, u_glow, u_fade] = &self.solid_at;
        // SAFETY: the context is current and the pass is begun.
        unsafe {
            gl.use_program(Some(self.solid.name));
            gl.uniform_matrix_4_f32_slice(u_vp.as_ref(), false, &vp);
            gl.uniform_1_f32(u_phase.as_ref(), phase);
            gl.uniform_3_f32(u_eye.as_ref(), eye[0], eye[1], eye[2]);
            gl.uniform_3_f32(u_tint.as_ref(), cam.tint[0], cam.tint[1], cam.tint[2]);
            gl.uniform_3_f32(u_glow.as_ref(), cam.glow[0], cam.glow[1], cam.glow[2]);
            gl.uniform_1_f32(u_fade.as_ref(), fade);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.disable(glow::BLEND);
            // The picture is drawn upside down (`math::perspective`), so a
            // face seen from outside winds clockwise.
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::BACK);
            gl.front_face(glow::CW);
        }
        for mesh in &self.opaque {
            mesh.draw(gl, drawn);
        }
        // SAFETY: see above.
        unsafe {
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            gl.depth_mask(false);
        }
        if let Some(glass) = &self.glass {
            glass.draw(gl, drawn);
        }
        if let Some(particles) = &self.particles {
            let [u_vp, u_phase, u_size, u_glow, u_fade] = &self.dust_at;
            let size = 0.018 * of.dust;
            // SAFETY: see above.
            unsafe {
                gl.use_program(Some(self.dust.name));
                gl.disable(glow::CULL_FACE);
                gl.uniform_matrix_4_f32_slice(u_vp.as_ref(), false, &vp);
                gl.uniform_1_f32(u_phase.as_ref(), phase);
                gl.uniform_2_f32(u_size.as_ref(), size / aspect, size);
                gl.uniform_3_f32(u_glow.as_ref(), cam.glow[0], cam.glow[1], cam.glow[2]);
                gl.uniform_1_f32(u_fade.as_ref(), fade);
            }
            particles.draw(gl, drawn);
        }
    }
}
