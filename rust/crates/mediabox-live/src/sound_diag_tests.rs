//! The sound's diagnosis on samples made here: every detector and every
//! cause, from what each layer is given.

use super::*;

const MS: u64 = 1_000_000;
const RATE: u32 = 48_000;

fn s16(channels: usize) -> PcmFormat {
    PcmFormat { sample: Sample::S16, rate: RATE, channels, interleaved: true }
}

/// `frames` of interleaved S16 at `dbfs` per channel (None: digital zero),
/// a square wave: its peak and RMS are both that level.
fn pcm(frames: usize, levels: &[Option<f32>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames * levels.len() * 2);
    for i in 0..frames {
        for level in levels {
            let v = level.map_or(0i16, |db| {
                let a = (linear(db) * 32767.0).round() as i16;
                if i % 2 == 0 { a } else { -a }
            });
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

/// A clock and a stream of buffers into `Diag::pcm`, contiguous unless told.
struct Feed {
    diag: Diag,
    now: u64,
    pts: u64,
    generation: u64,
    format: PcmFormat,
    incidents: Vec<Incident>,
}

impl Feed {
    fn new(channels: usize) -> Self {
        let mut diag = Diag::default();
        diag.context.gain = 1.0;
        Self { diag, now: 10_000 * MS, pts: 60_000 * MS, generation: 1, format: s16(channels), incidents: Vec::new() }
    }

    /// `ms` of sound at `levels`, in buffers of `frames` frames, the decoder
    /// fed and putting out for each (C), a snapshot a buffer (`tick`).
    fn play(&mut self, ms: u64, levels: &[Option<f32>], frames: usize) {
        let total = (u64::from(RATE) * ms / 1000) as usize;
        let mut done = 0;
        while done < total {
            let n = frames.min(total - done);
            let data = pcm(n, levels);
            let ns = self.format.ns(n as u64);
            self.diag.c.dec_in += 1;
            self.diag.c.dec_out_buffers += 1;
            self.diag.c.dec_out_frames += n as u64;
            let got = self.diag.pcm(self.now, self.generation, Ok(self.format), Some(self.pts), Some(ns), false, &data);
            self.incidents.extend(got);
            self.pts += ns;
            self.now += ns;
            let ticked = self.diag.tick(self.now, true, None);
            self.incidents.extend(ticked);
            done += n;
        }
    }

    fn kinds(&self) -> Vec<(Kind, Cause, u64)> {
        // To the nearest ms: sample frames are not whole nanoseconds.
        self.incidents.iter().map(|i| (i.kind, i.cause, (i.duration_ns + MS / 2) / MS)).collect()
    }
}

const LOUD: Option<f32> = Some(-20.0);
const ZERO: Option<f32> = None;

#[test]
fn digital_zero_is_an_event_of_its_own_with_what_the_decoder_did() {
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.play(300, &[ZERO, ZERO], 1024);
    f.play(200, &[LOUD, LOUD], 1024);
    assert_eq!(f.kinds(), vec![(Kind::Silence(Quiet::DigitalZero), Cause::DecoderOutputSilence, 300)]);
    let i = &f.incidents[0];
    assert_eq!(i.zero_ratio, Some(1.0));
    assert_eq!(i.peak_dbfs, Some(f32::NEG_INFINITY));
    assert_eq!(i.ended_by, Some("sound"));
    assert!(i.delta.dec_out_frames > 0 && i.delta.dec_in > 0);
    assert!(i.alarm);
}

#[test]
fn minus_90_is_below_the_threshold_and_minus_50_is_sound() {
    let mut f = Feed::new(2);
    f.play(300, &[Some(-90.0), Some(-90.0)], 960);
    f.play(100, &[LOUD, LOUD], 960);
    assert_eq!(f.kinds()[0].0, Kind::Silence(Quiet::BelowThreshold));
    let i = &f.incidents[0];
    assert!((i.peak_dbfs.unwrap() + 90.0).abs() < 1.0, "{:?}", i.peak_dbfs);
    assert_eq!(i.zero_ratio, Some(0.0));
    let mut f = Feed::new(2);
    f.play(1000, &[Some(-50.0), Some(-50.0)], 960);
    assert!(f.incidents.is_empty());
    assert_eq!(f.diag.c.pcm_silent_frames, 0);
}

#[test]
fn a_short_silence_is_counted_but_is_no_event() {
    let mut f = Feed::new(2);
    f.play(100, &[LOUD, LOUD], 960);
    f.play(150, &[ZERO, ZERO], 960);
    f.play(100, &[LOUD, LOUD], 960);
    assert!(f.incidents.is_empty());
    assert_eq!(f.diag.c.pcm_silent_frames, 150 * 48);
}

#[test]
fn a_silence_going_on_shows_while_it_lasts() {
    let mut f = Feed::new(2);
    f.play(100, &[LOUD, LOUD], 960);
    f.play(1500, &[ZERO, ZERO], 960);
    assert!(f.incidents.is_empty(), "not over yet");
    let ongoing = f.diag.ongoing_json(f.now);
    assert_eq!(ongoing["kind"], "decoded_pcm_digital_zero");
    assert_eq!(ongoing["ms"].as_f64().unwrap().round(), 1500.0);
    assert_eq!(f.diag.json(f.now)["ongoing"]["kind"], "decoded_pcm_digital_zero");
}

#[test]
fn a_fade_through_the_thresholds_is_one_run_not_many() {
    let mut f = Feed::new(2);
    f.play(200, &[LOUD, LOUD], 960);
    // Down through -60, then between the thresholds (-57: below -54 but
    // above -60) for a while, then back up: one run.
    for db in [-40.0, -50.0, -58.0, -62.0, -70.0, -62.0, -57.0, -58.0, -57.0, -56.0] {
        f.play(60, &[Some(db), Some(db)], 960);
    }
    f.play(200, &[Some(-30.0), Some(-30.0)], 960);
    let runs: Vec<_> = f.kinds().into_iter().filter(|k| matches!(k.0, Kind::Silence(_))).collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    // From -62 (the first below -60) to the first above -54: 7 x 60 ms.
    assert_eq!(runs[0].2, 7 * 60);
    // Between the thresholds, out of a run: no run starts.
    let mut f = Feed::new(2);
    f.play(1000, &[Some(-57.0), Some(-57.0)], 960);
    assert!(f.incidents.is_empty());
}

#[test]
fn a_click_in_silence_does_not_end_it() {
    let mut f = Feed::new(2);
    f.play(300, &[ZERO, ZERO], 960);
    f.play(20, &[LOUD, LOUD], 960); // shorter than RELEASE_NS
    f.play(300, &[ZERO, ZERO], 960);
    f.play(100, &[LOUD, LOUD], 960);
    let runs = f.kinds();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].2, 600);
}

#[test]
fn one_channel_silent_while_the_other_sounds_is_told_with_which() {
    let mut f = Feed::new(2);
    f.play(100, &[LOUD, LOUD], 960);
    f.play(400, &[LOUD, ZERO], 960);
    f.play(100, &[LOUD, LOUD], 960);
    assert_eq!(f.kinds(), vec![(Kind::ChannelSilence, Cause::DecoderOutputSilence, 400)]);
    assert_eq!(f.incidents[0].mask, Some(0b10));
    assert_eq!(f.diag.c.pcm_silent_frames, 0, "the whole is not silent");
}

#[test]
fn fragmented_buffers_make_one_sample_exact_run() {
    let mut f = Feed::new(2);
    f.play(100, &[LOUD, LOUD], 37);
    f.play(250, &[ZERO, ZERO], 37);
    f.play(100, &[LOUD, LOUD], 41);
    assert_eq!(f.incidents.len(), 1);
    // 250 ms is 12000 frames: in 37-frame pieces, exact.
    assert_eq!(f.incidents[0].frames, Some(12_000));
    assert_eq!(f.diag.c.pcm_gaps + f.diag.c.pcm_overlaps, 0);
}

#[test]
fn a_duration_field_that_disagrees_with_the_samples_is_not_believed() {
    let mut f = Feed::new(2);
    let data = pcm(240, &[ZERO, ZERO]); // 5 ms of samples
    let format = Ok(f.format);
    for k in 0..60 {
        // Each says 10 ms; the PTS run on by the samples (5 ms).
        let got = f.diag.pcm(f.now, 1, format, Some(k * 5 * MS), Some(10 * MS), false, &data);
        f.incidents.extend(got);
        f.now += 5 * MS;
    }
    assert_eq!(f.diag.c.pcm_duration_mismatch, 60);
    assert_eq!(f.diag.c.pcm_gaps + f.diag.c.pcm_overlaps, 0, "contiguous by the samples");
    assert_eq!(f.diag.meter.ongoing().map(|r| r.silent_ns), Some(300 * MS));
}

#[test]
fn a_gap_an_overlap_and_a_jump_back_end_the_run() {
    let mut f = Feed::new(2);
    f.play(300, &[ZERO, ZERO], 960);
    f.pts += 250 * MS; // samples missing
    f.play(300, &[ZERO, ZERO], 960);
    let ends: Vec<_> = f.incidents.iter().map(|i| (i.kind, i.ended_by)).collect();
    assert_eq!(ends[0], (Kind::PcmGap, None));
    assert_eq!(ends[1], (Kind::Silence(Quiet::DigitalZero), Some("pts-gap")));
    assert_eq!(f.diag.c.pcm_gaps, 1);
    let mut f = Feed::new(2);
    f.play(300, &[ZERO, ZERO], 960);
    f.pts -= 5 * MS; // doubled
    f.play(300, &[ZERO, ZERO], 960);
    assert_eq!(f.incidents[0].ended_by, Some("pts-overlap"));
    assert_eq!(f.diag.c.pcm_overlaps, 1);
    let mut f = Feed::new(2);
    f.play(300, &[ZERO, ZERO], 960);
    f.pts -= 2000 * MS; // back past the run's start
    f.play(300, &[ZERO, ZERO], 960);
    assert_eq!(f.incidents[0].ended_by, Some("pts-backward"));
    assert_eq!(f.diag.c.pcm_backward, 1);
}

#[test]
fn another_channel_or_format_ends_the_run() {
    let mut f = Feed::new(2);
    f.play(300, &[ZERO, ZERO], 960);
    f.generation = 2;
    f.play(300, &[ZERO, ZERO], 960);
    assert_eq!(f.incidents[0].ended_by, Some("channel"));
    let mut f = Feed::new(2);
    f.play(300, &[ZERO, ZERO], 960);
    f.format = s16(1);
    f.play(300, &[ZERO], 960);
    assert_eq!(f.incidents[0].ended_by, Some("format"));
}

#[test]
fn caps_it_cannot_read_are_said_and_never_silent() {
    let mut d = Diag::default();
    for (name, format, layout) in [
        ("audio/x-raw", Some("S24LE"), None),
        ("audio/x-raw", Some("U8"), None),
        ("audio/x-raw", Some("S16LE"), Some("weird")),
        ("audio/x-ac3", None, None),
    ] {
        let parsed = PcmFormat::from_caps(name, format, Some(48000), Some(2), layout);
        assert!(parsed.is_err(), "{name} {format:?}");
        d.pcm(1, 1, parsed, Some(0), None, false, &[0; 1920]);
    }
    assert_eq!(d.c.pcm_unsupported, 4);
    assert_eq!((d.c.pcm_buffers, d.c.pcm_silent_frames), (0, 0));
    assert!(d.meter.ongoing().is_none());
    // A buffer that is not whole frames: also unreadable, not silence.
    d.pcm(2, 1, Ok(s16(2)), Some(0), None, false, &[0; 3]);
    assert_eq!(d.c.pcm_unsupported, 5);
}

#[test]
fn the_three_formats_and_planar_measure_alike() {
    let level = linear(-20.0);
    let s16 = pcm(4, &[Some(-20.0), ZERO]);
    let mut s32 = Vec::new();
    let mut f32i = Vec::new();
    let mut f32p = vec![0u8; 4 * 2 * 4];
    for i in 0..4 {
        let v = if i % 2 == 0 { level } else { -level };
        s32.extend_from_slice(&((v as f64 * 2_147_483_647.0) as i32).to_le_bytes());
        s32.extend_from_slice(&0i32.to_le_bytes());
        f32i.extend_from_slice(&v.to_le_bytes());
        f32i.extend_from_slice(&0f32.to_le_bytes());
        f32p[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    let format = |sample, interleaved| PcmFormat { sample, rate: RATE, channels: 2, interleaved };
    for (data, format) in [
        (s16, format(Sample::S16, true)),
        (s32, format(Sample::S32, true)),
        (f32i, format(Sample::F32, true)),
        (f32p, format(Sample::F32, false)),
    ] {
        let l = measure(&format, &data).unwrap();
        assert_eq!(l.frames, 4, "{format:?}");
        assert!((dbfs(l.peak[0]) + 20.0).abs() < 0.01 && (dbfs(l.rms(0)) + 20.0).abs() < 0.01, "{format:?}");
        assert_eq!((l.peak[1], l.zero_frames), (0.0, 0), "{format:?}");
    }
}

#[test]
fn the_viewer_s_mute_changes_the_alarm_not_the_measurement() {
    let mut f = Feed::new(2);
    f.play(100, &[LOUD, LOUD], 960);
    assert_eq!(f.diag.user(f.now, 1.0, true), None);
    f.play(300, &[ZERO, ZERO], 960);
    f.play(100, &[LOUD, LOUD], 960);
    // Measured as ever: the decoded sound was silent, whatever was heard.
    assert_eq!(f.kinds(), vec![(Kind::Silence(Quiet::DigitalZero), Cause::DecoderOutputSilence, 300)]);
    assert!(!f.incidents[0].alarm, "muted: not the viewer's alarm");
    // Unmuted after 400 ms: the silence they chose, as such.
    let back = f.diag.user(f.now, 1.0, false).expect("an event");
    assert_eq!((back.kind, back.cause, back.duration_ns / MS), (Kind::UserSilenced, Cause::UserMuted, 400));
    // Volume to nothing and back: the same.
    f.diag.user(f.now, 0.0, false);
    f.now += 250 * MS;
    assert_eq!(f.diag.user(f.now, 0.5, false).map(|i| i.cause), Some(Cause::UserMuted));
    assert_eq!(f.diag.c.user_changes, 4);
}

#[test]
fn a_dropped_buffer_at_the_sink_is_render_starvation() {
    // `audio-drop`: a buffer of whole PCM missing on its way into the sink.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    let small = f.diag.sink_off(f.now, 21 * MS as i64);
    assert!(small.is_none(), "shorter than an event: counted only");
    assert_eq!(f.diag.sink_by_cause[Cause::RenderStarvation.index()], 1);
    let big = f.diag.sink_off(f.now, 250 * MS as i64).expect("an event");
    assert_eq!((big.kind, big.cause), (Kind::SinkGap, Cause::RenderStarvation));
}

#[test]
fn a_stalled_stream_is_no_samples_and_the_late_sink_says_why() {
    // `audio-stall`: no buffers for 400 ms, then late at the sink.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    for _ in 0..16 {
        f.now += 25 * MS;
        let ticked = f.diag.tick(f.now, true, Some(3.0));
        f.incidents.extend(ticked);
    }
    assert!(f.incidents.is_empty(), "going on");
    assert_eq!(f.diag.json(f.now)["ongoing"]["kind"], "decoded_pcm_absent");
    f.diag.c.sink_behind += 1;
    f.play(100, &[LOUD, LOUD], 1024);
    let i = &f.incidents[0];
    assert_eq!((i.kind, i.cause), (Kind::PcmAbsent, Cause::RenderStarvation));
    assert!(i.duration_ns >= 400 * MS);
    assert_eq!(i.alsa_least_ms, Some(3.0));
    // Without a word from the sink: unknown, not guessed.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    for _ in 0..16 {
        f.now += 25 * MS;
        f.diag.tick(f.now, true, None);
    }
    f.play(100, &[LOUD, LOUD], 1024);
    assert_eq!(f.kinds()[0].1, Cause::Unknown);
}

#[test]
fn a_buffer_early_for_its_time_holding_the_sink_is_no_absence() {
    // 51616's opening, 2026-11-04: the first buffer 358 ms before its time,
    // the sink holding the stream until then; the next 340 ms later, its
    // PTS contiguous. Nothing went missing at the output.
    let mut f = Feed::new(2);
    let data = pcm(1152, &[LOUD, LOUD]);
    let ns = f.format.ns(1152);
    f.diag.pcm(f.now, 1, Ok(f.format), Some(f.pts), Some(ns), false, &data);
    f.diag.played_until(f.now + 358 * MS + ns);
    for _ in 0..14 {
        f.now += 25 * MS;
        f.incidents.extend(f.diag.tick(f.now, true, Some(90.0)));
    }
    f.pts += ns;
    f.play(200, &[LOUD, LOUD], 1152);
    assert!(f.incidents.is_empty(), "{:?}", f.kinds());
    // Late past its time by an event's length: absent from when it ended.
    let mut f = Feed::new(2);
    f.diag.tick(f.now, true, None);
    f.diag.pcm(f.now, 1, Ok(f.format), Some(f.pts), Some(ns), false, &data);
    f.diag.played_until(f.now + 40 * MS + ns);
    f.now += 400 * MS;
    f.pts += ns;
    f.diag.c.sink_behind += 1;
    f.play(100, &[LOUD, LOUD], 1152);
    let kinds = f.kinds();
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert_eq!((kinds[0].0, kinds[0].2), (Kind::PcmAbsent, 336));
}

#[test]
fn a_gap_from_the_player_s_own_wait_is_the_player_s_not_the_input_s() {
    // 51616, 2026-11-05: the wait at 805.0 s, the gap at the sink at 805.9 s,
    // once the sound queued before it had played. The player moved the
    // sinks: its own wait, whatever the picture waited for.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    // The sound given so far ends at the output 850 ms from now.
    f.diag.played_until(f.now + 850 * MS);
    f.diag.waited(f.now);
    f.play(850, &[LOUD, LOUD], 1024);
    f.diag.tick(f.now, true, None);
    f.now += 300 * MS;
    f.diag.c.sink_discont += 1;
    f.play(100, &[LOUD, LOUD], 1024);
    assert_eq!(f.kinds()[0].1, Cause::PresentationWait, "{:?}", f.kinds());
}

#[test]
fn a_wait_does_not_make_a_later_gap_the_input_s() {
    // A gap after the queued sound and a report's time: not the wait's, and
    // nothing measured at the input -- the sink's own, as before the wait.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.diag.played_until(f.now + 100 * MS);
    f.diag.waited(f.now);
    f.play(1_500, &[LOUD, LOUD], 1024);
    f.diag.tick(f.now, true, None);
    f.now += 300 * MS;
    f.diag.c.sink_discont += 1;
    f.play(100, &[LOUD, LOUD], 1024);
    assert_eq!(f.kinds()[0].1, Cause::RenderStarvation, "{:?}", f.kinds());
    // And with no layer's evidence at all: unknown, never the input.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.diag.waited(f.now);
    f.play(1_500, &[LOUD, LOUD], 1024);
    f.diag.tick(f.now, true, None);
    f.now += 300 * MS;
    f.play(100, &[LOUD, LOUD], 1024);
    let kinds = f.kinds();
    assert!(!kinds.is_empty());
    assert!(kinds.iter().all(|k| k.1 != Cause::InputTransportDiscontinuity && k.1 != Cause::PresentationWait), "{kinds:?}");
}

#[test]
fn sound_that_came_late_at_the_input_is_the_input_s() {
    // 51616, 2026-11-05: the sound PID stopped for 800 ms, the picture and
    // the PCRs went on; the sink had nothing for 357 ms.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.diag.tick(f.now, true, None);
    f.now += 400 * MS;
    f.diag.c.ts_sound_late += 1;
    f.play(100, &[LOUD, LOUD], 1024);
    assert_eq!(f.kinds()[0].1, Cause::InputTransportDiscontinuity, "{:?}", f.kinds());
}

#[test]
fn a_stall_between_two_reports_is_still_one_event() {
    // `audio-stall=400` with the reports 250 ms apart: one at 150 ms into
    // the stall, the next 100 ms after the samples are back.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.now += 150 * MS;
    f.incidents.extend(f.diag.tick(f.now, true, Some(3.0)));
    f.now += 250 * MS;
    assert!(f.incidents.is_empty(), "too short at the report");
    f.diag.c.sink_behind += 1;
    f.play(100, &[LOUD, LOUD], 1024);
    let kinds = f.kinds();
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert_eq!((kinds[0].0, kinds[0].1), (Kind::PcmAbsent, Cause::RenderStarvation));
    assert!(kinds[0].2 >= 400, "{kinds:?}");
    // Shorter than an event between them: none.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.now += 150 * MS;
    f.diag.tick(f.now, true, None);
    f.play(100, &[LOUD, LOUD], 1024);
    assert!(f.incidents.is_empty());
    // Not playing (paused, or the sound passed through): none either.
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.diag.tick(f.now, false, None);
    f.now += 400 * MS;
    f.play(100, &[LOUD, LOUD], 1024);
    assert!(f.incidents.is_empty());
}

#[test]
fn a_device_restart_is_an_xrun_unless_it_was_ours() {
    let mut f = Feed::new(2);
    f.play(300, &[LOUD, LOUD], 1024);
    let xrun = f.diag.device_restart(f.now, false);
    assert_eq!((xrun.kind, xrun.cause), (Kind::DeviceRestart, Cause::SinkXrun));
    assert_eq!((f.diag.c.alsa_restarts, f.diag.c.alsa_xruns), (1, 1));
    let mut f = Feed::new(2);
    f.play(300, &[LOUD, LOUD], 1024);
    f.diag.route(Some("hdmi:CARD=rockchiphdmi0,DEV=0".into()));
    let ours = f.diag.device_restart(f.now, true);
    assert_eq!(ours.cause, Cause::RouteChange);
    assert_eq!(f.diag.c.alsa_xruns, 0);
}

#[test]
fn a_bluetooth_route_change_touches_no_transport_or_picture_count() {
    let mut f = Feed::new(2);
    f.play(300, &[LOUD, LOUD], 1024);
    let before = f.diag.c;
    f.diag.route(Some("bluealsa:DEV=20:AF:1B:19:E8:D2,PROFILE=a2dp".into()));
    let d = f.diag.c.since(&before);
    assert_eq!(d, Counters { route_changes: 1, ..Counters::default() });
    assert_eq!(route_kind(f.diag.context.route.as_deref()), "bluetooth");
    // A gap while it reopens: the route's, not the stream's.
    let gap = f.diag.sink_off(f.now, 300 * MS as i64).expect("an event");
    assert_eq!(gap.cause, Cause::RouteChange);
    assert_eq!((gap.delta.ts_cc_loss, gap.delta.frame_gaps, gap.delta.video_dropped), (0, 0, 0));
    assert_eq!(gap.json()["route_kind"], "bluetooth");
}

#[test]
fn a_loss_in_the_input_is_the_input_s_and_a_broken_timeline_the_timeline_s() {
    let mut f = Feed::new(2);
    f.play(300, &[LOUD, LOUD], 1024);
    f.diag.c.ts_cc_loss += 1;
    f.play(300, &[ZERO, ZERO], 1024);
    f.play(100, &[LOUD, LOUD], 1024);
    assert_eq!(f.kinds()[0].1, Cause::InputTransportDiscontinuity);
    // The parser's frames: 24 ms each, one 100 ms on.
    let mut f = Feed::new(2);
    f.play(1000, &[LOUD, LOUD], 1024);
    for k in 0..5 {
        f.diag.frame(1, Some(k * 24 * MS), Some(24 * MS), false);
    }
    f.diag.frame(1, Some(5 * 24 * MS + 100 * MS), Some(24 * MS), false);
    f.diag.frame(1, Some(0), Some(24 * MS), false); // and back
    assert_eq!((f.diag.c.frame_gaps, f.diag.c.frame_backward), (1, 1));
    f.play(300, &[ZERO, ZERO], 1024);
    f.play(100, &[LOUD, LOUD], 1024);
    assert_eq!(f.kinds()[0].1, Cause::PesTimestampDiscontinuity);
    // A frame flagged DISCONT (the reader's break) is not a gap of its own.
    let mut d = Diag::default();
    d.frame(1, Some(0), Some(24 * MS), false);
    d.frame(1, Some(500 * MS), Some(24 * MS), true);
    assert_eq!((d.c.frame_gaps, d.c.frame_discont), (0, 1));
}

#[test]
fn a_channel_change_is_no_absence_and_cuts_a_run() {
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.play(300, &[ZERO, ZERO], 1024);
    // The next channel: no samples for 600 ms while it starts.
    f.diag.new_channel(2, f.now);
    for _ in 0..24 {
        f.now += 25 * MS;
        let ticked = f.diag.tick(f.now, true, None);
        f.incidents.extend(ticked);
    }
    assert_eq!(f.kinds().len(), 1, "{:?}", f.kinds());
    assert_eq!(f.incidents[0].ended_by, Some("channel"));
}

#[test]
fn an_incident_carries_every_layer_of_its_window() {
    let mut f = Feed::new(2);
    f.play(1000, &[LOUD, LOUD], 1024);
    f.diag.c.ts_payload_bytes += 4000;
    f.diag.c.video_dropped += 0;
    f.play(300, &[ZERO, ZERO], 1024);
    f.play(100, &[LOUD, LOUD], 1024);
    let json = f.incidents[0].json();
    for layer in ["transport", "timeline", "decoder", "pcm", "sink", "viewer", "picture"] {
        assert!(json["window"][layer].is_object(), "{layer}");
    }
    assert_eq!(json["window"]["transport"]["ts_payload_bytes"], 4000);
    assert_eq!(json["kind"], "decoded_pcm_digital_zero");
    assert_eq!(json["cause"], "decoder_output_silence");
    assert!(json["pcm"]["rms_dbfs"].as_f64().is_some());
}

#[test]
fn the_sink_s_resync_is_read_from_its_own_words() {
    assert_eq!(discont_ns("Unexpected discontinuity in audio timestamps of +0:00:00.012345678, resyncing"), Some(12_345_678));
    assert_eq!(discont_ns("Unexpected discontinuity in audio timestamps of -0:00:01.500000000, resyncing"), Some(-1_500_000_000));
    assert_eq!(discont_ns("Unexpected discontinuity"), None);
    let mut f = Feed::new(2);
    f.play(500, &[LOUD, LOUD], 1024);
    f.diag.c.sink_behind += 1;
    assert!(f.diag.sink_discont(f.now, Some(40 * MS as i64)).is_none(), "short: counted");
    assert_eq!(f.diag.sink_by_cause[Cause::RenderStarvation.index()], 1);
    let big = f.diag.sink_discont(f.now, Some(-300 * MS as i64)).expect("an event");
    assert_eq!((big.kind, big.cause), (Kind::SinkResync, Cause::RenderStarvation));
    // With the decoded PCM's timestamps running off the samples: not
    // starvation -- said as unknown, with the drift in the record.
    let mut f = Feed::new(2);
    f.play(300, &[LOUD, LOUD], 1024);
    f.pts += MS / 2;
    f.play(300, &[LOUD, LOUD], 1024);
    let i = f.diag.sink_discont(f.now, Some(250 * MS as i64)).expect("an event");
    assert_eq!(i.cause, Cause::Unknown);
    assert_eq!(i.json()["pcm_timestamp_gap_ms"], 0.5);
}
