//! A dump of `evidence` read back and judged: `mediabox-live
//! --evidence-classify <dump>`. Diagnosis only; nothing here runs in a
//! playing player.
//!
//! The transport, by indicators of ETSI TR 101 290 V1.4.1 §5.2.1–5.2.2 as
//! far as the packets the dump keeps allow: the sync byte of each captured
//! packet, transport_error_indicator, continuity of the sound's PID, the
//! PCR's repetition against the standard's 100 ms (and, apart, a stricter
//! 40 ms of this project's own), the sound PES's PTS repetition (700 ms)
//! and step. PAT and PMT packets are counted, nothing more: their
//! repetition, section CRC and version are not measured here, nor
//! PCR_accuracy, nor TS_sync_loss (the whole stream is not kept); the
//! output says so rather than a pass.
//!
//! The sound, decoded again by a decoder that is not the player's (ffmpeg's
//! `mp2`, libavcodec; the player's is libmpg123), from the same packets.
//! Both decodings are put on the running time by PTS -- each independent
//! frame by its PES's PTS through the reader's own `pts_on_clock` of it
//! (`pes.jsonl`), the player's by its buffers' PTS -- never by the wall
//! clock, and measured over the event's PTS window alike.
//!
//! What a silence was (`classify_silence`), and what a step of the sound
//! clock against the pipeline's was (`classify_clock`), from what was
//! measured -- `unclassified` with the reason where a layer's evidence is
//! missing.

use crate::sound_diag::{self, EXIT_DBFS, Levels, MAX_CHANNELS, PcmFormat, Sample};
use std::path::Path;

// ---------------------------------------------------------------- levels

/// Timed PCM: its first frame's running time, ns, and its bytes.
#[derive(Debug, Clone, Copy)]
pub struct Segment<'a> {
    pub pts: u64,
    pub data: &'a [u8],
}

fn empty_levels(channels: usize) -> Levels {
    Levels { frames: 0, channels, peak: [0.0; MAX_CHANNELS], sum_sq: [0.0; MAX_CHANNELS], zero_frames: 0 }
}

fn add(total: &mut Levels, l: &Levels) {
    total.frames += l.frames;
    total.zero_frames += l.zero_frames;
    for c in 0..total.channels {
        total.peak[c] = total.peak[c].max(l.peak[c]);
        total.sum_sq[c] += l.sum_sq[c];
    }
}

/// The frames of `segments` whose time is in `[from, to)`, measured; and
/// what share of the window they cover.
pub fn level_in(segments: &[Segment], format: &PcmFormat, from: u64, to: u64) -> (Levels, f64) {
    let fb = format.channels
        * match format.sample {
            Sample::S16 => 2,
            Sample::S32 | Sample::F32 => 4,
        };
    let mut total = empty_levels(format.channels);
    let rate = u128::from(format.rate);
    for s in segments {
        let frames = (s.data.len() / fb) as u64;
        let end = s.pts + format.ns(frames);
        if end <= from || s.pts >= to {
            continue;
        }
        let i0 = if from > s.pts { (u128::from(from - s.pts) * rate).div_ceil(1_000_000_000) as u64 } else { 0 };
        let i1 = if to < end { (u128::from(to - s.pts) * rate / 1_000_000_000) as u64 } else { frames };
        if i1 <= i0 {
            continue;
        }
        if let Ok(l) = sound_diag::measure(format, &s.data[i0 as usize * fb..i1 as usize * fb]) {
            add(&mut total, &l);
        }
    }
    let want = (u128::from(to.saturating_sub(from)) * rate / 1_000_000_000) as f64;
    let coverage = if want > 0.0 { total.frames as f64 / want } else { 0.0 };
    (total, coverage)
}

/// Silent as the player's meter holds a run silent: every sample's peak
/// under `EXIT_DBFS`, the meter's own threshold -- not a BS.1770 or R128
/// loudness, nor a silence limit any standard sets.
pub fn silent(l: &Levels) -> bool {
    l.frames > 0 && l.peak_max() < 10f32.powf(EXIT_DBFS / 20.0)
}

fn level_json(l: &Levels, coverage: f64) -> serde_json::Value {
    let samples = l.frames * l.channels as u64;
    let rms = if samples == 0 { 0.0 } else { (l.sum_sq[..l.channels].iter().sum::<f64>() / samples as f64).sqrt() as f32 };
    serde_json::json!({
        "frames": l.frames, "coverage": coverage,
        "peak_dbfs": finite(sound_diag::dbfs(l.peak_max())), "rms_dbfs": finite(sound_diag::dbfs(rms)),
        "zero_ratio": if l.frames > 0 { l.zero_frames as f64 / l.frames as f64 } else { 0.0 },
        "silent": silent(l),
    })
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { -200.0 }
}

// --------------------------------------------------------------- silence

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceClass {
    SourceContentSilence,
    InputAudioBitstreamError,
    ProductDecodeSilence,
    ProductRenderSilence,
    UserSilenced,
    Unclassified(&'static str),
}

impl SilenceClass {
    pub fn name(self) -> &'static str {
        match self {
            SilenceClass::SourceContentSilence => "source_content_silence",
            SilenceClass::InputAudioBitstreamError => "input_audio_bitstream_error",
            SilenceClass::ProductDecodeSilence => "product_decode_silence",
            SilenceClass::ProductRenderSilence => "product_render_silence",
            SilenceClass::UserSilenced => "user_silenced",
            SilenceClass::Unclassified(_) => "unclassified",
        }
    }
}

/// Least share of the event's window a decoding must cover to be judged.
pub const COVERAGE: f64 = 0.95;

#[derive(Debug, Clone, Default)]
pub struct SilenceFacts {
    /// Muted or the volume at nothing over the event.
    pub user_silenced: bool,
    /// Transport or PES errors on the sound in the event's window
    /// (continuity, TEI, sync, a PES PTS off its step).
    pub input_errors: u64,
    /// Frames the independent decoder refused or concealed.
    pub decoder_errors: u64,
    /// Over the event's PTS window: the independent decoding, the player's.
    pub independent: Option<(Levels, f64)>,
    pub product: Option<(Levels, f64)>,
    /// Sound lost on its way into the sink in the window, ns (gaps between
    /// buffers, or decoded frames that never reached it).
    pub sink_lost_ns: u64,
}

pub fn classify_silence(f: &SilenceFacts) -> SilenceClass {
    if f.user_silenced {
        return SilenceClass::UserSilenced;
    }
    if f.input_errors > 0 || f.decoder_errors > 0 {
        return SilenceClass::InputAudioBitstreamError;
    }
    let Some((independent, ic)) = &f.independent else { return SilenceClass::Unclassified("no independent decoding") };
    let Some((product, pc)) = &f.product else { return SilenceClass::Unclassified("no product pcm") };
    if *ic < COVERAGE {
        return SilenceClass::Unclassified("independent decoding does not cover the window");
    }
    if *pc < COVERAGE {
        return SilenceClass::Unclassified("product pcm does not cover the window");
    }
    match (silent(independent), silent(product)) {
        (true, true) => SilenceClass::SourceContentSilence,
        (false, true) => SilenceClass::ProductDecodeSilence,
        (_, false) if f.sink_lost_ns >= sound_diag::EVENT_NS => SilenceClass::ProductRenderSilence,
        (true, false) => SilenceClass::Unclassified("independent silent, product sounding"),
        (false, false) => SilenceClass::Unclassified("no layer silent"),
    }
}

// ----------------------------------------------------------------- clock

/// One clock sample as dumped (`evidence::ClockSample`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClockRow {
    pub at: u64,
    pub after: u64,
    pub late: Option<i64>,
    pub internal: Option<u64>,
    pub calibration: Option<[u64; 4]>,
    pub running: bool,
    pub trigger: Option<u64>,
    pub tstamp: Option<u64>,
    /// When the status was read (the middle of its read), for a device
    /// that gives no timestamp of its own (alsasink leaves them off: 0).
    pub read_at: Option<u64>,
    pub hw_ptr: Option<u64>,
    pub appl_ptr: Option<u64>,
    /// Frames into the sink up to `at` (`sink.jsonl`).
    pub fed: u64,
}

/// Where the device began playing in a window, each on its own: the
/// trigger time ALSA gives, the first sample RUNNING, the first with a
/// frame played, the first buffer into the sink (its time and PTS) and the
/// first counted as fed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Start {
    pub trigger_ns: Option<u64>,
    pub first_running_at: Option<u64>,
    pub first_hw_at: Option<u64>,
    pub first_sink_at: Option<u64>,
    pub first_sink_pts: Option<u64>,
    pub first_fed_at: Option<u64>,
}

impl Start {
    pub fn of(rows: &[ClockRow], sink: &[(u64, u64)]) -> Self {
        Self {
            trigger_ns: rows.iter().find(|r| r.running).and_then(|r| r.trigger),
            first_running_at: rows.iter().find(|r| r.running).map(|r| r.at),
            first_hw_at: rows.iter().find(|r| r.running && r.hw_ptr.is_some_and(|h| h > 0)).map(|r| r.at),
            first_sink_at: sink.first().map(|s| s.0),
            first_sink_pts: sink.first().map(|s| s.1),
            first_fed_at: rows.iter().find(|r| r.fed > 0).map(|r| r.at),
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "alsa_trigger_ns": self.trigger_ns, "first_running_at": self.first_running_at, "first_hw_ptr_at": self.first_hw_at,
            "first_sink_at": self.first_sink_at, "first_sink_pts": self.first_sink_pts, "first_fed_at": self.first_fed_at,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClockClass {
    MeasurementArtifact(&'static str),
    StartupMeasurementConvergence(&'static str),
    ProductAudioTimingStep(&'static str),
    Unclassified(&'static str),
}

impl ClockClass {
    pub fn name(self) -> &'static str {
        match self {
            ClockClass::MeasurementArtifact(_) => "measurement_artifact",
            ClockClass::StartupMeasurementConvergence(_) => "startup_measurement_convergence",
            ClockClass::ProductAudioTimingStep(_) => "product_audio_timing_step",
            ClockClass::Unclassified(_) => "unclassified",
        }
    }

    pub fn why(self) -> &'static str {
        match self {
            ClockClass::MeasurementArtifact(w)
            | ClockClass::StartupMeasurementConvergence(w)
            | ClockClass::ProductAudioTimingStep(w)
            | ClockClass::Unclassified(w) => w,
        }
    }
}

/// A step of the presentation larger than this is the product's.
pub const PRESENT_STEP_NS: f64 = 2_000_000.0;
/// A step this soon after the device began playing is in its start.
pub const STARTUP_NS: u64 = 3_000_000_000;
/// Samples (10 ms each) the device must have played on each side of a
/// step for the feed to be judged.
pub const LEAST_SIDE: usize = 10;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClockFacts {
    /// The step: its sample's offset from the median of the second before,
    /// and how long its two clock readings were apart.
    pub off_ns: i64,
    pub read_ns: u64,
    /// Samples (10 ms each) until the reading was back within 1 ms of the
    /// median before; None: not back in the window.
    pub back_after: Option<usize>,
    /// The device's played frames against its own timestamps over the
    /// window: the largest residual off a straight line, ns.
    pub hw_residual_ns: Option<f64>,
    /// Frames written to the device less frames fed into the sink: how
    /// far its mean moved over the step, ns.
    pub feed_shift_ns: Option<f64>,
    pub device_restarted: bool,
    pub calibration_changed: bool,
    /// How long after the device began playing (first RUNNING with a
    /// frame played) the step came; None: it did not in the window.
    pub since_start_ns: Option<u64>,
    /// Calibrations the sound clock went through from the start to a
    /// second after the step.
    pub calibration_changes: usize,
    /// The sink's buffers against the PTS plan around the step: the
    /// largest jump between one's end and the next's start, ns.
    pub sink_jump_ns: Option<f64>,
    /// Where audiobasesink put each buffer in its ring against the
    /// buffer's running time (`placement_steps`), from the device's start
    /// to 1.5 s after the step: the largest step between two buffers and
    /// the whole movement, ns. None: no placement log with the dump.
    pub placement_step_ns: Option<f64>,
    pub placement_moved_ns: Option<f64>,
}

/// One buffer as audiobasesink rendered it (its DEBUG log,
/// gstaudiobasesink.c 1.26: "running: start <t>" then "rendering at
/// <sample_offset> <samples>/<out_samples>"): its running time and the
/// ring sample it was put at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub running_ns: u64,
    pub sample_offset: u64,
    pub samples: u64,
    pub out_samples: u64,
}

fn clock_time(t: &str) -> Option<u64> {
    let mut parts = t.split(':');
    let (h, m, rest) = (parts.next()?.parse::<u64>().ok()?, parts.next()?.parse::<u64>().ok()?, parts.next()?);
    let (sec, ns) = rest.split_once('.')?;
    Some(((h * 60 + m) * 60 + sec.parse::<u64>().ok()?) * 1_000_000_000 + format!("{ns:0<9}").get(..9)?.parse::<u64>().ok()?)
}

/// The placements of a GST_DEBUG=audiobasesink:5 log, in order.
pub fn placements(log: &str) -> Vec<Placement> {
    let mut out = Vec::new();
    let mut running = None;
    for line in log.lines() {
        if let Some(rest) = line.split("running: start ").nth(1) {
            running = rest.split_whitespace().next().and_then(clock_time);
        } else if let Some(rest) = line.split("rendering at ").nth(1) {
            let mut f = rest.split_whitespace();
            let offset = f.next().and_then(|v| v.parse::<u64>().ok());
            let counts = f.next().and_then(|v| v.split_once('/')).and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)));
            if let (Some(running_ns), Some(sample_offset), Some((samples, out_samples))) = (running.take(), offset, counts) {
                out.push(Placement { running_ns, sample_offset, samples, out_samples });
            }
        }
    }
    out
}

/// Each buffer's place in the ring as time less its running time: where
/// the device plays it against its PTS plan, up to a constant (the ring is
/// played in a line: `hw_residual_ns`). Its largest step between two
/// buffers, and how far it moved in all, ns.
pub fn placement_steps(p: &[Placement], rate: f64) -> Option<(f64, f64)> {
    let d: Vec<f64> = p.iter().map(|b| b.sample_offset as f64 / rate * 1e9 - b.running_ns as f64).collect();
    if d.len() < 2 {
        return None;
    }
    let step = d.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
    let (lo, hi) = d.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    Some((step, hi - lo))
}

pub fn classify_clock(f: &ClockFacts) -> ClockClass {
    if f.device_restarted {
        return ClockClass::ProductAudioTimingStep("the device started again (XRUN or reopen) after it began");
    }
    let Some(hw) = f.hw_residual_ns else { return ClockClass::Unclassified("no device timestamps") };
    if hw > PRESENT_STEP_NS {
        return ClockClass::ProductAudioTimingStep("the device's playout stepped against its timestamps");
    }
    if f.sink_jump_ns.is_some_and(|j| j > PRESENT_STEP_NS) {
        return ClockClass::ProductAudioTimingStep("the sink's samples jumped on their PTS plan");
    }
    if f.placement_step_ns.is_some_and(|s| s > PRESENT_STEP_NS) {
        return ClockClass::ProductAudioTimingStep("audiobasesink placed a buffer off its PTS plan by more than 2 ms");
    }
    // At a start the sink's ring is filling: frames written to the device
    // less frames fed moves with its fill, not only with samples gained or
    // lost, and does not decide. Where each buffer was put in the ring
    // against its running time does.
    if f.since_start_ns.is_some_and(|s| s <= STARTUP_NS) {
        return match f.placement_step_ns {
            None => ClockClass::Unclassified("a step at the start needs the sink's placement of its buffers"),
            Some(_) if f.calibration_changes >= 2 => {
                ClockClass::StartupMeasurementConvergence("the sound clock's calibration converging, the playout in a line, no buffer placed 2 ms off")
            }
            Some(_) => ClockClass::Unclassified("a step at the start with the calibration still"),
        };
    }
    let Some(feed) = f.feed_shift_ns else { return ClockClass::Unclassified("too little of the device playing on a side of the step") };
    if feed.abs() > PRESENT_STEP_NS {
        return ClockClass::ProductAudioTimingStep("samples written to the device moved against the feed");
    }
    // The playout went on in a line and its samples were the feed's: the
    // step is in the reading.
    if f.read_ns as f64 >= f.off_ns.unsigned_abs() as f64 * 0.5 {
        return ClockClass::MeasurementArtifact("the two clocks were read that far apart");
    }
    if f.back_after.is_some_and(|n| n <= 3) {
        return ClockClass::MeasurementArtifact("one reading off, the next back, the playout in a line");
    }
    if f.calibration_changed {
        return ClockClass::MeasurementArtifact("the sound clock's calibration moved, the playout did not");
    }
    ClockClass::Unclassified("the reading stayed off with nothing to show for it")
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// The facts of the clock sample at `step` in `rows`; `sink`: each buffer
/// into the sink (arrival, PTS, duration).
pub fn clock_facts(rows: &[ClockRow], step: usize, rate: f64, sink: &[(u64, u64, u64)], placed: &[Placement]) -> ClockFacts {
    let at = rows[step].at;
    // Only what the device played counts: from its first sample RUNNING
    // with a frame played. Before it, its status is that of a device being
    // set up, not one that stopped.
    let start = rows.iter().position(|r| r.running && r.hw_ptr.is_some_and(|h| h > 0));
    let playing = |r: &&ClockRow| start.is_some_and(|s| r.at >= rows[s].at);
    let before: Vec<f64> = rows.iter().filter(|r| r.at < at && at - r.at <= 1_000_000_000).filter_map(|r| r.late.map(|l| l as f64)).collect();
    let base = median(before.clone()).unwrap_or(0.0);
    let off = rows[step].late.map_or(0.0, |l| l as f64 - base);
    let back_after = rows[step + 1..].iter().position(|r| r.late.is_some_and(|l| (l as f64 - base).abs() <= 1_000_000.0)).map(|n| n + 1);
    // The device's frames against its timestamps -- or, where it keeps
    // none, the monotonic time its status was read at: a line by least
    // squares.
    let points: Vec<(f64, f64)> =
        rows.iter().filter(playing).filter(|r| r.running).filter_map(|r| Some((r.tstamp.filter(|t| *t > 0).or(r.read_at)? as f64, r.hw_ptr? as f64))).collect();
    let hw_residual_ns = (points.len() >= 20).then(|| {
        let n = points.len() as f64;
        let (t0, p0) = points[0];
        let (mt, mp) = points.iter().fold((0.0, 0.0), |(a, b), (t, p)| (a + (t - t0) / n, b + (p - p0) / n));
        let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(a, b), (t, p)| (a + ((t - t0) - mt) * ((p - p0) - mp), b + ((t - t0) - mt).powi(2)));
        let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        // Residual in frames, as time at the device's rate.
        points.iter().map(|(t, p)| (((p - p0) - mp) - slope * ((t - t0) - mt)).abs()).fold(0.0, f64::max) / rate * 1e9
    });
    // Written less fed runs in a sawtooth (segments of 10 ms against
    // buffers of 24: a period of 120 ms, steps of 96 frames), so its median
    // and its extremes move by a step with nothing moved. Its mean over
    // the 1.3 s each side (ten periods and more) moves with samples gained
    // or lost.
    let mean = |lo: i64, hi: i64| {
        let v: Vec<f64> = rows
            .iter()
            .filter(playing)
            .filter(|r| {
                let d = r.at as i64 - at as i64;
                d >= lo && d <= hi
            })
            .filter_map(|r| Some(r.appl_ptr? as f64 - r.fed as f64))
            .collect();
        (v.len() >= LEAST_SIDE).then(|| v.iter().sum::<f64>() / v.len() as f64)
    };
    let feed_shift_ns = match (mean(-1_500_000_000, -200_000_000), mean(200_000_000, 1_500_000_000)) {
        (Some(a), Some(b)) => Some((b - a) / rate * 1e9),
        _ => None,
    };
    let after_start: Vec<&ClockRow> = rows.iter().filter(playing).collect();
    let triggers: Vec<u64> = after_start.iter().filter_map(|r| r.trigger).collect();
    let device_restarted = triggers.windows(2).any(|w| w[0] != w[1]) || after_start.iter().any(|r| !r.running);
    let calibration_changed = step > 0 && rows[step].calibration != rows[step - 1].calibration;
    // Since the device began: by ALSA's own trigger time, or, without it,
    // a start seen in the window (a sample not playing before the first
    // that plays). A device playing from the window's first sample began
    // before it: no start here.
    let began = rows.iter().find(|r| r.running).and_then(|r| r.trigger).or_else(|| start.filter(|s| *s > 0).map(|s| rows[s].at));
    let since_start_ns = began.filter(|b| *b <= at).map(|b| at - b);
    let calibration_changes =
        after_start.iter().filter(|r| r.at <= at + 1_000_000_000).collect::<Vec<_>>().windows(2).filter(|w| w[0].calibration != w[1].calibration).count();
    let near: Vec<&(u64, u64, u64)> = sink.iter().filter(|(t, _, _)| t.abs_diff(at) <= 1_500_000_000).collect();
    let sink_jump_ns = (near.len() >= 2).then(|| near.windows(2).map(|w| (w[1].1 as f64 - (w[0].1 + w[0].2) as f64).abs()).fold(0.0, f64::max));
    ClockFacts {
        off_ns: off as i64,
        read_ns: rows[step].after.saturating_sub(rows[step].at),
        back_after,
        hw_residual_ns,
        feed_shift_ns,
        device_restarted,
        calibration_changed,
        since_start_ns,
        calibration_changes,
        sink_jump_ns,
        placement_step_ns: placement_steps(placed, rate).map(|p| p.0),
        placement_moved_ns: placement_steps(placed, rate).map(|p| p.1),
    }
}

// ------------------------------------------------------------- transport

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TsReport {
    pub packets: u64,
    pub sync_errors: u64,
    pub tei: u64,
    pub cc_errors: u64,
    /// The raw PTS (90 kHz) of the sound PES each continuity error fell in.
    pub cc_error_pts: Vec<Option<u64>>,
    pub pat: u64,
    pub pmt: u64,
    pub pcr: u64,
    pub pcr_max_gap_ns: u64,
    pub pes: Vec<u64>,
    pub pts_max_gap_ns: u64,
    /// PES PTS steps unlike the usual one, with the PTS they came at.
    pub pts_irregular: Vec<(u64, i64)>,
}

fn pes_pts(payload: &[u8]) -> Option<u64> {
    if payload.len() < 14 || payload[..3] != [0, 0, 1] || payload[7] & 0x80 == 0 {
        return None;
    }
    let b = &payload[9..14];
    Some((u64::from(b[0] >> 1) & 0x07) << 30 | u64::from(b[1]) << 22 | (u64::from(b[2]) >> 1) << 15 | u64::from(b[3]) << 7 | u64::from(b[4]) >> 1)
}

/// TR 101 290 §5.2.1–5.2.2 on what the dump keeps of the stream.
pub fn ts_check(ts: &[u8], sound: u16, pcr: Option<u16>, pmts: &[u16]) -> TsReport {
    let mut r = TsReport::default();
    let mut cc: Option<u8> = None;
    let mut last_pcr: Option<u64> = None;
    let mut current_pes: Option<u64> = None;
    for p in ts.as_chunks::<188>().0 {
        r.packets += 1;
        if p[0] != 0x47 {
            r.sync_errors += 1;
            continue;
        }
        let id = crate::evidence::pid(p);
        if p[1] & 0x80 != 0 {
            r.tei += 1;
            if id == sound {
                cc = None;
            }
            continue;
        }
        let afc = (p[3] >> 4) & 0x3;
        let start = if afc & 0x2 != 0 { 5 + usize::from(p[4]) } else { 4 };
        if id == 0 {
            r.pat += 1;
        }
        if pmts.contains(&id) {
            r.pmt += 1;
        }
        if Some(id) == pcr && crate::evidence::carries_pcr(p) {
            r.pcr += 1;
            let base = (u64::from(p[6]) << 25) | (u64::from(p[7]) << 17) | (u64::from(p[8]) << 9) | (u64::from(p[9]) << 1) | (u64::from(p[10]) >> 7);
            let ext = (u64::from(p[10] & 1) << 8) | u64::from(p[11]);
            let v = base * 300 + ext;
            if let Some(last) = last_pcr
                && v > last
            {
                r.pcr_max_gap_ns = r.pcr_max_gap_ns.max((v - last) * 1000 / 27);
            }
            last_pcr = Some(v);
        }
        if id != sound {
            continue;
        }
        let this = p[3] & 0x0F;
        let has_payload = afc & 0x1 != 0;
        if let Some(prev) = cc {
            let expected = if has_payload { (prev + 1) & 0x0F } else { prev };
            if this != expected && !(has_payload && this == prev) {
                r.cc_errors += 1;
                r.cc_error_pts.push(current_pes);
            }
        }
        cc = Some(this);
        if has_payload
            && p[1] & 0x40 != 0
            && start < 188
            && let Some(pts) = pes_pts(&p[start..])
        {
            current_pes = Some(pts);
            r.pes.push(pts);
        }
    }
    let steps: Vec<i64> = r.pes.windows(2).map(|w| w[1] as i64 - w[0] as i64).collect();
    let usual = {
        let mut counts = std::collections::BTreeMap::new();
        for s in &steps {
            *counts.entry(*s).or_insert(0u64) += 1;
        }
        counts.into_iter().max_by_key(|(_, n)| *n).map(|(s, _)| s)
    };
    for (i, s) in steps.iter().enumerate() {
        r.pts_max_gap_ns = r.pts_max_gap_ns.max(s.unsigned_abs() * 100_000 / 9);
        if Some(*s) != usual {
            r.pts_irregular.push((r.pes[i + 1], *s));
        }
    }
    r
}

// ----------------------------------------------------------------- a dump

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

fn u(v: &serde_json::Value, k: &str) -> Option<u64> {
    v.get(k).and_then(serde_json::Value::as_u64)
}

fn i(v: &serde_json::Value, k: &str) -> Option<i64> {
    v.get(k).and_then(serde_json::Value::as_i64)
}

/// The player's PCM of channel `generation` only: a channel changed in the
/// window starts its times again, and its samples are not the event's.
pub fn segments_of<'a>(index: &[serde_json::Value], raw: &'a [u8], generation: u64) -> Vec<Segment<'a>> {
    index
        .iter()
        .filter(|v| u(v, "gen") == Some(generation))
        .filter_map(|v| {
            let (pts, offset, bytes) = (u(v, "pts")?, u(v, "offset")? as usize, u(v, "bytes")? as usize);
            Some(Segment { pts, data: raw.get(offset..offset + bytes)? })
        })
        .collect()
}

/// The running time of a raw PTS by the reader's own mapping of the PES
/// starts nearest it.
pub fn running_of(raw: u64, pes: &[(u64, u64)]) -> Option<u64> {
    let (r, at) = pes.iter().min_by_key(|(r, _)| r.abs_diff(raw))?;
    let ns = (raw as i64 - *r as i64) * 100_000 / 9;
    Some((*at as i64 + ns).max(0) as u64)
}

/// The independent decoding: ffmpeg's `mp2` decoder on the dump's sound
/// PES, cut at the last whole one, with the PAT, PMT and sound packets only
/// (the PCR_PID's packets the dump keeps are pieces of the picture's PES,
/// which ffmpeg's demultiplexer took for corrupt ones and lost sound PES
/// over: 67 of 266 in FAZ 2H's first silence dump). Each decoded frame's
/// raw PTS and samples (ffprobe, the same decoder), its PCM (S16LE, as the
/// stream's rate and channels) in their order, and every packet's PTS.
#[derive(Debug, Default)]
pub struct Independent {
    pub pcm: Vec<u8>,
    pub packets: Vec<u64>,
    pub frames: Vec<(u64, u64)>,
    pub rate: u32,
    pub channels: usize,
}

fn ffprobe(cut: &Path, args: &[&str]) -> Result<Vec<Vec<u64>>, String> {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error"])
        .args(args)
        .args(["-of", "compact=p=0:nk=1"])
        .arg(cut)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(|l| l.split('|').filter_map(|f| f.parse::<u64>().ok()).collect()).collect())
}

fn decode_independent(dir: &Path, ts: &[u8], sound: u16, pmts: &[u16]) -> Result<Independent, String> {
    let kept: Vec<&[u8; 188]> =
        ts.as_chunks::<188>().0.iter().filter(|p| matches!(crate::evidence::pid(*p), id if id == 0 || id == sound || pmts.contains(&id))).collect();
    let last_start = kept.iter().rposition(|p| crate::evidence::pid(*p) == sound && p[1] & 0x40 != 0).ok_or("no sound PES start")?;
    let cut = dir.join("independent-input.ts");
    std::fs::write(&cut, kept[..last_start].iter().flat_map(|p| p.iter().copied()).collect::<Vec<u8>>()).map_err(|e| e.to_string())?;
    let mut ind = Independent {
        packets: ffprobe(&cut, &["-select_streams", "a:0", "-show_entries", "packet=pts"])?.into_iter().filter_map(|r| r.first().copied()).collect(),
        ..Default::default()
    };
    // Each frame as the decoder put it out, from the decoding itself
    // (ashowinfo): ffprobe's choice of decoder (-codec:a) is refused by the
    // appliance's ffprobe 7.1.5 ("Option not found"; the PC's 9.0.2 has
    // it), which left the frames empty and every packet "undecoded".
    let out = dir.join("independent.s16");
    let run = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-v", "info", "-y", "-copyts", "-c:a", "mp2", "-i"])
        .arg(&cut)
        .args(["-map", "0:a:0", "-af", "ashowinfo", "-f", "s16le", "-c:a", "pcm_s16le"])
        .arg(&out)
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    std::fs::write(dir.join("independent-ffmpeg.log"), &run.stderr).map_err(|e| e.to_string())?;
    for (raw, samples, rate, channels) in ashowinfo_frames(&String::from_utf8_lossy(&run.stderr)) {
        ind.frames.push((raw, samples));
        ind.rate = rate;
        ind.channels = channels;
    }
    ind.pcm = std::fs::read(&out).map_err(|e| e.to_string())?;
    Ok(ind)
}

/// The frames of ffmpeg's ashowinfo lines ("n:0 pts:4341588721 ...
/// channels:2 ... rate:48000 nb_samples:1152"): each one's PTS as the raw
/// 90 kHz PTS (ashowinfo's is in samples), samples, rate, channels.
pub fn ashowinfo_frames(log: &str) -> Vec<(u64, u64, u32, usize)> {
    log.lines()
        .filter(|l| l.contains("Parsed_ashowinfo") && l.contains(" n:"))
        .filter_map(|l| {
            let field = |name: &str| l.split_whitespace().find_map(|f| f.strip_prefix(name)).and_then(|v| v.parse::<u64>().ok());
            let (pts, rate, samples, channels) = (field("pts:")?, field("rate:")?, field("nb_samples:")?, field("channels:")?);
            (rate > 0).then(|| (((u128::from(pts) * 90_000 + u128::from(rate) / 2) / u128::from(rate)) as u64, samples, rate as u32, channels as usize))
        })
        .collect()
}

/// Packets whose time is in the event's window (from 100 ms before it to
/// its end) that the independent decoder put out no frame for: what it
/// refused or could not find a header in.
/// None where the decoder gave no frames at all: that is the decoding not
/// done, which says nothing of the input.
pub fn undecoded_in(packets: &[u64], frames: &[(u64, u64)], pes: &[(u64, u64)], from: u64, to: u64) -> Option<u64> {
    if frames.is_empty() {
        return None;
    }
    Some(
        packets
            .iter()
            .filter(|raw| running_of(**raw, pes).is_some_and(|t| t + 100_000_000 >= from && t <= to))
            .filter(|raw| !frames.iter().any(|(pts, _)| pts == *raw))
            .count() as u64,
    )
}

/// Judge one dump; the result is also written as `classify.json` in it.
pub fn classify_dump(dir: &Path) -> Result<serde_json::Value, String> {
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let kind = meta["kind"].as_str().unwrap_or_default().to_string();
    let service = &meta["service"];
    let sound = u(service, "sound_pid").ok_or("no sound PID in the dump")? as u16;
    let pcr = u(service, "pcr_pid").map(|p| p as u16);
    let pmts: Vec<u16> = service["pmt_pids"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as u16).collect()).unwrap_or_default();
    let ts = std::fs::read(dir.join("sound.ts")).map_err(|e| e.to_string())?;
    let report = ts_check(&ts, sound, pcr, &pmts);
    let pes: Vec<(u64, u64)> = lines(&dir.join("pes.jsonl")).iter().filter_map(|v| Some((u(v, "raw")?, u(v, "pts")?))).collect();
    let mut out = serde_json::json!({
        "dump": dir.file_name().map(|n| n.to_string_lossy().to_string()), "kind": kind,
        "tr101290": {
            "edition": "ETSI TR 101 290 V1.4.1, indicators of §5.2.1-5.2.2 on the captured packets only",
            "captured_packets": report.packets, "sync_byte_errors_in_captured": report.sync_errors,
            "transport_error_indicator_packets": report.tei, "continuity_errors_sound_pid": report.cc_errors,
            "captured_pat_packets": report.pat, "captured_pmt_packets": report.pmt, "pcr_count": report.pcr,
            "pcr_max_gap_ms": report.pcr_max_gap_ns as f64 / 1e6,
            "pcr_repetition_error_100ms": report.pcr_max_gap_ns > 100_000_000,
            "project_strict_pcr_40ms": report.pcr_max_gap_ns > 40_000_000,
            "sound_pes": report.pes.len(), "pts_max_gap_ms": report.pts_max_gap_ns as f64 / 1e6,
            "pts_error_700ms": report.pts_max_gap_ns > 700_000_000, "pts_irregular": report.pts_irregular,
            "not_measured": ["TS_sync_loss (the whole stream is not kept)", "PAT/PMT repetition interval", "PAT/PMT section CRC",
                             "PAT/PMT version consistency", "PCR_accuracy (arrival is kept per piece, not per packet)",
                             "PCR_discontinuity_indicator handling"],
        },
    });
    if kind == "clock" {
        out["clock"] = classify_clock_dump(dir, &meta)?;
    } else {
        out["silence"] = classify_silence_dump(dir, &meta, &ts, (sound, &pmts), &report, &pes)?;
    }
    std::fs::write(dir.join("classify.json"), serde_json::to_vec_pretty(&out).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(out)
}

fn pcm_format(meta: &serde_json::Value) -> Option<PcmFormat> {
    // "PcmFormat { sample: S16, rate: 48000, channels: 2, interleaved: true }"
    let f = meta["pcm_format"].as_str()?;
    let value = |key: &str| f.split(key).nth(1)?.trim_start_matches([':', ' ']).split([',', ' ', '}']).next().map(str::to_string);
    let sample = match value("sample")?.as_str() {
        "S16" => Sample::S16,
        "S32" => Sample::S32,
        "F32" => Sample::F32,
        _ => return None,
    };
    Some(PcmFormat { sample, rate: value("rate")?.parse().ok()?, channels: value("channels")?.parse().ok()?, interleaved: value("interleaved")? == "true" })
}

fn classify_silence_dump(
    dir: &Path,
    meta: &serde_json::Value,
    ts: &[u8],
    (sound, pmts): (u16, &[u16]),
    report: &TsReport,
    pes: &[(u64, u64)],
) -> Result<serde_json::Value, String> {
    let note = &meta["note"];
    let format = pcm_format(meta).ok_or("the player's PCM format is not known")?;
    let (Some(from), Some(ms)) = (u(note, "pts"), note["duration_ms"].as_f64()) else {
        return Ok(serde_json::json!({ "class": "unclassified", "why": "not a silence of decoded samples (no PTS window)" }));
    };
    let to = from + (ms * 1e6) as u64;
    // The player's PCM.
    let raw = std::fs::read(dir.join("pcm.raw")).map_err(|e| e.to_string())?;
    let index = lines(&dir.join("pcm-index.jsonl"));
    let product = segments_of(&index, &raw, u(note, "generation").unwrap_or(0));
    let product_level = level_in(&product, &format, from, to);
    // The independent decoding, on the same running time by its PTS.
    let mut facts = SilenceFacts {
        user_silenced: note["muted"].as_bool().unwrap_or(false) || note["gain"].as_f64().is_some_and(|g| g <= 0.0),
        product: Some(product_level),
        ..Default::default()
    };
    let independent_json;
    let mut lag = serde_json::Value::Null;
    let codec = meta["service"]["codec"].as_str().unwrap_or_default();
    if codec == "mpeg-audio" {
        match decode_independent(dir, ts, sound, pmts) {
            Ok(ind) => {
                let ind_format = PcmFormat { sample: Sample::S16, rate: ind.rate, channels: ind.channels, interleaved: true };
                let fb = ind.channels * 2;
                let samples: u64 = ind.frames.iter().map(|(_, n)| n).sum();
                // A decoding not done whole (no frames, or PCM other than
                // its frames say) is no decoding: the event stays
                // unclassified, not the input's.
                let whole = !ind.frames.is_empty() && ind.rate > 0 && ind.channels > 0 && ind.pcm.len() as u64 == samples * fb as u64;
                facts.decoder_errors = if whole { undecoded_in(&ind.packets, &ind.frames, pes, from, to).unwrap_or(0) } else { 0 };
                if whole && ind.rate == format.rate && ind.channels == format.channels {
                    let mut at = 0usize;
                    let segments: Vec<Segment> = ind
                        .frames
                        .iter()
                        .filter_map(|(raw, n)| {
                            let data = &ind.pcm[at..at + *n as usize * fb];
                            at += *n as usize * fb;
                            Some(Segment { pts: running_of(*raw, pes)?, data })
                        })
                        .collect();
                    facts.independent = Some(level_in(&segments, &ind_format, from, to));
                    lag = decoder_lag(&product, &segments, &format, from);
                }
                independent_json = serde_json::json!({
                    "decoder": "ffmpeg mp2 (libavcodec)", "packets": ind.packets.len(), "frames": ind.frames.len(),
                    "rate": ind.rate, "channels": ind.channels, "samples_whole": whole,
                    "undecoded_in_window": facts.decoder_errors,
                    "level": facts.independent.as_ref().map(|(l, c)| level_json(l, *c)),
                });
            }
            Err(why) => independent_json = serde_json::json!({ "error": why }),
        }
    } else {
        independent_json = serde_json::json!({ "error": format!("no independent decoder wired for {codec}") });
    }
    // Input errors in the event's PES window (100 ms before it to its end).
    let near = |raw: Option<u64>| raw.and_then(|r| running_of(r, pes)).is_some_and(|t| t + 100_000_000 >= from && t <= to);
    facts.input_errors = report.cc_error_pts.iter().filter(|p| near(**p)).count() as u64
        + report.pts_irregular.iter().filter(|(p, _)| near(Some(*p))).count() as u64
        + report.tei
        + report.sync_errors;
    // The sink: gaps between buffers into it over the window.
    let sink = lines(&dir.join("sink.jsonl"));
    let mut lost = 0u64;
    for w in sink.windows(2) {
        if let (Some(p0), Some(d0), Some(p1)) = (u(&w[0], "pts"), u(&w[0], "dur"), u(&w[1], "pts"))
            && p1 + 100_000_000 >= from
            && p0 <= to
            && p1 > p0 + d0 + 1_000_000
        {
            lost += p1 - p0 - d0;
        }
    }
    facts.sink_lost_ns = lost;
    let class = classify_silence(&facts);
    Ok(serde_json::json!({
        "class": class.name(), "why": if let SilenceClass::Unclassified(w) = class { w } else { "" },
        "window_pts": [from, to],
        "product": level_json(&product_level.0, product_level.1),
        "independent": independent_json, "decoder_lag": lag,
        "input_errors": facts.input_errors, "decoder_errors": facts.decoder_errors,
        "sink_lost_ms": lost as f64 / 1e6, "user_silenced": facts.user_silenced,
        "product_peak_detector_threshold_dbfs": EXIT_DBFS,
        "threshold_is": "the player's meter's own sample-peak threshold; not BS.1770/R128 loudness, not a standard silence limit",
        "basis": "two independent decoders (libmpg123, ffmpeg mp2) compared on the same PTS window",
    }))
}

/// The two decodings' lag by cross-correlation over the half second
/// before the event (channel 0), and how alike they are there.
fn decoder_lag(product: &[Segment], independent: &[Segment], format: &PcmFormat, event: u64) -> serde_json::Value {
    if format.sample != Sample::S16 {
        return serde_json::Value::Null;
    }
    let samples = |segments: &[Segment], from: u64, n: usize| -> Vec<f64> {
        let mut out = vec![f64::NAN; n];
        let fb = format.channels * 2;
        for s in segments {
            for k in 0..s.data.len() / fb {
                let t = s.pts + format.ns(k as u64);
                if t < from {
                    continue;
                }
                let i = ((t - from) as u128 * u128::from(format.rate) / 1_000_000_000) as usize;
                if i < n {
                    out[i] = f64::from(i16::from_le_bytes([s.data[k * fb], s.data[k * fb + 1]]));
                }
            }
        }
        out
    };
    let n = format.rate as usize / 2;
    let from = event.saturating_sub(600_000_000);
    let a = samples(product, from, n);
    let b = samples(independent, from, n);
    let max_lag = 1152isize;
    let mut best = (0isize, f64::MIN);
    for lag in -max_lag..=max_lag {
        let (mut s, mut sa, mut sb) = (0.0, 0.0, 0.0);
        for i in 0..n as isize {
            let j = i + lag;
            if j < 0 || j >= n as isize {
                continue;
            }
            let (x, y) = (a[i as usize], b[j as usize]);
            if x.is_nan() || y.is_nan() {
                continue;
            }
            s += x * y;
            sa += x * x;
            sb += y * y;
        }
        let c = if sa > 0.0 && sb > 0.0 { s / (sa * sb).sqrt() } else { 0.0 };
        if c > best.1 {
            best = (lag, c);
        }
    }
    serde_json::json!({ "lag_samples": best.0, "correlation": best.1, "over": "0.5 s ending 0.1 s before the event, channel 0" })
}

fn classify_clock_dump(dir: &Path, meta: &serde_json::Value) -> Result<serde_json::Value, String> {
    let format = pcm_format(meta);
    let fb = format.map_or(4, |f| f.channels as u64 * if f.sample == Sample::S16 { 2 } else { 4 });
    let rate = format.map_or(48_000.0, |f| f64::from(f.rate));
    let sink = lines(&dir.join("sink.jsonl"));
    let feed: Vec<(u64, u64)> = sink.iter().filter_map(|v| Some((u(v, "at")?, u(v, "bytes")? / fb))).collect();
    let mut rows: Vec<ClockRow> = lines(&dir.join("clock.jsonl"))
        .iter()
        .map(|v| {
            let a = &v["alsa"];
            ClockRow {
                at: u(v, "at").unwrap_or(0),
                after: u(v, "after").unwrap_or(0),
                late: i(v, "late"),
                internal: u(v, "internal"),
                calibration: v["calibration"]
                    .as_array()
                    .and_then(|c| Some([c.first()?.as_u64()?, c.get(1)?.as_u64()?, c.get(2)?.as_u64()?, c.get(3)?.as_u64()?])),
                running: a["running"].as_bool().unwrap_or(false),
                trigger: u(a, "trigger"),
                tstamp: u(a, "tstamp"),
                read_at: u(v, "alsa_read_at").map(|t| t + u(v, "alsa_read_ns").unwrap_or(0) / 2),
                hw_ptr: u(a, "hw_ptr"),
                appl_ptr: u(a, "appl_ptr"),
                fed: 0,
            }
        })
        .collect();
    for r in rows.iter_mut() {
        r.fed = feed.iter().filter(|(at, _)| *at <= r.at).map(|(_, f)| f).sum();
    }
    let Some(step) = u(&meta["note"], "at").and_then(|t| rows.iter().position(|r| r.at >= t)) else {
        return Ok(serde_json::json!({ "class": "unclassified", "why": "the step's sample is not in the dump" }));
    };
    let timed: Vec<(u64, u64, u64)> = sink.iter().filter_map(|v| Some((u(v, "at")?, u(v, "pts")?, u(v, "dur")?))).collect();
    let start = Start::of(&rows, &timed.iter().map(|(a, p, _)| (*a, *p)).collect::<Vec<_>>());
    // The sink's own placement, where a run kept it (GST_DEBUG=audiobasesink:5
    // into the dump as gst-audiobasesink.log): its buffers from the
    // device's start to 1.5 s after the step, by running time against the
    // sink's own PTS (`sink.jsonl`).
    let placed: Vec<Placement> = std::fs::read_to_string(dir.join("gst-audiobasesink.log")).map(|t| placements(&t)).unwrap_or_default();
    let window: Vec<Placement> = match (start.first_sink_pts, start.first_sink_at, u(&meta["note"], "at")) {
        (Some(pts0), Some(at0), Some(step_at)) => {
            let until = pts0 + (step_at + 1_500_000_000).saturating_sub(at0);
            placed.iter().copied().filter(|p| p.running_ns >= pts0 && p.running_ns <= until).collect()
        }
        _ => Vec::new(),
    };
    let facts = clock_facts(&rows, step, rate, &timed, &window);
    let class = classify_clock(&facts);
    let tstamp_vs_mono = rows.iter().filter_map(|r| Some(r.tstamp? as i64 - r.at as i64)).map(|d| d as f64 / 1e6).fold(f64::NAN, f64::max);
    Ok(serde_json::json!({
        "class": class.name(), "why": class.why(),
        "off_ms": facts.off_ns as f64 / 1e6, "read_us": facts.read_ns as f64 / 1e3, "back_after_samples": facts.back_after,
        "hw_residual_ms": facts.hw_residual_ns.map(|v| v / 1e6), "feed_shift_ms": facts.feed_shift_ns.map(|v| v / 1e6),
        "device_restarted": facts.device_restarted, "calibration_changed": facts.calibration_changed,
        "samples": rows.len(), "tstamp_minus_mono_ms_max": tstamp_vs_mono,
        "start": start.json(), "since_start_ms": facts.since_start_ns.map(|v| v as f64 / 1e6),
        "calibration_changes": facts.calibration_changes, "sink_jump_ms": facts.sink_jump_ns.map(|v| v / 1e6),
        "placement_buffers": window.len(), "placement_step_ms": facts.placement_step_ns.map(|v| v / 1e6),
        "placement_moved_ms": facts.placement_moved_ns.map(|v| v / 1e6),
        "threshold_ms": PRESENT_STEP_NS / 1e6,
    }))
}

#[cfg(test)]
#[path = "evidence_classify_tests.rs"]
mod tests;
