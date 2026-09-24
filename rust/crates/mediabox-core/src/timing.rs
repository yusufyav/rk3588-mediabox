//! A display timing, as the kernel holds it, and the identity it carries.
//!
//! A mode used to be known here by its size, its refresh in millihertz and an
//! interlace bit, and that is not enough to name one. 1080i60 and 1080p30
//! share a pixel clock and both totals; two 1080p60 timings can differ only in
//! where the sync pulses sit; VIC 6 and VIC 7 are 480i with every pixel sent
//! twice, which a clock of 13.5 MHz says nothing about. Every one of those
//! differences is one the kernel keeps in `struct drm_mode_modeinfo`, and this
//! is that struct's timing, all of it, and nothing else.
//!
//! What a timing *is* here follows `drm_mode_match()` with
//! `DRM_MODE_MATCH_TIMINGS | DRM_MODE_MATCH_CLOCK | DRM_MODE_MATCH_FLAGS`
//! (drivers/gpu/drm/drm_modes.c): every horizontal and vertical number, the
//! clock, and the flags, stereo excluded. The mode's type -- `preferred`,
//! `driver`, `userdef` -- and its name are what the kernel says *about* a
//! timing, not the timing, and are not part of it.

use serde::{Deserialize, Serialize};

use crate::{ColorFormat, ColorMode};

/// `DRM_MODE_FLAG_*`, include/uapi/drm/drm_mode.h.
pub mod mode_flags {
    pub const PHSYNC: u32 = 1 << 0;
    pub const NHSYNC: u32 = 1 << 1;
    pub const PVSYNC: u32 = 1 << 2;
    pub const NVSYNC: u32 = 1 << 3;
    pub const INTERLACE: u32 = 1 << 4;
    pub const DBLSCAN: u32 = 1 << 5;
    pub const CSYNC: u32 = 1 << 6;
    pub const PCSYNC: u32 = 1 << 7;
    pub const NCSYNC: u32 = 1 << 8;
    pub const HSKEW: u32 = 1 << 9;
    pub const BCAST: u32 = 1 << 10;
    pub const PIXMUX: u32 = 1 << 11;
    pub const DBLCLK: u32 = 1 << 12;
    pub const CLKDIV2: u32 = 1 << 13;
    /// The flags that are part of a timing: bits 0-13. Above them are the
    /// stereo layout (`DRM_MODE_FLAG_3D_MASK`, bits 14-18), which this product
    /// does not drive, and the picture aspect ratio (`DRM_MODE_FLAG_PIC_AR_MASK`,
    /// bits 19-22), which the kernel reports only to a client that asked for it
    /// and which describes the picture rather than the signal.
    pub const TIMING: u32 = 0x3FFF;
}

/// One timing, field for field `struct drm_mode_modeinfo` without the name,
/// the type and the kernel's own rounded `vrefresh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModeTiming {
    pub clock_khz: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    /// Only the bits in [`mode_flags::TIMING`].
    pub flags: u32,
}

impl ModeTiming {
    /// A timing from the kernel's numbers, in the kernel's order. Flags that
    /// are not part of a timing are dropped here, once, so that two timings
    /// the kernel would call the same compare equal.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        clock_khz: u32,
        hdisplay: u16,
        hsync_start: u16,
        hsync_end: u16,
        htotal: u16,
        hskew: u16,
        vdisplay: u16,
        vsync_start: u16,
        vsync_end: u16,
        vtotal: u16,
        vscan: u16,
        flags: u32,
    ) -> Self {
        Self {
            clock_khz,
            hdisplay,
            hsync_start,
            hsync_end,
            htotal,
            hskew,
            vdisplay,
            vsync_start,
            vsync_end,
            vtotal,
            vscan,
            flags: flags & mode_flags::TIMING,
        }
    }

    pub const fn interlaced(&self) -> bool {
        self.flags & mode_flags::INTERLACE != 0
    }

    /// Every pixel sent twice (CTA-861 pixel repetition): 480i and 576i.
    pub const fn double_clocked(&self) -> bool {
        self.flags & mode_flags::DBLCLK != 0
    }

    /// The refresh, exactly: `drm_mode_vrefresh()` (drivers/gpu/drm/drm_modes.c)
    /// before its rounding to the nearest hertz. Interlace doubles it, since a
    /// field is a refresh; double scan and `vscan` divide it.
    pub fn refresh(&self) -> Refresh {
        let mut num = u64::from(self.clock_khz) * 1000;
        let mut den = u64::from(self.htotal) * u64::from(self.vtotal);
        if self.interlaced() {
            num *= 2;
        }
        if self.flags & mode_flags::DBLSCAN != 0 {
            den *= 2;
        }
        if self.vscan > 1 {
            den *= u64::from(self.vscan);
        }
        Refresh::new(num, den)
    }

    /// What names this timing and nothing else. See [`TimingKey`].
    pub fn key(&self) -> TimingKey {
        TimingKey(*self)
    }

    /// `3840x2160p59.94`, `1920x1080i60`: what a person recognises a mode by.
    /// Two timings can share one, so it is never used to find a timing.
    pub fn label(&self) -> String {
        format!(
            "{}x{}{}{}",
            self.hdisplay,
            self.vdisplay,
            if self.interlaced() { "i" } else { "p" },
            self.refresh().label()
        )
    }

    /// The TMDS character rate this timing costs in `mode`, in hertz.
    ///
    /// Ported from mainline Linux `drm_hdmi_compute_mode_clock()`
    /// (drivers/gpu/drm/display/drm_hdmi_helper.c), rule for rule:
    ///
    ///   * VIC 1 is eight bits only (CTA-861-G s5.4) -- enforced by the caller,
    ///     which knows the VIC, as a refusal rather than a zero rate;
    ///   * 4:2:2 is carried as if it were eight-bit RGB, whatever its depth,
    ///     up to twelve bits (HDMI 1.0 s6.5), and more than twelve is nothing;
    ///   * 4:2:0 runs at half the pixel clock (HDMI 2.0 s7.1);
    ///   * a double-clocked mode runs at twice it (`DRM_MODE_FLAG_DBLCLK`);
    ///   * then `DIV_ROUND_CLOSEST(clock * bpc, 8)`.
    ///
    /// The vendor 6.1 driver this product runs agrees on the one rule the old
    /// code here missed: `dw_hdmi_rockchip_select_output()` doubles the pixel
    /// clock of a DBLCLK mode before comparing it with the link. So VIC 6 at
    /// eight-bit RGB is 27 MHz on the wire, not the 13.5 MHz its clock says.
    pub fn hdmi_character_rate_hz(&self, mode: ColorMode) -> Option<u64> {
        let mut clock = u64::from(self.clock_khz) * 1000;
        let mut bpc = u64::from(mode.bits);
        if mode.format == ColorFormat::Ycbcr422 {
            if bpc > 12 {
                return None;
            }
            bpc = 8;
        }
        if mode.format == ColorFormat::Ycbcr420 {
            clock /= 2;
        }
        if self.double_clocked() {
            clock *= 2;
        }
        Some((clock * bpc + 4) / 8)
    }

    /// [`Self::hdmi_character_rate_hz`] in kHz, rounded up so that a rate is
    /// never shown as fitting a limit it exceeds by less than a kilohertz.
    pub fn hdmi_character_rate_khz(&self, mode: ColorMode) -> u32 {
        self.hdmi_character_rate_hz(mode)
            .map(|hz| hz.div_ceil(1000).min(u64::from(u32::MAX)) as u32)
            .unwrap_or(u32::MAX)
    }
}

/// A refresh rate as the fraction it is: `num / den` hertz, in lowest terms.
///
/// Never a float, so 59.94 and 60 are different numbers and 23.976 is not a
/// rounding of anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Refresh {
    pub num: u64,
    pub den: u64,
}

impl Refresh {
    /// `num / den`, reduced. A zero denominator is a timing with no totals,
    /// which has no refresh: it is kept as 0/1.
    pub fn new(num: u64, den: u64) -> Self {
        if den == 0 || num == 0 {
            return Self { num: 0, den: 1 };
        }
        let divisor = gcd(num, den);
        Self {
            num: num / divisor,
            den: den / divisor,
        }
    }

    /// Millihertz, rounded down -- the unit the persisted choice and the older
    /// interfaces carry (`refresh_mhz`). Display and compatibility only.
    pub fn millihertz(&self) -> u32 {
        (self.num * 1000 / self.den).min(u64::from(u32::MAX)) as u32
    }

    /// `drm_mode_vrefresh()`: rounded to the nearest hertz.
    pub fn hertz_rounded(&self) -> u32 {
        ((self.num + self.den / 2) / self.den).min(u64::from(u32::MAX)) as u32
    }

    /// For a caller whose own format is a decimal number of hertz (Kodi's
    /// screen mode, sway's output mode). Never compared.
    pub fn as_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// `60`, `59.94`, `23.976`: whole hertz when it is within five
    /// millihertz of one, otherwise to the millihertz with the trailing zeros
    /// off. The spelling every mode label in this product has always used.
    pub fn label(&self) -> String {
        let hz = f64::from(self.millihertz()) / 1000.0;
        if (hz - hz.round()).abs() < 0.005 {
            format!("{}", hz.round() as u32)
        } else {
            format!("{hz:.3}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string()
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A timing's identity, versioned.
///
/// ```text
/// t1:<clock kHz>:<hdisplay>,<hsync_start>,<hsync_end>,<htotal>,<hskew>:<vdisplay>,<vsync_start>,<vsync_end>,<vtotal>,<vscan>:<flags, 4 hex digits>
/// t1:594000:3840,4016,4104,4400,0:2160,2168,2178,2250,0:0005
/// ```
///
/// Why this shape: it is every field `drm_mode_match()` compares for timings,
/// clock and flags, in the kernel's own order, so two keys are equal exactly
/// when the kernel would call the modes the same timing. Decimal because a
/// person reading a log recognises 594000 and 2250; the flags in hex because
/// they are bits. The `t1` prefix is the format's version: a later format
/// gets a new prefix rather than a silently different meaning.
///
/// A label is not a key: `1920x1080p60` names a CTA timing and a DMT one with
/// different sync, and both keys say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimingKey(pub ModeTiming);

impl TimingKey {
    pub const VERSION: &'static str = "t1";

    pub fn timing(&self) -> ModeTiming {
        self.0
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        if parts.next()? != Self::VERSION {
            return None;
        }
        let clock_khz: u32 = parts.next()?.parse().ok()?;
        let numbers = |part: &str| -> Option<[u16; 5]> {
            let values: Vec<u16> = part
                .split(',')
                .map(|value| value.parse().ok())
                .collect::<Option<_>>()?;
            values.try_into().ok()
        };
        let [hd, hss, hse, ht, hsk] = numbers(parts.next()?)?;
        let [vd, vss, vse, vt, vs] = numbers(parts.next()?)?;
        let flags_text = parts.next()?;
        if flags_text.len() != 4 || parts.next().is_some() {
            return None;
        }
        let flags = u32::from_str_radix(flags_text, 16).ok()?;
        if flags & !mode_flags::TIMING != 0 {
            return None;
        }
        Some(Self(ModeTiming::new(
            clock_khz, hd, hss, hse, ht, hsk, vd, vss, vse, vt, vs, flags,
        )))
    }
}

impl std::fmt::Display for TimingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let t = &self.0;
        write!(
            f,
            "{}:{}:{},{},{},{},{}:{},{},{},{},{}:{:04x}",
            Self::VERSION,
            t.clock_khz,
            t.hdisplay,
            t.hsync_start,
            t.hsync_end,
            t.htotal,
            t.hskew,
            t.vdisplay,
            t.vsync_start,
            t.vsync_end,
            t.vtotal,
            t.vscan,
            t.flags
        )
    }
}

impl Serialize for TimingKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TimingKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| serde::de::Error::custom("not a t1 timing key"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mode_flags::*;

    // The kernel's own CTA entries (drivers/gpu/drm/drm_edid.c).
    const P1080_60: ModeTiming =
        ModeTiming::new(148_500, 1920, 2008, 2052, 2200, 0, 1080, 1084, 1089, 1125, 0, PHSYNC | PVSYNC);
    const I1080_60: ModeTiming = ModeTiming::new(
        74_250, 1920, 2008, 2052, 2200, 0, 1080, 1084, 1094, 1125, 0,
        PHSYNC | PVSYNC | INTERLACE,
    );
    const P1080_30: ModeTiming =
        ModeTiming::new(74_250, 1920, 2008, 2052, 2200, 0, 1080, 1084, 1089, 1125, 0, PHSYNC | PVSYNC);
    const VIC6: ModeTiming = ModeTiming::new(
        13_500, 720, 739, 801, 858, 0, 480, 488, 494, 525, 0,
        NHSYNC | NVSYNC | INTERLACE | DBLCLK,
    );

    fn uhd(clock: u32, htotal: u16, hss: u16, hse: u16) -> ModeTiming {
        ModeTiming::new(clock, 3840, hss, hse, htotal, 0, 2160, 2168, 2178, 2250, 0, PHSYNC | PVSYNC)
    }

    #[test]
    fn interlace_is_a_field_rate_not_half_a_frame_rate() {
        assert_eq!(I1080_60.refresh(), Refresh::new(60, 1));
        assert_eq!(I1080_60.label(), "1920x1080i60");
        assert_eq!(P1080_30.refresh(), Refresh::new(30, 1));
        assert_eq!(P1080_30.label(), "1920x1080p30");
        assert_ne!(I1080_60.key(), P1080_30.key());
    }

    #[test]
    fn fractional_rates_are_exact_fractions() {
        let p2398 = uhd(296_703, 5500, 5116, 5204);
        let p24 = uhd(297_000, 5500, 5116, 5204);
        let p2997 = uhd(296_703, 4400, 4016, 4104);
        let p30 = uhd(297_000, 4400, 4016, 4104);
        let p5994 = uhd(593_407, 4400, 4016, 4104);
        let p60 = uhd(594_000, 4400, 4016, 4104);
        assert_eq!(p24.refresh(), Refresh::new(24, 1));
        assert_eq!(p30.refresh(), Refresh::new(30, 1));
        assert_eq!(p60.refresh(), Refresh::new(60, 1));
        // 296703000 / 12375000, not 23.976 rounded from anything.
        assert_eq!(p2398.refresh(), Refresh::new(296_703_000, 12_375_000));
        assert_eq!(p2398.refresh().millihertz(), 23_976);
        assert_eq!(p2997.refresh().millihertz(), 29_970);
        assert_eq!(p5994.refresh().millihertz(), 59_940);
        for (timing, label) in [
            (p2398, "3840x2160p23.976"),
            (p24, "3840x2160p24"),
            (p2997, "3840x2160p29.97"),
            (p30, "3840x2160p30"),
            (p5994, "3840x2160p59.94"),
            (p60, "3840x2160p60"),
        ] {
            assert_eq!(timing.label(), label);
        }
        let keys: std::collections::HashSet<_> =
            [p2398, p24, p2997, p30, p5994, p60].iter().map(ModeTiming::key).collect();
        assert_eq!(keys.len(), 6);
    }

    #[test]
    fn the_same_label_can_name_two_timings_and_the_keys_differ() {
        // CTA-861 1080p60 and a reduced-blanking timing a monitor might list
        // as 1080p60 too.
        let reduced =
            ModeTiming::new(138_500, 1920, 1968, 2000, 2080, 0, 1080, 1083, 1088, 1111, 0, PHSYNC | NVSYNC);
        assert_eq!(P1080_60.label(), "1920x1080p60");
        assert_ne!(reduced.key(), P1080_60.key());
        // Same size, same clock, same totals, same rate: only the sync
        // polarity differs.
        let polarity = ModeTiming { flags: NHSYNC | NVSYNC, ..P1080_60 };
        assert_eq!(polarity.label(), P1080_60.label());
        assert_ne!(polarity.key(), P1080_60.key());
        // And only the sync position.
        let moved = ModeTiming { hsync_start: 2010, hsync_end: 2054, ..P1080_60 };
        assert_eq!(moved.label(), P1080_60.label());
        assert_ne!(moved.key(), P1080_60.key());
    }

    #[test]
    fn vic_6_costs_27_mhz_at_eight_bit_rgb() {
        let rgb8 = ColorMode::new(ColorFormat::Rgb, 8);
        assert_eq!(VIC6.hdmi_character_rate_hz(rgb8), Some(27_000_000));
        assert_eq!(VIC6.hdmi_character_rate_khz(rgb8), 27_000);
        assert_eq!(VIC6.label(), "720x480i59.94");
    }

    #[test]
    fn the_link_rate_follows_drm_hdmi_compute_mode_clock() {
        let rate = |format, bits| P1080_60.hdmi_character_rate_khz(ColorMode::new(format, bits));
        assert_eq!(rate(ColorFormat::Rgb, 8), 148_500);
        assert_eq!(rate(ColorFormat::Rgb, 10), 185_625);
        assert_eq!(rate(ColorFormat::Ycbcr422, 12), 148_500);
        assert_eq!(rate(ColorFormat::Ycbcr420, 8), 74_250);
        assert_eq!(rate(ColorFormat::Ycbcr420, 10), 92_813);
        assert_eq!(
            P1080_60.hdmi_character_rate_hz(ColorMode::new(ColorFormat::Ycbcr422, 16)),
            None
        );
    }

    #[test]
    fn a_key_round_trips_and_rejects_what_it_did_not_write() {
        let key = VIC6.key();
        let text = key.to_string();
        assert_eq!(text, "t1:13500:720,739,801,858,0:480,488,494,525,0:101a");
        assert_eq!(TimingKey::parse(&text), Some(key));
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<TimingKey>(&json).unwrap(), key);
        for bad in [
            "t2:13500:720,739,801,858,0:480,488,494,525,0:101a",
            "t1:13500:720,739,801,858:480,488,494,525,0:101a",
            "t1:13500:720,739,801,858,0:480,488,494,525,0:101a:x",
            "t1:13500:720,739,801,858,0:480,488,494,525,0:4000",
            "1920x1080p60",
        ] {
            assert_eq!(TimingKey::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn flags_that_are_not_timing_are_not_identity() {
        // Picture aspect ratio 16:9 (2 << 19) and a stereo layout (1 << 14).
        let tagged = ModeTiming::new(
            148_500, 1920, 2008, 2052, 2200, 0, 1080, 1084, 1089, 1125, 0,
            PHSYNC | PVSYNC | (2 << 19) | (1 << 14),
        );
        assert_eq!(tagged.key(), P1080_60.key());
    }
}
