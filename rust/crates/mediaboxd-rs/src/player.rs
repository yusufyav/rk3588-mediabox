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
    /// What to call it, and how long it is, as the caller knew them. Neither
    /// is derivable here; see `Request::MediaPlayHere`.
    pub title: Option<String>,
    pub duration: Option<u64>,
}

pub struct PlayerManager {
    launcher: PathBuf,
    socket: PathBuf,
    playing: Mutex<Option<Playing>>,
}

impl PlayerManager {
    pub fn new(launcher: impl Into<PathBuf>, socket: impl Into<PathBuf>) -> Self {
        Self {
            launcher: launcher.into(),
            socket: socket.into(),
            playing: Mutex::new(None),
        }
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
        let paused = self
            .ask(json!({"command": ["get_property", "pause"], "request_id": 3}))
            .await
            .and_then(|answer| answer.get("data").and_then(Value::as_bool));

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
        let request = match action {
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
            let read = tokio::time::timeout(
                std::time::Duration::from_millis(800),
                reader.read_line(&mut buffer),
            )
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
