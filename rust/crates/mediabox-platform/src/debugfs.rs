//! The display device's debugfs directory, found rather than assumed.
//!
//! `/sys/kernel/debug/dri/0` is the first DRM device's directory on a board
//! whose display registered first. The directory is named by the device's
//! *primary minor*, which is whatever the kernel handed out; a board whose NPU
//! registers first, or a second display device, moves it. And a directory by
//! that number is not necessarily ours: the render node's minor has one too,
//! and on this driver its `name` reads the same.
//!
//! So the chain is walked every time, from the selected KMS device:
//!
//! ```text
//! selected KMS device -> cardN -> /sys/class/drm/cardN/dev (major:minor)
//!   -> /sys/kernel/debug/dri/<minor> -> `name` names the same platform device
//! ```
//!
//! and anything that does not hold up is [`Debugfs::Unknown`], with the reason.
//! What debugfs says is diagnostics -- the bus format on the wire, the mode a
//! video port runs -- and never the authority for what a sink can take, what a
//! mode costs, whether HDR fits or what a person chose. Nothing breaks when it
//! is not there; the answer is just "unknown".

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::drm::DrmNode;
use crate::roots::{Roots, read_trimmed};

/// Where debugfs is mounted, under `roots`.
fn debugfs(roots: &Roots) -> PathBuf {
    roots.sys("kernel/debug")
}

/// The display device's debugfs directory, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "debugfs", rename_all = "snake_case")]
pub enum Debugfs {
    Resolved { dir: PathBuf, minor: u32 },
    Unknown { reason: String },
}

impl Debugfs {
    pub fn dir(&self) -> Option<&PathBuf> {
        match self {
            Debugfs::Resolved { dir, .. } => Some(dir),
            Debugfs::Unknown { .. } => None,
        }
    }

    /// Read one file of the directory. `Unknown` if the directory was not
    /// resolved or the file is not there.
    pub fn read(&self, file: &str) -> Observed<String> {
        match self {
            Debugfs::Resolved { dir, .. } => match std::fs::read_to_string(dir.join(file)) {
                Ok(text) => Observed::Known(text),
                Err(error) => Observed::Unknown(format!("{}: {error}", dir.join(file).display())),
            },
            Debugfs::Unknown { reason } => Observed::Unknown(reason.clone()),
        }
    }
}

/// A value read from somewhere that may not be there. Unknown is an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Observed<T> {
    Known(T),
    Unknown(String),
}

impl<T> Observed<T> {
    pub fn known(self) -> Option<T> {
        match self {
            Observed::Known(value) => Some(value),
            Observed::Unknown(_) => None,
        }
    }
}

/// The debugfs directory of the display device discovery chooses, the way
/// [`crate::Platform::inspect`] chooses it: the DRM device that owns
/// connectors. For callers that want this one answer and nothing else.
pub fn discover(roots: &Roots) -> Debugfs {
    match crate::drm::find_kms(roots, None) {
        Some((kms, _)) => resolve(roots, &kms),
        None => Debugfs::Unknown {
            reason: "no DRM device with connectors".into(),
        },
    }
}

/// Resolve the debugfs directory of `kms`.
pub fn resolve(roots: &Roots, kms: &DrmNode) -> Debugfs {
    let unknown = |reason: String| Debugfs::Unknown { reason };
    let dev = roots.sys(&format!("class/drm/{}/dev", kms.name));
    let Some(numbers) = read_trimmed(&dev) else {
        return unknown(format!("{} is not readable", dev.display()));
    };
    let Some(minor) = numbers
        .split_once(':')
        .and_then(|(_, minor)| minor.trim().parse::<u32>().ok())
    else {
        return unknown(format!("{} is not major:minor ({numbers})", dev.display()));
    };
    let dir = debugfs(roots).join(format!("dri/{minor}"));
    let name_file = dir.join("name");
    let Some(name) = read_trimmed(&name_file) else {
        return unknown(format!(
            "{} is not readable (debugfs not mounted, or not permitted)",
            name_file.display()
        ));
    };
    // `<driver> dev=<device> unique=<unique>`: the device must be the one the
    // KMS node belongs to.
    let Some(parent) = kms.parent.as_deref() else {
        return unknown(format!("{} has no parent device to check {} against", kms.name, name_file.display()));
    };
    let names_it = name
        .split_whitespace()
        .any(|field| field.strip_prefix("dev=") == Some(parent));
    if !names_it {
        return unknown(format!(
            "{} reads '{name}', which is not {parent}",
            name_file.display()
        ));
    }
    Debugfs::Resolved { dir, minor }
}

/// A CEC adapter's physical address as its debugfs status reports it,
/// without opening the adapter: `phys_addr: 3.0.0.0`. `f.f.f.f` is "no sink".
pub fn cec_physical_address(roots: &Roots, adapter: &str) -> Observed<Option<u16>> {
    let path = debugfs(roots).join(format!("cec/{adapter}/status"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Observed::Unknown(format!("{} is not readable", path.display()));
    };
    for line in text.lines() {
        if let Some(value) = line.trim().strip_prefix("phys_addr:") {
            let value = value.trim();
            if value.eq_ignore_ascii_case("f.f.f.f") {
                return Observed::Known(None);
            }
            let digits: Vec<u16> = value
                .split('.')
                .filter_map(|part| u16::from_str_radix(part, 16).ok())
                .collect();
            if digits.len() == 4 && digits.iter().all(|digit| *digit <= 0xF) {
                return Observed::Known(Some(
                    (digits[0] << 12) | (digits[1] << 8) | (digits[2] << 4) | digits[3],
                ));
            }
            return Observed::Unknown(format!("{}: phys_addr '{value}'", path.display()));
        }
    }
    Observed::Unknown(format!("{} has no phys_addr", path.display()))
}

/// A clock's rate and whether anything holds it enabled, from the common
/// clock framework's debugfs.
pub fn clock(roots: &Roots, name: &str) -> Observed<(u64, u32)> {
    let dir = debugfs(roots).join(format!("clk/{name}"));
    let rate = read_trimmed(dir.join("clk_rate")).and_then(|text| text.parse::<u64>().ok());
    let enabled = read_trimmed(dir.join("clk_enable_count")).and_then(|text| text.parse::<u32>().ok());
    match (rate, enabled) {
        (Some(rate), Some(enabled)) => Observed::Known((rate, enabled)),
        _ => Observed::Unknown(format!("{} is not readable", dir.display())),
    }
}

/// What the vendor summary says one connector is doing: the video port it is
/// on, whether that port is active, the pixel clock and the bus format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorActivity {
    pub video_port: u32,
    pub active: bool,
    pub dclk_khz: Option<u32>,
    pub bus_format: Option<String>,
    pub mode: Option<String>,
}

/// Parse the vendor driver's `summary` for `connector`.
pub fn connector_activity(summary: &str, connector: &str) -> Option<ConnectorActivity> {
    let mut port: Option<(u32, bool)> = None;
    let mut found: Option<ConnectorActivity> = None;
    for line in summary.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Video Port") {
            if found.is_some() {
                break;
            }
            let (number, state) = rest.split_once(':').unwrap_or((rest, ""));
            port = number.trim().parse().ok().map(|n| (n, state.trim() == "ACTIVE"));
            continue;
        }
        if let Some(rest) = line.strip_prefix("Connector:") {
            if found.is_some() {
                break;
            }
            if rest.split_whitespace().next() == Some(connector)
                && let Some((video_port, active)) = port
            {
                found = Some(ConnectorActivity {
                    video_port,
                    active,
                    dclk_khz: None,
                    bus_format: None,
                    mode: None,
                });
            }
            continue;
        }
        let Some(activity) = found.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("bus_format[")
            && let Some((_, name)) = rest.split_once("]:")
        {
            activity.bus_format.get_or_insert_with(|| name.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Display mode:") {
            activity.mode.get_or_insert_with(|| rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("dclk[")
            && let Some((value, _)) = rest.split_once(" kHz]")
        {
            activity.dclk_khz = activity.dclk_khz.or(value.trim().parse().ok());
        }
    }
    found
}
