//! What a television can be sent at a given mode, by the rules of HDMI.
//!
//! Nothing here is this product's own opinion. Every rule is the one the Linux
//! kernel implements, named where it lives, with the specification section the
//! kernel itself cites:
//!
//!   * the rate a mode costs on the wire     `drm_hdmi_compute_mode_clock`
//!     (drivers/gpu/drm/display/drm_hdmi_helper.c; HDMI 1.0 s6.5, HDMI 2.0 s7.1)
//!   * whether the sink takes a format/depth `sink_supports_format_bpc`
//!     (drivers/gpu/drm/display/drm_hdmi_state_helper.c; CTA-861-F s5.4,
//!     HDMI 1.3 s6.5)
//!   * whether the rate fits                 `hdmi_clock_valid` (same file)
//!   * what `Auto` picks                      `hdmi_compute_config` (same file)
//!   * what the EDID declares                 `drm_parse_hdmi_deep_color_info`,
//!     `drm_parse_ycbcr420_deep_color_info`, `drm_parse_hdmi_forum_scds`,
//!     `parse_cta_y420vdb`, `parse_cta_y420cmdb` (drivers/gpu/drm/drm_edid.c)
//!
//! The source's own limits are the ones the running vendor HDMI driver
//! exposes on this board, read off the connector: `color_format` offers RGB,
//! YCbCr 4:4:4, 4:2:2 and 4:2:0; `color_depth` offers 24 and 30 bit, so ten
//! bits per component at most; and its `mode_valid` stops TMDS at 600 MHz.
//!
//! Checked against the reference Android box on both inputs of the same Sony,
//! whose lists are in `tests/color_modes.rs`.

pub use mediabox_core::{ColorFormat, ColorMode, ColourCell, ModeTiming, Refusal, TimingKey};
use mediabox_core::mode_flags;
use serde::{Deserialize, Serialize};

use crate::cta_vics::{CTA_VICS, cta_mode, match_cea_mode};
use crate::edid::{CtaCapabilities, Edid, cta_dtds, data_blocks, svd_to_vic};

/// What a source's HDMI transmitter can send: the link limits of a
/// [`crate::source::SourceProfile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceCaps {
    pub max_tmds_khz: u32,
    pub max_bpc: u8,
    pub formats: &'static [ColorFormat],
}

/// RK3588 HDMI TX under the vendor kernel this product runs: the link limits
/// of [`crate::source::RK3588_VENDOR_61`]. Code that has a resolved profile
/// passes its `caps`; this name is that profile's, for the callers that
/// assume the shipping stack.
pub const RK3588_HDMI: SourceCaps = crate::source::RK3588_VENDOR_61.caps;

/// What one sink declared, on the port it is plugged into. Parsed the way
/// `drm_edid.c` parses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkVideo {
    /// `display_info.max_tmds_clock`: the HDMI VSDB's Max_TMDS_Clock, replaced
    /// by the HF-VSDB's Max_TMDS_Character_Rate when that is above 340 MHz.
    /// Zero when the sink declared neither.
    pub max_character_rate_khz: u32,
    pub rate_is_declared: bool,
    /// Everything declared, before any mode or link budget is applied -- what
    /// the Amlogic stack calls `dc_cap`.
    pub advertised: Vec<ColorMode>,
    pub st2084: bool,
    pub hlg: bool,
    /// An HDMI VSDB is present (`display_info.is_hdmi`). A DVI sink takes RGB
    /// at eight bits and nothing else.
    pub is_hdmi: bool,
    pub ycbcr444: bool,
    pub ycbcr422: bool,
    /// Deep colour for RGB (`edid_hdmi_rgb444_dc_modes`), for 4:4:4
    /// (`edid_hdmi_ycbcr444_dc_modes`, only with DC_Y444) and for 4:2:0
    /// (`hdmi.y420_dc_modes`), as bit depths.
    pub rgb_deep: Vec<u8>,
    pub ycbcr444_deep: Vec<u8>,
    pub ycbcr420_deep: Vec<u8>,
    /// VICs the sink takes only as 4:2:0 (Y420VDB) and those it takes as
    /// 4:2:0 as well (Y420CMDB over the video data block).
    pub y420_only: Vec<u8>,
    pub y420_also: Vec<u8>,
}

/// One timing, as a mode the kernel lists or the EDID declares.
///
/// `mode` is the timing itself, whole ([`ModeTiming`]), and what names it is
/// its [`TimingKey`]. The fields beside it are derived from it once, here, for
/// the code that reads a size or a rate: none of them identifies a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timing {
    pub mode: ModeTiming,
    pub width: u16,
    pub height: u16,
    /// Millihertz, rounded down from the exact refresh: 23.976 is a number
    /// rather than a rounding, and an interlaced mode's is its field rate.
    pub refresh_mhz: u32,
    pub pixel_clock_khz: u32,
    pub htotal: u16,
    pub vtotal: u16,
    pub interlaced: bool,
    /// The sink's preferred timing. What the kernel says about the timing,
    /// not part of it.
    pub preferred: bool,
    /// The CTA-861 code, when it is one (`drm_match_cea_mode`).
    pub vic: Option<u8>,
}

impl Timing {
    /// A timing the kernel listed, or the EDID declared, with its CTA code
    /// worked out the way the kernel does it.
    pub fn from_mode(mode: ModeTiming, preferred: bool) -> Self {
        Self::with_vic(mode, preferred, match_cea_mode(&mode))
    }

    fn with_vic(mode: ModeTiming, preferred: bool, vic: Option<u8>) -> Self {
        Self {
            mode,
            width: mode.hdisplay,
            height: mode.vdisplay,
            refresh_mhz: mode.refresh().millihertz(),
            pixel_clock_khz: mode.clock_khz,
            htotal: mode.htotal,
            vtotal: mode.vtotal,
            interlaced: mode.interlaced(),
            preferred,
            vic,
        }
    }

    /// A timing known only by its size, clock and totals -- a test, or a
    /// capture from before timings were kept whole.
    ///
    /// When those are a CTA-861 code's, at its clock or its 1000/1001 variant,
    /// the code's own timing is used (its sync, its flags, pixel repetition
    /// included); otherwise the sync positions are unknown and left at the
    /// active edge, and the key says so by differing from any mode the kernel
    /// would list. Production code builds a timing from the kernel's mode with
    /// [`Timing::from_mode`].
    pub fn new(
        width: u16,
        height: u16,
        pixel_clock_khz: u32,
        htotal: u16,
        vtotal: u16,
        interlaced: bool,
        preferred: bool,
    ) -> Self {
        let cta = CTA_VICS
            .iter()
            .filter(|entry| {
                let (_, _, hd, _, _, ht, _, vd, _, _, vt, _, flags) = **entry;
                hd == width
                    && vd == height
                    && ht == htotal
                    && vt == vtotal
                    && (flags & mode_flags::INTERLACE != 0) == interlaced
            })
            .filter_map(|entry| cta_mode(entry.0).map(|mode| (entry.0, mode)))
            .find(|(_, mode)| {
                mode.clock_khz.abs_diff(pixel_clock_khz) <= 1
                    || cea_alternate_clock(mode).abs_diff(pixel_clock_khz) <= 1
            });
        match cta {
            Some((vic, mode)) => Self::with_vic(
                ModeTiming {
                    clock_khz: pixel_clock_khz,
                    ..mode
                },
                preferred,
                Some(vic),
            ),
            None => Self::with_vic(
                ModeTiming::new(
                    pixel_clock_khz,
                    width,
                    width,
                    width,
                    htotal,
                    0,
                    height,
                    height,
                    height,
                    vtotal,
                    0,
                    if interlaced { mode_flags::INTERLACE } else { 0 },
                ),
                preferred,
                None,
            ),
        }
    }

    pub fn key(&self) -> TimingKey {
        self.mode.key()
    }

    /// `3840x2160p59.94` / `1920x1080i60` — what a person recognises a mode by.
    /// Never what a mode is looked up by.
    pub fn label(self) -> String {
        self.mode.label()
    }

    /// The TMDS character rate `colour` needs at this timing, in hertz
    /// ([`ModeTiming::hdmi_character_rate_hz`]).
    pub fn character_rate_hz(&self, colour: ColorMode) -> Option<u64> {
        self.mode.hdmi_character_rate_hz(colour)
    }
}

/// The 1000/1001 variant's clock, for [`Timing::new`]'s lookup by totals
/// (`cea_mode_alternate_clock`).
fn cea_alternate_clock(mode: &ModeTiming) -> u32 {
    if mode.refresh().hertz_rounded() % 6 != 0 {
        return mode.clock_khz;
    }
    let clock = u64::from(mode.clock_khz);
    let alternate = if mode.vdisplay == 240 || mode.vdisplay == 480 {
        (clock * 1001 + 500) / 1000
    } else {
        (clock * 1000 + 500) / 1001
    };
    alternate as u32
}

impl SinkVideo {
    /// `drm_mode_is_420_only` / `drm_mode_is_420`.
    fn is_420_only(&self, timing: &Timing) -> bool {
        timing.vic.is_some_and(|vic| self.y420_only.contains(&vic))
    }
    fn is_420(&self, timing: &Timing) -> bool {
        timing
            .vic
            .is_some_and(|vic| self.y420_only.contains(&vic) || self.y420_also.contains(&vic))
    }

    /// `sink_supports_format_bpc`, then `hdmi_clock_valid`, in the kernel's
    /// order, answering with the first rule that refuses -- or `None` when the
    /// mode may be sent.
    pub fn refusal(
        &self,
        timing: &Timing,
        mode: ColorMode,
        source: &SourceCaps,
    ) -> Option<Refusal> {
        let ColorMode { format, bits } = mode;
        if bits > source.max_bpc && format != ColorFormat::Ycbcr422 {
            return Some(Refusal::SourceDepth {
                bits,
                max: source.max_bpc,
            });
        }
        // CTA-861-F s5.4: VIC 1 is eight bits only.
        if timing.vic == Some(1) && bits != 8 {
            return Some(Refusal::EightBitOnly);
        }
        if !self.is_hdmi && (format != ColorFormat::Rgb || bits != 8) {
            return Some(Refusal::NotHdmi);
        }
        if self.is_420_only(timing) && format != ColorFormat::Ycbcr420 {
            return Some(Refusal::Only420);
        }
        let declared = match format {
            ColorFormat::Rgb => {
                if bits == 8 || self.rgb_deep.contains(&bits) {
                    None
                } else {
                    Some(Refusal::DepthNotDeclared { bits })
                }
            }
            ColorFormat::Ycbcr420 => {
                if !self.is_420(timing) {
                    Some(Refusal::No420Here)
                } else if bits == 8 || self.ycbcr420_deep.contains(&bits) {
                    None
                } else {
                    Some(Refusal::DepthNotDeclared { bits })
                }
            }
            // HDMI 1.3 s6.5: deep colour is not relevant to 4:2:2; up to 12.
            ColorFormat::Ycbcr422 => (!self.ycbcr422).then_some(Refusal::FormatNotDeclared),
            ColorFormat::Ycbcr444 => {
                if !self.ycbcr444 {
                    Some(Refusal::FormatNotDeclared)
                } else if bits == 8 || self.ycbcr444_deep.contains(&bits) {
                    None
                } else {
                    Some(Refusal::DepthNotDeclared { bits })
                }
            }
        };
        if declared.is_some() {
            return declared;
        }
        // `hdmi_clock_valid`, with this board's transmitter as the driver's
        // own `tmds_char_rate_valid`: compared in hertz, as the kernel does,
        // on the rate `drm_hdmi_compute_mode_clock` gives -- which is where
        // pixel repetition doubles a 480i clock.
        let Some(need_hz) = timing.character_rate_hz(mode) else {
            return Some(Refusal::SourceDepth {
                bits,
                max: source.max_bpc,
            });
        };
        let need_khz = timing.mode.hdmi_character_rate_khz(mode);
        if self.max_character_rate_khz != 0
            && need_hz > u64::from(self.max_character_rate_khz) * 1000
        {
            return Some(Refusal::OverSink {
                need_khz,
                max_khz: self.max_character_rate_khz,
            });
        }
        if need_hz > u64::from(source.max_tmds_khz) * 1000 {
            return Some(Refusal::OverSource {
                need_khz,
                max_khz: source.max_tmds_khz,
            });
        }
        None
    }

    /// Every cell the display screen draws at this timing: each format at
    /// each depth the source has, 4:2:2 once. Refused ones say why.
    pub fn cells_for(&self, timing: &Timing, source: &SourceCaps) -> Vec<ColourCell> {
        let mut cells = Vec::new();
        for &format in source.formats {
            for bits in depths(format, source) {
                let mode = ColorMode::new(format, bits);
                cells.push(ColourCell {
                    mode,
                    rate_khz: timing.mode.hdmi_character_rate_khz(mode),
                    refused: self.refusal(timing, mode, source),
                });
            }
        }
        cells
    }

    /// Every format and depth that may be sent at this timing.
    ///
    /// 4:2:2 is listed once, at the source's depth: HDMI carries it in a
    /// twelve-bit container whatever the depth, so a shallower 4:2:2 is the
    /// same signal with the low bits zero (HDMI 1.0 s6.5).
    pub fn modes_for(&self, timing: &Timing) -> Vec<ColorMode> {
        self.modes_for_source(timing, &RK3588_HDMI)
    }

    pub fn modes_for_source(&self, timing: &Timing, source: &SourceCaps) -> Vec<ColorMode> {
        let mut modes = Vec::new();
        for &format in source.formats {
            for bits in depths(format, source).into_iter().rev() {
                let mode = ColorMode::new(format, bits);
                if self.refusal(timing, mode, source).is_none() {
                    modes.push(mode);
                }
            }
        }
        modes
    }

    /// What `Auto` sends: `hdmi_compute_config` -- RGB at the deepest depth
    /// asked for down to eight, then 4:2:0 the same way.
    ///
    /// For HDR the depth asked for is ten, because HDR10 is a ten-bit format;
    /// where RGB cannot carry ten bits the other formats that can are tried
    /// in the order of what they give up least -- 4:4:4, then 4:2:2, then
    /// 4:2:0 -- which is where the reference Android box lands on a 300 MHz
    /// input (2160p24, YCbCr 4:2:2 12-bit). If nothing carries ten bits there
    /// is no HDR at this timing and the SDR answer is returned.
    pub fn best_for(&self, timing: &Timing, hdr: bool) -> Option<ColorMode> {
        self.best_for_source(timing, hdr, &RK3588_HDMI)
    }

    /// [`Self::best_for`] for a source other than the shipping one.
    pub fn best_for_source(
        &self,
        timing: &Timing,
        hdr: bool,
        source: &SourceCaps,
    ) -> Option<ColorMode> {
        let modes = self.modes_for_source(timing, source);
        let has = |format: ColorFormat, bits: u8| {
            modes
                .iter()
                .copied()
                .find(|mode| mode.format == format && mode.bits == bits)
        };
        if hdr && self.st2084 {
            for format in [
                ColorFormat::Rgb,
                ColorFormat::Ycbcr444,
                ColorFormat::Ycbcr422,
                ColorFormat::Ycbcr420,
            ] {
                if let Some(mode) = modes
                    .iter()
                    .copied()
                    .filter(|mode| mode.format == format && mode.carries_hdr())
                    .max_by_key(|mode| mode.bits)
                {
                    return Some(mode);
                }
            }
        }
        has(ColorFormat::Rgb, 8).or_else(|| has(ColorFormat::Ycbcr420, 8))
    }

    /// Whether HDR10 can be both signalled and carried at this timing.
    pub fn hdr10_fits(&self, timing: &Timing) -> bool {
        self.hdr10_fits_source(timing, &RK3588_HDMI)
    }

    /// [`Self::hdr10_fits`] for a source other than the shipping one.
    pub fn hdr10_fits_source(&self, timing: &Timing, source: &SourceCaps) -> bool {
        self.st2084
            && self
                .modes_for_source(timing, source)
                .iter()
                .any(|mode| mode.carries_hdr())
    }
}

/// The mode `Auto` drives the television at.
///
/// HDMI names no such rule -- a source may use any mode the sink lists -- so
/// this is the reference Android box's, measured on both inputs of the Sony:
/// the largest mode in the shape of the sink's preferred one, at the fastest
/// refresh the link carries in any format, progressive. On the 300 MHz input
/// that is 2160p60 in 4:2:0 even though the preferred mode is 1080p.
pub fn auto_timing(timings: &[Timing], sink: Option<&SinkVideo>) -> Option<Timing> {
    auto_timing_source(timings, sink, &RK3588_HDMI)
}

/// [`auto_timing`] for a source other than the shipping one.
pub fn auto_timing_source(
    timings: &[Timing],
    sink: Option<&SinkVideo>,
    source: &SourceCaps,
) -> Option<Timing> {
    let shape = timings
        .iter()
        .find(|timing| timing.preferred)
        .map(|timing| u32::from(timing.width) * 1000 / u32::from(timing.height).max(1));
    let carried = |timing: &&Timing| {
        !timing.interlaced
            && sink.is_none_or(|sink| !sink.modes_for_source(timing, source).is_empty())
    };
    let key = |timing: &&Timing| {
        let ratio = u32::from(timing.width) * 1000 / u32::from(timing.height).max(1);
        (
            shape.is_none_or(|want| ratio == want),
            u32::from(timing.width) * u32::from(timing.height),
            timing.refresh_mhz,
            timing.preferred,
        )
    };
    timings
        .iter()
        .filter(carried)
        .max_by_key(key)
        .or_else(|| timings.iter().max_by_key(key))
        .copied()
}

/// The mode a resolution choice resolves to on the sink plugged in now: the
/// mode chosen when this sink lists it and the link carries it, `Auto`
/// otherwise. The stored choice is matched as it is stored -- see
/// `OutputModeOffer::is` for why that is not a mode's identity.
pub fn choose_timing(
    choice: mediabox_core::ResolutionChoice,
    timings: &[Timing],
    sink: Option<&SinkVideo>,
) -> Option<Timing> {
    if let mediabox_core::ResolutionChoice::Fixed {
        width,
        height,
        refresh_mhz,
        interlaced,
    } = choice
    {
        if let Some(timing) = timings.iter().find(|timing| {
            timing.width == width
                && timing.height == height
                && timing.interlaced == interlaced
                && timing.refresh_mhz.abs_diff(refresh_mhz) <= 5
                && sink.is_none_or(|sink| !sink.modes_for(timing).is_empty())
        }) {
            return Some(*timing);
        }
    }
    auto_timing(timings, sink)
}

/// The depths a format is offered at, shallowest first. 4:2:2 is offered once,
/// at the source's depth: HDMI carries it in a twelve-bit container whatever
/// the depth, so a shallower 4:2:2 is the same signal with the low bits zero
/// (HDMI 1.0 s6.5).
fn depths(format: ColorFormat, source: &SourceCaps) -> Vec<u8> {
    if format == ColorFormat::Ycbcr422 {
        vec![source.max_bpc.min(12)]
    } else {
        (8..=source.max_bpc).step_by(2).collect()
    }
}

/// `display_info.max_tmds_clock` for a sink that declares nothing.
const UNDECLARED: u32 = 0;

/// Read an EDID's video capabilities, the way `drm_edid.c` does.
///
/// Returns `None` for something that is not an EDID at all
/// ([`crate::edid::EdidStatus::InvalidBase`] or absent). A valid EDID with no
/// CTA extension is a DVI sink: RGB at eight bits. Only blocks that passed
/// their checks are read ([`crate::edid`]); what was set aside is in
/// [`crate::edid::EdidReport`].
pub fn parse_sink_video(edid: &[u8]) -> Option<SinkVideo> {
    let edid = Edid::parse(edid);
    if !edid.status.usable() {
        return None;
    }
    let (cta, _) = CtaCapabilities::read(&edid);
    Some(sink_video(&cta))
}

/// The display screen's view of what the CTA extensions declare.
pub fn sink_video(cta: &CtaCapabilities) -> SinkVideo {
    let mut sink = SinkVideo {
        max_character_rate_khz: cta.max_tmds_khz(),
        rate_is_declared: false,
        advertised: Vec::new(),
        st2084: cta.hdr_static.is_some_and(|hdr| hdr.eotf_st2084),
        hlg: cta.hdr_static.is_some_and(|hdr| hdr.eotf_hlg),
        is_hdmi: cta.hdmi_vsdb.is_some(),
        ycbcr444: cta.ycbcr444,
        ycbcr422: cta.ycbcr422,
        rgb_deep: Vec::new(),
        ycbcr444_deep: Vec::new(),
        ycbcr420_deep: Vec::new(),
        y420_only: cta.y420_only.clone(),
        y420_also: cta.y420_also.clone(),
    };
    sink.rate_is_declared = sink.max_character_rate_khz != UNDECLARED;
    if let Some(vsdb) = cta.hdmi_vsdb {
        for (declared, bits) in [(vsdb.dc_30bit, 10u8), (vsdb.dc_36bit, 12), (vsdb.dc_48bit, 16)] {
            if declared {
                sink.rgb_deep.push(bits);
            }
        }
        if vsdb.dc_y444 {
            sink.ycbcr444_deep = sink.rgb_deep.clone();
        }
    }
    if let Some(forum) = cta.hdmi_forum {
        for (declared, bits) in [
            (forum.dc_420_30bit, 10u8),
            (forum.dc_420_36bit, 12),
            (forum.dc_420_48bit, 16),
        ] {
            if declared {
                sink.ycbcr420_deep.push(bits);
            }
        }
    }

    // The declared set, for showing.
    sink.advertised.push(ColorMode::new(ColorFormat::Rgb, 8));
    for bits in &sink.rgb_deep {
        sink.advertised.push(ColorMode::new(ColorFormat::Rgb, *bits));
    }
    if sink.ycbcr444 {
        sink.advertised.push(ColorMode::new(ColorFormat::Ycbcr444, 8));
        for bits in &sink.ycbcr444_deep {
            sink.advertised.push(ColorMode::new(ColorFormat::Ycbcr444, *bits));
        }
    }
    if sink.ycbcr422 {
        sink.advertised.push(ColorMode::new(ColorFormat::Ycbcr422, 12));
    }
    if !sink.y420_only.is_empty() || !sink.y420_also.is_empty() {
        sink.advertised.push(ColorMode::new(ColorFormat::Ycbcr420, 8));
        for bits in &sink.ycbcr420_deep {
            sink.advertised.push(ColorMode::new(ColorFormat::Ycbcr420, *bits));
        }
    }
    sink.advertised.sort_by_key(|mode| (mode.format, mode.bits));
    sink.advertised.dedup();
    sink
}

/// Every timing the EDID declares -- detailed timings and CTA video codes --
/// largest first.
pub fn parse_timings(edid: &[u8]) -> Vec<Timing> {
    let mut timings: Vec<Timing> = Vec::new();
    let edid = Edid::parse(edid);
    let Some(base) = edid.base() else {
        return timings;
    };
    for slot in 0..4 {
        let at = 54 + slot * 18;
        if let Some(timing) = detailed_timing(&base[at..at + 18], slot == 0) {
            timings.push(timing);
        }
    }
    let mut ignored = Vec::new();
    for (index, block) in edid.cta_extensions() {
        for db in data_blocks(index, block, &mut ignored) {
            if db.tag == 2 {
                for svd in db.payload {
                    if let Some(timing) = cta_timing(svd_to_vic(*svd)) {
                        timings.push(timing);
                    }
                }
            }
            // Modes the sink takes only as 4:2:0 are modes too
            // (`do_y420vdb_modes`): 2160p60 on a 300 MHz input is one.
            if db.tag == 7 && db.payload.first() == Some(&0x0E) {
                for svd in &db.payload[1..] {
                    if let Some(timing) = cta_timing(svd_to_vic(*svd)) {
                        timings.push(timing);
                    }
                }
            }
        }
        for dtd in cta_dtds(block) {
            if let Some(timing) = detailed_timing(dtd, false) {
                timings.push(timing);
            }
        }
    }
    timings.sort_by_key(|timing| {
        (
            std::cmp::Reverse(u32::from(timing.width) * u32::from(timing.height)),
            std::cmp::Reverse(timing.refresh_mhz),
        )
    });
    // The same timing declared twice -- a detailed timing that is also a CTA
    // code -- is one timing; what names it is its key, not its label.
    let mut unique: Vec<Timing> = Vec::new();
    for timing in timings {
        match unique.iter_mut().find(|kept| kept.key() == timing.key()) {
            Some(kept) => kept.preferred |= timing.preferred,
            None => unique.push(timing),
        }
    }
    unique
}

/// A timing the EDID declares by its code. The code is the one declared: the
/// kernel's mode for it carries its picture aspect ratio, which is what tells
/// VIC 7 from VIC 6 -- the same timing at 16:9 rather than 4:3 -- and which a
/// timing matched without it cannot recover.
fn cta_timing(vic: u8) -> Option<Timing> {
    cta_mode(vic).map(|mode| Timing::with_vic(mode, false, Some(vic)))
}

/// One 18-byte detailed timing descriptor, as `drm_mode_detailed()`
/// (drivers/gpu/drm/drm_edid.c) reads it, without the per-sink quirks: sync
/// offsets and widths, the bogus-total fix-up, the interlace quirk for the
/// CTA interlaced sizes, and the sync polarity. Stereo descriptors and ones
/// with no sync width are refused, as the kernel refuses them.
fn detailed_timing(bytes: &[u8], preferred: bool) -> Option<Timing> {
    if bytes.len() < 18 {
        return None;
    }
    let clock_10khz = u16::from_le_bytes([bytes[0], bytes[1]]);
    if clock_10khz == 0 {
        return None;
    }
    let hactive = u16::from(bytes[2]) | (u16::from(bytes[4] & 0xF0) << 4);
    let hblank = u16::from(bytes[3]) | (u16::from(bytes[4] & 0x0F) << 8);
    let vactive = u16::from(bytes[5]) | (u16::from(bytes[7] & 0xF0) << 4);
    let vblank = u16::from(bytes[6]) | (u16::from(bytes[7] & 0x0F) << 8);
    let hi = bytes[11];
    let hsync_offset = (u16::from(hi & 0xC0) << 2) | u16::from(bytes[8]);
    let hsync_width = (u16::from(hi & 0x30) << 4) | u16::from(bytes[9]);
    let vsync_offset = (u16::from(hi & 0x0C) << 2) | u16::from(bytes[10] >> 4);
    let vsync_width = (u16::from(hi & 0x03) << 4) | u16::from(bytes[10] & 0x0F);
    let misc = bytes[17];
    if hactive < 64 || vactive < 64 || misc & 0x20 != 0 || hsync_width == 0 || vsync_width == 0 {
        return None;
    }
    let hsync_start = hactive + hsync_offset;
    let hsync_end = hsync_start + hsync_width;
    let mut htotal = hactive + hblank;
    let mut vdisplay = vactive;
    let mut vsync_start = vactive + vsync_offset;
    let mut vsync_end = vsync_start + vsync_width;
    let mut vtotal = vactive + vblank;
    if hsync_end > htotal {
        htotal = hsync_end + 1;
    }
    if vsync_end > vtotal {
        vtotal = vsync_end + 1;
    }
    let mut flags = 0;
    // drm_mode_do_interlace_quirk: a descriptor for one of the CTA interlaced
    // sizes gives one field, and is doubled into the frame.
    if misc & 0x80 != 0 {
        const CTA_INTERLACED: [(u16, u16); 7] = [
            (1920, 1080),
            (720, 480),
            (1440, 480),
            (2880, 480),
            (720, 576),
            (1440, 576),
            (2880, 576),
        ];
        if CTA_INTERLACED
            .iter()
            .any(|&(w, h)| hactive == w && vdisplay == h / 2)
        {
            vdisplay *= 2;
            vsync_start *= 2;
            vsync_end *= 2;
            vtotal = vtotal * 2 | 1;
        }
        flags |= mode_flags::INTERLACE;
    }
    flags |= if misc & 0x02 != 0 { mode_flags::PHSYNC } else { mode_flags::NHSYNC };
    flags |= if misc & 0x04 != 0 { mode_flags::PVSYNC } else { mode_flags::NVSYNC };
    Some(Timing::from_mode(
        ModeTiming::new(
            u32::from(clock_10khz) * 10,
            hactive,
            hsync_start,
            hsync_end,
            htotal,
            0,
            vdisplay,
            vsync_start,
            vsync_end,
            vtotal,
            0,
            flags,
        ),
        preferred,
    ))
}
