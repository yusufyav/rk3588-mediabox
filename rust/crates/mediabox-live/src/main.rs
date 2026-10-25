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
//!   MEDIABOX_LIVE_DEMUX_LATENCY_MS tsdemux latency (default 700)
//!   MEDIABOX_LIVE_RENDER_DELAY_MS  sink wake-up to scan-out (default 0)
//!   MEDIABOX_LIVE_MAX_LATENESS_MS  later than this, a frame is not sent (20)
//!   MEDIABOX_LIVE_AUDIO_DEVICE     an ALSA PCM instead of the sound plan's
//!   MEDIABOX_LIVE_PROGRAM          one service of a whole multiplex (a recording)
//!   MEDIABOX_VO_TRACE              the frame trace, vo_mediabox's format

mod engine;
mod ipc;
mod plane;
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

fn main() {
    let signals = block_signals();
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
        demux_latency_ms: env_u32("MEDIABOX_LIVE_DEMUX_LATENCY_MS", 700),
        render_delay_ms: env_u32("MEDIABOX_LIVE_RENDER_DELAY_MS", 0),
        max_lateness_ms: env_u32("MEDIABOX_LIVE_MAX_LATENESS_MS", 20),
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

    match engine.open(&url) {
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
    // The same channel opened again after an error, without a frame since.
    let mut reopened = 0;
    // Why the player ends, when it ends because it cannot play.
    let mut failure: Option<String> = None;
    let mut report = Instant::now();
    let dropped_late = Mutex::new(0u64);
    loop {
        match inbox.recv_timeout(Duration::from_millis(20)) {
            Ok((command, done)) => {
                let quit = matches!(command, Command::Quit | Command::Fail(_));
                let mut result = Ok(());
                match command {
                    // The next channel in the same player: not the end of
                    // anything, so no end-file. A channel that cannot be
                    // opened is answered with why.
                    Command::Load(path) => {
                        reopened = 0;
                        result = engine.open(&path).map_err(|error| format!("Kanal açılamadı: {error}"));
                        if result.is_ok() {
                            clients.event(json!({"event": "start-file"}));
                        }
                    }
                    Command::Pause(paused) => {
                        engine.set_paused(paused);
                        clients.changed("pause", json!(paused));
                    }
                    Command::Aid(aid) => engine.set_aid(aid),
                    Command::Volume { gain, muted } => engine.set_volume(gain, muted),
                    Command::AudioDelay(seconds) => engine.set_audio_delay(seconds),
                    Command::ReloadSound => engine.reopen_audio_sink(),
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
                            engine.reopen_audio_sink();
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
                    eprintln!("mediabox-live: uyarı ({name}): {}", warning.error());
                }
                MessageView::Latency(_) => engine.recalculate_latency(),
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
        }
    }

    eprintln!("mediabox-live: {}", stats(&engine, &shared, &plane));
    // Nothing reaches the plane after this, then the picture is taken off.
    plane.close();
    plane.stop();
    trace.flush();
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
            _ => return None,
        })
    })
}
