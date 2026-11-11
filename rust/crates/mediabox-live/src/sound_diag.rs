//! The sound's diagnosis. Each place the sound passes is counted on its own,
//! the decoded samples are measured before the viewer's volume, and every
//! loss or silence of `EVENT_NS` or more becomes one record of what every
//! layer did in the same window -- with a cause only where the counts show
//! one (`classify`), `unknown` otherwise.
//!
//! - A, transport: the sound PID's packets as the reader (`es`) saw them --
//!   transport errors, continuity, duplicates, announced discontinuities,
//!   the PMT moving the sound.
//! - B, timeline: the parser's frames, each one's PTS against where the one
//!   before ended (sample exact), and the discontinuities flagged on them.
//! - C, decoder: units in; PCM buffers and sample frames out; new caps.
//! - D, decoded PCM, before the volume (`Meter`): per channel peak and RMS,
//!   digital zero, below threshold. A buffer's length comes from its bytes
//!   and its caps, never from its duration field; caps it cannot read are
//!   counted as such, never as silence.
//! - E, sink: buffers into it (gap, overlap), audiobasesink's late samples
//!   and discontinuities, the ALSA device's restarts -- one of ours (a
//!   reopen) or not (an XRUN recovered).
//! - F, viewer and route: mute, volume, the device the sound goes to.
//!
//! Levels are in dBFS (full scale 1.0, the sample scaling ITU-R BS.1770
//! measures from). BS.1770 and EBU R128 define how loud programme is; they
//! define no level below which sound is "lost" and no cause for it. The
//! thresholds here are this monitor's own (`ENTER_DBFS`, `EXIT_DBFS`), and
//! what crosses them is named as what was seen --
//! `decoded_pcm_below_threshold` -- not why it was so.

use std::collections::VecDeque;

/// A loss or silence this long is an event with a record of its own.
pub const EVENT_NS: u64 = 200_000_000;
/// Into a silent run below this, out of it above `EXIT_DBFS` (hysteresis).
pub const ENTER_DBFS: f32 = -60.0;
pub const EXIT_DBFS: f32 = -54.0;
/// Sound above `EXIT_DBFS` this long ends a silent run (release); shorter,
/// the run goes on (a click in silence is not its end).
pub const RELEASE_NS: u64 = 50_000_000;
/// Channels measured one by one; more are counted as unsupported.
pub const MAX_CHANNELS: usize = 8;
/// What a window looks back before an event's start.
const WINDOW_BEFORE_NS: u64 = 500_000_000;
/// Snapshots kept of the counters, one a report (`health::PERIOD`).
const HISTORY: usize = 64;

fn linear(dbfs: f32) -> f32 {
    10f32.powf(dbfs / 20.0)
}

pub fn dbfs(level: f32) -> f32 {
    if level <= 0.0 { f32::NEG_INFINITY } else { 20.0 * level.log10() }
}

// ----------------------------------------------------------------- formats

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    S16,
    S32,
    F32,
}

impl Sample {
    fn bytes(self) -> usize {
        match self {
            Sample::S16 => 2,
            Sample::S32 | Sample::F32 => 4,
        }
    }
}

/// Decoded PCM as its caps say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample: Sample,
    pub rate: u32,
    pub channels: usize,
    pub interleaved: bool,
}

impl PcmFormat {
    /// From the caps' fields; an error names what could not be read.
    pub fn from_caps(
        name: &str,
        format: Option<&str>,
        rate: Option<i32>,
        channels: Option<i32>,
        layout: Option<&str>,
    ) -> Result<Self, &'static str> {
        if name != "audio/x-raw" {
            return Err("not raw audio");
        }
        let sample = match format {
            Some("S16LE") => Sample::S16,
            Some("S32LE") => Sample::S32,
            Some("F32LE") => Sample::F32,
            _ => return Err("sample format"),
        };
        let rate = rate.filter(|&r| r > 0).ok_or("rate")? as u32;
        let channels = channels.filter(|&c| c > 0).ok_or("channels")? as usize;
        if channels > MAX_CHANNELS {
            return Err("channels");
        }
        let interleaved = match layout {
            None | Some("interleaved") => true,
            Some("non-interleaved") => false,
            Some(_) => return Err("layout"),
        };
        Ok(Self { sample, rate, channels, interleaved })
    }

    fn frame_bytes(&self) -> usize {
        self.sample.bytes() * self.channels
    }

    /// Sample frames as nanoseconds.
    pub fn ns(&self, frames: u64) -> u64 {
        (frames as u128 * 1_000_000_000 / self.rate as u128) as u64
    }

    /// One sample period, ns: how far a PTS may be off and still be exact
    /// (timestamps are nanoseconds; samples are not).
    fn tolerance_ns(&self) -> u64 {
        1_000_000_000u64.div_ceil(self.rate as u64)
    }
}

/// One buffer's samples, measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub frames: u64,
    pub channels: usize,
    pub peak: [f32; MAX_CHANNELS],
    pub sum_sq: [f64; MAX_CHANNELS],
    /// Frames whose every sample is exactly zero.
    pub zero_frames: u64,
}

impl Levels {
    pub fn peak_max(&self) -> f32 {
        self.peak[..self.channels].iter().copied().fold(0.0, f32::max)
    }

    pub fn rms(&self, channel: usize) -> f32 {
        if self.frames == 0 { 0.0 } else { (self.sum_sq[channel] / self.frames as f64).sqrt() as f32 }
    }
}

/// The samples of `data` in `format`. The bytes must be whole frames.
pub fn measure(format: &PcmFormat, data: &[u8]) -> Result<Levels, &'static str> {
    let frame = format.frame_bytes();
    if !data.len().is_multiple_of(frame) {
        return Err("not whole frames");
    }
    let frames = data.len() / frame;
    let mut levels = Levels {
        frames: frames as u64,
        channels: format.channels,
        peak: [0.0; MAX_CHANNELS],
        sum_sq: [0.0; MAX_CHANNELS],
        zero_frames: 0,
    };
    let size = format.sample.bytes();
    let value = |bytes: &[u8]| -> f32 {
        match format.sample {
            Sample::S16 => f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0,
            Sample::S32 => (i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64 / 2_147_483_648.0) as f32,
            Sample::F32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }
    };
    for i in 0..frames {
        let mut all_zero = true;
        for c in 0..format.channels {
            let at = if format.interleaved { (i * format.channels + c) * size } else { (c * frames + i) * size };
            let v = value(&data[at..at + size]);
            let a = v.abs();
            if a > levels.peak[c] {
                levels.peak[c] = a;
            }
            levels.sum_sq[c] += f64::from(v) * f64::from(v);
            if v != 0.0 {
                all_zero = false;
            }
        }
        levels.zero_frames += u64::from(all_zero);
    }
    Ok(levels)
}

// ------------------------------------------------------------------- meter

/// What a silent run's buffers were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quiet {
    /// Every sample exactly zero.
    DigitalZero,
    /// Below the threshold, not all zero.
    BelowThreshold,
}

impl Quiet {
    pub fn name(self) -> &'static str {
        match self {
            Quiet::DigitalZero => "decoded_pcm_digital_zero",
            Quiet::BelowThreshold => "decoded_pcm_below_threshold",
        }
    }
}

/// Why a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    /// Sound above the exit threshold for `RELEASE_NS`.
    Sound,
    Gap,
    Overlap,
    Backward,
    Discont,
    Generation,
    Format,
    /// No samples came (`Diag::tick`).
    Absent,
}

impl RunEnd {
    pub fn name(self) -> &'static str {
        match self {
            RunEnd::Sound => "sound",
            RunEnd::Gap => "pts-gap",
            RunEnd::Overlap => "pts-overlap",
            RunEnd::Backward => "pts-backward",
            RunEnd::Discont => "discont",
            RunEnd::Generation => "channel",
            RunEnd::Format => "format",
            RunEnd::Absent => "no-samples",
        }
    }
}

/// A run of silent buffers, contiguous to the sample.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub kind: Quiet,
    pub start_pts: u64,
    /// Silent sample frames in it, as time.
    pub silent_ns: u64,
    /// Sound above the exit threshold since the last silent buffer, and
    /// where it began: the run's end if it lasts `RELEASE_NS`.
    release_ns: u64,
    release_pts: Option<u64>,
    /// Over the run: frames, the loudest sample, sum of squares (all
    /// channels), zero frames.
    pub frames: u64,
    pub peak: f32,
    sum_sq: f64,
    samples: u64,
    pub zero_frames: u64,
    /// For a run of one channel: which ones were silent.
    pub mask: u16,
}

impl Run {
    pub fn rms(&self) -> f32 {
        if self.samples == 0 { 0.0 } else { (self.sum_sq / self.samples as f64).sqrt() as f32 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MeterEvent {
    /// A silent run of all channels ended; `channel` runs are some channels
    /// silent while another sounded.
    RunEnded { run: Run, end: RunEnd, end_pts: u64, channel: bool },
    /// Samples missing between two buffers, or doubled, or going back.
    Gap { at_pts: u64, ns: u64 },
    Overlap { at_pts: u64, ns: u64 },
    Backward { at_pts: u64, ns: u64 },
    /// The buffer's duration field said otherwise than its samples.
    DurationMismatch { declared_ns: u64, samples_ns: u64 },
    Unsupported { why: &'static str },
}

/// The decoded PCM of one channel at a time, before the volume.
#[derive(Default)]
pub struct Meter {
    generation: u64,
    format: Option<PcmFormat>,
    next_pts: Option<u64>,
    run: Option<Run>,
    channel_run: Option<Run>,
    /// The last buffer's levels, and whether it counted as silent.
    pub last: Option<Levels>,
    pub last_silent: bool,
}

impl Meter {
    /// One buffer: `pts` and `declared_ns` (its duration field) as it came,
    /// `discont` its flag. Returns its levels if they could be read.
    #[allow(clippy::too_many_arguments)]
    pub fn buffer(
        &mut self,
        generation: u64,
        format: Result<PcmFormat, &'static str>,
        pts: Option<u64>,
        declared_ns: Option<u64>,
        discont: bool,
        data: &[u8],
        out: &mut Vec<MeterEvent>,
    ) -> Option<Levels> {
        if generation != self.generation {
            self.close(RunEnd::Generation, out);
            self.generation = generation;
            self.format = None;
            self.next_pts = None;
        }
        let format = match format {
            Ok(format) => format,
            Err(why) => {
                // Not readable is not silent: what was running ends here.
                self.close(RunEnd::Format, out);
                self.format = None;
                self.next_pts = None;
                out.push(MeterEvent::Unsupported { why });
                return None;
            }
        };
        if self.format.is_some_and(|f| f != format) {
            self.close(RunEnd::Format, out);
            self.next_pts = None;
        }
        self.format = Some(format);
        let levels = match measure(&format, data) {
            Ok(levels) => levels,
            Err(why) => {
                self.close(RunEnd::Format, out);
                self.next_pts = None;
                out.push(MeterEvent::Unsupported { why });
                return None;
            }
        };
        let samples_ns = format.ns(levels.frames);
        if let Some(declared) = declared_ns
            && declared.abs_diff(samples_ns) > format.tolerance_ns()
        {
            out.push(MeterEvent::DurationMismatch { declared_ns: declared, samples_ns });
        }
        // Where this buffer is against where the last one ended.
        let pts = match (pts, self.next_pts) {
            (Some(pts), Some(next)) => {
                let tolerance = format.tolerance_ns();
                if pts > next + tolerance {
                    out.push(MeterEvent::Gap { at_pts: next, ns: pts - next });
                    self.close(RunEnd::Gap, out);
                } else if pts + tolerance < next {
                    let back = next - pts;
                    let previous = self.run.as_ref().map_or(next, |r| r.start_pts);
                    if pts < previous || back > samples_ns {
                        out.push(MeterEvent::Backward { at_pts: pts, ns: back });
                        self.close(RunEnd::Backward, out);
                    } else {
                        out.push(MeterEvent::Overlap { at_pts: pts, ns: back });
                        self.close(RunEnd::Overlap, out);
                    }
                } else if discont {
                    self.close(RunEnd::Discont, out);
                }
                pts
            }
            (Some(pts), None) => pts,
            // Untimed: it runs on from the last.
            (None, Some(next)) => next,
            (None, None) => {
                self.last = Some(levels);
                return Some(levels);
            }
        };
        self.next_pts = Some(pts + samples_ns);
        self.step(pts, samples_ns, &levels, out);
        self.last = Some(levels);
        Some(levels)
    }

    fn step(&mut self, pts: u64, ns: u64, levels: &Levels, out: &mut Vec<MeterEvent>) {
        let peak = levels.peak_max();
        let zero = levels.zero_frames == levels.frames;
        let silent = if self.run.is_some() { peak < linear(EXIT_DBFS) } else { peak < linear(ENTER_DBFS) };
        self.last_silent = silent;
        let sum_sq: f64 = levels.sum_sq[..levels.channels].iter().sum();
        let samples = levels.frames * levels.channels as u64;
        if silent {
            let run = self.run.get_or_insert(Run {
                kind: Quiet::DigitalZero,
                start_pts: pts,
                silent_ns: 0,
                release_ns: 0,
                release_pts: None,
                frames: 0,
                peak: 0.0,
                sum_sq: 0.0,
                samples: 0,
                zero_frames: 0,
                mask: (1u16 << levels.channels) - 1,
            });
            if !zero {
                run.kind = Quiet::BelowThreshold;
            }
            run.silent_ns += ns;
            run.release_ns = 0;
            run.release_pts = None;
            run.frames += levels.frames;
            run.peak = run.peak.max(peak);
            run.sum_sq += sum_sq;
            run.samples += samples;
            run.zero_frames += levels.zero_frames;
        } else if let Some(run) = self.run.as_mut() {
            run.release_pts.get_or_insert(pts);
            run.release_ns += ns;
            if run.release_ns >= RELEASE_NS {
                let end = run.release_pts.unwrap_or(pts);
                let run = self.run.take().expect("there is one");
                out.push(MeterEvent::RunEnded { run, end: RunEnd::Sound, end_pts: end, channel: false });
            }
        }
        // Some channels silent while another sounds: a run of its own.
        let mut mask = 0u16;
        let loud = (0..levels.channels).any(|c| levels.peak[c] >= linear(EXIT_DBFS));
        if loud {
            for c in 0..levels.channels {
                let limit = if self.channel_run.as_ref().is_some_and(|r| r.mask & (1 << c) != 0) { EXIT_DBFS } else { ENTER_DBFS };
                if levels.peak[c] < linear(limit) {
                    mask |= 1 << c;
                }
            }
        }
        match self.channel_run.as_mut() {
            Some(run) if run.mask == mask => {
                run.silent_ns += ns;
                run.frames += levels.frames;
            }
            _ => {
                if let Some(run) = self.channel_run.take() {
                    out.push(MeterEvent::RunEnded { run, end: RunEnd::Sound, end_pts: pts, channel: true });
                }
                if mask != 0 {
                    self.channel_run = Some(Run {
                        kind: Quiet::BelowThreshold,
                        start_pts: pts,
                        silent_ns: ns,
                        release_ns: 0,
                        release_pts: None,
                        frames: levels.frames,
                        peak: 0.0,
                        sum_sq: 0.0,
                        samples: 0,
                        zero_frames: 0,
                        mask,
                    });
                }
            }
        }
    }

    /// What runs ends, for `end`.
    pub fn close(&mut self, end: RunEnd, out: &mut Vec<MeterEvent>) {
        let end_pts = self.next_pts.unwrap_or(0);
        if let Some(run) = self.run.take() {
            out.push(MeterEvent::RunEnded { run, end, end_pts, channel: false });
        }
        if let Some(run) = self.channel_run.take() {
            out.push(MeterEvent::RunEnded { run, end, end_pts, channel: true });
        }
    }

    /// The silent run going on, if it is already an event's length.
    pub fn ongoing(&self) -> Option<&Run> {
        self.run.as_ref().filter(|r| r.silent_ns >= EVENT_NS)
    }

    pub fn format(&self) -> Option<PcmFormat> {
        self.format
    }
}

// ---------------------------------------------------------------- counters

macro_rules! counters {
    ($($layer:literal: $($field:ident),+;)+) => {
        /// Every layer's counts, cumulative for the process.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct Counters {
            $($(pub $field: u64,)+)+
        }

        impl Counters {
            /// What happened between `before` and this.
            pub fn since(&self, before: &Counters) -> Counters {
                Counters { $($($field: self.$field.saturating_sub(before.$field),)+)+ }
            }

            pub fn json(&self) -> serde_json::Value {
                let mut layers = serde_json::Map::new();
                $(
                    let mut layer = serde_json::Map::new();
                    $(layer.insert(stringify!($field).into(), self.$field.into());)+
                    layers.insert($layer.into(), layer.into());
                )+
                layers.into()
            }
        }
    };
}

counters! {
    "transport": ts_payload_bytes, ts_pes_starts, ts_tei, ts_cc_loss, ts_duplicate, ts_discontinuity, ts_sound_resets,
        ts_arrival_steps;
    "timeline": frames, frame_gaps, frame_overlaps, frame_backward, frame_discont, frames_gated;
    "decoder": dec_in, dec_out_buffers, dec_out_frames, dec_reconfigured;
    "pcm": pcm_buffers, pcm_frames, pcm_zero_frames, pcm_silent_frames, pcm_gaps, pcm_gap_ns, pcm_overlaps, pcm_overlap_ns,
        pcm_backward,
        pcm_unsupported, pcm_duration_mismatch, pcm_channel_silent;
    "sink": sink_buffers, sink_gaps, sink_overlaps, sink_late, sink_behind, sink_discont, alsa_restarts, alsa_xruns,
        sink_reopens;
    "viewer": user_changes, route_changes;
    "picture": video_dropped, plane_full_dropped;
}

// ------------------------------------------------------------------ causes

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    InputTransportDiscontinuity,
    PesTimestampDiscontinuity,
    DecoderOutputSilence,
    RenderStarvation,
    SinkXrun,
    UserMuted,
    RouteChange,
    /// The sink opened again on the same device for the stream's new form
    /// (PCM, or a bitstream passed through), nothing lost on the way.
    StreamFormatChange,
    Unknown,
}

pub const CAUSES: [Cause; 9] = [
    Cause::InputTransportDiscontinuity,
    Cause::PesTimestampDiscontinuity,
    Cause::DecoderOutputSilence,
    Cause::RenderStarvation,
    Cause::SinkXrun,
    Cause::UserMuted,
    Cause::RouteChange,
    Cause::StreamFormatChange,
    Cause::Unknown,
];

impl Cause {
    pub fn name(self) -> &'static str {
        match self {
            Cause::InputTransportDiscontinuity => "input_transport_discontinuity",
            Cause::PesTimestampDiscontinuity => "pes_timestamp_discontinuity",
            Cause::DecoderOutputSilence => "decoder_output_silence",
            Cause::RenderStarvation => "render_starvation",
            Cause::SinkXrun => "sink_xrun",
            Cause::UserMuted => "user_muted",
            Cause::RouteChange => "route_change",
            Cause::StreamFormatChange => "stream_format_change",
            Cause::Unknown => "unknown",
        }
    }

    fn index(self) -> usize {
        CAUSES.iter().position(|c| *c == self).expect("every cause is listed")
    }
}

/// The signed offset in audiobasesink's "Unexpected discontinuity in audio
/// timestamps of +0:00:00.012345678, resyncing", ns.
pub fn discont_ns(message: &str) -> Option<i64> {
    let rest = message.split(" of ").nth(1)?;
    let (sign, rest) = match rest.as_bytes().first()? {
        b'+' => (1, &rest[1..]),
        b'-' => (-1, &rest[1..]),
        _ => (1, rest),
    };
    let time = rest.split(',').next()?.trim();
    let mut parts = time.split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next()?.parse().ok()?;
    let (seconds, nanos) = parts.next()?.split_once('.')?;
    let ns = ((hours * 60 + minutes) * 60 + seconds.parse::<i64>().ok()?) * 1_000_000_000 + nanos.parse::<i64>().ok()?;
    Some(sign * ns)
}

/// What an event was, as seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Decoded samples silent, every channel.
    Silence(Quiet),
    /// Some channels silent while another sounded.
    ChannelSilence,
    /// Samples missing from the decoded PCM's timeline.
    PcmGap,
    /// No decoded samples came at all.
    PcmAbsent,
    /// Sound missing on its way into the sink.
    SinkGap,
    SinkOverlap,
    /// The sink found its samples off their timestamps and resynced.
    SinkResync,
    /// The ALSA device started again.
    DeviceRestart,
    /// The viewer muted, or turned the sound to nothing.
    UserSilenced,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Silence(quiet) => quiet.name(),
            Kind::ChannelSilence => "decoded_pcm_channel_below_threshold",
            Kind::PcmGap => "decoded_pcm_gap",
            Kind::PcmAbsent => "decoded_pcm_absent",
            Kind::SinkGap => "sink_input_gap",
            Kind::SinkOverlap => "sink_input_overlap",
            Kind::SinkResync => "sink_resync",
            Kind::DeviceRestart => "alsa_device_restart",
            Kind::UserSilenced => "user_silenced",
        }
    }
}

/// The cause the window's counts show. First what explains anything after
/// it: the viewer's choice; the route changed under the sound; the device
/// started again because the sink was opened again on it for the stream's
/// new form, and nothing at all was lost (`format_change_clean`); packets
/// of the sound lost before the player had
/// them; the timeline broken without a packet lost. Then by where the event
/// was seen, from what comes before that place only -- the decoded PCM is
/// measured before the sink, so nothing at the sink explains its silence:
///
/// - decoded samples silent, the decoder fed and putting out samples:
///   `DecoderOutputSilence` (what came out was silent; not why);
/// - samples missing or none at all: the device ran dry by itself (XRUN),
///   or the sink got its sound late (the streaming thread held up);
/// - into the sink short with the PCM before it whole: `RenderStarvation`.
///
/// Anything else is `Unknown`, said as such, not guessed.
pub fn classify(kind: Kind, d: &Counters) -> Cause {
    if kind == Kind::UserSilenced {
        return Cause::UserMuted;
    }
    if d.route_changes > 0 {
        return Cause::RouteChange;
    }
    if kind == Kind::DeviceRestart && format_change_clean(d) {
        return Cause::StreamFormatChange;
    }
    // Lost or damaged on the way in, or held back upstream and come late
    // for good (`stamp::STEP_NS`): the sound had nothing to play.
    if d.ts_cc_loss + d.ts_tei + d.ts_arrival_steps > 0 {
        return Cause::InputTransportDiscontinuity;
    }
    if d.frame_gaps + d.frame_overlaps + d.frame_backward + d.frame_discont > 0 {
        return Cause::PesTimestampDiscontinuity;
    }
    let late = d.sink_late + d.sink_behind + d.sink_discont > 0;
    match kind {
        Kind::Silence(_) | Kind::ChannelSilence => {
            if d.dec_in > 0 && d.dec_out_frames > 0 { Cause::DecoderOutputSilence } else { Cause::Unknown }
        }
        Kind::PcmGap | Kind::PcmAbsent | Kind::DeviceRestart if d.alsa_xruns > 0 => Cause::SinkXrun,
        Kind::PcmGap | Kind::PcmAbsent if late => Cause::RenderStarvation,
        Kind::SinkGap | Kind::SinkOverlap | Kind::SinkResync if d.alsa_xruns > 0 => Cause::SinkXrun,
        Kind::SinkGap | Kind::SinkOverlap if d.pcm_gaps + d.pcm_overlaps + d.pcm_backward == 0 && d.dec_out_frames > 0 => {
            Cause::RenderStarvation
        }
        // Resynced while the PCM ran whole to the sample and on time: the
        // sink's own clock against the samples (starved of time, or ahead).
        Kind::SinkResync if d.pcm_gaps + d.pcm_overlaps + d.pcm_backward == 0 && d.dec_out_frames > 0 && late => {
            Cause::RenderStarvation
        }
        _ => Cause::Unknown,
    }
}

/// The device's restart is the stream's change of form, and only that: the
/// sink was opened again for it in the window (`Diag::format_change`, on
/// the device it was on), the restart was ours (no XRUN), and nothing was
/// lost or late -- no PCM gap, overlap or step back, nothing missing or
/// doubled into the sink, nothing late there and no resync. Any of them and
/// the restart is classified as any other, its alarm kept.
fn format_change_clean(d: &Counters) -> bool {
    d.sink_reopens > 0
        && d.alsa_xruns == 0
        && d.pcm_gaps + d.pcm_overlaps + d.pcm_backward == 0
        && d.sink_gaps + d.sink_overlaps == 0
        && d.sink_late + d.sink_behind + d.sink_discont == 0
}

// --------------------------------------------------------------- the whole

/// The sound's context when an event is recorded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Context {
    pub generation: u64,
    pub pid: Option<u16>,
    pub codec: Option<&'static str>,
    pub muted: bool,
    pub gain: f64,
    /// The ALSA device ("hdmi:...", "bluealsa:..."), and its kind.
    pub route: Option<String>,
    pub passthrough: bool,
}

pub fn route_kind(device: Option<&str>) -> &'static str {
    match device {
        None => "none",
        Some(d) if d.starts_with("bluealsa") || d.contains("bluealsa") => "bluetooth",
        Some(d) if d.contains("hdmi") => "hdmi",
        Some(d) if d.contains("es8388") || d.contains("analog") => "analog",
        Some(_) => "other",
    }
}

/// One event, measured, with the window it was in.
#[derive(Debug, Clone, PartialEq)]
pub struct Incident {
    pub kind: Kind,
    pub cause: Cause,
    /// Monotonic, ns (`mediabox_core::timeline::now_ns`).
    pub start_ns: u64,
    pub end_ns: Option<u64>,
    pub pts: Option<u64>,
    pub duration_ns: u64,
    pub delta: Counters,
    pub context: Context,
    /// Over the event's samples, where they were measured.
    pub frames: Option<u64>,
    pub peak_dbfs: Option<f32>,
    pub rms_dbfs: Option<f32>,
    pub zero_ratio: Option<f32>,
    pub mask: Option<u16>,
    pub ended_by: Option<&'static str>,
    /// Whether the viewer is to be told: not while they silenced it.
    pub alarm: bool,
    /// The ALSA device's least queue in the window, ms, if read.
    pub alsa_least_ms: Option<f64>,
}

impl Incident {
    pub fn json(&self) -> serde_json::Value {
        let c = &self.context;
        serde_json::json!({
            "kind": self.kind.name(), "cause": self.cause.name(),
            "start_ns": self.start_ns, "end_ns": self.end_ns, "pts": self.pts,
            "duration_ms": self.duration_ns as f64 / 1e6,
            "generation": c.generation, "pid": c.pid, "codec": c.codec,
            "muted": c.muted, "gain": c.gain, "route": c.route, "route_kind": route_kind(c.route.as_deref()),
            "passthrough": c.passthrough,
            "pcm": { "frames": self.frames, "peak_dbfs": self.peak_dbfs.map(finite), "rms_dbfs": self.rms_dbfs.map(finite),
                     "zero_ratio": self.zero_ratio, "channel_mask": self.mask },
            "ended_by": self.ended_by, "alarm": self.alarm, "alsa_least_ms": self.alsa_least_ms,
            // Decoded samples' timestamps run on from where the samples
            // before ended by this much in the window: ahead of the samples.
            "pcm_timestamp_gap_ms": self.delta.pcm_gap_ns as f64 / 1e6,
            "pcm_timestamp_overlap_ms": self.delta.pcm_overlap_ns as f64 / 1e6,
            "window": self.delta.json(),
        })
    }
}

/// -inf dBFS as JSON can carry it.
fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { -200.0 }
}

/// The sound device opened again for a change of the display (`ao-reload`),
/// counted, and the sink's resyncs and its starved renders around it: the
/// ones while a reload was on its way are the mode change's own (the HDMI
/// stream stopped by the commit); one after it came back is not.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Reloads {
    pub requested: u64,
    pub completed: u64,
    pub failed: u64,
    /// Since when one has been on its way, and when the last one ended.
    pub pending_since: Option<u64>,
    pub done_at: Option<u64>,
    pub during_sink_resync: u64,
    pub during_render_starvation: u64,
    pub after_sink_resync: u64,
    pub after_render_starvation: u64,
    /// From the last one asked to its device open again, ns.
    pub last_ns: u64,
}

impl Reloads {
    pub fn asked(&mut self, now_ns: u64) {
        self.requested += 1;
        self.pending_since.get_or_insert(now_ns);
    }

    /// The swap it joined ended (`ok`: the device open again).
    pub fn ended(&mut self, now_ns: u64, ok: bool) {
        let Some(since) = self.pending_since.take() else { return };
        if ok {
            self.completed += 1;
        } else {
            self.failed += 1;
        }
        self.last_ns = now_ns.saturating_sub(since);
        self.done_at = Some(now_ns);
    }

    fn incident(&mut self, kind: Kind, cause: Cause, seen_ns: u64) {
        let (resync, starved) = (kind == Kind::SinkResync, cause == Cause::RenderStarvation);
        if !resync && !starved {
            return;
        }
        let after = match (self.pending_since, self.done_at) {
            (Some(_), _) => false,
            (None, Some(done)) if seen_ns >= done => true,
            _ => return,
        };
        let (resyncs, starvations) = if after {
            (&mut self.after_sink_resync, &mut self.after_render_starvation)
        } else {
            (&mut self.during_sink_resync, &mut self.during_render_starvation)
        };
        *resyncs += u64::from(resync);
        *starvations += u64::from(starved);
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "requested": self.requested, "completed": self.completed, "failed": self.failed,
            "pending": self.pending_since.is_some(), "last_ms": self.last_ns as f64 / 1e6,
            "during": {"sink_resync": self.during_sink_resync, "render_starvation": self.during_render_starvation},
            "after": {"sink_resync": self.after_sink_resync, "render_starvation": self.after_render_starvation},
        })
    }
}

/// The diagnosis of one process: counters, the meter, the window's history
/// and what came of it.
#[derive(Default)]
pub struct Diag {
    pub c: Counters,
    pub context: Context,
    pub meter: Meter,
    history: VecDeque<(u64, Counters)>,
    /// Where the timeline's next frame is to begin (B), and of what.
    frame_next: Option<(u64, u64)>,
    /// When decoded samples last came, monotonic ns; since when none have.
    last_pcm_ns: Option<u64>,
    absent_since: Option<u64>,
    /// Whether the last report's tick found the sound playing.
    playing: bool,
    /// Since when the viewer has had it silenced.
    silenced_since: Option<u64>,
    pub by_cause: [u64; 9],
    /// Every sink gap and overlap, whatever its length, by cause: what the
    /// player added is what is not the input's.
    pub sink_by_cause: [u64; 9],
    pub incidents: u64,
    /// Every incident by its kind (`Kind::name`): a sink resync is counted
    /// as one, not only as its cause.
    pub by_kind: std::collections::BTreeMap<&'static str, u64>,
    /// The sound device opened again for a change of the display (`ao-reload`).
    pub reload: Reloads,
    pub last: Option<Incident>,
    alsa_least_ms: Option<f64>,
    /// The largest PCM gap or overlap, ns: how far off the timestamps were.
    pub pcm_off_max_ns: u64,
    /// Incidents made where they could not be handed back (`new_channel`),
    /// for the next `tick`.
    pending: Vec<Incident>,
}

impl Diag {
    /// The counts in the window `[start - WINDOW_BEFORE_NS, now]`.
    fn window(&self, start_ns: u64) -> Counters {
        let from = start_ns.saturating_sub(WINDOW_BEFORE_NS);
        let before = self
            .history
            .iter()
            .rev()
            .find(|(at, _)| *at <= from)
            .or(self.history.front())
            .map_or(Counters::default(), |(_, c)| *c);
        self.c.since(&before)
    }

    /// A report's tick: a snapshot for windows, and samples that stopped.
    /// Returns what ended or started being an event.
    pub fn tick(&mut self, now_ns: u64, playing: bool, alsa_least_ms: Option<f64>) -> Vec<Incident> {
        if let Some(least) = alsa_least_ms {
            self.alsa_least_ms = Some(self.alsa_least_ms.map_or(least, |l: f64| l.min(least)));
        }
        self.history.push_back((now_ns, self.c));
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        let mut out = std::mem::take(&mut self.pending);
        // No samples: none while the sound plays, past the event's length.
        self.playing = playing;
        let expected = playing && !self.context.passthrough && self.last_pcm_ns.is_some();
        match (self.last_pcm_ns, self.absent_since) {
            (Some(last), None) if expected && now_ns.saturating_sub(last) >= EVENT_NS => {
                self.absent_since = Some(last);
                let mut events = Vec::new();
                self.meter.close(RunEnd::Absent, &mut events);
                out.extend(self.meter_events(events, now_ns));
            }
            (_, Some(_)) if !expected => self.absent_since = None,
            _ => {}
        }
        out
    }

    /// Another channel: what was waited for was the last one's.
    pub fn new_channel(&mut self, generation: u64, now_ns: u64) {
        let mut events = Vec::new();
        self.meter.close(RunEnd::Generation, &mut events);
        // A run the channel change cut is said, as it was.
        let ended = self.meter_events(events, now_ns);
        self.pending.extend(ended);
        self.context.generation = generation;
        self.last_pcm_ns = None;
        self.absent_since = None;
        self.frame_next = None;
    }

    /// The decoded samples began again after none came: for an event's
    /// length or more, whether a report's tick saw them missing or not (a
    /// tick comes every 250 ms; a stall of 400 ms is missing at one for
    /// only 200 of them: 4 of 5 stalls were seen so, 2026-11-04).
    fn samples_back(&mut self, now_ns: u64) -> Vec<Incident> {
        let mut out = Vec::new();
        let since = match self.absent_since.take() {
            Some(since) => since,
            None => match self.last_pcm_ns {
                Some(last) if self.playing && !self.context.passthrough && now_ns.saturating_sub(last) >= EVENT_NS => {
                    let mut events = Vec::new();
                    self.meter.close(RunEnd::Absent, &mut events);
                    out.extend(self.meter_events(events, now_ns));
                    last
                }
                _ => return out,
            },
        };
        out.push(self.record(Kind::PcmAbsent, since, Some(now_ns), now_ns - since, None));
        out
    }

    fn record(&mut self, kind: Kind, start_ns: u64, end_ns: Option<u64>, duration_ns: u64, run: Option<(&Run, &'static str)>) -> Incident {
        let delta = self.window(start_ns);
        let cause = classify(kind, &delta);
        let silenced = self.context.muted || self.context.gain <= 0.0;
        let incident = Incident {
            kind,
            cause,
            start_ns,
            end_ns,
            pts: run.map(|(r, _)| r.start_pts),
            duration_ns,
            delta,
            context: self.context.clone(),
            frames: run.map(|(r, _)| r.frames),
            peak_dbfs: run.map(|(r, _)| dbfs(r.peak)),
            rms_dbfs: run.map(|(r, _)| dbfs(r.rms())),
            zero_ratio: run.filter(|(r, _)| r.frames > 0).map(|(r, _)| r.zero_frames as f32 / r.frames as f32),
            mask: run.filter(|(r, _)| r.mask != 0).map(|(r, _)| r.mask),
            ended_by: run.map(|(_, by)| by),
            alarm: !silenced && cause != Cause::UserMuted && cause != Cause::StreamFormatChange,
            alsa_least_ms: self.alsa_least_ms.take(),
        };
        self.by_cause[cause.index()] += 1;
        *self.by_kind.entry(kind.name()).or_default() += 1;
        // When it was seen: a resync's start is its offset back, which
        // can be far before the reload it came after.
        self.reload.incident(kind, cause, end_ns.unwrap_or(start_ns));
        self.incidents += 1;
        self.last = Some(incident.clone());
        incident
    }

    fn meter_events(&mut self, events: Vec<MeterEvent>, now_ns: u64) -> Vec<Incident> {
        let mut out = Vec::new();
        for event in events {
            match event {
                MeterEvent::RunEnded { run, end, channel, .. } => {
                    if channel {
                        self.c.pcm_channel_silent += 1;
                    }
                    if run.silent_ns >= EVENT_NS {
                        let kind = if channel { Kind::ChannelSilence } else { Kind::Silence(run.kind) };
                        let start = now_ns.saturating_sub(run.silent_ns);
                        out.push(self.record(kind, start, Some(now_ns), run.silent_ns, Some((&run, end.name()))));
                    }
                }
                MeterEvent::Gap { ns, .. } => {
                    self.c.pcm_gaps += 1;
                    self.c.pcm_gap_ns += ns;
                    self.pcm_off_max_ns = self.pcm_off_max_ns.max(ns);
                    if ns >= EVENT_NS {
                        out.push(self.record(Kind::PcmGap, now_ns.saturating_sub(ns), Some(now_ns), ns, None));
                    }
                }
                MeterEvent::Overlap { ns, .. } => {
                    self.c.pcm_overlaps += 1;
                    self.c.pcm_overlap_ns += ns;
                    self.pcm_off_max_ns = self.pcm_off_max_ns.max(ns);
                }
                MeterEvent::Backward { .. } => self.c.pcm_backward += 1,
                MeterEvent::DurationMismatch { .. } => self.c.pcm_duration_mismatch += 1,
                MeterEvent::Unsupported { .. } => self.c.pcm_unsupported += 1,
            }
        }
        out
    }

    /// D: a buffer of decoded PCM before the volume.
    #[allow(clippy::too_many_arguments)]
    pub fn pcm(
        &mut self,
        now_ns: u64,
        generation: u64,
        format: Result<PcmFormat, &'static str>,
        pts: Option<u64>,
        declared_ns: Option<u64>,
        discont: bool,
        data: &[u8],
    ) -> Vec<Incident> {
        if generation != self.context.generation {
            self.new_channel(generation, now_ns);
        }
        let mut out: Vec<Incident> = std::mem::take(&mut self.pending);
        out.extend(self.samples_back(now_ns));
        self.last_pcm_ns = Some(now_ns);
        let mut events = Vec::new();
        if let Some(levels) = self.meter.buffer(generation, format, pts, declared_ns, discont, data, &mut events) {
            self.c.pcm_buffers += 1;
            self.c.pcm_frames += levels.frames;
            self.c.pcm_zero_frames += levels.zero_frames;
            if self.meter.last_silent {
                self.c.pcm_silent_frames += levels.frames;
            }
        }
        out.extend(self.meter_events(events, now_ns));
        out
    }

    /// B: a frame out of the parser, `discont` flagged.
    pub fn frame(&mut self, generation: u64, pts: Option<u64>, duration: Option<u64>, discont: bool) {
        self.c.frames += 1;
        if discont {
            self.c.frame_discont += 1;
        }
        let Some(pts) = pts else {
            self.frame_next = None;
            return;
        };
        if let Some((g, next)) = self.frame_next
            && g == generation
            && !discont
        {
            // The parsers time frames to the sample; 1 ms is past any rounding.
            const SLACK: u64 = 1_000_000;
            if pts > next + SLACK {
                self.c.frame_gaps += 1;
            } else if pts + SLACK < next {
                if next - pts > duration.unwrap_or(0) { self.c.frame_backward += 1 } else { self.c.frame_overlaps += 1 }
            }
        }
        self.frame_next = duration.map(|d| (generation, pts + d));
    }

    /// E: a buffer into the sink was `off` ns from where the last ended.
    /// Every one is classified; one of an event's length is recorded.
    pub fn sink_off(&mut self, now_ns: u64, off: i64) -> Option<Incident> {
        let (kind, ns) = if off > 0 {
            self.c.sink_gaps += 1;
            (Kind::SinkGap, off as u64)
        } else {
            self.c.sink_overlaps += 1;
            (Kind::SinkOverlap, off.unsigned_abs())
        };
        let cause = classify(kind, &self.window(now_ns.saturating_sub(ns)));
        self.sink_by_cause[cause.index()] += 1;
        (ns >= EVENT_NS).then(|| self.record(kind, now_ns.saturating_sub(ns), Some(now_ns), ns, None))
    }

    /// E: audiobasesink resynced, its samples `off` ns from where their
    /// timestamps put them (`discont_ns`; None if it did not say).
    pub fn sink_discont(&mut self, now_ns: u64, off: Option<i64>) -> Option<Incident> {
        self.c.sink_discont += 1;
        let ns = off.map_or(0, i64::unsigned_abs);
        let cause = classify(Kind::SinkResync, &self.window(now_ns.saturating_sub(ns)));
        self.sink_by_cause[cause.index()] += 1;
        (ns >= EVENT_NS).then(|| self.record(Kind::SinkResync, now_ns.saturating_sub(ns), Some(now_ns), ns, None))
    }

    /// E: the ALSA device started again; `ours` if a reopen of ours was
    /// within the second before.
    pub fn device_restart(&mut self, now_ns: u64, ours: bool) -> Incident {
        self.c.alsa_restarts += 1;
        if !ours {
            self.c.alsa_xruns += 1;
        }
        self.record(Kind::DeviceRestart, now_ns, Some(now_ns), 0, None)
    }

    /// F: the viewer's volume. Silenced and back: an event of its own.
    pub fn user(&mut self, now_ns: u64, gain: f64, muted: bool) -> Option<Incident> {
        if gain == self.context.gain && muted == self.context.muted {
            return None;
        }
        self.c.user_changes += 1;
        let silenced = muted || gain <= 0.0;
        let was = std::mem::replace(&mut self.silenced_since, silenced.then_some(now_ns));
        let back = match was {
            Some(since) if !silenced => Some(since),
            Some(since) => {
                self.silenced_since = Some(since);
                None
            }
            None => None,
        };
        self.context.gain = gain;
        self.context.muted = muted;
        let since = back?;
        let ns = now_ns - since;
        (ns >= EVENT_NS).then(|| self.record(Kind::UserSilenced, since, Some(now_ns), ns, None))
    }

    /// E: the sink opened again for the stream's new form (PCM or a
    /// bitstream) on `device`. On the device it was on, a reopen of the
    /// sink, not a route; on another, the route changed.
    pub fn format_change(&mut self, device: Option<String>) {
        if device != self.context.route {
            self.route(device);
        } else {
            self.c.sink_reopens += 1;
        }
    }

    /// F: the sound goes to another device, or the same is opened again.
    pub fn route(&mut self, device: Option<String>) {
        self.c.route_changes += 1;
        self.context.route = device;
    }

    /// The silence going on, for the report: what, since when, how long.
    pub fn ongoing_json(&self, now_ns: u64) -> serde_json::Value {
        if let Some(since) = self.absent_since {
            return serde_json::json!({ "kind": Kind::PcmAbsent.name(), "ms": now_ns.saturating_sub(since) as f64 / 1e6 });
        }
        match self.meter.ongoing() {
            Some(run) => serde_json::json!({
                "kind": run.kind.name(), "ms": run.silent_ns as f64 / 1e6, "from_pts": run.start_pts,
                "peak_dbfs": finite(dbfs(run.peak)), "rms_dbfs": finite(dbfs(run.rms())),
                "alarm": !(self.context.muted || self.context.gain <= 0.0),
            }),
            None => serde_json::Value::Null,
        }
    }

    pub fn json(&self, now_ns: u64) -> serde_json::Value {
        let by = |counts: &[u64; 9]| {
            CAUSES.iter().map(|c| (c.name().to_string(), serde_json::Value::from(counts[c.index()]))).collect::<serde_json::Map<_, _>>()
        };
        let levels = self.meter.last.map(|l| {
            (0..l.channels)
                .map(|c| serde_json::json!({ "peak_dbfs": finite(dbfs(l.peak[c])), "rms_dbfs": finite(dbfs(l.rms(c))) }))
                .collect::<Vec<_>>()
        });
        serde_json::json!({
            "measured_at": "volume sink pad: decoded, converted, resampled, before the viewer's volume and mute",
            "format": self.meter.format().map(|f| format!("{:?} {} Hz {} ch {}", f.sample, f.rate, f.channels, if f.interleaved { "interleaved" } else { "planar" })),
            "levels": levels,
            "ongoing": self.ongoing_json(now_ns),
            "incidents": self.incidents,
            "by_cause": by(&self.by_cause),
            "by_kind": self.by_kind,
            "reload": self.reload.json(),
            "sink_off_by_cause": by(&self.sink_by_cause),
            "last": self.last.as_ref().map(Incident::json),
            "layers": self.c.json(),
            "route_kind": route_kind(self.context.route.as_deref()),
            "pcm_off_max_us": self.pcm_off_max_ns / 1000,
            "passthrough": self.context.passthrough,
        })
    }
}

#[cfg(test)]
#[path = "sound_diag_tests.rs"]
mod tests;
