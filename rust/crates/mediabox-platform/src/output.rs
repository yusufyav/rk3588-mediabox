//! The display screen's answer: every mode the kernel lists for the display
//! plugged in, grouped by size, with every colour mode at each and why it can
//! or cannot be sent there. Computed here, once, from the kernel's mode list
//! and the EDID; the television, the web page and `mediaboxctl` all draw this
//! and none of them decides a rule of its own.

use mediabox_core::{
    ColorMode, OutputGroup, OutputLink, OutputModeOffer, OutputOffer, OutputSetting,
    ResolutionChoice, edid_checkvalue,
};

use crate::video::{SinkVideo, SourceCaps, Timing, auto_timing, parse_sink_video};

/// The offer for one display.
///
/// `timings` is what the kernel lists for the connector, in its order. The
/// same timing listed twice -- a CTA mode and a VESA one with the same
/// numbers, which the kernel keeps both of -- is shown once, and a mode
/// label names exactly one timing: the CTA one when there is a choice, since
/// that is the one the sink's colour rules are written against.
///
/// `None` when `edid` is not an EDID: with nothing declared there is nothing
/// to reason from, and the screen says so instead of guessing.
pub fn offer(
    timings: &[Timing],
    edid: &[u8],
    connector: &str,
    sink_name: Option<String>,
    source: &SourceCaps,
) -> Option<OutputOffer> {
    let sink = parse_sink_video(edid)?;

    let mut unique: Vec<Timing> = Vec::new();
    for timing in timings {
        match unique
            .iter_mut()
            .find(|kept| kept.label() == timing.label())
        {
            Some(kept) => {
                let better = (timing.vic.is_some() && kept.vic.is_none())
                    || (timing.preferred && !kept.preferred && timing.vic.is_some() == kept.vic.is_some());
                if better {
                    let preferred = kept.preferred || timing.preferred;
                    *kept = *timing;
                    kept.preferred = preferred;
                } else if timing.preferred {
                    kept.preferred = true;
                }
            }
            None => unique.push(*timing),
        }
    }

    let auto = auto_timing(&unique, Some(&sink)).map(|timing| timing.label());

    let mut groups: Vec<OutputGroup> = Vec::new();
    for timing in &unique {
        let mode = mode_offer(&sink, timing, source);
        match groups
            .iter_mut()
            .find(|group| group.width == timing.width && group.height == timing.height)
        {
            Some(group) => group.modes.push(mode),
            None => groups.push(OutputGroup {
                width: timing.width,
                height: timing.height,
                name: size_name(timing.width, timing.height).to_string(),
                computer: false,
                modes: vec![mode],
            }),
        }
    }
    for group in &mut groups {
        group.computer = !group
            .modes
            .iter()
            .any(|mode| mode.vic.is_some_and(|vic| vic != 1));
        // Progressive before interlaced, fastest first.
        group
            .modes
            .sort_by_key(|mode| (mode.interlaced, std::cmp::Reverse(mode.refresh_mhz)));
    }
    // Television sizes before a monitor's, largest first.
    groups.sort_by_key(|group| {
        (
            group.computer,
            std::cmp::Reverse(u32::from(group.width) * u32::from(group.height)),
            std::cmp::Reverse(group.width),
        )
    });

    Some(OutputOffer {
        sink: edid_checkvalue(edid),
        edid_sha256: crate::edid::Edid::parse(edid)
            .identity()
            .map(|identity| identity.0),
        sink_name,
        connector: connector.to_string(),
        link: link(&sink, source),
        auto: auto.unwrap_or_default(),
        groups,
    })
}

fn mode_offer(sink: &SinkVideo, timing: &Timing, source: &SourceCaps) -> OutputModeOffer {
    let auto_hdr = sink
        .best_for(timing, true)
        .filter(|mode| sink.st2084 && mode.carries_hdr());
    OutputModeOffer {
        label: timing.label(),
        width: timing.width,
        height: timing.height,
        refresh_mhz: timing.refresh_mhz,
        interlaced: timing.interlaced,
        pixel_clock_khz: timing.pixel_clock_khz,
        htotal: timing.htotal,
        vtotal: timing.vtotal,
        preferred: timing.preferred,
        vic: timing.vic,
        timing_key: Some(timing.key()),
        timing: Some(timing.mode),
        cells: sink.cells_for(timing, source),
        auto_sdr: sink.best_for(timing, false),
        auto_hdr,
    }
}

fn link(sink: &SinkVideo, source: &SourceCaps) -> OutputLink {
    OutputLink {
        max_character_rate_khz: sink.max_character_rate_khz,
        declared_by: if !sink.rate_is_declared {
            String::new()
        } else if sink.max_character_rate_khz > 340_000 {
            "HF-VSDB".into()
        } else {
            "HDMI VSDB".into()
        },
        is_hdmi: sink.is_hdmi,
        ycbcr444: sink.ycbcr444,
        ycbcr422: sink.ycbcr422,
        rgb_deep: sink.rgb_deep.clone(),
        ycbcr420_deep: sink.ycbcr420_deep.clone(),
        y420_only: sink.y420_only.clone(),
        y420_also: sink.y420_also.clone(),
        st2084: sink.st2084,
        hlg: sink.hlg,
        source_max_khz: source.max_tmds_khz,
        source_max_bits: source.max_bpc,
    }
}

/// The display's own name, from the EDID's display product name descriptor
/// (tag 0xFC in one of the four 18-byte descriptors of the base block).
pub fn edid_name(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 {
        return None;
    }
    (0..4).find_map(|slot| {
        let at = 54 + slot * 18;
        let descriptor = &edid[at..at + 18];
        if descriptor[0..3] != [0, 0, 0] || descriptor[3] != 0xFC {
            return None;
        }
        let text: String = descriptor[5..18]
            .iter()
            .take_while(|byte| **byte != 0x0A)
            .map(|byte| char::from(*byte))
            .collect();
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    })
}

/// The name the industry gives a size, where it has one.
fn size_name(width: u16, height: u16) -> &'static str {
    match (width, height) {
        (7680, 4320) => "8K UHD",
        (4096, 2160) => "DCI 4K",
        (3840, 2160) => "4K UHD",
        (2560, 1440) => "QHD",
        (1920, 1080) => "Full HD",
        (1280, 720) => "HD",
        (720, 576) => "SD · PAL",
        (720, 480) => "SD · NTSC",
        _ => "",
    }
}

/// What the display controller says it is sending on a connector, read from
/// the vendor driver's `/sys/kernel/debug/dri/<n>/summary`: the media bus
/// format by name, and the colour mode that name is.
pub fn wire_bus_format(summary: &str, connector: &str) -> Option<(String, Option<ColorMode>)> {
    let mut ours = false;
    for line in summary.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Connector:") {
            ours = rest.split_whitespace().next() == Some(connector);
            continue;
        }
        if line.starts_with("Video Port") {
            ours = false;
            continue;
        }
        if ours
            && let Some(rest) = line.strip_prefix("bus_format[")
            && let Some((_, name)) = rest.split_once("]:")
        {
            let name = name.trim().to_string();
            let mode = bus_colour(&name);
            return Some((name, mode));
        }
    }
    None
}

/// The media bus format names (include/uapi/linux/media-bus-format.h) this
/// driver puts on an HDMI link.
fn bus_colour(name: &str) -> Option<ColorMode> {
    use mediabox_core::ColorFormat::*;
    let (format, bits) = match name {
        "RGB888_1X24" => (Rgb, 8),
        "RGB101010_1X30" => (Rgb, 10),
        "RGB121212_1X36" => (Rgb, 12),
        "YUV8_1X24" => (Ycbcr444, 8),
        "YUV10_1X30" => (Ycbcr444, 10),
        "YUV12_1X36" => (Ycbcr444, 12),
        "UYVY8_1X16" | "YUYV8_1X16" => (Ycbcr422, 8),
        "UYVY10_1X20" | "YUYV10_1X20" => (Ycbcr422, 10),
        "UYVY12_1X24" | "YUYV12_1X24" => (Ycbcr422, 12),
        "UYYVYY8_0_5X24" => (Ycbcr420, 8),
        "UYYVYY10_0_5X30" => (Ycbcr420, 10),
        "UYYVYY12_0_5X36" => (Ycbcr420, 12),
        _ => return None,
    };
    Some(ColorMode::new(format, bits))
}

/// What the other two owners of the display are told: Kodi's starting mode
/// and the modes it may change refresh between, and the browser compositor's
/// mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Kodi's own spelling: `%05d%05d%09.5fpstd`.
    pub kodi_screenmode: String,
    pub kodi_whitelist: Vec<String>,
    /// sway's spelling: `3840x2160@60.000Hz`.
    pub browser_mode: String,
    /// Every mode Kodi may be on, with the colour it sends there: SDR, and an
    /// HDR film -- `colour 3840x2160@296703/5500x2250 rgb:8 ycbcr422:10`,
    /// `none` where nothing carries HDR10. Keyed by the size, the pixel clock
    /// and the totals, which is what Kodi knows of the mode it set: the clock
    /// alone does not name a mode -- 2160p23.976 and 2160p29.97 share one --
    /// and neither do the totals: 1080i60 and 1080p30 share those, so an
    /// interlaced mode's size carries an `i`.
    pub colours: Vec<String>,
}

/// The chosen mode, as the other owners take it.
///
/// Kodi starts in it and may change only the refresh while a film plays --
/// every refresh this display lists at that size that something can be sent
/// at, fastest first, which is the order Kodi writes. That is what a
/// television box does when a film starts: the cadence follows the content,
/// the resolution does not.
///
/// The browser cannot follow a film's cadence, so it holds one rate. A mode a
/// person chose is that rate. On `Auto` it is the fastest refresh at that size
/// that is a whole multiple of 60 or 59.94 Hz, the rates the web is made of
/// (measured on a 143.999 Hz monitor: 60 fps video held for two refreshes and
/// three in turn, and stuttered), and the fastest when there is none.
pub fn plan(offer: &OutputOffer, setting: &OutputSetting) -> Option<Plan> {
    let chosen = offer.resolve(setting.resolution)?;
    let mut same: Vec<&OutputModeOffer> = offer
        .modes()
        .filter(|mode| {
            mode.width == chosen.width
                && mode.height == chosen.height
                && !mode.interlaced
                && mode.allowed().next().is_some()
        })
        .collect();
    same.sort_by(|a, b| hz(b).total_cmp(&hz(a)));
    let browser = match setting.resolution {
        ResolutionChoice::Fixed { .. } => chosen,
        ResolutionChoice::Auto => same
            .iter()
            .copied()
            .filter(|mode| whole(hz(mode), 60.0) || whole(hz(mode), 60_000.0 / 1001.0))
            .max_by(|a, b| hz(a).total_cmp(&hz(b)))
            .or_else(|| same.first().copied())
            .unwrap_or(chosen),
    };
    let spell = |mode: Option<ColorMode>| match mode {
        Some(mode) => format!("{}:{}", format_name(mode.format), mode.bits),
        None => "none".to_string(),
    };
    let colours = offer
        .modes()
        .filter(|mode| mode.allowed().next().is_some())
        .map(|mode| {
            format!(
                "colour {}x{}{}@{}/{}x{} {} {}",
                mode.width,
                mode.height,
                if mode.interlaced { "i" } else { "" },
                mode.pixel_clock_khz,
                mode.htotal,
                mode.vtotal,
                spell(offer.colour(setting, mode)),
                spell(offer.hdr_colour(setting, mode))
            )
        })
        .collect();
    Some(Plan {
        colours,
        kodi_screenmode: kodi(chosen),
        kodi_whitelist: same.iter().map(|mode| kodi(mode)).collect(),
        browser_mode: format!("{}x{}@{:.3}Hz", browser.width, browser.height, hz(browser)),
    })
}

/// The name this driver's `color_format` enum gives a format.
fn format_name(format: mediabox_core::ColorFormat) -> &'static str {
    use mediabox_core::ColorFormat::*;
    match format {
        Rgb => "rgb",
        Ycbcr444 => "ycbcr444",
        Ycbcr422 => "ycbcr422",
        Ycbcr420 => "ycbcr420",
    }
}

/// The refresh from the timing itself, as Kodi and sway compute it: the
/// exact fraction ([`OutputModeOffer::refresh`]), so an interlaced mode's is
/// its field rate -- 1080i60 is 60 here, not the 30 its clock over its totals
/// would say -- and only then a decimal, because that is what both are told.
fn hz(mode: &OutputModeOffer) -> f64 {
    let refresh = mode.refresh();
    if refresh.num == 0 {
        return f64::from(mode.refresh_mhz) / 1000.0;
    }
    refresh.as_f64()
}

/// Kodi's `videoscreen.screenmode`: `%05d%05d%09.5f` then `istd` or `pstd`
/// (`CDisplaySettings::GetStringFromResolution`); the refresh of an
/// interlaced mode is its field rate, as Kodi's own resolution list has it.
fn kodi(mode: &OutputModeOffer) -> String {
    format!(
        "{:05}{:05}{:09.5}{}std",
        mode.width,
        mode.height,
        hz(mode),
        if mode.interlaced { "i" } else { "p" }
    )
}

/// Every video frame shown for the same number of refreshes, to a thousandth.
fn whole(hz: f64, base: f64) -> bool {
    let ratio = hz / base;
    let n = ratio.round();
    n >= 1.0 && (ratio - n).abs() < 0.001
}
