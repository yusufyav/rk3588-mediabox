//! The colour mode list, checked against a television that gives two answers.
//!
//! Both EDIDs below were read off the same Sony KD-65XE9005 on 2026-09-18, from
//! its HDMI 1 and HDMI 3 inputs. The set reports those ports differently and
//! that is the whole point of the fixture: HDMI 1 has no HDMI Forum block and
//! tops out at 300 MHz, HDMI 3 has one and reaches 600 MHz. A rule that only
//! ever saw one of them would look correct and be wrong on the other.
//!
//! The expected lists are not this code's own output written down. They are
//! what the vendor Android box -- an Amlogic UGOOS on the same two ports, the
//! same afternoon -- put on screen in its own colour mode picker. If this
//! module and that box disagree, this module is wrong.

use mediabox_platform::video::{ColorFormat, ColorMode, parse_sink_video};

/// Sony KD-65XE9005, HDMI 1. No HDMI Forum VSDB; maximum TMDS clock 0x3c,
/// which is 300 MHz. Carries a 4:2:0 video data block, so 4K50/60 exist at all,
/// but declares no 4:2:0 deep colour.
const SONY_HDMI1: &str = "\
00ffffffffffff004dd903f301010101011a0103809051780a0dc9a05747982712484c2108008180a9c0714f\
    b3000101010101010101023a801871382d40582c45009f295300001e011d007251d01e206e2855009f295300\
    001e000000fc00534f4e5920545620202a30300a000000fd00303e0e461e000a20202020202001d302034df0\
    575d5e5f621f101405130420223c3e1216030711150206012c0d7f071507503d07bc570600830f00006e030c\
    001000b83c2f008001020304e200f9e305ff01e50e60616566e3060d01011d8018711c1620582c25009f2953\
    00009e000000000000000000000000000000000000000000000000000000000000000007";

/// The same set, HDMI 3. HDMI Forum VSDB present: maximum character rate 0x78,
/// which is 600 MHz, and DC_30bit_420 set -- 4:2:0 at ten bits but not twelve.
const SONY_HDMI3: &str = "\
00ffffffffffff004dd903f901010101011b0103809051780a0dc9a05747982712484c2108008180a9c0714f\
    b300010101010101010108e80030f2705a80b0588a009f295300001e023a801871382d40582c45009f295300\
    001e000000fc00534f4e5920545620202a30300a000000fd00173e0e883c000a2020202020200171020359f0\
    5b61605d5e5f621f101405130420223c3e12160307111502060165662c0d7f071507503d07bc570400830f00\
    006e030c003000b83c2f00800102030467d85dc401788001e200cbe305ff01e50f03000006e3060d01011d00\
    7251d01e206e2855009f295300001e00000000000000000000000000000000000000006b";

/// 3840x2160 at 60 Hz: 594 MHz of pixels.
const UHD60_KHZ: u32 = 594_000;
/// 3840x2160 at 24 Hz, which is what a film is: 297 MHz.
const UHD24_KHZ: u32 = 297_000;

fn edid(hex: &str) -> Vec<u8> {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
        .collect();
    assert_eq!(bytes.len(), 256, "an EDID with one extension block");
    for block in bytes.chunks_exact(128) {
        let sum: u8 = block.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        assert_eq!(sum, 0, "EDID block checksum");
    }
    bytes
}

fn labels(modes: &[ColorMode]) -> Vec<String> {
    modes.iter().map(|mode| mode.label()).collect()
}

#[test]
fn the_two_ports_of_one_television_declare_different_limits() {
    let one = parse_sink_video(&edid(SONY_HDMI1)).expect("HDMI 1 EDID");
    let three = parse_sink_video(&edid(SONY_HDMI3)).expect("HDMI 3 EDID");

    assert_eq!(one.max_character_rate_khz, 300_000);
    assert_eq!(three.max_character_rate_khz, 600_000);
    assert!(one.rate_is_declared && three.rate_is_declared);

    // Both ports of this set take HDR10 and HLG. The limit is the link, not the
    // panel -- which is exactly why the rate has to be checked separately.
    assert!(one.st2084 && one.hlg);
    assert!(three.st2084 && three.hlg);
}

#[test]
fn hdmi1_at_4k60_offers_only_what_300mhz_can_carry() {
    let sink = parse_sink_video(&edid(SONY_HDMI1)).expect("EDID");
    // The vendor box's picker on this port, at this mode, had exactly one entry.
    assert_eq!(labels(&sink.modes_for(UHD60_KHZ)), vec!["YCbCr420 8bit"]);

    // And so there is no HDR worth signalling here: eight bits would band.
    assert!(!sink.hdr10_fits(UHD60_KHZ));
    assert_eq!(
        sink.best_for(UHD60_KHZ, true),
        Some(ColorMode::new(ColorFormat::Ycbcr420, 8)),
        "a link with no room for ten bits still has to send something"
    );
}

#[test]
fn hdmi3_at_4k60_offers_exactly_what_the_vendor_box_offered() {
    let sink = parse_sink_video(&edid(SONY_HDMI3)).expect("EDID");
    assert_eq!(
        labels(&sink.modes_for(UHD60_KHZ)),
        vec![
            "RGB 8bit",
            "YCbCr444 8bit",
            "YCbCr422 12bit",
            "YCbCr420 10bit",
            "YCbCr420 8bit",
        ]
    );

    // The list is the vendor box's list. What is chosen from it is not.
    //
    // With HDR content on screen that box selects 4:2:2 12-bit, and on its own
    // hardware that is correct. This product cannot follow it there: the
    // RK3588 display driver gets HDR over a 4:2:2 link wrong, measured on this
    // same television. So the deepest non-4:2:2 mode that fits wins instead,
    // and at 4K60 on a 600 MHz port that is 4:2:0 at ten bits.
    assert!(sink.hdr10_fits(UHD60_KHZ));
    assert_eq!(
        sink.best_for(UHD60_KHZ, true),
        Some(ColorMode::new(ColorFormat::Ycbcr420, 10))
    );
}

#[test]
fn deep_colour_that_does_not_fit_is_never_offered() {
    let sink = parse_sink_video(&edid(SONY_HDMI3)).expect("EDID");
    // The sink advertises RGB and 4:4:4 at ten and twelve bits...
    assert!(sink.advertised.contains(&ColorMode::new(ColorFormat::Rgb, 10)));
    assert!(sink.advertised.contains(&ColorMode::new(ColorFormat::Ycbcr444, 12)));
    // ...and at 4K60 neither can be sent: 594 MHz of pixels is 742 MHz at ten
    // bits, above this port's 600. The vendor box leaves them out too.
    let offered = sink.modes_for(UHD60_KHZ);
    assert!(!offered.contains(&ColorMode::new(ColorFormat::Rgb, 10)));
    assert!(!offered.contains(&ColorMode::new(ColorFormat::Ycbcr444, 10)));
}

#[test]
fn a_film_gets_no_hdr_on_the_slow_port() {
    // The case the product actually has to serve, and the answer is no.
    //
    // At 4K24 a 300 MHz port has room for 4:2:2 12-bit and for nothing else
    // above eight bits -- and 4:2:2 is the link this driver renders HDR wrong
    // on. So there is no way to put HDR10 on this port at 4K that is worth
    // looking at, and the honest answer is to say so rather than to send
    // something and call it HDR.
    //
    // Measured on a Sony KD-65XE9005, HDMI 1, 2026-09-18: at 4K the link is
    // 4:2:2 and the picture is wrong; at 1080p, where RGB 10-bit fits, the
    // same film on the same cable is correct. Both colorimetry tags were
    // tried at 4K and neither helped.
    let sink = parse_sink_video(&edid(SONY_HDMI1)).expect("EDID");
    assert!(!sink.hdr10_fits(UHD24_KHZ), "4:2:2 is not an HDR path here");

    // 1080p is where this television can be given HDR, and it is RGB that
    // makes it possible.
    const HD60_KHZ: u32 = 148_500;
    assert!(sink.hdr10_fits(HD60_KHZ));
    assert_eq!(
        sink.best_for(HD60_KHZ, true),
        Some(ColorMode::new(ColorFormat::Rgb, 12))
    );

    // And the RGB 10-bit the player asks for unconditionally is still not on
    // the 4K list, which is the arithmetic that started all of this.
    assert!(
        !sink
            .modes_for(UHD24_KHZ)
            .contains(&ColorMode::new(ColorFormat::Rgb, 10))
    );
}

#[test]
fn sdr_keeps_full_chroma_rather_than_spending_the_budget_on_bits() {
    let sink = parse_sink_video(&edid(SONY_HDMI3)).expect("EDID");
    assert_eq!(
        sink.best_for(UHD60_KHZ, false),
        Some(ColorMode::new(ColorFormat::Rgb, 8)),
        "the interface is RGB and there is nothing to gain from deep colour"
    );
}

#[test]
fn a_sink_that_declares_nothing_is_given_the_floor() {
    // A base EDID with no CTA extension: valid, and advertising nothing beyond
    // the RGB 8-bit every HDMI receiver must take.
    let mut bytes = edid(SONY_HDMI1);
    bytes.truncate(128);
    bytes[126] = 0; // no extension blocks
    let sum = bytes[0..127].iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    bytes[127] = (256u16 - u16::from(sum)) as u8;

    let sink = parse_sink_video(&bytes).expect("base EDID");
    assert!(!sink.rate_is_declared);
    assert_eq!(sink.max_character_rate_khz, 165_000);
    assert_eq!(sink.advertised, vec![ColorMode::new(ColorFormat::Rgb, 8)]);
    assert!(!sink.st2084);
    // 165 MHz cannot carry 4K at all, and the answer is an empty list rather
    // than a hopeful one.
    assert!(sink.modes_for(UHD24_KHZ).is_empty());
}

#[test]
fn rubbish_is_not_an_edid() {
    assert!(parse_sink_video(&[]).is_none());
    assert!(parse_sink_video(&[0u8; 128]).is_none());
}

#[test]
fn the_timings_a_port_lists_are_the_ones_it_can_be_sent() {
    use mediabox_platform::video::parse_timings;

    let one = parse_timings(&edid(SONY_HDMI1));
    let three = parse_timings(&edid(SONY_HDMI3));

    let labels = |timings: &[mediabox_platform::video::Timing]| -> Vec<String> {
        timings.iter().map(|timing| timing.label()).collect()
    };

    // 4K exists on both ports. What differs is the rate: HDMI 1 is listed only
    // up to 30 Hz, HDMI 3 reaches 60.
    let one_labels = labels(&one);
    let three_labels = labels(&three);
    assert!(one_labels.iter().any(|label| label.starts_with("3840x2160p30")));
    assert!(
        !one_labels.iter().any(|label| label.starts_with("3840x2160p60")),
        "HDMI 1 lists no 4K60: {one_labels:?}"
    );
    assert!(three_labels.iter().any(|label| label.starts_with("3840x2160p60")));

    // And the clock carried with each is what the budget is decided on.
    let uhd30 = one
        .iter()
        .find(|timing| timing.width == 3840 && timing.refresh_mhz == 30_000)
        .expect("4K30 on HDMI 1");
    assert_eq!(uhd30.pixel_clock_khz, 297_000);
}

#[test]
fn every_listed_timing_gets_an_answer() {
    // The property that matters for a settings screen: for each mode the sink
    // lists, there is a definite list of colour modes and a definite verdict on
    // HDR. Nothing is left undecided, and 4K60 on the slow port is the one that
    // must come back without HDR.
    use mediabox_platform::video::parse_timings;

    let sink = parse_sink_video(&edid(SONY_HDMI1)).expect("EDID");
    for timing in parse_timings(&edid(SONY_HDMI1)) {
        let modes = sink.modes_for(timing.pixel_clock_khz);
        assert!(
            !modes.is_empty(),
            "{} is listed by the sink and nothing can be sent at it",
            timing.label()
        );
        if timing.width == 3840 && timing.refresh_mhz >= 50_000 {
            assert!(!sink.hdr10_fits(timing.pixel_clock_khz));
        }
    }
}
