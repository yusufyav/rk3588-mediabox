//! The Motion Lab: Quiet OS's motion, turned on a real television.
//!
//! A development tool, not a setting. Its panel (ui/quiet/lab.slint) and the
//! parameters it turns (`QuietMotion` in ui/quiet/motion.slint) are part of
//! every build, because they are the same properties the product animates
//! with; what keeps the lab out of the product is that nothing opens it
//! unless the interface was started with MEDIABOX_TV_MOTION_LAB set. Nothing
//! it turns is written anywhere: a restart is the product's motion again.
//!
//! On Quiet OS's home screen the Menu key opens and closes it. While it is
//! open it takes the remote: Up and Down choose a parameter, Left and Right
//! turn it, Ok replays the camera's last move, Back closes it.

use crate::actions::Intent;
use crate::{MediaBoxWindow, QuietMotion};
use slint::ComponentHandle;

/// The rows, in the panel's order.
const ROWS: i32 = 8;
const REPLAY: i32 = 7;

pub struct Lab {
    enabled: bool,
}

impl Lab {
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var_os("MEDIABOX_TV_MOTION_LAB").is_some(),
        }
    }

    /// As if started with MEDIABOX_TV_MOTION_LAB.
    #[cfg(test)]
    pub fn enabled() -> Self {
        Self { enabled: true }
    }

    pub fn is_open(&self, ui: &MediaBoxWindow) -> bool {
        self.enabled && ui.global::<QuietMotion>().get_lab_open()
    }

    /// The Menu key on Quiet OS's home. False when the lab is not enabled,
    /// so the key means what it always meant.
    pub fn toggle(&self, ui: &MediaBoxWindow) -> bool {
        if !self.enabled {
            return false;
        }
        let motion = ui.global::<QuietMotion>();
        motion.set_lab_open(!motion.get_lab_open());
        true
    }

    /// A key while the panel is open: always the lab's.
    pub fn act(&self, ui: &MediaBoxWindow, intent: Intent) {
        let motion = ui.global::<QuietMotion>();
        match intent {
            Intent::Move(0, dy) => {
                motion.set_lab_row((motion.get_lab_row() + dy).rem_euclid(ROWS));
            }
            Intent::Move(dx, _) => turn(&motion, motion.get_lab_row(), dx),
            Intent::Select if motion.get_lab_row() == REPLAY => {
                motion.set_replay(motion.get_replay().wrapping_add(1));
            }
            Intent::Select => {}
            Intent::Dismiss => motion.set_lab_open(false),
            _ => {}
        }
    }
}

/// One step of a parameter, kept within what still reads as motion.
fn turn(motion: &QuietMotion<'_>, row: i32, dx: i32) {
    let clamp = |value: f32, lo: f32, hi: f32| {
        (((value + 0.1 * dx as f32) * 100.0).round() / 100.0).clamp(lo, hi)
    };
    match row {
        0 => motion.set_pace(clamp(motion.get_pace(), 0.3, 3.0)),
        1 => motion.set_depth(clamp(motion.get_depth(), 0.0, 2.5)),
        2 => motion.set_travel(clamp(motion.get_travel(), 0.0, 2.5)),
        3 => motion.set_glow(clamp(motion.get_glow(), 0.0, 2.5)),
        4 => motion.set_parallax(clamp(motion.get_parallax(), 0.0, 2.5)),
        5 => motion.set_profile((motion.get_profile() + dx).rem_euclid(3)),
        6 => motion.set_reduced(!motion.get_reduced()),
        _ => {}
    }
}
