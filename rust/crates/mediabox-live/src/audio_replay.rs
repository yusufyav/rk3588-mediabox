//! A replay of the sound's clock and placement, in seconds of test: the
//! device's sample clock against CLOCK_MONOTONIC, the clock observations
//! taken of it, the calibration made of them and where audiobasesink puts
//! each buffer -- and so where each sample is heard against its time.
//!
//! The defect it reproduces (FAZ 2H, replayed against the logs in FAZ 2I):
//! with `slave-method=resample` the audio clock is slaved to the pipeline's
//! by GstClock (gstclock.c `gst_clock_slave_callback`): every 100 ms the
//! audio clock and then the master are read, the pair is an observation, and
//! a least-squares line through the last 32 (gstutils.c
//! `gst_calculate_linear_regression`, based at the newest) is the new
//! calibration. A thread held 20-45 ms between its two readings makes one
//! observation that far off; at the window's newest point its leverage on
//! the line is 0.119 and its pull on the slope 1100-2600 ppm, a 2.4-5.5 ms
//! jump of the calibration. audiobasesink aligns each buffer's start to the
//! one before and, resampling, not its end (gstaudiobasesink.c render: "no
//! stop time align needed: we are slaved"), so the whole jump goes into one
//! buffer: 1152 samples played in 991, the sound 3.35 ms early from there
//! on; and back the other way when the observation leaves the window 3.2 s
//! later.
//!
//! Times are f64 nanoseconds: CLOCK_MONOTONIC here (the pipeline's clock,
//! "external" in GstClock's words) and the audio clock's (its "internal",
//! the device's played samples).

/// The sample rate of the sound replayed.
pub const RATE: f64 = 48_000.0;
/// One MPEG-1 Layer II frame.
pub const FRAME: usize = 1152;
pub const FRAME_NS: f64 = FRAME as f64 * 1e9 / RATE;
/// audiobasesink's alignment-threshold (default 40 ms), in samples.
pub const ALIGN_SAMPLES: f64 = 0.040 * RATE;
/// The device's crystal against the NTP-steered CLOCK_MONOTONIC (FAZ 1M:
/// -11.8 ppm).
pub const CRYSTAL: f64 = 1.0 - 11.8e-6;

/// The device: its internal time at an external one.
#[derive(Debug, Clone, Copy)]
pub struct Device {
    pub at: f64,
    pub internal: f64,
    pub rate: f64,
}

impl Device {
    pub fn internal(&self, external: f64) -> f64 {
        self.internal + (external - self.at) * self.rate
    }
}

/// A GstClock calibration: external = (internal - internal0) * num/denom + external0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cal {
    pub internal: f64,
    pub external: f64,
    /// d external / d internal.
    pub slope: f64,
}

impl Cal {
    /// gstaudiobasesink.c `clock_convert_external`.
    pub fn to_internal(self, external: f64) -> f64 {
        self.internal + (external - self.external) / self.slope
    }
}

/// gstutils.c `gst_calculate_linear_regression`: the line through the
/// points' centroid, reported at the largest x.
pub fn regression(points: &[(f64, f64)]) -> Cal {
    let n = points.len() as f64;
    let xmin = points.iter().map(|p| p.0).fold(f64::MAX, f64::min);
    let ymin = points.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    let xbar = points.iter().map(|p| p.0 - xmin).sum::<f64>() / n;
    let ybar = points.iter().map(|p| p.1 - ymin).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|p| (p.0 - xmin - xbar).powi(2)).sum();
    let sxy: f64 = points.iter().map(|p| (p.0 - xmin - xbar) * (p.1 - ymin - ybar)).sum();
    let m = sxy / sxx;
    let xmax = points.iter().map(|p| p.0).fold(f64::MIN, f64::max);
    Cal { internal: xmax, external: ymin + ybar + (xmax - xmin - xbar) * m, slope: m }
}

/// GstClock's slaving of the audio clock (`gst_clock_set_master`): an
/// observation every `timeout` (100 ms), the regression over the last
/// `window_size` (32) once there are `window_threshold` (4).
#[derive(Debug, Default)]
pub struct GstSlave {
    points: std::collections::VecDeque<(f64, f64)>,
}

impl GstSlave {
    pub const PERIOD_NS: f64 = 100e6;
    const WINDOW: usize = 32;
    const THRESHOLD: usize = 4;

    pub fn observe(&mut self, internal: f64, external: f64) -> Option<Cal> {
        self.points.push_back((internal, external));
        if self.points.len() > Self::WINDOW {
            self.points.pop_front();
        }
        (self.points.len() >= Self::THRESHOLD).then(|| regression(self.points.make_contiguous()))
    }
}

/// What the sink was given and where it put it.
#[derive(Debug, Clone, Copy)]
pub struct Placed {
    /// The buffer's content's time: running time plus base time, external.
    pub external: f64,
    pub samples_in: usize,
    pub samples_out: usize,
    /// Its first sample's place in the ring, samples.
    pub start: f64,
    /// How early (-) or late (+) its first sample is heard against its
    /// time, ns: where the device plays it less where it should.
    pub error_ns: f64,
}

/// audiobasesink's placement of a buffer (gstaudiobasesink.c render and
/// `gst_audio_base_sink_get_alignment`): its start from its time through
/// the calibration, aligned to the end of the buffer before when within
/// the alignment-threshold; its end aligned too, unless resampling.
#[derive(Debug, Default)]
pub struct Sink {
    pub next: Option<f64>,
    pub resample: bool,
}

impl Sink {
    pub fn place(&mut self, external: f64, samples: usize, cal: &Cal, device: &Device) -> Placed {
        let offset = (cal.to_internal(external) * RATE / 1e9).round();
        let stop = offset + samples as f64;
        let (start, stop) = match self.next {
            Some(next) if (next - offset).abs() < ALIGN_SAMPLES => {
                let align = next - offset;
                (next, if self.resample { stop } else { stop + align })
            }
            _ => (offset, stop),
        };
        self.next = Some(stop);
        let heard = start * 1e9 / RATE;
        Placed { external, samples_in: samples, samples_out: (stop - start) as usize, start, error_ns: heard - device.internal(external) }
    }
}

/// The steps of the sound's place from one buffer to the next, ns.
pub fn steps(placed: &[Placed]) -> Vec<f64> {
    placed.windows(2).map(|w| w[1].error_ns - w[0].error_ns).collect()
}

pub fn largest_step(placed: &[Placed]) -> f64 {
    steps(placed).iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// A disturbance of the replay.
#[derive(Debug, Clone, Copy)]
pub enum Fault {
    /// The master read this long after the audio clock, at this time.
    LateMasterRead { at: f64, by: f64 },
    /// The audio clock read this far off at this time (one segment: the
    /// reading race of `gstaudiosink.c`, FAZ 2G).
    Race { at: f64, by: f64 },
    /// The calibration moved by this much of external time, at this time.
    CalStep { at: f64, by: f64 },
}

/// The sound with `slave-method=resample` as audiobasesink and GstClock do
/// it, for `seconds`, one MP2 frame after another; the sink 100 ms ahead of
/// the device. `regress`: GstClock's slaving on (the product) or the
/// calibration held as set at the start (to see a step alone).
pub fn run_resample(seconds: f64, device: Device, faults: &[Fault], regress: bool) -> Vec<Placed> {
    let start = device.at;
    let mut cal = Cal { internal: device.internal(start), external: start, slope: 1.0 / device.rate };
    let mut slave = GstSlave::default();
    let mut sink = Sink { next: None, resample: true };
    let mut placed = Vec::new();
    let mut next_observation = start;
    let mut done: Vec<bool> = vec![false; faults.len()];
    let mut k = 0usize;
    loop {
        let external = start + 100e6 + k as f64 * FRAME_NS;
        if external > start + seconds * 1e9 {
            break;
        }
        // The calibration as it stands when this buffer is rendered, 100 ms
        // before its time.
        let now = external - 100e6;
        while regress && next_observation <= now {
            let t = next_observation;
            let mut internal = device.internal(t);
            let mut master = t;
            for f in faults {
                match *f {
                    Fault::LateMasterRead { at, by } if (at - t).abs() < GstSlave::PERIOD_NS / 2.0 => master += by,
                    Fault::Race { at, by } if (at - t).abs() < GstSlave::PERIOD_NS / 2.0 => internal += by,
                    _ => {}
                }
            }
            if let Some(c) = slave.observe(internal, master) {
                cal = c;
            }
            next_observation += GstSlave::PERIOD_NS;
        }
        for (f, d) in faults.iter().zip(done.iter_mut()) {
            if let Fault::CalStep { at, by } = *f
                && !*d
                && at <= now
            {
                cal.external += by;
                *d = true;
            }
        }
        placed.push(sink.place(external, FRAME, &cal, &device));
        k += 1;
    }
    placed
}

/// The faults the controller's replay takes as well.
#[derive(Debug, Clone, Copy)]
pub enum Input {
    /// The sound's own times jump by this much at this time (a frame lost).
    Jump { at: f64, by: f64 },
    /// Another channel at this time: times from `base` on, and the sink's
    /// start over -- its calibration set again by its sync to the latency,
    /// `off` from the device's truth (a reading in the race).
    Channel { at: f64, base: f64, off: f64 },
}

/// The sound with the player's own controller (`audio_clock::Controller`,
/// `slave-method=custom`): the same device and faults, the sink aligning
/// each buffer's start and end, the resampler counted.
pub fn run_controlled(seconds: f64, device: Device, faults: &[Fault], inputs: &[Input]) -> (Vec<Placed>, std::sync::Arc<crate::audio_clock::Stats>) {
    use crate::audio_clock::{BufferIn, Calibration, Controller, Observation, Sink as SinkNow, Stats};
    let start = device.at;
    let stats = std::sync::Arc::new(Stats::default());
    let mut control = Controller::new(0, RATE, std::sync::Arc::clone(&stats));
    let cal_of = |internal: f64, external: f64| Calibration { internal: internal.round() as u64, external: external.round() as u64, num: 1, denom: 1 };
    let mut clock = cal_of(device.internal(start), start);
    let mut sink = Sink { next: None, resample: false };
    let mut placed = Vec::new();
    let mut done = vec![false; faults.len() + inputs.len()];
    // The sound's own times, and what makes them the pipeline's (base time
    // and running time): another channel's begin elsewhere.
    let mut times = start + 100e6;
    let mut shift = 0.0;
    let mut generation = 1;
    let mut k = 0usize;
    loop {
        // When this buffer comes to the sink, 100 ms before its time.
        let now = start + k as f64 * FRAME_NS;
        if now > start + seconds * 1e9 {
            break;
        }
        let mut discont = false;
        for (i, f) in inputs.iter().enumerate() {
            match *f {
                Input::Jump { at, by } if !done[faults.len() + i] && at <= now => {
                    times += by;
                    discont = false;
                    done[faults.len() + i] = true;
                }
                Input::Channel { at, base, off } if !done[faults.len() + i] && at <= now => {
                    shift = now + 100e6 - base;
                    times = base;
                    generation += 1;
                    sink.next = None;
                    // The sink's start: its calibration at the device's
                    // reading, `off` in the race, the rate kept.
                    clock = Calibration { internal: (device.internal(now) + off).round() as u64, external: now.round() as u64, ..clock };
                    done[faults.len() + i] = true;
                }
                _ => {}
            }
        }
        let (mut span, mut race) = (2e3, 0.0);
        for (i, f) in faults.iter().enumerate() {
            match *f {
                Fault::LateMasterRead { at, by } if (at - now).abs() < FRAME_NS / 2.0 => span = by,
                Fault::Race { at, by } if (at - now).abs() < FRAME_NS / 2.0 => race = by,
                Fault::CalStep { at, by } if !done[i] && at <= now => {
                    clock.external = (clock.external as f64 + by).round() as u64;
                    done[i] = true;
                }
                _ => {}
            }
        }
        let observation = Observation { before: now as u64, internal: (device.internal(now) + race).round() as u64, after: (now + span) as u64 };
        let place = |t: u64, c: &Calibration| c.to_internal(t as f64 + shift) * RATE / 1e9;
        let sample = |t: u64, c: &Calibration| place(t, c).round() as i64;
        let out = control.buffer(
            BufferIn { generation, pts: times as u64, frames: FRAME, samples: &[], discont },
            SinkNow { calibration: clock, observation, next_sample: sink.next, place: &place, sample: &sample },
        );
        if let Some(c) = out.calibration {
            clock = c;
        }
        if out.discont {
            sink.next = None;
        }
        let cal = Cal { internal: clock.internal as f64, external: clock.external as f64, slope: 1.0 / clock.rate() };
        if out.frames > 0 {
            // The sink puts the time it is stamped with; its sound's own is
            // what it is heard against.
            let mut p = sink.place(out.pts as f64 + shift, out.frames, &cal, &device);
            p.external = out.content as f64 + shift;
            p.error_ns = p.start * 1e9 / RATE - device.internal(p.external);
            placed.push(p);
        }
        times += FRAME_NS;
        k += 1;
    }
    (placed, stats)
}

/// audiobasesink as the product runs it (`slave-method=custom`, no
/// callback; gstaudiobasesink.c 1.26 render and
/// `gst_audio_base_sink_get_alignment`): the buffer's sample from its time
/// in the sink's own arithmetic (`audio_clock::sink_sample`), aligned to
/// the end of the buffer before when within the alignment-threshold --
/// unless that writes it back behind the read segment (`headroom`
/// samples): then it is put at its own sample, a resync.
#[derive(Debug, Default)]
pub struct GstSink {
    pub next: Option<i64>,
    pub headroom: i64,
    /// Each resync: its sample less the end of the buffer before.
    pub resyncs: Vec<i64>,
    /// Each alignment the sink made: the end of the buffer before less the buffer's sample.
    pub aligns: std::collections::BTreeMap<i64, u64>,
}

impl GstSink {
    /// The sample of a time (running time, sync offset and base time) as
    /// gstaudiobasesink.c 1.26 renders it with no slaving of its own:
    /// `clock_convert_external` (`gst_util_uint64_scale`, rounding down),
    /// the clock's time offset 0, then `gst_util_uint64_scale_int` to the
    /// rate. The test's own: not the product's (`audio_clock::sink_sample`).
    pub fn sample_of(external: u64, c: &crate::audio_clock::Calibration) -> i64 {
        let scale = |v: u64, num: u64, denom: u64| (u128::from(v) * u128::from(num) / u128::from(denom)) as u64;
        let (num, denom) = if c.num == 0 { (1, 1) } else { (c.num, c.denom) };
        let internal = if external >= c.external {
            scale(external - c.external, denom, num) + c.internal
        } else {
            c.internal.saturating_sub(scale(c.external - external, denom, num))
        };
        scale(internal, RATE as u64, 1_000_000_000) as i64
    }

    pub fn render(&mut self, sample: i64, samples: usize, discont: bool) -> i64 {
        let start = match self.next {
            Some(next) if !discont => {
                let align = next - sample;
                *self.aligns.entry(align).or_default() += 1;
                let allow = !(align.abs() > self.headroom && align < 0);
                if align.abs() < ALIGN_SAMPLES as i64 && allow {
                    next
                } else {
                    self.resyncs.push(-align);
                    sample
                }
            }
            _ => sample,
        };
        self.next = Some(start + samples as i64);
        start
    }
}

/// A replay through the product's controller to `GstSink`.
pub struct GstRun {
    pub placed: Vec<Placed>,
    pub resyncs: Vec<i64>,
    pub aligns: std::collections::BTreeMap<i64, u64>,
    /// Each buffer's first sample's own time and the time it is stamped with.
    pub stamped: Vec<(u64, u64)>,
    pub stats: std::sync::Arc<crate::audio_clock::Stats>,
}

/// The sound as a parser gives it: PES of `per_pes` frames of `frame`
/// samples, each frame's time its PES's time on the clock plus its place
/// in it -- the PES's own time put on the clock off the frames' by
/// `jitter` ns (the reader's mapping of the PTS through the PCR in force,
/// a PES after another), in turn; `jumps` (at, by ns) move the sound's
/// times (a frame lost), a DISCONT on the buffer there. The sink
/// `headroom` samples ahead of its read segment; the device's crystal off
/// the pipeline's clock.
pub fn run_on_gst_sink(seconds: f64, device: Device, frame: usize, per_pes: usize, jitter: &[i64], headroom: i64, jumps: &[(f64, i64)]) -> GstRun {
    use crate::audio_clock::{BufferIn, Calibration, Controller, Observation, Sink as SinkNow, Stats, sink_sample};
    let start = device.at;
    let frame_ns = (frame as u64 * 1_000_000_000) / RATE as u64;
    let stats = std::sync::Arc::new(Stats::default());
    let mut control = Controller::new(0, RATE, std::sync::Arc::clone(&stats));
    let mut clock = Calibration { internal: device.internal(start).round() as u64, external: start.round() as u64, num: 1, denom: 1 };
    let mut sink = GstSink { headroom, ..GstSink::default() };
    let (mut placed, mut stamped) = (Vec::new(), Vec::new());
    // The sound's times: base and running time make them the pipeline's.
    let shift: i128 = 100_000_000;
    let mut pes_at = start as u64 + 100_000_000;
    let mut moved = 0i64;
    let mut done = vec![false; jumps.len()];
    let mut k = 0usize;
    'run: loop {
        let pes = k / per_pes;
        let now = start + k as f64 * frame_ns as f64;
        if now > start + seconds * 1e9 {
            break 'run;
        }
        let mut discont = false;
        for (i, &(at, by)) in jumps.iter().enumerate() {
            if !done[i] && at <= now {
                moved += by;
                done[i] = true;
                discont = true;
            }
        }
        let content = (pes_at as i64 + moved + jitter.get(pes % jitter.len().max(1)).copied().unwrap_or(0)) as u64 + (k % per_pes) as u64 * frame_ns;
        let observation = Observation { before: now as u64, internal: device.internal(now).round() as u64, after: now as u64 };
        let place = |t: u64, c: &Calibration| c.to_internal((t as i128 + shift) as f64) * RATE / 1e9;
        let sample = |t: u64, c: &Calibration| sink_sample(t as i128 + shift, c, 0, RATE as u64);
        let out = control.buffer(
            BufferIn { generation: 1, pts: content, frames: frame, samples: &[], discont },
            SinkNow { calibration: clock, observation, next_sample: sink.next.map(|n| n as f64), place: &place, sample: &sample },
        );
        if let Some(c) = out.calibration {
            clock = c;
        }
        if out.frames > 0 {
            let sample = GstSink::sample_of((out.pts as i128 + shift) as u64, &clock);
            let at = sink.render(sample, out.frames, out.discont);
            let external = (out.content as i128 + shift) as f64;
            stamped.push((out.content, out.pts));
            placed.push(Placed { external, samples_in: frame, samples_out: out.frames, start: at as f64, error_ns: at as f64 * 1e9 / RATE - device.internal(external) });
        }
        k += 1;
        if k.is_multiple_of(per_pes) {
            pes_at += per_pes as u64 * frame_ns;
        }
    }
    GstRun { placed, resyncs: sink.resyncs, aligns: sink.aligns, stamped, stats }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: f64 = 67_000e9;

    fn device() -> Device {
        Device { at: T0, internal: 120e9, rate: CRYSTAL }
    }

    /// The project's acceptance limit for a step the product adds to the
    /// sound's place in one buffer (a product gate, not a standard's).
    const GATE_NS: f64 = 2e6;

    #[test]
    fn a_calibration_step_goes_into_one_buffer_when_resampling() {
        // The steps seen on the device, as calibration moves held alone.
        let faults: Vec<Fault> =
            [2.4e6, 3.35e6, 4.29e6, 5.5e6].iter().enumerate().map(|(i, &by)| Fault::CalStep { at: T0 + (10.0 + 10.0 * i as f64) * 1e9, by }).collect();
        let placed = run_resample(60.0, Device { rate: 1.0, ..device() }, &faults, false);
        let squeezed: Vec<usize> = placed.iter().filter(|p| p.samples_out != p.samples_in).map(|p| p.samples_out).collect();
        // FAZ 2H's 1152/991 (3.35 ms) and 1152/946 (4.29 ms), and the others.
        assert_eq!(squeezed, vec![1037, 991, 946, 888]);
        let steps: Vec<f64> = steps(&placed).into_iter().filter(|s| s.abs() > 1e3).collect();
        assert_eq!(steps.len(), 4);
        for (s, by) in steps.iter().zip([2.4e6, 3.35e6, 4.29e6, 5.5e6]) {
            assert!((s + by).abs() < 21e3, "a step of {s} ns for {by}");
        }
        assert!(largest_step(&placed) > GATE_NS, "the old slaving breaks the product's gate");
        // Written without a gap or an overlap: the step is the squeeze.
        for w in placed.windows(2) {
            assert_eq!(w[1].start, w[0].start + w[0].samples_out as f64);
            assert_eq!(w[1].external - w[0].external, FRAME_NS);
        }
        // And it stays: the sound is early from there on.
        assert!((placed.last().unwrap().error_ns + 15.54e6).abs() < 0.1e6, "{}", placed.last().unwrap().error_ns);
    }

    #[test]
    fn one_late_master_read_moves_gstclocks_calibration_by_its_leverage() {
        // The three steps solved from the FAZ 2H logs: the master read
        // 20.31, 27.30 and 45.52 ms after the audio clock gave 2.44, 3.10
        // and 5.48 ms (`gst-1854012`, `gst-1859992`, `gst-1840837`).
        for (by, seen) in [(20.31e6, 2.442e6), (27.30e6, 3.095e6), (45.52e6, 5.478e6)] {
            let at = T0 + 30.02e9;
            let placed = run_resample(40.0, device(), &[Fault::LateMasterRead { at, by }], true);
            let worst = steps(&placed).into_iter().fold(0.0f64, |m, s| if s.abs() > m.abs() { s } else { m });
            // The leverage of the newest of 32 evenly spaced points on the
            // line at that point: 1/32 + 15.5^2 / 2728.
            let leverage = 1.0 / 32.0 + 15.5f64.powi(2) / 2728.0;
            assert!((leverage - 0.119).abs() < 1e-3);
            // Plus the slope's pull over the time the calibration is used
            // ahead of its newest point (up to 200 ms here): within 0.3 ms.
            assert!((worst.abs() - by * leverage).abs() < 0.3e6, "{worst} for {by}");
            // The device's step against the replay's: the replay's buffers
            // run 100 ms ahead of their time, the device's as they came.
            assert!((worst.abs() - seen).abs() < 0.5e6, "{worst} against the device's {seen}");
            assert!(worst.abs() > GATE_NS);
            // The observation leaving the window 3.2 s on steps the sound
            // again, the same way: as the oldest point it had tilted the
            // line the other way (the device: 1152/863 at 153.7 s, then
            // 1152/993 at 156.9 s, `gst-1840837`).
            let both: Vec<(usize, f64)> = steps(&placed).into_iter().enumerate().filter(|(_, s)| s.abs() > 0.5e6).collect();
            assert_eq!(both.len(), 2, "{both:?}");
            assert_eq!(both[0].1.signum(), both[1].1.signum());
            assert!(both[1].1.abs() > 0.04 * by && both[1].1.abs() < 0.09 * by, "{both:?}");
            let apart = (both[1].0 - both[0].0) as f64 * FRAME_NS;
            assert!((apart - 3.2e9).abs() < 0.15e9, "{apart}");
        }
    }

    #[test]
    fn a_reading_race_of_one_segment_reaches_the_sound_through_gstclock() {
        let placed = run_resample(40.0, device(), &[Fault::Race { at: T0 + 30.02e9, by: -10e6 }], true);
        let worst = largest_step(&placed);
        // Under the gate, but a transient reading put into the sound's
        // place: 0.119 of it.
        assert!((worst - 10e6 * 0.119).abs() < 0.1e6, "{worst}");
    }

    // ------------------------------------------------ the player's own

    use crate::audio_clock::STEP_BUDGET_NS;
    use std::sync::atomic::Ordering;

    /// The sound's place over the last of the replay: within this of its
    /// time, ns.
    fn settled(placed: &[Placed], from: f64) -> f64 {
        placed.iter().filter(|p| p.external >= from).fold(0.0, |m, p| m.max(p.error_ns.abs()))
    }

    #[test]
    fn the_controller_takes_the_same_calibration_steps_without_a_step() {
        let faults: Vec<Fault> =
            [2.4e6, 3.35e6, 4.29e6, 5.5e6].iter().enumerate().map(|(i, &by)| Fault::CalStep { at: T0 + (30.0 + 30.0 * i as f64) * 1e9, by }).collect();
        let (placed, stats) = run_controlled(240.0, device(), &faults, &[]);
        assert!(largest_step(&placed) < STEP_BUDGET_NS, "{}", largest_step(&placed));
        assert!(placed.iter().all(|p| (p.samples_out as f64 - p.samples_in as f64).abs() <= 2.0), "no buffer squeezed");
        assert_eq!(stats.adopted.load(Ordering::Relaxed), 4);
        // Back on its time once the readings have the phase again.
        assert!(settled(&placed, T0 + 200e9) < 0.1e6, "{}", settled(&placed, T0 + 200e9));
    }

    #[test]
    fn late_master_reads_and_reading_races_leave_the_sound_where_it_was() {
        let faults = [
            Fault::LateMasterRead { at: T0 + 40.01e9, by: 20.31e6 },
            Fault::LateMasterRead { at: T0 + 50.01e9, by: 27.30e6 },
            Fault::LateMasterRead { at: T0 + 60.01e9, by: 45.52e6 },
            Fault::Race { at: T0 + 70.01e9, by: -10e6 },
            Fault::Race { at: T0 + 80.01e9, by: 10e6 },
        ];
        let (clean, _) = run_controlled(120.0, device(), &[], &[]);
        let (placed, stats) = run_controlled(120.0, device(), &faults, &[]);
        assert_eq!(stats.not_one_moment.load(Ordering::Relaxed), 3);
        assert_eq!(stats.transient.load(Ordering::Relaxed), 2);
        // Not a microsecond apart from the replay without them.
        let apart = clean.iter().zip(&placed).fold(0.0f64, |m, (a, b)| m.max((a.error_ns - b.error_ns).abs()));
        assert!(apart < 1e3, "{apart}");
    }

    #[test]
    fn the_crystal_is_followed_without_a_sample_repeated_and_without_drift() {
        // Half an hour at -11.8 ppm, then the NTP-steered clock moved to
        // +5 ppm.
        let (placed, stats) = run_controlled(1800.0, device(), &[], &[]);
        assert!(largest_step(&placed) < 50e3, "{}", largest_step(&placed));
        assert!(settled(&placed, T0 + 120e9) < 0.1e6, "{}", settled(&placed, T0 + 120e9));
        assert!(settled(&placed, T0 + 1500e9) < 0.05e6, "no drift: {}", settled(&placed, T0 + 1500e9));
        let rate = stats.rate_ppb.load(Ordering::Relaxed) as f64;
        assert!((rate + 11_800.0).abs() < 300.0, "the crystal learned: {rate} ppb");
        let (moved, _) = run_controlled(600.0, Device { rate: 1.0 + 5e-6, ..device() }, &[], &[]);
        assert!(settled(&moved, T0 + 300e9) < 0.1e6);
    }

    #[test]
    fn a_jump_in_the_sounds_times_is_put_at_its_time_and_counted_apart() {
        // A frame lost on the way: the times 24 ms on.
        let (placed, stats) = run_controlled(60.0, device(), &[], &[Input::Jump { at: T0 + 30e9, by: FRAME_NS }]);
        assert_eq!(stats.input_discont.load(Ordering::Relaxed), 1);
        assert_eq!(stats.channel_starts.load(Ordering::Relaxed), 0);
        assert!(settled(&placed, T0 + 31e9) < 0.1e6, "{}", settled(&placed, T0 + 31e9));
    }

    #[test]
    fn another_channel_begins_the_phase_and_the_resampler_again() {
        // A channel whose times begin far from the last's, its sink's start
        // calibrated 7 ms off in the race.
        let at = T0 + 30e9;
        let (placed, stats) = run_controlled(90.0, device(), &[], &[Input::Channel { at, base: 900_000e9, off: 7e6 }]);
        // The buffer the change comes with, then the new channel's: its
        // first sample first, at the pipeline's time for it -- nothing of
        // the last channel's held in the resampler before it.
        let begins = T0 + ((at - T0) / FRAME_NS).ceil() * FRAME_NS + 100e6;
        let k = placed.iter().position(|p| p.external >= begins - 1.0).unwrap();
        assert!((placed[k].external - begins).abs() < 1.0, "{:?}", &placed[k - 1..=k]);
        assert!(placed[k - 1].external < at + 100e6);
        assert!(stats.resets.load(Ordering::Relaxed) >= 2);
        // Another channel is no fault of the input's; nothing of the last
        // channel's lost but the resampler's taps.
        assert_eq!(stats.input_discont.load(Ordering::Relaxed), 0);
        assert!(stats.held_at_reset.load(Ordering::Relaxed) <= 2 * crate::audio_clock::HALF as u64);
        // Put where the sink's start calibration says (7 ms off), then the
        // readings' median has the phase within a second and the bound takes
        // the rest without a step.
        assert!((placed[k].error_ns - 7e6).abs() < 0.1e6, "{}", placed[k].error_ns);
        assert!(largest_step(&placed[k..]) < STEP_BUDGET_NS, "{}", largest_step(&placed[k..]));
        assert!(settled(&placed, at + 15e9) < 0.1e6, "{}", settled(&placed, at + 15e9));
    }

    #[test]
    fn the_crystal_alone_is_followed_by_samples_repeated_or_dropped() {
        // No fault: -11.8 ppm, followed by stretching single buffers by a
        // sample -- audiobasesink's ring buffer repeats or drops a sample
        // for it (gstaudioringbuffer.c, FWD_UP/DOWN_SAMPLES).
        let placed = run_resample(120.0, device(), &[], true);
        let stretched = placed.iter().skip(200).filter(|p| p.samples_out != p.samples_in).count();
        assert!(stretched > 0);
        assert!(largest_step(&placed[200..]) < 0.1e6);
    }

    // ------------------------------------------- on audiobasesink's own

    /// The sound's place against the device over a window of the replay:
    /// the largest and the mean error, ns.
    fn window(placed: &[Placed], from: f64, to: f64) -> (f64, f64) {
        let w: Vec<f64> = placed.iter().filter(|p| p.external >= from && p.external < to).map(|p| p.error_ns).collect();
        (w.iter().fold(0.0f64, |m, e| m.max(e.abs())), w.iter().sum::<f64>() / w.len().max(1) as f64)
    }

    /// FAZ 3E.6/3E.7: on the headset (no headroom before the read segment)
    /// the sink resynced by one sample, at PES starts (+20.833 us,
    /// `p-g`: 123 in 90 s). The controller stamped a buffer with its
    /// sound's own time and left the sample between that and the end of
    /// the buffer before for the sink to align back -- which the sink does
    /// not do without headroom: it resyncs, a sample of nothing. Now each
    /// buffer is stamped where the sink writes it, the end of the one
    /// before to the fraction carried, its phase the resampler's: every
    /// alignment 0, no resync, the sound's place not drifting off.
    ///
    /// The old controller, on the same replay (2026-11-10): 170 resyncs in
    /// 300 s for a crystal 11.8 ppm fast, 231-346 with PES times off their
    /// frames' by 15 us; each +1 sample.
    #[test]
    fn a_buffer_is_written_where_the_one_before_ends_and_never_left_for_the_sink_to_align_back() {
        for rate in [1.0 - 11.8e-6, 1.0 + 11.8e-6, 1.0 + 100e-6, 1.0] {
            for jitter in [vec![0i64], vec![7_000], vec![0, 3_000, -2_000, 1_000, -3_000, 2_000], vec![0, 15_000, -10_000]] {
                for (frame, per) in [(1536usize, 9usize), (1152, 9), (1152, 1)] {
                    let r = run_on_gst_sink(300.0, Device { rate, ..device() }, frame, per, &jitter, 0, &[]);
                    let case = format!("crystal {rate} jitter {jitter:?} frame {frame}x{per}");
                    assert_eq!(r.resyncs, Vec::<i64>::new(), "{case}");
                    assert_eq!(r.aligns.keys().copied().collect::<Vec<_>>(), vec![0], "{case}: {:?}", r.aligns);
                    assert_eq!(r.stats.unplaced.load(Ordering::Relaxed), 0, "{case}");
                    // Written without a gap or an overlap, sample after sample.
                    for w in r.placed.windows(2) {
                        assert_eq!(w[1].start, w[0].start + w[0].samples_out as f64, "{case}");
                    }
                    // The sound where its time is, and staying there: the
                    // last minute no further off than the second.
                    let (early, _) = window(&r.placed, T0 + 60e9, T0 + 120e9);
                    let (late, mean) = window(&r.placed, T0 + 240e9, T0 + 300e9);
                    assert!(late <= early.max(1e9 / RATE) + 1e9 / RATE, "{case}: {early} then {late}");
                    assert!(mean.abs() < 2.0 * 1e9 / RATE, "{case}: mean {mean}");
                    assert!(r.stats.stretch_max_millis.load(Ordering::Relaxed) <= (RATE_RANGE * frame as f64 * 1000.0).ceil() as i64, "{case}");
                }
            }
        }
    }

    use crate::audio_clock::RATE_RANGE;

    /// With the device's clock the pipeline's, the time stamped on each
    /// buffer is its sound's own to the nanosecond: the fraction carried is
    /// the sound's, not reset to the sample nor kept across a DISCONT --
    /// after a jump of 14.4 samples, and one of 50 ms, the times are the
    /// new ones.
    #[test]
    fn with_no_drift_each_buffer_keeps_its_own_time_across_a_discont() {
        let jumps = [(T0 + 20e9, 300_007i64), (T0 + 40e9, 50_000_000)];
        for (frame, per) in [(1152usize, 9usize), (1536, 9)] {
            let r = run_on_gst_sink(60.0, Device { rate: 1.0, ..device() }, frame, per, &[7_000], 0, &jumps);
            // To the nanosecond the resampler rounds its times to.
            for (k, (content, stamped)) in r.stamped.iter().enumerate() {
                assert!(content.abs_diff(*stamped) <= 1, "buffer {k} of {frame}: {content} stamped {stamped}");
            }
            // Not a sample stretched: no phase made up where there is none.
            assert_eq!(r.stats.stretch_max_millis.load(Ordering::Relaxed), 0);
            assert_eq!(r.resyncs, Vec::<i64>::new());
            assert_eq!(r.aligns.keys().copied().collect::<Vec<_>>(), vec![0]);
        }
    }

    /// A DISCONT on a buffer that begins where the sound before ended, to
    /// within a sample (the first buffer of a part cut to the end of the
    /// part before): the sound goes on -- not begun again, nothing the
    /// resampler held dropped, the sink not resynced. A DISCONT a sample
    /// or more off is one.
    #[test]
    fn a_discont_within_a_sample_of_the_sound_before_is_the_sound_going_on() {
        let r = run_on_gst_sink(30.0, Device { rate: 1.0, ..device() }, 1152, 9, &[7_000], 0, &[(T0 + 10e9, 20_000)]);
        assert_eq!(r.stats.joined.load(Ordering::Relaxed), 1);
        assert_eq!(r.stats.resets.load(Ordering::Relaxed), 1, "only the channel's start");
        assert_eq!(r.stats.held_at_reset.load(Ordering::Relaxed), 0);
        assert_eq!(r.resyncs, Vec::<i64>::new());
        assert_eq!(r.aligns.keys().copied().collect::<Vec<_>>(), vec![0]);
        for w in r.placed.windows(2) {
            assert_eq!(w[1].start, w[0].start + w[0].samples_out as f64);
        }
        let r = run_on_gst_sink(30.0, Device { rate: 1.0, ..device() }, 1152, 9, &[7_000], 0, &[(T0 + 10e9, 21_000)]);
        assert_eq!(r.stats.joined.load(Ordering::Relaxed), 0);
        assert_eq!(r.stats.resets.load(Ordering::Relaxed), 2);
    }
}
