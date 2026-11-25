//! How much of the appliance scenes may have. The numbers are measured
//! (docs/quietos-advanced-graphics.md, "Bütçe"), not chosen; a scene that
//! would go past one is drawn by Slint instead, whole, never in part.

use std::time::Duration;

use super::gl::Error;

/// What drawing a scene once asks of the GPU.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Cost {
    pub triangles: u64,
    pub instances: u64,
    /// How many times it may blend over a pixel of its place.
    pub layers: u32,
}

impl Cost {
    /// The GPU's cycles for it over `pixels`, from what scenes of every shape
    /// took on a Mali-G610, at 3840x2160 and at 2560x1440: from a tenth under
    /// what was counted to a third over.
    pub fn cycles(&self, pixels: u64) -> u64 {
        let layers = u64::from(self.layers);
        // Its own pass, the one that makes it straight and Slint's drawing of
        // it are 0.45 a pixel; a layer 0.05, for a shell covers its middle.
        (pixels.saturating_mul(45 + 5 * layers) / 100)
            .saturating_add(self.triangles.saturating_mul(6))
            .saturating_add(self.instances.saturating_mul(40))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    /// Scenes shown at once.
    pub scenes: usize,
    /// Textures, depth and geometry.
    pub bytes: u64,
    /// The scenes' part of a frame's time on the GPU at its full speed.
    pub share: f32,
}

/// Mali-G610's, as the RK3588 runs it.
const FULL_SPEED_HZ: f64 = 1e9;

impl Budget {
    pub const PRODUCT: Self = Self { scenes: 6, bytes: 192 << 20, share: 0.55 };

    /// Anything, for finding out what the numbers above should be.
    #[cfg(any(test, feature = "spatial-lab"))]
    pub const NONE: Self = Self { scenes: 64, bytes: 1 << 30, share: f32::INFINITY };

    /// Whether `cycles` in a frame of `period` are within it.
    pub fn holds(&self, cycles: u64, period: Duration) -> Result<(), Error> {
        if cycles as f64 > period.as_secs_f64() * FULL_SPEED_HZ * f64::from(self.share) {
            return Err(Error::Budget("GPU time"));
        }
        Ok(())
    }
}
