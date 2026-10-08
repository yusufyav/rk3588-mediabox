//! Past 600 MHz, HDMI 2.1's fixed-rate link: checked against a BenQ RD280UG
//! on the Orange Pi 5 Plus's HDMI-A-1, 2026-10-05.
//!
//! The EDID is the connector's whole one, three blocks through HF-EEODB, the
//! third a DisplayID block holding 3840x2560 at 60, 100 and 120 Hz. The modes
//! are the kernel's own list for it (`modetest -M rockchip -c`) on the patched
//! kernel, where 3840x2560 at 59.98 and 119.99 Hz trained FRL at 4 x 12 Gbps,
//! RGB 8 bit, all four lanes locked by the sink's SCDC.

use mediabox_core::{ColorFormat, ColorMode, ModeTiming, Refusal, mode_flags::*};
use mediabox_platform::output::offer;
use mediabox_platform::video::{RK3588_HDMI, SourceCaps, Timing, parse_sink_video};

const BENQ: &str = include_str!("benq-rd280ug.edid.hex");

fn edid() -> Vec<u8> {
    let hex: String = BENQ.split_whitespace().collect();
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
        .collect()
}

fn kernel(clock: u32, h: [u16; 4], v: [u16; 4], flags: u32, preferred: bool) -> Timing {
    Timing::from_mode(
        ModeTiming::new(clock, h[0], h[1], h[2], h[3], 0, v[0], v[1], v[2], v[3], 0, flags),
        preferred,
    )
}

fn p120() -> Timing {
    kernel(1_287_700, [3840, 3848, 3880, 3960], [2560, 2696, 2704, 2710], PHSYNC | NVSYNC, false)
}
fn p100() -> Timing {
    kernel(1_063_200, [3840, 3848, 3880, 3960], [2560, 2670, 2678, 2684], PHSYNC | NVSYNC, false)
}
fn p60() -> Timing {
    kernel(631_750, [3840, 3888, 3920, 4000], [2560, 2563, 2573, 2633], PHSYNC | NVSYNC, true)
}
fn p50() -> Timing {
    kernel(544_960, [3840, 3936, 4000, 4160], [2560, 2563, 2573, 2621], PHSYNC | NVSYNC, true)
}

/// The kernel's list for the BenQ, the modes this test is about.
fn kernel_modes() -> Vec<Timing> {
    vec![
        p60(),
        p50(),
        p120(),
        p100(),
        kernel(1_188_000, [3840, 4016, 4104, 4400], [2160, 2168, 2178, 2250], PHSYNC | PVSYNC, false),
        kernel(148_500, [1920, 2008, 2052, 2200], [1080, 1084, 1089, 1125], PHSYNC | PVSYNC, false),
    ]
}

const RGB8: ColorMode = ColorMode { format: ColorFormat::Rgb, bits: 8 };

#[test]
fn the_benq_declares_four_lanes_at_twelve_gigabits() {
    let sink = parse_sink_video(&edid()).expect("an EDID");
    assert_eq!(sink.max_character_rate_khz, 600_000);
    assert_eq!(sink.max_frl_gbps, 48);
}

#[test]
fn its_own_120_hz_goes_out_in_rgb_over_frl() {
    let sink = parse_sink_video(&edid()).expect("an EDID");
    // 1287.7 MHz x 24 bit x 18/16 x 1.03 is 35.8 Gbps: inside 48.
    assert_eq!(sink.refusal(&p120(), RGB8, &RK3588_HDMI), None);
    assert_eq!(sink.refusal(&p100(), RGB8, &RK3588_HDMI), None);
    assert_eq!(sink.refusal(&p60(), RGB8, &RK3588_HDMI), None);
    assert_eq!(sink.best_for_source(&p120(), false, &RK3588_HDMI), Some(RGB8));
    // Its DisplayID timings declare no 4:2:0: that stays refused.
    assert_eq!(
        sink.refusal(&p120(), ColorMode::new(ColorFormat::Ycbcr420, 8), &RK3588_HDMI),
        Some(Refusal::No420Here)
    );
}

#[test]
fn automatic_drives_it_at_120_hz() {
    let offer = offer(&kernel_modes(), &edid(), "HDMI-A-1", Some("BenQ RD280UG".into()), &RK3588_HDMI)
        .expect("an offer");
    assert_eq!(offer.auto, p120().label());
}

#[test]
fn a_source_without_frl_keeps_the_tmds_answer() {
    let tmds_only = SourceCaps { max_frl_gbps: 0, ..RK3588_HDMI };
    let sink = parse_sink_video(&edid()).expect("an EDID");
    assert!(matches!(
        sink.refusal(&p120(), RGB8, &tmds_only),
        Some(Refusal::OverSink { max_khz: 600_000, .. })
    ));
    let offer = offer(&kernel_modes(), &edid(), "HDMI-A-1", None, &tmds_only).expect("an offer");
    assert_eq!(offer.auto, p50().label());
}

#[test]
fn a_link_too_narrow_for_rgb_refuses_it() {
    // Six lanes' worth of gigabits short: 4 x 6 Gbps carries 2160p60 but not
    // 1287.7 MHz in RGB.
    let narrow = SourceCaps { max_frl_gbps: 24, ..RK3588_HDMI };
    let sink = parse_sink_video(&edid()).expect("an EDID");
    assert!(sink.refusal(&p120(), RGB8, &narrow).is_some());
    assert_eq!(sink.refusal(&p60(), RGB8, &narrow), None);
}
