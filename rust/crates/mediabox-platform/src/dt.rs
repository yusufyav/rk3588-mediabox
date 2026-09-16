//! The device tree, as the kernel publishes it under sysfs.
//!
//! Two things are read from it and nothing else.
//!
//! *Phandles*, because that is the only link between an ALSA card and the HDMI
//! controller it carries audio for. A sound card's node names its codec by
//! phandle; resolving that phandle gives the controller, and the controller is
//! what a connector, a CEC adapter and a sound card all hang off. Without it
//! the mapping has to be a table of board names, which is the thing this whole
//! change exists to delete.
//!
//! *Unit addresses*, because when two controllers of the same kind have to be
//! put in an order, the address they are at is the one property of them that
//! does not move between boots, kernels or boards.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::roots::{Roots, sorted_children};

pub struct DeviceTree {
    base: PathBuf,
    phandles: HashMap<u32, PathBuf>,
}

impl DeviceTree {
    /// Index every node that has a phandle. The tree is a few thousand small
    /// files and this is one walk of it, done once per inspection.
    pub fn open(roots: &Roots) -> Self {
        let base = roots.sys("firmware/devicetree/base");
        let mut phandles = HashMap::new();
        if base.is_dir() {
            index(&base, &mut phandles, 0);
        }
        Self { base, phandles }
    }

    pub fn is_present(&self) -> bool {
        self.base.is_dir()
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// The node a phandle names, as a path under the tree's base.
    pub fn resolve(&self, phandle: u32) -> Option<&Path> {
        self.phandles.get(&phandle).map(PathBuf::as_path)
    }

    /// A `<u32>` property's first cell. Device-tree properties are big-endian.
    pub fn u32_property(&self, node: &Path, name: &str) -> Option<u32> {
        let bytes = std::fs::read(node.join(name)).ok()?;
        (bytes.len() >= 4).then(|| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A string property, with the trailing NUL taken off.
    pub fn string_property(&self, node: &Path, name: &str) -> Option<String> {
        let bytes = std::fs::read(node.join(name)).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim_end_matches('\0').trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    /// Follow a phandle property to the node it names.
    pub fn follow(&self, node: &Path, property: &str) -> Option<&Path> {
        self.resolve(self.u32_property(node, property)?)
    }

    /// What `/aliases` calls a node, if it calls it anything.
    ///
    /// This is how a board says "the controller at fdea0000 is hdmi1", and it
    /// is the same vocabulary the sound-card node names use, so it is worth
    /// carrying even though the phandle link is the one decisions are made on.
    pub fn alias_of(&self, node: &Path) -> Option<String> {
        let aliases = self.base.join("aliases");
        let wanted = node.strip_prefix(&self.base).ok()?;
        let wanted = format!("/{}", wanted.display());
        for entry in sorted_children(&aliases) {
            let name = crate::roots::file_name(&entry);
            if name == "name" {
                continue;
            }
            let Ok(value) = std::fs::read(&entry) else {
                continue;
            };
            let value = String::from_utf8_lossy(&value);
            if value.trim_end_matches('\0').trim() == wanted {
                return Some(name);
            }
        }
        None
    }
}

fn index(dir: &Path, into: &mut HashMap<u32, PathBuf>, depth: usize) {
    // The tree is shallow; the bound is only there so a symlink loop in a
    // captured fixture cannot hang an appliance service.
    if depth > 12 {
        return;
    }
    if let Ok(bytes) = std::fs::read(dir.join("phandle"))
        && bytes.len() >= 4
    {
        let phandle = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        into.entry(phandle).or_insert_with(|| dir.to_path_buf());
    }
    for child in sorted_children(dir) {
        if child.is_dir() && !child.is_symlink() {
            index(&child, into, depth + 1);
        }
    }
}

/// The address out of a device-tree node name: `hdmi@fdea0000` -> `0xfdea0000`.
///
/// Used only to put same-kind controllers in a fixed order. A board that names
/// its nodes without a unit address sorts by name instead, which is still
/// stable — it is just not an address.
pub fn unit_address(node_name: &str) -> Option<u64> {
    let (_, address) = node_name.split_once('@')?;
    u64::from_str_radix(address.trim(), 16).ok()
}
