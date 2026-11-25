//! The few matrices a camera needs. Column-major, as GL takes them.

pub type Mat4 = [f32; 16];

pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    out
}

/// A pinhole of `fovy` radians. `y` is flipped: the picture's first row is
/// its top one, which is what Slint is told (`BorrowedOpenGLTextureOrigin`).
pub fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fovy / 2.0).tan();
    let mut m = [0.0; 16];
    m[0] = f / aspect;
    m[5] = -f;
    m[10] = (far + near) / (near - far);
    m[11] = -1.0;
    m[14] = 2.0 * far * near / (near - far);
    m
}

pub fn look_at(eye: [f32; 3], at: [f32; 3]) -> Mat4 {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let cross =
        |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let unit = |a: [f32; 3]| {
        let n = dot(a, a).sqrt().max(1e-6);
        [a[0] / n, a[1] / n, a[2] / n]
    };
    let forward = unit(sub(at, eye));
    let side = unit(cross(forward, [0.0, 1.0, 0.0]));
    let up = cross(side, forward);
    [
        side[0], up[0], -forward[0], 0.0,
        side[1], up[1], -forward[1], 0.0,
        side[2], up[2], -forward[2], 0.0,
        -dot(side, eye), -dot(up, eye), dot(forward, eye), 1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(m: &Mat4, p: [f32; 3]) -> [f32; 4] {
        std::array::from_fn(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
    }

    #[test]
    fn what_the_camera_looks_at_is_in_the_middle_and_up_is_the_first_row() {
        let m = mul(&perspective(0.8, 1.5, 0.1, 50.0), &look_at([3.0, 2.0, 5.0], [0.5, 0.0, 0.0]));
        let middle = at(&m, [0.5, 0.0, 0.0]);
        assert!((middle[0] / middle[3]).abs() < 1e-5 && (middle[1] / middle[3]).abs() < 1e-5);
        let above = at(&m, [0.5, 1.0, 0.0]);
        assert!(above[1] / above[3] < 0.0);
        // Nearer is smaller in depth.
        let near = at(&m, [1.5, 1.0, 2.0]);
        assert!(near[2] / near[3] < middle[2] / middle[3]);
    }
}
