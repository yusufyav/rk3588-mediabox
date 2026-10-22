//! The receiver's firmware, read at run time from the owner's own files and
//! checked against the digests the hardware was verified with.
//!
//! None of it is in this repository, and none of it may be: the AVL62X1 patch
//! script and the RDA5815M tables live in the Kanky Link application's
//! `libdvb_usb_phy.so` (arm64-v8a, 2.103/2.109/2.110 -- read as data, never
//! loaded), the bridge firmware is Linux's `dvb-usb-it9303-01.fw`. They are put
//! in the firmware directory by whoever owns the device (see
//! docs/platform/dvb.md) and refused here unless they are exactly those files.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::sha256;

pub const DEFAULT_DIR: &str = "/var/lib/mediabox-dvb/firmware";
pub const LIBRARY: &str = "libdvb_usb_phy.so";
pub const BRIDGE_FIRMWARE: &str = "dvb-usb-it9303-01.fw";

/// Kanky Link 2.103/2.109 `ucAvlPatchData` (2.110 carries the same bytes).
const KNOWN_PATCH: (&str, usize) = ("4373f391a533a3735f11ec20e0067d4a1c1850191710d70d2d741ea4b498e8d3", 49636);

/// RDA5815M init tables in .rodata (vaddr == file offset): pairs and digest.
const TABLE_A: (usize, &str) = (139, "01a9eb278082190d072f270de8cbd24b1fe01f558c39e745115af6ee7dae1d69");
const TABLE_B: (usize, &str) = (99, "c0d32249e742ee0fccd1f2666f327af4e7ab95c20a0934a327a8608e6152345f");
/// Table contents are byte-identical across releases; only .rodata moved.
const TABLE_OFFSETS: [(&str, usize, usize); 2] = [("Kanky 2.109", 0x2B408, 0x2B51E), ("Kanky 2.110", 0x2B4C8, 0x2B5DE)];

const BRIDGE_SHA256: &str = "5213a5a38872661277a2cc1b2dfdfe88faf06f41205f460f3b51857f0568b484";
const BRIDGE_SIZE: usize = 2169;
const MAX_BLOCK: usize = 58;

#[derive(Debug, Clone)]
pub struct TunerTables {
    pub a: Vec<(u8, u8)>,
    pub b: Vec<(u8, u8)>,
    pub release: &'static str,
}

/// Everything the receiver needs to come up, verified.
#[derive(Debug, Clone)]
pub struct Firmware {
    pub patch: Vec<u8>,
    pub tables: TunerTables,
    /// The bridge firmware's scatter-write blocks; None when the file is not
    /// there (a device that is already WARM does not need it).
    pub bridge_blocks: Option<Vec<Vec<u8>>>,
}

impl Firmware {
    pub fn load(dir: &Path) -> Result<Self> {
        let library = dir.join(LIBRARY);
        let image = std::fs::read(&library)
            .map_err(|error| Error::Firmware(format!("{} okunamadı: {error}", library.display())))?;
        let patch = avl62x1_patch(&image)?;
        let tables = rda5815m_tables(&image)?;
        let bridge = dir.join(BRIDGE_FIRMWARE);
        let bridge_blocks = match std::fs::read(&bridge) {
            Ok(data) => Some(bridge_blocks(&data)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::Firmware(format!("{} okunamadı: {error}", bridge.display()))),
        };
        Ok(Self { patch, tables, bridge_blocks })
    }

    pub fn paths(dir: &Path) -> [PathBuf; 2] {
        [dir.join(LIBRARY), dir.join(BRIDGE_FIRMWARE)]
    }
}

fn u16le(image: &[u8], at: usize) -> Result<u16> {
    image.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(truncated)
}
fn u32le(image: &[u8], at: usize) -> Result<u32> {
    image.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(truncated)
}
fn u64le(image: &[u8], at: usize) -> Result<u64> {
    image
        .get(at..at + 8)
        .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        .ok_or_else(truncated)
}
fn truncated() -> Error {
    Error::Firmware("ELF truncated".into())
}

/// The file bytes of a dynamic symbol in a little-endian ELF64 library.
pub fn elf_symbol<'i>(image: &'i [u8], wanted: &str) -> Result<&'i [u8]> {
    if image.get(..4) != Some(b"\x7fELF") || image.get(4) != Some(&2) || image.get(5) != Some(&1) {
        return Err(Error::Firmware("only little-endian ELF64 libraries are supported".into()));
    }
    let phoff = u64le(image, 0x20)? as usize;
    let shoff = u64le(image, 0x28)? as usize;
    let phentsize = u16le(image, 0x36)? as usize;
    let phnum = u16le(image, 0x38)? as usize;
    let shentsize = u16le(image, 0x3A)? as usize;
    let shnum = u16le(image, 0x3C)? as usize;
    // Section header: name, type, flags, addr, offset, size, link, info, align, entsize.
    let section = |i: usize| -> Result<(u32, u64, u64, u32, u64)> {
        let at = shoff + i * shentsize;
        Ok((u32le(image, at + 4)?, u64le(image, at + 24)?, u64le(image, at + 32)?, u32le(image, at + 40)?, u64le(image, at + 56)?))
    };
    let dynsym = (0..shnum)
        .map(section)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|s| s.0 == 11) // SHT_DYNSYM
        .ok_or_else(|| Error::Firmware("no .dynsym".into()))?;
    let (_, sym_offset, sym_size, link, sym_entsize) = dynsym;
    let strtab = section(link as usize)?;
    let name_at = |offset: u32| -> Option<&[u8]> {
        let start = strtab.1 as usize + offset as usize;
        let rest = image.get(start..)?;
        Some(&rest[..rest.iter().position(|&b| b == 0)?])
    };
    let mut found = None;
    for i in 0..(sym_size / sym_entsize.max(1)) as usize {
        let at = sym_offset as usize + i * sym_entsize as usize;
        let name = u32le(image, at)?;
        if name != 0 && name_at(name) == Some(wanted.as_bytes()) {
            found = Some((u64le(image, at + 8)?, u64le(image, at + 16)?));
            break;
        }
    }
    let (value, size) = found.ok_or_else(|| Error::Firmware(format!("symbol {wanted} not found")))?;
    for i in 0..phnum {
        let at = phoff + i * phentsize;
        let (kind, offset, vaddr, filesz) = (u32le(image, at)?, u64le(image, at + 8)?, u64le(image, at + 16)?, u64le(image, at + 32)?);
        if kind == 1 && vaddr <= value && value + size <= vaddr + filesz {
            let start = (value - vaddr + offset) as usize;
            return image.get(start..start + size as usize).ok_or_else(truncated);
        }
    }
    Err(Error::Firmware(format!("symbol {wanted} is not backed by file data")))
}

pub fn avl62x1_patch(image: &[u8]) -> Result<Vec<u8>> {
    let data = elf_symbol(image, "ucAvlPatchData")?;
    let digest = sha256::hex(data);
    if digest != KNOWN_PATCH.0 || data.len() != KNOWN_PATCH.1 {
        return Err(Error::Firmware(format!("unknown AVL62X1 patch (sha256 {digest}, {} bytes)", data.len())));
    }
    Ok(data.to_vec())
}

pub fn rda5815m_tables(image: &[u8]) -> Result<TunerTables> {
    let table = |offset: usize, (pairs, digest): (usize, &str)| -> Option<Vec<(u8, u8)>> {
        let raw = image.get(offset..offset + 2 * pairs)?;
        (sha256::hex(raw) == digest).then(|| raw.as_chunks::<2>().0.iter().map(|&[reg, value]| (reg, value)).collect())
    };
    for (release, a, b) in TABLE_OFFSETS {
        if let (Some(a), Some(b)) = (table(a, TABLE_A), table(b, TABLE_B)) {
            return Ok(TunerTables { a, b, release });
        }
    }
    Err(Error::Firmware("RDA5815M tables do not match Kanky 2.109/2.110 (sha256 mismatch)".into()))
}

/// The bridge firmware, split the way Linux af9035 scatter-writes it.
pub fn bridge_blocks(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    if data.len() != BRIDGE_SIZE {
        return Err(Error::Firmware("unexpected bridge firmware size".into()));
    }
    if data[..4] != [0x03, 0x00, 0x00, 0x03] {
        return Err(Error::Firmware("unexpected bridge firmware header".into()));
    }
    if sha256::hex(data) != BRIDGE_SHA256 {
        return Err(Error::Firmware("bridge firmware SHA256 mismatch".into()));
    }
    let mut offsets = vec![0];
    for i in 7..data.len() - 2 {
        if data[i] == 3 && matches!(data[i + 1], 0 | 1) && data[i + 2] == 0 {
            offsets.push(i);
        }
    }
    offsets.push(data.len());
    let blocks: Vec<Vec<u8>> = offsets.windows(2).map(|pair| data[pair[0]..pair[1]].to_vec()).collect();
    if blocks.is_empty() || blocks.iter().any(|block| block.is_empty() || block.len() > MAX_BLOCK) {
        return Err(Error::Firmware("invalid bridge firmware block sizes".into()));
    }
    Ok(blocks)
}
