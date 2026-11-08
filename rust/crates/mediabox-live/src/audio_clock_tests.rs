use super::*;
use gst::prelude::*;

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
    // In the contract's S16, sample for sample.
    let bytes = from_f32(&fed);
    let mut back = Vec::new();
    to_f32(&bytes, &mut back);
    assert_eq!(from_f32(&back), bytes);
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

fn raw(format: &str, layout: &str, rate: i32, channels: i32) -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", format)
        .field("layout", layout)
        .field("rate", rate)
        .field("channels", channels)
        // Six: 5.1's positions, as a decoder gives them.
        .field_if_some("channel-mask", (channels == 6).then_some(gst::Bitmask::new(0x3f)))
        .build()
}

#[test]
fn the_chain_brings_any_raw_format_to_the_contract_at_its_own_rate_and_channels() {
    gst::init().unwrap();
    for rate in [32_000, 44_100, 48_000] {
        for channels in [1, 2, 6] {
            for (format, bytes) in [("S16LE", 2), ("S24LE", 3), ("S32LE", 4), ("F32LE", 4), ("F64LE", 8)] {
                let pipeline = gst::Pipeline::new();
                let src = gst_app::AppSrc::builder().caps(&raw(format, "interleaved", rate, channels)).format(gst::Format::Time).build();
                let convert = gst::ElementFactory::make("audioconvert").build().unwrap();
                let resample = gst::ElementFactory::make("audioresample").build().unwrap();
                let contract = contract_filter().unwrap();
                let out = gst_app::AppSink::builder().sync(false).build();
                let chain = [src.upcast_ref(), &convert, &resample, &contract, out.upcast_ref()];
                pipeline.add_many(chain).unwrap();
                gst::Element::link_many(chain).unwrap();
                pipeline.set_state(gst::State::Playing).unwrap();
                let mut b = gst::Buffer::from_mut_slice(vec![0u8; 480 * channels as usize * bytes]);
                b.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
                src.push_buffer(b).unwrap();
                let sample = out.pull_sample().unwrap_or_else(|e| panic!("{format} {rate} {channels}: {e}"));
                let caps = sample.caps().unwrap();
                assert_eq!(admit(caps), Ok(Format { rate: rate as u32, channels: channels as usize }), "{format} {rate} {channels}: {caps}");
                pipeline.set_state(gst::State::Null).unwrap();
            }
        }
    }
}

#[test]
fn a_format_other_than_the_contracts_is_refused_counted_and_told_once() {
    gst::init().unwrap();
    let stats = Stats::default();
    stats.installed.store(true, Ordering::Relaxed);
    let contract = raw("S16LE", "interleaved", 48_000, 2);
    assert_eq!(admission(&stats, 1, Some(&contract)), Admission::Control(Format { rate: 48_000, channels: 2 }));
    assert!(stats.active());
    let float = raw("F32LE", "interleaved", 48_000, 2);
    assert_eq!(admission(&stats, 1, Some(&float)), Admission::Refuse);
    assert_eq!(admission(&stats, 1, Some(&float)), Admission::Refuse);
    assert!(!stats.active(), "refusing: not the controller's sound");
    assert_eq!((stats.unsupported.load(Ordering::Relaxed), stats.degraded.load(Ordering::Relaxed)), (2, 1), "every buffer counted, the event once");
    let told = stats.json();
    assert_eq!(told["controller_active"], false);
    assert_eq!(told["degraded_last"]["generation"], 1);
    assert!(told["degraded_last"]["reason"].as_str().unwrap().contains("format F32LE"), "{told}");
    // Back on the contract (another rate, other channels): the controller's.
    let six = raw("S16LE", "interleaved", 44_100, 6);
    assert_eq!(admission(&stats, 2, Some(&six)), Admission::Control(Format { rate: 44_100, channels: 6 }));
    assert!(stats.active());
    let planar = raw("S16LE", "non-interleaved", 48_000, 2);
    assert_eq!(admission(&stats, 3, Some(&planar)), Admission::Refuse);
    assert_eq!(admission(&stats, 3, None), Admission::Refuse);
    assert_eq!(stats.degraded.load(Ordering::Relaxed), 2);
    assert_eq!(stats.json()["degraded_last"]["generation"], 3);
}

/// Buffers of `caps` (S16 or F32 at 48 or 44.1 kHz), `count` of `frames`.
fn push(src: &gst_app::AppSrc, caps: &gst::Caps, count: usize, frames: usize, from: &mut u64) {
    let s = caps.structure(0).unwrap();
    let rate = s.get::<i32>("rate").unwrap() as u64;
    let channels = s.get::<i32>("channels").unwrap() as usize;
    let bytes = if s.get::<&str>("format").unwrap() == "S16LE" { 2 } else { 4 };
    src.set_caps(Some(caps));
    for _ in 0..count {
        let mut b = gst::Buffer::from_mut_slice(vec![0u8; frames * channels * bytes]);
        {
            let b = b.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_nseconds(*from));
            b.set_duration(gst::ClockTime::from_nseconds(frames as u64 * 1_000_000_000 / rate));
        }
        *from += frames as u64 * 1_000_000_000 / rate;
        src.push_buffer(b).unwrap();
    }
}

#[test]
fn on_a_real_sink_the_contract_is_controlled_a_new_rate_followed_and_another_format_never_played() {
    gst::init().unwrap();
    let pipeline = gst::Pipeline::new();
    pipeline.use_clock(Some(&gst::SystemClock::obtain()));
    let src = gst_app::AppSrc::builder().format(gst::Format::Time).build();
    let sink = gst::ElementFactory::make("alsasink")
        .property("device", "null")
        .property("buffer-time", 100_000i64)
        .property("latency-time", 10_000i64)
        .build()
        .unwrap();
    let stats = std::sync::Arc::new(Stats::default());
    install(&sink, || 7, std::sync::Arc::clone(&stats)).unwrap();
    sink.set_property_from_str("slave-method", "custom");
    // What reaches the sink past the controller, in a format not the
    // contract's.
    let passed = std::sync::Arc::new(AtomicU64::new(0));
    let p = std::sync::Arc::clone(&passed);
    let pad = sink.static_pad("sink").unwrap();
    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        if info.buffer().is_some() && pad.current_caps().is_some_and(|c| admit(&c).is_err()) {
            p.fetch_add(1, Ordering::Relaxed);
        }
        gst::PadProbeReturn::Ok
    });
    pipeline.add_many([src.upcast_ref(), &sink]).unwrap();
    src.link(&sink).unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let mut at = 0;
    push(&src, &raw("S16LE", "interleaved", 48_000, 2), 40, 1152, &mut at);
    push(&src, &raw("S16LE", "interleaved", 44_100, 6), 40, 1152, &mut at);
    push(&src, &raw("F32LE", "interleaved", 48_000, 2), 20, 1152, &mut at);
    src.end_of_stream().unwrap();
    let bus = pipeline.bus().unwrap();
    let msg = bus.timed_pop_filtered(gst::ClockTime::from_seconds(60), &[gst::MessageType::Eos, gst::MessageType::Error]);
    pipeline.set_state(gst::State::Null).unwrap();
    assert!(matches!(msg.as_ref().map(|m| m.view()), Some(gst::MessageView::Eos(_))), "{msg:?}");
    assert_eq!(stats.unsupported.load(Ordering::Relaxed), 20, "every float buffer refused");
    assert_eq!(stats.degraded.load(Ordering::Relaxed), 1);
    assert_eq!(passed.load(Ordering::Relaxed), 0, "no refused buffer reached the sink");
    assert_eq!(stats.frames_in.load(Ordering::Relaxed), 80 * 1152, "both rates through the controller");
    assert_eq!(stats.rate_hz.load(Ordering::Relaxed), 44_100);
    assert_eq!(stats.unavailable.load(Ordering::Relaxed), 0);
    assert!(!stats.active());
}

#[test]
fn refused_by_the_guard_the_sink_is_left_uncontrolled_never_on_resample_and_says_so() {
    gst::init().unwrap();
    let sink = gst::ElementFactory::make("alsasink").build().unwrap();
    sink.set_property_from_str("slave-method", "custom");
    let stats = Stats::default();
    fail_closed(&sink, &stats, "test: another libgstaudio");
    let method = sink.property_value("slave-method");
    let method = gst::glib::EnumValue::from_value(&method).unwrap().1.nick();
    assert_ne!(method, "resample", "not the slaving that steps the sound");
    assert_eq!(method, UNCONTROLLED_SLAVING);
    let told = stats.json();
    assert_eq!(told["controller_active"], false);
    assert_eq!(told["abi_guard_failed"], true);
    assert_eq!(told["fallback"], 1);
    assert_eq!(told["abi_reason"], "test: another libgstaudio");
}

#[test]
fn the_cost_is_counted_as_a_share_of_each_buffer_and_held_to_its_limits() {
    let c = Cost::default();
    assert_eq!(c.json()["within_limits"], false, "nothing measured is no pass");
    for _ in 0..199 {
        c.record(1_000_000, 24_000_000);
    }
    // One in 200: above the 99th percentile.
    c.record(5_000_000, 24_000_000);
    assert_eq!(c.elapsed_us(0.5), Some(1_010));
    assert!((c.share(0.99).unwrap() - 0.042).abs() < 1e-9, "{:?}", c.share(0.99));
    assert_eq!(c.json()["within_limits"], true);
    c.record(13_000_000, 24_000_000);
    assert_eq!((c.over_limit.load(Ordering::Relaxed), c.deadline_miss.load(Ordering::Relaxed)), (1, 0));
    assert_eq!(c.json()["within_limits"], false, "one buffer over half its duration");
    c.record(30_000_000, 24_000_000);
    assert_eq!(c.deadline_miss.load(Ordering::Relaxed), 1, "longer than the buffer: a deadline missed");
    assert_eq!(c.json()["max_us"], 30_000);
}

/// Allocations made on this thread, counted for the test binary only (a
/// global allocator in a `cfg(test)` module: the product's is untouched).
struct Counting;

thread_local! {
    static ALLOCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn allocations() -> u64 {
    ALLOCATIONS.try_with(std::cell::Cell::get).unwrap_or(0)
}

fn count_allocation() {
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
}

// SAFETY: every call goes to the system allocator as it came.
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        count_allocation();
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        count_allocation();
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        count_allocation();
        unsafe { std::alloc::System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The resampler's work as the appliance accepted it (FAZ 2J, ec1c26a: p99
/// 0.197 of a buffer, no deadline missed in 60 minutes): taps a sample and
/// the table's phases. More is a heavier controller than the one measured.
const ACCEPTED_TAPS: usize = 64;
const ACCEPTED_PHASES: usize = 512;
/// Allocations a buffer, settled: the S16 buffer for the sink and the
/// model's median of its window (`median`), measured on ec1c26a.
const ACCEPTED_ALLOCATIONS: u64 = 2;

/// The heaviest broadcast sound through the controller as the probe takes
/// it, its work counted, not timed: deterministic under any emulator.
/// The real-time budget (p99 under a quarter of a buffer, none over a half,
/// no deadline missed) is the appliance's gate, `audio_clock.cost` in its
/// health report: an emulator's clock says nothing of it.
#[test]
fn the_heaviest_broadcast_sound_does_bounded_work_its_realtime_budget_is_the_devices_gate() {
    // As `install` does, before the first buffer: made once.
    let made_at = table().as_ptr();
    assert_eq!(2 * HALF, ACCEPTED_TAPS, "taps a sample");
    assert_eq!(table().len(), (ACCEPTED_PHASES + 1) * ACCEPTED_TAPS, "the table's rows");
    // 5.1 at 48 kHz in AC-3's 1536 frames; the sink 200 samples (4.2 ms,
    // under DISCONT_NS) off where the time is, so every buffer is
    // resampled at the bound.
    let (channels, rate, frames) = (6usize, 48_000.0, 1536usize);
    let stats = std::sync::Arc::new(Stats::default());
    let mut control = Controller::new(channels, rate, std::sync::Arc::clone(&stats));
    // The clock's calibration, set by the controller as the probe sets it.
    let mut cal = Calibration { internal: 0, external: 0, num: 1, denom: 1 };
    let place = |t: u64, c: &Calibration| (c.to_internal(t as f64) * rate / 1e9).floor();
    let duration = (frames as f64 * 1e9 / rate) as u64;
    let bound = RATE_RANGE * frames as f64;
    let mut samples = Vec::new();
    let (mut frames_in, mut frames_out, mut at_bound) = (0usize, 0usize, 0usize);
    for k in 0..400u64 {
        let pts = k * duration;
        // The sound goes on from buffer to buffer, as a decoder's.
        let bytes = from_f32(&sine(997.0, rate, (k as usize * frames) as f64, frames, channels));
        let before = allocations();
        to_f32(&bytes, &mut samples);
        let out = control.buffer(
            BufferIn { generation: 1, pts, frames, samples: &samples, discont: false },
            Sink {
                calibration: cal,
                observation: Observation { before: pts, internal: pts, after: pts },
                next_sample: (k > 0).then(|| place(pts, &cal) + 200.0),
                place: &place,
            },
        );
        let made = from_f32(control.output());
        let allocated = allocations() - before;
        if let Some(c) = out.calibration {
            cal = c;
        }
        assert_eq!(table().as_ptr(), made_at, "buffer {k}: the table made again");
        // Once the model's second of readings is full (its window at its
        // last capacity), a buffer allocates as the accepted controller
        // does: the buffer handed to the sink and the median's scratch.
        if k >= 2 * BASELINE as u64 {
            assert!(allocated <= ACCEPTED_ALLOCATIONS, "buffer {k}: {allocated} allocations");
        }
        assert_eq!(made.len(), out.frames * channels * 2, "S16 out, every channel");
        assert!(control.output().iter().all(|v| v.is_finite() && v.abs() <= 0.5 + 1e-3), "buffer {k}: a sample off the sine's range");
        if k >= 2 {
            // No more than the bound's stretch (a frame's rounding): the work
            // a buffer is its frames out times the taps times the channels.
            let off = out.frames as f64 - frames as f64;
            assert!(off.abs() <= bound + 1.0, "buffer {k}: {} frames out of {frames}", out.frames);
            at_bound += usize::from(off.abs() >= bound - 1.0);
            assert!(out.frames * ACCEPTED_TAPS * channels <= ((frames as f64 + bound + 1.0) as usize) * ACCEPTED_TAPS * channels);
        }
        frames_in += frames;
        frames_out += out.frames;
    }
    assert!(at_bound > 390, "resampled at the bound, not passed through: {at_bound}");
    assert_eq!(stats.frames_in.load(Ordering::Relaxed) as usize, frames_in);
    assert_eq!(stats.frames_out.load(Ordering::Relaxed) as usize, frames_out);
    assert_eq!(stats.input_discont.load(Ordering::Relaxed), 0);
}

#[test]
fn six_channels_at_the_rate_bound_are_each_the_sounds_value() {
    // The heavy case's output, channel by channel, against the sine at each
    // output's position (as `fidelity`, for 5.1).
    let (channels, rate, freq) = (6usize, 48_000.0, 997.0);
    for ratio in [1.0 + RATE_RANGE, 1.0 - RATE_RANGE] {
        let mut r = Resampler::new(channels, rate);
        let mut out = Vec::new();
        for k in 0..20 {
            let input = sine(freq, rate, (k * 1536) as f64, 1536, channels);
            r.push(&input, 1536, 0);
            r.process(ratio, &mut out);
        }
        let n_out = out.len() / channels;
        for c in 0..channels {
            let sign = if c == 0 { 1.0 } else { -1.0 };
            let (mut err, mut sig) = (0.0, 0.0);
            for n in (2 * HALF)..(n_out - 4 * HALF) {
                let x = sign * (2.0 * std::f64::consts::PI * freq * (n as f64 / ratio) / rate).sin() * 0.5;
                err += (f64::from(out[n * channels + c]) - x).powi(2);
                sig += x * x;
            }
            let db = 10.0 * (err / sig).log10();
            assert!(db < -90.0, "channel {c} at {ratio}: {db:.1} dB");
        }
    }
}
