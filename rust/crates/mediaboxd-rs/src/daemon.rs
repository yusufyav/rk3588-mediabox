use crate::kodi::KodiClient;
use crate::leds::LedController;
use crate::lifecycle::{ApplicationManager, KodiLifecycle, SurfaceManager};
use crate::media::MediaClient;
use crate::player::{PlayerManager, Playing};
use mediabox_cec::Adapter;
use mediabox_core::{
    CecStatus, InputMode, InputSource, PowerAction, Request, Response, ServiceHealth, Surface,
    SystemStatus,
};
use mediabox_input::{InputManager, KodiRoute, RouteDecision};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const MAX_REQUEST_BYTES: usize = 64 * 1024;

pub struct CecRuntime {
    pub adapter: Option<Arc<Adapter>>,
    pub unavailable: CecStatus,
}

impl CecRuntime {
    pub fn status(&self) -> CecStatus {
        self.adapter
            .as_ref()
            .map_or_else(|| self.unavailable.clone(), |adapter| adapter.status())
    }
}

pub struct AppState {
    pub kodi: Arc<KodiClient>,
    pub lifecycle: KodiLifecycle,
    pub cec: CecRuntime,
    pub input: InputManager,
    pub media: Arc<MediaClient>,
    pub surface: SurfaceManager,
    /// What this box can run. The launcher is driven from here.
    pub applications: ApplicationManager,
    /// The interface's own player: a film inside the application rather than
    /// another application in front of it.
    pub player: Arc<PlayerManager>,
    /// The board's two indicator lights. Here rather than in the interface
    /// because `/sys/class/leds` is root's, and the unit that draws the
    /// television mounts /sys read-only.
    pub leds: LedController,
}

impl AppState {
    pub async fn handle(&self, request: Request) -> Response {
        match request {
            Request::Status => Response::success(self.status().await),
            Request::System => Response::success(system_snapshot()),
            Request::Diagnostics => Response::success(crate::system::diagnostics()),
            Request::KodiStatus => Response::success(self.kodi.status().await),
            Request::KodiPlayPause => result(self.kodi.play_pause().await),
            Request::KodiStop => result(self.kodi.stop().await),
            Request::KodiSeek { seconds } => result(self.kodi.seek_relative(seconds).await),
            Request::KodiOpen {
                url,
                resume_seconds,
            } => result(self.kodi.open(&url, resume_seconds).await),
            Request::KodiRestart => match self.lifecycle.restart().await {
                Ok(()) => Response::success(json!({"restarted":true})),
                Err(error) => Response::failure("KODI_LIFECYCLE_ERROR", error.to_string()),
            },
            Request::CecStatus => Response::success(self.cec.status()),
            Request::CecDevices => {
                let Some(adapter) = self.cec.adapter.clone() else {
                    return Response::failure("CEC_UNAVAILABLE", cec_error(&self.cec.status()));
                };
                match tokio::task::spawn_blocking(move || adapter.discover_devices()).await {
                    Ok(Ok(devices)) => Response::success(devices),
                    Ok(Err(error)) => Response::failure("CEC_ERROR", error.to_string()),
                    Err(error) => Response::failure("INTERNAL_ERROR", error.to_string()),
                }
            }
            Request::CecActiveSource => {
                cec_action(&self.cec, |adapter| adapter.active_source()).await
            }
            Request::CecWakeTv => cec_action(&self.cec, |adapter| adapter.wake_tv()).await,
            Request::CecStandbyTv => cec_action(&self.cec, |adapter| adapter.standby_tv()).await,
            Request::LedsStatus => Response::success(self.leds.status()),
            Request::LedsSet { mode } => match self.leds.set(mode) {
                Ok(status) => Response::success(status),
                Err(error) => Response::failure("LED_ERROR", error),
            },
            Request::MediaStatus => media_result(self.media.status().await),
            Request::MediaCapabilities => media_result(self.media.capabilities().await),
            Request::MediaHome => media_result(self.media.home().await),
            Request::MediaCatalog {
                media_type,
                id,
                addon_id,
                limit,
            } => media_result(
                self.media
                    .catalog(&media_type, &id, addon_id.as_deref(), limit)
                    .await,
            ),
            Request::MediaMeta { media_type, id } => {
                media_result(self.media.meta(&media_type, &id).await)
            }
            Request::MediaSubtitles {
                media_type,
                id,
                video_id,
            } => media_result(
                self.media
                    .subtitles(&media_type, &id, video_id.as_deref())
                    .await,
            ),
            Request::MediaLibrary => media_result(self.media.library().await),
            Request::MediaLibraryItem { id } => media_result(self.media.library_item(&id).await),
            Request::MediaResolve { stream } => media_result(self.media.resolve(stream).await),
            Request::MediaStreamPlan { stream } => {
                media_result(self.media.stream_plan(stream).await)
            }
            Request::MediaPlayOnKodi {
                url,
                stream,
                start_seconds,
            } => self.play_on_kodi(url, stream, start_seconds).await,
            Request::MediaPlayHere {
                url,
                stream,
                start_seconds,
                title,
                duration_seconds,
            } => {
                self.play_here(url, stream, start_seconds, title, duration_seconds)
                    .await
            }
            Request::MediaHandoffToKodi => self.handoff_to_kodi().await,
            Request::MediaStatusHere => Response::success(self.player.status().await),
            Request::MediaTransportHere { action } => {
                let moved = self.player.transport(action).await;
                Response::success(json!({"moved": moved}))
            }
            Request::MediaStopHere => {
                let stopped = self.player.stop().await;
                if let Some(playing) = &stopped {
                    self.stop_session(&playing.session_id).await;
                }
                Response::success(json!({"stopped": stopped.is_some()}))
            }
            Request::SurfaceStatus => Response::success(self.surface.status().await),
            Request::Applications => Response::success(self.applications.status().await),
            Request::ApplicationLaunch { id } => match self.applications.launch(&id).await {
                Ok(status) => {
                    if id != crate::lifecycle::IDLE {
                        remember(crate::owner::DisplayOwner::MediaBox);
                    }
                    Response::success(status)
                }
                Err(error) => Response::failure("APPLICATION_ERROR", error.to_string()),
            },
            Request::BrowserOpen { url } => match self.applications.browser_open(&url).await {
                Ok(status) => Response::success(status),
                Err(error) => Response::failure("APPLICATION_ERROR", error.to_string()),
            },
            Request::SurfaceSwitch { target } => match self.switch_surface(target).await {
                Ok(status) => Response::success(status),
                Err(error) => Response::failure("SURFACE_ERROR", error.to_string()),
            },
            Request::DisplayOwner => Response::success(self.display_owner_status().await),
            Request::DisplayOwnerSet { owner } => match crate::owner::DisplayOwner::parse(&owner) {
                Some(wanted) => match self.set_display_owner(wanted).await {
                    Ok(status) => Response::success(status),
                    Err(error) => Response::failure("DISPLAY_OWNER_ERROR", error.to_string()),
                },
                None => Response::failure(
                    "DISPLAY_OWNER_ERROR",
                    format!("bilinmeyen sahip '{owner}'; mediabox veya screenbridge"),
                ),
            },
            Request::MediaLogin { email, password } => {
                media_result(self.media.login(&email, &password).await)
            }
            Request::MediaLogout => media_result(self.media.logout().await),
            Request::MediaSearch { query } => media_result(self.media.search(&query).await),
            Request::MediaInspect { url } => media_result(self.media.inspect(&url).await),
            Request::MediaStreams { media_type, id } => {
                media_result(self.media.streams(&media_type, &id).await)
            }
            Request::MediaPolicy { url } => media_result(self.media.policy(&url).await),
            Request::MediaSessions => media_result(self.media.sessions().await),
            Request::MediaSessionStart { url } => {
                media_result(self.media.session_start(&url).await)
            }
            Request::MediaSessionStop { id } => media_result(self.media.session_stop(&id).await),
            Request::InputInject { action } => {
                let decision = self.input.publish(action, InputSource::Api, true, None);
                if let Err(error) = apply_kodi_route(&self.kodi, decision).await {
                    return Response::failure("INPUT_ROUTE_ERROR", error);
                }
                Response::success(
                    json!({"accepted":true,"action":action,"route":format!("{decision:?}")}),
                )
            }
            Request::InputMonitor => {
                Response::failure("PROTOCOL_ERROR", "input.monitor akış komutudur")
            }
            Request::SystemPower { action } => Self::system_power(action).await,
        }
    }

    /// Restart or shut the appliance down.
    ///
    /// The only place in this daemon that does either. `systemctl` is spelled out
    /// and the verb comes from a two-valued enum, so there is no path from a
    /// request body to an arbitrary command — the request cannot even name one.
    ///
    /// systemd-logind used to do this by itself, on a key event from the HDMI
    /// block's CEC remote-control device. That is switched off on the appliance
    /// now (a udev rule drops the `power-switch` tag, and a logind drop-in ignores
    /// the power and reboot keys), which is what makes this the only route and
    /// makes it worth having.
    async fn system_power(action: PowerAction) -> Response {
        let verb = action.verb();
        eprintln!("mediaboxd.power request verb={verb}");
        match tokio::time::timeout(
            std::time::Duration::from_secs(20),
            tokio::process::Command::new("/usr/bin/systemctl")
                .arg(verb)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => {
                Response::success(json!({"action": action, "accepted": true}))
            }
            Ok(Ok(output)) => Response::failure(
                "POWER_ERROR",
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ),
            Ok(Err(error)) => Response::failure("POWER_ERROR", error.to_string()),
            Err(_) => Response::failure("POWER_ERROR", "systemctl zaman aşımı"),
        }
    }

    /// Hand the display to `target`, and point the input bus at whatever now
    /// owns it.
    ///
    /// These two belong together. While Kodi is on the television there is no
    /// browser to receive a remote press, so CEC has to drive the player
    /// directly; while the UI is up the opposite is true and the daemon must
    /// stay out of the way. Letting them drift apart is how a remote ends up
    /// controlling something nobody can see.
    pub async fn switch_surface(
        &self,
        target: Surface,
    ) -> Result<mediabox_core::SurfaceStatus, crate::lifecycle::LifecycleError> {
        let status = self.surface.switch(target).await?;
        // Taking the panel is choosing to be the board's display owner, so it
        // is recorded. Idle is not that choice: it puts nothing on the screen
        // and leaves the recorded answer alone for whoever comes back to it.
        if target != Surface::Idle {
            remember(crate::owner::DisplayOwner::MediaBox);
        }
        self.input.set_mode(match target {
            Surface::Kodi => InputMode::KodiPlayback,
            Surface::Ui | Surface::Idle => InputMode::Ui,
        });
        Ok(status)
    }

    /// What the board is set to come up as, and what is on it now.
    pub async fn display_owner_status(&self) -> Value {
        let recorded = crate::owner::read();
        let surface = self.surface.status().await;
        json!({
            "owner": recorded.as_str(),
            "file": crate::owner::OWNER_FILE,
            "mediabox_on_display": surface.kodi_active || surface.ui_active,
            "screenbridge_active": unit_active(crate::owner::SCREENBRIDGE_UNIT).await,
        })
    }

    /// Hand the television to one product and remember that it was asked for.
    ///
    /// The order is the point, in both directions: whoever is holding the
    /// display lets go before the other one reaches for it, because DRM master
    /// cannot be held twice and the loser of a race does not degrade, it fails.
    /// Handing it to MediaBox goes through the ordinary surface switch, whose
    /// units already declare the interlock; handing it back stops every
    /// MediaBox surface first and only then starts the other product.
    ///
    /// The preference is written before the move rather than after, so a box
    /// that loses power midway comes back as the thing that was asked for.
    pub async fn set_display_owner(
        &self,
        wanted: crate::owner::DisplayOwner,
    ) -> Result<Value, crate::lifecycle::LifecycleError> {
        crate::owner::write(wanted).map_err(|error| {
            crate::lifecycle::LifecycleError::Failed(format!(
                "{} yazılamadı: {error}",
                crate::owner::OWNER_FILE
            ))
        })?;
        match wanted {
            crate::owner::DisplayOwner::MediaBox => {
                self.switch_surface(Surface::Ui).await?;
            }
            crate::owner::DisplayOwner::ScreenBridge => {
                self.surface.switch(Surface::Idle).await?;
                let output =
                    crate::lifecycle::systemctl(&["start", crate::owner::SCREENBRIDGE_UNIT])
                        .await?;
                if !output.status.success() {
                    return Err(crate::lifecycle::LifecycleError::Failed(
                        String::from_utf8_lossy(&output.stderr).trim().to_string(),
                    ));
                }
            }
        }
        Ok(self.display_owner_status().await)
    }

    /// Create a media session and put it on the television.
    ///
    /// The ordering is the whole point of routing this through the daemon: any
    /// existing session is torn down first so a preview encoder cannot outlive
    /// the screen that asked for it, the display is taken back from the UI, and
    /// only then is Kodi asked to open the URL the media core produced. A
    /// failure anywhere after the session exists stops that session, so a
    /// refused `Player.Open` never leaves an ffmpeg running with no reader.
    /// Open it in the interface's own player.
    ///
    /// No display changes hands: the player is a window of the same
    /// compositor the catalogue is drawn on, so the film appears in front of
    /// the page it was chosen from and Back puts the page back.
    async fn play_here(
        &self,
        url: Option<String>,
        stream: Option<Value>,
        start_seconds: u64,
        title: Option<String>,
        duration_seconds: Option<u64>,
    ) -> Response {
        if url.is_none() && stream.is_none() {
            return Response::failure("INVALID_REQUEST", "url veya stream alanı gerekli");
        }
        // Whatever was playing is over, here or on Kodi.
        if let Some(previous) = self.player.stop().await {
            self.stop_session(&previous.session_id).await;
        }
        self.stop_all_sessions().await;

        let created = match (url.as_deref(), stream) {
            (Some(url), _) => self.media.session_start_at(url, start_seconds).await,
            (None, Some(stream)) => self.media.session_start_stream(stream, start_seconds).await,
            (None, None) => unreachable!("guarded above"),
        };
        let session = match created {
            Ok(value) => value,
            Err(error) => return Response::failure("MEDIA_WORKER_ERROR", error.to_string()),
        };
        let session_id = session
            .get("sessionId")
            .or_else(|| session.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if session_id.is_empty() {
            return Response::failure("MEDIA_WORKER_ERROR", "medya oturumu kimlik üretmedi");
        }
        // What Kodi would be given if the film is handed over later.
        let source = session
            .pointer("/handoff/kodiPlaybackUrl")
            .or_else(|| session.get("playbackUrl"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let local = self.media.session_url(&session_id);
        let playing = Playing {
            session_id: session_id.clone(),
            source: source.clone(),
            title: title.filter(|title| !title.trim().is_empty()),
            duration: duration_seconds.filter(|seconds| *seconds > 0),
        };
        if let Err(error) = self.player.start(&local, start_seconds, playing).await {
            self.stop_session(&session_id).await;
            return Response::failure("PLAYER_FAILED", error);
        }
        Response::success(json!({
            "playing": "here",
            "sessionId": session_id,
            "startSeconds": start_seconds,
        }))
    }

    /// Hand what is playing here to Kodi, at the second it had reached.
    async fn handoff_to_kodi(&self) -> Response {
        let Some(playing) = self.player.playing().await else {
            return Response::failure("NOTHING_PLAYING", "burada oynayan bir şey yok");
        };
        // Asked before the player is stopped, because afterwards there is
        // nobody to ask.
        let at = self.player.position().await.unwrap_or(0);
        self.player.stop().await;
        self.stop_session(&playing.session_id).await;
        if playing.source.is_empty() {
            return Response::failure("NO_SOURCE", "bu kaynağın Kodi için adresi yok");
        }
        self.play_on_kodi(Some(playing.source), None, at).await
    }

    async fn play_on_kodi(
        &self,
        url: Option<String>,
        stream: Option<Value>,
        start_seconds: u64,
    ) -> Response {
        if url.is_none() && stream.is_none() {
            return Response::failure("INVALID_REQUEST", "url veya stream alanı gerekli");
        }
        self.stop_all_sessions().await;

        // Resolving the stream and handing Kodi the display are independent
        // until the moment Kodi is asked to play, so they are done at the same
        // time rather than one after the other. Measured on the appliance: the
        // resolve is about six seconds of network — a debrid link and a probe —
        // and the handover about two and a half of stopping the interface and
        // starting Kodi. Run in sequence that is the whole eleven seconds
        // between pressing Play and seeing a picture; overlapped, the handover
        // is free.
        let resolve = async {
            match (url.as_deref(), stream) {
                (Some(url), _) => self.media.session_start_at(url, start_seconds).await,
                (None, Some(stream)) => {
                    self.media.session_start_stream(stream, start_seconds).await
                }
                (None, None) => unreachable!("guarded above"),
            }
        };
        let (created, handover) = tokio::join!(resolve, self.switch_surface(Surface::Kodi));
        let session = match created {
            Ok(value) => value,
            Err(error) => {
                // The display is already Kodi's by now, and Kodi with nothing
                // to play is not an answer: give the interface back so the
                // failure is read where it can be acted on.
                let _ = self.switch_surface(Surface::Ui).await;
                return Response::failure("MEDIA_WORKER_ERROR", error.to_string());
            }
        };
        let session_id = session
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(playback_url) = session
            .pointer("/handoff/kodiPlaybackUrl")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            self.stop_session(&session_id).await;
            return Response::failure(
                "MEDIA_WORKER_ERROR",
                "medya oturumu Kodi için oynatma URL'si üretmedi",
            );
        };

        let surface = match handover {
            Ok(status) => status,
            Err(error) => {
                self.stop_session(&session_id).await;
                return Response::failure("SURFACE_ERROR", error.to_string());
            }
        };
        if let Err(error) = self.await_kodi().await {
            self.stop_session(&session_id).await;
            return Response::failure("KODI_UNAVAILABLE", error);
        }
        match self.kodi.open(&playback_url, start_seconds).await {
            Ok(_) => Response::success(json!({
                "session": session,
                "surface": surface,
                "playbackUrl": playback_url,
                "startSeconds": start_seconds,
            })),
            Err(error) => {
                self.stop_session(&session_id).await;
                Response::failure("KODI_ERROR", error.to_string())
            }
        }
    }

    async fn stop_all_sessions(&self) {
        let Ok(listing) = self.media.sessions().await else {
            return;
        };
        let Some(sessions) = listing.get("sessions").and_then(Value::as_array) else {
            return;
        };
        for id in sessions
            .iter()
            .filter_map(|session| session.get("id").and_then(Value::as_str))
        {
            let _ = self.media.session_stop(id).await;
        }
    }

    async fn stop_session(&self, id: &str) {
        if !id.is_empty() {
            let _ = self.media.session_stop(id).await;
        }
    }

    /// Kodi has just been started by systemd; its JSON-RPC listener comes up a
    /// few seconds after the process does.
    async fn await_kodi(&self) -> Result<(), String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut last = "Kodi JSON-RPC yanıt vermedi".to_string();
        while std::time::Instant::now() < deadline {
            match self.kodi.ping().await {
                Ok(true) => return Ok(()),
                Ok(false) => last = "Kodi JSON-RPC beklenmeyen yanıt verdi".into(),
                Err(error) => last = error.to_string(),
            }
            tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        }
        Err(last)
    }

    async fn status(&self) -> SystemStatus {
        let kodi = self.kodi.status().await;
        let cec = self.cec.status();
        let system = system_snapshot();
        let media = match self.media.status().await {
            Ok(value) => value,
            Err(error) => {
                json!({"available":false,"error":{"code":"MEDIA_WORKER_UNAVAILABLE","message":error.to_string()}})
            }
        };
        let input_devices = mediabox_input::enumerate()
            .into_iter()
            .filter_map(|item| serde_json::to_value(item).ok())
            .collect();
        let services = vec![
            ServiceHealth {
                name: "mediaboxd-rs".into(),
                healthy: true,
                detail: None,
            },
            ServiceHealth {
                name: "kodi".into(),
                healthy: kodi.jsonrpc_reachable,
                detail: kodi.error.clone(),
            },
            ServiceHealth {
                name: "cec".into(),
                healthy: cec.available,
                detail: cec.error.clone(),
            },
            ServiceHealth {
                name: "media-worker".into(),
                healthy: media.get("available").and_then(Value::as_bool) != Some(false),
                detail: media
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
        ];
        SystemStatus {
            version: env!("CARGO_PKG_VERSION").into(),
            hostname: string_field(&system, "hostname"),
            kernel: string_field(&system, "kernel"),
            architecture: std::env::consts::ARCH.into(),
            uptime_seconds: system
                .get("uptime_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            input_mode: self.input.mode(),
            input_devices,
            services,
            kodi,
            cec,
            media,
            surface: self.surface.status().await,
            leds: self.leds.status(),
        }
    }
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn cec_error(status: &CecStatus) -> String {
    status
        .error
        .clone()
        .unwrap_or_else(|| "CEC kullanılamıyor".into())
}

async fn cec_action<F>(runtime: &CecRuntime, action: F) -> Response
where
    F: FnOnce(&Adapter) -> Result<(), mediabox_cec::CecError> + Send + 'static,
{
    let Some(adapter) = runtime.adapter.clone() else {
        return Response::failure("CEC_UNAVAILABLE", cec_error(&runtime.status()));
    };
    match tokio::task::spawn_blocking(move || action(&adapter)).await {
        Ok(Ok(())) => Response::success(json!({"transmitted":true})),
        Ok(Err(error)) => Response::failure("CEC_ERROR", error.to_string()),
        Err(error) => Response::failure("INTERNAL_ERROR", error.to_string()),
    }
}

fn result(value: Result<Value, crate::kodi::KodiError>) -> Response {
    match value {
        Ok(value) => Response::success(value),
        Err(error) => Response::failure("KODI_ERROR", error.to_string()),
    }
}

fn media_result(value: Result<Value, crate::media::MediaError>) -> Response {
    match value {
        Ok(value) => Response::success(value),
        Err(error) => Response::failure("MEDIA_WORKER_ERROR", error.to_string()),
    }
}

pub async fn apply_kodi_route(kodi: &KodiClient, decision: RouteDecision) -> Result<(), String> {
    let RouteDecision::Kodi(route) = decision else {
        return Ok(());
    };
    let response = match route {
        KodiRoute::PlayPause => kodi.play_pause().await,
        KodiRoute::Play => kodi.set_playing(true).await,
        KodiRoute::Pause => kodi.set_playing(false).await,
        KodiRoute::Stop => kodi.stop().await,
        KodiRoute::Seek(seconds) => kodi.seek_relative(seconds).await,
        // Kodi Application.SetVolume supports increment/decrement and mute is a separate call.
        KodiRoute::VolumeUp => {
            kodi.call("Application.SetVolume", Some(json!({"volume":"increment"})))
                .await
        }
        KodiRoute::VolumeDown => {
            kodi.call("Application.SetVolume", Some(json!({"volume":"decrement"})))
                .await
        }
        KodiRoute::Mute => {
            kodi.call("Application.SetMute", Some(json!({"mute":"toggle"})))
                .await
        }
    };
    response.map(|_| ()).map_err(|error| error.to_string())
}

pub async fn serve_unix(listener: UnixListener, state: Arc<AppState>) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let _ = handle_unix(stream, state).await;
        });
    }
}

async fn handle_unix(stream: UnixStream, state: Arc<AppState>) -> io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut line = String::new();
    let count = read.read_line(&mut line).await?;
    if count == 0 {
        return Ok(());
    }
    if count > MAX_REQUEST_BYTES || !line.ends_with('\n') {
        return write_response(
            &mut write,
            &Response::failure("INVALID_REQUEST", "istek çok büyük veya satır sonu yok"),
        )
        .await;
    }
    let request: Request = match serde_json::from_str(&line) {
        Ok(value) => value,
        Err(error) => {
            return write_response(
                &mut write,
                &Response::failure("INVALID_REQUEST", error.to_string()),
            )
            .await;
        }
    };
    if request == Request::InputMonitor {
        let mut events = state.input.subscribe();
        write_response(&mut write, &Response::success(json!({"monitoring":true}))).await?;
        loop {
            match events.recv().await {
                Ok(event) => {
                    write
                        .write_all(
                            format!("{}\n", serde_json::to_string(&event).expect("event JSON"))
                                .as_bytes(),
                        )
                        .await?
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    write
                        .write_all(
                            format!("{}\n", json!({"warning":"lagged","events":count})).as_bytes(),
                        )
                        .await?
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
    write_response(&mut write, &state.handle(request).await).await
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    response: &Response,
) -> io::Result<()> {
    write
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(response).expect("response JSON")
            )
            .as_bytes(),
        )
        .await
}

pub fn system_snapshot() -> Value {
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_else(|_| "unknown".into())
        .trim()
        .to_string();
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_else(|_| "unknown".into())
        .trim()
        .to_string();
    let uptime_seconds = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    json!({"hostname":hostname,"kernel":kernel,"architecture":std::env::consts::ARCH,"uptime_seconds":uptime_seconds})
}

pub fn socket_is_live(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{ApplicationManager, KodiLifecycle, SurfaceManager};
    use mediabox_core::{CecStatus, InputMode};
    use std::time::Duration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn unix_socket_request_returns_structured_response() {
        let dir = tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let state = Arc::new(AppState {
            kodi: Arc::new(
                KodiClient::new("http://127.0.0.1:9/jsonrpc", Duration::from_millis(20)).unwrap(),
            ),
            lifecycle: KodiLifecycle::new("kodi.service").unwrap(),
            cec: CecRuntime {
                adapter: None,
                unavailable: CecStatus {
                    error: Some("testte kapalı".into()),
                    ..Default::default()
                },
            },
            input: InputManager::new(InputMode::Ui),
            media: Arc::new(
                MediaClient::new("http://127.0.0.1:9", Duration::from_millis(20)).unwrap(),
            ),
            surface: SurfaceManager::new("kodi.service", "mediabox-tv-ui.service").unwrap(),
            // Never started in the tests; it exists so the state is whole.
            player: Arc::new(PlayerManager::new("/bin/true", "/run/mediabox/player.sock")),
            // Pointed at the empty temporary directory, so the test never
            // reaches the machine's own sysfs and reports no lights.
            leds: LedController::new(dir.path(), dir.path().join("leds")),
            applications: ApplicationManager::load(None, "kodi.service", "mediabox-tv-ui.service")
                .unwrap(),
        });
        let task = tokio::spawn(serve_unix(listener, state));
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream
            .write_all(b"{\"command\":\"system\"}\n")
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        let response: Response = serde_json::from_str(&line).unwrap();
        assert!(response.ok);
        task.abort();
    }

    #[tokio::test]
    async fn arbitrary_command_is_rejected_at_wire_boundary() {
        let parsed =
            serde_json::from_str::<Request>(r#"{"command":"exec","command_line":"reboot"}"#);
        assert!(parsed.is_err());
    }
}

/// Record the display owner, and say so if it cannot be recorded.
///
/// Never fatal. Failing to write a preference must not fail the switch a
/// person just asked for; it means the next boot falls back to the safe
/// default, which is a worse answer than the right one but not a broken box.
fn remember(owner: crate::owner::DisplayOwner) {
    if let Err(error) = crate::owner::write(owner) {
        eprintln!(
            "mediaboxd-rs: {} yazılamadı ({error}); açılış tercihi değişmedi",
            crate::owner::OWNER_FILE
        );
    }
}

/// Whether a unit this product does not own is running.
async fn unit_active(unit: &str) -> bool {
    crate::lifecycle::systemctl(&["is-active", "--quiet", unit])
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}
