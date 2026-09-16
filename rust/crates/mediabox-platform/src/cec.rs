//! The CEC adapter that belongs to a display output.
//!
//! An HDMI transmitter's CEC adapter is registered as a child device of the
//! transmitter, so this is a directory listing rather than an inference: the
//! adapter under `fdea0000.hdmi` is that output's adapter, and on a board with
//! two transmitters each has its own.
//!
//! `/dev/cec0` is therefore never written down. On the Ultra it happens to be
//! the only adapter there is; on the Plus, which of the two is the television's
//! depends on which socket the television is in.
//!
//! DisplayPort has no CEC. That is reported as an output with no adapter, not
//! as a failure — a box on DisplayPort still plays films, it just cannot turn
//! the screen on.

use serde::{Deserialize, Serialize};

use crate::drm::Controller;
use crate::roots::{Roots, file_name, sorted_children};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CecAdapter {
    /// `cec0` — the kernel's name for it this boot.
    pub name: String,
    /// The node to open.
    pub device: std::path::PathBuf,
    /// The transmitter it is the adapter for.
    pub controller: String,
}

/// Every CEC adapter that belongs to a display transmitter.
pub fn adapters(roots: &Roots, controllers: &[Controller]) -> Vec<CecAdapter> {
    let mut found = Vec::new();
    for controller in controllers {
        for entry in sorted_children(&controller.sysfs) {
            let name = file_name(&entry);
            let Some(index) = name.strip_prefix("cec") else {
                continue;
            };
            if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            found.push(CecAdapter {
                device: roots.dev(&name),
                name,
                controller: controller.device.clone(),
            });
        }
    }
    found
}
