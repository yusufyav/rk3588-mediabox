//! An EDID, as received, checked before anything is believed from it.
//!
//! Everything the display screen offers and everything a stored choice is
//! bound to is read out of these bytes, so a malformed EDID must not become a
//! capability. The rules are the E-EDID and CTA-861 structure, applied block by
//! block, with the answer stated rather than implied:
//!
//! * the base block must carry the fixed header and a checksum that sums to
//!   zero, or nothing in the EDID is used ([`EdidStatus::InvalidBase`]);
//! * each extension is checked on its own; one whose checksum fails is dropped
//!   and said to be dropped ([`EdidStatus::ValidWithDroppedExtensions`]) -- the
//!   kernel is laxer here (`edid_block_status_valid` tolerates a bad CTA
//!   checksum), and this product chooses not to reason from a block that is
//!   known to be corrupt;
//! * fewer bytes than the base block declares is [`EdidStatus::Truncated`]:
//!   the complete, valid blocks present are used, and the EDID has no identity;
//! * a CTA data block whose length runs past the data block collection ends
//!   the collection there (`cea_db_iter`), and is reported.
//!
//! Every read is bounded by the slice it reads from: no index here is trusted
//! from the input without a check.
//!
//! The EDID's identity is the SHA-256 of the exact bytes received, when they
//! are a whole, valid EDID. The checksum bytes the older identity was built
//! from (`edid_checkvalue`) are kept for compatibility and diagnostics only:
//! two different displays can share them.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const BLOCK: usize = 128;
const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
const CTA_EXTENSION: u8 = 0x02;

/// What was received, in one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdidStatus {
    /// No bytes at all: nothing is plugged in, or the driver published none.
    Absent,
    /// Not an EDID: short of a block, a wrong header, or a base checksum that
    /// does not sum to zero. Nothing in it is used.
    InvalidBase,
    /// Fewer bytes than the base block declares, or a partial block. The
    /// complete, valid blocks present are used; the EDID has no identity.
    Truncated,
    /// Whole, but one or more extensions failed their checksum and were
    /// dropped. What the rest declares is used.
    ValidWithDroppedExtensions,
    Valid,
}

impl EdidStatus {
    /// Whether anything in the EDID may be used at all.
    pub fn usable(self) -> bool {
        !matches!(self, EdidStatus::Absent | EdidStatus::InvalidBase)
    }
}

/// Something the parser noticed and did not use. For diagnostics: none of
/// these ever becomes a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum EdidIssue {
    ShortBase { bytes: usize },
    BadHeader,
    BadChecksum { block: usize },
    /// The base block declares `declared` extensions; `present` whole blocks
    /// followed it.
    MissingExtensions { declared: usize, present: usize },
    /// Bytes after the last whole block.
    PartialBlock { bytes: usize },
    /// Whole blocks beyond the count the base block declares. Not read.
    UndeclaredBlocks { count: usize },
    /// An extension this parser does not read (DisplayID, a block map, ...).
    UnknownExtension { block: usize, tag: u8 },
    /// A CTA extension older than revision 3 carries no data blocks
    /// (CTA-861-H s7.3.3); its capabilities are the byte-3 flags alone.
    CtaRevisionWithoutDataBlocks { block: usize, revision: u8 },
    /// The CTA detailed-timing offset `d` is outside 4..=127 (0 means none).
    CtaDtdOffset { block: usize, offset: u8 },
    /// A data block that claims more bytes than the collection has left;
    /// the collection is read up to it and no further.
    CtaDataBlockOverrun { block: usize, at: usize, length: usize },
    /// A data block of a known kind too short to be read.
    CtaDataBlockTooShort { block: usize, at: usize, kind: String },
}

/// The EDID's identity: SHA-256 of the exact bytes received, lower-case hex.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EdidIdentity(pub String);

impl std::fmt::Display for EdidIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An EDID, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edid {
    bytes: Vec<u8>,
    pub status: EdidStatus,
    pub issues: Vec<EdidIssue>,
    /// Indices of the extension blocks that passed their checksum.
    accepted: Vec<usize>,
}

impl Edid {
    pub fn parse(bytes: &[u8]) -> Self {
        let mut issues = Vec::new();
        let mut edid = Self {
            bytes: bytes.to_vec(),
            status: EdidStatus::Absent,
            issues: Vec::new(),
            accepted: Vec::new(),
        };
        if bytes.is_empty() {
            return edid;
        }
        if bytes.len() < BLOCK {
            issues.push(EdidIssue::ShortBase { bytes: bytes.len() });
            edid.status = EdidStatus::InvalidBase;
            edid.issues = issues;
            return edid;
        }
        let base = &bytes[..BLOCK];
        let mut invalid = false;
        if base[..8] != HEADER {
            issues.push(EdidIssue::BadHeader);
            invalid = true;
        }
        if !checksum_ok(base) {
            issues.push(EdidIssue::BadChecksum { block: 0 });
            invalid = true;
        }
        if invalid {
            edid.status = EdidStatus::InvalidBase;
            edid.issues = issues;
            return edid;
        }

        let declared = usize::from(base[126]);
        let whole = bytes.len() / BLOCK - 1;
        let partial = bytes.len() % BLOCK;
        let mut truncated = false;
        if partial != 0 {
            issues.push(EdidIssue::PartialBlock { bytes: partial });
            truncated = true;
        }
        if whole < declared {
            issues.push(EdidIssue::MissingExtensions {
                declared,
                present: whole,
            });
            truncated = true;
        }
        if whole > declared {
            issues.push(EdidIssue::UndeclaredBlocks {
                count: whole - declared,
            });
        }
        let mut dropped = false;
        for index in 1..=whole.min(declared) {
            let block = &bytes[index * BLOCK..(index + 1) * BLOCK];
            if !checksum_ok(block) {
                issues.push(EdidIssue::BadChecksum { block: index });
                dropped = true;
                continue;
            }
            if block[0] != CTA_EXTENSION {
                issues.push(EdidIssue::UnknownExtension {
                    block: index,
                    tag: block[0],
                });
            }
            edid.accepted.push(index);
        }
        edid.status = if truncated {
            EdidStatus::Truncated
        } else if dropped {
            EdidStatus::ValidWithDroppedExtensions
        } else {
            EdidStatus::Valid
        };
        edid.issues = issues;
        edid
    }

    /// The bytes exactly as received.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The base block, when the EDID has a valid one.
    pub fn base(&self) -> Option<&[u8]> {
        self.status.usable().then(|| &self.bytes[..BLOCK])
    }

    /// Every extension block that passed its checksum, in order.
    pub fn extensions(&self) -> impl Iterator<Item = (usize, &[u8])> {
        self.accepted
            .iter()
            .map(|&index| (index, &self.bytes[index * BLOCK..(index + 1) * BLOCK]))
    }

    /// The CTA-861 extensions among them.
    pub fn cta_extensions(&self) -> impl Iterator<Item = (usize, &[u8])> {
        self.extensions()
            .filter(|(_, block)| block[0] == CTA_EXTENSION)
    }

    /// SHA-256 of the exact bytes received, when they are a whole EDID
    /// whose every block was checked: [`EdidStatus::Valid`] or
    /// [`EdidStatus::ValidWithDroppedExtensions`]. A truncated read has no
    /// identity -- the next read of the same display may well differ.
    pub fn identity(&self) -> Option<EdidIdentity> {
        matches!(
            self.status,
            EdidStatus::Valid | EdidStatus::ValidWithDroppedExtensions
        )
        .then(|| sha256_hex(&self.bytes))
    }

    /// The checksum byte of every whole block, as hex: the older identity
    /// (`mediabox_core::edid_checkvalue`). Compatibility and diagnostics only.
    pub fn legacy_checkvalue(&self) -> String {
        mediabox_core::edid_checkvalue(&self.bytes)
    }
}

fn checksum_ok(block: &[u8]) -> bool {
    block.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

pub fn sha256_hex(bytes: &[u8]) -> EdidIdentity {
    let digest = Sha256::digest(bytes);
    EdidIdentity(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

// ------------------------------------------------------------ CTA-861

/// The HDMI Licensing VSDB (OUI 00-0C-03), as `drm_parse_hdmi_vsdb_video` and
/// `drm_parse_hdmi_deep_color_info` read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HdmiVsdb {
    /// The source physical address the sink gave this input (`a.b.c.d`
    /// packed into sixteen bits), which is also what the CEC adapter is told.
    pub physical_address: u16,
    pub dc_30bit: bool,
    pub dc_36bit: bool,
    pub dc_48bit: bool,
    /// Deep colour applies to 4:4:4 too.
    pub dc_y444: bool,
    /// Max_TMDS_Clock, when declared.
    pub max_tmds_khz: Option<u32>,
}

/// Which block carried the HDMI Forum's sink capability structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForumBlock {
    /// HF-VSDB: vendor-specific, OUI C4-5D-D8.
    Vsdb,
    /// HF-SCDB: CTA extended tag 0x79, same structure.
    Scdb,
}

/// The HDMI Forum sink capability data structure, as
/// `drm_parse_hdmi_forum_scds` and `drm_parse_ycbcr420_deep_color_info` read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HdmiForum {
    pub block: ForumBlock,
    pub version: u8,
    /// Max_TMDS_Character_Rate, when declared.
    pub max_tmds_character_rate_khz: Option<u32>,
    pub scdc_present: bool,
    pub dc_420_30bit: bool,
    pub dc_420_36bit: bool,
    pub dc_420_48bit: bool,
}

/// The HDR Static Metadata Data Block (CTA extended tag 0x06): the transfer
/// functions and metadata the sink says it takes. Parsed, not acted on: what
/// may be sent is policy and lives elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HdrStaticMetadata {
    pub eotf_traditional_sdr: bool,
    pub eotf_traditional_hdr: bool,
    pub eotf_st2084: bool,
    pub eotf_hlg: bool,
    /// Static Metadata Type 1 (the one HDR10 carries).
    pub static_metadata_type1: bool,
    /// Desired content max luminance, max frame-average and min luminance,
    /// as coded values, where declared.
    pub max_luminance: Option<u8>,
    pub max_frame_average: Option<u8>,
    pub min_luminance: Option<u8>,
}

/// The Colorimetry Data Block (CTA extended tag 0x05).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colorimetry {
    pub xvycc_601: bool,
    pub xvycc_709: bool,
    pub sycc_601: bool,
    pub opycc_601: bool,
    pub oprgb: bool,
    pub bt2020_cycc: bool,
    pub bt2020_ycc: bool,
    pub bt2020_rgb: bool,
    pub dci_p3: bool,
}

/// Everything the CTA-861 extensions declare that this product reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CtaCapabilities {
    /// The first CTA extension's revision.
    pub revision: Option<u8>,
    /// There is a CTA extension at all, which implies RGB.
    pub present: bool,
    /// Byte 3 of a CTA extension.
    pub ycbcr444: bool,
    pub ycbcr422: bool,
    /// Short video descriptors, as VICs, in order.
    pub svds: Vec<u8>,
    pub hdmi_vsdb: Option<HdmiVsdb>,
    pub hdmi_forum: Option<HdmiForum>,
    /// Y420VDB: the VICs taken only as 4:2:0.
    pub y420_only: Vec<u8>,
    /// Y420CMDB: the VICs of the video data block also taken as 4:2:0.
    pub y420_also: Vec<u8>,
    pub hdr_static: Option<HdrStaticMetadata>,
    pub colorimetry: Option<Colorimetry>,
}

/// `svd_to_vic`: a native flag in bit 7 only for codes below 65.
pub(crate) fn svd_to_vic(svd: u8) -> u8 {
    if (129..=192).contains(&svd) {
        svd & 0x7F
    } else {
        svd
    }
}

/// One data block of a CTA extension: its tag, its payload, and where it sat.
pub(crate) struct DataBlock<'a> {
    pub tag: u8,
    pub payload: &'a [u8],
    pub at: usize,
}

/// The data blocks of one CTA extension, as `cea_db_iter` walks them: only
/// from revision 3, only inside the collection `4..d`, and stopping at the
/// first block that claims more than the collection has left.
pub(crate) fn data_blocks<'a>(
    index: usize,
    block: &'a [u8],
    issues: &mut Vec<EdidIssue>,
) -> Vec<DataBlock<'a>> {
    let mut out = Vec::new();
    if block.len() != BLOCK {
        return out;
    }
    let revision = block[1];
    if revision < 3 {
        issues.push(EdidIssue::CtaRevisionWithoutDataBlocks {
            block: index,
            revision,
        });
        return out;
    }
    let d = block[2];
    if d != 0 && !(4..=127).contains(&d) {
        issues.push(EdidIssue::CtaDtdOffset {
            block: index,
            offset: d,
        });
        return out;
    }
    if d < 4 {
        return out;
    }
    let end = usize::from(d);
    let mut at = 4usize;
    while at < end {
        let header = block[at];
        let length = usize::from(header & 0x1F);
        if at + 1 + length > end {
            issues.push(EdidIssue::CtaDataBlockOverrun {
                block: index,
                at,
                length,
            });
            break;
        }
        out.push(DataBlock {
            tag: header >> 5,
            payload: &block[at + 1..at + 1 + length],
            at,
        });
        at += 1 + length;
    }
    out
}

/// The detailed timing descriptors of one CTA extension: from `d` in steps of
/// 18 while a whole descriptor fits before the checksum.
pub(crate) fn cta_dtds(block: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if block.len() != BLOCK {
        return out;
    }
    let d = usize::from(block[2]);
    if !(4..=127).contains(&d) {
        return out;
    }
    let mut at = d;
    while at + 18 <= 127 {
        out.push(&block[at..at + 18]);
        at += 18;
    }
    out
}

const HDMI_OUI: [u8; 3] = [0x03, 0x0C, 0x00];
const HDMI_FORUM_OUI: [u8; 3] = [0xD8, 0x5D, 0xC4];

impl CtaCapabilities {
    /// Read every CTA extension the EDID's checks let through.
    pub fn read(edid: &Edid) -> (Self, Vec<EdidIssue>) {
        let mut caps = Self::default();
        let mut issues = Vec::new();
        let mut cmdb: Option<u64> = None;
        for (index, block) in edid.cta_extensions() {
            caps.present = true;
            caps.revision.get_or_insert(block[1]);
            // drm_parse_cea_ext reads byte 3 of every CTA extension.
            caps.ycbcr444 |= block[3] & 0x20 != 0;
            caps.ycbcr422 |= block[3] & 0x10 != 0;
            for db in data_blocks(index, block, &mut issues) {
                let short = |kind: &str, issues: &mut Vec<EdidIssue>| {
                    issues.push(EdidIssue::CtaDataBlockTooShort {
                        block: index,
                        at: db.at,
                        kind: kind.to_string(),
                    })
                };
                let payload = db.payload;
                match db.tag {
                    2 => caps.svds.extend(payload.iter().map(|svd| svd_to_vic(*svd))),
                    3 if payload.len() >= 3 && payload[..3] == HDMI_OUI => {
                        // cea_db_is_hdmi_vsdb: at least five payload bytes.
                        if payload.len() < 5 {
                            short("HDMI VSDB", &mut issues);
                            continue;
                        }
                        if caps.hdmi_vsdb.is_some() {
                            continue;
                        }
                        let dc = payload.get(5).copied().unwrap_or(0);
                        caps.hdmi_vsdb = Some(HdmiVsdb {
                            physical_address: u16::from_be_bytes([payload[3], payload[4]]),
                            dc_30bit: dc & 0x10 != 0,
                            dc_36bit: dc & 0x20 != 0,
                            dc_48bit: dc & 0x40 != 0,
                            dc_y444: dc & 0x08 != 0,
                            max_tmds_khz: payload
                                .get(6)
                                .filter(|rate| **rate != 0)
                                .map(|rate| u32::from(*rate) * 5_000),
                        });
                    }
                    3 if payload.len() >= 3 && payload[..3] == HDMI_FORUM_OUI => {
                        // cea_db_is_hdmi_forum_vsdb: at least seven.
                        if payload.len() < 7 {
                            short("HF-VSDB", &mut issues);
                            continue;
                        }
                        if caps.hdmi_forum.is_none() {
                            caps.hdmi_forum = Some(forum(ForumBlock::Vsdb, payload));
                        }
                    }
                    7 if !payload.is_empty() => match payload[0] {
                        0x05 => {
                            if payload.len() < 3 {
                                short("Colorimetry", &mut issues);
                                continue;
                            }
                            let (a, b) = (payload[1], payload[2]);
                            caps.colorimetry = Some(Colorimetry {
                                xvycc_601: a & 0x01 != 0,
                                xvycc_709: a & 0x02 != 0,
                                sycc_601: a & 0x04 != 0,
                                opycc_601: a & 0x08 != 0,
                                oprgb: a & 0x10 != 0,
                                bt2020_cycc: a & 0x20 != 0,
                                bt2020_ycc: a & 0x40 != 0,
                                bt2020_rgb: a & 0x80 != 0,
                                dci_p3: b & 0x80 != 0,
                            });
                        }
                        0x06 => {
                            // cea_db_is_hdmi_hdr_metadata_block: at least three.
                            if payload.len() < 3 {
                                short("HDR Static Metadata", &mut issues);
                                continue;
                            }
                            let eotf = payload[1];
                            caps.hdr_static = Some(HdrStaticMetadata {
                                eotf_traditional_sdr: eotf & 0x01 != 0,
                                eotf_traditional_hdr: eotf & 0x02 != 0,
                                eotf_st2084: eotf & 0x04 != 0,
                                eotf_hlg: eotf & 0x08 != 0,
                                static_metadata_type1: payload[2] & 0x01 != 0,
                                max_luminance: payload.get(3).copied(),
                                max_frame_average: payload.get(4).copied(),
                                min_luminance: payload.get(5).copied(),
                            });
                        }
                        // parse_cta_y420vdb
                        0x0E => caps
                            .y420_only
                            .extend(payload[1..].iter().map(|svd| svd_to_vic(*svd))),
                        // parse_cta_y420cmdb: an empty map means every SVD.
                        0x0F => {
                            let bytes = &payload[1..];
                            let mut map = 0u64;
                            if bytes.is_empty() {
                                map = u64::MAX;
                            } else {
                                for (index, byte) in bytes.iter().take(8).enumerate() {
                                    map |= u64::from(*byte) << (8 * index);
                                }
                            }
                            cmdb = Some(cmdb.unwrap_or(0) | map);
                        }
                        // cea_db_is_hdmi_forum_scdb: at least seven.
                        0x79 => {
                            if payload.len() < 7 {
                                short("HF-SCDB", &mut issues);
                                continue;
                            }
                            if caps.hdmi_forum.is_none() {
                                caps.hdmi_forum = Some(forum(ForumBlock::Scdb, payload));
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        if let Some(map) = cmdb {
            for (index, vic) in caps.svds.iter().enumerate().take(64) {
                if map & (1u64 << index) != 0 {
                    caps.y420_also.push(*vic);
                }
            }
        }
        (caps, issues)
    }

    /// `display_info.max_tmds_clock`: the VSDB's Max_TMDS_Clock, replaced by
    /// the Forum structure's Max_TMDS_Character_Rate when that is above
    /// 340 MHz. Zero when neither was declared.
    pub fn max_tmds_khz(&self) -> u32 {
        let vsdb = self.hdmi_vsdb.and_then(|vsdb| vsdb.max_tmds_khz).unwrap_or(0);
        match self.hdmi_forum.and_then(|forum| forum.max_tmds_character_rate_khz) {
            Some(rate) if rate > 340_000 => rate,
            _ => vsdb,
        }
    }
}

/// The Forum structure, at the same offsets in both blocks: the payload's
/// first three bytes are the OUI in the VSDB and the extended tag plus two
/// reserved bytes in the SCDB.
fn forum(block: ForumBlock, payload: &[u8]) -> HdmiForum {
    let dc = payload[6];
    HdmiForum {
        block,
        version: payload[3],
        max_tmds_character_rate_khz: (payload[4] != 0).then(|| u32::from(payload[4]) * 5_000),
        scdc_present: payload[5] & 0x80 != 0,
        dc_420_30bit: dc & 0x01 != 0,
        dc_420_36bit: dc & 0x02 != 0,
        dc_420_48bit: dc & 0x04 != 0,
    }
}

/// An EDID, read: what it is, what it declares, and what was set aside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdidReport {
    pub status: EdidStatus,
    pub bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<EdidIdentity>,
    /// `edid_checkvalue`, for comparison with what older records hold.
    pub legacy_checkvalue: String,
    pub cta: CtaCapabilities,
    pub issues: Vec<EdidIssue>,
}

impl EdidReport {
    pub fn of(bytes: &[u8]) -> Self {
        let edid = Edid::parse(bytes);
        let (cta, mut issues) = CtaCapabilities::read(&edid);
        let mut all = edid.issues.clone();
        all.append(&mut issues);
        Self {
            status: edid.status,
            bytes: bytes.len(),
            sha256: edid.identity(),
            legacy_checkvalue: edid.legacy_checkvalue(),
            cta,
            issues: all,
        }
    }
}
