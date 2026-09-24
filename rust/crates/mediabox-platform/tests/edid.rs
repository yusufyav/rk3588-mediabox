//! EDIDs built byte by byte, and what the parser must make of them.
//!
//! Every expected value is the CTA-861 / E-EDID structure or the kernel's
//! reading of it (drivers/gpu/drm/drm_edid.c), not what some display happens
//! to send: the builder writes a data block, the test says what that block
//! means. The captured EDIDs of a real television are in tests/output.rs and
//! tests/color_modes.rs, for provenance.

use mediabox_core::{ColorFormat, ColorMode, Refusal};
use mediabox_platform::edid::{
    CtaCapabilities, Edid, EdidIssue, EdidReport, EdidStatus, ForumBlock,
};
use mediabox_platform::video::{HdrRefusal, RK3588_HDMI, Timing, parse_sink_video, parse_timings};

// ------------------------------------------------------------ the builder

/// A CTA-861 extension, block by block.
#[derive(Clone)]
struct Cta {
    revision: u8,
    byte3: u8,
    blocks: Vec<Vec<u8>>,
    dtd_offset: Option<u8>,
}

impl Cta {
    fn new() -> Self {
        // Revision 3, YCbCr 4:4:4 and 4:2:2 declared.
        Self { revision: 3, byte3: 0x30, blocks: Vec::new(), dtd_offset: None }
    }
    fn revision(mut self, revision: u8) -> Self {
        self.revision = revision;
        self
    }
    fn byte3(mut self, byte3: u8) -> Self {
        self.byte3 = byte3;
        self
    }
    fn block(mut self, tag: u8, payload: &[u8]) -> Self {
        let mut block = vec![(tag << 5) | payload.len() as u8];
        block.extend_from_slice(payload);
        self.blocks.push(block);
        self
    }
    fn raw(mut self, bytes: &[u8]) -> Self {
        self.blocks.push(bytes.to_vec());
        self
    }
    fn svds(self, vics: &[u8]) -> Self {
        self.block(2, vics)
    }
    /// HDMI VSDB: physical address, deep colour byte, Max_TMDS_Clock / 5 MHz.
    fn hdmi_vsdb(self, pa: u16, dc: u8, max_5mhz: u8) -> Self {
        let [a, b] = pa.to_be_bytes();
        self.block(3, &[0x03, 0x0C, 0x00, a, b, dc, max_5mhz])
    }
    /// HF-VSDB: rate / 5 MHz, SCDC flags byte, DC_420 byte.
    fn hf_vsdb(self, rate_5mhz: u8, scdc: u8, dc420: u8) -> Self {
        self.block(3, &[0xD8, 0x5D, 0xC4, 0x01, rate_5mhz, scdc, dc420])
    }
    /// HF-SCDB: the same structure behind extended tag 0x79.
    fn hf_scdb(self, rate_5mhz: u8, scdc: u8, dc420: u8) -> Self {
        self.block(7, &[0x79, 0x00, 0x00, 0x01, rate_5mhz, scdc, dc420])
    }
    fn hdr_static(self, eotf: u8, descriptors: u8) -> Self {
        self.block(7, &[0x06, eotf, descriptors])
    }
    fn colorimetry(self, a: u8, b: u8) -> Self {
        self.block(7, &[0x05, a, b])
    }
    fn y420vdb(self, vics: &[u8]) -> Self {
        let mut payload = vec![0x0E];
        payload.extend_from_slice(vics);
        self.block(7, &payload)
    }
    fn y420cmdb(self, map: &[u8]) -> Self {
        let mut payload = vec![0x0F];
        payload.extend_from_slice(map);
        self.block(7, &payload)
    }
    fn dtd_offset(mut self, d: u8) -> Self {
        self.dtd_offset = Some(d);
        self
    }
    fn build(&self) -> Vec<u8> {
        let mut block = vec![0u8; 128];
        block[0] = 0x02;
        block[1] = self.revision;
        block[3] = self.byte3;
        let mut at = 4;
        for db in &self.blocks {
            block[at..at + db.len()].copy_from_slice(db);
            at += db.len();
        }
        block[2] = self.dtd_offset.unwrap_or(at as u8);
        checksum(&mut block);
        block
    }
}

/// A base block (E-EDID 1.3, one 1080p60 detailed timing) and extensions.
fn edid(extensions: &[Vec<u8>]) -> Vec<u8> {
    let mut base = vec![0u8; 128];
    base[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    base[8..10].copy_from_slice(&[0x04, 0x21]); // "AAA"
    base[10] = 0x01;
    base[18] = 1;
    base[19] = 3;
    // CTA-861 1080p60 as a detailed timing, +h +v sync.
    base[54..72].copy_from_slice(&[
        0x02, 0x3A, 0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58, 0x2C, 0x45, 0x00, 0x9F, 0x29,
        0x53, 0x00, 0x00, 0x1E,
    ]);
    base[126] = extensions.len() as u8;
    checksum(&mut base);
    let mut out = base;
    for extension in extensions {
        out.extend_from_slice(extension);
    }
    out
}

fn checksum(block: &mut [u8]) {
    let sum = block[..127].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    block[127] = 0u8.wrapping_sub(sum);
}

fn uhd60() -> Timing {
    Timing::new(3840, 2160, 594_000, 4400, 2250, false, false)
}
fn uhd30() -> Timing {
    Timing::new(3840, 2160, 297_000, 4400, 2250, false, false)
}

const DC_30: u8 = 0x10;
const DC_36: u8 = 0x20;
const DC_Y444: u8 = 0x08;
const EOTF_SDR: u8 = 0x01;
const EOTF_PQ: u8 = 0x04;
const EOTF_HLG: u8 = 0x08;

// ------------------------------------------------------------ capabilities

#[test]
fn hdmi20_600mhz_hdr10_bt2020() {
    let bytes = edid(&[Cta::new()
        .svds(&[16, 95, 97])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x03)
        .hdr_static(EOTF_SDR | EOTF_PQ | EOTF_HLG, 0x01)
        .colorimetry(0xC0, 0x00)
        .y420cmdb(&[0x04])
        .build()]);
    let report = EdidReport::of(&bytes);
    assert_eq!(report.status, EdidStatus::Valid);
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    assert!(report.sha256.is_some());
    let cta = &report.cta;
    assert_eq!(cta.revision, Some(3));
    assert_eq!(cta.svds, vec![16, 95, 97]);

    let vsdb = cta.hdmi_vsdb.expect("HDMI VSDB");
    assert_eq!(vsdb.physical_address, 0x1000);
    assert!(vsdb.dc_30bit && vsdb.dc_36bit && !vsdb.dc_48bit && vsdb.dc_y444);
    assert_eq!(vsdb.max_tmds_khz, Some(340_000));

    let forum = cta.hdmi_forum.expect("HF-VSDB");
    assert_eq!(forum.block, ForumBlock::Vsdb);
    assert_eq!(forum.max_tmds_character_rate_khz, Some(600_000));
    assert!(forum.scdc_present);
    assert!(forum.dc_420_30bit && forum.dc_420_36bit && !forum.dc_420_48bit);
    // Above 340 MHz the Forum's rate replaces the VSDB's (drm_parse_hdmi_forum_scds).
    assert_eq!(cta.max_tmds_khz(), 600_000);

    // Transfer functions, metadata type and colorimetry: each in its own field.
    let hdr = cta.hdr_static.expect("HDR static metadata");
    assert!(hdr.eotf_traditional_sdr && !hdr.eotf_traditional_hdr);
    assert!(hdr.eotf_st2084 && hdr.eotf_hlg);
    assert!(hdr.static_metadata_type1);
    let colour = cta.colorimetry.expect("colorimetry");
    assert!(colour.bt2020_rgb && colour.bt2020_ycc && !colour.bt2020_cycc);
    assert!(!colour.dci_p3 && !colour.xvycc_709);

    // Y420CMDB bit 2 is the third SVD: VIC 97.
    assert_eq!(cta.y420_also, vec![97]);

    let sink = parse_sink_video(&bytes).unwrap();
    assert_eq!(sink.max_character_rate_khz, 600_000);
    assert_eq!(sink.rgb_deep, vec![10, 12]);
    assert_eq!(sink.ycbcr444_deep, vec![10, 12]);
    assert_eq!(sink.ycbcr420_deep, vec![10, 12]);
    assert!(sink.st2084 && sink.hlg);
    assert_eq!(sink.refusal(&uhd60(), ColorMode::new(ColorFormat::Rgb, 8), &RK3588_HDMI), None);
}

#[test]
fn hf_scdb_carries_the_same_structure_as_hf_vsdb() {
    let bytes = edid(&[Cta::new()
        .hdmi_vsdb(0x2000, 0, 68)
        .hf_scdb(120, 0x80, 0x01)
        .build()]);
    let (cta, issues) = CtaCapabilities::read(&Edid::parse(&bytes));
    assert!(issues.is_empty(), "{issues:?}");
    let forum = cta.hdmi_forum.expect("HF-SCDB");
    assert_eq!(forum.block, ForumBlock::Scdb);
    assert_eq!(forum.max_tmds_character_rate_khz, Some(600_000));
    assert!(forum.dc_420_30bit && !forum.dc_420_36bit);
    assert_eq!(cta.max_tmds_khz(), 600_000);
}

#[test]
fn hdmi14_no_deep_color() {
    let bytes = edid(&[Cta::new().svds(&[16, 95]).hdmi_vsdb(0x1000, 0, 60).build()]);
    let cta = EdidReport::of(&bytes).cta;
    assert!(cta.hdmi_forum.is_none());
    assert_eq!(cta.max_tmds_khz(), 300_000);
    let sink = parse_sink_video(&bytes).unwrap();
    assert!(sink.is_hdmi);
    assert!(sink.rgb_deep.is_empty() && sink.ycbcr444_deep.is_empty());
    assert!(!sink.st2084);
    // 4K30 at eight bits is 297 MHz; ten would be undeclared.
    assert_eq!(sink.refusal(&uhd30(), ColorMode::new(ColorFormat::Rgb, 8), &RK3588_HDMI), None);
    assert_eq!(
        sink.refusal(&uhd30(), ColorMode::new(ColorFormat::Rgb, 10), &RK3588_HDMI),
        Some(Refusal::DepthNotDeclared { bits: 10 })
    );
    // 4K60 is 594 MHz, over the 300 the sink declared.
    assert!(matches!(
        sink.refusal(&uhd60(), ColorMode::new(ColorFormat::Rgb, 8), &RK3588_HDMI),
        Some(Refusal::OverSink { max_khz: 300_000, .. })
    ));
}

#[test]
fn sdr_deep_colour_no_hdr() {
    let bytes = edid(&[Cta::new().svds(&[16]).hdmi_vsdb(0x1000, DC_30 | DC_36, 68).build()]);
    let report = EdidReport::of(&bytes);
    assert!(report.cta.hdr_static.is_none());
    assert!(report.cta.colorimetry.is_none());
    let sink = parse_sink_video(&bytes).unwrap();
    assert_eq!(sink.rgb_deep, vec![10, 12]);
    // Deep colour without DC_Y444 is RGB's alone.
    assert!(sink.ycbcr444_deep.is_empty());
    assert!(!sink.st2084 && !sink.hlg);
}

#[test]
fn y420_only_4k60() {
    let bytes = edid(&[Cta::new()
        .svds(&[16, 95])
        .hdmi_vsdb(0x1000, 0, 60)
        .y420vdb(&[97])
        .build()]);
    let cta = EdidReport::of(&bytes).cta;
    assert_eq!(cta.y420_only, vec![97]);
    assert!(!cta.svds.contains(&97));
    // do_y420vdb_modes: the mode is listed, and only as 4:2:0.
    let timings = parse_timings(&bytes);
    let uhd = timings.iter().find(|timing| timing.vic == Some(97)).expect("4K60 listed");
    let sink = parse_sink_video(&bytes).unwrap();
    assert_eq!(
        sink.refusal(uhd, ColorMode::new(ColorFormat::Rgb, 8), &RK3588_HDMI),
        Some(Refusal::Only420)
    );
    assert_eq!(sink.refusal(uhd, ColorMode::new(ColorFormat::Ycbcr420, 8), &RK3588_HDMI), None);
}

// ------------------------------------------------------------ input contract

#[test]
fn invalid_extension_checksum() {
    let mut cta = Cta::new().svds(&[16]).hdmi_vsdb(0x1000, DC_30 | DC_36, 68).build();
    cta[127] = cta[127].wrapping_add(1);
    let bytes = edid(&[cta]);
    let report = EdidReport::of(&bytes);
    assert_eq!(report.status, EdidStatus::ValidWithDroppedExtensions);
    assert!(report.issues.contains(&EdidIssue::BadChecksum { block: 1 }));
    // Nothing of the dropped block is believed: this is a DVI sink.
    assert!(!report.cta.present);
    let sink = parse_sink_video(&bytes).unwrap();
    assert!(!sink.is_hdmi && sink.rgb_deep.is_empty());
    assert_eq!(
        sink.refusal(&uhd30(), ColorMode::new(ColorFormat::Ycbcr444, 8), &RK3588_HDMI),
        Some(Refusal::NotHdmi)
    );
    // The bytes are still what the display sends, whole: they identify it.
    assert!(report.sha256.is_some());
}

#[test]
fn truncated_cta_block() {
    let whole = edid(&[Cta::new().svds(&[16]).hdmi_vsdb(0x1000, DC_30, 68).build()]);
    let bytes = &whole[..128 + 64];
    let report = EdidReport::of(bytes);
    assert_eq!(report.status, EdidStatus::Truncated);
    assert!(report.issues.contains(&EdidIssue::PartialBlock { bytes: 64 }));
    assert!(report.issues.contains(&EdidIssue::MissingExtensions { declared: 1, present: 0 }));
    assert!(report.sha256.is_none(), "a partial read has no identity");
    assert!(!report.cta.present);
    let sink = parse_sink_video(bytes).unwrap();
    assert!(!sink.is_hdmi);

    // Declared two, one arrived: the one that did is whole and valid, and is
    // read -- but the EDID is still truncated and still has no identity.
    let mut two = edid(&[Cta::new().hdmi_vsdb(0x1000, 0, 60).build()]);
    two[126] = 2;
    checksum(&mut two[..128]);
    let report = EdidReport::of(&two);
    assert_eq!(report.status, EdidStatus::Truncated);
    assert!(report.cta.hdmi_vsdb.is_some());
    assert!(report.sha256.is_none());
}

#[test]
fn malformed_data_block_length_stops_the_collection_there() {
    // A video data block, then a block claiming 31 bytes where 4 remain,
    // then an HDMI VSDB the collection never reaches.
    let cta = Cta::new()
        .svds(&[16])
        .raw(&[0x7F, 0x06, 0x01, 0x02])
        .hdmi_vsdb(0x1000, DC_30, 68)
        .dtd_offset(4 + 2 + 4)
        .build();
    let report = EdidReport::of(&edid(&[cta]));
    assert_eq!(report.status, EdidStatus::Valid);
    assert_eq!(report.cta.svds, vec![16]);
    assert!(report.cta.hdmi_vsdb.is_none());
    assert!(
        report
            .issues
            .iter()
            .any(|issue| matches!(issue, EdidIssue::CtaDataBlockOverrun { block: 1, at: 6, length: 31 })),
        "{:?}",
        report.issues
    );
}

#[test]
fn a_known_block_too_short_to_read_is_not_read() {
    // An HDMI VSDB with four payload bytes: cea_db_is_hdmi_vsdb wants five.
    let cta = Cta::new().block(3, &[0x03, 0x0C, 0x00, 0x10]).build();
    let report = EdidReport::of(&edid(&[cta]));
    assert!(report.cta.hdmi_vsdb.is_none());
    assert!(report.issues.iter().any(|issue| matches!(issue, EdidIssue::CtaDataBlockTooShort { .. })));
    // And an HDR block with only its tag.
    let cta = Cta::new().block(7, &[0x06]).build();
    assert!(EdidReport::of(&edid(&[cta])).cta.hdr_static.is_none());
}

#[test]
fn dtd_offset_boundaries() {
    // d = 0: no detailed timings and no data blocks, and nothing wrong.
    let none = EdidReport::of(&edid(&[Cta::new().svds(&[16]).dtd_offset(0).build()]));
    assert!(none.cta.svds.is_empty());
    assert!(none.issues.is_empty(), "{:?}", none.issues);
    // d = 2: not a valid offset.
    let bad = EdidReport::of(&edid(&[Cta::new().svds(&[16]).dtd_offset(2).build()]));
    assert!(bad.cta.svds.is_empty());
    assert!(bad.issues.contains(&EdidIssue::CtaDtdOffset { block: 1, offset: 2 }));
    // d = 130: past the block.
    let past = EdidReport::of(&edid(&[Cta::new().dtd_offset(130).build()]));
    assert!(past.issues.contains(&EdidIssue::CtaDtdOffset { block: 1, offset: 130 }));
    // d = 127: the whole block is data blocks, and there is room for no
    // detailed timing.
    let full = Cta::new().svds(&[16]).dtd_offset(127).build();
    let report = EdidReport::of(&edid(&[full.clone()]));
    assert_eq!(report.cta.svds, vec![16]);
    let timings = parse_timings(&edid(&[full]));
    assert!(timings.iter().all(|timing| timing.vic == Some(16)), "{timings:?}");
}

#[test]
fn cta_revisions_below_3_have_no_data_blocks() {
    let bytes = edid(&[Cta::new().revision(2).byte3(0x30).svds(&[16]).hdmi_vsdb(0x1000, DC_30, 68).build()]);
    let report = EdidReport::of(&bytes);
    assert_eq!(report.cta.revision, Some(2));
    assert!(report.cta.hdmi_vsdb.is_none() && report.cta.svds.is_empty());
    assert!(report.cta.ycbcr444 && report.cta.ycbcr422);
    assert!(report.issues.contains(&EdidIssue::CtaRevisionWithoutDataBlocks { block: 1, revision: 2 }));
}

#[test]
fn an_unknown_extension_is_reported_and_not_read() {
    let mut display_id = vec![0u8; 128];
    display_id[0] = 0x70;
    display_id[4] = 0xFF;
    checksum(&mut display_id);
    let bytes = edid(&[display_id, Cta::new().hdmi_vsdb(0x1000, 0, 60).build()]);
    let report = EdidReport::of(&bytes);
    assert_eq!(report.status, EdidStatus::Valid);
    assert!(report.issues.contains(&EdidIssue::UnknownExtension { block: 1, tag: 0x70 }));
    assert!(report.cta.hdmi_vsdb.is_some());
}

#[test]
fn blocks_past_the_declared_count_are_not_read() {
    let mut bytes = edid(&[]);
    bytes.extend(Cta::new().hdmi_vsdb(0x1000, DC_30, 68).build());
    let report = EdidReport::of(&bytes);
    assert_eq!(report.status, EdidStatus::Valid);
    assert!(report.issues.contains(&EdidIssue::UndeclaredBlocks { count: 1 }));
    assert!(!report.cta.present);
}

#[test]
fn not_an_edid_is_said_to_be_one_thing_or_the_other() {
    assert_eq!(Edid::parse(&[]).status, EdidStatus::Absent);
    assert_eq!(Edid::parse(&[0u8; 100]).status, EdidStatus::InvalidBase);

    let mut header = edid(&[]);
    header[1] = 0x00;
    checksum(&mut header[..128]);
    let parsed = Edid::parse(&header);
    assert_eq!(parsed.status, EdidStatus::InvalidBase);
    assert!(parsed.issues.contains(&EdidIssue::BadHeader));

    let mut sum = edid(&[]);
    sum[127] ^= 0xFF;
    let parsed = Edid::parse(&sum);
    assert_eq!(parsed.status, EdidStatus::InvalidBase);
    assert!(parsed.issues.contains(&EdidIssue::BadChecksum { block: 0 }));

    for bad in [&[][..], &[0u8; 100][..], &header[..], &sum[..]] {
        assert!(parse_sink_video(bad).is_none());
        assert!(parse_timings(bad).is_empty());
        assert!(Edid::parse(bad).identity().is_none());
    }
}

// ------------------------------------------------------------ identity

#[test]
fn same_legacy_checkvalue_different_sha256() {
    let one = edid(&[Cta::new().svds(&[16, 95]).hdmi_vsdb(0x1000, 0, 60).build()]);
    // Another display: serial bytes moved by +1 and -1, so every block still
    // sums to the same checksum byte.
    let mut two = one.clone();
    two[12] = two[12].wrapping_add(1);
    two[13] = two[13].wrapping_sub(1);
    assert_eq!(Edid::parse(&one).legacy_checkvalue(), Edid::parse(&two).legacy_checkvalue());
    assert_eq!(Edid::parse(&two).status, EdidStatus::Valid);
    let (a, b) = (Edid::parse(&one).identity().unwrap(), Edid::parse(&two).identity().unwrap());
    assert_ne!(a, b);
    assert_eq!(a.0.len(), 64);
    // And the identity is of the bytes: the same bytes, the same identity.
    assert_eq!(Edid::parse(&one.clone()).identity().unwrap(), a);
}

// ------------------------------------------------------------ robustness

/// Every byte of a full-featured EDID set to a spread of values, and every
/// length it could be cut to: the parser answers each without panicking, and
/// never reads a capability out of an EDID it calls invalid.
#[test]
fn no_mutation_or_truncation_panics_or_invents_a_capability() {
    let good = edid(&[Cta::new()
        .svds(&[16, 95, 97, 129, 200])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x07)
        .hdr_static(EOTF_SDR | EOTF_PQ | EOTF_HLG, 0x01)
        .colorimetry(0xE0, 0x80)
        .y420vdb(&[97, 96])
        .y420cmdb(&[0xFF, 0xFF])
        .build()]);
    let check = |bytes: &[u8]| {
        let report = EdidReport::of(bytes);
        let sink = parse_sink_video(bytes);
        let _ = parse_timings(bytes);
        if !report.status.usable() {
            assert!(sink.is_none());
            assert!(!report.cta.present);
        }
        if report.status == EdidStatus::Truncated {
            assert!(report.sha256.is_none());
        }
    };
    for at in 0..good.len() {
        for value in [0x00, 0x01, 0x1F, 0x20, 0x7F, 0x80, 0xE0, 0xFF] {
            let mut bytes = good.clone();
            bytes[at] = value;
            check(&bytes);
            // And again with the block's checksum repaired, so the damage
            // reaches the data-block walk instead of stopping at the checksum.
            let block = at / 128 * 128;
            checksum(&mut bytes[block..block + 128]);
            check(&bytes);
        }
    }
    for length in 0..=good.len() + 130 {
        let mut bytes = good.clone();
        bytes.resize(length, 0xA5);
        check(&bytes);
    }
}

// ------------------------------------------------------------ HDR10

/// BT.2020 RGB and YCC in the colorimetry block's first byte.
const BT2020_RGB: u8 = 0x80;
const BT2020_YCC: u8 = 0x40;

fn uhd24() -> Timing {
    Timing::new(3840, 2160, 297_000, 5500, 2250, false, false)
}

#[test]
fn an_hdr10_capable_sink_takes_hdr10_where_every_condition_holds() {
    // 600 MHz, deep colour for RGB, PQ with Static Metadata Type 1, BT.2020 both ways.
    let bytes = edid(&[Cta::new()
        .svds(&[16, 93, 97])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x01)
        .hdr_static(EOTF_SDR | EOTF_PQ, 0x01)
        .colorimetry(BT2020_RGB | BT2020_YCC, 0x00)
        .build()]);
    let sink = parse_sink_video(&bytes).unwrap();
    assert!(sink.st2084 && sink.static_metadata_type1 && sink.bt2020_rgb && sink.bt2020_ycc);
    // 4K24: RGB at ten bits fits 600 MHz, and that is what HDR10 goes out in.
    assert_eq!(sink.best_hdr10(&uhd24(), &RK3588_HDMI), Some(ColorMode::new(ColorFormat::Rgb, 10)));
    // 4K60 RGB10 needs 742.5 MHz: not RGB, and the refusal says the link.
    assert_eq!(
        sink.hdr10_refusal(&uhd60(), ColorMode::new(ColorFormat::Rgb, 10), &RK3588_HDMI),
        Some(HdrRefusal::Link(Refusal::OverSink { need_khz: 742_500, max_khz: 600_000 }))
    );
    // Eight bits of PQ is not HDR10, whatever else holds.
    assert_eq!(
        sink.hdr10_refusal(&uhd24(), ColorMode::new(ColorFormat::Rgb, 8), &RK3588_HDMI),
        Some(HdrRefusal::Depth { bits: 8 })
    );
}

#[test]
fn an_sdr_only_sink_gets_no_hdr10_anywhere() {
    let bytes = edid(&[Cta::new()
        .svds(&[16, 93, 97])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x01)
        .colorimetry(BT2020_RGB | BT2020_YCC, 0x00)
        .build()]);
    let sink = parse_sink_video(&bytes).unwrap();
    assert!(!sink.st2084);
    assert!(!sink.hdr10_fits(&uhd24()));
    assert_eq!(
        sink.hdr10_refusal(&uhd24(), ColorMode::new(ColorFormat::Rgb, 10), &RK3588_HDMI),
        Some(HdrRefusal::NoPq)
    );
    // And `Auto` for an HDR film is the SDR answer.
    assert_eq!(sink.best_for(&uhd24(), true), sink.best_for(&uhd24(), false));
}

#[test]
fn pq_alone_is_not_hdr10_without_static_metadata_type1_or_bt2020() {
    let no_type1 = edid(&[Cta::new()
        .svds(&[16, 93])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x01)
        .hdr_static(EOTF_SDR | EOTF_PQ, 0x00)
        .colorimetry(BT2020_RGB | BT2020_YCC, 0x00)
        .build()]);
    let sink = parse_sink_video(&no_type1).unwrap();
    assert!(sink.st2084 && !sink.static_metadata_type1);
    assert!(!sink.hdr10_fits(&uhd24()), "PQ without SMT1: the HDR10 infoframe has nothing to carry");
    assert_eq!(
        sink.hdr10_refusal(&uhd24(), ColorMode::new(ColorFormat::Rgb, 10), &RK3588_HDMI),
        Some(HdrRefusal::NoStaticMetadataType1)
    );

    // BT.2020 declared for YCC only: HDR10 goes out as YCbCr, never RGB.
    let ycc_only = edid(&[Cta::new()
        .svds(&[16, 93])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x01)
        .hdr_static(EOTF_SDR | EOTF_PQ, 0x01)
        .colorimetry(BT2020_YCC, 0x00)
        .build()]);
    let sink = parse_sink_video(&ycc_only).unwrap();
    assert_eq!(
        sink.hdr10_refusal(&uhd24(), ColorMode::new(ColorFormat::Rgb, 10), &RK3588_HDMI),
        Some(HdrRefusal::NoBt2020 { format: ColorFormat::Rgb })
    );
    assert_eq!(sink.best_hdr10(&uhd24(), &RK3588_HDMI), Some(ColorMode::new(ColorFormat::Ycbcr444, 10)));

    // No colorimetry block at all: no HDR10.
    let none = edid(&[Cta::new()
        .svds(&[16, 93])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x01)
        .hdr_static(EOTF_SDR | EOTF_PQ, 0x01)
        .build()]);
    assert!(!parse_sink_video(&none).unwrap().hdr10_fits(&uhd24()));
}

#[test]
fn a_y420_only_high_bandwidth_mode_carries_hdr10_only_as_ten_bit_4_2_0() {
    // 4K60 is 4:2:0 only (Y420VDB), on a 600 MHz input.
    let with_deep = edid(&[Cta::new()
        .svds(&[16, 93])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x01) // DC_30bit_420
        .y420vdb(&[97])
        .hdr_static(EOTF_SDR | EOTF_PQ, 0x01)
        .colorimetry(BT2020_RGB | BT2020_YCC, 0x00)
        .build()]);
    let sink = parse_sink_video(&with_deep).unwrap();
    assert_eq!(sink.best_hdr10(&uhd60(), &RK3588_HDMI), Some(ColorMode::new(ColorFormat::Ycbcr420, 10)));
    assert_eq!(
        sink.hdr10_refusal(&uhd60(), ColorMode::new(ColorFormat::Ycbcr422, 10), &RK3588_HDMI),
        Some(HdrRefusal::Link(Refusal::Only420))
    );
    // Without DC_30bit_420 the same mode has no ten-bit cell: no HDR10.
    let without = edid(&[Cta::new()
        .svds(&[16, 93])
        .hdmi_vsdb(0x1000, DC_30 | DC_36 | DC_Y444, 68)
        .hf_vsdb(120, 0x80, 0x00)
        .y420vdb(&[97])
        .hdr_static(EOTF_SDR | EOTF_PQ, 0x01)
        .colorimetry(BT2020_RGB | BT2020_YCC, 0x00)
        .build()]);
    let sink = parse_sink_video(&without).unwrap();
    assert_eq!(sink.best_hdr10(&uhd60(), &RK3588_HDMI), None);
    assert_eq!(
        sink.hdr10_refusal(&uhd60(), ColorMode::new(ColorFormat::Ycbcr420, 10), &RK3588_HDMI),
        Some(HdrRefusal::Link(Refusal::DepthNotDeclared { bits: 10 }))
    );
}
