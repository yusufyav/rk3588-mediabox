//! A dump of `evidence` read back and judged: `mediabox-live
//! --evidence-classify <dump>`. Diagnosis only; nothing here runs in a
//! playing player.
//!
//! The transport (ETSI TR 101 290 §5.2.1–5.2.2, on the packets the dump
//! keeps): sync byte, transport_error_indicator, continuity of the sound's
//! PID, the PAT's and PMT's repetition, the PCR's repetition (40 ms) and
//! the sound PES's PTS repetition (700 ms) and step.
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

/// Silent as the meter holds a run silent: every sample under `EXIT_DBFS`.
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClockClass {
    MeasurementArtifact(&'static str),
    ProductAudioTimingStep(&'static str),
    Unclassified(&'static str),
}

impl ClockClass {
    pub fn name(self) -> &'static str {
        match self {
            ClockClass::MeasurementArtifact(_) => "measurement_artifact",
            ClockClass::ProductAudioTimingStep(_) => "product_audio_timing_step",
            ClockClass::Unclassified(_) => "unclassified",
        }
    }

    pub fn why(self) -> &'static str {
        match self {
            ClockClass::MeasurementArtifact(w) | ClockClass::ProductAudioTimingStep(w) | ClockClass::Unclassified(w) => w,
        }
    }
}

/// A step of the presentation larger than this is the product's.
pub const PRESENT_STEP_NS: f64 = 2_000_000.0;

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
    /// far its median moved over the step, ns.
    pub feed_shift_ns: Option<f64>,
    pub device_restarted: bool,
    pub calibration_changed: bool,
}

pub fn classify_clock(f: &ClockFacts) -> ClockClass {
    if f.device_restarted {
        return ClockClass::Unclassified("the device started again in the window");
    }
    let Some(hw) = f.hw_residual_ns else { return ClockClass::Unclassified("no device timestamps") };
    let Some(feed) = f.feed_shift_ns else { return ClockClass::Unclassified("no device pointers against the feed") };
    if hw > PRESENT_STEP_NS {
        return ClockClass::ProductAudioTimingStep("the device's playout stepped against its timestamps");
    }
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

/// The facts of the clock sample at `step` in `rows`.
pub fn clock_facts(rows: &[ClockRow], step: usize, rate: f64) -> ClockFacts {
    let at = rows[step].at;
    let before: Vec<f64> = rows.iter().filter(|r| r.at < at && at - r.at <= 1_000_000_000).filter_map(|r| r.late.map(|l| l as f64)).collect();
    let base = median(before.clone()).unwrap_or(0.0);
    let off = rows[step].late.map_or(0.0, |l| l as f64 - base);
    let back_after = rows[step + 1..].iter().position(|r| r.late.is_some_and(|l| (l as f64 - base).abs() <= 1_000_000.0)).map(|n| n + 1);
    // The device's frames against its timestamps -- or, where it keeps
    // none, the monotonic time its status was read at: a line by least
    // squares.
    let points: Vec<(f64, f64)> =
        rows.iter().filter(|r| r.running).filter_map(|r| Some((r.tstamp.filter(|t| *t > 0).or(r.read_at)? as f64, r.hw_ptr? as f64))).collect();
    let hw_residual_ns = (points.len() >= 20).then(|| {
        let n = points.len() as f64;
        let (t0, p0) = points[0];
        let (mt, mp) = points.iter().fold((0.0, 0.0), |(a, b), (t, p)| (a + (t - t0) / n, b + (p - p0) / n));
        let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(a, b), (t, p)| (a + ((t - t0) - mt) * ((p - p0) - mp), b + ((t - t0) - mt).powi(2)));
        let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        // Residual in frames, as time at the device's rate.
        points.iter().map(|(t, p)| (((p - p0) - mp) - slope * ((t - t0) - mt)).abs()).fold(0.0, f64::max) / rate * 1e9
    });
    let shift = |lo: i64, hi: i64| {
        median(
            rows.iter()
                .filter(|r| {
                    let d = r.at as i64 - at as i64;
                    d >= lo && d <= hi
                })
                .filter_map(|r| Some(r.appl_ptr? as f64 - r.fed as f64))
                .collect(),
        )
    };
    let feed_shift_ns = match (shift(-1_500_000_000, -200_000_000), shift(200_000_000, 1_500_000_000)) {
        (Some(a), Some(b)) => Some((b - a) / rate * 1e9),
        _ => None,
    };
    let triggers: Vec<u64> = rows.iter().filter_map(|r| r.trigger).collect();
    let device_restarted = triggers.windows(2).any(|w| w[0] != w[1]) || rows.iter().any(|r| r.appl_ptr.is_some() && !r.running);
    let calibration_changed = step > 0 && rows[step].calibration != rows[step - 1].calibration;
    ClockFacts {
        off_ns: off as i64,
        read_ns: rows[step].after.saturating_sub(rows[step].at),
        back_after,
        hw_residual_ns,
        feed_shift_ns,
        device_restarted,
        calibration_changed,
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
/// PES, cut at the last whole one. Its PCM (S16LE, as the stream's rate and
/// channels), each packet's raw PTS, and the decoder's complaints.
/// PCM, each packet's raw PTS, complaints, rate, channels.
type Decoded = (Vec<u8>, Vec<u64>, u64, u32, usize);

fn decode_independent(dir: &Path, ts: &[u8], sound: u16) -> Result<Decoded, String> {
    let last_start = ts.as_chunks::<188>().0.iter().rposition(|p| crate::evidence::pid(p) == sound && p[1] & 0x40 != 0).ok_or("no sound PES start")?;
    let cut = dir.join("independent-input.ts");
    std::fs::write(&cut, &ts[..last_start * 188]).map_err(|e| e.to_string())?;
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=sample_rate,channels:packet=pts", "-of", "compact=p=0:nk=1"])
        .arg(&cut)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    let text = String::from_utf8_lossy(&probe.stdout);
    let mut packets = Vec::new();
    let mut rate = 0u32;
    let mut channels = 0usize;
    for line in text.lines() {
        let fields: Vec<&str> = line.split('|').collect();
        match fields.as_slice() {
            [pts] => packets.extend(pts.parse::<u64>().ok()),
            [r, c] => {
                rate = r.parse().unwrap_or(0);
                channels = c.parse().unwrap_or(0);
            }
            _ => {}
        }
    }
    let out = dir.join("independent.s16");
    let run = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-v", "warning", "-y", "-c:a", "mp2", "-i"])
        .arg(&cut)
        .args(["-map", "0:a:0", "-f", "s16le", "-c:a", "pcm_s16le"])
        .arg(&out)
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let complaints = String::from_utf8_lossy(&run.stderr);
    std::fs::write(dir.join("independent-ffmpeg.log"), complaints.as_bytes()).map_err(|e| e.to_string())?;
    let errors = complaints.lines().filter(|l| !l.trim().is_empty()).count() as u64;
    let pcm = std::fs::read(&out).map_err(|e| e.to_string())?;
    Ok((pcm, packets, errors, rate, channels))
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
            "packets": report.packets, "sync_errors": report.sync_errors, "transport_error": report.tei,
            "cc_errors": report.cc_errors, "pat": report.pat, "pmt": report.pmt, "pcr": report.pcr,
            "pcr_max_gap_ms": report.pcr_max_gap_ns as f64 / 1e6, "pcr_repetition_error": report.pcr_max_gap_ns > 40_000_000,
            "sound_pes": report.pes.len(), "pts_max_gap_ms": report.pts_max_gap_ns as f64 / 1e6,
            "pts_error": report.pts_max_gap_ns > 700_000_000, "pts_irregular": report.pts_irregular,
        },
        "pcr_accuracy": "not measured: the dump keeps arrival per piece, not per packet",
    });
    if kind == "clock" {
        out["clock"] = classify_clock_dump(dir, &meta)?;
    } else {
        out["silence"] = classify_silence_dump(dir, &meta, &ts, sound, &report, &pes)?;
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
    sound: u16,
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
        match decode_independent(dir, ts, sound) {
            Ok((pcm, packets, errors, rate, channels)) => {
                let ind_format = PcmFormat { sample: Sample::S16, rate, channels, interleaved: true };
                let per = 1152 * channels * 2;
                let frames_ok = rate > 0 && channels > 0 && pcm.len() == packets.len() * per;
                facts.decoder_errors = errors + u64::from(!frames_ok);
                if frames_ok && rate == format.rate && channels == format.channels {
                    let segments: Vec<Segment> = packets
                        .iter()
                        .enumerate()
                        .filter_map(|(k, raw)| Some(Segment { pts: running_of(*raw, pes)?, data: &pcm[k * per..(k + 1) * per] }))
                        .collect();
                    facts.independent = Some(level_in(&segments, &ind_format, from, to));
                    lag = decoder_lag(&product, &segments, &format, from);
                }
                independent_json = serde_json::json!({
                    "decoder": "ffmpeg mp2 (libavcodec)", "packets": packets.len(), "rate": rate, "channels": channels,
                    "samples_whole": frames_ok, "complaints": errors,
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
        "threshold_dbfs": EXIT_DBFS,
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
    let facts = clock_facts(&rows, step, rate);
    let class = classify_clock(&facts);
    let tstamp_vs_mono = rows.iter().filter_map(|r| Some(r.tstamp? as i64 - r.at as i64)).map(|d| d as f64 / 1e6).fold(f64::NAN, f64::max);
    Ok(serde_json::json!({
        "class": class.name(), "why": class.why(),
        "off_ms": facts.off_ns as f64 / 1e6, "read_us": facts.read_ns as f64 / 1e3, "back_after_samples": facts.back_after,
        "hw_residual_ms": facts.hw_residual_ns.map(|v| v / 1e6), "feed_shift_ms": facts.feed_shift_ns.map(|v| v / 1e6),
        "device_restarted": facts.device_restarted, "calibration_changed": facts.calibration_changed,
        "samples": rows.len(), "tstamp_minus_mono_ms_max": tstamp_vs_mono,
        "threshold_ms": PRESENT_STEP_NS / 1e6,
    }))
}

#[cfg(test)]
#[path = "evidence_classify_tests.rs"]
mod tests;
