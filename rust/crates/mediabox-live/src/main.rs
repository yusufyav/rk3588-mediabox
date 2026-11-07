//! mediabox-live: live television's player, and its only one.
//!
//! Started by packaging/mediabox-player when the control plane opens a DVB
//! service (MEDIABOX_PLAYER_LIVE=1), on the receiver's FIFO, with the
//! launcher's environment and a control socket. See engine.rs for the
//! pipeline and its clock, plane.rs for the wire to the interface, ipc.rs for
//! the control language.
//!
//! Environment, beyond the launcher's own:
//!   MEDIABOX_LIVE_VIDEO_DECODER    the decoder element (default mppvideodec)
//!   MEDIABOX_LIVE_DEMUX_LATENCY_MS tsdemux latency (default 700: see `DEMUX_LATENCY_MS`)
//!   MEDIABOX_LIVE_LATENCY_MS       the pipeline's latency (default 150: see `engine::LATENCY_MS`)
//!   MEDIABOX_LIVE_RENDER_DELAY_MS  how far ahead of its time a frame is sent (default 40: two refreshes at 50 Hz)
//!   MEDIABOX_LIVE_MAX_LATENESS_MS  later than this, the sink drops a frame (default: never; the panel decides)
//!   MEDIABOX_LIVE_AUDIO_DEVICE     an ALSA PCM instead of the sound plan's
//!   MEDIABOX_LIVE_PROGRAM          one service of a whole multiplex (a recording)
//!   MEDIABOX_VO_TRACE              the frame trace, vo_mediabox's format

mod clock_probe;
mod engine;
mod es;
mod gate;
mod health;
mod ipc;
mod plane;
mod sound_diag;
mod stamp;
mod trace;

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;
use serde_json::{Value, json};

use engine::{Audio, Engine, Options, Shared};
use ipc::{Clients, Command};

const PLATFORM: &str = "/opt/rk3588-mediabox/bin/mediabox-platform";

/// tsdemux's latency: GStreamer's default, 700 ms "for smooth demuxing".
///
/// Lowered to 50 ms on 2026-11-01 from pictures alone (no skipped or repeated
/// frame at 700, 300, 150 or 50 ms; 754, 360, 203 and 110 ms behind the
/// broadcast), it starved the sound: tsdemux hands a sound PES on only once
/// the next one starts, and some services carry long ones. The live monitor
/// (`health`, key M), 30-40 s per service on the Plus with the Kanky,
/// 2026-11-02 -- sound reaching the sink after its time, and audiobasesink
/// resyncing because what came was behind its read position:
///   50 ms  TRT 1 88 late + 12 resyncs
///   150 ms SD 16300 88 late + 14 resyncs, 10901 12 resyncs, TRT 1 4
///   300 ms SD 16300 2 resyncs; 10901, TRT 1 clean
///   700 ms SD 16300, 10901, TRT 1 clean, pictures too
/// The sound now comes packet by packet (`es`), not from tsdemux, and the
/// pipeline's latency is set (`engine::LATENCY_MS`), not asked of tsdemux:
/// this is only what it says when asked.
///
/// And what it said was not so. tsdemux uses the property nowhere but in
/// its answer to the latency query (gst-plugins-bad 1.26.11, tsdemux.c
/// 418-549 and 626-632): it adds it to its upstream's minimum, H.222.0
/// Annex D's 700 ms between valid PTS. The pipeline's minimum came out at
/// 760-800 ms (tsdemux 700, h264parse 40, the picture's sink 60; queried
/// element by element on the Plus, 2026-11-03) against the 150 ms it is run
/// at, and gst_pipeline_do_latency warned at every latency message,
/// "Configured latency is lower than detected minimum latency" (core/Clock,
/// gstpipeline.c 716): 389 times in an evening's player.log. The latency
/// the sinks wait by did not change with it -- 150 ms, measured at 150 ms
/// with no picture or sound late in a 30-minute run (99,446 pictures).
///
/// So it says what it holds: tsdemux gives a PES on when the next one on
/// its PID starts, one access unit -- 40 ms at 25 pictures a second, the
/// longest a DVB broadcast's picture has. Asked, the pipeline's minimum is
/// then 140 ms, within the 150 it runs at.
const DEMUX_LATENCY_MS: u32 = 40;

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// Where the sound goes, from the kept sound setting (`mediabox-platform
/// audio-plan`), the plan films are played with too.
fn sound_plan() -> Audio {
    let display_sync = std::env::var("MEDIABOX_PLAYER_DISPLAY_SYNC").as_deref() == Ok("1");
    if let Ok(device) = std::env::var("MEDIABOX_LIVE_AUDIO_DEVICE") {
        return Audio {
            device: (!device.is_empty() && device != "none").then_some(device),
            passthrough: Vec::new(),
            gain: 1.0,
            muted: false,
            ac3_encode: None,
        };
    }
    let plan = std::process::Command::new(PLATFORM)
        .arg("audio-plan")
        .stderr(std::process::Stdio::inherit())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<mediabox_core::AudioPlan>(&output.stdout).ok());
    let Some(plan) = plan else {
        eprintln!("mediabox-live: ses planı okunamadı; ses gönderilmiyor");
        return Audio { device: None, passthrough: Vec::new(), gain: 1.0, muted: false, ac3_encode: None };
    };
    let volume = f64::from(plan.player_volume()) / 100.0;
    Audio {
        device: plan.device.as_ref().map(|device| device.pcm.clone()),
        // "Oynatımı görüntüye eşitle" sends PCM, as films do (Kodi: no
        // passthrough while usedisplayasclock is on).
        passthrough: if display_sync { Vec::new() } else { plan.passthrough.clone() },
        gain: volume.powi(3),
        muted: plan.muted,
        // The setting's Dolby Digital encoding; off with "Oynatımı görüntüye
        // eşitle", as for films.
        ac3_encode: plan
            .ac3_encode
            .filter(|_| !display_sync)
            .map(|encode| (encode.kbps.unwrap_or(448), u32::from(encode.min_channels))),
    }
}

/// SIGTERM and SIGINT end the film the way `quit` does: the picture is taken
/// off and every frame given back before the process goes.
fn block_signals() -> libc::sigset_t {
    // SAFETY: a sigset on the stack, filled and applied to this thread before
    // any other exists, so every thread inherits the mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

/// Every line this process (GStreamer, MPP and itself) writes to stderr,
/// stamped with CLOCK_MONOTONIC -- the clock of the timeline, of the
/// interface's journal and of the page flips -- and the wall clock.
///
/// The player's log (`/run/mediabox/player.log`) had no time at all: over a
/// two-hour match (2026-11-03) when a sound fault was said could only be
/// guessed from the frame counts. Without a pipe (stderr unchanged) nothing
/// is stamped and nothing is lost.
fn stamp_stderr() {
    use std::io::{BufRead, Write};
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills two descriptors; dup/dup2 on descriptors this
    // process owns.
    unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return;
        }
        let original = libc::dup(2);
        if original < 0 || libc::dup2(fds[1], 2) < 0 {
            libc::close(fds[0]);
            libc::close(fds[1]);
            return;
        }
        libc::close(fds[1]);
        let reader = std::io::BufReader::new(std::fs::File::from_raw_fd(fds[0]));
        let mut out = std::fs::File::from_raw_fd(original);
        let _ = std::thread::Builder::new().name("stderr-stamp".into()).spawn(move || {
            for line in reader.split(b'\n') {
                let Ok(line) = line else { break };
                let mut mono = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                let mut wall = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                let mut tm: libc::tm = std::mem::zeroed();
                libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut mono);
                libc::clock_gettime(libc::CLOCK_REALTIME, &mut wall);
                libc::localtime_r(&wall.tv_sec, &mut tm);
                let _ = write!(
                    out,
                    "[{}.{:06} {:02}:{:02}:{:02}.{:03}] ",
                    mono.tv_sec,
                    mono.tv_nsec / 1000,
                    tm.tm_hour,
                    tm.tm_min,
                    tm.tm_sec,
                    wall.tv_nsec / 1_000_000
                );
                let _ = out.write_all(&line);
                let _ = out.write_all(b"\n");
            }
        });
    }
}

fn main() {
    let signals = block_signals();
    stamp_stderr();
    mediabox_core::timeline::init("live");
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("kullanım: mediabox-live <FIFO>");
        std::process::exit(2);
    };
    let url = url.trim_start_matches("file://").to_string();
    if let Err(error) = gst::init() {
        eprintln!("mediabox-live: GStreamer başlatılamadı: {error}");
        std::process::exit(1);
    }

    let (commands, inbox) = mpsc::channel::<(Command, Option<mpsc::SyncSender<Result<(), String>>>)>();
    let shared = Arc::new(Shared::default());
    {
        let commands = commands.clone();
        std::thread::spawn(move || {
            let mut signal = 0;
            // SAFETY: waits on the set blocked above.
            unsafe { libc::sigwait(&signals, &mut signal) };
            eprintln!("mediabox-live: sinyal {signal}; kapanılıyor");
            let _ = commands.send((Command::Quit, None));
        });
    }

    let trace = trace::Trace::from_env();
    let socket = std::env::var("MEDIABOX_VIDEO_SOCKET").ok().filter(|path| !path.is_empty());
    let socket = socket.as_deref().unwrap_or(plane::SOCKET);
    let gone = {
        let commands = commands.clone();
        move || {
            let _ = commands.send((Command::Quit, None));
        }
    };
    let plane = match plane::Plane::connect(socket, trace.clone(), gone) {
        Ok(plane) => plane,
        Err(error) => {
            eprintln!("mediabox-live: {error}");
            std::process::exit(1);
        }
    };
    eprintln!("mediabox-live: MediaBox video düzlemine çiziliyor ({socket})");

    let audio = sound_plan();
    eprintln!(
        "mediabox-live: ses: {} geçiş {:?} kazanç {:.3}{}",
        audio.device.as_deref().unwrap_or("yok"),
        audio.passthrough,
        audio.gain,
        if audio.muted { " (sessiz)" } else { "" }
    );
    let options = Options {
        video_decoder: std::env::var("MEDIABOX_LIVE_VIDEO_DECODER").unwrap_or_else(|_| "mppvideodec".into()),
        audio,
        demux_latency_ms: env_u32("MEDIABOX_LIVE_DEMUX_LATENCY_MS", DEMUX_LATENCY_MS),
        render_delay_ms: env_u32("MEDIABOX_LIVE_RENDER_DELAY_MS", 40),
        max_lateness_ms: std::env::var("MEDIABOX_LIVE_MAX_LATENESS_MS").ok().and_then(|value| value.parse().ok()),
    };
    let fail = {
        let commands = commands.clone();
        move |why: Option<String>| {
            let _ = commands.send((why.map_or(Command::Quit, Command::Fail), None));
        }
    };
    let engine = match Engine::new(options, Arc::clone(&plane), Arc::clone(&shared), trace.clone(), fail) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("mediabox-live: {error}");
            plane.stop();
            std::process::exit(1);
        }
    };

    let clients = Arc::new(Clients::default());
    if let Ok(path) = std::env::var("MEDIABOX_PLAYER_IPC")
        && !path.is_empty()
    {
        let query = query(Arc::clone(&engine), Arc::clone(&shared), Arc::clone(&plane));
        if let Err(error) = ipc::serve(&path, Arc::clone(&clients), query, commands.clone()) {
            eprintln!("mediabox-live: denetim soketi açılamadı: {error}");
        }
    }

    // The control plane's number for the channel it started this player
    // on: held at its opening gate until it lets it go (`gate`).
    let epoch = std::env::var("MEDIABOX_LIVE_EPOCH").ok().and_then(|value| value.parse::<u64>().ok());
    match engine.open_channel(&url, epoch) {
        Ok(()) => clients.event(json!({"event": "start-file"})),
        Err(error) => {
            let _ = commands.send((Command::Fail(format!("Kanal açılamadı: {error}")), None));
        }
    }

    let bus = engine.bus();
    let mut last_eof = false;
    let mut last_playing = false;
    let mut audio_failed: Option<Instant> = None;
    let mut branch_failed: Option<Instant> = None;
    let mut latency_told: Option<String> = None;
    // The same channel opened again after an error, without a frame since.
    let mut reopened = 0;
    // Why the player ends, when it ends because it cannot play.
    let mut failure: Option<String> = None;
    let mut report = Instant::now();
    let dropped_late = Mutex::new(0u64);
    loop {
        match inbox.recv_timeout(Duration::from_millis(20)) {
            Ok((command, mut done)) => {
                let mut quit = matches!(command, Command::Quit | Command::Fail(_));
                let mut result = Ok(());
                match command {
                    // The next channel in the same player: not the end of
                    // anything, so no end-file. A channel that cannot be
                    // opened is answered with why.
                    Command::Load(path, epoch) => {
                        reopened = 0;
                        result = engine.open_channel(&path, epoch).map_err(|error| format!("Kanal açılamadı: {error}"));
                        if result.is_ok() {
                            clients.event(json!({"event": "start-file"}));
                        }
                    }
                    Command::OpenGate { epoch, outcome } => result = engine.open_gate(epoch, &outcome),
                    // Answered, then the end: nothing of it was shown or heard.
                    Command::FailGate { epoch, code } => {
                        result = engine.fail_gate(epoch, &code);
                        if result.is_ok() {
                            failure = Some(format!("Kanal açılamadı ({code})"));
                            quit = true;
                        }
                    }
                    Command::Pause(paused) => {
                        engine.set_paused(paused);
                        clients.changed("pause", json!(paused));
                    }
                    Command::Aid(aid) => engine.set_aid(aid),
                    Command::Volume { gain, muted } => engine.set_volume(gain, muted),
                    Command::AudioDelay(seconds) => engine.set_audio_delay(seconds),
                    // Answered when the device is open again, or given up
                    // on: not when asked.
                    Command::ReloadSound => engine.reopen_audio_sink(done.take()),
                    Command::Fail(why) => {
                        eprintln!("mediabox-live: {why}");
                        failure = Some(why);
                    }
                    Command::Quit => {}
                }
                if let Some(done) = done {
                    let _ = done.send(result);
                }
                if quit {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if engine.expire_gate() {
            failure = Some("Kanal açılamadı (player-timeout)".to_string());
            break;
        }

        while let Some(message) = bus.pop() {
            use gst::MessageView;
            match message.view() {
                MessageView::Error(error) => {
                    let source = error.src();
                    let name = source.map(|source| source.path_string().to_string()).unwrap_or_default();
                    eprintln!("mediabox-live: hata ({name}): {} {:?}", error.error(), error.debug());
                    if source.is_some_and(|source| engine.is_audio_sink(source)) {
                        // A display mode change takes the HDMI sound device
                        // away; it is opened again, not given up.
                        if audio_failed.is_none_or(|at| at.elapsed() > Duration::from_secs(1)) {
                            audio_failed = Some(Instant::now());
                            engine.reopen_audio_sink(None);
                        }
                    } else if source.is_some_and(|source| engine.is_current(source))
                        && branch_failed.is_none_or(|at| at.elapsed() > Duration::from_secs(2))
                        && let Some(path) = engine.current_path()
                    {
                        // Opened again, a few times: a broadcast's glitch
                        // passes. An error that comes back every time is the
                        // channel's, and is said.
                        branch_failed = Some(Instant::now());
                        if shared.playing.load(Ordering::SeqCst) {
                            reopened = 0;
                        }
                        reopened += 1;
                        let opened = if reopened > 3 {
                            Err(error.error().to_string())
                        } else {
                            eprintln!("mediabox-live: kanal yeniden açılıyor");
                            engine.open(&path)
                        };
                        if let Err(why) = opened {
                            let _ = commands.send((Command::Fail(format!("Kanal oynatılamıyor: {why}")), None));
                        }
                    }
                }
                MessageView::Warning(warning) => {
                    let name = warning.src().map(|source| source.path_string().to_string()).unwrap_or_default();
                    let error = warning.error();
                    let debug = warning.debug().map(|debug| debug.to_string()).unwrap_or_default();
                    let (domain, code) = error_kind(&error);
                    let generation = shared.generation.load(Ordering::SeqCst);
                    eprintln!("mediabox-live: uyarı ({name}) [{domain}/{code}] gen={generation}: {error} -- {debug}");
                    mediabox_core::timeline!(
                        "gst-warning",
                        "gen={generation} src={name} domain={domain} code={code} debug={}",
                        debug.replace(char::is_whitespace, "_")
                    );
                }
                MessageView::Latency(_) => {
                    engine.recalculate_latency();
                    let generation = shared.generation.load(Ordering::SeqCst);
                    let query = engine.latency_query();
                    mediabox_core::timeline!(
                        "latency",
                        "gen={generation} live={:?} min_ns={:?} max_ns={:?} configured_ns={:?}",
                        query.map(|(live, _, _)| live),
                        query.map(|(_, min, _)| min),
                        query.and_then(|(_, _, max)| max),
                        engine.configured_latency()
                    );
                    // Every element's share, for the record: said again
                    // whenever it is not what was said last.
                    {
                        let parts: Vec<String> = engine
                            .latency_by_element()
                            .into_iter()
                            .map(|(name, live, min, max)| {
                                format!("{name}={}{}/{}", if live { "" } else { "~" }, min / 1_000_000, max.map_or("∞".into(), |max| (max / 1_000_000).to_string()))
                            })
                            .collect();
                        let said = format!("{query:?} {}", parts.join(" "));
                        if latency_told.as_deref() == Some(said.as_str()) {
                            continue;
                        }
                        latency_told = Some(said);
                        eprintln!(
                            "mediabox-live: gecikme sorgusu gen={generation}: boru hattı {:?} ms (en az/en çok), ayarlı {:?} ms; öğeler (en az/en çok ms): {}",
                            query.map(|(_, min, max)| (min / 1_000_000, max.map(|max| max / 1_000_000))),
                            engine.configured_latency().map(|ns| ns / 1_000_000),
                            parts.join(" ")
                        );
                    }
                }
                MessageView::Qos(qos) => {
                    let (_processed, dropped) = qos.stats();
                    if qos.src().is_some_and(|source| source.name() == "video") {
                        let dropped = u64::try_from(dropped.value()).unwrap_or(0);
                        *dropped_late.lock().unwrap_or_else(|p| p.into_inner()) = dropped;
                        shared.late.store(dropped, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        }

        let eof = shared.eof.load(Ordering::SeqCst);
        if eof != last_eof {
            last_eof = eof;
            clients.changed("eof-reached", json!(eof));
        }
        let playing = shared.playing.load(Ordering::SeqCst);
        if playing != last_playing {
            last_playing = playing;
            if playing {
                clients.event(json!({"event": "file-loaded"}));
                clients.event(json!({"event": "playback-restart"}));
            }
            clients.changed("core-idle", json!(!playing));
        }
        if report.elapsed() >= Duration::from_secs(10) {
            report = Instant::now();
            eprintln!("mediabox-live: {}", stats(&engine, &shared, &plane));
            trace.flush();
            mediabox_core::timeline::flush();
        }
    }

    eprintln!("mediabox-live: {}", stats(&engine, &shared, &plane));
    // The last word on the channel, for whoever reads the health file after
    // the player: its gate's result above all (`outcome`, `failed`).
    engine.write_health();
    // Nothing reaches the plane after this, then the picture is taken off.
    plane.close();
    plane.stop();
    trace.flush();
    mediabox_core::timeline::flush();
    clients.event(match &failure {
        Some(why) => json!({"event": "end-file", "reason": "error", "file_error": why}),
        None => json!({"event": "end-file", "reason": "quit"}),
    });
    if let Ok(path) = std::env::var("MEDIABOX_PLAYER_IPC") {
        let _ = std::fs::remove_file(path);
    }
    // The pipeline is stopped on a thread of its own and given 1.5 s: a
    // decoder or a reader blocked on the FIFO kept this process alive for
    // 90 s until systemd killed it (measured 2026-10-24). The picture is
    // already off and the frames given back, so nothing is lost by leaving.
    let (done, wait) = mpsc::channel();
    let engine_for_shutdown = Arc::clone(&engine);
    std::thread::spawn(move || {
        engine_for_shutdown.shutdown();
        let _ = done.send(());
    });
    if wait.recv_timeout(Duration::from_millis(1500)).is_err() {
        eprintln!("mediabox-live: pipeline 1,5 s içinde durmadı; çıkılıyor");
    }
    std::process::exit(0);
}

fn stats(engine: &Engine, shared: &Shared, plane: &plane::Plane) -> Value {
    let info = shared.info.lock().unwrap_or_else(|p| p.into_inner()).clone();
    json!({
        "frames": shared.frames.load(Ordering::Relaxed),
        "decoded": shared.decoded.load(Ordering::Relaxed),
        "out_of_order": shared.out_of_order.load(Ordering::Relaxed),
        "late_dropped": shared.late.load(Ordering::Relaxed),
        "full_dropped": plane.dropped_full(),
        "held": plane.held(),
        "first_frame_ms": info.first_frame_ms,
        "latency_s": engine.latency(),
        "audio_lead_s": shared.audio_lead_ns.load(Ordering::Relaxed) as f64 / 1e9,
        "video_lag_s": shared.video_lag_ns.load(Ordering::Relaxed) as f64 / 1e9,
        "video": info.video_codec,
        "size": [info.width, info.height],
        "fps": info.fps,
        "interlace": info.interlaced,
        "audio": info.audio_codec,
        "passthrough": info.passthrough,
        "audio_caps": engine.audio_caps(),
    })
}

/// A channel count as mpv names the layout.
fn layout(channels: i32) -> String {
    match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n}ch"),
    }
}

/// The answers to `get_property`, read from the engine's shared state.
///
/// `core-idle` is false once the channel opened last has put its first
/// frame on the interface's plane -- handed over and committed, not merely
/// decoded -- or, for a service without a picture, once its sound reaches
/// the sound device; and while it is not paused. The control plane reads
/// that as the channel's picture (`picture` in its status).
fn query(engine: Arc<Engine>, shared: Arc<Shared>, plane: Arc<plane::Plane>) -> ipc::Query {
    Arc::new(move |name: &str| -> Option<Value> {
        let info = || shared.info.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let playing = shared.playing.load(Ordering::SeqCst);
        let paused = shared.paused.load(Ordering::SeqCst);
        Some(match name {
            "pid" => json!(std::process::id()),
            "time-pos" | "playback-time" => json!(engine.running_time()?),
            "pause" => json!(paused),
            "core-idle" => json!(!playing || paused),
            "idle-active" => json!(false),
            "eof-reached" => json!(shared.eof.load(Ordering::SeqCst)),
            "seeking" => json!(false),
            "speed" => json!(1.0),
            "audio-delay" => json!(engine.audio_delay()),
            "sub-delay" => json!(0.0),
            "path" | "stream-open-filename" => json!(info().path),
            "container-fps" | "estimated-vf-fps" => json!(info().fps?),
            "audio-codec-name" => json!(info().audio_codec?),
            "video-codec" => json!(info().video_codec?),
            // In mpv's form, which the sound screen reads
            // (mediabox_core::StreamAudio::from_mpv): what the stream is, and
            // what leaves for the device -- `spdif-ac3` for the bursts.
            "audio-params" => {
                let (rate, channels) = info().audio_decoded?;
                json!({"samplerate": rate, "channels": layout(channels), "channel-count": channels})
            }
            "audio-out-params" => {
                let (format, rate, channels) = engine.audio_caps()?;
                if engine.bitstream() {
                    json!({"format": "spdif-ac3", "samplerate": rate, "channels": "stereo", "channel-count": 2})
                } else {
                    json!({"format": format, "samplerate": rate, "channels": layout(channels), "channel-count": channels})
                }
            }
            "volume" => json!((engine.audio().gain.cbrt() * 100.0).round()),
            "mute" => json!(engine.audio().muted),
            "aid" => match engine.aid() {
                Some(order) => json!(order + 1),
                None => json!(false),
            },
            "track-list" => {
                let info = info();
                let mut tracks = Vec::new();
                if let Some(codec) = &info.video_codec {
                    tracks.push(json!({"id": 1, "type": "video", "codec": codec, "selected": true,
                        "demux-w": info.width, "demux-h": info.height}));
                }
                for (order, (codec, language)) in info.audio_tracks.iter().enumerate() {
                    let mut track = json!({"id": order + 1, "type": "audio", "codec": codec,
                        "selected": info.aid == Some(order)});
                    if let Some(language) = language {
                        track["lang"] = json!(language);
                    }
                    tracks.push(track);
                }
                json!(tracks)
            }
            "mediabox-live" => stats(&engine, &shared, &plane),
            "mediabox-gate" => engine.gate_snapshot(),
            _ => return None,
        })
    })
}

/// A GStreamer error's domain and code, by name: what `GST_ELEMENT_WARNING`
/// was given (`CORE, CLOCK` and so on), for the log and the timeline.
fn error_kind(error: &gst::glib::Error) -> (&'static str, String) {
    if let Some(kind) = error.kind::<gst::CoreError>() {
        ("core", format!("{kind:?}"))
    } else if let Some(kind) = error.kind::<gst::StreamError>() {
        ("stream", format!("{kind:?}"))
    } else if let Some(kind) = error.kind::<gst::ResourceError>() {
        ("resource", format!("{kind:?}"))
    } else if let Some(kind) = error.kind::<gst::LibraryError>() {
        ("library", format!("{kind:?}"))
    } else {
        ("other", "?".into())
    }
}

#[cfg(test)]
mod warning_tests {
    use super::error_kind;
    use gst::prelude::*;

    /// A pipeline's latency warning (gstpipeline.c, `GST_ELEMENT_WARNING
    /// (pipeline, CORE, CLOCK, ...)`) is told as core/Clock with its debug
    /// text: what the log used to show only as "clock problem".
    #[test]
    fn a_warning_is_told_by_domain_code_and_debug() {
        gst::init().expect("GStreamer");
        let pipeline = gst::Pipeline::new();
        let bus = pipeline.bus().unwrap();
        // The pipeline's own: what gst_pipeline_do_latency posts.
        let message = gst::message::Warning::builder(gst::CoreError::Clock, "clock problem.")
            .src(&pipeline)
            .debug("Configured latency is lower than detected minimum latency: configured 0:00:00.150000000 < min 0:00:00.760000000")
            .build();
        bus.post(message).unwrap();
        let message = bus.timed_pop(gst::ClockTime::from_seconds(1)).expect("the warning");
        let gst::MessageView::Warning(warning) = message.view() else { panic!("a warning") };
        let (domain, code) = error_kind(&warning.error());
        assert_eq!((domain, code.as_str()), ("core", "Clock"));
        assert!(warning.debug().unwrap().contains("lower than detected minimum latency"));
    }
}
