//! The display screen's offer, checked against what the Orange Pi 5 Plus
//! actually saw on 2026-09-23: the kernel's own mode list for HDMI-A-2
//! (`modetest -M rockchip -c`), the EDID on that connector, and the display
//! controller's summary from debugfs, all taken from the same boot with the
//! Sony KD-65XE9005 on its 300 MHz input.
//!
//! The 600 MHz input is the same set's other EDID (tests/color_modes.rs). Its
//! kernel list was not captured, so it is checked against the same list: the
//! rules are per mode, and every mode below is one that input declares too.

use mediabox_core::{
    ColorFormat, ColorMode, DisplayGeneration, DisplayIdentity, OutputSetting, Refusal,
    ResolutionChoice,
};
use mediabox_platform::output::{Plan, Provenance, bus_colour, edid_name, offer};
use mediabox_platform::video::{RK3588_HDMI, Timing};

/// The EDID on HDMI-A-2, read from the connector's EDID property.
const PLUS_300: &str = "\
00ffffffffffff004dd903f301010101011a0103809051780a0dc9a05747982712484c2108008180a9c0714f\
b3000101010101010101023a801871382d40582c45009f295300001e011d007251d01e206e2855009f295300\
001e000000fc00534f4e5920545620202a30300a000000fd00303e0e461e000a20202020202001d302034df0\
575d5e5f621f101405130420223c3e1216030711150206012c0d7f071507503d07bc570600830f00006e030c\
004000b83c2f008001020304e200f9e305ff01e50e60616566e3060d01011d8018711c1620582c25009f2953\
00009e0000000000000000000000000000000000000000000000000000000000000000d7";

/// The Sony's other input: HF-VSDB, 600 MHz, 4:2:0 at ten bits.
const SONY_600: &str = "\
00ffffffffffff004dd903f901010101011b0103809051780a0dc9a05747982712484c2108008180a9c0714f\
b300010101010101010108e80030f2705a80b0588a009f295300001e023a801871382d40582c45009f295300\
001e000000fc00534f4e5920545620202a30300a000000fd00173e0e883c000a2020202020200171020359f0\
5b61605d5e5f621f101405130420223c3e12160307111502060165662c0d7f071507503d07bc570400830f00\
006e030c003000b83c2f00800102030467d85dc401788001e200cbe305ff01e50f03000006e3060d01011d00\
7251d01e206e2855009f295300001e00000000000000000000000000000000000000006b";

/// width, height, clock kHz, htotal, vtotal, interlaced, preferred -- in the
/// kernel's order.
const KERNEL_MODES: &[(u16, u16, u32, u16, u16, bool, bool)] = &[
    (1920, 1080, 148500, 2200, 1125, false, true),
    (4096, 2160, 594000, 4400, 2250, false, false),
    (4096, 2160, 593407, 4400, 2250, false, false),
    (4096, 2160, 594000, 5280, 2250, false, false),
    (4096, 2160, 297000, 5500, 2250, false, false),
    (4096, 2160, 296703, 5500, 2250, false, false),
    (3840, 2160, 594000, 4400, 2250, false, false),
    (3840, 2160, 593407, 4400, 2250, false, false),
    (3840, 2160, 594000, 5280, 2250, false, false),
    (3840, 2160, 297000, 4400, 2250, false, false),
    (3840, 2160, 296703, 4400, 2250, false, false),
    (3840, 2160, 297000, 5280, 2250, false, false),
    (3840, 2160, 297000, 5500, 2250, false, false),
    (3840, 2160, 296703, 5500, 2250, false, false),
    (1920, 1080, 148352, 2200, 1125, false, false),
    (1920, 1080, 74250, 2200, 1125, true, false),
    (1920, 1080, 74176, 2200, 1125, true, false),
    (1920, 1080, 148500, 2640, 1125, false, false),
    (1920, 1080, 74250, 2640, 1125, true, false),
    (1920, 1080, 74250, 2200, 1125, false, false),
    (1920, 1080, 74176, 2200, 1125, false, false),
    (1920, 1080, 74250, 2750, 1125, false, false),
    (1920, 1080, 74176, 2750, 1125, false, false),
    (1680, 1050, 119000, 1840, 1080, false, false),
    (1600, 900, 108000, 1800, 1000, false, false),
    (1280, 1024, 108000, 1688, 1066, false, false),
    (1152, 864, 108000, 1600, 900, false, false),
    (1280, 720, 74250, 1650, 750, false, false),
    (1280, 720, 74176, 1650, 750, false, false),
    (1280, 720, 74250, 1980, 750, false, false),
    (1280, 720, 74250, 3300, 750, false, false),
    (1280, 720, 74176, 3300, 750, false, false),
    (1280, 720, 59400, 3300, 750, false, false),
    (1280, 720, 59341, 3300, 750, false, false),
    (1024, 768, 65000, 1344, 806, false, false),
    (800, 600, 40000, 1056, 628, false, false),
    (720, 576, 27000, 864, 625, false, false),
    (720, 480, 27027, 858, 525, false, false),
    (720, 480, 27000, 858, 525, false, false),
    (640, 480, 25200, 800, 525, false, false),
    (640, 480, 25175, 800, 525, false, false),
];

const SUMMARY: &str = r#"Video Port0: ACTIVE
    Connector:HDMI-A-2	Encoder: TMDS-235
	bus_format[2026]: UYYVYY8_0_5X24
	overlay_mode[1] output_mode[e] SDR[0] color-encoding[BT.709] color-range[Limited]
    Display mode: 3840x2160p60
	dclk[594000 kHz] real_dclk[594000 kHz] aclk[750000 kHz] type[40] flag[5]
	H: 3840 4016 4104 4400
	V: 2160 2168 2178 2250
	Fixed H: 3840 4016 4104 4400
	Fixed V: 2160 2168 2178 2250
    Cluster0-win0: ACTIVE
	win_id: 0
	format: AR24 little-endian (0x34325241) pixel_blend_mode[0] glb_alpha[0xff]
	color: SDR[0] color-encoding[BT.601] color-range[Limited]
	rotate: xmirror: 0 ymirror: 0 rotate_90: 0 rotate_270: 0
	csc: y2r[0] r2y[1] csc mode[1]
	zpos: 11
	src: pos[0, 0] rect[3840 x 2160]
	dst: pos[0, 0] rect[3840 x 2160]
	buf[0]: addr: 0x0000000001fa4000 pitch: 15360 offset: 0
Video Port1: DISABLED
Video Port2: DISABLED
"#;

fn edid(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
        .collect()
}

fn kernel() -> Vec<Timing> {
    KERNEL_MODES
        .iter()
        .map(|&(w, h, clock, htotal, vtotal, interlaced, preferred)| {
            Timing::new(w, h, clock, htotal, vtotal, interlaced, preferred)
        })
        .collect()
}

fn offer_for(hex: &str) -> mediabox_core::OutputOffer {
    offer(&kernel(), &edid(hex), "HDMI-A-2", Some("SONY TV".into()), &RK3588_HDMI)
        .expect("an EDID")
}

/// What a plan is made for: this offer's sink at generation `seq`.
fn provenance(offer: &mediabox_core::OutputOffer, seq: u64) -> Provenance {
    Provenance {
        generation: DisplayGeneration {
            boot_id: "b0071d".into(),
            seq,
        },
        identity: DisplayIdentity {
            connector: offer.connector.clone(),
            edid_sha256: offer.edid_sha256.clone(),
        },
        transmitter: "fdea0000.hdmi".into(),
        source_profile: "rk3588-vendor61-dw-hdmi-qp@2".into(),
    }
}

fn plan(offer: &mediabox_core::OutputOffer, setting: &OutputSetting) -> Option<Plan> {
    mediabox_platform::output::plan(offer, setting, provenance(offer, 7))
}

fn key(offer: &mediabox_core::OutputOffer, label: &str) -> mediabox_core::TimingKey {
    offer.mode(label).unwrap_or_else(|| panic!("{label} listed")).timing_key
}

fn cell(offer: &mediabox_core::OutputOffer, mode: &str, format: ColorFormat, bits: u8) -> Option<Refusal> {
    offer
        .mode(mode)
        .unwrap_or_else(|| panic!("{mode} listed"))
        .cells
        .iter()
        .find(|cell| cell.mode == ColorMode::new(format, bits))
        .unwrap_or_else(|| panic!("{mode} {format:?} {bits} drawn"))
        .refused
}

#[test]
fn every_mode_the_kernel_lists_is_shown_once_under_its_size() {
    let offer = offer_for(PLUS_300);
    let labels: Vec<&str> = offer.modes().map(|mode| mode.label.as_str()).collect();
    assert_eq!(labels.len(), 41, "the kernel lists 41, none twice");
    let mut sorted = labels.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), labels.len(), "no label twice: {labels:?}");

    let sizes: Vec<(u16, u16, bool)> = offer
        .groups
        .iter()
        .map(|group| (group.width, group.height, group.computer))
        .collect();
    assert_eq!(
        sizes,
        vec![
            (4096, 2160, false),
            (3840, 2160, false),
            (1920, 1080, false),
            (1280, 720, false),
            (720, 576, false),
            (720, 480, false),
            (1680, 1050, true),
            (1600, 900, true),
            (1280, 1024, true),
            (1152, 864, true),
            (1024, 768, true),
            (800, 600, true),
            (640, 480, true),
        ]
    );
    let uhd = &offer.groups[1];
    assert_eq!(uhd.name, "4K UHD");
    let rates: Vec<&str> = uhd.modes.iter().map(|mode| mode.label.as_str()).collect();
    assert_eq!(
        rates,
        vec![
            "3840x2160p60",
            "3840x2160p59.94",
            "3840x2160p50",
            "3840x2160p30",
            "3840x2160p29.97",
            "3840x2160p25",
            "3840x2160p24",
            "3840x2160p23.976",
        ]
    );
    // Progressive first, then interlaced.
    let full_hd: Vec<&str> = offer.groups[2].modes.iter().map(|mode| mode.label.as_str()).collect();
    assert_eq!(full_hd.last(), Some(&"1920x1080i50"));
    assert!(offer.mode("1920x1080p60").unwrap().preferred);
}

#[test]
fn auto_is_the_largest_mode_in_the_panels_shape_at_the_fastest_rate_on_both_inputs() {
    assert_eq!(offer_for(PLUS_300).auto, "3840x2160p60");
    assert_eq!(offer_for(SONY_600).auto, "3840x2160p60");
}

#[test]
fn on_the_300_mhz_input_4k60_is_4_2_0_eight_bit_and_every_other_cell_says_why() {
    let offer = offer_for(PLUS_300);
    let m = "3840x2160p60";
    assert_eq!(cell(&offer, m, ColorFormat::Ycbcr420, 8), None);
    assert_eq!(cell(&offer, m, ColorFormat::Rgb, 8), Some(Refusal::Only420));
    assert_eq!(cell(&offer, m, ColorFormat::Ycbcr422, 10), Some(Refusal::Only420));
    assert_eq!(
        cell(&offer, m, ColorFormat::Ycbcr420, 10),
        Some(Refusal::DepthNotDeclared { bits: 10 })
    );
    let mode = offer.mode(m).unwrap();
    assert_eq!(mode.auto_sdr, Some(ColorMode::new(ColorFormat::Ycbcr420, 8)));
    assert_eq!(mode.auto_hdr, None, "no ten-bit cell: no HDR10 at 4K60 here");
}

#[test]
fn on_the_300_mhz_input_4k30_takes_rgb_and_carries_hdr_as_4_2_2() {
    let offer = offer_for(PLUS_300);
    let m = "3840x2160p30";
    let mode = offer.mode(m).unwrap();
    let allowed: Vec<String> = mode.allowed().map(|mode| mode.label()).collect();
    assert_eq!(allowed, vec!["RGB 8bit", "YCbCr444 8bit", "YCbCr422 10bit"]);
    assert_eq!(
        cell(&offer, m, ColorFormat::Rgb, 10),
        Some(Refusal::OverSink { need_khz: 371_250, max_khz: 300_000 })
    );
    assert_eq!(cell(&offer, m, ColorFormat::Ycbcr420, 8), Some(Refusal::No420Here));
    assert_eq!(mode.auto_sdr, Some(ColorMode::new(ColorFormat::Rgb, 8)));
    assert_eq!(mode.auto_hdr, Some(ColorMode::new(ColorFormat::Ycbcr422, 10)));
}

#[test]
fn on_the_600_mhz_input_4k60_takes_everything_that_fits_600_mhz() {
    let offer = offer_for(SONY_600);
    let m = "3840x2160p60";
    let allowed: Vec<String> = offer.mode(m).unwrap().allowed().map(|mode| mode.label()).collect();
    assert_eq!(
        allowed,
        vec!["RGB 8bit", "YCbCr444 8bit", "YCbCr422 10bit", "YCbCr420 8bit", "YCbCr420 10bit"]
    );
    assert_eq!(
        cell(&offer, m, ColorFormat::Rgb, 10),
        Some(Refusal::OverSink { need_khz: 742_500, max_khz: 600_000 })
    );
}

#[test]
fn vga_is_eight_bits_only() {
    let offer = offer_for(PLUS_300);
    assert_eq!(cell(&offer, "640x480p60", ColorFormat::Rgb, 10), Some(Refusal::EightBitOnly));
}

#[test]
fn the_display_says_its_own_name() {
    assert_eq!(edid_name(&edid(PLUS_300)).as_deref(), Some("SONY TV  *00"));
}

#[test]
fn the_display_is_known_by_the_sha256_of_its_edid_and_the_checksums_are_kept_only_to_migrate() {
    let slow = offer_for(PLUS_300);
    let fast = offer_for(SONY_600);
    assert_eq!(slow.legacy_checkvalue, "d3d7");
    assert_ne!(fast.legacy_checkvalue, "d3d7");
    assert_eq!(slow.edid_sha256.len(), 64);
    assert_ne!(slow.edid_sha256, fast.edid_sha256);
}

#[test]
fn a_choice_made_on_another_display_is_auto_here() {
    let offer = offer_for(PLUS_300);
    let mut colours = std::collections::BTreeMap::new();
    colours.insert(key(&offer, "3840x2160p30"), ColorMode::new(ColorFormat::Ycbcr422, 10));
    let made_elsewhere = OutputSetting {
        edid_sha256: "7171".repeat(16),
        resolution: key(&offer, "3840x2160p30").pipe_choice(),
        colours,
    };
    let here = made_elsewhere.for_sink(&offer.edid_sha256);
    assert_eq!(here.resolution, ResolutionChoice::Auto);
    assert!(here.colours.is_empty());
    assert_eq!(here.edid_sha256, offer.edid_sha256);
    assert_eq!(made_elsewhere.for_sink(&"7171".repeat(16)), made_elsewhere);
    // And the plan for this sink is made from that: Auto.
    assert_eq!(plan(&offer, &made_elsewhere).unwrap().kodi_screenmode, "0384002160060.00000pstd");
}

trait Choice {
    fn pipe_choice(self) -> ResolutionChoice;
}
impl Choice for mediabox_core::TimingKey {
    fn pipe_choice(self) -> ResolutionChoice {
        ResolutionChoice::Timing { key: self }
    }
}

#[test]
fn a_colour_that_cannot_be_sent_at_a_mode_is_auto_there() {
    let offer = offer_for(PLUS_300);
    let mut setting = OutputSetting { edid_sha256: offer.edid_sha256.clone(), ..Default::default() };
    setting
        .colours
        .insert(key(&offer, "3840x2160p60"), ColorMode::new(ColorFormat::Rgb, 8));
    let mode = offer.mode("3840x2160p60").unwrap();
    assert_eq!(offer.colour(&setting, mode), Some(ColorMode::new(ColorFormat::Ycbcr420, 8)));
    let thirty = offer.mode("3840x2160p30").unwrap();
    setting
        .colours
        .insert(key(&offer, "3840x2160p30"), ColorMode::new(ColorFormat::Ycbcr422, 10));
    assert_eq!(offer.colour(&setting, thirty), Some(ColorMode::new(ColorFormat::Ycbcr422, 10)));
}

#[test]
fn the_bus_formats_on_an_hdmi_link_are_colour_modes() {
    let activity = mediabox_platform::debugfs::connector_activity(SUMMARY, "HDMI-A-2").expect("HDMI-A-2");
    let bus = activity.bus_format.expect("a bus format");
    assert_eq!(bus, "UYYVYY8_0_5X24");
    assert_eq!(bus_colour(&bus), Some(ColorMode::new(ColorFormat::Ycbcr420, 8)));
    assert_eq!(bus_colour("YUYV10_1X20"), Some(ColorMode::new(ColorFormat::Ycbcr422, 10)));
    assert_eq!(bus_colour("NOT_A_FORMAT"), None);
    assert_eq!(mediabox_platform::debugfs::connector_activity(SUMMARY, "HDMI-A-1"), None);
}

#[test]
fn kodi_and_the_browser_take_the_chosen_mode() {
    let offer = offer_for(PLUS_300);
    let auto = plan(&offer, &OutputSetting::default()).unwrap();
    assert_eq!(auto.kodi_screenmode, "0384002160060.00000pstd");
    assert_eq!(auto.kodi_whitelist.len(), 8, "every 4K UHD refresh: {:?}", auto.kodi_whitelist);
    assert_eq!(auto.kodi_whitelist.last().unwrap(), "0384002160023.97600pstd");
    assert_eq!(auto.browser_mode, "3840x2160@60.000Hz");

    let fixed = OutputSetting {
        edid_sha256: offer.edid_sha256.clone(),
        resolution: key(&offer, "3840x2160p30").pipe_choice(),
        ..Default::default()
    };
    let thirty = plan(&offer, &fixed).unwrap();
    assert_eq!(thirty.kodi_screenmode, "0384002160030.00000pstd");
    assert_eq!(thirty.browser_mode, "3840x2160@30.000Hz");
}

#[test]
fn kodi_is_told_the_colour_of_every_mode_it_may_be_on() {
    let offer = offer_for(PLUS_300);
    let line = |plan: &mediabox_platform::output::Plan, key: &str| {
        plan.colours
            .iter()
            .find(|line| line.starts_with(&format!("colour {key} ")))
            .cloned()
            .unwrap_or_else(|| panic!("{key} in {:?}", plan.colours))
    };
    let auto = plan(&offer, &OutputSetting::default()).unwrap();
    // 4K60 on 300 MHz: 4:2:0 eight-bit, and no HDR10 at all.
    assert_eq!(line(&auto, "3840x2160@594000/4400x2250"), "colour 3840x2160@594000/4400x2250 ycbcr420:8 none");
    // 4K 23.976: RGB for SDR, HDR10 as 4:2:2 -- the Android box's answer.
    // Its clock is 29.97's too; the totals tell them apart.
    assert_eq!(line(&auto, "3840x2160@296703/5500x2250"), "colour 3840x2160@296703/5500x2250 rgb:8 ycbcr422:10");

    // A colour chosen for a mode is that mode's, for SDR and -- when it
    // carries ten bits -- for HDR; 23.976 beside it is untouched.
    let mut chosen = OutputSetting { edid_sha256: offer.edid_sha256.clone(), ..Default::default() };
    chosen
        .colours
        .insert(key(&offer, "3840x2160p29.97"), ColorMode::new(ColorFormat::Ycbcr422, 10));
    let with = plan(&offer, &chosen).unwrap();
    assert_eq!(line(&with, "3840x2160@296703/4400x2250"), "colour 3840x2160@296703/4400x2250 ycbcr422:10 ycbcr422:10");
    assert_eq!(line(&with, "3840x2160@296703/5500x2250"), "colour 3840x2160@296703/5500x2250 rgb:8 ycbcr422:10");
    let keys: Vec<&str> = with.colours.iter().map(|l| l.split(' ').nth(1).unwrap()).collect();
    let mut unique = keys.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), keys.len(), "every key names one mode");
    chosen
        .colours
        .insert(key(&offer, "3840x2160p29.97"), ColorMode::new(ColorFormat::Rgb, 8));
    let eight = plan(&offer, &chosen).unwrap();
    assert_eq!(line(&eight, "3840x2160@296703/4400x2250"), "colour 3840x2160@296703/4400x2250 rgb:8 ycbcr422:10");
}

#[test]
fn an_interlaced_choice_is_told_to_kodi_and_sway_as_interlaced_at_its_field_rate() {
    let offer = offer_for(PLUS_300);
    let i60 = offer.mode("1920x1080i60").expect("1080i60 listed");
    assert!(i60.interlaced);
    assert_eq!(i60.vic, Some(5));
    let fixed = OutputSetting {
        edid_sha256: offer.edid_sha256.clone(),
        resolution: i60.choice(),
        ..Default::default()
    };
    let plan = plan(&offer, &fixed).unwrap();
    // Not `0192001080030.00000pstd`: the clock over the totals is a frame
    // rate, and an interlaced mode's refresh is its field rate.
    assert_eq!(plan.kodi_screenmode, "0192001080060.00000istd");
    assert_eq!(plan.browser_mode, "1920x1080@60.000Hz");
    // The whitelist Kodi switches refresh between stays progressive.
    assert!(plan.kodi_whitelist.iter().all(|mode| mode.ends_with("pstd")));
    // And its colour line is keyed as interlaced, apart from 1080p30.
    assert!(plan.colours.iter().any(|line| line.starts_with("colour 1920x1080i@74250/2200x1125 ")));
    assert!(plan.colours.iter().any(|line| line.starts_with("colour 1920x1080@74250/2200x1125 ")));
}

#[test]
fn every_mode_offered_carries_the_key_of_the_timing_it_is() {
    let offer = offer_for(PLUS_300);
    let mut keys: Vec<String> = offer
        .modes()
        .map(|mode| {
            let key = mode.timing_key;
            assert_eq!(key.timing(), mode.timing);
            assert_eq!(key.timing().label(), mode.label);
            key.to_string()
        })
        .collect();
    let count = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), count);

    // What the web page and the television receive: the key survives the
    // wire, and the refresh of an interlaced mode is its field rate.
    let json = serde_json::to_string(&offer).unwrap();
    let back: mediabox_core::OutputOffer = serde_json::from_str(&json).unwrap();
    assert_eq!(back, offer);
    assert_eq!(back.mode("1920x1080i60").unwrap().refresh(), mediabox_core::Refresh::new(60, 1));
}

/// The captured EDIDs, read by the checked parser. What is asserted is what
/// the bytes say under CTA-861, not what the television is known to do.
#[test]
fn the_captured_edids_are_whole_and_say_what_their_blocks_say() {
    use mediabox_platform::edid::{EdidReport, EdidStatus};
    let slow = EdidReport::of(&edid(PLUS_300));
    let fast = EdidReport::of(&edid(SONY_600));
    for report in [&slow, &fast] {
        assert_eq!(report.status, EdidStatus::Valid);
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        let hdr = report.cta.hdr_static.expect("HDR static metadata block");
        // 0x0d: traditional SDR, SMPTE ST 2084, HLG; 0x01: Static Metadata Type 1.
        assert!(hdr.eotf_traditional_sdr && hdr.eotf_st2084 && hdr.eotf_hlg);
        assert!(!hdr.eotf_traditional_hdr && hdr.static_metadata_type1);
        // 0xff: every colorimetry bit, BT.2020 RGB, YCC and cYCC among them.
        let colour = report.cta.colorimetry.expect("colorimetry block");
        assert!(colour.bt2020_rgb && colour.bt2020_ycc && colour.bt2020_cycc);
        assert!(!colour.dci_p3);
    }
    // The physical address each input gave the box: 4.0.0.0 and 3.0.0.0.
    assert_eq!(slow.cta.hdmi_vsdb.unwrap().physical_address, 0x4000);
    assert_eq!(fast.cta.hdmi_vsdb.unwrap().physical_address, 0x3000);
    assert!(slow.cta.hdmi_forum.is_none());
    assert_eq!(fast.cta.hdmi_forum.unwrap().max_tmds_character_rate_khz, Some(600_000));
    // Same display, two inputs, two EDIDs: two identities. The 600 MHz one is
    // byte for byte what HDMI-A-2 on the Plus published on 2026-09-24.
    assert_eq!(
        fast.sha256.as_ref().unwrap().0,
        "1175a696da42d0dc913a90983653ceef0ba6572ba64f85c1c8826ab43345fc54"
    );
    assert_ne!(slow.sha256, fast.sha256);
    // And the offer carries it.
    assert_eq!(Some(offer_for(SONY_600).edid_sha256), fast.sha256.map(|id| id.0));
}

// ------------------------------------------------------------ the plan

#[test]
fn the_plan_carries_what_it_was_made_for_and_reads_back_whole() {
    let offer = offer_for(SONY_600);
    let made = plan(&offer, &OutputSetting::default()).unwrap();
    let text = made.render();
    for line in [
        "schema=2",
        "boot_id=b0071d",
        "generation=7",
        "connector=HDMI-A-2",
        "transmitter=fdea0000.hdmi",
        "source_profile=rk3588-vendor61-dw-hdmi-qp@2",
        "kodi_screenmode=0384002160060.00000pstd",
    ] {
        assert!(text.lines().any(|have| have == line), "{line} in\n{text}");
    }
    assert!(text.contains(&format!("edid_sha256={}\n", offer.edid_sha256)));
    assert!(text.contains(&format!("timing_key={}\n", key(&offer, "3840x2160p60"))));
    assert_eq!(Plan::parse(&text), Ok(made));
    // A plan from before provenance is not a plan.
    let old = "kodi_screenmode=0384002160060.00000pstd\nbrowser_mode=3840x2160@60.000Hz\n";
    assert!(Plan::parse(old).is_err());
    let truncated: String = text.lines().take(4).map(|line| format!("{line}\n")).collect();
    assert!(Plan::parse(&truncated).is_err());
}

#[test]
fn a_plan_is_current_only_for_its_generation_and_its_sink() {
    let sony = offer_for(SONY_600);
    let other = offer_for(PLUS_300);
    let made = plan(&sony, &OutputSetting::default()).unwrap();
    let now = |seq| DisplayGeneration { boot_id: "b0071d".into(), seq };
    let on = |offer: &mediabox_core::OutputOffer| DisplayIdentity {
        connector: offer.connector.clone(),
        edid_sha256: offer.edid_sha256.clone(),
    };
    assert_eq!(made.stale(Some(&now(7)), Some(&on(&sony))), None);
    // The generation moved: stale, whatever else holds.
    assert!(made.stale(Some(&now(8)), Some(&on(&sony))).unwrap().contains("generation"));
    // Another boot.
    let reboot = DisplayGeneration { boot_id: "other".into(), seq: 7 };
    assert!(made.stale(Some(&reboot), Some(&on(&sony))).is_some());
    // Sink A's plan on sink B, even at an equal generation number.
    assert!(made.stale(Some(&now(7)), Some(&on(&other))).is_some());
    // Nothing to check against is not current.
    assert!(made.stale(None, Some(&on(&sony))).is_some());
    assert!(made.stale(Some(&now(7)), None).is_some());
    // And a plan is never made for one sink from another's offer.
    assert!(mediabox_platform::output::plan(&other, &OutputSetting::default(), provenance(&sony, 7)).is_none());
}

#[test]
fn sway_is_given_the_mode_on_the_selected_connector_only() {
    let offer = offer_for(SONY_600);
    let made = plan(&offer, &OutputSetting::default()).unwrap();
    assert_eq!(made.sway_output(), "output HDMI-A-2 mode 3840x2160@60.000Hz\n");
    assert!(!made.sway_output().contains('*'));
}

// ------------------------------------------------------------ HDR10

#[test]
fn hdr10_on_the_sony_at_2160p23_976_is_ycbcr_422_ten_bit() {
    // The physical case this product runs: Plus, Sony, 300 MHz input.
    let offer = offer_for(PLUS_300);
    let mode = offer.mode("3840x2160p23.976").unwrap();
    assert_eq!(mode.auto_hdr, Some(ColorMode::new(ColorFormat::Ycbcr422, 10)));
    assert!(mode.carries_hdr10(ColorMode::new(ColorFormat::Ycbcr422, 10)));
    assert!(!mode.carries_hdr10(ColorMode::new(ColorFormat::Rgb, 8)), "eight bits is not HDR10");
    let link = &offer.link;
    assert!(link.st2084 && link.static_metadata_type1 && link.bt2020_rgb && link.bt2020_ycc && link.source_hdr10);
}

