//! The interface's own player.
//!
//! A film opened from the catalogue is a window of the interface, not another
//! application taking the television away from it. That is the whole
//! difference between this and handing Kodi the display: the catalogue is
//! still behind it, Back returns to it, and nothing has to be handed back.
//!
//! A film's player is mpv, built on the appliance against MediaBox's own
//! Rockchip media runtime; live television's is mediabox-live
//! (rust/crates/mediabox-live). `mediabox-player` starts whichever it is, and
//! both answer the same JSON IPC. This module only starts it, asks it where it
//! has got to, and stops it.

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
    /// Live television's position is the player's clock: the first one read
    /// on a channel, so what is shown is how long it has been on.
    live_origin: std::sync::Mutex<Option<f64>>,
    /// The number of the live channel opened last (its epoch): moved on
    /// every open and every zap, so whatever acts for one channel can tell
    /// it has been replaced -- the film number stays through a zap.
    epoch: AtomicU64,
    /// Display mode changes committed under the sound device, counted, and
    /// the count the device was last opened after: a mode commit stops
    /// HDMI's sound stream (`rematch_live`), and the device is opened again
    /// for the change it came after -- not for one that came later.
    mode_changes: AtomicU64,
    sound_opened_after: AtomicU64,
    /// The rate the live channel's display was matched to at its opening,
    /// and the channel's epoch: a later change in the stream is a format
    /// change (`follow_film`).
    live_cadence: std::sync::Mutex<Option<(u64, mediabox_core::Cadence)>>,
    /// The number of the last sound plan sent to the live player.
    audio_seq: AtomicU64,
}

/// How long the live player's answer to a plan is waited for: longer than
/// it may take (`LIVE_PLAN_ANSWER_MS`, inside which its own deadline and
/// the plan before put back both end), so that an answer given up on here
/// is never one the player acts on after -- it abandons a plan its own
/// socket stopped waiting for.
const LIVE_PLAN_PATIENCE: std::time::Duration = std::time::Duration::from_millis(mediabox_core::audio::LIVE_PLAN_ANSWER_MS + 2_000);
/// A plan refused as another channel's is asked again this often, while a
/// channel change is under way (the player opens it within a second).
const LIVE_PLAN_TRIES: usize = 10;
const LIVE_PLAN_AGAIN: std::time::Duration = std::time::Duration::from_millis(200);

/// How long a live player just started is given to answer on its socket,
/// as its opening gate gives it (`live_gate::OPEN_PATIENCE`), and then to
/// have its plan's device open: what it may take for its own tries of a
/// device busy at its start, within what a plan's answer may take.
const LIVE_SOCKET_PATIENCE: std::time::Duration = crate::live_gate::OPEN_PATIENCE;
const LIVE_SOUND_PATIENCE: std::time::Duration = std::time::Duration::from_millis(mediabox_core::audio::LIVE_PLAN_ANSWER_MS);
const LIVE_SOUND_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// The live player's start was called off (Back) while its device was
/// being checked.
pub const START_CALLED_OFF: &str = "called off";

/// The launch argument that hands the live player its sound plan, whole
/// (`mediabox_core::audio::plan_to_env`).
pub fn live_plan_arg(plan: &mediabox_core::AudioPlan) -> String {
    format!("--setenv={}={}", mediabox_core::audio::LIVE_PLAN_ENV, mediabox_core::audio::plan_to_env(plan))
}

/// The interface's player takes the sound plan (`apply_audio`).
impl crate::audio::PlanTaker for PlayerManager {
    fn take<'a>(
        &'a self,
        before: &'a mediabox_core::AudioPlan,
        after: &'a mediabox_core::AudioPlan,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<crate::audio::Taken, String>> + Send + 'a>> {
        Box::pin(self.apply_audio(before, after))
    }
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
            epoch: AtomicU64::new(0),
            mode_changes: AtomicU64::new(0),
            sound_opened_after: AtomicU64::new(0),
            live_cadence: std::sync::Mutex::new(None),
            audio_seq: AtomicU64::new(0),
        }
    }

    /// A new live channel's number (`epoch`).
    pub fn next_epoch(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The live channel opened last.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// A display mode change was committed under the sound device.
    pub fn mode_changed(&self) {
        self.mode_changes.fetch_add(1, Ordering::SeqCst);
    }

    /// The sound device was opened after the last mode change.
    pub fn sound_current(&self) -> bool {
        self.sound_opened_after.load(Ordering::SeqCst) >= self.mode_changes.load(Ordering::SeqCst)
    }

    pub fn set_live_cadence(&self, epoch: u64, cadence: mediabox_core::Cadence) {
        *self.live_cadence.lock().unwrap_or_else(|p| p.into_inner()) = Some((epoch, cadence));
    }

    /// The rate channel `epoch`'s display was matched to.
    pub fn live_cadence(&self, epoch: u64) -> Option<mediabox_core::Cadence> {
        self.live_cadence.lock().unwrap_or_else(|p| p.into_inner()).filter(|(of, _)| *of == epoch).map(|(_, cadence)| cadence)
    }

    /// Let live channel `epoch` go: its display is matched (`outcome` says
    /// how). The player refuses another channel's number.
    pub async fn gate_open(&self, epoch: u64, outcome: &str) -> Result<(), String> {
        let answer = self
            .ask(json!({"command": ["mediabox-gate", "open", epoch, outcome], "request_id": 42}))
            .await
            .ok_or(crate::live_gate::NO_ANSWER)?;
        match answer.get("error").and_then(Value::as_str) {
            Some("success") => Ok(()),
            other => Err(other.unwrap_or("no answer").to_string()),
        }
    }

    /// Give live channel `epoch` up: the player's gate stays shut and the
    /// player ends, saying `code`. A player past answering is stopped by the
    /// caller all the same.
    pub async fn gate_fail(&self, epoch: u64, code: &str) -> Result<(), String> {
        let answer = self
            .ask(json!({"command": ["mediabox-gate", "fail", epoch, code], "request_id": 43}))
            .await
            .ok_or(crate::live_gate::NO_ANSWER)?;
        match answer.get("error").and_then(Value::as_str) {
            Some("success") => Ok(()),
            other => Err(other.unwrap_or("no answer").to_string()),
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

    /// Put a changed sound plan into what is playing. Nothing when nothing
    /// is: the next film or channel is started from the plan anyway.
    ///
    /// A film (mpv): the volume always, and the device, the passthrough
    /// formats and the encoder when they moved -- mpv reopens its output for
    /// those, a moment of silence rather than a restart.
    ///
    /// Live television (mediabox-live): the whole plan in one request
    /// (`apply_live_audio`), done once the device it names is open there.
    /// An error is the player's: the plan before it is still in force.
    pub async fn apply_audio(&self, before: &mediabox_core::AudioPlan, after: &mediabox_core::AudioPlan) -> Result<crate::audio::Taken, String> {
        let Some(playing) = self.playing().await else { return Ok(crate::audio::Taken::Nothing) };
        if playing.live().is_some() {
            return self.apply_live_audio(after).await.map(|()| crate::audio::Taken::Player);
        }
        let set = |name: &str, value: Value| json!({"command": ["set_property", name, value], "request_id": 30});
        if before.mpv_device() != after.mpv_device() && after.device.is_some() {
            // The device is what the route is: not taken, not in force.
            let answer = self.ask(set("audio-device", json!(after.mpv_device()))).await;
            match answer.as_ref().and_then(|answer| answer.get("error")).and_then(Value::as_str) {
                Some("success") => {}
                other => return Err(format!("oynatıcı ses aygıtını almadı: {}", other.unwrap_or("yanıt yok"))),
            }
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
        Ok(crate::audio::Taken::Player)
    }

    /// The whole plan to the live player, for the channel on now (its
    /// epoch) and numbered, so that an older request that comes late is
    /// not done after a newer one. Done when the player says the device it
    /// opened is the plan's. A channel opened meanwhile -- the player not
    /// on the channel asked for -- is asked again, for the one on now.
    async fn apply_live_audio(&self, plan: &mediabox_core::AudioPlan) -> Result<(), String> {
        let wanted = plan.device.as_ref().map(|device| device.pcm.clone());
        let mut why = String::new();
        for _ in 0..LIVE_PLAN_TRIES {
            // The channel's number; none (null) only before any channel had
            // one, which the player accepts only for a channel opened so.
            let epoch = Some(self.epoch()).filter(|epoch| *epoch > 0);
            let seq = self.audio_seq.fetch_add(1, Ordering::SeqCst) + 1;
            let answer = self
                .ask_within(
                    json!({"command": ["mediabox-audio-plan", plan, epoch, seq], "request_id": 44}),
                    LIVE_PLAN_PATIENCE,
                )
                .await
                .ok_or("canlı oynatıcı ses planına yanıt vermedi")?;
            match answer.get("error").and_then(Value::as_str) {
                Some("success") => {
                    let opened = answer.get("data").and_then(Value::as_str).map(str::to_owned);
                    return if opened == wanted {
                        Ok(())
                    } else {
                        Err(format!("canlı oynatıcı {opened:?} açtı, plan {wanted:?}"))
                    };
                }
                Some(other) if other == mediabox_core::audio::LIVE_PLAN_OTHER_CHANNEL => {
                    why = other.to_string();
                    tokio::time::sleep(LIVE_PLAN_AGAIN).await;
                }
                other => return Err(other.unwrap_or("canlı oynatıcı yanıt vermedi").to_string()),
            }
        }
        Err(format!("canlı oynatıcı ses planını almadı: {why}"))
    }

    /// What the film's sound is and what leaves the box, as its player says.
    pub async fn audio_stream(&self) -> Option<mediabox_core::StreamAudio> {
        let live = self.playing().await?.live().is_some();
        let codec = self
            .property("audio-codec-name")
            .await
            .and_then(|value| value.as_str().map(str::to_owned));
        let params = self.property("audio-params").await;
        let out = self.property("audio-out-params").await;
        // mpv encodes through its lavcac3enc filter; live television's AC-3
        // that is not the broadcast's own is always its encoder's.
        let encoding = live
            || self
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

    /// The player's sound device opened again (`ao-reload`). The live
    /// player answers once it is open, or once it gave up after its tries
    /// (mediabox-live `SINK_RETRY_MS`, 1.55 s): longer than a request's
    /// usual patience.
    ///
    /// Counted for the mode changes made before it was asked: one made
    /// while it was on its way needs another (`sound_current`).
    pub async fn reload_sound(&self) -> Result<(), String> {
        let after = self.mode_changes.load(Ordering::SeqCst);
        let answer = self
            .ask_within(json!({"command": ["ao-reload"], "request_id": 41}), std::time::Duration::from_secs(3))
            .await
            .ok_or("no answer")?;
        match answer.get("error").and_then(Value::as_str) {
            Some("success") => {
                self.sound_opened_after.fetch_max(after, Ordering::SeqCst);
                Ok(())
            }
            other => Err(other.unwrap_or("no answer").to_string()),
        }
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
        live: Option<(u64, &mediabox_core::AudioPlan)>,
    ) -> Result<(), String> {
        self.stop().await;
        self.film.fetch_add(1, Ordering::SeqCst);
        // A new player opens the sound device on the mode on the wire now.
        self.sound_opened_after.store(self.mode_changes.load(Ordering::SeqCst), Ordering::SeqCst);
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
            // Live television: the launcher starts mediabox-live, not mpv.
            .arg(format!("--setenv=MEDIABOX_PLAYER_LIVE={}", u8::from(playing.live().is_some())))
            // The live channel's number: held at its opening gate until the
            // display is matched (`live_gate`). Empty: not gated.
            .arg(format!("--setenv=MEDIABOX_LIVE_EPOCH={}", live.map(|(epoch, _)| epoch.to_string()).unwrap_or_default()))
            // Live television's sound plan: the one in force, which the
            // player opens and works out nothing of its own (`start_live`).
            .args(live.filter(|_| playing.live().is_some()).map(|(_, plan)| live_plan_arg(plan)))
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

    /// Start live television's player and put its sound in force as one
    /// change of the sound's (`Audio::starting`): the player is started on
    /// the plan in force -- the route's, whose one owner is `audio`; the
    /// player works out none of its own -- and only once it says on its
    /// socket that the device it has open is that plan's is it the route
    /// applied. Not so within its time: the player is stopped and the
    /// route is not applied (`Starting::not_applied`); the error says why.
    /// No change of the sound runs meanwhile: a headset coming while the
    /// player starts is put to it after, as a change of its own.
    pub async fn start_live(
        &self,
        audio: &crate::audio::Audio,
        url: &str,
        playing: Playing,
        display_sync: bool,
        epoch: u64,
        called_off: impl Fn() -> bool + Sync,
    ) -> Result<(), String> {
        self.start_live_with(
            audio,
            |plan| async move { self.start(url, 0, playing, display_sync, None, Some((epoch, &plan))).await },
            called_off,
            (LIVE_SOCKET_PATIENCE, LIVE_SOUND_PATIENCE),
        )
        .await
    }

    /// `start_live`, the player launched by `launch` on the plan given it.
    pub(crate) async fn start_live_with<F, Fut>(
        &self,
        audio: &crate::audio::Audio,
        launch: F,
        called_off: impl Fn() -> bool + Sync,
        patience: (std::time::Duration, std::time::Duration),
    ) -> Result<(), String>
    where
        F: FnOnce(mediabox_core::AudioPlan) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let sound = audio.starting().await;
        let plan = sound.plan().clone();
        if let Err(error) = launch(plan.clone()).await {
            sound.not_applied();
            return Err(error);
        }
        match self.live_sound_opened(&plan, &called_off, patience).await {
            Ok(()) => {
                sound.applied();
                Ok(())
            }
            Err(error) => {
                sound.not_applied();
                self.stop().await;
                Err(error)
            }
        }
    }

    /// Whether the live player just started has `plan`'s device open, as it
    /// says on its socket (`audio-device`: the PCM its sound sink has open,
    /// null for none or a stand-in): asked until it does, its socket up
    /// within the first patience and the device within the second after.
    async fn live_sound_opened(
        &self,
        plan: &mediabox_core::AudioPlan,
        called_off: &(dyn Fn() -> bool + Sync),
        (socket_patience, sound_patience): (std::time::Duration, std::time::Duration),
    ) -> Result<(), String> {
        let wanted = plan.device.as_ref().map(|device| device.pcm.clone());
        let started = tokio::time::Instant::now();
        let mut answered: Option<tokio::time::Instant> = None;
        let mut opened: Option<String> = None;
        loop {
            if called_off() {
                return Err(START_CALLED_OFF.into());
            }
            if let Some(answer) = self.ask(json!({"command": ["get_property", "audio-device"], "request_id": 45})).await {
                answered.get_or_insert_with(tokio::time::Instant::now);
                opened = (answer.get("error").and_then(Value::as_str) == Some("success"))
                    .then(|| answer.get("data").and_then(Value::as_str).map(str::to_owned))
                    .flatten();
                if opened == wanted {
                    return Ok(());
                }
            }
            match answered {
                None if started.elapsed() >= socket_patience => {
                    return Err(format!("canlı oynatıcının soketi {} ms içinde açılmadı", socket_patience.as_millis()));
                }
                Some(at) if at.elapsed() >= sound_patience => {
                    return Err(format!("canlı oynatıcı {opened:?} açtı, plan {wanted:?}"));
                }
                _ => tokio::time::sleep(LIVE_SOUND_POLL).await,
            }
        }
    }

    /// Open live channel `service_id`, on `url`, in the player that is
    /// running: its picture plane, its decoder and its sound device stay as
    /// they are, so the sound over HDMI is never stopped and started again.
    /// Done once the player has the new channel's stream open; its first
    /// picture is the status's `picture`, read for as long as it takes. An
    /// error is the player's own reason.
    pub async fn reload_live(&self, url: &str, service_id: u16, title: Option<String>, epoch: u64) -> Result<(), String> {
        let answer = self
            .ask(json!({"command": ["loadfile", url, "replace", -1, format!("mediabox-epoch={epoch}")], "request_id": 23}))
            .await
            .ok_or("oynatıcı yanıt vermedi")?;
        match answer.get("error").and_then(Value::as_str) {
            Some("success") => {}
            other => return Err(other.unwrap_or("oynatıcı yanıt vermedi").to_string()),
        }
        *self.live_origin.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if let Some(current) = self.playing.lock().await.as_mut() {
            current.content = Playback::Live { service_id };
            current.title = title;
        }
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
        // rather than decided once when the channel was asked for. For live
        // television it is mediabox-live's word that the channel opened last
        // has had a frame committed on the interface's plane (or, without a
        // picture, its sound at the device), and is not paused.
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
        // Not from tests: on a developer's machine the system's systemd asked
        // for a password (polkit) to stop a unit it does not have, and a test
        // waited on the dialog.
        if !cfg!(test) {
            let _ = Command::new("systemctl")
                .arg("stop")
                .arg(UNIT)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
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

/// A live player for tests: every request handed to the test, which says
/// when and what it is answered.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    pub type Request = (Value, tokio::sync::oneshot::Sender<(String, Value)>);

    /// Listening on `socket`; each sound plan request comes on the receiver
    /// with where to answer it (an `error`, and `data`). Anything else --
    /// the status's properties -- is answered here as a property this
    /// player does not have.
    pub fn live(socket: &Path) -> tokio::sync::mpsc::UnboundedReceiver<Request> {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        let (requests, inbox) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let requests = requests.clone();
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let mut line = String::new();
                    if BufReader::new(reader).read_line(&mut line).await.is_err() {
                        return;
                    }
                    let Ok(request) = serde_json::from_str::<Value>(&line) else { return };
                    let (error, data) = if request["command"][0] == "mediabox-audio-plan" {
                        let (answer, answered) = tokio::sync::oneshot::channel();
                        if requests.send((request.clone(), answer)).is_err() {
                            return;
                        }
                        let Ok(answer) = answered.await else { return };
                        answer
                    } else {
                        ("property unavailable".to_string(), Value::Null)
                    };
                    let answer = json!({"error": error, "data": data, "request_id": request["request_id"]});
                    let _ = writer.write_all(format!("{answer}\n").as_bytes()).await;
                });
            }
        });
        inbox
    }

    /// The device a plan request names.
    pub fn device_of(request: &Value) -> Value {
        request["command"][1]["device"]["pcm"].clone()
    }

    /// Every request answered as the player that opens what it is asked.
    pub fn opening_all(mut inbox: tokio::sync::mpsc::UnboundedReceiver<Request>) -> std::sync::Arc<std::sync::Mutex<Vec<Value>>> {
        let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let into = std::sync::Arc::clone(&got);
        tokio::spawn(async move {
            while let Some((request, answer)) = inbox.recv().await {
                into.lock().unwrap().push(request.clone());
                let _ = answer.send(("success".into(), device_of(&request)));
            }
        });
        got
    }

    /// A live player that keeps the device its sound sink has open, as
    /// mediabox-live does: started on a plan it opens that plan's device --
    /// none (its stand-in) if `refuses` names it; asked a plan, it opens its
    /// device, or keeps the one before and says why if it refuses it.
    /// `audio-device` says what is open.
    #[derive(Default)]
    pub struct Sink {
        pub opened: std::sync::Mutex<Option<String>>,
        pub refuses: std::sync::Mutex<Vec<String>>,
        /// Every plan's device asked of it, in order.
        pub asked: std::sync::Mutex<Vec<Value>>,
    }

    /// A player on `sink`, started on `plan`, listening on `socket`.
    pub fn sink_player(socket: &Path, sink: std::sync::Arc<Sink>, plan: &mediabox_core::AudioPlan) {
        let _ = std::fs::remove_file(socket);
        let pcm = plan.device.as_ref().map(|device| device.pcm.clone());
        *sink.opened.lock().unwrap() = pcm.filter(|pcm| !sink.refuses.lock().unwrap().contains(pcm));
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let sink = std::sync::Arc::clone(&sink);
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let mut line = String::new();
                    if BufReader::new(reader).read_line(&mut line).await.is_err() {
                        return;
                    }
                    let Ok(request) = serde_json::from_str::<Value>(&line) else { return };
                    let (error, data) = match (request["command"][0].as_str(), request["command"][1].as_str()) {
                        (Some("get_property"), Some("audio-device")) => ("success", json!(*sink.opened.lock().unwrap())),
                        (Some("mediabox-audio-plan"), _) => {
                            let device = device_of(&request);
                            sink.asked.lock().unwrap().push(device.clone());
                            let pcm = device.as_str().map(str::to_owned);
                            if pcm.as_ref().is_some_and(|pcm| sink.refuses.lock().unwrap().contains(pcm)) {
                                ("ses aygıtı açılamadı", Value::Null)
                            } else {
                                *sink.opened.lock().unwrap() = pcm;
                                ("success", device)
                            }
                        }
                        _ => ("property unavailable", Value::Null),
                    };
                    let answer = json!({"error": error, "data": data, "request_id": request["request_id"]});
                    let _ = writer.write_all(format!("{answer}\n").as_bytes()).await;
                });
            }
        });
    }

    impl PlayerManager {
        /// Live television on, as after a channel's start.
        pub async fn pretend_live(&self) {
            *self.playing.lock().await = Some(Playing { content: Playback::Live { service_id: 10603 }, title: None, duration: None });
            self.next_epoch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    fn plan(pcm: &str) -> mediabox_core::AudioPlan {
        serde_json::from_value(json!({
            "device": {"id": "bluetooth:x", "label": "x", "kind": "bluetooth", "card_id": "bluealsa", "pcm": pcm, "bitstream": false},
            "passthrough": [], "volume": 60, "muted": false
        }))
        .unwrap()
    }

    /// A live player on `socket` answering each request with the next of
    /// `answers` (an `error` and the device open), and keeping what it got.
    fn live_player(socket: &Path, answers: Vec<(&'static str, Value)>) -> std::sync::Arc<std::sync::Mutex<Vec<Value>>> {
        let listener = UnixListener::bind(socket).unwrap();
        let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let into = std::sync::Arc::clone(&got);
        tokio::spawn(async move {
            for (error, data) in answers {
                let Ok((stream, _)) = listener.accept().await else { return };
                let (reader, mut writer) = stream.into_split();
                let mut line = String::new();
                BufReader::new(reader).read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                into.lock().unwrap().push(request.clone());
                let answer = json!({"error": error, "data": data, "request_id": request["request_id"]});
                writer.write_all(format!("{answer}\n").as_bytes()).await.unwrap();
            }
        });
        got
    }

    async fn on_live(dir: &Path) -> PlayerManager {
        let player = PlayerManager::new("/nonexistent", dir.join("player.sock"));
        *player.playing.lock().await = Some(Playing { content: Playback::Live { service_id: 10603 }, title: None, duration: None });
        player.next_epoch();
        player
    }

    /// The live player is sent the whole plan in one request, for the
    /// channel on, numbered; done when it opened the plan's device.
    #[tokio::test]
    async fn the_live_player_is_sent_the_whole_plan_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let player = on_live(dir.path()).await;
        let got = live_player(&player.socket, vec![("success", json!("bluealsa:DEV=1"))]);
        let before = plan("hdmi:CARD=x,DEV=0");
        assert_eq!(player.apply_audio(&before, &plan("bluealsa:DEV=1")).await, Ok(crate::audio::Taken::Player));
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 1, "one request: {got:?}");
        let command = got[0]["command"].as_array().unwrap();
        assert_eq!(command[0], "mediabox-audio-plan");
        assert_eq!(serde_json::from_value::<mediabox_core::AudioPlan>(command[1].clone()).unwrap(), plan("bluealsa:DEV=1"));
        assert_eq!(command[2], json!(1), "the channel's epoch");
        assert_eq!(command[3], json!(1), "the first plan sent");
    }

    /// Not done: said so, never taken for done -- the player's own reason,
    /// or a device open that is not the plan's.
    #[tokio::test]
    async fn a_plan_the_live_player_did_not_put_in_force_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let player = on_live(dir.path()).await;
        let _got = live_player(&player.socket, vec![("ses aygıtı açılamadı", Value::Null), ("success", json!("hdmi:CARD=x,DEV=0"))]);
        let before = plan("hdmi:CARD=x,DEV=0");
        let refused = player.apply_audio(&before, &plan("bluealsa:DEV=1")).await;
        assert_eq!(refused, Err("ses aygıtı açılamadı".into()));
        let other = player.apply_audio(&before, &plan("bluealsa:DEV=1")).await;
        assert!(other.is_err(), "{other:?}");
    }

    /// Refused as another channel's while a channel change is under way:
    /// asked again, a newer number, for the channel on now.
    #[tokio::test]
    async fn a_plan_refused_as_another_channel_s_is_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let player = on_live(dir.path()).await;
        let got = live_player(
            &player.socket,
            vec![(mediabox_core::audio::LIVE_PLAN_OTHER_CHANNEL, Value::Null), ("success", json!("bluealsa:DEV=1"))],
        );
        player.next_epoch();
        let before = plan("hdmi:CARD=x,DEV=0");
        assert_eq!(player.apply_audio(&before, &plan("bluealsa:DEV=1")).await, Ok(crate::audio::Taken::Player));
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0]["command"][3].clone(), got[1]["command"][3].clone()), (json!(1), json!(2)), "a newer number");
        assert_eq!(got[1]["command"][2], json!(2), "the channel on now");
    }
}
