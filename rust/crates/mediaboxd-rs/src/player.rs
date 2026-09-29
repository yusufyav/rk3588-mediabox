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
    /// The session the worker opened for it; stopped with the player.
    pub session_id: String,
    /// The address Kodi is given when the film is handed over: the session's
    /// own `handoff.kodiPlaybackUrl`, on the media core's loopback. It is
    /// something Kodi opens, never something the core can be asked to resolve
    /// -- it refuses its own loopback as a source, which is what broke the
    /// handover once. See `Daemon::handoff_to_kodi`.
    pub source: String,
    /// Where that film actually comes from: the upstream address the media
    /// core resolved (`handoff.resolvedInput`). When `source` turns out to be
    /// the core's own proxy, this is the only thing a second player can be
    /// given -- the proxy is one ffmpeg on one pipe and a late reader joins it
    /// mid-stream, not at the beginning.
    pub origin: String,
    /// What to call it, and how long it is, as the caller knew them. Neither
    /// is derivable here; see `Request::MediaPlayHere`.
    pub title: Option<String>,
    pub duration: Option<u64>,
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
}

impl PlayerManager {
    pub fn new(launcher: impl Into<PathBuf>, socket: impl Into<PathBuf>) -> Self {
        Self {
            launcher: launcher.into(),
            socket: socket.into(),
            playing: Mutex::new(None),
            film: AtomicU64::new(0),
            holding: AtomicBool::new(false),
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
        if before.mpv_spdif() != after.mpv_spdif() {
            let _ = self.ask(set("audio-spdif", json!(after.mpv_spdif()))).await;
        }
        if before.mpv_af() != after.mpv_af() {
            let _ = self.ask(set("af", json!(after.mpv_af()))).await;
        }
        let _ = self.ask(set("volume", json!(after.volume))).await;
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

    /// Whether a player answers on the socket.
    pub async fn answers(&self) -> bool {
        self.ask(json!({"command": ["get_property", "pid"], "request_id": 22}))
            .await
            .is_some_and(|answer| answer.get("error").and_then(Value::as_str) == Some("success"))
    }

    pub async fn playing(&self) -> Option<Playing> {
        self.playing.lock().await.clone()
    }

    /// Start the player on `url`, replacing whatever was playing.
    pub async fn start(
        &self,
        url: &str,
        start_seconds: u64,
        playing: Playing,
    ) -> Result<(), String> {
        self.stop().await;
        self.film.fetch_add(1, Ordering::SeqCst);
        self.holding.store(true, Ordering::SeqCst);
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
            .arg("--setenv=MEDIABOX_PLAYER_HOLD=1")
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
        let duration = known
            .map(|seconds| seconds as f64)
            .or(duration.filter(|seconds| *seconds > 0.0))
            .unwrap_or(0.0);

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

        json!({
            "playing": playing.is_some() && position.is_some(),
            "source": playing.as_ref().map(|playing| playing.source.clone()),
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
