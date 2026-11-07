use super::*;

const RATE: u32 = 48_000;
const FRAME: usize = 1152;
const FRAME_NS: u64 = 24_000_000;

fn format() -> PcmFormat {
    PcmFormat { sample: Sample::S16, rate: RATE, channels: 2, interleaved: true }
}

/// One MPEG frame of S16 stereo: a tone at `amplitude` (0 = digital zero).
fn frame(amplitude: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME * 4);
    for k in 0..FRAME {
        let v = (amplitude * 32767.0 * (k as f32 * 0.07).sin()) as i16;
        out.extend_from_slice(&v.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Frames from `pts`, one per 24 ms; those in `quiet` (frame numbers) at
/// nothing.
fn frames(pts: u64, n: usize, quiet: std::ops::Range<usize>) -> Vec<(u64, Vec<u8>)> {
    (0..n).map(|k| (pts + k as u64 * FRAME_NS, frame(if quiet.contains(&k) { 0.0 } else { 0.3 }))).collect()
}

fn segments(f: &[(u64, Vec<u8>)]) -> Vec<Segment<'_>> {
    f.iter().map(|(pts, data)| Segment { pts: *pts, data }).collect()
}

const T0: u64 = 10_000_000_000;
/// The event: frames 20..40, on the running time.
const FROM: u64 = T0 + 20 * FRAME_NS;
const TO: u64 = T0 + 40 * FRAME_NS;

fn facts(independent: &[(u64, Vec<u8>)], product: &[(u64, Vec<u8>)]) -> SilenceFacts {
    SilenceFacts {
        independent: Some(level_in(&segments(independent), &format(), FROM, TO)),
        product: Some(level_in(&segments(product), &format(), FROM, TO)),
        ..Default::default()
    }
}

#[test]
fn the_source_silent_in_both_decodings_is_the_source() {
    let source = frames(T0, 60, 20..40);
    assert_eq!(classify_silence(&facts(&source, &source)), SilenceClass::SourceContentSilence);
}

#[test]
fn the_source_sounding_and_the_player_silent_is_the_players_decoder() {
    let independent = frames(T0, 60, 0..0);
    let product = frames(T0, 60, 20..40);
    assert_eq!(classify_silence(&facts(&independent, &product)), SilenceClass::ProductDecodeSilence);
}

#[test]
fn decoded_sound_lost_on_the_way_to_the_sink_is_the_render() {
    let sound = frames(T0, 60, 0..0);
    let mut f = facts(&sound, &sound);
    f.sink_lost_ns = 480_000_000;
    assert_eq!(classify_silence(&f), SilenceClass::ProductRenderSilence);
    f.sink_lost_ns = 0;
    assert_eq!(classify_silence(&f), SilenceClass::Unclassified("no layer silent"));
}

#[test]
fn a_muted_or_nothing_volume_is_the_viewers() {
    let source = frames(T0, 60, 0..0);
    let product = frames(T0, 60, 20..40);
    let mut f = facts(&source, &product);
    f.user_silenced = true;
    assert_eq!(classify_silence(&f), SilenceClass::UserSilenced);
}

#[test]
fn a_decoding_short_of_the_window_or_missing_is_unclassified() {
    let source = frames(T0, 60, 20..40);
    let short = frames(T0, 30, 20..30);
    assert!(matches!(classify_silence(&facts(&short, &source)), SilenceClass::Unclassified(_)));
    let mut f = facts(&source, &source);
    f.independent = None;
    assert_eq!(classify_silence(&f), SilenceClass::Unclassified("no independent decoding"));
}

#[test]
fn the_windows_are_cut_by_pts_not_by_order() {
    // The independent decoding begins 5 frames later than the player's:
    // the same PTS window must find the same silence in both.
    let product = frames(T0, 60, 20..40);
    let independent: Vec<(u64, Vec<u8>)> = frames(T0, 60, 20..40).into_iter().skip(5).collect();
    let f = facts(&independent, &product);
    assert_eq!(classify_silence(&f), SilenceClass::SourceContentSilence);
    // One frame off the window, and it is not silent.
    let (l, _) = level_in(&segments(&product), &format(), FROM - FRAME_NS, TO);
    assert!(!silent(&l));
}

#[test]
fn raw_pts_go_on_the_running_time_by_the_readers_own_mapping() {
    // PES PTS every 2160 ticks; the reader's running times every 24 ms
    // from T0, mapped for every fifth PES only.
    let pes: Vec<(u64, u64)> = (0..60u64).step_by(5).map(|k| (900_000 + k * 2160, T0 + k * FRAME_NS)).collect();
    assert_eq!(running_of(900_000 + 7 * 2160, &pes), Some(T0 + 7 * FRAME_NS));
    // Independent frames placed by it land where the player's are.
    let independent: Vec<(u64, Vec<u8>)> =
        (0..60u64).map(|k| (running_of(900_000 + k * 2160, &pes).unwrap(), frame(if (20..40).contains(&k) { 0.0 } else { 0.3 }))).collect();
    let product = frames(T0, 60, 20..40);
    assert_eq!(classify_silence(&facts(&independent, &product)), SilenceClass::SourceContentSilence);
}

#[test]
fn a_channel_changed_in_the_window_keeps_to_the_events_samples() {
    let quiet = frame(0.0);
    let loud = frame(0.3);
    let mut raw = Vec::new();
    let mut index = Vec::new();
    for k in 0..60u64 {
        // The event's channel (1) silent where channel 2 sounds at the
        // same PTS.
        for (g, data) in [(1u64, &quiet), (2u64, &loud)] {
            index.push(serde_json::json!({ "gen": g, "pts": T0 + k * FRAME_NS, "offset": raw.len(), "bytes": data.len() }));
            raw.extend_from_slice(data);
        }
    }
    let one = segments_of(&index, &raw, 1);
    let (l, coverage) = level_in(&one, &format(), FROM, TO);
    assert!(silent(&l));
    assert!((coverage - 1.0).abs() < 0.01, "{coverage}");
    assert_eq!(one.len(), 60);
}

// ------------------------------------------------------------- transport

const SOUND: u16 = 0x201;

fn packet(pid: u16, cc: u8, pusi: bool, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0x47, (u8::from(pusi) << 6) | (pid >> 8) as u8, pid as u8, 0x10 | (cc & 0x0F)];
    p.extend_from_slice(payload);
    p.resize(188, 0xFF);
    p
}

fn pes(pts: u64) -> Vec<u8> {
    let b = [0x21 | (((pts >> 30) as u8 & 0x07) << 1), (pts >> 22) as u8, ((pts >> 15) as u8) << 1 | 1, (pts >> 7) as u8, (pts as u8) << 1 | 1];
    let mut h = vec![0, 0, 1, 0xC0, 0, 0, 0x80, 0x80, 5];
    h.extend_from_slice(&b);
    h
}

/// PES of the sound every 2160 ticks, five packets each; `skip` PES left
/// out, `lose` packets (by number) lost.
fn stream(n: u64, skip: &[u64], lose: &[usize]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cc = 0u8;
    let mut number = 0;
    for k in 0..n {
        for i in 0..5 {
            let p = packet(SOUND, cc, i == 0, &if i == 0 { pes(900_000 + k * 2160) } else { vec![0xAA; 10] });
            cc = (cc + 1) & 0x0F;
            if !skip.contains(&k) && !lose.contains(&number) {
                out.extend(p);
            }
            number += 1;
        }
    }
    out
}

#[test]
fn a_whole_stream_has_no_transport_errors() {
    let r = ts_check(&stream(50, &[], &[]), SOUND, None, &[]);
    assert_eq!((r.cc_errors, r.tei, r.sync_errors), (0, 0, 0));
    assert_eq!(r.pes.len(), 50);
    assert!(r.pts_irregular.is_empty());
    assert_eq!(r.pts_max_gap_ns, 24_000_000);
}

#[test]
fn a_lost_packet_is_a_continuity_error_in_its_pes() {
    // Packet 52 is the third of PES 10.
    let r = ts_check(&stream(50, &[], &[52]), SOUND, None, &[]);
    assert_eq!(r.cc_errors, 1);
    assert_eq!(r.cc_error_pts, vec![Some(900_000 + 10 * 2160)]);
}

#[test]
fn a_pes_missing_is_a_pts_step_off_the_usual() {
    let r = ts_check(&stream(50, &[25], &[]), SOUND, None, &[]);
    // Its packets went with it: the counter breaks too.
    assert_eq!(r.cc_errors, 1);
    assert_eq!(r.pts_irregular, vec![(900_000 + 26 * 2160, 4320)]);
}

#[test]
fn input_errors_make_the_silence_the_inputs() {
    let source = frames(T0, 60, 20..40);
    let mut f = facts(&source, &source);
    f.input_errors = 1;
    assert_eq!(classify_silence(&f), SilenceClass::InputAudioBitstreamError);
    f.input_errors = 0;
    f.decoder_errors = 2;
    assert_eq!(classify_silence(&f), SilenceClass::InputAudioBitstreamError);
}

// ----------------------------------------------------------------- clock

/// A second and a half of samples every 10 ms around a step at sample 150,
/// the device playing 480 frames each, fed as much.
fn rows(n: usize) -> Vec<ClockRow> {
    (0..n)
        .map(|k| {
            let at = T0 + k as u64 * 10_000_000;
            ClockRow {
                at,
                after: at + 20_000,
                late: Some(100_000),
                internal: Some(at),
                calibration: Some([1, 1, 1, 1]),
                running: true,
                trigger: Some(5),
                tstamp: Some(at - 1_000_000),
                hw_ptr: Some(k as u64 * 480),
                appl_ptr: Some(4800 + k as u64 * 480),
                fed: k as u64 * 480,
            }
        })
        .collect()
}

#[test]
fn one_reading_off_and_the_playout_in_a_line_is_the_reading() {
    let mut r = rows(300);
    r[150].late = Some(100_000 + 10_000_000);
    let f = clock_facts(&r, 150, 48_000.0);
    assert_eq!(f.back_after, Some(1));
    assert!(f.hw_residual_ns.unwrap() < 1_000.0);
    assert!(f.feed_shift_ns.unwrap().abs() < 1_000.0);
    assert_eq!(classify_clock(&f), ClockClass::MeasurementArtifact("one reading off, the next back, the playout in a line"));
}

#[test]
fn clocks_read_far_apart_make_a_late_of_their_own() {
    // Preempted between the two clocks: the reading is off by the time
    // between, for good in this window (no return), the playout in a line.
    let mut r = rows(300);
    for row in r.iter_mut().skip(150) {
        row.late = Some(100_000 - 6_000_000);
    }
    r[150].after = r[150].at + 6_000_000;
    let f = clock_facts(&r, 150, 48_000.0);
    assert_eq!(f.back_after, None);
    assert_eq!(classify_clock(&f), ClockClass::MeasurementArtifact("the two clocks were read that far apart"));
}

#[test]
fn the_device_stepping_against_its_timestamps_is_the_products() {
    let mut r = rows(300);
    for (k, row) in r.iter_mut().enumerate().skip(150) {
        row.late = Some(100_000 + 10_000_000);
        // 10 ms of frames (480) skipped by the device.
        row.hw_ptr = Some(k as u64 * 480 + 480);
    }
    let f = clock_facts(&r, 150, 48_000.0);
    assert!(f.hw_residual_ns.unwrap() > PRESENT_STEP_NS, "{f:?}");
    assert!(matches!(classify_clock(&f), ClockClass::ProductAudioTimingStep(_)));
}

#[test]
fn samples_dropped_between_the_feed_and_the_device_are_the_products() {
    let mut r = rows(300);
    for row in r.iter_mut().skip(150) {
        row.late = Some(100_000 + 10_000_000);
        // 10 ms of the feed never written to the device.
        row.appl_ptr = row.appl_ptr.map(|a| a - 480);
    }
    let f = clock_facts(&r, 150, 48_000.0);
    assert!((f.feed_shift_ns.unwrap() + 10_000_000.0).abs() < 1_000.0, "{f:?}");
    assert_eq!(classify_clock(&f), ClockClass::ProductAudioTimingStep("samples written to the device moved against the feed"));
}

#[test]
fn a_device_started_again_or_without_timestamps_is_unclassified() {
    let mut r = rows(300);
    r[200].trigger = Some(6);
    assert!(matches!(classify_clock(&clock_facts(&r, 150, 48_000.0)), ClockClass::Unclassified(_)));
    let mut r = rows(300);
    for row in r.iter_mut() {
        row.tstamp = None;
    }
    assert_eq!(classify_clock(&clock_facts(&r, 150, 48_000.0)), ClockClass::Unclassified("no device timestamps"));
}
