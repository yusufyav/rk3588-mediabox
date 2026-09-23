//! The colour mode list, checked against a television that gives two answers.
//!
//! Both EDIDs below were read off the same Sony KD-65XE9005 on 2026-09-18, from
//! its HDMI 1 and HDMI 3 inputs. The set reports those ports differently and
//! that is the whole point of the fixture: HDMI 1 has no HDMI Forum block and
//! tops out at 300 MHz, HDMI 3 has one and reaches 600 MHz. A rule that only
//! ever saw one of them would look correct and be wrong on the other.
//!
//! The expected lists are not this code's own output written down. They are
//! what the reference Android box (Ugoos SK1, Amlogic) reported on the same
//! two inputs on 2026-09-23, read from its logcat
//! (`getCurrentSupportDeepColor current mode: ... supported color: ...`).
//! The one difference is the source's own depth: that box sends twelve bits,
//! this board's HDMI transmitter ten, so the 12-bit entries appear here at 10.

use mediabox_platform::video::{ColorFormat, ColorMode, Timing, auto_timing, parse_sink_video, parse_timings};

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

/// CTA-861 timings, as the kernel lists them.
fn uhd60() -> Timing {
    Timing::new(3840, 2160, 594_000, 4400, 2250, false, false)
}
fn uhd30() -> Timing {
    Timing::new(3840, 2160, 297_000, 4400, 2250, false, false)
}
fn uhd24() -> Timing {
    Timing::new(3840, 2160, 297_000, 5500, 2250, false, false)
}
fn hd60() -> Timing {
    Timing::new(1920, 1080, 148_500, 2200, 1125, false, false)
}

#[test]
fn timings_are_matched_to_their_cta_codes() {
    assert_eq!(uhd60().vic, Some(97));
    assert_eq!(uhd30().vic, Some(95));
    assert_eq!(uhd24().vic, Some(93));
    assert_eq!(hd60().vic, Some(16));
    // The 1000/1001 variant is the same code.
    assert_eq!(Timing::new(3840, 2160, 593_407, 4400, 2250, false, false).vic, Some(97));
    assert_eq!(uhd60().label(), "3840x2160p60");
    assert_eq!(Timing::new(3840, 2160, 593_407, 4400, 2250, false, false).label(), "3840x2160p59.94");
}

#[test]
fn the_two_inputs_declare_different_limits() {
    let one = parse_sink_video(&edid(SONY_HDMI1)).expect("HDMI 1 EDID");
    let three = parse_sink_video(&edid(SONY_HDMI3)).expect("HDMI 3 EDID");
    assert_eq!(one.max_character_rate_khz, 300_000);
    assert_eq!(three.max_character_rate_khz, 600_000);
    assert!(one.st2084 && one.hlg && three.st2084 && three.hlg);
}

#[test]
fn slow_input_at_4k60_is_what_the_android_box_offered() {
    // Android, 300 MHz input, 2160p60: "supported color: 420,8bit".
    let sink = parse_sink_video(&edid(SONY_HDMI1)).expect("EDID");
    assert_eq!(labels(&sink.modes_for(&uhd60())), vec!["YCbCr420 8bit"]);
    assert_eq!(sink.best_for(&uhd60(), false), Some(ColorMode::new(ColorFormat::Ycbcr420, 8)));
    assert!(!sink.hdr10_fits(&uhd60()));
}

#[test]
fn slow_input_at_4k30_is_what_the_android_box_offered() {
    // Android, 300 MHz input, 2160p30: "444,8bit 422,12bit rgb,8bit".
    // 4:2:0 is not offered: the sink allows it only for the modes its 4:2:0
    // blocks name, and 2160p30 is not one of them.
    let sink = parse_sink_video(&edid(SONY_HDMI1)).expect("EDID");
    assert_eq!(
        labels(&sink.modes_for(&uhd30())),
        vec!["RGB 8bit", "YCbCr444 8bit", "YCbCr422 10bit"]
    );
}

#[test]
fn fast_input_at_4k60_is_what_the_android_box_offered() {
    // Android, 600 MHz input, 2160p60: "420,10bit 420,8bit 444,8bit 422,12bit rgb,8bit".
    let sink = parse_sink_video(&edid(SONY_HDMI3)).expect("EDID");
    assert_eq!(
        labels(&sink.modes_for(&uhd60())),
        vec!["RGB 8bit", "YCbCr444 8bit", "YCbCr422 10bit", "YCbCr420 10bit", "YCbCr420 8bit"]
    );
    assert_eq!(sink.best_for(&uhd60(), false), Some(ColorMode::new(ColorFormat::Rgb, 8)));
}

#[test]
fn fast_input_at_4k30_is_what_the_android_box_offered() {
    // Android, 600 MHz input, 2160p30:
    // "444,12bit 444,10bit 444,8bit 422,12bit rgb,12bit rgb,10bit rgb,8bit".
    let sink = parse_sink_video(&edid(SONY_HDMI3)).expect("EDID");
    assert_eq!(
        labels(&sink.modes_for(&uhd30())),
        vec!["RGB 10bit", "RGB 8bit", "YCbCr444 10bit", "YCbCr444 8bit", "YCbCr422 10bit"]
    );
}

#[test]
fn hdr_at_4k24_on_the_slow_input_is_4_2_2_as_on_the_android_box() {
    // Android, 300 MHz input, an HDR10 film: 2160p24, 422,12bit, HDR.
    let sink = parse_sink_video(&edid(SONY_HDMI1)).expect("EDID");
    assert!(sink.hdr10_fits(&uhd24()));
    assert_eq!(sink.best_for(&uhd24(), true), Some(ColorMode::new(ColorFormat::Ycbcr422, 10)));
    // Ten-bit RGB is 371 MHz and does not fit 300.
    assert!(!sink.modes_for(&uhd24()).contains(&ColorMode::new(ColorFormat::Rgb, 10)));
}

#[test]
fn hdr_at_4k24_on_the_fast_input_is_rgb_ten_bit() {
    let sink = parse_sink_video(&edid(SONY_HDMI3)).expect("EDID");
    assert_eq!(sink.best_for(&uhd24(), true), Some(ColorMode::new(ColorFormat::Rgb, 10)));
}

#[test]
fn auto_is_4k60_on_both_inputs_as_on_the_android_box() {
    for hex in [SONY_HDMI1, SONY_HDMI3] {
        let bytes = edid(hex);
        let sink = parse_sink_video(&bytes).expect("EDID");
        let chosen = auto_timing(&parse_timings(&bytes), Some(&sink)).expect("a mode");
        assert_eq!((chosen.width, chosen.height, chosen.refresh_mhz), (3840, 2160, 60_000));
    }
}
