//! Display devices, render devices, connectors and the controllers behind them.
//!
//! Nothing here is allowed to say `card0`, `renderD128` or `HDMI-A-1`. Those
//! are names the kernel happened to hand out on one board on one boot, and all
//! three change: a board with an NPU registers a second DRM device, a kernel
//! that probes in a different order renumbers the render nodes, and a board
//! with two HDMI transmitters has an `HDMI-A-2` that the first one does not.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dt::{DeviceTree, unit_address};
use crate::roots::{Roots, file_name, link_basename, read_trimmed, sorted_children};

/// A DRM device, named by what it can do rather than by its number.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrmNode {
    /// `card0`, `renderD128` — reported, never matched on.
    pub name: String,
    /// The path a consumer opens.
    pub device: PathBuf,
    /// The driver bound to it, from sysfs.
    pub driver: Option<String>,
    /// The platform device the DRM device belongs to. Two DRM nodes with the
    /// same one are two faces of the same hardware.
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKind {
    /// An HDMI transmitter.
    HdmiA,
    /// DisplayPort, including the USB-C alternate mode a board may route to it.
    DisplayPort,
    /// Embedded DisplayPort — a panel, not a television.
    EmbeddedDisplayPort,
    /// Writeback, DSI, and anything else that is not a television output.
    Other,
}

impl ConnectorKind {
    fn from_connector_name(name: &str) -> (Self, String, u32) {
        // `card0-HDMI-A-2` -> kind HDMI-A, index 2. The card prefix is dropped
        // because it is the device's number, not the connector's identity.
        let bare = name.split_once('-').map(|(_, rest)| rest).unwrap_or(name);
        let (type_name, index) = match bare.rsplit_once('-') {
            Some((head, tail)) => match tail.parse::<u32>() {
                Ok(index) => (head.to_string(), index),
                Err(_) => (bare.to_string(), 0),
            },
            None => (bare.to_string(), 0),
        };
        let kind = match type_name.as_str() {
            "HDMI-A" | "HDMI-B" => ConnectorKind::HdmiA,
            "DP" => ConnectorKind::DisplayPort,
            "eDP" => ConnectorKind::EmbeddedDisplayPort,
            _ => ConnectorKind::Other,
        };
        (kind, type_name, index)
    }

    /// Whether a person could plug a television into it.
    pub fn is_display_output(self) -> bool {
        matches!(
            self,
            ConnectorKind::HdmiA | ConnectorKind::DisplayPort | ConnectorKind::EmbeddedDisplayPort
        )
    }

    /// What a controller of this kind reports through extcon.
    fn extcon_cable(self) -> &'static str {
        match self {
            ConnectorKind::HdmiA => "HDMI",
            ConnectorKind::DisplayPort | ConnectorKind::EmbeddedDisplayPort => "DP",
            ConnectorKind::Other => "",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Connector {
    /// `HDMI-A-1`, without the card prefix: the card's number is not part of
    /// the connector's identity and changes when a second DRM device appears.
    pub name: String,
    pub kind: ConnectorKind,
    /// The `-N` in the name. Assigned by the kernel per connector type, in
    /// registration order.
    pub index: u32,
    pub connected: bool,
    pub enabled: bool,
    /// The sink's preferred mode as the kernel lists it, if it listed any.
    pub preferred_mode: Option<String>,
    pub mode_count: usize,
    /// A sink identity that survives a reboot, from the EDID: manufacturer,
    /// product and serial. This, not the connector's DRM object id, is what a
    /// remembered output choice is stored against.
    pub sink: Option<SinkIdentity>,
    /// Where the kernel publishes this connector. Carried because the things
    /// that have to be done to a connector before anything draws on it — force
    /// a detect, read the EDID back — are sysfs writes, and a caller that has
    /// to rebuild this path is a caller that has to know the card's number.
    pub sysfs: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SinkIdentity {
    pub manufacturer: String,
    pub product: u16,
    pub serial: u32,
    pub name: Option<String>,
}

/// One HDMI or DisplayPort transmitter, as a platform device.
///
/// This is the anchor the whole topology hangs off: the CEC adapter is a child
/// of it, the sound card names it as its codec, and exactly one DRM connector
/// is driven by it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Controller {
    /// `fdea0000.hdmi` — the platform device's name.
    pub device: String,
    /// `hdmi1` if the device tree gives it an alias. Diagnostic only.
    pub alias: Option<String>,
    pub kind: ConnectorKind,
    /// Where the node sits in the address map. Used only to order controllers
    /// of the same kind, which is the one thing about them that is stable.
    pub address: Option<u64>,
    /// Whether the transmitter itself reports a cable. Measured, and the thing
    /// that makes a connector-to-controller match evidence rather than a guess.
    pub cable_present: Option<bool>,
    #[serde(skip)]
    pub sysfs: PathBuf,
    #[serde(skip)]
    pub of_node: Option<PathBuf>,
}

/// Every DRM device this machine has, with the connectors each one owns.
pub fn drm_devices(roots: &Roots) -> Vec<(DrmNode, Vec<Connector>)> {
    let class = roots.sys("class/drm");
    let mut cards: Vec<(DrmNode, Vec<Connector>)> = Vec::new();
    for entry in sorted_children(&class) {
        let name = file_name(&entry);
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let node = DrmNode {
            name: name.clone(),
            device: roots.dev(&format!("dri/{name}")),
            driver: link_basename(entry.join("device/driver")),
            parent: std::fs::canonicalize(entry.join("device"))
                .ok()
                .map(|path| file_name(&path)),
        };
        let mut connectors: Vec<Connector> = sorted_children(&class)
            .into_iter()
            .filter(|path| {
                let child = file_name(path);
                child.starts_with(&format!("{name}-"))
            })
            .map(|path| read_connector(&path))
            .collect();
        connectors.sort_by_key(|connector| (connector.kind as u8, connector.index));
        cards.push((node, connectors));
    }
    cards
}

fn read_connector(path: &Path) -> Connector {
    let full = file_name(path);
    let (kind, type_name, index) = ConnectorKind::from_connector_name(&full);
    let modes: Vec<String> = std::fs::read_to_string(path.join("modes"))
        .ok()
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let edid = std::fs::read(path.join("edid")).unwrap_or_default();
    Connector {
        name: if index > 0 {
            format!("{type_name}-{index}")
        } else {
            type_name
        },
        kind,
        index,
        connected: read_trimmed(path.join("status")).as_deref() == Some("connected"),
        enabled: read_trimmed(path.join("enabled")).as_deref() == Some("enabled"),
        preferred_mode: modes.first().cloned(),
        mode_count: modes.len(),
        sink: parse_edid_identity(&edid),
        sysfs: path.to_path_buf(),
    }
}

/// The three fields of an EDID base block that together identify a sink.
///
/// Deliberately not the whole EDID: what is wanted is something short, stable
/// and comparable, so that "the television in the living room" survives a
/// reboot that renumbers every DRM object on the board.
fn parse_edid_identity(edid: &[u8]) -> Option<SinkIdentity> {
    if edid.len() < 128 || edid[0..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }
    let packed = u16::from_be_bytes([edid[8], edid[9]]);
    let letter = |shift: u16| -> char {
        let value = ((packed >> shift) & 0x1F) as u8;
        (b'A' + value.saturating_sub(1)) as char
    };
    let manufacturer = format!("{}{}{}", letter(10), letter(5), letter(0));
    let product = u16::from_le_bytes([edid[10], edid[11]]);
    let serial = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);

    // Descriptor 0xFC is the monitor's own name, when it publishes one.
    let mut name = None;
    for block in 0..4 {
        let at = 54 + block * 18;
        if at + 18 > edid.len() {
            break;
        }
        if edid[at] == 0 && edid[at + 1] == 0 && edid[at + 2] == 0 && edid[at + 3] == 0xFC {
            let text: String = edid[at + 5..at + 18]
                .iter()
                .take_while(|byte| **byte != 0x0A)
                .map(|byte| *byte as char)
                .collect();
            let text = text.trim().to_string();
            if !text.is_empty() {
                name = Some(text);
            }
        }
    }
    Some(SinkIdentity {
        manufacturer,
        product,
        serial,
        name,
    })
}

/// The DRM device that can set a mode.
///
/// "Can set a mode" is asked as "does it own connectors", which sysfs answers
/// without opening the device. On this silicon the second DRM device is the
/// NPU: same driver family, same `/dev/dri` directory, no connectors and no
/// display. An override is honoured for diagnostics, and reported as one.
pub fn find_kms(roots: &Roots, forced: Option<&str>) -> Option<(DrmNode, Vec<Connector>)> {
    if let Some(forced) = forced {
        let wanted = file_name(Path::new(forced));
        if let Some(found) = drm_devices(roots)
            .into_iter()
            .find(|(node, _)| node.name == wanted)
        {
            return Some(found);
        }
    }
    drm_devices(roots)
        .into_iter()
        .find(|(_, connectors)| !connectors.is_empty())
}

/// The render device that belongs to the same hardware as `kms`.
///
/// Rendering and scanning out are two DRM devices on this board and the split
/// is the whole shape of the display path, but they are still one piece of
/// hardware: the render node whose parent device is the display subsystem is
/// the one the vendor GBM implementation drives. Picking it by number instead
/// selects the NPU on any board where the NPU probes first.
///
/// The consumer checks the result again at the moment it matters — a GBM device
/// created on it must report the vendor backend — because "same parent" is a
/// topology fact and "the vendor driver is actually behind it" is not.
pub fn find_render(roots: &Roots, kms: &DrmNode, forced: Option<&str>) -> Option<DrmNode> {
    if let Some(forced) = forced {
        let wanted = file_name(Path::new(forced));
        if let Some(found) = render_nodes(roots)
            .into_iter()
            .find(|node| node.name == wanted)
        {
            return Some(found);
        }
    }
    let nodes = render_nodes(roots);
    nodes
        .iter()
        .find(|node| node.parent.is_some() && node.parent == kms.parent)
        // A board that does not publish the parent link at all: fall back to
        // the same driver. Both sides must actually name one — two nodes that
        // each report no driver are not thereby the same hardware.
        .or_else(|| {
            nodes
                .iter()
                .find(|node| node.driver.is_some() && node.driver == kms.driver)
        })
        .cloned()
}

fn render_nodes(roots: &Roots) -> Vec<DrmNode> {
    let class = roots.sys("class/drm");
    sorted_children(&class)
        .into_iter()
        .filter(|path| file_name(path).starts_with("renderD"))
        .map(|path| {
            let name = file_name(&path);
            DrmNode {
                device: roots.dev(&format!("dri/{name}")),
                driver: link_basename(path.join("device/driver")),
                parent: std::fs::canonicalize(path.join("device"))
                    .ok()
                    .map(|path| file_name(&path)),
                name,
            }
        })
        .collect()
}

/// Every display transmitter on the board.
///
/// Found by what the device tree calls them rather than by driver name: a
/// platform device only exists for a node the board brought to `okay`, so this
/// is already the set of transmitters this board actually wired up. The Ultra
/// has one `hdmi@`, the Plus has two plus a `dp@`, and neither number is
/// written down anywhere.
pub fn controllers(roots: &Roots, dt: &DeviceTree) -> Vec<Controller> {
    let platform = roots.sys("devices/platform");
    let mut found: Vec<Controller> = Vec::new();
    for entry in sorted_children(&platform) {
        let of_node = std::fs::canonicalize(entry.join("of_node")).ok();
        let node_name = of_node.as_deref().map(file_name).unwrap_or_default();
        let kind = match node_name.split('@').next().unwrap_or_default() {
            "hdmi" => ConnectorKind::HdmiA,
            "dp" => ConnectorKind::DisplayPort,
            "edp" => ConnectorKind::EmbeddedDisplayPort,
            _ => continue,
        };
        let device = file_name(&entry);
        found.push(Controller {
            alias: of_node.as_deref().and_then(|node| dt.alias_of(node)),
            kind,
            address: unit_address(&node_name),
            cable_present: cable_present(&entry, kind),
            device,
            sysfs: entry,
            of_node,
        });
    }
    found.sort_by_key(|controller| {
        (
            controller.kind as u8,
            controller.address,
            controller.device.clone(),
        )
    });
    found
}

/// Whether the transmitter itself says a cable is in it.
///
/// The extcon device under a transmitter is the driver's own hotplug state, so
/// this is the transmitter's answer rather than the connector's. Comparing the
/// two is what turns "these are probably the same output" into evidence.
fn cable_present(device: &Path, kind: ConnectorKind) -> Option<bool> {
    let cable = kind.extcon_cable();
    if cable.is_empty() {
        return None;
    }
    for extcon in sorted_children(device.join("extcon")) {
        let Some(state) = read_trimmed(extcon.join("state")) else {
            continue;
        };
        for line in state.lines() {
            if let Some((name, value)) = line.split_once('=')
                && name.trim() == cable
            {
                return Some(value.trim() == "1");
            }
        }
    }
    None
}
