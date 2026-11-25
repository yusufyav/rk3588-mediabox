//! How much of the appliance scenes may have. The numbers are measured on the
//! slowest case it drives, a panel of 3840x2160 at 60 Hz on a Mali-G610
//! (docs/quietos-advanced-graphics.md, "Bütçe"); a scene that would go past
//! one is drawn by Slint instead, whole, never in part.

use super::gl::Error;

/// What a frame's scenes ask of the GPU.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Cost {
    pub triangles: u64,
    pub instances: u64,
    /// Of one scene: how many times it may cover a pixel of its place. Of a
    /// frame: how many pixels are covered, each time counted.
    pub layers: u32,
}

impl Cost {
    /// This frame's, and another scene of `pixels`.
    pub fn add(&mut self, scene: &Cost, pixels: u64) {
        self.triangles += scene.triangles;
        self.instances += scene.instances;
        // In hundredths of a megapixel: a frame's is far under what u32 holds.
        self.layers += (pixels * u64::from(scene.layers)).div_ceil(10_000) as u32;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    /// Scenes shown at once.
    pub scenes: usize,
    /// Textures, depth and geometry.
    pub bytes: u64,
    /// In a frame.
    pub triangles: u64,
    pub instances: u64,
    /// Pixels covered in a frame, each time counted, in hundredths of a
    /// megapixel.
    pub fill: u32,
}

impl Budget {
    pub const PRODUCT: Self =
        Self { scenes: 6, bytes: 192 << 20, triangles: 600_000, instances: 60_000, fill: 2_400 };

    /// Anything, for finding out what the numbers above should be.
    #[cfg(any(test, feature = "spatial-lab"))]
    pub const NONE: Self =
        Self { scenes: 64, bytes: 1 << 30, triangles: u64::MAX, instances: u64::MAX, fill: u32::MAX };

    /// Whether a frame of `spent` is within it.
    pub fn holds(&self, spent: &Cost) -> Result<(), Error> {
        let over = |have: u64, most: u64, what| if have > most { Err(Error::Budget(what)) } else { Ok(()) };
        over(spent.triangles, self.triangles, "triangles")?;
        over(spent.instances, self.instances, "instances")?;
        over(spent.layers.into(), self.fill.into(), "fill")
    }
}
