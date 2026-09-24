//! Timings as the kernel holds them: the CTA codes they are, the refresh they
//! run at, and what they cost on the wire. Every expected value below is the
//! kernel's own -- `drm_match_cea_mode`, `drm_mode_vrefresh`,
//! `drm_hdmi_compute_mode_clock` -- worked by hand from the numbers in
//! drivers/gpu/drm/drm_edid.c.

use mediabox_core::{ColorFormat, ColorMode, ModeTiming, mode_flags::*};
use mediabox_platform::video::{RK3588_HDMI, SinkVideo, Timing, parse_timings};

/// A kernel mode, as `drm_mode_modeinfo` hands it over.
fn kernel(
    clock: u32,
    h: [u16; 4],
    v: [u16; 4],
    flags: u32,
) -> Timing {
    Timing::from_mode(
        ModeTiming::new(clock, h[0], h[1], h[2], h[3], 0, v[0], v[1], v[2], v[3], 0, flags),
        false,
    )
}

fn uhd(clock: u32, htotal: u16) -> Timing {
    let (hss, hse) = match htotal {
        4400 => (4016, 4104),
        5280 => (4896, 4984),
        _ => (5116, 5204),
    };
    kernel(clock, [3840, hss, hse, htotal], [2160, 2168, 2178, 2250], PHSYNC | PVSYNC)
}

/// A sink that takes everything this board can send, so that the only limit
/// in play is the one a test is about.
fn wide_open(max_khz: u32) -> SinkVideo {
    SinkVideo {
        max_character_rate_khz: max_khz,
        rate_is_declared: true,
        advertised: Vec::new(),
        st2084: true,
        hlg: false,
        is_hdmi: true,
        ycbcr444: true,
        ycbcr422: true,
        rgb_deep: vec![10, 12],
        ycbcr444_deep: vec![10, 12],
        ycbcr420_deep: vec![10, 12],
        y420_only: Vec::new(),
        y420_also: Vec::new(),
    }
}

#[test]
fn interlaced_1080_at_60_is_vic_5_and_sixty_fields_a_second() {
    let i60 = kernel(74_250, [1920, 2008, 2052, 2200], [1080, 1084, 1094, 1125], PHSYNC | PVSYNC | INTERLACE);
    assert_eq!(i60.vic, Some(5));
    assert!(i60.interlaced);
    assert_eq!(i60.refresh_mhz, 60_000);
    assert_eq!(i60.label(), "1920x1080i60");

    let p30 = kernel(74_250, [1920, 2008, 2052, 2200], [1080, 1084, 1089, 1125], PHSYNC | PVSYNC);
    assert_eq!(p30.vic, Some(34));
    assert_eq!(p30.refresh_mhz, 30_000);
    assert_eq!(p30.label(), "1920x1080p30");

    // Same clock, same totals: only the timing tells them apart.
    assert_eq!((i60.pixel_clock_khz, i60.htotal, i60.vtotal), (p30.pixel_clock_khz, p30.htotal, p30.vtotal));
    assert_ne!(i60.key(), p30.key());
}

#[test]
fn the_4k_rates_and_their_1000_1001_variants_are_their_own_codes_and_keys() {
    let cases = [
        (uhd(296_703, 5500), 93, 23_976, "3840x2160p23.976"),
        (uhd(297_000, 5500), 93, 24_000, "3840x2160p24"),
        (uhd(296_703, 4400), 95, 29_970, "3840x2160p29.97"),
        (uhd(297_000, 4400), 95, 30_000, "3840x2160p30"),
        (uhd(593_407, 4400), 97, 59_940, "3840x2160p59.94"),
        (uhd(594_000, 4400), 97, 60_000, "3840x2160p60"),
    ];
    for (timing, vic, mhz, label) in cases {
        assert_eq!(timing.vic, Some(vic), "{label}");
        assert_eq!(timing.refresh_mhz, mhz, "{label}");
        assert_eq!(timing.label(), label);
    }
    let mut keys: Vec<String> = cases.iter().map(|case| case.0.key().to_string()).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), cases.len());
}

#[test]
fn a_cta_timing_is_matched_on_its_sync_and_flags_as_the_kernel_matches_it() {
    let cta = kernel(148_500, [1920, 2008, 2052, 2200], [1080, 1084, 1089, 1125], PHSYNC | PVSYNC);
    assert_eq!(cta.vic, Some(16));
    // The same totals and clock with the sync the other way up is not VIC 16
    // to drm_match_cea_mode, and so not here either.
    let inverted = kernel(148_500, [1920, 2008, 2052, 2200], [1080, 1084, 1089, 1125], NHSYNC | NVSYNC);
    assert_eq!(inverted.vic, None);
    assert_eq!(inverted.label(), cta.label());
    assert_ne!(inverted.key(), cta.key());
}

#[test]
fn pixel_repetition_is_part_of_480i_and_576i_and_doubles_the_link_rate() {
    let rgb8 = ColorMode::new(ColorFormat::Rgb, 8);
    for (vic, lines) in [(6u8, 480u16), (7, 480), (21, 576), (22, 576)] {
        let timings = parse_timings(&cta_only_edid(&[vic]));
        let timing = timings
            .iter()
            .find(|timing| timing.vic == Some(vic))
            .unwrap_or_else(|| panic!("VIC {vic} in {timings:?}"));
        assert_eq!(timing.pixel_clock_khz, 13_500, "VIC {vic}");
        assert!(timing.mode.double_clocked(), "VIC {vic} is DBLCLK");
        assert!(timing.interlaced && timing.height == lines);
        assert_eq!(timing.character_rate_hz(rgb8), Some(27_000_000), "VIC {vic}");
        let cells = wide_open(340_000).cells_for(timing, &RK3588_HDMI);
        let cell = cells.iter().find(|cell| cell.mode == rgb8).unwrap();
        assert_eq!(cell.rate_khz, 27_000, "VIC {vic}: 27 MHz, not 13.5");
    }
}

#[test]
fn the_link_rate_that_refuses_a_480i_mode_is_the_doubled_one() {
    // A sink that declares 25 MHz: 13.5 MHz would fit, 27 does not.
    let timings = parse_timings(&cta_only_edid(&[6]));
    let vic6 = timings.iter().find(|timing| timing.vic == Some(6)).unwrap();
    let refusal = wide_open(25_000).refusal(vic6, ColorMode::new(ColorFormat::Rgb, 8), &RK3588_HDMI);
    assert!(
        matches!(refusal, Some(mediabox_core::Refusal::OverSink { need_khz: 27_000, max_khz: 25_000 })),
        "{refusal:?}"
    );
}

#[test]
fn a_timing_known_only_by_its_totals_takes_the_cta_timing_it_is() {
    let legacy = Timing::new(720, 480, 13_500, 858, 525, true, false);
    assert_eq!(legacy.vic, Some(6));
    assert!(legacy.mode.double_clocked());
    let i60 = Timing::new(1920, 1080, 74_250, 2200, 1125, true, false);
    assert_eq!(i60.label(), "1920x1080i60");
    assert_eq!(i60.vic, Some(5));
}

#[test]
fn an_interlaced_detailed_timing_is_doubled_into_a_frame() {
    // CTA-861 1080i60 as an 18-byte descriptor: one field of 540 lines,
    // 22.5 lines of blanking written as 22, interlace and +h +v sync.
    let mut edid = cta_only_edid(&[]);
    let dtd: [u8; 18] = [
        0x01, 0x1D, 0x80, 0x18, 0x71, 0x1C, 0x16, 0x20, 0x58, 0x2C, 0x25, 0x00, 0x9F, 0x29,
        0x53, 0x00, 0x00, 0x9E,
    ];
    edid[54..72].copy_from_slice(&dtd);
    fix_checksum(&mut edid[..128]);
    let timings = parse_timings(&edid);
    let i60 = timings
        .iter()
        .find(|timing| timing.interlaced)
        .unwrap_or_else(|| panic!("an interlaced timing in {timings:?}"));
    assert_eq!((i60.width, i60.height, i60.vtotal), (1920, 1080, 1125));
    assert_eq!(i60.vic, Some(5));
    assert_eq!(i60.label(), "1920x1080i60");
}

/// A base block and one CTA-861 extension whose video data block lists
/// `vics`, checksums right.
fn cta_only_edid(vics: &[u8]) -> Vec<u8> {
    let mut edid = vec![0u8; 256];
    edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    edid[18] = 1;
    edid[19] = 3;
    edid[126] = 1;
    let cta = &mut edid[128..];
    cta[0] = 0x02;
    cta[1] = 0x03;
    let mut at = 4;
    // HDMI VSDB, so the sink is HDMI.
    cta[at..at + 6].copy_from_slice(&[0x65, 0x03, 0x0C, 0x00, 0x10, 0x00]);
    at += 6;
    if !vics.is_empty() {
        cta[at] = 0x40 | vics.len() as u8;
        cta[at + 1..at + 1 + vics.len()].copy_from_slice(vics);
        at += 1 + vics.len();
    }
    cta[2] = at as u8;
    fix_checksum(&mut edid[..128]);
    fix_checksum(&mut edid[128..]);
    edid
}

fn fix_checksum(block: &mut [u8]) {
    let sum = block[..127].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    block[127] = 0u8.wrapping_sub(sum);
}
