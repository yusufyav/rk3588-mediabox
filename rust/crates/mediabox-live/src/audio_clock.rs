//! The sound's clock: the device's sample clock followed against the
//! pipeline's (CLOCK_MONOTONIC) by a bounded controller of the player's
//! own, and the difference taken up by resampling the sound -- never by
//! moving a buffer, never by repeating or dropping a sample.
//!
//! Why (`audio_replay`, FAZ 2I): with `slave-method=resample` GStreamer
//! makes the audio clock's calibration a least-squares line through the
//! last 32 observations, each a reading of the audio clock and then of the
//! master; one observation whose master was read 20-45 ms late moved the
//! calibration by 2.4-5.5 ms, and audiobasesink put that move into one
//! buffer -- 1152 samples played in 991. The crystal's ordinary drift was
//! followed by a sample repeated or dropped now and then.
//!
//! What instead, for PCM (`slave-method=custom`, no callback: audiobasesink
//! applies the calibration and requests no skew, and aligns each buffer's
//! start and end to the one before, so it never stretches one):
//!
//! - `ClockModel`: the calibration -- the audio clock's time against the
//!   pipeline's -- set by this controller only. Each observation is the
//!   audio clock read between two readings of the pipeline's; one whose two
//!   readings lie further apart than `SPAN_NS` is not an observation of one
//!   moment and is left out, and one off the recent median by more than
//!   `GATE_NS` (the one-segment reading race) is a transient, left out. The
//!   rest feed a phase-locked loop (`LOOP_TAU_S`) whose rate is bounded by
//!   `RATE_RANGE`; every change is made continuous at the moment it is made,
//!   so the calibration has no step of its own.
//! - `PhaseLoop`: each buffer's phase, where audiobasesink will write it
//!   (the end of the buffer before) against where the calibration says its
//!   time is, taken out by resampling at most `RATE_RANGE` of a buffer at a
//!   time (`PHASE_TAU_NS`). A phase beyond `DISCONT_NS` is no drift -- the
//!   sound's own times jumped -- and goes to audiobasesink as a DISCONT: put
//!   at its time at once, and counted apart.
//! - `Resampler`: a band-limited (windowed-sinc) resampler by a ratio within
//!   1 +- `RATE_RANGE`; with no ratio it passes every sample through as it
//!   came.
//!
//! The reset contract: the sink beginning again (its next sample unknown),
//! a DISCONT, a FLUSH, new caps, another channel (generation) or a
//! calibration somebody else set (audiobasesink's sync to the latency)
//! starts the phase again -- the model takes the clock's calibration as it
//! stands, the phase loop and the resampler begin empty. The frequency
//! learned is the device's crystal's and is kept.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

/// The product's target for a step it adds to the sound's place in one
/// buffer (a project acceptance target, not a standard's; its limit is
/// 2 ms).
pub const STEP_BUDGET_NS: f64 = 0.5e6;
/// alsasink's latency-time (`engine`): the ring's segment, and the size of
/// the audio clock's reading race (a segment written before the count of
/// segments done is advanced, gstaudiosink.c; FAZ 2G).
pub const SEGMENT_NS: f64 = 10e6;
/// The most the device's clock may run off the pipeline's, either way:
/// NTP's largest frequency correction (500 ppm, adjtimex) and a crystal's
/// tolerance (100 ppm), with room. Also the most the resampler stretches
/// or squeezes a buffer.
pub const RATE_RANGE: f64 = 1e-3;
/// The longest PCM buffer a decoder gives: an AC-3 frame, 1536 samples.
pub const LONGEST_BUFFER: usize = 1536;
/// The bound's step in the longest buffer at the lowest rate the sink is
/// given (32 kHz): within the budget, so no buffer is stretched into a step.
const _: () = assert!(RATE_RANGE * LONGEST_BUFFER as f64 * 1e9 / 32_000.0 < STEP_BUDGET_NS);

/// A reading this far off the recent median is a transient: a quarter of
/// the reading race, four times the reading's jitter (ALSA's delay moves in
/// steps of 8 samples, 0.17 ms, measured 2026-11-07).
pub const GATE_NS: f64 = SEGMENT_NS / 4.0;
/// An observation's two readings of the pipeline's clock this far apart do
/// not place the audio clock's reading to within a tenth of the gate.
pub const SPAN_NS: f64 = GATE_NS / 10.0;
/// The median the gate is held against: the last second's readings, one a
/// buffer.
pub const BASELINE: usize = 41;
/// The model's loop: a period, and its time constant -- slow against a
/// reading's faults (tens of ms), fast against a crystal's warming
/// (minutes); critically damped.
pub const PERIOD_NS: f64 = 1e9;
pub const LOOP_TAU_S: f64 = 20.0;
const KP: f64 = 2.0 / LOOP_TAU_S;
const KI: f64 = (PERIOD_NS / 1e9) / (LOOP_TAU_S * LOOP_TAU_S);
/// The phase loop's time constant: 80 times faster than the model's, so
/// the two do not hunt each other; below `RATE_RANGE * PHASE_TAU_NS`
/// (0.25 ms) a phase is taken out in proportion, above it at the bound.
pub const PHASE_TAU_NS: f64 = 0.25e9;
/// A phase this large is a jump in the sound's times, not drift: the
/// model's loop moves the calibration by at most `RATE_RANGE` of the time
/// passed, which keeps drift's phase under a millisecond.
pub const DISCONT_NS: f64 = SEGMENT_NS;

// ------------------------------------------------------------- the model

/// A GstClock calibration: external = (internal - internal0) * num/denom + external0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calibration {
    pub internal: u64,
    pub external: u64,
    pub num: u64,
    pub denom: u64,
}

impl Calibration {
    /// The audio clock's time at the pipeline's `external`
    /// (gstaudiobasesink.c `clock_convert_external`).
    pub fn to_internal(self, external: f64) -> f64 {
        let (num, denom) = if self.num == 0 { (1, 1) } else { (self.num, self.denom) };
        self.internal as f64 + (external - self.external as f64) * denom as f64 / num as f64
    }

    /// d internal / d external.
    pub fn rate(self) -> f64 {
        if self.num == 0 { 1.0 } else { self.denom as f64 / self.num as f64 }
    }
}

/// The audio clock read between two readings of the pipeline's, ns.
#[derive(Debug, Clone, Copy)]
pub struct Observation {
    pub before: u64,
    pub internal: u64,
    pub after: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Used,
    /// The pipeline's two readings too far apart.
    NotOneMoment,
    /// Off the recent median by more than the gate.
    Transient,
}

const SCALE: f64 = 1e12;

#[derive(Debug, Clone)]
pub struct ClockModel {
    anchor_external: f64,
    anchor_internal: f64,
    rate: f64,
    frequency: f64,
    recent: VecDeque<f64>,
    period_from: Option<f64>,
    period_sum: f64,
    period_count: u32,
    last_set: Option<Calibration>,
    /// The phase taken from the median of the readings since the last
    /// adoption (once), not from any one reading.
    acquired: bool,
    /// The frequency learned from the readings, not just taken.
    learned: bool,
    pub updates: u64,
}

impl ClockModel {
    pub fn new(cal: Calibration) -> Self {
        let mut m = Self {
            anchor_external: 0.0,
            anchor_internal: 0.0,
            rate: 1.0,
            frequency: 1.0,
            recent: VecDeque::new(),
            period_from: None,
            period_sum: 0.0,
            period_count: 0,
            last_set: None,
            acquired: false,
            learned: false,
            updates: 0,
        };
        m.adopt(cal);
        m
    }

    /// A calibration somebody else set (audiobasesink's sync to the latency
    /// at its start), taken as it stands: its phase is where the sink put
    /// the sound. The frequency learned is the crystal's and is kept; the
    /// phase is acquired again from the readings that follow.
    pub fn adopt(&mut self, cal: Calibration) {
        self.anchor_external = cal.external as f64;
        self.anchor_internal = cal.internal as f64;
        if !self.learned {
            self.frequency = cal.rate();
        }
        self.rate = self.frequency;
        self.acquired = false;
        self.recent.clear();
        self.period_from = None;
        self.period_sum = 0.0;
        self.period_count = 0;
        self.last_set = Some(cal);
    }

    /// The calibration this model set last: one the clock has that is not
    /// this was set by somebody else.
    pub fn last_set(&self) -> Option<Calibration> {
        self.last_set
    }

    pub fn internal_at(&self, external: f64) -> f64 {
        self.anchor_internal + (external - self.anchor_external) * self.rate
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn calibration(&self) -> Calibration {
        Calibration {
            internal: self.anchor_internal.round().max(0.0) as u64,
            external: self.anchor_external.round().max(0.0) as u64,
            num: SCALE as u64,
            denom: (SCALE * self.rate).round() as u64,
        }
    }

    /// An observation in; a calibration out when the period's update is
    /// made.
    pub fn observe(&mut self, o: Observation) -> (Verdict, Option<Calibration>) {
        let span = o.after.saturating_sub(o.before) as f64;
        if span > SPAN_NS {
            return (Verdict::NotOneMoment, None);
        }
        let at = o.before as f64 + span / 2.0;
        let residual = o.internal as f64 - self.internal_at(at);
        let baseline = median(&self.recent).unwrap_or(residual);
        self.recent.push_back(residual);
        while self.recent.len() > BASELINE {
            self.recent.pop_front();
        }
        if (residual - baseline).abs() > GATE_NS {
            return (Verdict::Transient, None);
        }
        if !self.acquired && self.recent.len() >= BASELINE {
            // The phase from a second's median of readings: the sink's own
            // start may have read the audio clock in its race.
            let m = median(&self.recent).unwrap_or(0.0);
            self.anchor_internal += m;
            for r in self.recent.iter_mut() {
                *r -= m;
            }
            self.acquired = true;
            self.period_from = None;
            self.period_sum = 0.0;
            self.period_count = 0;
            let cal = self.calibration();
            self.last_set = Some(cal);
            return (Verdict::Used, Some(cal));
        }
        if !self.acquired {
            return (Verdict::Used, None);
        }
        let from = *self.period_from.get_or_insert(at);
        self.period_sum += residual;
        self.period_count += 1;
        if at - from < PERIOD_NS {
            return (Verdict::Used, None);
        }
        // The period's phase, in seconds of the audio clock ahead (+) of
        // the model.
        let phase = self.period_sum / f64::from(self.period_count) / 1e9;
        self.frequency = (self.frequency + KI * phase).clamp(1.0 - RATE_RANGE, 1.0 + RATE_RANGE);
        let rate = (self.frequency + (KP * phase).clamp(-RATE_RANGE, RATE_RANGE)).clamp(1.0 - RATE_RANGE, 1.0 + RATE_RANGE);
        // Continuous where it is made: the new line goes through the old
        // one's point now.
        self.anchor_internal = self.internal_at(at);
        self.anchor_external = at;
        self.rate = rate;
        self.period_from = Some(at);
        self.period_sum = 0.0;
        self.period_count = 0;
        self.learned = true;
        self.updates += 1;
        // The residuals were against the old line; against the new one they
        // are as near: the baseline is kept.
        let cal = self.calibration();
        self.last_set = Some(cal);
        (Verdict::Used, Some(cal))
    }
}

fn median(v: &VecDeque<f64>) -> Option<f64> {
    if v.len() < 5 {
        return None;
    }
    let mut s: Vec<f64> = v.iter().copied().collect();
    s.sort_by(|a, b| a.total_cmp(b));
    Some(s[s.len() / 2])
}

// --------------------------------------------------------- the phase loop

/// What the phase loop decides for a buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    /// Resample by this ratio (output/input samples).
    Ratio(f64),
    /// The sound's times jumped: to the sink as a DISCONT, at its time.
    Discont,
}

/// For a buffer of `samples` at `rate`: its phase -- where it will be
/// written less where its time is, in samples; none when the sink will put
/// it at its time anyway.
pub fn decide(phase: Option<f64>, samples: usize, rate: f64) -> Decision {
    let Some(phase) = phase else { return Decision::Ratio(1.0) };
    if phase.abs() * 1e9 / rate > DISCONT_NS {
        return Decision::Discont;
    }
    let n = samples as f64;
    let buffer_ns = n * 1e9 / rate;
    let want = phase * buffer_ns / PHASE_TAU_NS;
    let most = RATE_RANGE * n;
    Decision::Ratio(1.0 - want.clamp(-most, most) / n)
}

// ----------------------------------------------------------- the resampler

/// Taps either side of the point resampled.
pub const HALF: usize = 32;
const PHASES: usize = 512;
/// The Kaiser window's beta: some 86 dB down outside the band.
const BETA: f64 = 8.6;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let mut k = 1.0;
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

/// Row q: the 2*HALF taps for the point a fraction q/PHASES past a sample,
/// each row summing to one.
fn table() -> &'static [f32] {
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let taps = 2 * HALF;
        let mut t = Vec::with_capacity((PHASES + 1) * taps);
        let i0b = bessel_i0(BETA);
        for q in 0..=PHASES {
            let frac = q as f64 / PHASES as f64;
            let row: Vec<f64> = (0..taps)
                .map(|j| {
                    let x = j as f64 - (HALF as f64 - 1.0) - frac;
                    let sinc = if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                    let r = x / HALF as f64;
                    let w = if r.abs() >= 1.0 { 0.0 } else { bessel_i0(BETA * (1.0 - r * r).sqrt()) / i0b };
                    sinc * w
                })
                .collect();
            let sum: f64 = row.iter().sum();
            t.extend(row.iter().map(|c| (c / sum) as f32));
        }
        t
    })
}

/// A resampler of interleaved frames by a ratio near one. Each output
/// sample is the band-limited value of the input at its position: no
/// sample repeated or dropped. The input is held `HALF` frames for the
/// taps after the point; the times come from the input's own.
#[derive(Debug, Clone)]
pub struct Resampler {
    channels: usize,
    rate: f64,
    fifo: Vec<f32>,
    /// Frames in the fifo (also when no samples are kept: `channels == 0`).
    frames: usize,
    /// The next output's position, frames into the fifo.
    pos: f64,
    /// Where each input buffer began in the fifo, and its time.
    marks: VecDeque<(f64, u64)>,
}

impl Resampler {
    /// `channels == 0`: positions and counts only, no samples (the replay).
    pub fn new(channels: usize, rate: f64) -> Self {
        let mut r = Self { channels, rate, fifo: Vec::new(), frames: 0, pos: 0.0, marks: VecDeque::new() };
        r.reset();
        r
    }

    /// Begins empty: silence before the first input, the first output its
    /// first sample. Returns the frames held that were not played.
    pub fn reset(&mut self) -> usize {
        let held = (self.frames as f64 - self.pos).max(0.0) as usize;
        let held = if self.marks.is_empty() { 0 } else { held };
        self.frames = HALF - 1;
        self.fifo.clear();
        self.fifo.resize(self.frames * self.channels, 0.0);
        self.pos = self.frames as f64;
        self.marks.clear();
        held
    }

    pub fn push(&mut self, input: &[f32], frames: usize, pts: u64) {
        self.marks.push_back((self.frames as f64, pts));
        self.fifo.extend_from_slice(input);
        self.frames += frames;
    }

    /// The input's time at fifo position `p`.
    fn time_at(&self, p: f64) -> Option<u64> {
        let (at, pts) = self.marks.iter().rev().find(|(at, _)| *at <= p + 1e-9).or(self.marks.front()).copied()?;
        let ns = pts as f64 + (p - at) * 1e9 / self.rate;
        Some(ns.round().max(0.0) as u64)
    }

    /// The time of the next output's first sample.
    pub fn next_time(&self) -> Option<u64> {
        self.time_at(self.pos)
    }

    /// Outputs while the taps after the point are in; returns how many.
    pub fn process(&mut self, ratio: f64, out: &mut Vec<f32>) -> usize {
        let step = 1.0 / ratio;
        let ch = self.channels;
        let taps = 2 * HALF;
        let table = table();
        let mut n = 0;
        let mut coeffs = [0.0f32; 2 * HALF];
        while (self.pos.floor() as usize) + HALF < self.frames {
            if ch > 0 {
                let i = self.pos.floor() as usize;
                let f = (self.pos - i as f64) * PHASES as f64;
                let q = (f.floor() as usize).min(PHASES - 1);
                let w = (f - q as f64) as f32;
                let a = &table[q * taps..(q + 1) * taps];
                let b = &table[(q + 1) * taps..(q + 2) * taps];
                for j in 0..taps {
                    coeffs[j] = a[j] + (b[j] - a[j]) * w;
                }
                let frames = &self.fifo[(i + 1 - HALF) * ch..(i + 1 + HALF) * ch];
                for c in 0..ch {
                    let mut acc = 0.0f32;
                    for (j, coeff) in coeffs.iter().enumerate() {
                        acc += frames[j * ch + c] * coeff;
                    }
                    out.push(acc);
                }
            }
            n += 1;
            self.pos += step;
        }
        // What no output needs any more.
        let keep_from = (self.pos.floor() as usize + 1).saturating_sub(HALF).min(self.frames);
        if keep_from > 0 {
            self.fifo.drain(..keep_from * ch);
            self.frames -= keep_from;
            self.pos -= keep_from as f64;
            for m in self.marks.iter_mut() {
                m.0 -= keep_from as f64;
            }
            while self.marks.len() > 1 && self.marks[1].0 <= 0.0 {
                self.marks.pop_front();
            }
        }
        n
    }
}

// ------------------------------------------------------------ the whole

/// A buffer of sound for the sink, as decoded.
pub struct BufferIn<'a> {
    pub generation: u64,
    pub pts: u64,
    pub frames: usize,
    /// Interleaved, as f32; empty when only counted (the replay).
    pub samples: &'a [f32],
    pub discont: bool,
}

/// What the sink knows when the buffer comes.
pub struct Sink<'a> {
    /// The audio clock's calibration as it stands.
    pub calibration: Calibration,
    pub observation: Observation,
    /// Where the sink will write the buffer (the end of the one before);
    /// none when it will put it at its time.
    pub next_sample: Option<f64>,
    /// Where the sink puts a time with a calibration, samples.
    pub place: &'a dyn Fn(u64, &Calibration) -> f64,
}

/// The buffer for the sink.
pub struct BufferOut {
    pub pts: u64,
    pub frames: usize,
    pub samples: Vec<f32>,
    pub discont: bool,
    /// A calibration for the audio clock, before the sink renders this.
    pub calibration: Option<Calibration>,
}

/// The controller of one sound device: the model, the phase loop and the
/// resampler, a buffer at a time.
pub struct Controller {
    pub model: Option<ClockModel>,
    resampler: Resampler,
    rate: f64,
    generation: u64,
    last_phase: Option<f64>,
    pub stats: std::sync::Arc<Stats>,
}

impl Controller {
    pub fn new(channels: usize, rate: f64, stats: std::sync::Arc<Stats>) -> Self {
        stats.rate_hz.store(rate as u32, Ordering::Relaxed);
        Self { model: None, resampler: Resampler::new(channels, rate), rate, generation: 0, last_phase: None, stats }
    }

    fn begin_again(&mut self) {
        let held = self.resampler.reset();
        self.stats.held_at_reset.fetch_add(held as u64, Ordering::Relaxed);
        self.stats.resets.fetch_add(1, Ordering::Relaxed);
        self.last_phase = None;
        self.stats.phase_max_ns.store(0, Ordering::Relaxed);
    }

    pub fn buffer(&mut self, input: BufferIn, sink: Sink) -> BufferOut {
        let s = std::sync::Arc::clone(&self.stats);
        if input.generation != self.generation || input.discont || sink.next_sample.is_none() {
            self.generation = input.generation;
            self.begin_again();
        }
        let mut calibration = sink.calibration;
        let model = self.model.get_or_insert_with(|| ClockModel::new(calibration));
        if model.last_set() != Some(calibration) {
            model.adopt(calibration);
            s.adopted.fetch_add(1, Ordering::Relaxed);
        }
        let (verdict, set) = model.observe(sink.observation);
        match verdict {
            Verdict::Used => &s.used,
            Verdict::NotOneMoment => &s.not_one_moment,
            Verdict::Transient => &s.transient,
        }
        .fetch_add(1, Ordering::Relaxed);
        if let Some(c) = set {
            calibration = c;
            s.updates.fetch_add(1, Ordering::Relaxed);
        }
        s.rate_ppb.store(((model.rate() - 1.0) * 1e9).round() as i64, Ordering::Relaxed);

        self.resampler.push(input.samples, input.frames, input.pts);
        let time = self.resampler.next_time().unwrap_or(input.pts);
        let phase = sink.next_sample.map(|next| next - (sink.place)(time, &calibration));
        let mut discont = input.discont;
        let ratio = match decide(phase, input.frames, self.rate) {
            Decision::Ratio(r) => r,
            Decision::Discont => {
                // Put at its time by the sink: what the resampler held of
                // before the jump is not played.
                self.begin_again();
                s.input_discont.fetch_add(1, Ordering::Relaxed);
                self.resampler.push(input.samples, input.frames, input.pts);
                discont = true;
                1.0
            }
        };
        let time = self.resampler.next_time().unwrap_or(input.pts);
        if let (Some(p), false) = (phase, discont) {
            let ns = p * 1e9 / self.rate;
            s.phase_ns.store(ns.round() as i64, Ordering::Relaxed);
            raise(&s.phase_max_ns, ns.abs().round() as i64);
            if let Some(last) = self.last_phase {
                raise(&s.phase_change_max_ns, ((p - last).abs() * 1e9 / self.rate).round() as i64);
            }
            self.last_phase = Some(p);
            raise(&s.stretch_max_millis, ((1.0 - ratio).abs() * input.frames as f64 * 1000.0).round() as i64);
        }
        let mut samples = Vec::with_capacity(input.samples.len() + 64);
        let frames = self.resampler.process(ratio, &mut samples);
        s.frames_in.fetch_add(input.frames as u64, Ordering::Relaxed);
        s.frames_out.fetch_add(frames as u64, Ordering::Relaxed);
        BufferOut { pts: time, frames, samples, discont, calibration: set }
    }
}

// ------------------------------------------------------------ the counts

/// The controller's counts for the health report.
#[derive(Debug, Default)]
pub struct Stats {
    pub used: AtomicU64,
    pub not_one_moment: AtomicU64,
    pub transient: AtomicU64,
    pub updates: AtomicU64,
    pub resets: AtomicU64,
    /// Calibrations found set by somebody else (audiobasesink's sync to the
    /// latency), taken.
    pub adopted: AtomicU64,
    /// Phases beyond `DISCONT_NS`: the sound's times jumped (an input's
    /// fault), put at their time by the sink.
    pub input_discont: AtomicU64,
    pub frames_in: AtomicU64,
    pub frames_out: AtomicU64,
    /// Frames held in the resampler when it began again, not played.
    pub held_at_reset: AtomicU64,
    /// The model's rate, parts per billion off one.
    pub rate_ppb: AtomicI64,
    /// The last phase and the largest since the last reset, ns.
    pub phase_ns: AtomicI64,
    pub phase_max_ns: AtomicI64,
    /// The largest change of phase from one buffer to the next, ns: the
    /// sound's moves and the calibration's own (its acquisition) together
    /// -- not the sound's step.
    pub phase_change_max_ns: AtomicI64,
    /// The largest stretch of one buffer, in thousandths of a sample: the
    /// step the product adds to the sound's place, the sink aligning both
    /// ends of every buffer.
    pub stretch_max_millis: AtomicI64,
    /// The sample rate the stretch is counted at.
    pub rate_hz: AtomicU32,
    pub unsupported: AtomicU64,
    pub unavailable: AtomicU64,
}

impl Stats {
    pub fn json(&self) -> serde_json::Value {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let i = |a: &AtomicI64| a.load(Ordering::Relaxed);
        serde_json::json!({
            "observations": { "used": g(&self.used), "not_one_moment": g(&self.not_one_moment), "transient": g(&self.transient) },
            "updates": g(&self.updates), "resets": g(&self.resets), "adopted": g(&self.adopted),
            "input_discont": g(&self.input_discont),
            "frames_in": g(&self.frames_in), "frames_out": g(&self.frames_out), "held_at_reset": g(&self.held_at_reset),
            "rate_ppb": i(&self.rate_ppb), "phase_ns": i(&self.phase_ns), "phase_max_ns": i(&self.phase_max_ns),
            "phase_change_max_ns": i(&self.phase_change_max_ns), "stretch_max_millisamples": i(&self.stretch_max_millis),
            "step_max_ns": i(&self.stretch_max_millis) * 1_000_000 / i64::from(self.rate_hz.load(Ordering::Relaxed).max(1)),
            "unsupported_format": g(&self.unsupported), "unavailable": g(&self.unavailable),
        })
    }
}

pub fn raise(a: &AtomicI64, v: i64) {
    a.fetch_max(v, Ordering::Relaxed);
}

// --------------------------------------------------------------- the sink

/// The sample formats the resampler takes (alsasink's on HDMI).
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
            _ => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub sample: Sample,
    pub rate: u32,
    pub channels: usize,
}

/// Interleaved little-endian PCM of `caps`, if the resampler takes it.
pub fn format_of(caps: &gst::CapsRef) -> Option<Format> {
    let s = caps.structure(0)?;
    if s.name() != "audio/x-raw" || s.get::<&str>("layout").ok() != Some("interleaved") {
        return None;
    }
    let sample = match s.get::<&str>("format").ok()? {
        "S16LE" => Sample::S16,
        "S32LE" => Sample::S32,
        "F32LE" => Sample::F32,
        _ => return None,
    };
    let rate = u32::try_from(s.get::<i32>("rate").ok()?).ok()?;
    let channels = usize::try_from(s.get::<i32>("channels").ok()?).ok()?;
    (rate > 0 && channels > 0).then_some(Format { sample, rate, channels })
}

pub fn to_f32(format: Format, data: &[u8]) -> Vec<f32> {
    match format.sample {
        Sample::S16 => data.as_chunks::<2>().0.iter().map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0).collect(),
        Sample::S32 => data.as_chunks::<4>().0.iter().map(|b| (f64::from(i32::from_le_bytes(*b)) / 2_147_483_648.0) as f32).collect(),
        Sample::F32 => data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
    }
}

pub fn from_f32(format: Format, samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * format.sample.bytes());
    for &v in samples {
        match format.sample {
            Sample::S16 => out.extend_from_slice(&((f64::from(v) * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes()),
            Sample::S32 => out.extend_from_slice(&((f64::from(v) * 2_147_483_648.0).round().clamp(-2_147_483_648.0, 2_147_483_647.0) as i32).to_le_bytes()),
            Sample::F32 => out.extend_from_slice(&v.to_le_bytes()),
        }
    }
    out
}

/// Where audiobasesink and its GstAudioClock keep, as public instance
/// fields, the next sample's place and the clock's offset
/// (gstaudiobasesink.h and gstaudioclock.h, GStreamer 1.26): past their
/// parents' structs. Found only when the fields around them read back as
/// the sink's own -- its buffer-time and latency-time, a clock whose user
/// data is the sink, and (at the first buffer, `provide_clock` giving none
/// before the ring is acquired) that clock the one the sink provides -- so
/// another layout is refused, never read.
#[derive(Debug, Clone, Copy)]
pub struct Fields {
    next_sample: usize,
    time_offset: usize,
    clock: usize,
}

pub fn fields(sink: &gst::Element) -> Option<Fields> {
    use gst::prelude::*;
    let audio_base_sink = gst::glib::Type::from_name("GstAudioBaseSink")?;
    if !sink.type_().is_a(audio_base_sink) {
        return None;
    }
    let base = std::mem::size_of::<gst_app::gst_base::ffi::GstBaseSink>();
    let clock_base = std::mem::size_of::<gst::ffi::GstSystemClock>();
    let p = sink.as_ptr() as *const u8;
    // SAFETY: the sink is alive (held here) and an audiobasesink's instance
    // is a GstBaseSink followed by its own fields.
    let (buffer_time, latency_time, clock) = unsafe {
        (
            std::ptr::read_unaligned(p.add(base + 8) as *const u64),
            std::ptr::read_unaligned(p.add(base + 16) as *const u64),
            std::ptr::read_unaligned(p.add(base + 32) as *const *const u8),
        )
    };
    let sinks = i64::try_from(buffer_time).ok() == Some(sink.property::<i64>("buffer-time"))
        && i64::try_from(latency_time).ok() == Some(sink.property::<i64>("latency-time"))
        && !clock.is_null();
    if !sinks {
        return None;
    }
    // SAFETY: the sink holds its clock from its init to its finalize; a
    // GstAudioClock's instance is a GstSystemClock followed by its own.
    let user_data = unsafe { std::ptr::read_unaligned(clock.add(clock_base + 8) as *const usize) };
    (user_data == sink.as_ptr() as usize).then_some(Fields { next_sample: base + 24, time_offset: clock_base + 32, clock: base + 32 })
}

/// The sink's clock as its field holds it: the one `provide_clock` gives
/// once its ring is acquired.
fn clock_field(sink: &gst::Element, f: Fields) -> usize {
    use gst::prelude::*;
    // SAFETY: as `fields`.
    unsafe { std::ptr::read_unaligned((sink.as_ptr() as *const u8).add(f.clock) as *const usize) }
}

fn next_sample(sink: &gst::Element, f: Fields) -> Option<f64> {
    use gst::prelude::*;
    // SAFETY: `fields` found the layout; the field is written by the
    // streaming thread this probe runs on.
    let v = unsafe { std::ptr::read_volatile((sink.as_ptr() as *const u8).add(f.next_sample) as *const u64) };
    (v != u64::MAX).then_some(v as f64)
}

fn time_offset(clock: &gst::Clock, f: Fields) -> i64 {
    use gst::prelude::*;
    // SAFETY: as `next_sample`; an aligned word, written whole.
    unsafe { std::ptr::read_volatile((clock.as_ptr() as *const u8).add(f.time_offset) as *const i64) }
}

/// The controller on alsasink's sink pad, before the sink: every buffer of
/// PCM resampled to its place, the audio clock's calibration set. False,
/// and nothing installed, when the sink's fields are not where they are
/// read from: the caller keeps GStreamer's own slaving then.
pub fn install(sink: &gst::Element, generation: impl Fn() -> u64 + Send + Sync + 'static, stats: std::sync::Arc<Stats>) -> bool {
    use gst::prelude::*;
    use gst_app::gst_base::prelude::*;
    let (Some(f), Some(pad)) = (fields(sink), sink.static_pad("sink")) else {
        stats.unavailable.fetch_add(1, Ordering::Relaxed);
        return false;
    };
    let weak = sink.downgrade();
    let pipeline_clock = gst::SystemClock::obtain();
    let state: std::sync::Mutex<Option<(Format, Controller)>> = std::sync::Mutex::new(None);
    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        let (Some(sink), Some(caps)) = (weak.upgrade(), pad.current_caps()) else {
            return gst::PadProbeReturn::Ok;
        };
        let Some(format) = format_of(&caps) else {
            stats.unsupported.fetch_add(1, Ordering::Relaxed);
            return gst::PadProbeReturn::Ok;
        };
        let Some(buffer) = info.buffer() else { return gst::PadProbeReturn::Ok };
        let (Some(pts), Some(audio_clock)) = (buffer.pts(), sink.provide_clock()) else {
            return gst::PadProbeReturn::Ok;
        };
        if audio_clock.as_ptr() as usize != clock_field(&sink, f) {
            // Not the layout read: GStreamer's own slaving, the probe gone.
            stats.unavailable.fetch_add(1, Ordering::Relaxed);
            eprintln!("mediabox-live: ses saati alanı sink'in saati değil; GStreamer resample kullanılıyor");
            sink.set_property_from_str("slave-method", "resample");
            return gst::PadProbeReturn::Remove;
        }
        let samples = match buffer.map_readable() {
            Ok(map) => to_f32(format, map.as_slice()),
            Err(_) => return gst::PadProbeReturn::Ok,
        };
        let frames = samples.len() / format.channels;
        let discont = buffer.flags().intersects(gst::BufferFlags::DISCONT | gst::BufferFlags::RESYNC);
        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
        if guard.as_ref().map(|(f, _)| *f) != Some(format) {
            // New caps: the resampler for them; the model, the device's,
            // kept.
            let model = guard.take().and_then(|(_, c)| c.model);
            let mut c = Controller::new(format.channels, f64::from(format.rate), std::sync::Arc::clone(&stats));
            c.model = model;
            *guard = Some((format, c));
        }
        let (_, control) = guard.as_mut().expect("set above");
        let (internal, external, num, denom) = audio_clock.calibration();
        let calibration = Calibration { internal: internal.nseconds(), external: external.nseconds(), num, denom };
        let before = pipeline_clock.time().nseconds();
        let read = audio_clock.internal_time().nseconds();
        let after = pipeline_clock.time().nseconds();
        // Where the sink puts a time (gstaudiobasesink.c render): running
        // time, the sync offset (ts-offset - render-delay + latency) and
        // base time, through the calibration, less the clock's offset.
        let segment = pad.sticky_event::<gst::event::Segment>(0);
        let segment = segment.as_ref().and_then(|e| e.segment().downcast_ref::<gst::ClockTime>().cloned());
        let base = sink.base_time().map_or(0, |b| b.nseconds()) as f64;
        let sync = sink.property::<i64>("ts-offset") as f64 - sink.property::<u64>("render-delay") as f64
            + sink.downcast_ref::<gst_app::gst_base::BaseSink>().map_or(0, |b| b.latency().nseconds()) as f64;
        let offset = time_offset(&audio_clock, f) as f64;
        let rate = f64::from(format.rate);
        let place = |t: u64, c: &Calibration| -> f64 {
            let running = segment.as_ref().and_then(|s| s.to_running_time(gst::ClockTime::from_nseconds(t))).map_or(t, |r| r.nseconds());
            ((c.to_internal(running as f64 + sync + base) - offset) * rate / 1e9).floor()
        };
        let out = control.buffer(
            BufferIn { generation: generation(), pts: pts.nseconds(), frames, samples: &samples, discont },
            Sink { calibration, observation: Observation { before, internal: read, after }, next_sample: next_sample(&sink, f), place: &place },
        );
        drop(guard);
        if let Some(c) = out.calibration {
            audio_clock.set_calibration(gst::ClockTime::from_nseconds(c.internal), gst::ClockTime::from_nseconds(c.external), c.num, c.denom);
        }
        if out.frames == 0 {
            return gst::PadProbeReturn::Drop;
        }
        let mut made = gst::Buffer::from_mut_slice(from_f32(format, &out.samples));
        {
            let made = made.get_mut().expect("a new buffer is writable");
            made.set_pts(gst::ClockTime::from_nseconds(out.pts));
            made.set_duration(gst::ClockTime::from_nseconds((out.frames as f64 * 1e9 / rate).round() as u64));
            if out.discont {
                made.set_flags(gst::BufferFlags::DISCONT);
            }
        }
        info.data = Some(gst::PadProbeData::Buffer(made));
        gst::PadProbeReturn::Ok
    });
    true
}

#[cfg(test)]
#[path = "audio_clock_tests.rs"]
mod tests;
