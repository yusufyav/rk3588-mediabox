//! The interface's own player.
//!
//! A film opened from the catalogue is a window of the interface, not another
//! application taking the television away from it. That is the whole
//! difference between this and handing Kodi the display: the catalogue is
//! still behind it, Back returns to it, and nothing has to be handed back.
//!
//! The player is mpv, built on the appliance against MediaBox's own Rockchip
//! media runtime and started by `mediabox-player`. This module only starts it,
//! asks it where it has got to, and stops it.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::Mutex;

/// The transient unit the player runs as.
///
/// Not a child of this daemon, and the reason is the same one written on
/// kodi.service: a player needs the GPU, the DMA heaps and the input devices,
/// and this daemon is sandboxed away from all three. Started as a child it
/// inherited that sandbox and MPP could not open the render device at all, so
/// every film fell back to software or failed outright. systemd starts it
/// in its own context instead — the same context every time.
const UNIT: &str = "mediabox-player.service";

/// What is playing here, if anything.
#[derive(Debug, Clone)]
pub struct Playing {
    pub content: Playback,
    /// What to call it, and how long it is, as the caller knew them. Neither
    /// is derivable here; see `Request::MediaPlayHere`.
    pub title: Option<String>,
    pub duration: Option<u64>,
}

/// A film, which the media worker serves and Kodi can be handed, or live
/// television, which the DVB receiver's process serves and nothing else can.
#[derive(Debug, Clone)]
pub enum Playback {
    Film {
        /// The session the worker opened for it; stopped with the player.
        session_id: String,
        /// The address Kodi is given when the film is handed over: the
        /// session's own `handoff.kodiPlaybackUrl`, on the media core's
        /// loopback. It is something Kodi opens, never something the core can
        /// be asked to resolve -- it refuses its own loopback as a source,
        /// which is what broke the handover once. See `Daemon::handoff_to_kodi`.
        source: String,
        /// Where that film actually comes from: the upstream address the
        /// media core resolved (`handoff.resolvedInput`). When `source` turns
        /// out to be the core's own proxy, this is the only thing a second
        /// player can be given -- the proxy is one ffmpeg on one pipe and a
        /// late reader joins it mid-stream, not at the beginning.
        origin: String,
    },
    /// One service of `mediabox-dvb.service`, read from its FIFO.
    Live { service_id: u16 },
}

impl Playing {
    /// The worker session a film reads; live television has none.
    pub fn session_id(&self) -> Option<&str> {
        match &self.content {
            Playback::Film { session_id, .. } => Some(session_id),
            Playback::Live { .. } => None,
        }
    }

    pub fn live(&self) -> Option<u16> {
        match self.content {
            Playback::Live { service_id } => Some(service_id),
            Playback::Film { .. } => None,
        }
    }
}

pub struct PlayerManager {
    launcher: PathBuf,
    socket: PathBuf,
    playing: Mutex<Option<Playing>>,
    /// Which film this is: moved on every start and every stop, so whatever
    /// follows one film can tell it has ended or been replaced.
    film: AtomicU64,
    /// The player was started paused and is held there until the display
    /// has been matched to the film (see `Daemon::follow_film`).
    holding: AtomicBool,
    /// The film was started timed to the display: its sound is PCM for as
    /// long as it plays, whatever the sound plan says about passthrough.
    display_sync: AtomicBool,
    /// Live television's position is the broadcast's clock (its stream is
    /// not probed for a start time): the first one read, so what is shown
    /// is how long it has been on.
    live_origin: std::sync::Mutex<Option<f64>>,
    /// A live channel is being opened in the running player: the end of the
    /// file before it is not the end of anything (`playback::read_mpv`).
    reloading: AtomicBool,
}

impl PlayerManager {
    pub fn new(launcher: impl Into<PathBuf>, socket: impl Into<PathBuf>) -> Self {
        Self {
            launcher: launcher.into(),
            socket: socket.into(),
            playing: Mutex::new(None),
            film: AtomicU64::new(0),
            holding: AtomicBool::new(false),
            display_sync: AtomicBool::new(false),
            live_origin: std::sync::Mutex::new(None),
            reloading: AtomicBool::new(false),
        }
    }

    /// The player's IPC socket.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The number of the film playing now; it changes when the film does.
    pub fn film(&self) -> u64 {
        self.film.load(Ordering::SeqCst)
    }

    /// Let film `film` go, if it is still held. Asked of the player itself,
    /// and only once: a press of Pause since then is the viewer's, and is
    /// not undone here.
    pub async fn release(&self, film: u64) {
        if self.film() != film || !self.holding.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = self
            .ask(json!({"command": ["set_property", "pause", false], "request_id": 20}))
            .await;
    }

    /// One property of the running player, as mpv gives it.
    pub async fn property(&self, name: &str) -> Option<Value> {
        self.ask(json!({"command": ["get_property", name], "request_id": 21}))
            .await?
            .get("data")
            .cloned()
            .filter(|value| !value.is_null())
    }

    /// Put a changed sound plan into the running film: the volume always,
    /// and the device, the passthrough formats and the encoder when they
    /// moved -- mpv reopens its output for those, a moment of silence rather
    /// than a restart. Nothing when nothing is playing: the next film is
    /// started from the plan anyway.
    pub async fn apply_audio(&self, before: &mediabox_core::AudioPlan, after: &mediabox_core::AudioPlan) {
        if self.playing().await.is_none() {
            return;
        }
        let set = |name: &str, value: Value| json!({"command": ["set_property", name, value], "request_id": 30});
        if before.mpv_device() != after.mpv_device() && after.device.is_some() {
            let _ = self.ask(set("audio-device", json!(after.mpv_device()))).await;
        }
        // Timed to the display, the film never had passthrough (Kodi: no
        // passthrough while usedisplayasclock is on) and does not get it now.
        let pcm_only = self.display_sync.load(Ordering::SeqCst);
        if before.mpv_spdif() != after.mpv_spdif() && !pcm_only {
            let _ = self.ask(set("audio-spdif", json!(after.mpv_spdif()))).await;
        }
        if before.mpv_af() != after.mpv_af() && !pcm_only {
            let _ = self.ask(set("af", json!(after.mpv_af()))).await;
        }
        let _ = self.ask(set("volume", json!(after.player_volume()))).await;
        let _ = self.ask(set("mute", json!(after.muted))).await;
    }

    /// What the film's sound is and what leaves the box, as mpv says.
    pub async fn audio_stream(&self) -> Option<mediabox_core::StreamAudio> {
        self.playing().await?;
        let codec = self
            .property("audio-codec-name")
            .await
            .and_then(|value| value.as_str().map(str::to_owned));
        let params = self.property("audio-params").await;
        let out = self.property("audio-out-params").await;
        let encoding = self
            .property("af")
            .await
            .and_then(|value| value.as_array().cloned())
            .is_some_and(|filters| {
                filters
                    .iter()
                    .any(|filter| filter.get("name").and_then(Value::as_str) == Some("lavcac3enc"))
            });
        if codec.is_none() && out.is_none() {
            return None;
        }
        Some(mediabox_core::StreamAudio::from_mpv(codec, params.as_ref(), out.as_ref(), encoding))
    }

    /// One mpv command, and whether it succeeded.
    pub async fn command(&self, args: Value) -> bool {
        self.ask(json!({"command": args, "request_id": 40}))
            .await
            .is_some_and(|answer| answer.get("error").and_then(Value::as_str) == Some("success"))
    }

    /// What the film is made of, as mpv lists it.
    pub async fn tracks(&self) -> Vec<Value> {
        self.property("track-list")
            .await
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
    }

    /// Add a subtitle from `url` without selecting it, and answer the track
    /// number mpv gave it.
    ///
    /// `url` is always the worker's loopback address: this player's ffmpeg
    /// has no TLS. mpv loads the file before it answers, so this waits longer
    /// than an ordinary request -- a large ASS file is parsed in the reply.
    pub async fn sub_add(&self, url: &str, title: &str, language: Option<&str>) -> Option<i64> {
        let mut args = vec![json!("sub-add"), json!(url), json!("auto"), json!(title)];
        if let Some(language) = language {
            args.push(json!(language));
        }
        let answer = self
            .ask_within(
                json!({"command": args, "request_id": 41}),
                std::time::Duration::from_secs(10),
            )
            .await?;
        if answer.get("error").and_then(Value::as_str) != Some("success") {
            return None;
        }
        self.tracks()
            .await
            .iter()
            .filter(|track| {
                track.get("type").and_then(Value::as_str) == Some("sub")
                    && track.get("external-filename").and_then(Value::as_str) == Some(url)
            })
            .filter_map(|track| track.get("id").and_then(Value::as_i64))
            .max()
    }

    /// Take a subtitle track out of the film.
    pub async fn sub_remove(&self, id: i64) -> bool {
        self.command(json!(["sub-remove", id])).await
    }

    /// Which subtitle track is on; None turns them off.
    pub async fn select_subtitle(&self, id: Option<i64>) -> bool {
        let value = id.map(|id| json!(id)).unwrap_or_else(|| json!("no"));
        self.command(json!(["set_property", "sid", value])).await
    }

    /// Where the subtitle's lines go: `video = speed * subtitle + delay`,
    /// which is exactly how mpv applies `sub-speed` and `sub-delay`
    /// (measured with mpv 0.41: a line at 2-4 s with speed 2 and delay 1
    /// shows from 5 s to 9 s).
    pub async fn subtitle_timing(&self, delay: f64, speed: f64) -> bool {
        let delay = self
            .command(json!(["set_property", "sub-delay", delay.clamp(-600.0, 600.0)]))
            .await;
        let speed = self
            .command(json!(["set_property", "sub-speed", speed.clamp(0.5, 2.0)]))
            .await;
        delay && speed
    }

    /// The subtitle line on screen now, plain.
    pub async fn subtitle_text(&self) -> Option<String> {
        self.property("sub-text")
            .await
            .and_then(|value| value.as_str().map(str::to_owned))
    }

    /// The same line with its styling, as ASS: `{\i1}` for an italic
    /// stretch, `\N` between lines. An SRT's `<i>` -- how a voice off
    /// screen is written -- arrives here and is gone from `sub-text`.
    pub async fn subtitle_ass(&self) -> Option<String> {
        self.property("sub-text/ass")
            .await
            .and_then(|value| value.as_str().map(str::to_owned))
    }

    /// Whether a player answers on the socket.
    pub async fn answers(&self) -> bool {
        self.ask(json!({"command": ["get_property", "pid"], "request_id": 22}))
            .await
            .is_some_and(|answer| answer.get("error").and_then(Value::as_str) == Some("success"))
    }

    pub async fn playing(&self) -> Option<Playing> {
        self.playing.lock().await.clone()
    }

    /// Name what is playing, if nobody did: live television names itself
    /// (its service's SDT name) a moment after it starts.
    pub async fn name_if_unnamed(&self, film: u64, title: &str) {
        if self.film() != film {
            return;
        }
        if let Some(playing) = self.playing.lock().await.as_mut()
            && playing.title.is_none()
        {
            playing.title = Some(title.to_string());
        }
    }

    /// Start the player on `url`, replacing whatever was playing.
    pub async fn start(
        &self,
        url: &str,
        start_seconds: u64,
        playing: Playing,
        display_sync: bool,
        audio_track: Option<u32>,
    ) -> Result<(), String> {
        self.stop().await;
        self.film.fetch_add(1, Ordering::SeqCst);
        // Live television is not held: the control plane matches the
        // display before it starts the player (Daemon::play_live), and a
        // held player buffers what the broadcast sends while it waits, then
        // plays that far behind it.
        let hold = playing.live().is_none();
        self.holding.store(hold, Ordering::SeqCst);
        *self.live_origin.lock().unwrap_or_else(|p| p.into_inner()) = None;
        self.display_sync.store(display_sync, Ordering::SeqCst);
        // A socket left behind by a player that did not exit cleanly would be
        // answered by nobody; mpv replaces it, but only if it is not there.
        let _ = tokio::fs::remove_file(&self.socket).await;

        let output = Command::new("systemd-run")
            .arg("--collect")
            .arg(format!("--unit={UNIT}"))
            .arg("--property=Type=exec")
            // The film cannot outlive the interface, because the picture is
            // the interface's. This player draws nowhere: it hands the
            // decoder's buffers to the process that holds the display, and
            // when that process goes there is no way to hand them to its
            // successor. mpv notices and closes itself -- but only if mpv is
            // still answering, and a player wedged on a decoder is exactly
            // when this matters. `BindsTo=` is the part that does not depend
            // on the player being well: systemd stops this unit when the
            // interface stops, including across a restart.
            //
            // Without it, measured on the Ultra on 2026-09-20: the interface
            // died with a film on, came back eighteen minutes later, and drew
            // the home screen while the same mpv went on playing the sound.
            .arg("--property=BindsTo=mediabox-tv-ui.service")
            .arg("--property=After=mediabox-tv-ui.service")
            // What the media runtime says about a film is the film's business
            // and not the system log's. Rockchip MPP logs one line per skipped
            // NAL unit through syslog; on one 4K film that was a hundred and
            // forty-five thousand lines, ninety-nine and a half per cent of
            // the journal, and it rotated away every record of the fault that
            // was being diagnosed. The launcher quietens MPP itself; this is
            // the ceiling for anything else that ever runs in here.
            .arg("--property=LogRateLimitIntervalSec=30s")
            .arg("--property=LogRateLimitBurst=500")
            // mpv's 4 is "quit because something asked me to", which is what
            // a stop of the interface looks like from inside the player.
            // Without this the journal records the ordinary end of a film as
            // `status=4/NOPERMISSION` and `Failed with result 'exit-code'`,
            // which is a lie the next person to read it has to disprove.
            .arg("--property=SuccessExitStatus=4")
            // The interface's own runtime directory, which is where it puts
            // the socket it listens for frames on. Not a compositor: this
            // product has not had one since the television interface became a
            // process that holds DRM master itself.
            .arg("--setenv=XDG_RUNTIME_DIR=/run/mediabox-ui")
            .arg(format!(
                "--setenv=MEDIABOX_PLAYER_IPC={}",
                self.socket.display()
            ))
            // Opened paused: the film's frame rate is read before a frame
            // moves, the display is matched to it, and only then is it let
            // go -- the television re-locks once, on the first frame, not
            // three seconds into the film with the sound already running.
            .arg(format!("--setenv=MEDIABOX_PLAYER_HOLD={}", u8::from(hold)))
            // "Oynatımı görüntüye eşitle" (OutputStatus::display_sync): the
            // launcher turns it into mpv's display-resample and no passthrough.
            .arg(format!(
                "--setenv=MEDIABOX_PLAYER_DISPLAY_SYNC={}",
                u8::from(display_sync)
            ))
            // The audio track the plan chose, so the film plays what the plan
            // says: the remux's own AC-3 mix rather than its TrueHD default
            // converted (measured on the Plus on 2026-10-17). Empty: mpv's
            // own choice.
            .arg(format!(
                "--setenv=MEDIABOX_PLAYER_AID={}",
                audio_track.map(|track| track.to_string()).unwrap_or_default()
            ))
            // Live television: the launcher opens its transport stream
            // without probing it by decoding (see packaging/mediabox-player).
            .arg(format!("--setenv=MEDIABOX_PLAYER_LIVE={}", u8::from(playing.live().is_some())))
            .arg(self.launcher.as_os_str())
            .arg(url)
            .arg(start_seconds.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| format!("oynatıcı başlatılamadı: {error}"))?;
        if !output.status.success() {
            self.holding.store(false, Ordering::SeqCst);
            return Err(format!(
                "oynatıcı başlatılamadı: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        *self.playing.lock().await = Some(playing);
        Ok(())
    }

    /// Whether a live channel is being opened in the running player.
    pub fn reloading(&self) -> bool {
        self.reloading.load(Ordering::SeqCst)
    }

    /// The next live channel is about to be opened in this player: from
    /// now the end of the current file is the channel change's, not the
    /// end of the film.
    pub fn begin_reload(&self) {
        self.reloading.store(true, Ordering::SeqCst);
    }

    pub fn end_reload(&self) {
        self.reloading.store(false, Ordering::SeqCst);
    }

    /// Open live channel `service_id`, on `url`, in the player that is
    /// running -- its video output, its decoder and above all its audio
    /// device stay as they are (mpv keeps the device between files of one
    /// format, `--gapless-audio=weak`), so the sound over HDMI is never
    /// stopped and started again. True once the new file plays.
    /// `None` once `called_off` says a newer channel was asked for: the
    /// newer one's loadfile replaces this one, nothing waits for its picture.
    pub async fn reload_live(
        &self,
        url: &str,
        service_id: u16,
        title: Option<String>,
        called_off: impl Fn() -> bool,
    ) -> Option<bool> {
        let loaded = self
            .ask(json!({"command": ["loadfile", url, "replace"], "request_id": 23}))
            .await
            .is_some_and(|answer| answer.get("error").and_then(Value::as_str) == Some("success"));
        if loaded {
            // The old file's end paused it (`--keep-open`); the new one plays.
            let _ = self.ask(json!({"command": ["set_property", "pause", false], "request_id": 24})).await;
            // From here the player is on the new channel, picture or not: the
            // status says so, and its `picture` is this channel's.
            *self.live_origin.lock().unwrap_or_else(|p| p.into_inner()) = None;
            if let Some(current) = self.playing.lock().await.as_mut() {
                current.content = Playback::Live { service_id };
                current.title = title.clone();
            }
        }
        let mut playing = false;
        // Another multiplex: the stream starts after the lock, and the picture
        // after the next I-frame. 3 s was too short for ATV after TRT HABER
        // (2026-10-23), and the fallback booted the demodulator again: 10.9 s.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while loaded && std::time::Instant::now() < deadline {
            if called_off() {
                self.end_reload();
                return None;
            }
            let idle = self.property("core-idle").await.and_then(|value| value.as_bool());
            let ended = self.property("eof-reached").await.and_then(|value| value.as_bool());
            if idle == Some(false) && ended != Some(true) {
                playing = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        *self.live_origin.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if let Some(current) = self.playing.lock().await.as_mut() {
            current.content = Playback::Live { service_id };
            current.title = title;
        }
        self.end_reload();
        Some(playing)
    }

    /// How far into the film the player has got, in whole seconds.
    ///
    /// Asked of the player itself rather than remembered here, because the
    /// person watching may have moved it.
    pub async fn position(&self) -> Option<u64> {
        let answer = self
            .ask(json!({"command": ["get_property", "time-pos"], "request_id": 1}))
            .await?;
        answer
            .get("data")
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(|seconds| seconds as u64)
    }

    /// Where the film is and whether it is moving.
    ///
    /// Three questions rather than one because mpv answers one property per
    /// request; the alternative is a command language on this side, and the
    /// point of this module is that there is not one.
    pub async fn status(&self) -> Value {
        let playing = self.playing().await;
        let number = |answer: Option<Value>| -> Option<f64> {
            answer?
                .get("data")?
                .as_f64()
                .filter(|value| value.is_finite())
        };
        let position = number(
            self.ask(json!({"command": ["get_property", "time-pos"], "request_id": 1}))
                .await,
        );
        let duration = number(
            self.ask(json!({"command": ["get_property", "duration"], "request_id": 2}))
                .await,
        );
        // Held while the display is matched to the film: not a pause anybody
        // asked for, and not drawn as one.
        let paused = self
            .ask(json!({"command": ["get_property", "pause"], "request_id": 3}))
            .await
            .and_then(|answer| answer.get("data").and_then(Value::as_bool))
            .map(|paused| paused && !self.holding.load(Ordering::SeqCst));

        // What the caller said, over what the decoder guessed. A proxied
        // session has no duration in it and mpv's estimate from a partial
        // transfer is not one either; zero means "unknown", which an interface
        // can draw honestly, and a wrong number is one it cannot.
        let known = playing.as_ref().and_then(|playing| playing.duration);
        let live = playing.as_ref().and_then(Playing::live).is_some();
        let duration = known
            .map(|seconds| seconds as f64)
            .or(duration.filter(|seconds| *seconds > 0.0 && !live))
            .unwrap_or(0.0);
        // Live television has no length, and its clock starts where the
        // broadcast's was when it came on.
        let position = match (live, position) {
            (true, Some(at)) => {
                let origin = *self.live_origin.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert(at);
                Some((at - origin).max(0.0))
            }
            (_, position) => position,
        };

        // What the film is made of, for the subtitle and audio menus. Asked
        // for with the position rather than on its own call: a menu that opens
        // on a list fetched when it opened is a menu that opens empty.
        let tracks = self
            .ask(json!({"command": ["get_property", "track-list"], "request_id": 4}))
            .await
            .and_then(|answer| answer.get("data").cloned())
            .unwrap_or_else(|| json!([]));
        let speed = number(
            self.ask(json!({"command": ["get_property", "speed"], "request_id": 5}))
                .await,
        )
        .unwrap_or(1.0);
        let sub_delay = number(
            self.ask(json!({"command": ["get_property", "sub-delay"], "request_id": 6}))
                .await,
        )
        .unwrap_or(0.0);
        let audio_delay = number(
            self.ask(json!({"command": ["get_property", "audio-delay"], "request_id": 7}))
                .await,
        )
        .unwrap_or(0.0);

        // Whether the player is putting pictures out now. A live channel's
        // first picture is read from here for as long as it takes, as Android's
        // TV input says it (TvInputService.Session.notifyVideoAvailable),
        // rather than decided once when the channel was asked for.
        let picture = self
            .ask(json!({"command": ["get_property", "core-idle"], "request_id": 8}))
            .await
            .and_then(|answer| answer.get("data").and_then(Value::as_bool))
            .map(|idle| !idle);

        json!({
            "playing": playing.is_some() && position.is_some(),
            "picture": picture,
            "source": playing.as_ref().and_then(|playing| match &playing.content {
                Playback::Film { source, .. } => Some(source.clone()),
                Playback::Live { .. } => None,
            }),
            "live_service": playing.as_ref().and_then(Playing::live),
            "title": playing.as_ref().and_then(|playing| playing.title.clone()),
            "position": position.unwrap_or(0.0),
            "duration": duration,
            "paused": paused.unwrap_or(false),
            "tracks": tracks,
            "speed": speed,
            "sub_delay": sub_delay,
            "audio_delay": audio_delay,
        })
    }

    /// Pause or resume, or jump, without stopping. Quiet if nothing is
    /// playing: a remote pressed at the wrong moment is not an error.
    pub async fn transport(&self, action: mediabox_core::TransportAction) -> bool {
        use mediabox_core::TransportAction;
        // Play/Pause during the hold is the viewer taking over. The film
        // looks as if it were playing, so the press means Pause: it stays
        // where it is and is no longer the hold's to let go.
        let held = matches!(action, TransportAction::PlayPause)
            && self.holding.swap(false, Ordering::SeqCst);
        let request = match action {
            TransportAction::PlayPause if held => json!({"command": ["set_property", "pause", true]}),
            TransportAction::PlayPause => json!({"command": ["cycle", "pause"]}),
            TransportAction::Seek { seconds } => {
                json!({"command": ["seek", seconds, "relative"]})
            }
            TransportAction::SeekTo { seconds } => {
                json!({"command": ["seek", seconds, "absolute"]})
            }
            // mpv takes "no" for a subtitle track that is off, and the list's
            // first row sends a negative id to mean exactly that.
            TransportAction::Subtitle { id } => {
                let value = if id < 0 { json!("no") } else { json!(id) };
                json!({"command": ["set_property", "sid", value]})
            }
            TransportAction::Audio { id } => {
                json!({"command": ["set_property", "aid", id]})
            }
            // Chosen by meaning, and put back on time: both are the subtitle
            // manager's (`crate::subtitles`), which knows what an id means.
            TransportAction::SubtitleChoose { .. } | TransportAction::SubtitleAutoSync => {
                return false;
            }
            TransportAction::SubtitleDelay { seconds } => {
                json!({"command": ["set_property", "sub-delay", seconds.clamp(-60.0, 60.0)]})
            }
            TransportAction::AudioDelay { seconds } => {
                json!({"command": ["set_property", "audio-delay", seconds.clamp(-60.0, 60.0)]})
            }
            TransportAction::Speed { value } => {
                json!({"command": ["set_property", "speed", value.clamp(0.25, 4.0)]})
            }
            // Two properties, not one: panscan fills the panel by cropping the
            // edges and keepaspect is what lets the shapes bend. Both are set
            // every time so the modes cannot half-apply over each other.
            TransportAction::Scale { mode } => {
                let (keep, panscan) = match mode {
                    mediabox_core::ScaleMode::Fit => (true, 0.0),
                    mediabox_core::ScaleMode::Crop => (true, 1.0),
                    mediabox_core::ScaleMode::Stretch => (false, 0.0),
                };
                let _ = self
                    .ask(json!({"command": ["set_property", "keepaspect", keep]}))
                    .await;
                json!({"command": ["set_property", "panscan", panscan]})
            }
        };
        self.ask(request).await.is_some()
    }

    /// Stop the player and forget it. Quiet if nothing is playing.
    pub async fn stop(&self) -> Option<Playing> {
        let playing = self.playing.lock().await.take();
        self.film.fetch_add(1, Ordering::SeqCst);
        self.holding.store(false, Ordering::SeqCst);
        // Ask first: mpv exits cleanly and gives back the decoder and the
        // compositor surface. systemd stops the unit either way.
        let _ = self.ask(json!({"command": ["quit"]})).await;
        let _ = Command::new("systemctl")
            .arg("stop")
            .arg(UNIT)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        playing
    }

    /// One request on mpv's IPC socket, and its answer.
    async fn ask(&self, request: Value) -> Option<Value> {
        self.ask_within(request, std::time::Duration::from_millis(800)).await
    }

    async fn ask_within(&self, request: Value, patience: std::time::Duration) -> Option<Value> {
        if !Path::new(&self.socket).exists() {
            return None;
        }
        let stream = tokio::time::timeout(
            std::time::Duration::from_millis(800),
            UnixStream::connect(&self.socket),
        )
        .await
        .ok()?
        .ok()?;
        let (reader, mut writer) = stream.into_split();
        let line = format!("{request}\n");
        writer.write_all(line.as_bytes()).await.ok()?;
        writer.flush().await.ok()?;

        let mut reader = BufReader::new(reader);
        let mut buffer = String::new();
        // mpv answers with one JSON object per line, and may send events
        // before the answer; the reply carries the request id back.
        for _ in 0..16 {
            buffer.clear();
            let read = tokio::time::timeout(patience, reader.read_line(&mut buffer))
            .await
            .ok()?
            .ok()?;
            if read == 0 {
                return None;
            }
            if let Ok(value) = serde_json::from_str::<Value>(buffer.trim())
                && value.get("error").is_some()
            {
                return Some(value);
            }
        }
        None
    }
}
