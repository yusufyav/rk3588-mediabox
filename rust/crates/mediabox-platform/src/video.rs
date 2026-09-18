//! What the sink will accept, and which of those the link can actually carry.
//!
//! A television advertises the pixel formats it understands and, separately,
//! how fast a signal it can receive. Those two facts are not the same question
//! and the interesting answer is their intersection: at 4K60 a sink may well
//! understand RGB 10-bit and still be unable to be sent it, because RGB 10-bit
//! at that mode needs 742 MHz and the port tops out at 600.
//!
//! Getting this wrong is not theoretical. A Sony KD-65XE9005 reports HDMI 1 as
//! a 300 MHz port and HDMI 3 as a 600 MHz one, on the same set, with different
//! EDIDs. A player that asks for RGB 10-bit on the first of those does not get
//! an error: the driver quietly subsamples to 4:2:2, the picture is fine, and
//! the colorimetry the sink is told still says RGB. The television then decodes
//! YCbCr pixels with an RGB matrix and HDR arrives wrong.
//!
//! So this module computes, rather than assumes, and everything it reports is
//! read out of the EDID. `modes_for` is the list a person may be offered and
//! `best_for` is what to use when nobody has chosen. The Android stack on the
//! same hardware arrives at the same list; `tests/color_modes.rs` holds the
//! EDIDs of both ports of that Sony and checks this code against what the
//! vendor box put on screen.

pub use mediabox_core::{ColorFormat, ColorMode};
use serde::{Deserialize, Serialize};

/// What one sink said it can be sent, on the port it is plugged into.
///
/// Every field is parsed from the EDID. A sink that advertises nothing gets the
/// conservative answers, never generous ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkVideo {
    /// The fastest signal the sink will accept, in kHz. From the HDMI Forum
    /// VSDB when the sink has one, otherwise from the HDMI 1.4 VSDB, otherwise
    /// the 165 MHz every HDMI receiver is required to manage.
    pub max_character_rate_khz: u32,
    /// Whether the sink said so itself, or whether this is the floor assumed
    /// for a sink that said nothing. Worth reporting: it is the difference
    /// between a measured limit and a guess.
    pub rate_is_declared: bool,
    /// The format/depth pairs the sink advertises, before any link budget is
    /// applied. This is the same set the Amlogic vendor stack calls `dc_cap`.
    pub advertised: Vec<ColorMode>,
    /// Whether the sink declared the HDR10 transfer function (SMPTE ST 2084).
    pub st2084: bool,
    /// Hybrid Log-Gamma.
    pub hlg: bool,
}

impl SinkVideo {
    /// Every advertised mode this link can actually carry at `pixel_clock_khz`,
    /// in the order they are worth showing: full chroma first, then the
    /// subsampled formats, deepest colour first within each.
    pub fn modes_for(&self, pixel_clock_khz: u32) -> Vec<ColorMode> {
        let mut modes: Vec<ColorMode> = self
            .advertised
            .iter()
            .copied()
            .filter(|mode| mode.character_rate_khz(pixel_clock_khz) <= self.max_character_rate_khz)
            .collect();
        modes.sort_by_key(|mode| (mode.format, std::cmp::Reverse(mode.bits)));
        modes
    }

    /// What to send when nobody has chosen.
    ///
    /// For HDR the deepest colour that fits wins, and chroma is what gets given
    /// up to reach it -- 4:2:2 12-bit before 4:4:4 8-bit. That is not a
    /// preference, it is the same choice the vendor Android stack makes on this
    /// hardware, and it is the right way round: subsampled chroma is a detail
    /// at viewing distance, and eight-bit HDR bands where anyone can see it.
    ///
    /// For SDR the order reverses: there is nothing to gain from the extra bits
    /// and full chroma keeps text and the interface crisp.
    pub fn best_for(&self, pixel_clock_khz: u32, hdr: bool) -> Option<ColorMode> {
        let modes = self.modes_for(pixel_clock_khz);
        if hdr {
            // Deepest first; among equals, the fullest chroma.
            modes
                .iter()
                .filter(|mode| mode.carries_hdr())
                .copied()
                .max_by_key(|mode| (mode.bits, std::cmp::Reverse(mode.format)))
                .or_else(|| self.best_for(pixel_clock_khz, false))
        } else {
            // Fullest chroma first; among equals, the fewest bits, because deep
            // colour buys nothing here and costs link budget.
            modes
                .iter()
                .copied()
                .min_by_key(|mode| (mode.format, mode.bits))
        }
    }

    /// Whether HDR10 can be both signalled and carried at this mode.
    ///
    /// Both halves matter. A sink that declares ST 2084 on a link with no room
    /// for ten bits cannot be sent HDR worth having, and a link with room to
    /// spare cannot be sent HDR to a sink that never declared it.
    pub fn hdr10_fits(&self, pixel_clock_khz: u32) -> bool {
        self.st2084
            && self
                .modes_for(pixel_clock_khz)
                .iter()
                .any(|mode| mode.carries_hdr())
    }
}

/// The rate assumed for a sink that declares none: the HDMI floor, 165 MHz.
const DEFAULT_CHARACTER_RATE_KHZ: u32 = 165_000;

/// Read an EDID's video capabilities.
///
/// Returns `None` for something that is not an EDID at all. A valid EDID with
/// no CTA extension is not an error -- it is a sink that advertises only RGB at
/// eight bits, which is exactly what it is then told.
pub fn parse_sink_video(edid: &[u8]) -> Option<SinkVideo> {
    if edid.len() < 128 || edid[0..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }

    // Every HDMI sink takes RGB at eight bits. Nothing else is assumed.
    let mut video = SinkVideo {
        max_character_rate_khz: DEFAULT_CHARACTER_RATE_KHZ,
        rate_is_declared: false,
        advertised: vec![ColorMode::new(ColorFormat::Rgb, 8)],
        st2084: false,
        hlg: false,
    };

    // Deep colour is declared once, in the HDMI 1.4 VSDB, and applies to RGB;
    // whether it also applies to 4:4:4 is a separate bit in the same byte.
    let mut deep_bits: Vec<u8> = Vec::new();
    let mut deep_444 = false;
    let mut ycbcr444 = false;
    let mut ycbcr422 = false;
    let mut ycbcr420_any = false;
    let mut deep_420_bits: Vec<u8> = Vec::new();
    let mut forum_rate = false;

    for block in edid[128..].chunks_exact(128) {
        // CTA-861 extension only.
        if block[0] != 0x02 {
            continue;
        }
        let dtd_start = block[2] as usize;
        if block[1] >= 3 {
            // Byte 3 carries the colour-format support flags.
            ycbcr444 |= block[3] & 0x20 != 0;
            ycbcr422 |= block[3] & 0x10 != 0;
        }
        // The data block collection runs from byte 4 to the first detailed
        // timing. A sink with no collection says so by putting them at 4.
        if !(4..=127).contains(&dtd_start) {
            continue;
        }
        let mut at = 4usize;
        while at < dtd_start && at < block.len() {
            let header = block[at];
            let len = (header & 0x1F) as usize;
            let tag = header >> 5;
            let end = at + 1 + len;
            if len == 0 || end > block.len() || end > dtd_start {
                break;
            }
            let payload = &block[at + 1..end];
            match tag {
                // Vendor-specific: HDMI 1.4 (00-0C-03) or HDMI Forum (C4-5D-D8).
                3 if payload.len() >= 3 => {
                    let oui = [payload[0], payload[1], payload[2]];
                    if oui == [0x03, 0x0C, 0x00] {
                        // After the three OUI bytes come two bytes of source
                        // physical address, then the deep colour flags, then
                        // the maximum TMDS clock in units of 5 MHz. A sink that
                        // also carries an HDMI Forum block leaves a value here
                        // that a 1.4 source can live with, so the Forum block
                        // wins whichever order the two appear in.
                        if payload.len() >= 7 && payload[6] != 0 && !forum_rate {
                            video.max_character_rate_khz = u32::from(payload[6]) * 5_000;
                            video.rate_is_declared = true;
                        }
                        if payload.len() >= 6 {
                            let dc = payload[5];
                            deep_444 |= dc & 0x08 != 0;
                            if dc & 0x10 != 0 {
                                deep_bits.push(10);
                            }
                            if dc & 0x20 != 0 {
                                deep_bits.push(12);
                            }
                            if dc & 0x40 != 0 {
                                deep_bits.push(16);
                            }
                        }
                    } else if oui == [0xD8, 0x5D, 0xC4] {
                        // The HDMI Forum block supersedes the 1.4 rate: a 2.0
                        // sink puts its real ceiling here and leaves the older
                        // field at a value a 1.4 source can live with.
                        if payload.len() >= 5 && payload[4] != 0 {
                            video.max_character_rate_khz = u32::from(payload[4]) * 5_000;
                            video.rate_is_declared = true;
                            forum_rate = true;
                        }
                        // 4:2:0 deep colour is declared separately, and a sink
                        // that takes 4:2:0 at ten bits very often does not take
                        // it at twelve.
                        if payload.len() >= 7 {
                            let dc420 = payload[6];
                            if dc420 & 0x01 != 0 {
                                deep_420_bits.push(10);
                            }
                            if dc420 & 0x02 != 0 {
                                deep_420_bits.push(12);
                            }
                            if dc420 & 0x04 != 0 {
                                deep_420_bits.push(16);
                            }
                        }
                    }
                }
                // Extended tag: the byte after the header says which.
                7 if !payload.is_empty() => match payload[0] {
                    // HDR static metadata: the transfer functions the sink has.
                    0x06 if payload.len() >= 2 => {
                        video.st2084 |= payload[1] & 0x04 != 0;
                        video.hlg |= payload[1] & 0x08 != 0;
                    }
                    // Either 4:2:0 block means the sink takes 4:2:0 for at
                    // least some modes. Which modes is a per-VIC question this
                    // does not need: the caller already knows the mode it is
                    // asking about is one the sink listed.
                    0x0E | 0x0F => ycbcr420_any = true,
                    _ => {}
                },
                _ => {}
            }
            at = end;
        }
    }

    deep_bits.sort_unstable();
    deep_bits.dedup();
    deep_420_bits.sort_unstable();
    deep_420_bits.dedup();

    for bits in &deep_bits {
        // 16 bits per component is declarable and not something this product
        // has any use for; it is dropped rather than offered.
        if *bits <= 12 {
            video.advertised.push(ColorMode::new(ColorFormat::Rgb, *bits));
        }
    }
    if ycbcr444 {
        video.advertised.push(ColorMode::new(ColorFormat::Ycbcr444, 8));
        if deep_444 {
            for bits in &deep_bits {
                if *bits <= 12 {
                    video
                        .advertised
                        .push(ColorMode::new(ColorFormat::Ycbcr444, *bits));
                }
            }
        }
    }
    if ycbcr422 {
        // 4:2:2 is carried twelve bits wide or not at all.
        video.advertised.push(ColorMode::new(ColorFormat::Ycbcr422, 12));
    }
    if ycbcr420_any {
        video.advertised.push(ColorMode::new(ColorFormat::Ycbcr420, 8));
        for bits in &deep_420_bits {
            if *bits <= 12 {
                video
                    .advertised
                    .push(ColorMode::new(ColorFormat::Ycbcr420, *bits));
            }
        }
    }

    video.advertised.sort_by_key(|mode| (mode.format, mode.bits));
    video.advertised.dedup();
    Some(video)
}

/// One timing the sink says it supports, with the pixel clock that decides
/// what can be sent at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timing {
    pub width: u16,
    pub height: u16,
    /// Millihertz, so 23.976 Hz is a number rather than a rounding.
    pub refresh_mhz: u32,
    pub pixel_clock_khz: u32,
    /// The CTA-861 code, when the sink named one. Detailed timings have none.
    pub vic: Option<u8>,
}

impl Timing {
    /// `3840x2160p23.976` — what a person recognises a mode by.
    pub fn label(self) -> String {
        format!(
            "{}x{}p{:.3}",
            self.width,
            self.height,
            f64::from(self.refresh_mhz) / 1000.0
        )
    }
}

/// The CTA-861 video codes this appliance can encounter, with their timings.
///
/// A short table rather than a complete one, and unknown codes are skipped
/// instead of guessed: a mode this product cannot name is a mode it has no
/// business computing a link budget for. The fractional entries are the
/// 1000/1001 rates, which is what film actually arrives as.
const CTA_TIMINGS: &[(u8, u16, u16, u32, u32)] = &[
    // vic, width, height, refresh mHz, pixel clock kHz
    (4, 1280, 720, 60_000, 74_250),
    (16, 1920, 1080, 60_000, 148_500),
    (17, 720, 576, 50_000, 27_000),
    (19, 1280, 720, 50_000, 74_250),
    (31, 1920, 1080, 50_000, 148_500),
    (32, 1920, 1080, 24_000, 74_250),
    (33, 1920, 1080, 25_000, 74_250),
    (34, 1920, 1080, 30_000, 74_250),
    (60, 1280, 720, 24_000, 59_400),
    (61, 1280, 720, 25_000, 74_250),
    (62, 1280, 720, 30_000, 74_250),
    (93, 3840, 2160, 24_000, 297_000),
    (94, 3840, 2160, 25_000, 297_000),
    (95, 3840, 2160, 30_000, 297_000),
    (96, 3840, 2160, 50_000, 594_000),
    (97, 3840, 2160, 60_000, 594_000),
    (98, 4096, 2160, 24_000, 297_000),
    (99, 4096, 2160, 25_000, 297_000),
    (100, 4096, 2160, 30_000, 297_000),
    (101, 4096, 2160, 50_000, 594_000),
    (102, 4096, 2160, 60_000, 594_000),
];

/// Every timing the sink lists, largest first.
///
/// Read from the CTA video data block, where a television puts the modes it
/// actually wants to be sent, plus the base block's own detailed timings for
/// the preferred mode a monitor may only describe that way. Interlaced codes
/// are left out: nothing this product drives is interlaced, and offering one
/// would only be a mode a person could pick and regret.
pub fn parse_timings(edid: &[u8]) -> Vec<Timing> {
    let mut timings: Vec<Timing> = Vec::new();
    if edid.len() < 128 || edid[0..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return timings;
    }

    // The base block's detailed timings: four slots of eighteen bytes, a
    // zero pixel clock meaning the slot holds something else.
    for slot in 0..4 {
        let at = 54 + slot * 18;
        if at + 18 > 128 {
            break;
        }
        if let Some(timing) = detailed_timing(&edid[at..at + 18]) {
            timings.push(timing);
        }
    }

    for block in edid[128..].chunks_exact(128) {
        if block[0] != 0x02 {
            continue;
        }
        let dtd_start = block[2] as usize;
        if !(4..=127).contains(&dtd_start) {
            continue;
        }
        let mut at = 4usize;
        while at < dtd_start && at < block.len() {
            let header = block[at];
            let len = (header & 0x1F) as usize;
            let end = at + 1 + len;
            if len == 0 || end > block.len() || end > dtd_start {
                break;
            }
            // Tag 2 is the video data block: one byte per short video
            // descriptor, the low seven bits of which are the code.
            if header >> 5 == 2 {
                for byte in &block[at + 1..end] {
                    let vic = byte & 0x7F;
                    if let Some(timing) = cta_timing(vic) {
                        timings.push(timing);
                    }
                }
            }
            at = end;
        }
        // And the extension block's own detailed timings.
        let mut at = dtd_start;
        while at + 18 <= 127 {
            if let Some(timing) = detailed_timing(&block[at..at + 18]) {
                timings.push(timing);
            }
            at += 18;
        }
    }

    timings.sort_by_key(|timing| {
        (
            std::cmp::Reverse(u32::from(timing.width) * u32::from(timing.height)),
            std::cmp::Reverse(timing.refresh_mhz),
        )
    });
    timings.dedup_by_key(|timing| (timing.width, timing.height, timing.refresh_mhz));
    timings
}

fn cta_timing(vic: u8) -> Option<Timing> {
    CTA_TIMINGS
        .iter()
        .find(|(code, ..)| *code == vic)
        .map(|(code, width, height, refresh_mhz, pixel_clock_khz)| Timing {
            width: *width,
            height: *height,
            refresh_mhz: *refresh_mhz,
            pixel_clock_khz: *pixel_clock_khz,
            vic: Some(*code),
        })
}

/// One eighteen-byte detailed timing descriptor.
fn detailed_timing(bytes: &[u8]) -> Option<Timing> {
    if bytes.len() < 18 {
        return None;
    }
    // A zero pixel clock marks a descriptor that is a name or a range, not a
    // timing.
    let clock_10khz = u16::from_le_bytes([bytes[0], bytes[1]]);
    if clock_10khz == 0 {
        return None;
    }
    // Interlaced: the top bit of the last byte.
    if bytes[17] & 0x80 != 0 {
        return None;
    }
    let width = u16::from(bytes[2]) | (u16::from(bytes[4] & 0xF0) << 4);
    let hblank = u16::from(bytes[3]) | (u16::from(bytes[4] & 0x0F) << 8);
    let height = u16::from(bytes[5]) | (u16::from(bytes[7] & 0xF0) << 4);
    let vblank = u16::from(bytes[6]) | (u16::from(bytes[7] & 0x0F) << 8);
    if width == 0 || height == 0 {
        return None;
    }
    let pixel_clock_khz = u32::from(clock_10khz) * 10;
    let total = u64::from(width + hblank) * u64::from(height + vblank);
    if total == 0 {
        return None;
    }
    // Millihertz, computed from the timing rather than rounded to whole hertz,
    // so 23.976 does not become 24 and a film does not judder.
    let refresh_mhz = (u64::from(pixel_clock_khz) * 1_000_000 / total) as u32;
    Some(Timing {
        width,
        height,
        refresh_mhz,
        pixel_clock_khz,
        vic: None,
    })
}
