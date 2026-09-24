//! The display screen's answer: every mode the kernel lists for the display
//! plugged in, grouped by size, with every colour mode at each and why it can
//! or cannot be sent there. Computed here, once, from the kernel's mode list
//! and the EDID; the television, the web page and `mediaboxctl` all draw this
//! and none of them decides a rule of its own.

use mediabox_core::{
    ColorMode, DisplayGeneration, DisplayIdentity, OutputGroup, OutputLink, OutputModeOffer,
    OutputOffer, OutputSetting, ResolutionChoice, TimingKey,
};

use crate::video::{SinkVideo, SourceCaps, Timing, auto_timing_source, parse_sink_video};

/// The offer for one display.
///
/// `timings` is what the kernel lists for the connector, in its order. The
/// same timing listed twice -- a CTA mode and a VESA one with the same
/// numbers, which the kernel keeps both of -- is shown once, and a mode
/// label names exactly one timing: the CTA one when there is a choice, since
/// that is the one the sink's colour rules are written against.
///
/// `None` when `edid` is not a whole, valid EDID: with nothing declared there
/// is nothing to reason from and no identity to bind a setting to, and the
/// screen says so instead of guessing.
pub fn offer(
    timings: &[Timing],
    edid: &[u8],
    connector: &str,
    sink_name: Option<String>,
    source: &SourceCaps,
) -> Option<OutputOffer> {
    let sink = parse_sink_video(edid)?;
    let parsed = crate::edid::Edid::parse(edid);
    let identity = parsed.identity()?;

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

    let auto = auto_timing_source(&unique, Some(&sink), source).map(|timing| timing.label());

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
        edid_sha256: identity.0,
        legacy_checkvalue: parsed.legacy_checkvalue(),
        sink_name,
        connector: connector.to_string(),
        link: link(&sink, source),
        auto: auto.unwrap_or_default(),
        groups,
    })
}

fn mode_offer(sink: &SinkVideo, timing: &Timing, source: &SourceCaps) -> OutputModeOffer {
    let auto_hdr = sink.best_hdr10(timing, source);
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
        timing_key: timing.key(),
        timing: timing.mode,
        cells: sink.cells_for(timing, source),
        auto_sdr: sink.best_for_source(timing, false, source),
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
        static_metadata_type1: sink.static_metadata_type1,
        bt2020_rgb: sink.bt2020_rgb,
        bt2020_ycc: sink.bt2020_ycc,
        source_max_khz: source.max_tmds_khz,
        source_max_bits: source.max_bpc,
        source_hdr10: source.hdr10,
        source_profile: String::new(),
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

/// The colour mode a media bus format name is, for the names
/// (include/uapi/linux/media-bus-format.h) this driver puts on an HDMI link --
/// the `bus_format` the display controller reports in debugfs `summary`.
pub fn bus_colour(name: &str) -> Option<ColorMode> {
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

/// Where the plan is, under `/run` ([`crate::Roots::run`]).
pub const PLAN_FILE: &str = "mediabox/output-plan";

/// The version of `/run/mediabox/output-plan` this build writes and reads.
pub const PLAN_SCHEMA: u32 = 2;

/// What a plan was made for. A plan is only ever used for exactly this: the
/// same boot, the same observer generation, the same connector and sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub generation: DisplayGeneration,
    pub identity: DisplayIdentity,
    /// The transmitter behind the connector, or `-` when none is known.
    pub transmitter: String,
    /// The source profile the offer was computed under, by name.
    pub source_profile: String,
}

/// What the other two owners of the display are told: Kodi's starting mode
/// and the modes it may change refresh between, and the browser compositor's
/// mode -- and what all of it was made for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub provenance: Provenance,
    /// The timing the kept setting resolves to.
    pub timing_key: TimingKey,
    /// Kodi's own spelling: `%05d%05d%09.5fpstd`.
    pub kodi_screenmode: String,
    pub kodi_whitelist: Vec<String>,
    /// sway's spelling: `3840x2160@60.000Hz`, for `connector` only.
    pub browser_mode: String,
    /// Every mode Kodi may be on, with the colour it sends there: SDR, and an
    /// HDR film -- `colour 3840x2160@296703/5500x2250 rgb:8 ycbcr422:10`,
    /// `none` where nothing carries HDR10. Keyed by the size, the pixel clock
    /// and the totals, which is what Kodi knows of the mode it set: the clock
    /// alone does not name a mode -- 2160p23.976 and 2160p29.97 share one --
    /// and neither do the totals: 1080i60 and 1080p30 share those, so an
    /// interlaced mode's size carries an `i`. This line's shape is what
    /// Kodi's own reader parses (patches/kodi/0013) and does not change.
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
///
/// `None` when the offer is not the provenance's sink: a plan is never made
/// for one display from another's modes.
pub fn plan(offer: &OutputOffer, setting: &OutputSetting, provenance: Provenance) -> Option<Plan> {
    if offer.edid_sha256 != provenance.identity.edid_sha256
        || offer.connector != provenance.identity.connector
    {
        return None;
    }
    let setting = setting.for_sink(&offer.edid_sha256);
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
        ResolutionChoice::Timing { .. } => chosen,
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
                spell(offer.colour(&setting, mode)),
                spell(offer.hdr_colour(&setting, mode))
            )
        })
        .collect();
    Some(Plan {
        provenance,
        timing_key: chosen.timing_key,
        colours,
        kodi_screenmode: kodi(chosen),
        kodi_whitelist: same.iter().map(|mode| kodi(mode)).collect(),
        browser_mode: format!("{}x{}@{:.3}Hz", browser.width, browser.height, hz(browser)),
    })
}

impl Plan {
    /// The file: `key=value` lines, then the colour lines.
    pub fn render(&self) -> String {
        let p = &self.provenance;
        let mut text = format!(
            "schema={PLAN_SCHEMA}\nboot_id={}\ngeneration={}\nconnector={}\ntransmitter={}\n\
             edid_sha256={}\ntiming_key={}\nsource_profile={}\nkodi_screenmode={}\n\
             kodi_whitelist={}\nbrowser_mode={}\n",
            p.generation.boot_id,
            p.generation.seq,
            p.identity.connector,
            p.transmitter,
            p.identity.edid_sha256,
            self.timing_key,
            p.source_profile,
            self.kodi_screenmode,
            self.kodi_whitelist.join(","),
            self.browser_mode
        );
        for line in &self.colours {
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    /// A plan file, whole. Anything else -- a plan from an older version
    /// without its provenance, a truncated one -- is not a plan.
    pub fn parse(text: &str) -> Result<Plan, String> {
        let mut values = std::collections::BTreeMap::new();
        let mut colours = Vec::new();
        for line in text.lines() {
            if line.starts_with("colour ") {
                colours.push(line.to_string());
            } else if let Some((key, value)) = line.split_once('=') {
                values.insert(key, value);
            }
        }
        let get = |key: &str| {
            values
                .get(key)
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("plan has no {key}"))
        };
        let schema = get("schema")?;
        if schema != PLAN_SCHEMA.to_string() {
            return Err(format!("plan schema {schema}, this build reads {PLAN_SCHEMA}"));
        }
        Ok(Plan {
            provenance: Provenance {
                generation: DisplayGeneration {
                    boot_id: get("boot_id")?,
                    seq: get("generation")?
                        .parse()
                        .map_err(|_| "plan generation is not a number".to_string())?,
                },
                identity: DisplayIdentity {
                    connector: get("connector")?,
                    edid_sha256: get("edid_sha256")?,
                },
                transmitter: get("transmitter")?,
                source_profile: get("source_profile")?,
            },
            timing_key: TimingKey::parse(&get("timing_key")?)
                .ok_or_else(|| "plan timing_key is not a key".to_string())?,
            kodi_screenmode: get("kodi_screenmode")?,
            kodi_whitelist: get("kodi_whitelist")?
                .split(',')
                .map(str::to_string)
                .collect(),
            browser_mode: get("browser_mode")?,
            colours,
        })
    }

    /// Why this plan is not for what is plugged in now, or `None` when it is.
    ///
    /// Two independent checks. The observer's generation: the same boot and
    /// the same `seq` -- anything material that changed since, a sink, a mode
    /// list, a source profile, moved it. And the hardware itself, read now:
    /// the selected connector and the SHA-256 of the EDID it publishes. A
    /// plan made for one sink is never used on another, whatever the
    /// generation says.
    pub fn stale(
        &self,
        generation: Option<&DisplayGeneration>,
        identity: Option<&DisplayIdentity>,
    ) -> Option<String> {
        let p = &self.provenance;
        match generation {
            None => return Some("no observer generation to check the plan against".into()),
            Some(now) if *now != p.generation => {
                return Some(format!(
                    "plan is for generation {}:{}, the display is at {}:{}",
                    p.generation.boot_id, p.generation.seq, now.boot_id, now.seq
                ));
            }
            Some(_) => {}
        }
        match identity {
            None => Some("no connected output with a valid EDID now".into()),
            Some(now) if *now != p.identity => Some(format!(
                "plan is for {} {}, now {} {}",
                p.identity.connector,
                short(&p.identity.edid_sha256),
                now.connector,
                short(&now.edid_sha256)
            )),
            Some(_) => None,
        }
    }

    /// sway's configuration for this plan: the mode, on this plan's
    /// connector only. A wildcard would put this sink's mode on every other
    /// output sway lights.
    pub fn sway_output(&self) -> String {
        format!(
            "output {} mode {}\n",
            self.provenance.identity.connector, self.browser_mode
        )
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
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
