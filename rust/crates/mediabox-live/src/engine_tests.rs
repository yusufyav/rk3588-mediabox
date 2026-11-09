//! The engine itself, on a stream whose PMT changes while it plays: the
//! real pipeline -- the reader, tsdemux and its pad-added/pad-removed, the
//! sound's reader path (`route_sound`, `Engine::sound_changed`), the real
//! sound parsers and decoders, both selectors -- with only the picture's
//! decoder stood in for (`identity`: MPP needs the RK3588) and the sound
//! device a fakesink (`Audio::device` None, as the appliance has it with no
//! sound output).
//!
//! What reaches each selector's output is recorded with the input it came
//! from: the old part's sound or picture must not come after the new one's,
//! the new picture must start at a key frame, no EOS may end the picture,
//! and in the end each selector has the one input.

use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gst::prelude::*;

use super::{SINK_RETRY_MS, SWAP_BUDGET_MS, after_failed_open, idle_guard_ms};
use super::{Audio, Engine, Options, Shared};
use crate::es::spec::{pat, pcr_field, pes, pmt, Mux};
use crate::plane::Plane;
use crate::trace::Trace;

/// One pipeline at a time: they run in real time.
static SERIAL: Mutex<()> = Mutex::new(());

const VIDEO: u16 = 0x100;
const PCR: u16 = 0x1FF;
const PMT_PID: u16 = 0x1000;
/// Units are sent this far ahead of their PTS (90 kHz): 300 ms.
const LEAD: u64 = 27_000;
const SWITCH: u64 = LEAD + 180_000; // 2 s into the stream

fn video_units() -> Vec<(u64, Vec<u8>)> {
    let es: &[u8] = include_bytes!("fixtures/video.h264");
    // Access units, each starting at its access unit delimiter.
    let mut starts: Vec<usize> = es.windows(5).enumerate().filter(|(_, w)| *w == [0, 0, 0, 1, 0x09]).map(|(i, _)| i).collect();
    starts.push(es.len());
    starts.windows(2).enumerate().map(|(k, w)| (LEAD + k as u64 * 3_600, es[w[0]..w[1]].to_vec())).collect()
}

/// `units` (4 s of them) said again after themselves, each time on from
/// where they ended, to `seconds`: the pictures' file starts with its
/// parameter sets and a key frame, the sound's frames stand alone.
fn repeated(units: Vec<(u64, Vec<u8>)>, seconds: u64) -> Vec<(u64, Vec<u8>)> {
    let span = units.len() as u64 * (units[1].0 - units[0].0);
    let mut out = Vec::new();
    let mut k = 0;
    while LEAD + k * span < LEAD + seconds * 90_000 {
        out.extend(units.iter().map(|(pts, unit)| (pts + k * span, unit.clone())));
        k += 1;
    }
    out
}

/// (PTS, frame) of a sound file of fixed-size frames.
fn sound_units(es: &[u8], size: usize, ticks: u64) -> Vec<(u64, Vec<u8>)> {
    assert_eq!(es.len() % size, 0);
    es.chunks(size).enumerate().map(|(k, f)| (LEAD + k as u64 * ticks, f.to_vec())).collect()
}

fn mp2() -> Vec<(u64, Vec<u8>)> {
    sound_units(include_bytes!("fixtures/sound-4s.mp2"), 576, 2_160)
}

fn ac3() -> Vec<(u64, Vec<u8>)> {
    sound_units(include_bytes!("fixtures/sound-4s.ac3"), 768, 2_880)
}

type Streams = Vec<(u8, u16, Vec<u8>)>;

/// A sound stream of the stream: its PID, stream_id, frames, and from when
/// to when (PTS) it is sent.
struct Sound {
    pid: u16,
    stream_id: u8,
    units: Vec<(u64, Vec<u8>)>,
    from: u64,
    to: u64,
}

/// A transport stream as a multiplexer sends it: PAT and PMT every 100 ms
/// (the PMT of the version in force at that moment), a PCR every 30 ms on
/// its own PID, every unit `LEAD` ahead of its PTS. Each piece with the time
/// it goes out at (90 kHz).
fn stream(pmts: &[(u64, u8, Streams)], sounds: &[Sound]) -> Vec<(u64, Vec<u8>)> {
    stream_for(4, pmts, sounds)
}

/// The same, `seconds` long.
fn stream_for(seconds: u64, pmts: &[(u64, u8, Streams)], sounds: &[Sound]) -> Vec<(u64, Vec<u8>)> {
    enum Item {
        Tables(usize),
        Pcr(u64),
        Unit(u16, Vec<u8>),
    }
    let mut items: Vec<(u64, u8, Item)> = Vec::new();
    let end = LEAD + seconds * 90_000;
    let mut t = 0;
    while t < end {
        let version = pmts.iter().rposition(|(from, _, _)| *from <= t).expect("a PMT from 0");
        items.push((t, 0, Item::Tables(version)));
        t += 9_000;
    }
    // A new PMT goes out at once, not at the next repetition.
    for (i, (from, _, _)) in pmts.iter().enumerate().skip(1) {
        items.push((*from, 0, Item::Tables(i)));
    }
    let mut t = 0;
    while t < end {
        items.push((t, 1, Item::Pcr(t)));
        t += 2_700;
    }
    for (pts, au) in repeated(video_units(), seconds) {
        items.push((pts - LEAD, 2, Item::Unit(VIDEO, pes(0xE0, Some(pts), None, 0, &au, None))));
    }
    for sound in sounds {
        for (pts, frame) in sound.units.iter().filter(|(pts, _)| (sound.from..sound.to).contains(pts)) {
            items.push((pts - LEAD, 2, Item::Unit(sound.pid, pes(sound.stream_id, Some(*pts), None, 0, frame, None))));
        }
    }
    items.sort_by_key(|(t, order, _)| (*t, *order));
    let mut mux = Mux::default();
    let mut pieces: Vec<(u64, Vec<u8>)> = Vec::new();
    for (t, _, item) in items {
        if pieces.last().is_none_or(|(at, _)| *at != t) {
            pieces.push((t, Vec::new()));
        }
        let out = &mut pieces.last_mut().unwrap().1;
        match item {
            Item::Tables(i) => {
                let (_, version, streams) = &pmts[i];
                out.extend(mux.section(0, &pat(&[(1, PMT_PID)])));
                out.extend(mux.section(PMT_PID, &pmt(1, *version, PCR, streams)));
            }
            // Adaptation only: the counter does not move (2.4.3.3).
            Item::Pcr(t) => out.extend(crate::es::spec::packet(PCR, false, 0, Some((0x10, &pcr_field(t * 300))), &[])),
            Item::Unit(pid, unit) => {
                for p in mux.pes(pid, &unit, &[184]) {
                    out.extend(p);
                }
            }
        }
    }
    pieces
}

/// What went out of a selector: the input it came from, its PTS, and
/// whether it was a key unit; and the events (EOS) that went out.
#[derive(Default)]
struct Seen {
    buffers: Vec<(String, u64, bool)>,
    /// When each buffer went out.
    at: Vec<std::time::Instant>,
    /// Buffers that went out marked DISCONT, by index.
    discont: Vec<usize>,
    eos: usize,
}

fn watch(selector: &gst::Element) -> Arc<Mutex<Seen>> {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let src = selector.static_pad("src").unwrap();
    let into = Arc::clone(&seen);
    let weak = selector.downgrade();
    src.add_probe(gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
        let mut seen = into.lock().unwrap();
        match &info.data {
            Some(gst::PadProbeData::Buffer(buffer)) => {
                let active = weak
                    .upgrade()
                    .and_then(|s| s.property::<Option<gst::Pad>>("active-pad"))
                    .map(|p| p.name().to_string())
                    .unwrap_or_default();
                let pts = buffer.pts().map_or(0, |t| t.nseconds());
                seen.buffers.push((active, pts, !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT)));
                seen.at.push(std::time::Instant::now());
                if buffer.flags().contains(gst::BufferFlags::DISCONT) {
                    let index = seen.buffers.len() - 1;
                    seen.discont.push(index);
                }
            }
            Some(gst::PadProbeData::Event(event)) if event.type_() == gst::EventType::Eos => seen.eos += 1,
            _ => {}
        }
        gst::PadProbeReturn::Ok
    });
    seen
}

/// The inputs in the order they first gave a buffer, each with its
/// buffers; an input that comes back after another is a leak.
fn runs(seen: &Seen) -> Vec<(String, Vec<(u64, bool)>)> {
    let mut runs: Vec<(String, Vec<(u64, bool)>)> = Vec::new();
    for (pad, pts, key) in &seen.buffers {
        match runs.last_mut() {
            Some((last, buffers)) if last == pad => buffers.push((*pts, *key)),
            _ => {
                assert!(!runs.iter().any(|(p, _)| p == pad), "{pad} came back after another input: {:?}", runs.iter().map(|(p, b)| (p, b.len())).collect::<Vec<_>>());
                runs.push((pad.clone(), vec![(*pts, *key)]));
            }
        }
    }
    runs
}

struct Run {
    /// When the writer had written the whole stream.
    written: std::time::Instant,
    engine: Arc<Engine>,
    shared: Arc<Shared>,
    video: Arc<Mutex<Seen>>,
    audio: Arc<Mutex<Seen>>,
}

/// The engine as the appliance runs it, on `ts` written into a FIFO at the
/// broadcast's pace, as the receiver writes it, to its end.
fn play(ts: Vec<(u64, Vec<u8>)>, aid: usize) -> Run {
    play_doing(ts, aid, Duration::ZERO, |_| {})
}

/// The same, with `action` done on the engine `at` after the stream began.
fn play_doing(ts: Vec<(u64, Vec<u8>)>, aid: usize, at: Duration, action: impl FnOnce(&Arc<Engine>) + Send + 'static) -> Run {
    play_with(ts, aid, None, at, action)
}

/// The same, the channel numbered `epoch` by the control plane: held at its
/// opening gate until `action` lets it go.
fn play_with(ts: Vec<(u64, Vec<u8>)>, aid: usize, epoch: Option<u64>, at: Duration, action: impl FnOnce(&Arc<Engine>) + Send + 'static) -> Run {
    play_on(ts, aid, Audio { device: None, passthrough: vec![], gain: 1.0, muted: false, ac3_encode: None }, epoch, at, action)
}

/// The same, the sound going where `audio` says.
fn play_on(ts: Vec<(u64, Vec<u8>)>, aid: usize, audio: Audio, epoch: Option<u64>, at: Duration, action: impl FnOnce(&Arc<Engine>) + Send + 'static) -> Run {
    gst::init().expect("GStreamer");
    // MEDIABOX_TRACE_DIR set: the run on the timeline, as on the board.
    if std::env::var_os("MEDIABOX_TRACE_DIR").is_some() {
        mediabox_core::timeline::init("live");
    }
    let size: usize = ts.iter().map(|(_, piece)| piece.len()).sum();
    let dir = std::env::temp_dir().join(format!("mediabox-live-engine-{}-{size}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("plane");
    let listener = UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
        }
    });
    let plane = Plane::connect(socket.to_str().unwrap(), Trace::from_env(), || {}).expect("plane");
    let shared = Arc::new(Shared::default());
    let options = Options {
        video_decoder: "identity".into(),
        audio,
        display_sync: false,
        demux_latency_ms: 700,
        render_delay_ms: 0,
        max_lateness_ms: None,
    };
    let engine = Engine::new(options, plane, Arc::clone(&shared), Trace::from_env(), |why| {
        if let Some(why) = why {
            eprintln!("engine says: {why}");
        }
    })
    .expect("engine");
    *engine.aid.lock().unwrap() = Some(aid);
    let video = watch(&engine.video_selector);
    let audio = watch(&engine.audio_selector);
    let path = dir.join("stream.ts");
    let fifo = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "mkfifo");
    let writer = {
        let path = path.clone();
        std::thread::spawn(move || {
            use std::io::Write;
            let mut fifo = std::fs::OpenOptions::new().write(true).open(&path).expect("the FIFO");
            let start = std::time::Instant::now();
            for (at, piece) in ts {
                let due = Duration::from_micros(at * 1_000_000 / 90_000);
                if let Some(wait) = due.checked_sub(start.elapsed()) {
                    std::thread::sleep(wait);
                }
                fifo.write_all(&piece).expect("written");
            }
        })
    };
    engine.open_channel(path.to_str().unwrap(), epoch).expect("open");
    let doer = {
        let engine = Arc::clone(&engine);
        std::thread::spawn(move || {
            std::thread::sleep(at);
            action(&engine);
        })
    };
    writer.join().unwrap();
    let written = std::time::Instant::now();
    doer.join().unwrap();
    // The last of it played, and the teardown threads done.
    std::thread::sleep(Duration::from_millis(1_500));
    Run { written, engine, shared, video, audio }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.engine.shutdown();
        let _ = self.engine.pipeline.set_state(gst::State::Null);
    }
}

fn n_pads(selector: &gst::Element) -> u32 {
    selector.property::<u32>("n-pads")
}

/// The picture went on through the PMT change: handed to a new input from a
/// key frame at or after `at` (ns, running time), the old input not after
/// it, no EOS, one input left.
fn picture_handed_over(run: &Run, handed: bool) {
    let seen = run.video.lock().unwrap();
    assert_eq!(seen.eos, 0, "no EOS may end the picture");
    let runs = runs(&seen);
    if !handed {
        assert_eq!(runs.len(), 1, "one input: {:?}", runs.iter().map(|(p, b)| (p, b.len())).collect::<Vec<_>>());
        assert!(runs[0].1.len() >= 90, "the picture went on: {} frames", runs[0].1.len());
        // Not a picture left out: 25 a second, each 40 ms after the last.
        let worst = runs[0].1.windows(2).map(|w| w[1].0.saturating_sub(w[0].0)).max().unwrap_or(0);
        assert!(worst <= 41_000_000, "a step of {} ms in the picture", worst / 1_000_000);
        // The same stream going on: no DISCONT after its start, which made
        // MPP drop the picture it held (FAZ 2E).
        assert!(seen.discont.iter().all(|i| *i == 0), "DISCONT on pictures {:?}", seen.discont);
        return;
    }
    assert_eq!(runs.len(), 2, "{:?}", runs.iter().map(|(p, b)| (p, b.len())).collect::<Vec<_>>());
    let (old, new) = (&runs[0].1, &runs[1].1);
    assert!(new[0].1, "the new input starts at a key frame");
    let last_old = old.last().unwrap().0;
    let first_new = new[0].0;
    assert!(first_new > last_old, "the new picture after the old: {first_new} after {last_old}");
    // GOP of 10 at 25 fps: at most 400 ms without a new picture.
    assert!(first_new - last_old <= 440_000_000, "a gap of {} ms", (first_new - last_old) / 1_000_000);
    assert!(old.len() >= 40 && new.len() >= 40, "pictures before and after: {} and {}", old.len(), new.len());
    assert_eq!(n_pads(&run.engine.video_selector), 1, "the old picture input given back");
}

/// The sound: `inputs` selector inputs one after the other, the old one
/// never after the new; one input left; the reader on `pid`.
fn sound_went_on(run: &Run, inputs: usize, pid: u16, after_ns: u64) {
    let seen = run.audio.lock().unwrap();
    let runs = runs(&seen);
    assert_eq!(runs.len(), inputs, "{:?}", runs.iter().map(|(p, b)| (p, b.len())).collect::<Vec<_>>());
    // Times from the first sound out: the running time the stream started
    // at is the reader's (`stamp::Stamp`), not the PCR's zero.
    let first = seen.buffers.iter().map(|(_, pts, _)| *pts).min().unwrap();
    let last = runs.last().unwrap();
    let came = |i: usize| seen.at[i].saturating_duration_since(run.written).as_millis() as i64 - run.written.saturating_duration_since(seen.at[i]).as_millis() as i64;
    let longest = seen.at.windows(2).enumerate().max_by_key(|(_, w)| w[1] - w[0]).map(|(i, w)| (i, (w[1] - w[0]).as_millis(), seen.buffers[i].1.saturating_sub(first) / 1_000_000));
    assert!(
        last.1.iter().any(|(pts, _)| pts - first >= after_ns),
        "sound after the change, {} ms in: first={first} last={:?} n={} last out {} ms after the stream was written; longest stall between outputs (index, ms, at pts ms) {:?}",
        after_ns / 1_000_000,
        last.1.last(),
        last.1.len(),
        seen.at.len().checked_sub(1).map_or(0, came),
        longest
    );
    assert_eq!(n_pads(&run.engine.audio_selector), 1, "one sound input");
    let sound = run.shared.sound.lock().unwrap();
    let sound = sound.as_ref().expect("a sound part");
    assert_eq!(sound.pid, pid);
    assert_eq!(run.shared.health.sound_push_errors.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(run.shared.health.teardown_errors.load(std::sync::atomic::Ordering::Relaxed), 0);
}

/// The sound went from the old input to the new one where the old one's
/// sound ended: no more than a frame (`frame_ns`) apart either way.
fn sound_continuous(run: &Run, frame_ns: u64) {
    let seen = run.audio.lock().unwrap();
    let runs = runs(&seen);
    assert_eq!(runs.len(), 2, "{:?}", runs.iter().map(|(p, b)| (p, b.len())).collect::<Vec<_>>());
    let old = &runs[0].1;
    let step = old[old.len() - 1].0 - old[old.len() - 2].0;
    let old_end = old[old.len() - 1].0 + step;
    let new_start = runs[1].1[0].0;
    let apart = new_start as i64 - old_end as i64;
    assert!(apart.unsigned_abs() <= frame_ns, "the new sound {} ms from where the old one ended", apart / 1_000_000);
}

fn ns(pts: u64) -> u64 {
    // The first PCR is 0 and the reader puts it at the running time it came:
    // PTS against the first unit's, as a lower bound.
    (pts - LEAD) * 1_000_000_000 / 90_000
}

fn tracks(run: &Run) -> Vec<Option<String>> {
    run.shared.info.lock().unwrap().audio_tracks.iter().map(|(_, language)| language.clone()).collect()
}

#[test]
fn same_codec_another_pid() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let before: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let after: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x102, vec![])];
    let ts = stream(
        &[(0, 0, before), (SWITCH - LEAD, 1, after)],
        &[
            // The old PID goes on after the change: not the sound any more.
            Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX },
            Sound { pid: 0x102, stream_id: 0xC0, units: mp2(), from: SWITCH, to: u64::MAX },
        ],
    );
    let run = play(ts, 0);
    // The same codec: the same input, the reader on the new PID.
    sound_went_on(&run, 1, 0x102, ns(SWITCH) + 500_000_000);
    picture_handed_over(&run, false);
    assert_eq!(tracks(&run).len(), 1, "audio_order counted again for the new program generation");
}

/// The sound device opened again under a playing channel (`ao-reload`
/// after a mode change): the sound goes on from the same input, nothing
/// upstream stops, the picture is not opened again. Taken away under a
/// buffer, the sound's source stopped (FLUSHING, no word) or posted an
/// error that opened the whole channel again (2026-11-06, c05.ts).
#[test]
fn the_sound_device_opened_again_under_the_sound() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream(&[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX }]);
    let run = play_doing(ts, 0, Duration::from_secs(2), |engine| engine.reopen_audio_sink(None));
    sound_went_on(&run, 1, 0x101, 3_200_000_000);
    picture_handed_over(&run, false);
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
}

/// The sound device will not open after a swap (held by another process:
/// hdmi-codec takes one stream at a time): the sound goes on into a
/// stand-in, the device is tried again and taken when it is free. The
/// requests made while it is pending join it -- one swap for three, each
/// answered with its outcome when it is done. Before, three requests were
/// three swaps, each answered "success" when asked, and a device that did
/// not open left the sound stopped for the rest of the channel (2026-11-06,
/// the Plus).
#[test]
fn requests_while_a_swap_is_pending_join_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let answers = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&answers);
    let run = play_doing(ts, 0, Duration::from_secs(2), move |engine| {
        engine.audio.lock().unwrap().device = Some("hw:CARD=mediabox-none,DEV=0".into());
        let waits: Vec<_> = (0..3)
            .map(|_| {
                let (reply, wait) = std::sync::mpsc::sync_channel(1);
                engine.reopen_audio_sink(Some(reply));
                wait
            })
            .collect();
        std::thread::sleep(Duration::from_millis(250));
        engine.audio.lock().unwrap().device = None;
        for wait in waits {
            into.lock().unwrap().push(wait.recv_timeout(Duration::from_secs(3)));
        }
    });
    assert_eq!(*answers.lock().unwrap(), vec![Ok(Ok(())), Ok(Ok(())), Ok(Ok(()))]);
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 1, "one open for the three");
    let health = &run.shared.health;
    assert!(health.sink_open_failures.load(std::sync::atomic::Ordering::Relaxed) >= 1);
    assert!(!health.sink_given_up.load(std::sync::atomic::Ordering::Relaxed));
    sound_went_on(&run, 1, 0x101, 5_000_000_000);
    picture_handed_over(&run, false);
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
}

/// The sound selector never goes idle -- its push held, here by a blocking
/// probe, on the appliance a sink waiting in PAUSED: the request is answered
/// as failed once the guard is up, not left to the caller's timeout, and the
/// device is left as it was. When the push goes on, nothing is swapped late.
#[test]
fn a_swap_that_never_gets_its_turn_is_answered() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let answer = Arc::new(Mutex::new(None));
    let into = Arc::clone(&answer);
    let run = play_doing(ts, 0, Duration::from_secs(1), move |engine| {
        // Held in the sink: the selector's push goes on and on. (A pad
        // blocked by a probe of its own counts as idle.)
        let src = engine.audio_sink.lock().unwrap().as_ref().unwrap().static_pad("sink").unwrap();
        let (held, wait_held) = std::sync::mpsc::sync_channel(1);
        let block = src
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM | gst::PadProbeType::BUFFER, move |_, _| {
                let _ = held.try_send(());
                gst::PadProbeReturn::Ok
            })
            .unwrap();
        wait_held.recv_timeout(Duration::from_secs(3)).expect("the sound held");
        let (reply, wait) = std::sync::mpsc::sync_channel(1);
        let asked = std::time::Instant::now();
        engine.reopen_audio_sink(Some(reply));
        let got = wait.recv_timeout(Duration::from_secs(4));
        *into.lock().unwrap() = Some((got, asked.elapsed()));
        src.remove_probe(block);
    });
    let (got, took) = answer.lock().unwrap().take().unwrap();
    assert!(matches!(got, Ok(Err(_))), "{got:?}");
    assert!(took < Duration::from_secs(3), "answered after {took:?}");
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 0, "nothing swapped late");
    assert!(run.engine.swaps.lock().unwrap().pending.is_none());
    assert!(!run.shared.health.sink_given_up.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
}

/// Never free again: given up on after its tries, said so in the answer
/// and the health, and the channel goes on -- the picture and the sound's
/// source with it, into the stand-in.
#[test]
fn a_device_that_never_opens_is_given_up_on() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let answer = Arc::new(Mutex::new(None));
    let into = Arc::clone(&answer);
    let run = play_doing(ts, 0, Duration::from_secs(2), move |engine| {
        engine.audio.lock().unwrap().device = Some("hw:CARD=mediabox-none,DEV=0".into());
        let (reply, wait) = std::sync::mpsc::sync_channel(1);
        engine.reopen_audio_sink(Some(reply));
        *into.lock().unwrap() = Some(wait.recv_timeout(Duration::from_secs(3)));
    });
    assert!(matches!(*answer.lock().unwrap(), Some(Ok(Err(_)))), "{:?}", answer.lock().unwrap());
    let health = &run.shared.health;
    assert_eq!(health.sink_open_failures.load(std::sync::atomic::Ordering::Relaxed), 6);
    assert!(health.sink_given_up.load(std::sync::atomic::Ordering::Relaxed));
    sound_went_on(&run, 1, 0x101, 5_000_000_000);
    picture_handed_over(&run, false);
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
}

#[test]
fn same_pid_another_codec() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let before: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let after: Streams = vec![(0x1B, VIDEO, vec![]), (0x06, 0x101, vec![0x6A, 1, 0])];
    let ts = stream(
        &[(0, 0, before), (SWITCH - LEAD, 1, after)],
        &[
            Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: SWITCH + 20_000 },
            Sound { pid: 0x101, stream_id: 0xBD, units: ac3(), from: SWITCH, to: u64::MAX },
        ],
    );
    let run = play(ts, 0);
    // A new input for AC-3; MPEG audio's never after it, and all of it
    // played: the new sound goes on where the old one ended. The picture,
    // the same stream on the same PID, goes on through the change.
    sound_went_on(&run, 2, 0x101, ns(SWITCH) + 500_000_000);
    sound_continuous(&run, 24_000_000);
    picture_handed_over(&run, false);
    assert_eq!(tracks(&run).len(), 1);
}

#[test]
fn another_pid_and_codec() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let before: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let after: Streams = vec![(0x1B, VIDEO, vec![]), (0x06, 0x103, vec![0x6A, 1, 0])];
    let ts = stream(
        &[(0, 0, before), (SWITCH - LEAD, 1, after)],
        &[
            Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX },
            Sound { pid: 0x103, stream_id: 0xBD, units: ac3(), from: SWITCH, to: u64::MAX },
        ],
    );
    let run = play(ts, 0);
    sound_went_on(&run, 2, 0x103, ns(SWITCH) + 500_000_000);
    sound_continuous(&run, 24_000_000);
    picture_handed_over(&run, false);
}

#[test]
fn a_pmt_for_a_moment_empty() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    // Version 1 empty (2.4.3.5), version 2 whole again 400 ms later.
    let ts = stream(
        &[(0, 0, streams.clone()), (SWITCH - LEAD, 1, vec![]), (SWITCH - LEAD + 36_000, 2, streams)],
        &[Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX }],
    );
    let run = play(ts, 0);
    // tsdemux passes over a PMT with no streams and takes version 2 for the
    // same programme: the same inputs throughout. The sound gives way while
    // the PMT has none and comes back.
    sound_went_on(&run, 1, 0x101, ns(SWITCH) + 1_000_000_000);
    {
        // The sound held back while the PMT had none: a gap of most of the
        // 400 ms, where the change was, and sound on both sides of it.
        let seen = run.audio.lock().unwrap();
        let first = seen.buffers.iter().map(|(_, pts, _)| *pts).min().unwrap();
        let (gap, at) = seen.buffers.windows(2).map(|w| (w[1].1.saturating_sub(w[0].1), w[0].1 - first)).max().unwrap();
        assert!(gap >= 300_000_000, "no sound while the PMT had none: the longest gap is {} ms", gap / 1_000_000);
        assert!(at.abs_diff(ns(SWITCH)) <= 400_000_000, "the gap {} ms in, the change {} ms in", at / 1_000_000, ns(SWITCH) / 1_000_000);
    }
    picture_handed_over(&run, false);
}

#[test]
fn language_kept_when_the_pmt_reorders() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let tur = vec![0x0A, 4, b't', b'u', b'r', 0];
    let eng = vec![0x0A, 4, b'e', b'n', b'g', 0];
    let before: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, tur.clone()), (0x04, 0x102, eng.clone())];
    let after: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x202, eng), (0x04, 0x201, tur)];
    let ts = stream(
        &[(0, 0, before), (SWITCH - LEAD, 1, after)],
        &[
            Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: SWITCH },
            Sound { pid: 0x102, stream_id: 0xC0, units: mp2(), from: 0, to: SWITCH },
            Sound { pid: 0x201, stream_id: 0xC0, units: mp2(), from: SWITCH, to: u64::MAX },
            Sound { pid: 0x202, stream_id: 0xC0, units: mp2(), from: SWITCH, to: u64::MAX },
        ],
    );
    // The viewer chose the second track, English.
    let run = play(ts, 1);
    sound_went_on(&run, 1, 0x202, ns(SWITCH) + 500_000_000);
    picture_handed_over(&run, false);
    // The tracks counted again for the new program generation: two, not
    // four. (Their languages come in a tag event after the pad, so the
    // list made at pad-added has none -- as before this change.)
    assert_eq!(tracks(&run).len(), 2, "the tracks as the new PMT has them");
}

/// A buffer a source refuses is given back by `route_sound`, not dropped
/// unsaid.
#[test]
fn a_refused_buffer_is_told() {
    gst::init().expect("GStreamer");
    let source = gst_app::AppSrc::builder().format(gst::Format::Time).build();
    // A source that has ended refuses with EOS -- what one being taken
    // down under the reader's push gives.
    let _ = source.end_of_stream();
    let chunk = crate::es::Chunk { data: vec![0xFF; 10], pts: Some(90_000), dts: None, start: true, discont: None, new_time_base: false, switch: None };
    let mut target = Some(source);
    let failed = super::route_sound(&mut target, vec![chunk], Some((0, 0)), false, &mut |_| None, &mut |_| {});
    assert_eq!(failed, vec![gst::FlowError::Eos]);
}

/// The viewer changes the audio stream while the channel plays: MPEG audio
/// to AC-3 of the same PMT. Only the sound changes hands -- the generation,
/// the picture's input and its pictures go on as they were, without a gap;
/// the sound goes from the MPEG audio input to a new one for AC-3, the old
/// one never after it, and the reader is on the AC-3 PID.
#[test]
fn an_audio_stream_change_leaves_the_picture_alone() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![]), (0x06, 0x103, vec![0x6A, 1, 0])];
    let ts = stream(
        &[(0, 0, streams)],
        &[
            Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX },
            Sound { pid: 0x103, stream_id: 0xBD, units: ac3(), from: 0, to: u64::MAX },
        ],
    );
    let generations = Arc::new(Mutex::new((0u64, 0u64)));
    let told = Arc::clone(&generations);
    let run = play_doing(ts, 0, Duration::from_millis(1_800), move |engine| {
        let before = engine.shared.generation.load(std::sync::atomic::Ordering::SeqCst);
        engine.set_aid(Some(1));
        std::thread::sleep(Duration::from_millis(500));
        let after = engine.shared.generation.load(std::sync::atomic::Ordering::SeqCst);
        *told.lock().unwrap() = (before, after);
    });
    let (before, after) = *generations.lock().unwrap();
    assert!(before > 0 && before == after, "the generation stays: {before} -> {after}");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), before);
    // The picture: one input throughout, every picture 40 ms after the one
    // before (25 fps), no EOS.
    picture_handed_over(&run, false);
    {
        let seen = run.video.lock().unwrap();
        let steps: Vec<u64> = seen.buffers.windows(2).map(|w| w[1].1 - w[0].1).collect();
        assert!(steps.iter().all(|&s| s == 40_000_000), "pictures without a gap: {:?}", steps.iter().filter(|&&s| s != 40_000_000).collect::<Vec<_>>());
    }
    // The sound: MPEG audio's input, then AC-3's, one left; the reader on
    // the AC-3 PID; nothing refused or left behind.
    sound_went_on(&run, 2, 0x103, 2_500_000_000);
    assert_eq!(run.shared.sound.lock().unwrap().as_ref().unwrap().codec, Some(crate::es::Codec::Ac3));
    assert_eq!(run.engine.aid(), Some(1));
}

/// A PMT change well after the channel began (3.2 s in): the new part's
/// pictures are shown, from its own first key frame. They were all thrown
/// away as a bogus first picture, held against the channel's first key
/// frame seconds before (found through MPP and the plane, 2026-11-03).
#[test]
fn a_pmt_change_seconds_after_the_start_still_shows_the_new_picture() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let switch = LEAD + 288_000;
    let before: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let after: Streams = vec![(0x1B, 0x110, vec![]), (0x04, 0x101, vec![])];
    let mut ts = stream_for(
        7,
        &[(0, 0, before), (switch - LEAD, 1, after)],
        &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 7), from: 0, to: u64::MAX }],
    );
    // The picture on its new PID from the change on.
    let video = [(VIDEO >> 8) as u8 | 0x40, VIDEO as u8];
    let mut moved = false;
    for (at, piece) in ts.iter_mut() {
        for packet in piece.chunks_mut(188) {
            if *at >= switch - LEAD && (packet[1] & 0x1F) == video[0] & 0x1F && packet[2] == video[1] {
                packet[1] = (packet[1] & 0xE0) | (0x110 >> 8) as u8;
                packet[2] = 0x110u16 as u8;
                moved = true;
            }
        }
    }
    assert!(moved);
    let run = play(ts, 0);
    picture_handed_over(&run, true);
}

/// The multiplex lost for 480 ms, every PID at once, the piece at its start
/// cut short (TRT 1, 2026-11-05 20:42:39: seven PIDs' counters broken in one
/// USB transfer, the sound's PCM 480 ms apart). Nothing of the hole is made
/// up: no unit timed inside it reaches the decoder and the one cut short is
/// not passed on; the sound is back with its first frame after the hole;
/// the channel is not opened again; the picture's first unit after the
/// hole is kept. The units after the hole reach the decoder as they came,
/// delta units before the next key unit included -- tsdemux marks none of
/// them DISCONT -- and MPP's decoder puts no picture out that it flags
/// discarded or in error (gstmppdec.c `mpp_frame_get_discard ||
/// mpp_frame_get_errinfo`).
#[test]
fn a_multiplex_hole_makes_nothing_up_and_the_sound_comes_back_at_once() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let full = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX }]);
    // Going out from 2.00 s to 2.48 s: the units of PTS 2.0 s to 2.48 s
    // past the first.
    let (from, to) = (180_000u64, 180_000u64 + 43_200);
    let mut ts = Vec::new();
    let mut cut = false;
    for (at, piece) in full {
        if (from..to).contains(&at) {
            if !cut {
                // Its first three packets only: a piece cut short.
                ts.push((at, piece[..piece.len().min(3 * 188)].to_vec()));
                cut = true;
            }
            continue;
        }
        ts.push((at, piece));
    }
    let run = play(ts, 0);
    let generation = run.shared.generation.load(std::sync::atomic::Ordering::SeqCst);
    let video: Vec<(u64, bool)> = run.video.lock().unwrap().buffers.iter().map(|(_, pts, key)| (*pts, *key)).collect();
    let audio: Vec<u64> = run.audio.lock().unwrap().buffers.iter().map(|(_, pts, _)| *pts).collect();
    // Both on the pipeline's running time, from the first picture (PTS
    // LEAD): the sound's first buffer is not the stream's first frame.
    let (vfirst, afirst) = (video[0].0, video[0].0);
    let hole = (from * 1_000_000_000 / 90_000, to * 1_000_000_000 / 90_000);
    let rel: Vec<u64> = video.iter().map(|(pts, _)| pts - vfirst).collect();
    let arel: Vec<u64> = audio.iter().filter(|pts| **pts >= afirst).map(|pts| pts - afirst).collect();
    assert!(rel.iter().all(|pts| *pts < hole.0 || *pts >= hole.1), "a unit of the hole reached the decoder: {:?}", rel.iter().filter(|p| **p >= hole.0 && **p < hole.1).collect::<Vec<_>>());
    let last = rel.iter().filter(|pts| **pts < hole.0).max().copied().unwrap();
    assert_eq!(hole.0 - last, 40_000_000, "the picture up to the hole");
    // The first unit after the hole is whole, and kept: tsdemux 1.26.11
    // dropped it with the break (patches/gstreamer/gst-plugins-bad/0001).
    let next = rel.iter().find(|pts| **pts >= hole.1).copied().unwrap();
    assert_eq!(next, hole.1, "the picture's first unit after the hole, {} ms after it", (next - hole.1) / 1_000_000);
    let sound = arel.iter().find(|pts| **pts >= hole.0).copied().unwrap();
    let sound_last = arel.iter().filter(|pts| **pts < hole.0).max().copied().unwrap();
    // MPEG audio frames are 24 ms: one before the hole, one after it.
    assert!(sound_last + 24_000_000 + 24_000_000 >= hole.0, "the sound up to the hole: {} ms", sound_last / 1_000_000);
    assert!(sound - sound_last <= hole.1 - hole.0 + 2 * 24_000_000, "the sound back {} ms after its last", (sound - sound_last) / 1_000_000);
    assert_eq!(generation, 1, "the channel not opened again");
}

/// The sound's first decoded buffer is cut at the key frame's time: the
/// first sample kept is the first one at or after it, and its PTS and
/// duration say so. A buffer from that time on is left as it is.
#[test]
fn the_sound_starts_at_the_key_frame_s_time_to_the_sample() {
    gst::init().expect("GStreamer");
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("rate", 48_000i32)
        .field("channels", 2i32)
        .build();
    let start = 1_000_000_000u64;
    let frame = |pts: u64| {
        let mut buffer = gst::Buffer::from_mut_slice(vec![0u8; 1152 * 4]);
        let b = buffer.get_mut().expect("new");
        b.set_pts(gst::ClockTime::from_nseconds(pts));
        b.set_duration(gst::ClockTime::from_nseconds(24_000_000));
        buffer
    };
    // 10 ms into a 24 ms frame: 480 frames off its front.
    let mut buffer = frame(start);
    assert_eq!(super::trim(&mut buffer, start + 10_000_000, &caps), Some(480));
    assert_eq!(buffer.size(), (1152 - 480) * 4);
    assert_eq!(buffer.pts().map(|p| p.nseconds()), Some(start + 10_000_000));
    assert_eq!(buffer.duration().map(|d| d.nseconds()), Some(14_000_000));
    // Between two samples: the next one, never one before.
    let mut buffer = frame(start);
    assert_eq!(super::trim(&mut buffer, start + 10_000_001, &caps), Some(481));
    assert!(buffer.pts().expect("timed").nseconds() >= start + 10_000_001);
    let mut buffer = frame(start + 5);
    assert_eq!(super::trim(&mut buffer, start, &caps), Some(0));
    assert_eq!(buffer.size(), 1152 * 4);
}

/// A channel numbered by the control plane is held at its opening gate:
/// its pictures until then are pre-roll, none of them on to the plane; the
/// first after the gate opens is its first, and the sound starts there --
/// none of it before, none of it lost after. Another channel's number does
/// not open it. Before, a channel was shown as it came and its display
/// matched a second later under its pictures (FAZ 2E: 3-5 of them sent and
/// never on the panel, every time).
#[test]
fn a_gated_channel_starts_where_its_gate_opens() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream(&[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX }]);
    let stale = Arc::new(Mutex::new(None));
    let into = Arc::clone(&stale);
    let run = play_with(ts, 0, Some(5), Duration::from_millis(1_500), move |engine| {
        *into.lock().unwrap() = Some((engine.open_gate(4, "matched"), engine.gate_snapshot()));
        engine.open_gate(5, "matched").expect("its own number opens it");
    });
    let (refused, before) = stale.lock().unwrap().take().unwrap();
    assert!(refused.is_err(), "another channel's number");
    assert_eq!(before["open"], false, "still shut after it");
    assert!(before["preroll"].as_u64().unwrap() >= 10, "pictures held meanwhile: {before}");
    let gate = run.engine.gate_snapshot();
    assert_eq!(gate["open"], true);
    assert_eq!(gate["outcome"], "matched");
    let first = gate["picture"].as_u64().expect("the first picture after the gate");
    let audio = run.audio.lock().unwrap();
    let sound_start = audio.buffers.iter().map(|(_, pts, _)| *pts).min().expect("sound");
    // The frame that takes in the first picture's time is cut there.
    assert!(sound_start + 1_000_000 >= first, "sound {} ms before the first picture", (first - sound_start) / 1_000_000);
    assert!(sound_start <= first + 24_000_000, "sound {} ms after the first picture", (sound_start - first) / 1_000_000);
    let steps = audio.buffers.windows(2).map(|w| w[1].1.saturating_sub(w[0].1)).max().unwrap_or(0);
    assert!(steps <= 25_000_000, "a gap of {} ms in the sound after it started", steps / 1_000_000);
    // Not a fault of the gate: nothing dropped as late or out of order.
    assert_eq!(run.shared.health.video_dropped.load(std::sync::atomic::Ordering::Relaxed), run.shared.out_of_order.load(std::sync::atomic::Ordering::Relaxed) + not_dmabuf(&run));
}

/// The swap's bound, its clock in the test's hands: every try while the time
/// lasts, none after, whatever the selector made it wait for.
#[test]
fn a_swap_is_tried_within_its_budget_and_said_when_it_is_out() {
    assert_eq!(after_failed_open(0, 0), Some(50));
    assert_eq!(after_failed_open(4, 750), Some(800));
    assert_eq!(after_failed_open(5, 1_550), None, "out of tries");
    assert_eq!(after_failed_open(0, 2_700), None, "50 ms more is past the budget");
    assert_eq!(after_failed_open(4, 1_950), None);
    assert_eq!(idle_guard_ms(0), 2_000);
    assert_eq!(idle_guard_ms(1_750), 1_000);
    assert_eq!(idle_guard_ms(5_000), 0);
    // All the tries on time, the sum of the waits inside the budget: what is
    // given to the selector is what is left.
    assert!(SINK_RETRY_MS.iter().sum::<u64>() < SWAP_BUDGET_MS);
}

/// A numbered channel whose device will not open from the player's first
/// moment: the player does not end there, its pictures wait, and when the
/// tries are over the channel fails, nothing of it shown.
#[test]
fn a_device_taken_from_the_start_fails_the_channel_in_its_bound() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    gst::init().expect("GStreamer");
    let dir = std::env::temp_dir().join(format!("mediabox-live-busy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("plane");
    let listener = UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
        }
    });
    let plane = Plane::connect(socket.to_str().unwrap(), Trace::from_env(), || {}).expect("plane");
    let shared = Arc::new(Shared::default());
    let options = Options {
        video_decoder: "identity".into(),
        audio: Audio { device: Some("hw:CARD=mediabox-none,DEV=0".into()), passthrough: vec![], gain: 1.0, muted: false, ac3_encode: None },
        display_sync: false,
        demux_latency_ms: 700,
        render_delay_ms: 0,
        max_lateness_ms: None,
    };
    let (said, heard) = std::sync::mpsc::channel();
    let said = Mutex::new(said);
    let started = std::time::Instant::now();
    let engine = Engine::new(options, plane, Arc::clone(&shared), Trace::from_env(), move |why| {
        let _ = said.lock().unwrap().send(why);
    })
    .expect("the player goes on, with a stand-in");
    let path = dir.join("nothing.ts");
    std::fs::write(&path, b"").unwrap();
    engine.open_channel(path.to_str().unwrap(), Some(9)).expect("open");
    assert_eq!(engine.gate_snapshot()["open"], false);
    let why = heard.recv_timeout(Duration::from_millis(SWAP_BUDGET_MS + 2_000)).expect("said within its bound");
    assert_eq!(why.as_deref(), Some("Kanal açılamadı (sound-failed)"));
    assert!(started.elapsed() < Duration::from_millis(SWAP_BUDGET_MS + 2_000));
    let gate = engine.gate_snapshot();
    assert_eq!(gate["failed"], "sound-failed", "{gate}");
    assert_eq!(gate["sound"], "given-up", "{gate}");
    assert_eq!(shared.frames.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(engine.open_gate(9, "matched").is_err(), "a late answer opens nothing");
    engine.shutdown();
}

/// A failed gate lets nothing go, by the control plane's word or by the
/// player's own limit: no picture sent, no sound out -- with pictures held for
/// longer than the sound's own wait for a picture -- and a late answer opens
/// nothing. The player's next channel opens as ever.
#[test]
fn a_failed_gate_shows_and_plays_nothing_and_the_next_channel_opens() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for by_the_limit in [false, true] {
        let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
        let ts = stream(&[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX }]);
        let answers = Arc::new(Mutex::new(None));
        let into = Arc::clone(&answers);
        let run = play_with(ts, 0, Some(5), Duration::from_millis(1_000), move |engine| {
            let failed = if by_the_limit {
                let gate = engine.shared.gate.lock().unwrap().clone().unwrap();
                assert!(gate.expire(std::time::Instant::now() + crate::gate::ARM_MAX), "the player's own limit");
                Ok(())
            } else {
                assert!(engine.fail_gate(4, "mode-failed").is_err(), "another channel's number");
                engine.fail_gate(5, "mode-failed")
            };
            *into.lock().unwrap() = Some((failed, engine.open_gate(5, "matched")));
        });
        let (failed, late) = answers.lock().unwrap().take().unwrap();
        assert_eq!(failed, Ok(()));
        assert!(late.unwrap_err().starts_with("failed: "), "a late answer opens nothing");
        let gate = run.engine.gate_snapshot();
        assert_eq!(gate["open"], false, "{gate}");
        assert!(gate["preroll"].as_u64().unwrap() >= 50, "pictures held all along: {gate}");
        assert_eq!(not_dmabuf(&run), 0, "nothing past the gate: {gate}");
        assert!(run.audio.lock().unwrap().buffers.is_empty(), "no sound");
        assert_eq!(run.shared.frames.load(std::sync::atomic::Ordering::Relaxed), 0);
        // The player's next channel: another gate, and its own number opens it.
        run.engine.shutdown_feed_for_test();
        let path = std::env::temp_dir().join(format!("mediabox-live-next-{}-{by_the_limit}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        run.engine.open_channel(path.to_str().unwrap(), Some(6)).expect("open");
        assert_eq!(run.engine.gate_snapshot()["failed"], serde_json::Value::Null);
        assert!(run.engine.open_gate(5, "matched").is_err(), "the failed one is stale");
        run.engine.open_gate(6, "matched").expect("its own number opens it");
        assert_eq!(run.engine.gate_snapshot()["open"], true);
        let _ = std::fs::remove_file(path);
    }
}

/// Every picture decoded has its end, channel by channel, through a channel
/// change in the same player and the player's end: each ledger closes, and
/// what the stop left in the player is all its terminal drain is.
#[test]
fn every_picture_of_every_channel_has_its_end() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream(&[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX }]);
    let run = play_with(ts, 0, Some(5), Duration::from_millis(800), move |engine| {
        engine.open_gate(5, "matched").expect("opened");
    });
    // The next channel while this one's pictures still come: a stop with
    // pictures on their way.
    run.engine.shutdown_feed_for_test();
    let path = std::env::temp_dir().join(format!("mediabox-live-ledger-{}", std::process::id()));
    std::fs::write(&path, b"").unwrap();
    run.engine.open_channel(path.to_str().unwrap(), Some(6)).expect("open");
    std::thread::sleep(Duration::from_millis(500));
    run.shared.ledgers.close_all();
    let _ = std::fs::remove_file(path);
    let closed = run.shared.ledgers.closed();
    let first = closed.iter().find(|(generation, _)| *generation == 1).expect("the first channel's ledger").1.clone();
    assert_eq!(first.verdict(true), Ok(()), "{first:?}");
    assert!(first.decoded >= 90, "{first:?}");
    assert!(first.preroll >= 5, "pictures held before the gate: {first:?}");
    assert_eq!(first.decoded, first.preroll + first.not_dmabuf + first.terminal_drain_after_stop() + first.cancelled_before_submit() + first.order, "{first:?}");
    for (generation, ledger) in &closed {
        assert_eq!(ledger.verdict(true), Ok(()), "channel {generation}: {ledger:?}");
    }
}

/// The sound read before its part of the branch is up is not lost: a
/// channel's first sound frames come with its first picture, in the same
/// read, before tsdemux has put up the pad its part is built at. Before, the
/// reader passed them by -- the first 96 ms of the PMT fixture's sound (FAZ
/// 2F, 4 MP2 frames, there in the stream) -- and the sound began after the
/// picture.
#[test]
fn the_sound_read_before_its_part_is_up_is_played() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream(&[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX }]);
    let run = play(ts, 0);
    let picture = run.video.lock().unwrap().buffers.iter().map(|(_, pts, _)| *pts).min().expect("pictures");
    let audio = run.audio.lock().unwrap();
    let sound = audio.buffers.iter().map(|(_, pts, _)| *pts).min().expect("sound");
    // Both start at the stream's first PTS: the sound's first frame is the
    // picture's, to the clock's rounding.
    assert!(sound.abs_diff(picture) < 1_000_000, "the sound began {} ms after the picture", (sound as i64 - picture as i64) / 1_000_000);
    let steps = audio.buffers.windows(2).map(|w| w[1].1.saturating_sub(w[0].1)).max().unwrap_or(0);
    assert!(steps <= 25_000_000, "a gap of {} ms in the sound", steps / 1_000_000);
    assert_eq!(run.shared.health.sound_push_errors.load(std::sync::atomic::Ordering::Relaxed), 0);
}

/// Two sound changes, the second before the first has played out, in a
/// fixed order: the first old part held at the selector by the test until the
/// third part is the one waiting, then let play out. The part in the middle
/// never takes the selector or the device's mode, the last one does, and it
/// alone is left. Then the other order: the old part played out before the
/// next change is read. No sleep decides the order: the old part cannot reach
/// its end before the test lets it.
#[test]
fn a_sound_change_on_a_change_not_yet_played_out_goes_to_the_last() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for held in [true, false] {
        let a: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
        let b: Streams = vec![(0x1B, VIDEO, vec![]), (0x06, 0x103, vec![0x6A, 1, 0])];
        let c: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x104, vec![])];
        // The third PMT 40 ms after the second, or a second after it.
        let third = if held { SWITCH + 3_600 } else { SWITCH + 90_000 };
        let ts = stream(
            &[(0, 0, a), (SWITCH - LEAD, 1, b), (third - LEAD, 2, c)],
            &[
                Sound { pid: 0x101, stream_id: 0xC0, units: mp2(), from: 0, to: u64::MAX },
                Sound { pid: 0x103, stream_id: 0xBD, units: ac3(), from: SWITCH, to: u64::MAX },
                Sound { pid: 0x104, stream_id: 0xC0, units: mp2(), from: third, to: u64::MAX },
            ],
        );
        let order = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&order);
        let run = play_doing(ts, 0, Duration::from_millis(1_000), move |engine| {
            if !held {
                return;
            }
            // The first part's input: everything on it waits for the test.
            let input = engine.audio_selector.sink_pads().into_iter().next().expect("the first part's input");
            let (go, wait) = std::sync::mpsc::channel::<()>();
            let wait = Mutex::new(Some(wait));
            let id = input
                .add_probe(gst::PadProbeType::BLOCK | gst::PadProbeType::DATA_DOWNSTREAM, move |_, _| {
                    if let Some(wait) = wait.lock().unwrap().take() {
                        let _ = wait.recv();
                    }
                    gst::PadProbeReturn::Ok
                })
                .expect("a probe");
            // Until the third part is the one waiting on the first.
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while engine.shared.sound.lock().unwrap().as_ref().map(|s| s.pid) != Some(0x104) && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            into.lock().unwrap().push(engine.shared.sound.lock().unwrap().as_ref().map(|s| s.pid));
            input.remove_probe(id);
            let _ = go.send(());
        });
        if held {
            assert_eq!(*order.lock().unwrap(), [Some(0x104)], "the third part was asked for while the first was held");
        }
        let seen = run.audio.lock().unwrap();
        let inputs: Vec<String> = runs(&seen).into_iter().map(|(pad, _)| pad).collect();
        drop(seen);
        // The sound heard last is the last part's own input of the selector.
        let last_input: Vec<String> = run
            .shared
            .sound
            .lock()
            .unwrap()
            .as_ref()
            .map(|parts| parts.pads.lock().unwrap().iter().filter(|(element, _)| *element == run.engine.audio_selector).map(|(_, pad)| pad.name().to_string()).collect())
            .unwrap_or_default();
        assert_eq!(inputs.last(), last_input.first(), "held={held}: the last part is the one heard: {inputs:?} {last_input:?}");
        // The first part played out to its end before the last one began:
        // what lies between them is the middle part's time, never heard.
        if held {
            let seen = run.audio.lock().unwrap();
            let runs = runs(&seen);
            let (first, last) = (&runs[0].1, &runs[runs.len() - 1].1);
            let apart = last[0].0 as i64 - (first[first.len() - 1].0 + 24_000_000) as i64;
            let middle = (ns(third) - ns(SWITCH)) as i64;
            assert!((apart - middle).abs() <= 24_000_000, "the first part's end and the last part's start {} ms apart, the middle part's time {} ms", apart / 1_000_000, middle / 1_000_000);
        }
        // The first part and then the last; the middle one only where the
        // first played out before the last was read.
        assert!(inputs.len() == if held { 2 } else { 3 }, "held={held}: {inputs:?}");
        assert_eq!(n_pads(&run.engine.audio_selector), 1, "held={held}: one input left");
        assert_eq!(run.shared.sound.lock().unwrap().as_ref().map(|s| s.pid), Some(0x104));
        assert!(!run.engine.bitstream(), "held={held}: the device in the last part's mode, PCM");
        assert!(run.engine.sound_handover.lock().unwrap().is_none(), "held={held}: nothing left waiting");
        sound_went_on(&run, inputs.len(), 0x104, ns(third) + 500_000_000);
    }
}

/// The test's pictures are H.264 out of `identity`, not dma-bufs: each one
/// shown is counted dropped as such, and that is all that is.
fn not_dmabuf(run: &Run) -> u64 {
    run.shared.frames.load(std::sync::atomic::Ordering::Relaxed);
    run.video.lock().unwrap().buffers.len() as u64 - run.engine.gate_snapshot()["preroll"].as_u64().unwrap_or(0)
}

/// Held in the sound sink until `release` is sent: the selector's push goes
/// on and on (a pad blocked by a probe of its own counts as idle).
fn hold_the_sink(engine: &Arc<Engine>) -> (gst::Pad, gst::PadProbeId) {
    let src = engine.audio_sink.lock().unwrap().as_ref().unwrap().static_pad("sink").unwrap();
    let (held, wait_held) = std::sync::mpsc::sync_channel(1);
    let block = src
        .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM | gst::PadProbeType::BUFFER, move |_, _| {
            let _ = held.try_send(());
            gst::PadProbeReturn::Ok
        })
        .unwrap();
    wait_held.recv_timeout(Duration::from_secs(3)).expect("the sound held");
    (src, block)
}

/// A swap waiting for its turn when the next channel opens: the sink is the
/// engine's, not the channel's, so it is swapped for the new channel once
/// the selector is free, and the request is answered then -- not dropped
/// with the channel it was asked under. The new channel's sound goes on.
#[test]
fn a_swap_pending_across_a_channel_change_is_done_for_the_next() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let answer = Arc::new(Mutex::new(None));
    let into = Arc::clone(&answer);
    let run = play_doing(ts, 0, Duration::from_secs(1), move |engine| {
        let (src, block) = hold_the_sink(engine);
        let (reply, wait) = std::sync::mpsc::sync_channel(1);
        engine.reopen_audio_sink(Some(reply));
        // The same FIFO opened again: a new generation, the old branch down.
        let path = engine.current_path().unwrap();
        engine.open(&path).expect("opened again");
        src.remove_probe(block);
        *into.lock().unwrap() = Some(wait.recv_timeout(Duration::from_secs(4)));
    });
    let got = answer.lock().unwrap().take().unwrap();
    assert!(matches!(got, Ok(Ok(()))), "{got:?}");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 1, "swapped once");
    assert!(run.engine.swaps.lock().unwrap().pending.is_none());
    let audio = run.audio.lock().unwrap();
    let after = audio.at.iter().filter(|at| at.saturating_duration_since(run.written) > Duration::ZERO).count()
        + audio.at.iter().rev().take_while(|at| run.written.saturating_duration_since(**at) < Duration::from_millis(1500)).count();
    assert!(after > 20, "the new channel's sound went on: {after} buffers near the end");
}

/// The selector's output going away under a waiting swap (its pad
/// deactivated, as the pipeline going down does): the request is answered
/// as failed within the guard, nothing hangs, nothing is swapped.
#[test]
fn a_swap_waiting_when_the_selector_goes_down_is_answered() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(4, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 4), from: 0, to: u64::MAX }]);
    let answer = Arc::new(Mutex::new(None));
    let into = Arc::clone(&answer);
    let run = play_doing(ts, 0, Duration::from_secs(1), move |engine| {
        let (src, block) = hold_the_sink(engine);
        let (reply, wait) = std::sync::mpsc::sync_channel(1);
        let asked = std::time::Instant::now();
        engine.reopen_audio_sink(Some(reply));
        let selector_src = engine.audio_selector.static_pad("src").unwrap();
        let _ = selector_src.set_active(false);
        let got = wait.recv_timeout(Duration::from_secs(4));
        *into.lock().unwrap() = Some((got, asked.elapsed()));
        src.remove_probe(block);
    });
    let (got, took) = answer.lock().unwrap().take().unwrap();
    assert!(matches!(got, Ok(Err(_))), "{got:?}");
    assert!(took < Duration::from_millis(super::SWAP_IDLE_GUARD_MS + 500), "answered after {took:?}");
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 0);
    assert!(run.engine.swaps.lock().unwrap().pending.is_none());
}

/// A pad the swap cannot wait on (flushing: deactivated before the request)
/// is answered at once or by the guard -- never left to the caller.
#[test]
fn a_swap_on_a_selector_that_takes_no_probe_is_answered() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(3, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 3), from: 0, to: u64::MAX }]);
    let answer = Arc::new(Mutex::new(None));
    let into = Arc::clone(&answer);
    let _run = play_doing(ts, 0, Duration::from_secs(1), move |engine| {
        let selector_src = engine.audio_selector.static_pad("src").unwrap();
        let _ = selector_src.set_active(false);
        let (reply, wait) = std::sync::mpsc::sync_channel(1);
        let asked = std::time::Instant::now();
        engine.reopen_audio_sink(Some(reply));
        let got = wait.recv_timeout(Duration::from_secs(4));
        *into.lock().unwrap() = Some((got, asked.elapsed()));
    });
    let (got, took) = answer.lock().unwrap().take().unwrap();
    assert!(got.is_ok(), "answered: {got:?}");
    assert!(took < Duration::from_millis(super::SWAP_IDLE_GUARD_MS + 500), "answered after {took:?}");
}

/// A sink swapped in is told the pipeline's latency before anything
/// reaches it: through the bus it came 20 ms late or more, and the sink
/// had already found its first buffers 150 ms late (FAZ 2E). The sink takes
/// the LATENCY event and sends it on upstream through its pad: that, and
/// the buffers, are seen on its pad from the moment it is added.
#[test]
fn a_swapped_in_sink_has_the_latency_before_its_first_buffer() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(4, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 4), from: 0, to: u64::MAX }]);
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&seen);
    let run = play_doing(ts, 0, Duration::from_secs(1), move |engine| {
        let before = engine.audio_sink.lock().unwrap().clone();
        let into = Arc::clone(&into);
        let watching = engine.pipeline.connect_deep_element_added(move |_, _, element| {
            if element.factory().is_none_or(|f| !f.has_type(gst::ElementFactoryType::SINK)) || element.name().starts_with("video") {
                return;
            }
            let Some(pad) = element.static_pad("sink") else { return };
            let into = Arc::clone(&into);
            let name = element.name().to_string();
            pad.add_probe(gst::PadProbeType::EVENT_UPSTREAM | gst::PadProbeType::BUFFER, move |_, info| {
                let mut seen = into.lock().unwrap();
                match &info.data {
                    Some(gst::PadProbeData::Buffer(_)) => seen.push(format!("{name} buffer")),
                    Some(gst::PadProbeData::Event(e)) if e.type_() == gst::EventType::Latency => seen.push(format!("{name} latency")),
                    _ => {}
                }
                gst::PadProbeReturn::Ok
            });
        });
        let (reply, wait) = std::sync::mpsc::sync_channel(1);
        engine.reopen_audio_sink(Some(reply));
        let _ = wait.recv_timeout(Duration::from_secs(3));
        engine.pipeline.disconnect(watching);
        assert!(engine.audio_sink.lock().unwrap().clone() != before, "a new sink");
    });
    let seen = seen.lock().unwrap().clone();
    let sink = run.engine.audio_sink.lock().unwrap().as_ref().unwrap().name().to_string();
    let mine: Vec<&String> = seen.iter().filter(|s| s.starts_with(&format!("{sink} "))).collect();
    assert!(mine.len() > 5, "{seen:?}");
    assert_eq!(mine[0], &format!("{sink} latency"), "the latency before the first buffer: {mine:?}");
}

// The sound plan changed while a channel plays: the control plane's
// `mediabox-audio-plan`, through the control socket as the player's main
// loop carries it out. The devices are ALSA's own `null` PCM -- a real
// alsasink and a real device clock, no hardware -- under two names: one for
// the television's HDMI, one for a Bluetooth headset. A device that never
// opens is a card that is not there.

const HDMI_PCM: &str = "null";
const HEADSET_PCM: &str = "plug:null";
const ABSENT_PCM: &str = "hw:CARD=mediabox-none,DEV=0";

fn device(id: &str, kind: &str, pcm: &str, bitstream: bool) -> serde_json::Value {
    serde_json::json!({"id": id, "label": id, "kind": kind, "card_id": "test", "pcm": pcm, "bitstream": bitstream})
}

/// The plan the control plane sends: the sound to a Bluetooth headset, PCM
/// only, its volume the headset's own.
fn headset_plan(pcm: &str) -> serde_json::Value {
    serde_json::json!({"device": device("bluetooth:20:AF:1B:19:E8:D2", "bluetooth", pcm, false), "passthrough": [], "volume": 60, "muted": false})
}

/// ... and the television's, AC-3 passed through if `passthrough`.
fn hdmi_plan(volume: u8, muted: bool, passthrough: bool) -> serde_json::Value {
    let passthrough: Vec<&str> = if passthrough { vec!["ac3"] } else { vec![] };
    serde_json::json!({"device": device("display:hdmi", "hdmi", HDMI_PCM, true), "passthrough": passthrough, "volume": volume, "muted": muted})
}

fn on_hdmi() -> Audio {
    Audio { device: Some(HDMI_PCM.into()), passthrough: vec![], gain: 1.0, muted: false, ac3_encode: None }
}

/// A plan asked of the engine as the main loop hands it on; its answer
/// comes on the receiver.
fn ask_plan(engine: &Arc<Engine>, plan: serde_json::Value, epoch: Option<u64>, seq: u64) -> std::sync::mpsc::Receiver<Result<(), String>> {
    let plan: mediabox_core::AudioPlan = serde_json::from_value(plan).expect("a plan");
    let (reply, wait) = std::sync::mpsc::sync_channel(1);
    engine.set_audio_plan(super::PlanRequest::new(Audio::from_plan(&plan, false), epoch, seq), Some(reply));
    wait
}

fn answered(wait: &std::sync::mpsc::Receiver<Result<(), String>>) -> Result<(), String> {
    wait.recv_timeout(Duration::from_secs(4)).unwrap_or_else(|e| Err(format!("no answer: {e}")))
}

/// The level the PCM part plays at: its volume element's.
fn level(engine: &Engine) -> Option<(f64, bool)> {
    engine
        .current_elements()
        .into_iter()
        .find(|e| e.factory().is_some_and(|f| f.name() == "volume"))
        .map(|e| (e.property::<f64>("volume"), e.property::<bool>("mute")))
}

/// Nothing lost or doubled anywhere on the sound's way, the device never
/// given up nor stood in for at the end, and the sound clock taken anew on
/// each of `sinks` sinks opened after the first: a new controller and
/// model each (`audio_clock::install`), none of the sink before's.
fn sound_clean(run: &Run, sinks: u64) {
    let c = run.shared.health.diagnose(|d| d.c);
    assert_eq!(
        (c.frame_gaps, c.frame_overlaps, c.pcm_gaps, c.pcm_overlaps, c.pcm_backward, c.sink_gaps, c.sink_overlaps, c.alsa_xruns, c.alsa_restarts),
        (0, 0, 0, 0, 0, 0, 0, 0, 0),
        "{c:?}"
    );
    assert!(!run.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst), "no stand-in left");
    assert!(!run.shared.health.sink_given_up.load(std::sync::atomic::Ordering::Relaxed));
    let clock = &run.shared.audio_clock;
    let o = std::sync::atomic::Ordering::Relaxed;
    assert_eq!(
        clock.generations.load(o),
        1 + sinks,
        "the sound clock taken on each sink: used={} adopted={} resets={} fallback={} degraded={} unavailable={} unsupported={}",
        clock.used.load(o),
        clock.adopted.load(o),
        clock.resets.load(o),
        clock.fallback.load(o),
        clock.degraded.load(o),
        clock.unavailable.load(o),
        clock.unsupported.load(o)
    );
    assert_eq!(clock.abi_guard_failed.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(clock.used.load(std::sync::atomic::Ordering::Relaxed) > 0, "the sound clock followed the device");
    // No buffer out of the selector held up by the change: a 20-40 ms
    // frame's interval, not a wait.
    let audio = run.audio.lock().unwrap();
    let longest = audio.at.windows(2).map(|w| w[1] - w[0]).max().unwrap_or_default();
    assert!(longest < Duration::from_millis(250), "the sound stopped for {longest:?}");
}

/// The ALSA PCM the sound sink in the pipeline has open, if it is one.
fn sink_device(engine: &Engine) -> Option<String> {
    let sink = engine.audio_sink.lock().unwrap().clone()?;
    (sink.factory()?.name() == "alsasink").then(|| sink.property::<Option<String>>("device")).flatten()
}

/// The control socket's handling, with the sound's commands carried out on
/// a thread of their own as the main loop carries them out.
struct Control {
    commands: crate::ipc::Commands,
    clients: crate::ipc::Clients,
    query: crate::ipc::Query,
}

impl Control {
    fn new(engine: &Arc<Engine>) -> Self {
        let (commands, inbox) = std::sync::mpsc::channel::<(crate::ipc::Command, Option<super::SinkReply>)>();
        let carrier = Arc::clone(engine);
        std::thread::spawn(move || {
            for (command, done) in inbox {
                let mut done = done;
                match command {
                    crate::ipc::Command::Sound(command) => carrier.sound_command(command, &mut done),
                    _ => {
                        if let Some(done) = done.take() {
                            let _ = done.send(Err("not carried out in this test".into()));
                        }
                    }
                }
                if let Some(done) = done {
                    let _ = done.send(Ok(()));
                }
            }
        });
        let weak = Arc::downgrade(engine);
        let query: crate::ipc::Query = Arc::new(move |name: &str| match name {
            "audio-device" => weak.upgrade().map(|engine| serde_json::json!(sink_device(&engine))),
            _ => None,
        });
        Self { commands, clients: crate::ipc::Clients::default(), query }
    }

    fn ask(&self, command: &[serde_json::Value]) -> (String, Option<serde_json::Value>) {
        crate::ipc::handle(1, command, &self.clients, &self.query, &self.commands)
    }
}

/// HDMI to a Bluetooth headset under a playing channel, as the control
/// plane asks it: the device the player has open is the headset's, said
/// only once it is, and nothing else of the channel is touched. At 284efd1
/// the player had no such request ("invalid parameter"), and what the
/// control plane sent for a route change -- `audio-device` -- it does not
/// have either ("property unavailable"): the sound stayed on HDMI.
#[test]
fn a_headset_plan_moves_the_sound_device_under_the_channel() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, on_hdmi(), None, Duration::from_secs(2), move |engine| {
        let control = Control::new(engine);
        let before = sink_device(engine);
        let old_way = control.ask(&[serde_json::json!("set_property"), serde_json::json!("audio-device"), serde_json::json!(format!("alsa/{HEADSET_PCM}"))]);
        let answer = control.ask(&[serde_json::json!("mediabox-audio-plan"), headset_plan(HEADSET_PCM), serde_json::Value::Null, serde_json::json!(1)]);
        *into.lock().unwrap() = Some((before, old_way, answer, sink_device(engine)));
    });
    let (before, old_way, answer, after) = got.lock().unwrap().take().unwrap();
    assert_eq!(before.as_deref(), Some(HDMI_PCM), "the channel opened on HDMI");
    assert_eq!(old_way.0, "property unavailable", "audio-device is not made to look done");
    assert_eq!(answer.0, "success", "the headset plan: {answer:?}");
    assert_eq!(answer.1, Some(serde_json::json!(HEADSET_PCM)), "the answer says the device open");
    assert_eq!(after.as_deref(), Some(HEADSET_PCM), "the sound device the player has open");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
    sound_went_on(&run, 1, 0x101, 4_500_000_000);
    picture_handed_over(&run, false);
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 1, "one swap");
    sound_clean(&run, 1);
}

/// To the headset and back: the device and the level back where they were
/// (B), on the same channel, the same sound part, the same picture.
#[test]
fn a_headset_and_back_puts_the_television_s_sound_back() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let start = Audio { gain: 0.6f64.powi(3), ..on_hdmi() };
    let got = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let mut into = into.lock().unwrap();
        into.push((answered(&ask_plan(engine, headset_plan(HEADSET_PCM), None, 1)), sink_device(engine), level(engine)));
        std::thread::sleep(Duration::from_millis(1500));
        into.push((answered(&ask_plan(engine, hdmi_plan(60, false, false), None, 2)), sink_device(engine), level(engine)));
    });
    let got = got.lock().unwrap();
    // The volume element keeps its level as a float.
    let at = |level: Option<(f64, bool)>, want: f64| level.is_some_and(|(gain, muted)| (gain - want).abs() < 1e-6 && !muted);
    assert_eq!((&got[0].0, got[0].1.as_deref()), (&Ok(()), Some(HEADSET_PCM)), "on the headset");
    assert!(at(got[0].2, 1.0), "at the headset's own volume: {:?}", got[0].2);
    assert_eq!((&got[1].0, got[1].1.as_deref()), (&Ok(()), Some(HDMI_PCM)), "back on HDMI");
    assert!(at(got[1].2, 0.6f64.powi(3)), "at the television's volume: {:?}", got[1].2);
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 2, "two swaps");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
    sound_went_on(&run, 1, 0x101, 4_500_000_000);
    picture_handed_over(&run, false);
    sound_clean(&run, 2);
}

/// The level and mute alone (C): on the part as it plays, no device opened.
#[test]
fn a_plan_that_changes_the_level_only_opens_nothing() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(5, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 5), from: 0, to: u64::MAX }]);
    let got = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, on_hdmi(), None, Duration::from_millis(1500), move |engine| {
        let mut into = into.lock().unwrap();
        into.push((answered(&ask_plan(engine, hdmi_plan(50, false, false), None, 1)), level(engine)));
        into.push((answered(&ask_plan(engine, hdmi_plan(50, true, false), None, 2)), level(engine)));
    });
    let got = got.lock().unwrap();
    let at = |level: Option<(f64, bool)>, muted: bool| level.is_some_and(|(gain, m)| (gain - 0.125).abs() < 1e-6 && m == muted);
    assert!(got[0].0.is_ok() && at(got[0].1, false), "{:?}", got[0]);
    assert!(got[1].0.is_ok() && at(got[1].1, true), "{:?}", got[1]);
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 0, "no swap");
    assert_eq!(sink_device(&run.engine).as_deref(), Some(HDMI_PCM));
    sound_went_on(&run, 1, 0x101, 3_500_000_000);
    picture_handed_over(&run, false);
    sound_clean(&run, 0);
}

/// The headset's device will not open (D): answered so, the television's
/// device as it was -- the same sink, never closed: the headset is opened
/// ahead of its turn (FAZ 3E.5) and failed there -- the plan before in
/// force, the sound on.
#[test]
fn a_headset_that_will_not_open_leaves_the_television_s_sound() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, on_hdmi(), None, Duration::from_millis(1500), move |engine| {
        let sink = engine.audio_sink.lock().unwrap().clone();
        let answer = answered(&ask_plan(engine, headset_plan(ABSENT_PCM), None, 1));
        let same = engine.audio_sink.lock().unwrap().clone() == sink;
        *into.lock().unwrap() = Some((answer, sink_device(engine), engine.audio(), same));
    });
    let (answer, device, audio, same) = got.lock().unwrap().take().unwrap();
    assert!(answer.is_err(), "not answered as done: {answer:?}");
    assert_eq!(device.as_deref(), Some(HDMI_PCM), "the television's device");
    assert!(same, "the television's sink never closed");
    assert_eq!(audio, on_hdmi(), "the plan before in force");
    assert_eq!(run.shared.health.sink_open_failures.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1);
    sound_went_on(&run, 1, 0x101, 4_500_000_000);
    picture_handed_over(&run, false);
    // No sink after the first.
    sound_clean(&run, 0);
}

/// Requests one after another while one waits for its turn (E): the one
/// under way is answered only once its device is open; those after it but
/// the newest are answered superseded; the newest is done after it. The
/// headset's change does not undo the newer one's, and nothing is opened
/// for the ones that never were.
#[test]
fn the_newest_plan_wins_and_an_older_one_done_undoes_nothing() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, on_hdmi(), None, Duration::from_millis(1500), move |engine| {
        let (src, block) = hold_the_sink(engine);
        let headset = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        std::thread::sleep(Duration::from_millis(100));
        let back = ask_plan(engine, hdmi_plan(100, false, false), None, 2);
        let again = ask_plan(engine, headset_plan(HEADSET_PCM), None, 3);
        let last = ask_plan(engine, hdmi_plan(100, false, false), None, 4);
        // Nothing passes the selector: nothing is done, nothing answered.
        let early = headset.recv_timeout(Duration::from_millis(400));
        src.remove_probe(block);
        let answers = (answered(&headset), answered(&back), answered(&again), answered(&last));
        *into.lock().unwrap() = Some((early, answers, sink_device(engine)));
    });
    let (early, (headset, back, again, last), device) = got.lock().unwrap().take().unwrap();
    assert!(early.is_err(), "answered before its device was open: {early:?}");
    assert_eq!(headset, Ok(()));
    assert!(back.is_err() && again.is_err(), "the superseded: {back:?} {again:?}");
    assert_eq!(last, Ok(()));
    assert_eq!(device.as_deref(), Some(HDMI_PCM), "the newest plan in force");
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 2, "to the headset and back: no third");
    sound_went_on(&run, 1, 0x101, 4_500_000_000);
    picture_handed_over(&run, false);
}

/// A plan asked for the channel being left (F): called off by the change,
/// never made on the next; one for another channel's number refused; the
/// next channel opened on the plan in force, and the plan asked again for
/// it done on it.
#[test]
fn a_plan_for_the_channel_left_is_not_made_on_the_next() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(7, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 7), from: 0, to: u64::MAX }]);
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, on_hdmi(), None, Duration::from_millis(1500), move |engine| {
        let (src, block) = hold_the_sink(engine);
        let left = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        std::thread::sleep(Duration::from_millis(100));
        let path = engine.current_path().unwrap();
        engine.open(&path).expect("the next channel");
        src.remove_probe(block);
        let left = answered(&left);
        std::thread::sleep(Duration::from_millis(1000));
        let after_change = sink_device(engine);
        let other = answered(&ask_plan(engine, headset_plan(HEADSET_PCM), Some(9), 2));
        let asked_again = answered(&ask_plan(engine, headset_plan(HEADSET_PCM), None, 3));
        *into.lock().unwrap() = Some((left, after_change, other, asked_again, sink_device(engine)));
    });
    let (left, after_change, other, asked_again, device) = got.lock().unwrap().take().unwrap();
    assert!(left.is_err(), "called off: {left:?}");
    assert_eq!(after_change.as_deref(), Some(HDMI_PCM), "the next channel on the plan in force");
    assert_eq!(other, Err(mediabox_core::audio::LIVE_PLAN_OTHER_CHANNEL.to_string()));
    assert_eq!(asked_again, Ok(()));
    assert_eq!(device.as_deref(), Some(HEADSET_PCM));
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 1, "one swap, on the next channel");
}

/// AC-3 passed through to the television, to the headset and back (G):
/// the sound part built again for PCM and then for the bitstream, each
/// let go where the one before ended; the device opened in each part's
/// form; the picture, its decoder and the channel as they were.
#[test]
fn a_passed_through_sound_goes_to_the_headset_as_pcm_and_back() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x06, 0x101, vec![0x6A, 1, 0])];
    let ts = stream_for(7, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xBD, units: repeated(ac3(), 7), from: 0, to: u64::MAX }]);
    let start = Audio { passthrough: vec![mediabox_core::AudioCodec::Ac3], ..on_hdmi() };
    let got = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let mut into = into.lock().unwrap();
        let form = |engine: &Engine| (sink_device(engine), engine.bitstream(), engine.shared.info.lock().unwrap().passthrough);
        into.push((Ok(()), form(engine)));
        into.push((answered(&ask_plan(engine, headset_plan(HEADSET_PCM), None, 1)), form(engine)));
        std::thread::sleep(Duration::from_millis(1500));
        into.push((answered(&ask_plan(engine, hdmi_plan(100, false, true), None, 2)), form(engine)));
    });
    let got = got.lock().unwrap();
    assert_eq!(got[0], (Ok(()), (Some(HDMI_PCM.into()), true, true)), "passed through to the television");
    assert_eq!(got[1], (Ok(()), (Some(HEADSET_PCM.into()), false, false)), "PCM to the headset");
    assert_eq!(got[2], (Ok(()), (Some(HDMI_PCM.into()), true, true)), "passed through again");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
    // Three parts one after the other, the old never after the new.
    sound_went_on(&run, 3, 0x101, 5_500_000_000);
    picture_handed_over(&run, false);
    let c = run.shared.health.diagnose(|d| d.c);
    assert_eq!((c.frame_gaps, c.frame_overlaps, c.pcm_gaps, c.pcm_overlaps, c.alsa_xruns), (0, 0, 0, 0, 0), "{c:?}");
    assert!(!run.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
    let audio = run.audio.lock().unwrap();
    let longest = audio.at.windows(2).map(|w| w[1] - w[0]).max().unwrap_or_default();
    assert!(longest < Duration::from_millis(250), "the sound stopped for {longest:?}");
}

/// A headset that will not open, for a passed-through sound (D, G): tried
/// before anything is taken down, and nothing is.
#[test]
fn a_headset_that_will_not_open_takes_nothing_down() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x06, 0x101, vec![0x6A, 1, 0])];
    let ts = stream_for(5, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xBD, units: repeated(ac3(), 5), from: 0, to: u64::MAX }]);
    let start = Audio { passthrough: vec![mediabox_core::AudioCodec::Ac3], ..on_hdmi() };
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let opened = engine.swaps.lock().unwrap().opened;
        let answer = answered(&ask_plan(engine, headset_plan(ABSENT_PCM), None, 1));
        std::thread::sleep(Duration::from_millis(500));
        let swapped = engine.swaps.lock().unwrap().opened - opened;
        *into.lock().unwrap() = Some((answer, sink_device(engine), engine.bitstream(), engine.audio(), swapped));
    });
    let (answer, device, bitstream, audio, swapped) = got.lock().unwrap().take().unwrap();
    assert!(answer.is_err(), "{answer:?}");
    assert_eq!((device.as_deref(), bitstream), (Some(HDMI_PCM), true));
    assert_eq!(audio, start);
    assert_eq!(swapped, 0, "nothing swapped");
    sound_went_on(&run, 1, 0x101, 3_500_000_000);
    picture_handed_over(&run, false);
}

/// `ao-reload` after a mode change (H) opens the plan in force again --
/// the same device -- and one asked while a plan's swap waits is done by
/// that swap's open: the headset's, not the device before.
#[test]
fn a_reload_opens_the_plan_in_force_again() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(6, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 6), from: 0, to: u64::MAX }]);
    let got = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, on_hdmi(), None, Duration::from_millis(1500), move |engine| {
        let mut into = into.lock().unwrap();
        let (reply, wait) = std::sync::mpsc::sync_channel(1);
        engine.reopen_audio_sink(Some(reply));
        into.push((answered(&wait), sink_device(engine)));
        // Played on before it is held: its sound clock taken too.
        std::thread::sleep(Duration::from_millis(300));
        let (src, block) = hold_the_sink(engine);
        let plan = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        std::thread::sleep(Duration::from_millis(100));
        let (reply, reload) = std::sync::mpsc::sync_channel(1);
        engine.reopen_audio_sink(Some(reply));
        src.remove_probe(block);
        into.push((answered(&plan), sink_device(engine)));
        into.push((answered(&reload), sink_device(engine)));
    });
    let got = got.lock().unwrap();
    assert_eq!(got[0], (Ok(()), Some(HDMI_PCM.into())), "the same device again");
    assert_eq!(got[1], (Ok(()), Some(HEADSET_PCM.into())));
    assert_eq!(got[2], (Ok(()), Some(HEADSET_PCM.into())), "the reload done by the plan's open");
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 2, "the reload, then the plan's");
    // Every sink after the first played: the reload's and the headset's.
    assert_eq!(run.engine.audio().device.as_deref(), Some(HEADSET_PCM));
    sound_went_on(&run, 1, 0x101, 4_500_000_000);
    picture_handed_over(&run, false);
    sound_clean(&run, 2);
}

/// The PCM a bitstream goes on: the device's own, its channel status in
/// mpv's form. (The tests' `null` devices take no arguments.)
#[test]
fn a_bitstream_is_sent_on_the_device_with_its_channel_status() {
    assert_eq!(super::bitstream_pcm("hdmi:CARD=rockchiphdmi0,DEV=0"), "hdmi:CARD=rockchiphdmi0,DEV=0,AES0=0x06,AES1=0x82,AES2=0x00,AES3=0x02");
}

// FAZ 3E.1: a plan's change as one transaction, with one deadline of its
// own. The sound part's end is held where a test says (`hold_sound_end`),
// the reader and the rest of the channel going on: the turn of a part built
// again for a plan waits as long as the part before it has not ended.

/// The sound part playing now: its end, when it is told it (a plan's or a
/// PMT's part built again), held at its source until the probe is taken
/// off. Everything before the end goes on.
fn hold_sound_end(engine: &Arc<Engine>) -> (gst::Pad, gst::PadProbeId) {
    let source = engine.shared.sound.lock().unwrap().as_ref().expect("a sound part").source.clone();
    let src = source.static_pad("src").unwrap();
    let id = src
        .add_probe(gst::PadProbeType::BLOCK | gst::PadProbeType::EVENT_DOWNSTREAM, |_, info| match &info.data {
            Some(gst::PadProbeData::Event(event)) if event.type_() == gst::EventType::Eos => gst::PadProbeReturn::Ok,
            _ => gst::PadProbeReturn::Pass,
        })
        .unwrap();
    (src, id)
}

/// Whether the engine reached `state` within `within`: looked at every
/// 5 ms, never a time taken for it.
fn until(within: Duration, state: impl Fn() -> bool) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < within {
        if state() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    state()
}

fn handover_pending(engine: &Engine) -> bool {
    engine.sound_handover.lock().unwrap().is_some()
}

/// Buffers out of the sound selector from now on.
fn count_sound_out(engine: &Engine) -> Arc<std::sync::atomic::AtomicU64> {
    let count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let into = Arc::clone(&count);
    engine.audio_selector.static_pad("src").unwrap().add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        into.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        gst::PadProbeReturn::Ok
    });
    count
}

fn passed_through() -> (Vec<(u64, Vec<u8>)>, Audio) {
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x06, 0x101, vec![0x6A, 1, 0])];
    let ts = stream_for(9, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xBD, units: repeated(ac3(), 9), from: 0, to: u64::MAX }]);
    (ts, Audio { passthrough: vec![mediabox_core::AudioCodec::Ac3], ..on_hdmi() })
}

/// A headset that opened for its try and is gone when its turn comes (6):
/// the answer comes only once the television's sound is back as it was --
/// its device open for the bitstream, a part built again for it, the plan
/// before in force -- and says why. The headset is gone between its try
/// and its turn (`alsa_pcm`: `lost-after-try:`).
#[test]
fn a_headset_lost_after_its_try_is_answered_only_with_the_television_s_sound_back() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let gone = std::env::temp_dir().join(format!("mediabox-live-headset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&gone);
    std::fs::create_dir_all(&gone).unwrap();
    let pcm = format!("lost-after-try:{}", gone.display());
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let (src, held) = hold_sound_end(engine);
        let wait = ask_plan(engine, headset_plan(&pcm), None, 1);
        let pending = until(Duration::from_secs(3), || handover_pending(engine));
        std::fs::remove_dir_all(&gone).unwrap();
        src.remove_probe(held);
        let answer = answered(&wait);
        let form = || (sink_device(engine), engine.bitstream(), engine.shared.info.lock().unwrap().passthrough, engine.audio());
        let at_answer = form();
        // And so it stays while the sound goes on: nothing of the parts
        // taken down sets the device after the answer.
        let out = count_sound_out(engine);
        let playing = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 20);
        *into.lock().unwrap() = Some((pending, answer, at_answer, playing, form()));
    });
    let (pending, answer, form, playing, later) = got.lock().unwrap().take().unwrap();
    assert!(pending, "the headset's part waiting for its turn: {answer:?}");
    assert!(answer.as_ref().is_err_and(|why| !why.is_empty()), "said why: {answer:?}");
    assert_eq!(form, (Some(HDMI_PCM.into()), true, true, start.clone()), "the television's sound back when answered: {answer:?}");
    assert!(playing, "the sound goes on");
    assert_eq!(later, (Some(HDMI_PCM.into()), true, true, start), "and so it stays");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
    picture_handed_over(&run, false);
}

/// The device opened again while a plan's part waits for its turn (FAZ
/// 3E.4): the plan before's -- its part is the one playing -- in the form
/// before, never the plan's ahead of its part; the plan's device opened at
/// the part's turn, once.
#[test]
fn a_device_opened_again_while_a_plan_s_part_waits_is_the_plan_before_s() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let (src, held) = hold_sound_end(engine);
        let wait = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        let pending = until(Duration::from_secs(3), || handover_pending(engine));
        let (reply, reload) = std::sync::mpsc::sync_channel(1);
        engine.reopen_audio_sink(Some(reply));
        let reloaded = answered(&reload);
        let waiting = (sink_device(engine), engine.bitstream(), engine.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
        let opened = engine.swaps.lock().unwrap().opened;
        src.remove_probe(held);
        let answer = answered(&wait);
        let after = (sink_device(engine), engine.bitstream(), engine.swaps.lock().unwrap().opened - opened);
        *into.lock().unwrap() = Some((pending, reloaded, waiting, answer, after));
    });
    let (pending, reloaded, waiting, answer, after) = got.lock().unwrap().take().unwrap();
    assert!(pending, "the headset's part waiting for its turn");
    assert_eq!(reloaded, Ok(()));
    assert_eq!(waiting, (Some(HDMI_PCM.into()), true, false), "the television, for its bitstream, until the headset's part's turn");
    assert_eq!(answer, Ok(()));
    assert_eq!(after, (Some(HEADSET_PCM.into()), false, 1), "the headset opened once, at its part's turn");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
    picture_handed_over(&run, false);
}

/// The headset lost under a change back to the television (FAZ 3E.3, F2):
/// while the television's part waits for its turn the sound goes to the
/// stand-in, not to the television in the headset's form; the television
/// opened for its bitstream at the part's turn, once, and the device lost
/// answered by that open.
#[test]
fn a_headset_lost_under_a_change_back_waits_on_the_stand_in() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, _) = passed_through();
    let gone = std::env::temp_dir().join(format!("mediabox-live-lost-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&gone);
    std::fs::create_dir_all(&gone).unwrap();
    let pcm = format!("lost-after-try:{}", gone.display());
    let start = Audio { device: Some(pcm.clone()), ..on_hdmi() };
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let (src, held) = hold_sound_end(engine);
        let wait = ask_plan(engine, hdmi_plan(100, false, true), None, 1);
        let pending = until(Duration::from_secs(3), || handover_pending(engine));
        std::fs::remove_dir_all(&gone).unwrap();
        let (reply, lost) = std::sync::mpsc::sync_channel(1);
        engine.reopen_audio_sink(Some(reply));
        let stand_in = until(Duration::from_secs(1), || engine.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
        // Its retries go on meanwhile: none opens the television.
        std::thread::sleep(Duration::from_millis(400));
        let waiting = (sink_device(engine), engine.bitstream(), engine.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
        let opened = engine.swaps.lock().unwrap().opened;
        src.remove_probe(held);
        let answer = answered(&wait);
        let lost = answered(&lost);
        let after = (sink_device(engine), engine.bitstream(), engine.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst), engine.swaps.lock().unwrap().opened - opened);
        *into.lock().unwrap() = Some((pending, stand_in, waiting, answer, lost, after));
    });
    let (pending, stand_in, waiting, answer, lost, after) = got.lock().unwrap().take().unwrap();
    assert!(pending, "the television's part waiting for its turn");
    assert!(stand_in, "the stand-in for the headset lost");
    assert_eq!(waiting, (None, false, true), "nothing opened for the television ahead of its part");
    assert_eq!(answer, Ok(()));
    assert_eq!(lost, Ok(()), "the device lost answered by the plan's open");
    assert_eq!(after, (Some(HDMI_PCM.into()), true, false, 1), "the television opened once, for its bitstream");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
    picture_handed_over(&run, false);
}

/// Each sound sink's ring buffer acquired -- the device's stream started,
/// for a Bluetooth headset the A2DP stream -- by name and when, from the
/// sink's own word (gstaudiobasesink.c setcaps, "acquire ringbuffer").
struct Acquires {
    seen: Arc<Mutex<Vec<(String, std::time::Instant)>>>,
    function: Option<gst::log::DebugLogFunction>,
}

impl Acquires {
    fn watch() -> Self {
        let seen: Arc<Mutex<Vec<(String, std::time::Instant)>>> = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&seen);
        gst::log::set_threshold_for_name("audiobasesink", gst::DebugLevel::Debug);
        let function = gst::log::add_log_function(move |category, _, _, _, _, object, message| {
            if category.name() != "audiobasesink" || !message.get().is_some_and(|m| m.as_str().starts_with("acquire ringbuffer")) {
                return;
            }
            let name = object.map(|o| o.to_string()).unwrap_or_default();
            into.lock().unwrap().push((name.trim_matches(|c| c == '<' || c == '>').to_string(), std::time::Instant::now()));
        });
        Self { seen, function: Some(function) }
    }

    fn of(&self, sink: &str) -> Vec<std::time::Instant> {
        self.seen.lock().unwrap().iter().filter(|(name, _)| name == sink).map(|(_, at)| *at).collect()
    }
}

impl Drop for Acquires {
    fn drop(&mut self) {
        if let Some(function) = self.function.take() {
            gst::log::remove_log_function(function);
        }
    }
}

/// FAZ 3E.5's cause: a plan's device was opened at its part's turn, on the
/// sound's own thread, the clock going on meanwhile -- the GameBuds' A2DP
/// stream took 272-448 ms to start, and the part's first 4-9 buffers came
/// to the sink after their time and were thrown away, 8 changes in 8. Its
/// device is open now, its ring buffer acquired, before the part before
/// has ended -- and only once: the part brings the format it was opened in.
#[test]
fn a_plan_s_device_is_open_before_the_sound_before_it_ends() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let acquires = Acquires::watch();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let (src, held) = hold_sound_end(engine);
        let wait = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        let pending = until(Duration::from_secs(3), || handover_pending(engine));
        let before = sink_device(engine);
        let released = std::time::Instant::now();
        src.remove_probe(held);
        let answer = answered(&wait);
        let sink = engine.audio_sink.lock().unwrap().as_ref().map(|s| s.name().to_string()).unwrap_or_default();
        *into.lock().unwrap() = Some((pending, before, released, answer, sink, sink_device(engine)));
    });
    let (pending, before, released, answer, sink, after) = got.lock().unwrap().take().unwrap();
    assert!(pending, "the headset's part waiting for its turn");
    assert_eq!(before, Some(HDMI_PCM.into()), "the television until the turn");
    assert_eq!(answer, Ok(()));
    assert_eq!(after, Some(HEADSET_PCM.into()));
    let acquired = acquires.of(&sink);
    assert_eq!(acquired.len(), 1, "{sink}'s ring buffer acquired once: {:?}", acquires.seen.lock().unwrap());
    assert!(acquired[0] < released, "{sink}'s ring buffer acquired {:?} after the part before ended", acquired[0] - released);
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
}

/// The opening of a plan's device held where it is said ready: it waits on
/// what the test sends.
fn latch_prepare(engine: &Engine) -> std::sync::mpsc::SyncSender<()> {
    let (go, latch) = std::sync::mpsc::sync_channel(1);
    *engine.prepare_latch.lock().unwrap() = Some(latch);
    go
}

/// The plan's device opened and held at the latch.
fn prepare_held(engine: &Engine) -> bool {
    until(Duration::from_secs(3), || engine.prepare_latch.lock().unwrap().is_none())
}

/// Sound sinks in the pipeline: alsasinks, and the stand-in -- a fakesink
/// after the sound selector.
fn sound_sinks(engine: &Engine) -> usize {
    let selector = engine.audio_selector.clone();
    engine
        .pipeline
        .children()
        .iter()
        .filter(|e| {
            let after_selector = e.static_pad("sink").and_then(|p| p.peer()).and_then(|p| p.parent_element()).is_some_and(|p| p == selector);
            e.factory().is_some_and(|f| f.name() == "alsasink" || (f.name() == "fakesink" && after_selector))
        })
        .count()
}

/// Buffers reaching `sink`, each with when.
fn buffers_at(sink: &gst::Element) -> Arc<Mutex<Vec<std::time::Instant>>> {
    let at = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&at);
    sink.static_pad("sink").unwrap().add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        into.lock().unwrap().push(std::time::Instant::now());
        gst::PadProbeReturn::Ok
    });
    at
}

/// While a plan's device is opened ahead of its turn, the device before
/// plays the sound on, the plan's token does not own it and no part is
/// asked for it; open, the device is outside the pipeline and nothing
/// reaches it until the turn takes it, in one swap: from then on nothing
/// reaches the device before. One sound sink in the pipeline throughout.
#[test]
fn a_plan_s_device_opened_ahead_is_not_the_route_until_its_turn() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let go = latch_prepare(engine);
        let old = engine.audio_sink.lock().unwrap().clone().unwrap();
        let old_at = buffers_at(&old);
        let wait = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        let held = prepare_held(engine);
        let out = count_sound_out(engine);
        let played_on = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 10);
        let no_owner = engine.plans.lock().unwrap().owner.is_none();
        let none_asked = engine.shared.sound_rebuild.lock().unwrap().asked.is_none();
        let preparing = (sink_device(engine), engine.bitstream(), no_owner, none_asked, handover_pending(engine), sound_sinks(engine));
        let (src, hold) = hold_sound_end(engine);
        go.send(()).unwrap();
        let built = until(Duration::from_secs(3), || handover_pending(engine));
        let prepared = engine.prepared.lock().unwrap().as_ref().map(|p| p.sink.clone());
        let owner = engine.plans.lock().unwrap().owner.is_some();
        let ready = prepared.as_ref().map(|sink| (sink.parent().is_none(), sound_sinks(engine), owner));
        let new_at = prepared.as_ref().map(buffers_at);
        let before_turn = new_at.as_ref().map(|at| at.lock().unwrap().len());
        src.remove_probe(hold);
        let answer = answered(&wait);
        let new_count = new_at.as_ref().map(|at| until(Duration::from_secs(3), || at.lock().unwrap().len() >= 10));
        let taken = engine.audio_sink.lock().unwrap().clone() == prepared;
        let first_new = new_at.as_ref().and_then(|at| at.lock().unwrap().first().copied());
        let old_after = first_new.map(|first| old_at.lock().unwrap().iter().filter(|at| **at >= first).count());
        *into.lock().unwrap() = Some((held, played_on, preparing, built, ready, before_turn, answer, new_count, taken, old_after, sound_sinks(engine), sink_device(engine)));
    });
    let (held, played_on, preparing, built, ready, before_turn, answer, new_count, taken, old_after, sinks, device) = got.lock().unwrap().take().unwrap();
    assert!(held, "the headset opened and held");
    assert!(played_on, "the television plays the sound on while the headset opens");
    assert_eq!(preparing, (Some(HDMI_PCM.into()), true, true, true, false, 1), "the television's, no token, no part asked, one sink");
    assert!(built, "the headset's part built once its device opened");
    assert_eq!(ready, Some((true, 1, true)), "open out of the pipeline, one sink in it, the token owning the sound");
    assert_eq!(before_turn, Some(0), "nothing reached the headset before its turn");
    assert_eq!(answer, Ok(()));
    assert_eq!(new_count, Some(true), "the sound on the headset after its turn");
    assert!(taken, "the device opened ahead is the one the turn put in");
    assert_eq!(old_after, Some(0), "nothing reached the television after the headset's first buffer");
    assert_eq!((sinks, device), (1, Some(HEADSET_PCM.into())));
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1, "the channel was not opened again");
}

/// A plan whose device is still opening at its deadline: called off with
/// nothing of it done -- the television's sound in force, no part asked,
/// no owner -- and its device, open after, closed and never used.
#[test]
fn a_plan_whose_device_opens_after_its_deadline_changes_nothing() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let go = latch_prepare(engine);
        let opened = engine.swaps.lock().unwrap().opened;
        let wait = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        let held = prepare_held(engine);
        let answer = answered(&wait);
        let at_answer = (sink_device(engine), engine.bitstream(), engine.audio(), engine.plans.lock().unwrap().owner.is_none());
        go.send(()).unwrap();
        let out = count_sound_out(engine);
        let played = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 20);
        let closed = engine.prepared.lock().unwrap().is_none();
        let none_asked = engine.shared.sound_rebuild.lock().unwrap().asked.is_none();
        let swapped = engine.swaps.lock().unwrap().opened - opened;
        let after = (sink_device(engine), engine.bitstream(), engine.audio(), closed, none_asked, handover_pending(engine), swapped, sound_sinks(engine));
        *into.lock().unwrap() = Some((held, answer, at_answer, played, after));
    });
    let (held, answer, at_answer, played, after) = got.lock().unwrap().take().unwrap();
    assert!(held);
    assert!(answer.as_ref().is_err_and(|why| why.contains(&super::PLAN_DEADLINE_MS.to_string())), "{answer:?}");
    assert_eq!(at_answer, (Some(HDMI_PCM.into()), true, start.clone(), true), "the television's sound in force when answered");
    assert!(played, "the sound goes on");
    assert_eq!(after, (Some(HDMI_PCM.into()), true, start, true, true, false, 0, 1), "the headset closed, nothing asked, nothing swapped");
    assert!(!run.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// The device opened again while a plan's device opens ahead of its turn
/// (a mode, a device lost): the device in force, the television; the
/// headset, opened, untouched by it, and the turn's.
#[test]
fn a_device_opened_again_while_a_plan_s_device_opens_is_the_one_in_force() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let go = latch_prepare(engine);
        let wait = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        let held = prepare_held(engine);
        let (reply, reload) = std::sync::mpsc::sync_channel(1);
        engine.reopen_audio_sink(Some(reply));
        let reloaded = answered(&reload);
        let during = (sink_device(engine), engine.bitstream(), sound_sinks(engine));
        go.send(()).unwrap();
        let answer = answered(&wait);
        *into.lock().unwrap() = Some((held, reloaded, during, answer, sink_device(engine), engine.bitstream()));
    });
    let (held, reloaded, during, answer, device, bitstream) = got.lock().unwrap().take().unwrap();
    assert!(held);
    assert_eq!(reloaded, Ok(()));
    assert_eq!(during, (Some(HDMI_PCM.into()), true, 1), "the television, for its bitstream, opened again");
    assert_eq!(answer, Ok(()));
    assert_eq!((device, bitstream), (Some(HEADSET_PCM.into()), false));
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// A plan whose device will not open: answered with why at once, the
/// television's sound, its form and the sound's owner as they were, no
/// part asked; the sound never stops.
#[test]
fn a_plan_whose_device_will_not_open_leaves_the_route_as_it_was() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let old = engine.audio_sink.lock().unwrap().clone();
        let out = count_sound_out(engine);
        let answer = answered(&ask_plan(engine, headset_plan(ABSENT_PCM), None, 1));
        let played = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 20);
        let none_asked = engine.shared.sound_rebuild.lock().unwrap().asked.is_none();
        let closed = engine.prepared.lock().unwrap().is_none();
        let (no_owner, no_change) = {
            let plans = engine.plans.lock().unwrap();
            (plans.owner.is_none(), plans.active.is_none())
        };
        let same = engine.audio_sink.lock().unwrap().clone() == old;
        let after = (
            same,
            sink_device(engine),
            engine.bitstream(),
            engine.audio(),
            no_owner,
            no_change,
            none_asked,
            closed,
            sound_sinks(engine),
        );
        *into.lock().unwrap() = Some((answer, played, after));
    });
    let (answer, played, after) = got.lock().unwrap().take().unwrap();
    assert!(answer.as_ref().is_err_and(|why| why.contains(ABSENT_PCM)), "{answer:?}");
    assert!(played, "the sound goes on");
    assert_eq!(after, (true, Some(HDMI_PCM.into()), true, start, true, true, true, true, 1), "the same sink, plan and owner");
    assert!(!run.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// An open of the device asked for before a plan's turn and come after it
/// (`run_swap`: opened since): the device the turn put in stays, the same
/// sink, nothing opened again.
#[test]
fn an_open_asked_before_a_plan_s_turn_and_come_after_it_changes_nothing() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let _run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let before_turn = engine.swaps.lock().unwrap().opened;
        let answer = answered(&ask_plan(engine, headset_plan(HEADSET_PCM), None, 1));
        let sink = engine.audio_sink.lock().unwrap().clone();
        let opened = engine.swaps.lock().unwrap().opened;
        let (reply, stale) = std::sync::mpsc::sync_channel(1);
        engine.swaps.lock().unwrap().pending = Some(super::PendingSwap {
            asked: std::time::Instant::now(),
            after: before_turn,
            failed: 0,
            replies: vec![reply],
            reopen: true,
            plan: false,
        });
        engine.queue_swap();
        let stale = answered(&stale);
        let same = engine.audio_sink.lock().unwrap().clone() == sink;
        let after = (same, engine.swaps.lock().unwrap().opened - opened, sink_device(engine), engine.bitstream());
        *into.lock().unwrap() = Some((answer, stale, after));
    });
    let (answer, stale, after) = got.lock().unwrap().take().unwrap();
    assert_eq!(answer, Ok(()));
    assert_eq!(stale, Ok(()));
    assert_eq!(after, (true, 0, Some(HEADSET_PCM.into()), false), "the headset's sink as the turn put it");
}

/// A plan whose part was built and whose turn never comes (7): its
/// deadline ends it, the client answered before its own patience runs out
/// and only with the television's sound in force; the part's turn coming
/// after that changes neither the device nor the plan.
#[test]
fn a_plan_whose_turn_never_comes_ends_at_its_deadline_and_never_after() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let control = Control::new(engine);
        let (src, held) = hold_sound_end(engine);
        let opened = engine.swaps.lock().unwrap().opened;
        let asked = std::time::Instant::now();
        let (pending, answer) = std::thread::scope(|s| {
            let asking = s.spawn(|| control.ask(&[serde_json::json!("mediabox-audio-plan"), headset_plan(HEADSET_PCM), serde_json::Value::Null, serde_json::json!(1)]));
            let pending = until(Duration::from_secs(3), || handover_pending(engine));
            (pending, asking.join().unwrap())
        });
        let took = asked.elapsed();
        let at_answer = (sink_device(engine), engine.bitstream(), engine.audio());
        let closed = engine.prepared.lock().unwrap().is_none();
        let out = count_sound_out(engine);
        src.remove_probe(held);
        let turned = until(Duration::from_secs(3), || !handover_pending(engine) && out.load(std::sync::atomic::Ordering::SeqCst) >= 10);
        let after = (sink_device(engine), engine.bitstream(), engine.audio(), engine.swaps.lock().unwrap().opened - opened);
        *into.lock().unwrap() = Some((pending, answer, took, at_answer, closed, turned, after));
    });
    let (pending, answer, took, at_answer, closed, turned, after) = got.lock().unwrap().take().unwrap();
    assert!(pending, "the headset's part built and waiting for its turn");
    assert!(closed, "the headset opened ahead of its turn closed at the deadline");
    assert_ne!(answer.0, "success", "{answer:?}");
    assert!(took < Duration::from_millis(mediabox_core::audio::LIVE_PLAN_ANSWER_MS), "answered in {took:?}");
    assert_eq!(at_answer, (Some(HDMI_PCM.into()), true, start.clone()), "answered with the television's sound in force");
    assert!(turned, "the part after the held one played");
    assert_eq!(after, (Some(HDMI_PCM.into()), true, start, 0), "nothing changed by the turn that came late");
    assert!(!run.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// A plan without its channel's number or its own (9): refused as asked
/// wrongly, before it is queued; nothing changes. The channel here has a
/// number (5).
#[test]
fn a_plan_without_its_channel_or_number_is_refused_before_it_is_queued() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(5, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 5), from: 0, to: u64::MAX }]);
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, on_hdmi(), Some(5), Duration::from_millis(1500), move |engine| {
        engine.open_gate(5, "matched").expect("its gate");
        let control = Control::new(engine);
        let plan = || headset_plan(HEADSET_PCM);
        let word = |w: &str| serde_json::json!(w);
        let wrong = [
            control.ask(&[word("mediabox-audio-plan"), plan()]),
            control.ask(&[word("mediabox-audio-plan"), plan(), serde_json::json!(5)]),
            control.ask(&[word("mediabox-audio-plan"), plan(), serde_json::json!(5), serde_json::json!(0)]),
            control.ask(&[word("mediabox-audio-plan"), plan(), serde_json::Value::Null, serde_json::json!(1)]),
            control.ask(&[word("mediabox-audio-plan"), plan(), word("5"), serde_json::json!(1)]),
            control.ask(&[word("mediabox-audio-plan"), plan(), serde_json::json!(5), serde_json::json!(-1)]),
        ];
        let queued = engine.plans.lock().unwrap().seq;
        let device = sink_device(engine);
        let right = control.ask(&[word("mediabox-audio-plan"), plan(), serde_json::json!(5), serde_json::json!(1)]);
        *into.lock().unwrap() = Some((wrong, queued, device, right));
    });
    let (wrong, queued, device, right) = got.lock().unwrap().take().unwrap();
    for (n, answer) in wrong.iter().enumerate() {
        assert_eq!(answer.0, "invalid parameter", "request {n}: {answer:?}");
    }
    assert_eq!(queued, 0, "none of them taken");
    assert_eq!(device.as_deref(), Some(HDMI_PCM));
    assert_eq!(right, ("success".to_string(), Some(serde_json::json!(HEADSET_PCM))));
    assert_eq!(run.engine.swaps.lock().unwrap().opened, 1);
}

/// A channel change and a plan's deadline at once (8), its part built and
/// waiting for its turn: released together by a barrier, in whichever
/// order they take the engine. The plan is answered as not done, the next
/// channel opens on the plan before, and nothing of the plan for the
/// channel left -- its part's turn, its deadline's putting back -- is
/// done on the next.
#[test]
fn a_channel_change_at_a_plan_s_deadline_leaves_the_plan_to_the_channel_left() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let (src, held) = hold_sound_end(engine);
        let opened = engine.swaps.lock().unwrap().opened;
        let wait = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
        let pending = until(Duration::from_secs(3), || handover_pending(engine));
        let id = engine.plans.lock().unwrap().active.as_ref().map(|c| c.id).expect("the plan under way");
        let path = engine.current_path().unwrap();
        let both = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            s.spawn(|| {
                both.wait();
                engine.open(&path).expect("the next channel");
            });
            s.spawn(|| {
                both.wait();
                engine.plan_deadline(id);
            });
        });
        let answer = answered(&wait);
        src.remove_probe(held);
        let out = count_sound_out(engine);
        let playing = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 20);
        let after = (sink_device(engine), engine.bitstream(), engine.audio(), engine.swaps.lock().unwrap().opened - opened);
        let left = engine.plans.lock().unwrap().active.is_none() && { let r = engine.shared.sound_rebuild.lock().unwrap(); r.asked.is_none() && r.taken.is_none() };
        *into.lock().unwrap() = Some((pending, answer, playing, after, left));
    });
    let (pending, answer, playing, after, left) = got.lock().unwrap().take().unwrap();
    assert!(pending, "the headset's part built and waiting for its turn");
    assert!(answer.is_err(), "not done: {answer:?}");
    // Called off by the change, the plan before in force on the next: not
    // a failed state, which is what a plan left to its putting back on a
    // channel that is gone would end in.
    assert!(answer.as_ref().is_err_and(|why| !why.starts_with(mediabox_core::audio::LIVE_PLAN_FAILED_STATE)), "{answer:?}");
    assert!(playing, "the next channel's sound plays");
    assert_eq!(after, (Some(HDMI_PCM.into()), true, start, 0), "the next channel on the plan before, nothing swapped");
    assert!(left, "nothing of the plan left");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(!run.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
}

// FAZ 3E.2: whose a part built again for a plan is (`PlanToken`). The
// plan's change and the plan before put back each have their own token;
// only the one that owns the sound may set the device's mode, give the
// selector its input, swap the device, let its part pass or answer. The
// deadline and the plan part's turn decide under the same locks, and
// whichever comes first is the outcome -- forced here in each order, and
// left to a barrier.

/// The change under way's token.
fn plan_token(engine: &Engine) -> super::PlanToken {
    engine.plans.lock().unwrap().active.as_ref().map(|change| change.token()).expect("a plan under way")
}

/// The sound part whose token is `token`, built: its source's next pad,
/// where everything of it that passes its hold goes first.
fn part_of(engine: &Engine, token: super::PlanToken) -> Option<gst::Pad> {
    let sound = engine.shared.sound.lock().unwrap();
    let parts = sound.as_ref().filter(|parts| parts.token == Some(token))?;
    parts.source.static_pad("src")?.peer()
}

/// Buffers that pass `pad` from now on.
fn count_on(pad: &gst::Pad) -> Arc<std::sync::atomic::AtomicU64> {
    let count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let into = Arc::clone(&count);
    pad.add_probe(gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST, move |_, _| {
        into.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        gst::PadProbeReturn::Ok
    });
    count
}

/// The sound selector's input now, and whether it is one of the inputs of
/// the part whose token is `token`.
fn selector_given(engine: &Engine, token: super::PlanToken) -> (Option<String>, bool) {
    let active = engine.audio_selector.property::<Option<gst::Pad>>("active-pad");
    let sound = engine.shared.sound.lock().unwrap();
    let theirs = sound.as_ref().filter(|parts| parts.token == Some(token)).is_some_and(|parts| {
        parts.pads.lock().unwrap().iter().any(|(element, pad)| *element == engine.audio_selector && Some(pad) == active.as_ref())
    });
    (active.map(|pad| pad.name().to_string()), theirs)
}

/// The part after the one that played out was refused at its turn.
fn refused_at_turn(engine: &Engine) -> bool {
    engine.sound_handover.lock().unwrap().as_ref().is_some_and(|handover| handover.old.is_none())
}

/// A plan whose part is built and waits for its turn.
struct Waiting {
    /// The end of the part before it, held.
    held: (gst::Pad, gst::PadProbeId),
    /// Where its answer comes.
    wait: std::sync::mpsc::Receiver<Result<(), String>>,
    token: super::PlanToken,
    /// Buffers of its part that passed its hold.
    counted: Arc<std::sync::atomic::AtomicU64>,
}

/// A headset plan asked under AC-3 passed through to the television, its
/// part built and waiting for its turn (the television's part's end held).
fn plan_waiting(engine: &Arc<Engine>) -> Waiting {
    let held = hold_sound_end(engine);
    let wait = ask_plan(engine, headset_plan(HEADSET_PCM), None, 1);
    assert!(until(Duration::from_secs(3), || handover_pending(engine)), "the plan's part built and waiting");
    let token = plan_token(engine);
    assert!(until(Duration::from_secs(1), || part_of(engine, token).is_some()), "its part is the token's");
    let counted = count_on(&part_of(engine, token).unwrap());
    Waiting { held, wait, token, counted }
}

/// (5)(7) The deadline first, then the part before's end -- the plan's
/// part's turn: the plan's token is retired, the part never passes (not
/// one buffer reaches its parser, let alone the selector or the device),
/// the device's mode, the device and the selector's input stay the plan
/// before's, and a part for it under a token of its own goes on in its
/// place. The reader is held meanwhile where it takes a plan's part, so
/// that the turn comes before the plan before's part is built.
#[test]
fn a_plan_called_off_at_its_deadline_never_passes_a_buffer_when_its_turn_comes() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let Waiting { held: (src, held), wait, token, counted } = plan_waiting(engine);
        // The device: its PCM, its form, the swaps made.
        let device = |engine: &Engine| (sink_device(engine), engine.bitstream(), engine.swaps.lock().unwrap().opened);
        let before = device(engine);
        let reader = engine.shared.aid_request.lock().unwrap();
        engine.plan_deadline(token.id);
        let restore = plan_token(engine);
        src.remove_probe(held);
        let refused = until(Duration::from_secs(3), || refused_at_turn(engine));
        let at_turn = (counted.load(std::sync::atomic::Ordering::SeqCst), device(engine), selector_given(engine, token).1);
        // Its decoder's first caps, late, as they would come if anything of
        // it had passed: the mode is not set, the device not swapped.
        let caps = engine.part_sound_mode(false, Some(&std::sync::atomic::AtomicBool::new(false)), Some(token));
        let after_caps = (device(engine), selector_given(engine, token).1);
        drop(reader);
        let answer = answered(&wait);
        let out = count_sound_out(engine);
        let playing = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 20);
        let later = (counted.load(std::sync::atomic::Ordering::SeqCst), sink_device(engine), engine.bitstream(), engine.audio());
        *into.lock().unwrap() = Some((token, restore, before, refused, at_turn, caps, after_caps, answer, playing, later));
    });
    let (token, restore, before, refused, at_turn, caps, after_caps, answer, playing, later) = got.lock().unwrap().take().unwrap();
    assert_ne!(restore, token, "the plan before put back under a token of its own");
    assert_eq!((restore.generation, restore.seq), (token.generation, token.seq));
    assert!(refused, "the plan's part refused at its turn");
    assert_eq!(at_turn, (0, before.clone(), false), "not one buffer, the device as it was, the selector not given its input");
    assert!(!caps, "its caps set no mode");
    assert_eq!(after_caps, (before, false), "nor swapped the device");
    assert!(answer.as_ref().is_err_and(|why| !why.starts_with(mediabox_core::audio::LIVE_PLAN_FAILED_STATE)), "the plan before back: {answer:?}");
    assert!(playing, "the plan before's part plays");
    assert_eq!(later, (0, Some(HDMI_PCM.into()), true, start), "and nothing of the plan's part ever passed");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!run.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
}

/// (8) The plan's part's turn first: the plan in force, answered so; its
/// deadline coming after that -- as its own thread brings it -- takes
/// nothing back.
#[test]
fn a_plan_in_force_is_not_undone_by_its_deadline_after() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start, None, Duration::from_millis(1500), move |engine| {
        let Waiting { held: (src, held), wait, token, counted } = plan_waiting(engine);
        src.remove_probe(held);
        let answer = answered(&wait);
        let at_answer = (sink_device(engine), engine.bitstream(), engine.audio());
        engine.plan_deadline(token.id);
        let out = count_sound_out(engine);
        let playing = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 20);
        let after = (sink_device(engine), engine.bitstream(), engine.audio());
        let passed = counted.load(std::sync::atomic::Ordering::SeqCst);
        let left = engine.plans.lock().unwrap().active.is_none() && engine.plans.lock().unwrap().owner == Some(token);
        *into.lock().unwrap() = Some((answer, at_answer, after, passed, playing, left));
    });
    let (answer, at_answer, after, passed, playing, left) = got.lock().unwrap().take().unwrap();
    let headset = Audio { device: Some(HEADSET_PCM.into()), passthrough: vec![], gain: 0.216, muted: false, ac3_encode: None };
    assert_eq!(answer, Ok(()));
    assert_eq!((at_answer.0.as_deref(), at_answer.1), (Some(HEADSET_PCM), false), "PCM to the headset");
    assert_eq!(after, at_answer, "the deadline after it took nothing back");
    assert_eq!(after.2.device, headset.device);
    assert!(passed > 0 && playing, "the plan's part plays");
    assert!(left, "the plan done, its token the sound's");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// (5) The deadline and the part before's end released together by one
/// barrier: one of the two outcomes, whole -- the plan in force (its part
/// passing, the headset open in PCM, answered done), or the plan before
/// (not one buffer of the plan's part, the television open for the
/// bitstream, answered not done and not as the failed state). Nothing in
/// between, and nothing of the loser after.
#[test]
fn a_deadline_and_a_turn_at_once_end_in_one_outcome() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let Waiting { held: (src, held), wait, token, counted } = plan_waiting(engine);
        let both = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            s.spawn(|| {
                both.wait();
                engine.plan_deadline(token.id);
            });
            s.spawn(|| {
                both.wait();
                src.remove_probe(held);
            });
        });
        let answer = answered(&wait);
        let out = count_sound_out(engine);
        let playing = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 20);
        let after = (sink_device(engine), engine.bitstream(), engine.audio().device);
        let passed = counted.load(std::sync::atomic::Ordering::SeqCst);
        let owner = engine.plans.lock().unwrap().owner;
        *into.lock().unwrap() = Some((token, answer, playing, after, passed, owner));
    });
    let (token, answer, playing, after, passed, owner) = got.lock().unwrap().take().unwrap();
    assert!(playing, "the sound goes on");
    match &answer {
        Ok(()) => {
            assert_eq!(after, (Some(HEADSET_PCM.into()), false, Some(HEADSET_PCM.into())), "the plan in force, whole");
            assert!(passed > 0, "its part plays");
            assert_eq!(owner, Some(token));
        }
        Err(why) => {
            assert!(!why.starts_with(mediabox_core::audio::LIVE_PLAN_FAILED_STATE), "{why}");
            assert_eq!(after, (Some(HDMI_PCM.into()), true, start.device.clone()), "the plan before, whole");
            assert_eq!(passed, 0, "not one buffer of the plan's part");
            assert_ne!(owner, Some(token), "its token retired");
        }
    }
    eprintln!("deadline-turn outcome: {}", if answer.is_ok() { "turn" } else { "deadline" });
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!run.shared.stand_in.load(std::sync::atomic::Ordering::SeqCst));
}

/// (9) A channel opened retires every token of the channel left: the plan
/// part's turn is refused, its caps set no mode, its deadline does
/// nothing, and the next channel plays on the plan before.
#[test]
fn a_channel_opened_retires_every_token_of_the_channel_left() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (ts, start) = passed_through();
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    let run = play_on(ts, 0, start.clone(), None, Duration::from_millis(1500), move |engine| {
        let Waiting { held: (src, held), wait, token, .. } = plan_waiting(engine);
        let path = engine.current_path().unwrap();
        engine.open(&path).expect("the next channel");
        let answer = answered(&wait);
        // The device: its PCM, its form, the swaps made. (The selector's
        // input is the next channel's to change meanwhile.)
        let device = |engine: &Engine| (sink_device(engine), engine.bitstream(), engine.swaps.lock().unwrap().opened);
        let before = device(engine);
        let caps = engine.part_sound_mode(false, Some(&std::sync::atomic::AtomicBool::new(false)), Some(token));
        let turn = engine.let_sound_go(token.generation, super::SoundRelease { block: None, activate: None, token: Some(token) }).is_err();
        engine.plan_deadline(token.id);
        let after = (device(engine), selector_given(engine, token).1);
        src.remove_probe(held);
        let out = count_sound_out(engine);
        let playing = until(Duration::from_secs(3), || out.load(std::sync::atomic::Ordering::SeqCst) >= 20);
        let left = engine.plans.lock().unwrap().active.is_none() && engine.plans.lock().unwrap().owner.is_none();
        *into.lock().unwrap() = Some((answer, before, caps, turn, after, playing, left, sink_device(engine), engine.bitstream()));
    });
    let (answer, before, caps, turn, after, playing, left, device, bitstream) = got.lock().unwrap().take().unwrap();
    assert!(answer.is_err(), "called off: {answer:?}");
    assert!(!caps, "its caps set no mode");
    assert!(turn, "its turn refused");
    assert_eq!(after, (before, false), "nothing of it moved the device or took the selector");
    assert!(playing && left);
    assert_eq!((device.as_deref(), bitstream), (Some(HDMI_PCM), true), "the next channel on the plan before");
    assert_eq!(run.shared.generation.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// FAZ 3E.2, the start: the player opens the control plane's plan in force
/// -- handed over whole in its environment -- and nothing else; the
/// platform's candidate (a headset still found after its change failed) is
/// never asked for. No plan, or one not whole: no start, and still the
/// platform is not asked. Only a developer's run says otherwise.
#[test]
fn the_player_starts_on_the_control_plane_s_plan_and_never_the_platform_s() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let plan: mediabox_core::AudioPlan = serde_json::from_value(hdmi_plan(80, false, false)).unwrap();
    let candidate: mediabox_core::AudioPlan = serde_json::from_value(headset_plan(HEADSET_PCM)).unwrap();
    let value = mediabox_core::audio::plan_to_env(&plan);
    let env = |name: &str| (name == mediabox_core::audio::LIVE_PLAN_ENV).then(|| value.clone());
    let never = || -> Option<mediabox_core::AudioPlan> { panic!("the platform's candidate was asked for") };
    let audio = crate::sound_plan_from(env, never, false).expect("the plan");
    assert_eq!(audio, Audio::from_plan(&plan, false));
    assert!(crate::sound_plan_from(|_| None, never, false).is_err(), "no plan, no start");
    let broken = |name: &str| (name == mediabox_core::audio::LIVE_PLAN_ENV).then(|| value[..value.len() - 2].to_string());
    assert!(crate::sound_plan_from(broken, never, false).is_err(), "a plan not whole, no start");
    let developer = |name: &str| (name == "MEDIABOX_LIVE_AUDIO_FROM_PLATFORM").then(|| "1".to_string());
    let asked = crate::sound_plan_from(developer, || Some(candidate.clone()), false).expect("asked for");
    assert_eq!(asked.device.as_deref(), Some(HEADSET_PCM), "only when a developer's run says so");
    // And what it opens is that plan's device.
    let streams: Streams = vec![(0x1B, VIDEO, vec![]), (0x04, 0x101, vec![])];
    let ts = stream_for(3, &[(0, 0, streams)], &[Sound { pid: 0x101, stream_id: 0xC0, units: repeated(mp2(), 3), from: 0, to: u64::MAX }]);
    let got = Arc::new(Mutex::new(None));
    let into = Arc::clone(&got);
    play_on(ts, 0, audio, None, Duration::from_millis(800), move |engine| {
        *into.lock().unwrap() = Some((sink_device(engine), engine.audio()));
    });
    let (opened, audio) = got.lock().unwrap().take().unwrap();
    assert_eq!(opened, plan.device.as_ref().map(|device| device.pcm.clone()));
    assert_eq!(audio, Audio::from_plan(&plan, false));
}
