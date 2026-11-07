use super::*;

fn sine(freq: f64, rate: f64, from: f64, frames: usize, channels: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(frames * channels);
    for k in 0..frames {
        let x = (2.0 * std::f64::consts::PI * freq * (from + k as f64) / rate).sin() * 0.5;
        for c in 0..channels {
            v.push((x * if c == 0 { 1.0 } else { -1.0 }) as f32);
        }
    }
    v
}

#[test]
fn at_one_the_resampler_passes_every_sample_as_it_came() {
    let mut r = Resampler::new(2, 48_000.0);
    let mut out = Vec::new();
    let mut fed = Vec::new();
    for k in 0..10 {
        let input = sine(997.0, 48_000.0, (k * 1152) as f64, 1152, 2);
        fed.extend_from_slice(&input);
        r.push(&input, 1152, k * 24_000_000);
        r.process(1.0, &mut out);
    }
    // All but the last HALF frames, held for the taps after them.
    assert_eq!(out.len(), (10 * 1152 - HALF) * 2);
    let worst = out.iter().zip(&fed).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(worst < 1e-6, "{worst}");
    // In S16, sample for sample.
    let f = Format { sample: Sample::S16, rate: 48_000, channels: 2 };
    let bytes = from_f32(f, &fed);
    assert_eq!(from_f32(f, &to_f32(f, &bytes)), bytes);
}

/// The resampled sine against the sine at each output's position: how far
/// under it the error is, dB.
fn fidelity(freq: f64, ratio: f64) -> f64 {
    let rate = 48_000.0;
    let mut r = Resampler::new(1, rate);
    let mut out = Vec::new();
    for k in 0..40 {
        let input = sine(freq, rate, (k * 1152) as f64, 1152, 1);
        r.push(&input, 1152, 0);
        r.process(ratio, &mut out);
    }
    // Output n is the input at n / ratio.
    let (mut err, mut sig) = (0.0, 0.0);
    for (n, &y) in out.iter().enumerate().skip(2 * HALF).take(out.len() - 4 * HALF) {
        let x = (2.0 * std::f64::consts::PI * freq * (n as f64 / ratio) / rate).sin() * 0.5;
        err += (f64::from(y) - x).powi(2);
        sig += x * x;
    }
    10.0 * (err / sig).log10()
}

#[test]
fn off_one_each_sample_is_the_sounds_value_where_it_falls_not_a_neighbour_repeated() {
    for ratio in [1.0 + RATE_RANGE, 1.0 - RATE_RANGE, 1.0 - 11.8e-6] {
        for (freq, floor) in [(997.0, -90.0), (6_000.0, -85.0), (15_000.0, -75.0)] {
            let db = fidelity(freq, ratio);
            assert!(db < floor, "{freq} Hz at {ratio}: {db:.1} dB");
        }
    }
    // A sample repeated (nearest neighbour) is far worse.
    let mut nearest = Vec::new();
    let input = sine(6_000.0, 48_000.0, 0.0, 48_000, 1);
    for n in 0..47_000 {
        nearest.push(input[(n as f64 / (1.0 + RATE_RANGE)).round() as usize]);
    }
    let (mut err, mut sig) = (0.0, 0.0);
    for (n, &y) in nearest.iter().enumerate() {
        let x = (2.0 * std::f64::consts::PI * 6_000.0 * (n as f64 / (1.0 + RATE_RANGE)) / 48_000.0).sin() * 0.5;
        err += (f64::from(y) - x).powi(2);
        sig += x * x;
    }
    assert!(10.0 * (err / sig).log10() > -40.0);
}

#[test]
fn the_resampler_keeps_count_and_time() {
    let mut r = Resampler::new(0, 48_000.0);
    let mut out = Vec::new();
    let mut made = 0;
    for k in 0..1000u64 {
        r.push(&[], 1152, 1_000_000_000 + k * 24_000_000);
        made += r.process(1.0 + 1e-4, &mut out);
    }
    let expected = (1000.0 * 1152.0 - HALF as f64) * (1.0 + 1e-4);
    assert!((made as f64 - expected).abs() <= 2.0, "{made} {expected}");
    // The next output's time: the input's at its position.
    let t = r.next_time().unwrap();
    let pos = made as f64 / (1.0 + 1e-4);
    let want = 1_000_000_000.0 + pos * 1e9 / 48_000.0;
    assert!((t as f64 - want).abs() < 1e3, "{t} {want}");
}

#[test]
fn a_phase_is_taken_out_in_proportion_and_never_faster_than_the_bound() {
    assert_eq!(decide(None, 1152, 48_000.0), Decision::Ratio(1.0));
    // A small phase: in proportion.
    let Decision::Ratio(r) = decide(Some(1.0), 1152, 48_000.0) else { panic!() };
    assert!((1.0 - r) * 1152.0 > 0.09 && (1.0 - r) * 1152.0 < 0.1);
    // 7 ms: at the bound -- a sample of a 1152 buffer -- not 32 samples.
    for phase in [336.0, -336.0, 470.0] {
        let Decision::Ratio(r) = decide(Some(phase), 1152, 48_000.0) else { panic!() };
        assert!(((1.0 - r).abs() - RATE_RANGE).abs() < 1e-12, "{phase}: {r}");
        assert_eq!((1.0 - r).signum(), f64::signum(phase));
    }
    // Beyond a segment: the times jumped.
    assert_eq!(decide(Some(481.0), 1152, 48_000.0), Decision::Discont);
}

fn cal(internal: u64, external: u64) -> Calibration {
    Calibration { internal, external, num: 1, denom: 1 }
}

#[test]
fn readings_not_of_one_moment_or_off_the_median_are_left_out() {
    let mut m = ClockModel::new(cal(100_000_000_000, 70_000_000_000_000));
    let at = |k: u64| 70_000_000_000_000 + k * 24_000_000;
    let mut set = Vec::new();
    for k in 0..200u64 {
        let t = at(k);
        // Read at `t`, between readings of the pipeline's 1 us either side.
        let (internal, after) = match k {
            100 => (100_000_000_000 + (t - at(0)) - 10_000_000, t + 1_000),
            120 => (100_000_000_000 + (t - at(0)), t + 27_300_000),
            _ => (100_000_000_000 + (t - at(0)), t + 1_000),
        };
        let (v, c) = m.observe(Observation { before: t - 1_000, internal, after });
        match k {
            100 => assert_eq!(v, Verdict::Transient),
            120 => assert_eq!(v, Verdict::NotOneMoment),
            _ => assert_eq!(v, Verdict::Used, "{k}"),
        }
        set.extend(c);
    }
    // The truth is the start's calibration: every one set says it.
    assert!(!set.is_empty());
    for c in set {
        let off = c.to_internal(at(150) as f64) - (100_000_000_000 + at(150) - at(0)) as f64;
        assert!(off.abs() < 1_000.0, "{off} ns off, {c:?}");
    }
}

#[test]
fn a_calibration_set_by_another_is_taken_and_its_phase_acquired_from_a_seconds_median() {
    let truth = |t: u64| 100_000_000_000 + (t - 70_000_000_000_000);
    let mut m = ClockModel::new(cal(truth(70_000_000_000_000), 70_000_000_000_000));
    // The sink's start read the clock 10 ms off.
    let t0 = 70_001_000_000_000;
    m.adopt(cal(truth(t0) - 10_000_000, t0));
    let mut acquired = None;
    for k in 0..(BASELINE as u64 + 2) {
        let t = t0 + k * 24_000_000;
        let (_, c) = m.observe(Observation { before: t - 500, internal: truth(t), after: t + 500 });
        if let (Some(c), None) = (c, acquired) {
            acquired = Some((k, c));
        }
    }
    let (k, c) = acquired.expect("acquired");
    assert_eq!(k as usize, BASELINE - 1, "after a second's readings, not one");
    assert!((c.to_internal(t0 as f64) - truth(t0) as f64).abs() < 1_000.0);
}
