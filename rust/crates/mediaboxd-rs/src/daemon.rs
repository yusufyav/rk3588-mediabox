use crate::output::Output;
use crate::fan::FanController;
use crate::kodi::KodiClient;
use crate::leds::LedController;
use crate::lifecycle::{ApplicationManager, KodiLifecycle, SurfaceManager};
use crate::media::MediaClient;
use crate::player::{PlayerManager, Playing};
use crate::cec::CecTarget;
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
    /// Every adapter on the board, each configured as a playback device.
    /// All of them are listened to; a command is sent down one only.
    pub adapters: Vec<Arc<Adapter>>,
    /// What to report when there is no adapter at all.
    pub unavailable: CecStatus,
    /// Where a command goes: the selected output's transmitter's adapter,
    /// resolved when the command is sent (`crate::cec`).
    pub target: Arc<dyn Fn() -> CecTarget + Send + Sync>,
    /// The HDMI-CEC panel and what this boot has done (`crate::cec::Policy`).
    pub policy: Arc<crate::cec::Policy>,
    /// What this daemon was asked to do to the machine, for a stop whose
    /// reason systemd's queue does not say.
    pub requested: std::sync::Mutex<Option<PowerAction>>,
}

/// Said by every refusal that is CEC being off, on every interface.
pub const CEC_DISABLED: &str = "HDMI-CEC ayarlardan kapatıldı";

impl CecRuntime {
    /// The adapter a command goes down, or why none may: the selected
    /// output's, never merely the first one that is live.
    pub fn adapter(&self) -> Result<Arc<Adapter>, String> {
        let chosen = self.adapter_quiet();
        match (&chosen, (self.target)()) {
            (Ok(adapter), CecTarget::Adapter { confidence, .. }) => eprintln!(
                "mediaboxd-rs: CEC target {} ({confidence:?}, selected output)",
                adapter.path().display()
            ),
            (Err(why), _) => eprintln!("mediaboxd-rs: CEC not sent: {why}"),
            _ => {}
        }
        chosen
    }

    /// The same answer, without a line in the journal: for the policy's own
    /// sends, which ask on every message from the television.
    pub fn adapter_quiet(&self) -> Result<Arc<Adapter>, String> {
        crate::cec::choose(&self.adapters, &(self.target)())
    }

    /// Every adapter told what the kept settings make it: released with CEC
    /// off, a playback device with or without the remote's passthrough
    /// otherwise. Blocking: a claim waits for the bus.
    pub fn apply_claim(&self) -> Vec<String> {
        let claim = self.policy.claim();
        self.adapters
            .iter()
            .filter_map(|adapter| match adapter.claim(claim) {
                Ok(_) => None,
                Err(error) => Some(format!("{}: {error}", adapter.path().display())),
            })
            .collect()
    }

    pub fn status(&self) -> CecStatus {
        let mut status = self.adapter_status();
        status.settings = self.policy.settings();
        status.session = self.policy.session();
        if !status.settings.enabled {
            status.available = false;
            status.error = Some(CEC_DISABLED.into());
        }
        status
    }

    fn adapter_status(&self) -> CecStatus {
        if self.adapters.is_empty() {
            return self.unavailable.clone();
        }
        let target = (self.target)();
        match crate::cec::choose(&self.adapters, &target) {
            Ok(adapter) => adapter.status(),
            Err(why) => {
                let selected = match &target {
                    CecTarget::Adapter { path, .. } => self
                        .adapters
                        .iter()
                        .find(|adapter| adapter.path() == path.as_path())
                        .map(|adapter| adapter.status()),
                    CecTarget::Refused(_) => None,
                };
                CecStatus {
                    available: false,
                    error: Some(why),
                    ..selected.unwrap_or_default()
                }
            }
        }
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
    /// What subtitles that film has, which one is on, and its timing.
    pub subtitles: Arc<crate::subtitles::Subtitles>,
    /// The board's two indicator lights. Here rather than in the interface
    /// because `/sys/class/leds` is root's, and the unit that draws the
    /// television mounts /sys read-only.
    pub leds: LedController,
    /// The display setting: the kept one, bound to the display it was made
    /// on, and a new one on trial. The observer says what the display is and
    /// offers; the owner says what it committed; the daemon keeps the choice
    /// and the clock.
    pub output: Arc<Output>,
    /// The fan's curve for the next boot. The kernel drives the fan; this
    /// only writes the overlay it reads at boot, which is /boot's and root's.
    pub fan: FanController,
    /// The wired ports and an address written by hand on trial. Here for the
    /// radio's reason: /etc/netplan and networkd are root's.
    pub ethernet: Arc<crate::ethernet::Ethernet>,
    /// Sound: the kept setting, checked against the devices found now, and
    /// put into effect on the playing film and the softvol control.
    pub audio: Arc<crate::audio::Audio>,
    /// How each film ends, here or in Kodi: the one authority on it, and the
    /// one writer of where it got to on the account (`crate::playback`).
    pub playback: Arc<crate::playback::Supervisor>,
    /// Where Kodi sends its JSON-RPC notifications (TCP, loopback).
    pub kodi_events: std::net::SocketAddr,
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
            Request::CecSettingsSet { settings } => self.cec_settings_set(settings).await,
            Request::CecDevices => {
                if !self.cec.policy.enabled() {
                    return Response::failure("CEC_DISABLED", CEC_DISABLED);
                }
                let adapter = match self.cec.adapter() {
                    Ok(adapter) => adapter,
                    Err(why) => return Response::failure("CEC_UNAVAILABLE", why),
                };
                match tokio::task::spawn_blocking(move || adapter.discover_devices()).await {
                    Ok(Ok(devices)) => Response::success(devices),
                    Ok(Err(error)) => Response::failure("CEC_ERROR", error.to_string()),
                    Err(error) => Response::failure("INTERNAL_ERROR", error.to_string()),
                }
            }
            Request::CecActiveSource => {
                let answer = cec_action(&self.cec, |adapter| adapter.active_source()).await;
                if answer.ok {
                    self.cec.policy.session_update(|session| {
                        session.active_source = mediabox_core::ActiveSource::Us
                    });
                }
                answer
            }
            Request::CecWakeTv => cec_action(&self.cec, |adapter| adapter.wake_tv()).await,
            Request::CecStandbyTv => cec_action(&self.cec, |adapter| adapter.standby_tv()).await,
            Request::LedsStatus => Response::success(self.leds.status()),
            Request::LedsSet { mode } => match self.leds.set(mode) {
                Ok(status) => Response::success(status),
                Err(error) => Response::failure("LED_ERROR", error),
            },
            Request::OutputStatus => Response::success(self.output.status()),
            Request::OutputTry { resolution, colour } => {
                let holds = self.surface.status().await.active == mediabox_core::Surface::Ui;
                match self.output.try_setting(resolution, colour, holds) {
                    Ok(status) => Response::success(status),
                    Err(error) => Response::failure("OUTPUT_REFUSED", error),
                }
            }
            Request::OutputKeep { trial } => match self.output.keep(trial) {
                Ok(status) => Response::success(status),
                Err(error) => Response::failure("OUTPUT_REFUSED", error),
            },
            Request::OutputRevert { trial } => match self.output.revert(trial) {
                Ok(status) => Response::success(status),
                Err(error) => Response::failure("OUTPUT_REFUSED", error),
            },
            Request::OutputContentMatching { enabled } => match self.output.set_matching(enabled) {
                Ok(status) => Response::success(status),
                Err(error) => Response::failure("OUTPUT_REFUSED", error),
            },
            Request::EthernetStatus => Response::success(self.ethernet.status()),
            Request::EthernetTry { interface, config } => {
                ethernet_result(self.ethernet.try_config(&interface, config).await)
            }
            Request::EthernetKeep => ethernet_result(self.ethernet.keep().await),
            Request::EthernetRevert => ethernet_result(self.ethernet.revert().await),
            Request::AudioStatus => Response::success(self.audio_status().await),
            Request::AudioSet { setting } => {
                let audio = Arc::clone(&self.audio);
                match tokio::task::spawn_blocking(move || audio.set(setting)).await {
                    Ok(Ok((before, after))) => {
                        self.player.apply_audio(&before, &after).await;
                        Response::success(self.audio_status().await)
                    }
                    Ok(Err(why)) => Response::failure("AUDIO_REFUSED", why),
                    Err(error) => Response::failure("INTERNAL_ERROR", error.to_string()),
                }
            }
            Request::AudioVolume { volume, step, muted, toggle_mute } => {
                use crate::audio::VolumeChange;
                let change = match (volume, step, muted, toggle_mute) {
                    (Some(volume), None, None, false) => VolumeChange::Set(volume),
                    (None, Some(step), None, false) => VolumeChange::Step(step),
                    (None, None, Some(muted), false) => VolumeChange::Mute(muted),
                    (None, None, None, true) => VolumeChange::ToggleMute,
                    _ => {
                        return Response::failure(
                            "INVALID_REQUEST",
                            "volume, step, muted ya da toggle_mute: yalnızca biri",
                        );
                    }
                };
                let audio = Arc::clone(&self.audio);
                let before = self.audio.now().2;
                match tokio::task::spawn_blocking(move || audio.volume(change)).await {
                    Ok(Ok(after)) => {
                        self.player.apply_audio(&before, &after).await;
                        Response::success(self.audio_status().await)
                    }
                    Ok(Err(why)) => Response::failure("AUDIO_REFUSED", why),
                    Err(error) => Response::failure("INTERNAL_ERROR", error.to_string()),
                }
            }
            Request::FanStatus => Response::success(self.fan.status()),
            Request::FanCurveSet { profile, points } => {
                match mediabox_core::FanCurve::resolve(profile, points) {
                    Ok(curve) => fan_result(self.fan.set(curve)),
                    Err(error) => Response::failure("FAN_CURVE_INVALID", error.to_string()),
                }
            }
            Request::FanCurveReset => fan_result(self.fan.reset()),
            Request::MediaStatus => media_result(self.media.status().await),
            Request::MediaCapabilities => media_result(self.media.capabilities().await),
            Request::MediaHome => media_result(self.media.home().await),
            Request::MediaCatalog {
                media_type,
                id,
                addon_id,
                limit,
                extra,
            } => media_result(
                self.media
                    .catalog(&media_type, &id, addon_id.as_deref(), limit, &extra)
                    .await,
            ),
            Request::MediaDiscover => media_result(self.media.discover().await),
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
                watch,
            } => self.play_on_kodi(url, stream, start_seconds, watch).await,
            Request::MediaPlayHere {
                url,
                stream,
                start_seconds,
                title,
                duration_seconds,
                watch,
            } => {
                self.play_here(url, stream, start_seconds, title, duration_seconds, watch)
                    .await
            }
            Request::MediaHandoffToKodi => self.handoff_to_kodi().await,
            Request::MediaStatusHere => {
                // The player's own answer, and the control plane's on which
                // film this is and how the last one ended.
                let mut status = self.player.status().await;
                let playback = self.playback.status();
                status["film"] = playback["film"].clone();
                // Which title it is: the interface puts its page under a film
                // it did not start, for Back to land on.
                status["watch"] = playback["watch"].clone();
                status["finished"] = playback["finished"].clone();
                // Carried and fetched subtitles in one list, each saying
                // where it came from and how it is timed.
                let tracks = status["tracks"].as_array().cloned().unwrap_or_default();
                status["subtitles"] = self.subtitles.unified(&tracks);
                // The settings panel's "Tercih edilen altyazı dili".
                let preferences = self.subtitles.preferences();
                status["subtitle_preference"] = json!(match preferences.enabled {
                    Some(true) => preferences.language.clone(),
                    _ => None,
                });
                // ... and its "Otomatik eşitleme".
                status["subtitle_auto_sync"] = json!(preferences.auto_sync());
                // ... and its "AutoSync uyumsuz altyazıları göster".
                status["subtitle_show_incompatible"] = json!(preferences.show_incompatible());
                Response::success(status)
            }
            Request::SubtitlePreferenceSet { language } => {
                let applied = self.subtitles.set_preference(language).await;
                Response::success(json!({"applied": applied}))
            }
            Request::SubtitleAutoSyncSet { enabled } => {
                let applied = self.subtitles.set_auto_sync(enabled).await;
                Response::success(json!({"applied": applied, "enabled": enabled}))
            }
            Request::SubtitleShowIncompatibleSet { enabled } => {
                self.subtitles.set_show_incompatible(enabled).await;
                Response::success(json!({"applied": true, "enabled": enabled}))
            }
            Request::MediaSubtitleTextHere => Response::success(self.subtitles.text().await),
            Request::MediaTransportHere { action } => {
                use mediabox_core::TransportAction;
                let moved = match action {
                    TransportAction::SubtitleChoose { ref id } => self.subtitles.choose(id).await,
                    TransportAction::SubtitleAutoSync => self.subtitles.auto_sync().await,
                    TransportAction::Subtitle { id } => {
                        let moved = self.player.transport(action).await;
                        self.subtitles.chose_track(id).await;
                        moved
                    }
                    TransportAction::SubtitleDelay { .. } => {
                        self.subtitles.delay_moved();
                        self.player.transport(action).await
                    }
                    _ => self.player.transport(action).await,
                };
                Response::success(json!({"moved": moved}))
            }
            Request::MediaStopHere => {
                let stopped = self.stop_here(crate::playback::Intent::Stop).await;
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
            Request::MediaSuggest { query } => media_result(self.media.suggest(&query).await),
            Request::MediaInspect { url } => media_result(self.media.inspect(&url).await),
            Request::MediaStreams {
                media_type,
                id,
                video_id,
            } => media_result(
                self.media
                    .streams(&media_type, &id, video_id.as_deref())
                    .await,
            ),
            Request::MediaWatchState { media_type, id } => {
                media_result(self.media.watch_state(&media_type, &id).await)
            }
            Request::MediaAccount { action, change } => {
                media_result(self.media.account(action.path(), change).await)
            }
            Request::MediaPolicy { url } => media_result(self.media.policy(&url).await),
            Request::MediaSessions => media_result(self.media.sessions().await),
            Request::MediaSessionStart { url } => {
                media_result(self.media.session_start(&url).await)
            }
            Request::MediaSessionStop { id } => media_result(self.media.session_stop(&id).await),
            Request::InputInject { action, pressed, source } => {
                let source = source.unwrap_or(InputSource::Api);
                let decision = self.input.publish(action, source, pressed, None);
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
            Request::SystemPower { action } => {
                *self.cec.requested.lock().expect("power request") = Some(action);
                Self::system_power(action).await
            }

            // The radios. Every one of these shells out as root — rfkill,
            // netplan, bluetoothctl — which is exactly why they are here and
            // not in the interface.
            Request::WifiStatus => Response::success(crate::wireless::wifi_status().await),
            Request::WifiScan => radio(crate::wireless::wifi_scan().await),
            Request::WifiConnect { ssid, psk } => {
                radio(crate::wireless::wifi_connect(&ssid, psk.as_deref()).await)
            }
            Request::WifiDisconnect => radio(crate::wireless::wifi_disconnect().await),
            Request::WifiForget { ssid } => radio(crate::wireless::wifi_forget(&ssid).await),
            Request::WifiPower { on } => radio(crate::wireless::wifi_power(on).await),

            Request::BluetoothStatus => {
                Response::success(crate::wireless::bluetooth_status().await)
            }
            Request::BluetoothScan => radio(crate::wireless::bluetooth_scan(12).await),
            Request::BluetoothPower { on } => radio(crate::wireless::bluetooth_power(on).await),
            Request::BluetoothPair { address } => {
                radio(crate::wireless::bluetooth_pair(&address).await)
            }
            Request::BluetoothConnect { address } => {
                radio(crate::wireless::bluetooth_connect(&address).await)
            }
            Request::BluetoothDisconnect { address } => {
                radio(crate::wireless::bluetooth_disconnect(&address).await)
            }
            Request::BluetoothRemove { address } => {
                radio(crate::wireless::bluetooth_remove(&address).await)
            }
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

    /// Keep the HDMI-CEC panel and put it into effect now.
    ///
    /// The master switch and the remote are the adapters' claim, so a change
    /// to either releases or re-claims every adapter -- the change takes
    /// effect on the bus, not at the next boot. Nothing is sent to the
    /// television for it: AOSP's One Touch Play is a boot's, not a setting's.
    async fn cec_settings_set(&self, settings: mediabox_core::CecSettings) -> Response {
        let before = match self.cec.policy.set(settings) {
            Ok(before) => before,
            Err(crate::cec::SetError::Invalid(why)) => {
                return Response::failure("CEC_SETTINGS_INVALID", why);
            }
            Err(crate::cec::SetError::Write(why)) => {
                return Response::failure("CEC_SETTINGS_WRITE", why);
            }
        };
        let reclaim = before.enabled != settings.enabled
            || (settings.enabled && before.remote_control != settings.remote_control);
        if reclaim && !self.cec.adapters.is_empty() {
            let adapters = self.cec.adapters.clone();
            let policy = Arc::clone(&self.cec.policy);
            let claim = policy.claim();
            let failures = tokio::task::spawn_blocking(move || {
                adapters
                    .iter()
                    .filter_map(|adapter| match adapter.claim(claim) {
                        Ok(_) => None,
                        Err(error) => Some(format!("{}: {error}", adapter.path().display())),
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_else(|error| vec![error.to_string()]);
            eprintln!(
                "mediaboxd-rs: CEC {} (remote {}){}",
                if settings.enabled { "on" } else { "off" },
                if settings.remote_control { "on" } else { "off" },
                if failures.is_empty() { String::new() } else { format!(": {}", failures.join("; ")) }
            );
            if !settings.enabled {
                self.cec.policy.session_update(|session| {
                    session.active_source = mediabox_core::ActiveSource::Unknown
                });
            }
        }
        Response::success(self.cec.status())
    }

    /// One Touch Play at boot, beside the daemon rather than in front of it.
    ///
    /// Runs once a boot: a daemon restarted by a deploy finds this boot's
    /// session already done and sends nothing. A box that came up with the
    /// television's hotplug low waits a bounded time for an address and then
    /// gives up, saying why.
    pub fn cec_start(self: &Arc<Self>) {
        let plan = mediabox_cec::policy::start(&self.cec.policy.settings(), &self.cec.policy.session());
        if !plan.anything() {
            self.cec.policy.session_update(|session| session.start_done = true);
            return;
        }
        let state = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("mediabox-cec-start".into()).spawn(move || {
            let cec = &state.cec;
            let report = crate::cec::run_start(
                plan,
                |round| {
                    let chosen = cec.adapter_quiet();
                    // An address the claim did not get is asked for again,
                    // twice in the whole wait.
                    if chosen.is_err() && (round == 5 || round == 15) {
                        for adapter in &cec.adapters {
                            let _ = adapter.recover(cec.policy.claim());
                        }
                    }
                    chosen
                },
                || cec.policy.enabled(),
                || std::thread::sleep(std::time::Duration::from_secs(1)),
                || std::thread::sleep(std::time::Duration::from_millis(250)),
            );
            match report {
                Ok(report) => {
                    eprintln!(
                        "mediaboxd-rs: CEC start tv={:?} woke={} active_source={}{}",
                        report.tv_power,
                        report.woke,
                        report.announced,
                        if report.notes.is_empty() { String::new() } else { format!(" ({})", report.notes.join("; ")) }
                    );
                    cec.policy.session_update(|session| {
                        session.tv_power_at_start = report.tv_power;
                        session.tv_power = report.tv_power;
                        session.woke_tv = report.woke;
                        if report.announced {
                            session.active_source = mediabox_core::ActiveSource::Us;
                        }
                        session.start_done = true;
                    });
                }
                Err(why) => {
                    eprintln!("mediaboxd-rs: CEC start skipped: {why}");
                    cec.policy.session_update(|session| session.start_done = true);
                }
            }
        });
        if let Err(error) = spawned {
            eprintln!("mediaboxd-rs: CEC start thread: {error}");
        }
    }

    /// At the daemon's stop, before the adapters are released: `<Standby>`
    /// if the box is powering off and the settings ask for it. Bounded to
    /// three seconds; a reboot and a restart of the daemon send nothing.
    pub async fn cec_shutdown(&self) {
        let jobs = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::process::Command::new("/usr/bin/systemctl")
                .args(["list-jobs", "--no-legend", "--plain"])
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            _ => None,
        };
        let requested = *self.cec.requested.lock().expect("power request");
        let stop = crate::cec::stop_reason(jobs.as_deref(), requested);
        let plan = mediabox_cec::policy::shutdown(
            &self.cec.policy.settings(),
            &self.cec.policy.session(),
            stop,
        );
        eprintln!(
            "mediaboxd-rs: CEC stop {stop:?} standby -> {:?} inactive_source={}",
            plan.standby, plan.inactive_source
        );
        if plan.standby.is_empty() && !plan.inactive_source {
            return;
        }
        let adapter = match self.cec.adapter() {
            Ok(adapter) => adapter,
            Err(_) => return,
        };
        let sent = tokio::task::spawn_blocking(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            crate::cec::run_shutdown(
                adapter.as_ref(),
                &plan,
                || std::time::Instant::now() < deadline,
                || std::thread::sleep(std::time::Duration::from_millis(200)),
            )
        })
        .await
        .unwrap_or_default();
        for (message, result) in sent {
            match result {
                Ok(()) => eprintln!("mediaboxd-rs: CEC {message}: sent"),
                Err(why) => eprintln!("mediaboxd-rs: CEC {message}: {why}"),
            }
        }
    }

    /// A message the selected adapter received, through the policy: the
    /// session follows what the bus said, and an answer is sent when one is
    /// owed. Called from the receive thread; sends go to a blocking task.
    pub fn cec_received(self: &Arc<Self>, adapter: &Arc<Adapter>, event: &mediabox_core::CecEvent) {
        if !mediabox_cec::policy::relevant(event.opcode) {
            return;
        }
        // Only the adapter the picture is on: another television on the
        // other socket is not this box's to answer.
        let Ok(selected) = self.cec.adapter_quiet() else { return };
        if !Arc::ptr_eq(&selected, adapter) {
            return;
        }
        let (physical, Some(logical)) = adapter.addresses() else { return };
        let settings = self.cec.policy.settings();
        let reactions = self.cec.policy.session_update(|session| {
            mediabox_cec::policy::receive(&settings, session, logical, physical, event)
        });
        for reaction in reactions {
            let state = Arc::clone(self);
            let adapter = Arc::clone(adapter);
            tokio::spawn(async move {
                use mediabox_cec::policy::Reaction;
                match reaction {
                    Reaction::AnnounceActiveSource => {
                        let _ = tokio::task::spawn_blocking(move || adapter.active_source()).await;
                    }
                    Reaction::ReportPowerOn(to) => {
                        let _ = tokio::task::spawn_blocking(move || adapter.report_power_on(to)).await;
                    }
                    Reaction::SourceLost => state.cec_source_lost().await,
                }
            });
        }
    }

    /// "Aktif HDMI kaynağı kaybedildiğinde: Beklemeye geç".
    ///
    /// This box has no sleep state of its own, so standby is what the
    /// lifecycle already does safely: the film here stops with its place
    /// written to the account, Kodi stops what it plays and gives the panel
    /// back, and the home screen is what is waiting when the television
    /// comes back to this input. Nothing is powered off.
    async fn cec_source_lost(&self) {
        eprintln!("mediaboxd-rs: CEC active source lost; standing by");
        if self.player.playing().await.is_some() {
            let _ = self.stop_here(crate::playback::Intent::Stop).await;
        }
        if self.surface.status().await.kodi_active {
            let _ = self.kodi.stop().await;
            if let Err(error) = self.switch_surface(Surface::Ui).await {
                eprintln!("mediaboxd-rs: CEC standby -> ui: {error}");
            }
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
        let from_kodi = target == Surface::Ui && self.surface.status().await.kodi_active;
        let status = self.surface.switch(target).await?;
        // Back from Kodi, nothing it was playing is left running: a session
        // the media core opened for it (a remux, an audio transcode) would
        // otherwise go on until its idle timeout, and a player of ours that
        // somehow outlived the handover would hold the video plane.
        if from_kodi {
            self.stop_all_sessions().await;
            if self.player.playing().await.is_some() {
                self.player.stop().await;
            }
        }
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
        watch: Option<mediabox_core::WatchRef>,
    ) -> Response {
        if url.is_none() && stream.is_none() {
            return Response::failure("INVALID_REQUEST", "url veya stream alanı gerekli");
        }
        // Whatever was playing is over, here or on Kodi.
        self.stop_here(crate::playback::Intent::Replace).await;
        self.stop_all_sessions().await;

        let created = self
            .media
            .session_start_here(url.as_deref(), stream, start_seconds)
            .await;
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

        // And where it comes from, which is not always the same thing: when
        // the film needs a transform, the address above is this core's own
        // proxy and only the upstream one can be given to a second player.
        let origin = session
            .pointer("/handoff/resolvedInput")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| url.clone())
            .unwrap_or_default();

        let local = self.media.session_url(&session_id);
        let subtitle_watch = watch.clone();
        let playing = Playing {
            session_id: session_id.clone(),
            source: source.clone(),
            origin,
            title: title.filter(|title| !title.trim().is_empty()),
            duration: duration_seconds.filter(|seconds| *seconds > 0),
        };
        if let Err(error) = self.player.start(&local, start_seconds, playing).await {
            self.stop_session(&session_id).await;
            return Response::failure("PLAYER_FAILED", error);
        }
        let film = self.playback.begin(crate::playback::Start {
            player: crate::playback::PlayerKind::Here,
            watch,
            session: Some(session_id.clone()),
            duration: duration_seconds,
            position: start_seconds,
        });
        tokio::spawn(crate::playback::follow_mpv(
            Arc::clone(&self.playback),
            Arc::clone(&self.player),
            film,
            self.player.film(),
        ));
        tokio::spawn(follow_film(
            Arc::clone(&self.player),
            Arc::clone(&self.output),
            self.player.film(),
        ));
        self.subtitles
            .begin(self.player.film(), session_id.clone(), subtitle_watch, duration_seconds);
        Response::success(json!({
            "playing": "here",
            "film": film,
            "sessionId": session_id,
            "startSeconds": start_seconds,
        }))
    }

    /// Hand what is playing here to Kodi, at the second it had reached.
    ///
    /// Hand the film over by giving Kodi the source, and nothing else.
    ///
    /// Kodi is a player. It resolves, probes, seeks and decodes; the one thing
    /// it will not do is read a pipe somebody else is holding. Both of the
    /// ways this went wrong were ways of forgetting that.
    ///
    /// It used to end the session and ask the media core for a new one with
    /// `playing.source` as the source -- and that address is the core's own
    /// loopback, which it refuses by design:
    ///
    ///   POST /media/session -> 400
    ///   INVALID_REQUEST: loopback sources are only allowed for the
    ///   configured streaming server
    ///
    /// Because resolving and the handover ran at the same time, Kodi had
    /// already been started when the refusal came back: it lived 73 ms, the
    /// interface came back and the film was gone with no reason shown -- the
    /// notice belongs to the interface, which the handover restarts.
    ///
    /// Handing that same address to Kodi to *open* is no better, and it is
    /// the failure that looked like success. The transform is one ffmpeg
    /// writing one pipe, and Kodi opens a URL several times before it plays
    /// it -- mime type, CurlFile, file cache. Whichever reader ends up
    /// playing is not the one that got the container's header, so its ffmpeg
    /// probes bytes from the middle of a cluster:
    ///
    ///   Input #0, ac3, from 'http://127.0.0.1:8790/media/session/5b04df...'
    ///
    /// Kodi played the sound and showed no picture, with the session's id
    /// where the film's name belongs and no duration at all.
    ///
    /// So Kodi is given an address it can own. Measured on the Ultra, same
    /// remux, handed over as the source instead:
    ///
    ///   Input #0, matroska,webm, from 'https://.../To.Rome.with.Love.2012
    ///     .1080p.BluRay.REMUX.AVC.MULTi.DTS-HD.MA.5.1-4K4U.mkv'
    ///   duration 1:51:48, audio dtshd_ma 6ch, video on plane 75 as NV12
    ///
    /// -- the whole film, the real length, and Kodi decoding the DTS-HD the
    /// core would have transcoded for a lesser player.
    async fn handoff_to_kodi(&self) -> Response {
        let Some(playing) = self.player.playing().await else {
            return Response::failure("NOTHING_PLAYING", "burada oynayan bir şey yok");
        };
        // Asked before the player is stopped, because afterwards there is
        // nobody to ask.
        let at = self.player.position().await.unwrap_or(0);
        if playing.source.is_empty() {
            return Response::failure("NO_SOURCE", "bu kaynağın Kodi için adresi yok");
        }

        // Which address Kodi is given. Never one of ours: that is a live pipe
        // and Kodi is not a reader it can serve. When the film here is going
        // through one, Kodi gets the source instead and does its own work --
        // which for this remux means decoding DTS-HD MA itself, measured
        // below.
        let address = if is_proxy_address(&playing.source) {
            if playing.origin.is_empty() {
                return Response::failure(
                    "NO_SOURCE",
                    "bu kaynak dönüştürülerek oynuyor ve Kodi için ayrı bir adresi yok",
                );
            }
            playing.origin.clone()
        } else {
            playing.source.clone()
        };

        // Nothing here belongs to Kodi, so all of it goes: the player and the
        // transform it was reading. The film itself goes on, and so does what
        // the account is told about it.
        let watch = self
            .playback
            .current(crate::playback::PlayerKind::Here)
            .and_then(|film| self.playback.watch(film));
        self.stop_here(crate::playback::Intent::Handover).await;

        let surface = match self.switch_surface(Surface::Kodi).await {
            Ok(status) => status,
            Err(error) => {
                let _ = self.switch_surface(Surface::Ui).await;
                return Response::failure("SURFACE_ERROR", error.to_string());
            }
        };
        if let Err(error) = self.await_kodi().await {
            let _ = self.switch_surface(Surface::Ui).await;
            return Response::failure("KODI_UNAVAILABLE", error);
        }
        match self.kodi.open(&address, at).await {
            Ok(_) => {
                self.follow_kodi(watch, None, playing.duration, at);
                Response::success(json!({
                    "playing": "kodi",
                    "surface": surface,
                    "playbackUrl": address,
                    "startSeconds": at,
                }))
            }
            Err(error) => {
                let _ = self.switch_surface(Surface::Ui).await;
                Response::failure("KODI_ERROR", error.to_string())
            }
        }
    }

    async fn play_on_kodi(
        &self,
        url: Option<String>,
        stream: Option<Value>,
        start_seconds: u64,
        watch: Option<mediabox_core::WatchRef>,
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
        // What Kodi opens: the source, resolved. The core is asked to resolve
        // and to say what is in the file; it is not asked to stand between
        // Kodi and the film. A transform it offers is for players that need
        // one -- the browser, and the interface's own -- and Kodi is not one
        // of those. See the note on `handoff_to_kodi` for what happens when
        // it is handed the transform instead.
        let resolved = session
            .pointer("/handoff/resolvedInput")
            .and_then(Value::as_str)
            .filter(|address| !address.is_empty())
            .map(str::to_owned);
        let kept = resolved.is_none().then(|| session_id.clone());
        let playback_url = match resolved {
            Some(address) => {
                // Nothing will read the transform, so it does not run.
                self.stop_session(&session_id).await;
                address
            }
            None => {
                let Some(address) = session
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
                address
            }
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
            Ok(_) => {
                self.follow_kodi(watch, kept, None, start_seconds);
                Response::success(json!({
                    "session": session,
                    "surface": surface,
                    "playbackUrl": playback_url,
                    "startSeconds": start_seconds,
                }))
            }
            Err(error) => {
                self.stop_session(&session_id).await;
                Response::failure("KODI_ERROR", error.to_string())
            }
        }
    }
}

/// Is this one of the media core's own session addresses?
///
/// Tested on the path rather than on the host: the core hands Kodi a loopback
/// rewrite of its own base URL, so the address the interface is playing and
/// the address Kodi is given are not spelled the same even though they are
/// the same proxy. `/media/session/<id>` is what they share.
fn is_proxy_address(address: &str) -> bool {
    reqwest::Url::parse(address)
        .map(|parsed| parsed.path().starts_with("/media/session/"))
        .unwrap_or(false)
}

impl AppState {
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

    /// End the film playing here, for `intent`: the player, and then the
    /// film's record -- which writes the account and lets the session go.
    /// Decided here rather than waiting for the player's own word, which may
    /// never come from a player that is already gone.
    async fn stop_here(&self, intent: crate::playback::Intent) -> Option<crate::player::Playing> {
        let film = self.playback.current(crate::playback::PlayerKind::Here);
        if let Some(film) = film {
            self.playback.intend(film, intent);
        }
        let stopped = self.player.stop().await;
        match (film, &stopped) {
            (Some(film), _) => {
                self.playback.finish(film, crate::playback::Signal::Gone);
            }
            // A player nobody is following: its session still goes.
            (None, Some(playing)) => self.stop_session(&playing.session_id).await,
            (None, None) => {}
        }
        stopped
    }

    /// A film has started in Kodi: follow it until Kodi says how it ended.
    fn follow_kodi(
        &self,
        watch: Option<mediabox_core::WatchRef>,
        session: Option<String>,
        duration: Option<u64>,
        position: u64,
    ) {
        let film = self.playback.begin(crate::playback::Start {
            player: crate::playback::PlayerKind::Kodi,
            watch,
            session,
            duration,
            position,
        });
        tokio::spawn(crate::playback::follow_kodi(
            Arc::clone(&self.playback),
            Arc::clone(&self.kodi),
            self.kodi_events,
            film,
        ));
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
                // Off by choice is not a fault.
                healthy: cec.available || !cec.settings.enabled,
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
            output: self.output.status(),
            fan: self.fan.status(),
            ethernet: self.ethernet.status(),
            audio: self.audio_status().await,
        }
    }

    /// Sound as it is now: the devices, the kept setting and its plan, and
    /// what the film playing is sending.
    async fn audio_status(&self) -> mediabox_core::AudioStatus {
        let audio = Arc::clone(&self.audio);
        let (setting, devices, plan) = match tokio::task::spawn_blocking(move || audio.now()).await {
            Ok(now) => now,
            Err(error) => {
                return mediabox_core::AudioStatus {
                    error: Some(error.to_string()),
                    ..Default::default()
                };
            }
        };
        mediabox_core::AudioStatus {
            setting,
            devices,
            plan: Some(plan),
            stream: self.player.audio_stream().await,
            error: None,
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

async fn cec_action<F>(runtime: &CecRuntime, action: F) -> Response
where
    F: FnOnce(&Adapter) -> Result<(), mediabox_cec::CecError> + Send + 'static,
{
    // The daemon's own lock, whatever an interface drew: with CEC off the
    // adapters are released, and nothing is sent even if one were not.
    if !runtime.policy.enabled() {
        return Response::failure("CEC_DISABLED", CEC_DISABLED);
    }
    let adapter = match runtime.adapter() {
        Ok(adapter) => adapter,
        Err(why) => return Response::failure("CEC_UNAVAILABLE", why),
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

/// A radio call's answer. The error is already a sentence a viewer can read —
/// and never carries what they typed.
fn radio(value: Result<Value, String>) -> Response {
    match value {
        Ok(value) => Response::success(value),
        Err(error) => Response::failure("RADIO_ERROR", error),
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
    let peer = stream.peer_cred().ok().map(|credentials| credentials.uid());
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
    // What the display's owner committed is not a request. It has a type of
    // its own, which only this socket parses -- the HTTP listeners take
    // `Request` and nothing else -- and only root may send it: the owner runs
    // as root, and nothing on the LAN can be it.
    if let Ok(serde_json::Value::Object(object)) = serde_json::from_str::<Value>(&line)
        && object.contains_key("report")
    {
        let response = match (peer, serde_json::from_value::<mediabox_core::OwnerReport>(Value::Object(object))) {
            (Some(0), Ok(report)) => match state.output.owner_report(report) {
                Ok(status) => Response::success(status),
                Err(error) => Response::failure("OUTPUT_REPORT_REFUSED", error),
            },
            (_, Err(error)) => Response::failure("INVALID_REQUEST", error.to_string()),
            (peer, Ok(_)) => Response::failure(
                "OUTPUT_REPORT_REFUSED",
                format!("ekran raporu yalnız root'tan kabul edilir (uid {peer:?})"),
            ),
        };
        return write_response(&mut write, &response).await;
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

fn ethernet_result(result: Result<mediabox_core::EthernetStatus, String>) -> Response {
    match result {
        Ok(status) => Response::success(status),
        Err(error) => Response::failure("ETHERNET_REFUSED", error),
    }
}

fn fan_result(result: Result<mediabox_core::FanStatus, crate::fan::FanError>) -> Response {
    match result {
        Ok(status) => Response::success(status),
        Err(error) => Response::failure(error.code(), error.to_string()),
    }
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

    /// Which handover a film gets is decided by this, and getting it wrong
    /// is either a 400 from the media core or a film with no picture.
    #[test]
    fn the_core_s_own_sessions_are_recognised_whatever_host_they_wear() {
        assert!(is_proxy_address(
            "http://127.0.0.1:8790/media/session/ddd28e498d739d04226cb16d2e9f4f59"
        ));
        assert!(is_proxy_address(
            "http://10.27.27.35:8790/media/session/ddd28e498d739d04226cb16d2e9f4f59"
        ));
        assert!(!is_proxy_address(
            "https://21-4.download.real-debrid.com/d/ZI6AQSXVDNJ2K/A%20Film.mkv"
        ));
        assert!(!is_proxy_address(
            "http://127.0.0.1:11470/hlsv2/abc/master.m3u8"
        ));
        assert!(!is_proxy_address(""));
    }

    /// A whole daemon state, pointed at `dir` for everything it keeps.
    fn test_state(dir: &tempfile::TempDir) -> Arc<AppState> {
        let media = Arc::new(MediaClient::new("http://127.0.0.1:9", Duration::from_millis(20)).unwrap());
        let player = Arc::new(PlayerManager::new("/bin/true", "/run/mediabox/player.sock"));
        Arc::new(AppState {
            kodi: Arc::new(
                KodiClient::new("http://127.0.0.1:9/jsonrpc", Duration::from_millis(20)).unwrap(),
            ),
            lifecycle: KodiLifecycle::new("kodi.service").unwrap(),
            cec: CecRuntime {
                adapters: Vec::new(),
                unavailable: CecStatus {
                    error: Some("testte kapalı".into()),
                    ..Default::default()
                },
                target: Arc::new(|| crate::cec::CecTarget::Refused("testte kapalı".into())),
                policy: Arc::new(crate::cec::Policy::load(
                    dir.path().join("cec.json"),
                    dir.path().join("cec-session.json"),
                    "test-boot",
                )),
                requested: std::sync::Mutex::new(None),
            },
            input: InputManager::new(InputMode::Ui),
            media: Arc::clone(&media),
            surface: SurfaceManager::new(
                "kodi.service",
                "mediabox-tv-ui.service",
                crate::transition::DisplayTransition::new(),
            )
            .unwrap(),
            // Never started in the tests; it exists so the state is whole.
            player: Arc::clone(&player),
            subtitles: crate::subtitles::Subtitles::new(media, player, dir.path().join("subtitles.json")),
            // Pointed at the empty temporary directory, so the test never
            // reaches the machine's own sysfs and reports no lights.
            leds: LedController::new(dir.path(), dir.path().join("leds")),
            output: Output::new(
                crate::output::Paths {
                    setting: dir.path().join("output.json"),
                    trial: dir.path().join("output-trial.json"),
                    plan: dir.path().join("output-plan"),
                    observer: dir.path().join("observer"),
                    matching: dir.path().join("output-content-matching"),
                },
                || {},
            ),
            fan: FanController::new(crate::fan::FanPaths::under(dir.path())),
            ethernet: crate::ethernet::Ethernet::new(
                crate::ethernet::EthernetPaths::under(dir.path()),
                false,
            ),
            audio: Arc::new(crate::audio::Audio::new(dir.path().join("audio.json"), Vec::new, |_| {})),
            playback: Arc::new(crate::playback::Supervisor::new(|_| {})),
            kodi_events: "127.0.0.1:9".parse().unwrap(),
            applications: ApplicationManager::load(
                None,
                "kodi.service",
                "mediabox-tv-ui.service",
                crate::transition::DisplayTransition::new(),
            )
            .unwrap(),
        })
    }

    #[tokio::test]
    async fn unix_socket_request_returns_structured_response() {
        let dir = tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let state = test_state(&dir);
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

    #[test]
    fn nothing_a_controller_sends_can_say_what_the_display_hardware_is() {
        // The interface used to report the offer and the wire as a command
        // any client could send. There is no such command any more.
        assert!(serde_json::from_str::<Request>(
            r#"{"command":"output_report","offer":{},"wire":{"mode":"3840x2160p60"}}"#
        )
        .is_err());
        // What the owner committed is its own type, which `Request` -- all
        // the HTTP listeners parse -- is not.
        let report = mediabox_core::OwnerReport::Failed {
            identity: mediabox_core::DisplayIdentity::default(),
            trial: None,
            error: "x".into(),
        };
        let text = serde_json::to_string(&report).unwrap();
        assert!(serde_json::from_str::<Request>(&text).is_err());
    }

    #[tokio::test]
    async fn an_owner_report_from_anyone_but_root_is_refused() {
        if unsafe { libc::getuid() } == 0 {
            return; // the check is about the peer, and here the peer is root
        }
        let dir = tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let state = test_state(&dir);
        let task = tokio::spawn(serve_unix(listener, state));
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        let report = mediabox_core::OwnerReport::Failed {
            identity: mediabox_core::DisplayIdentity::default(),
            trial: None,
            error: "x".into(),
        };
        stream
            .write_all(format!("{}\n", serde_json::to_string(&report).unwrap()).as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        let response: Response = serde_json::from_str(&line).unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "OUTPUT_REPORT_REFUSED");
        task.abort();
    }

    fn code(response: &Response) -> Option<&str> {
        response.error.as_ref().map(|error| error.code.as_str())
    }

    #[tokio::test]
    async fn with_cec_off_every_cec_command_is_refused_by_the_daemon() {
        let dir = tempdir().unwrap();
        let state = test_state(&dir);
        // On, with no adapter: refused for the adapter, not for the switch.
        let on = state.handle(Request::CecWakeTv).await;
        assert_eq!(code(&on), Some("CEC_UNAVAILABLE"));
        let off = mediabox_core::CecSettings { enabled: false, ..Default::default() };
        assert!(state.handle(Request::CecSettingsSet { settings: off }).await.ok);
        for request in [
            Request::CecWakeTv,
            Request::CecStandbyTv,
            Request::CecActiveSource,
            Request::CecDevices,
        ] {
            let answer = state.handle(request.clone()).await;
            assert_eq!(code(&answer), Some("CEC_DISABLED"), "{request:?}");
        }
        // The status still answers, and says why.
        let status = state.cec.status();
        assert!(!status.available && !status.settings.enabled);
        assert_eq!(status.error.as_deref(), Some(CEC_DISABLED));
    }

    #[tokio::test]
    async fn turning_cec_back_on_brings_every_choice_back() {
        let dir = tempdir().unwrap();
        let state = test_state(&dir);
        let chosen = mediabox_core::CecSettings {
            wake_tv_on_start: true,
            active_source_on_start: true,
            restore_power_on_shutdown: true,
            power_target: mediabox_core::CecPowerTarget::TvAndAudioSystem,
            on_active_source_lost: mediabox_core::CecSourceLost::Standby,
            remote_control: false,
            ..Default::default()
        };
        assert!(state.handle(Request::CecSettingsSet { settings: chosen }).await.ok);
        let off = chosen.with(mediabox_core::CecChange::Enabled(false));
        assert!(state.handle(Request::CecSettingsSet { settings: off }).await.ok);
        // Kept on disk while off, as a later daemon reads it.
        let reread = crate::cec::Policy::load(
            dir.path().join("cec.json"),
            dir.path().join("cec-session.json"),
            "test-boot",
        );
        assert_eq!(reread.settings(), off);
        let on = state.cec.status().settings.with(mediabox_core::CecChange::Enabled(true));
        assert!(state.handle(Request::CecSettingsSet { settings: on }).await.ok);
        assert_eq!(state.cec.status().settings, chosen);
    }

    #[tokio::test]
    async fn a_contradictory_pair_is_refused_and_nothing_changes() {
        let dir = tempdir().unwrap();
        let state = test_state(&dir);
        let both = mediabox_core::CecSettings {
            standby_tv_on_shutdown: true,
            restore_power_on_shutdown: true,
            ..Default::default()
        };
        let answer = state.handle(Request::CecSettingsSet { settings: both }).await;
        assert_eq!(code(&answer), Some("CEC_SETTINGS_INVALID"));
        assert_eq!(state.cec.status().settings, mediabox_core::CecSettings::default());
        assert!(!dir.path().join("cec.json").exists());
    }

    #[tokio::test]
    async fn the_settings_request_is_on_the_wire_as_the_interfaces_send_it() {
        let parsed: Request = serde_json::from_str(
            r#"{"command":"cec_settings_set","settings":{"enabled":false,"power_target":"broadcast"}}"#,
        )
        .unwrap();
        let Request::CecSettingsSet { settings } = parsed else { panic!("{parsed:?}") };
        assert!(!settings.enabled && settings.remote_control);
        assert_eq!(settings.power_target, mediabox_core::CecPowerTarget::Broadcast);
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

/// The display for the length of one film: matched to its frame rate before a
/// frame moves, and put back when it ends.
///
/// The player opens paused (`MEDIABOX_PLAYER_HOLD`). Its own reading of the
/// container -- `container-fps` -- is there as soon as the file is open,
/// before the first frame is shown, and the refresh is chosen from it by the
/// same offer every other display decision comes from
/// ([`mediabox_core::OutputOffer::content_mode`]); the interface, which holds
/// the display, commits it. The film is let go once the owner has reported
/// the mode committed and the television has had a moment to lock to it.
///
/// A container that does not say (a raw elementary stream, a live source) is
/// let go at once and measured instead: `estimated-vf-fps`, from frames the
/// player has actually shown, once a few seconds of them exist. The display
/// then changes under a playing film -- rarer, and later, but right.
///
/// Whatever happens, the film is let go: a player never stays held because
/// the display could not be matched.
async fn follow_film(
    player: Arc<PlayerManager>,
    output: Arc<crate::output::Output>,
    film: u64,
) {
    use std::time::Duration;
    let tick = Duration::from_millis(100);
    let rate = |value: Option<Value>| value.and_then(|value| value.as_f64()).and_then(mediabox_core::Cadence::from_fps);

    let mut cadence = None;
    for _ in 0..80 {
        if player.film() != film {
            return;
        }
        if let Some(found) = rate(player.property("container-fps").await) {
            cadence = Some((found, "container-fps"));
            break;
        }
        tokio::time::sleep(tick).await;
    }
    if cadence.is_none() {
        player.release(film).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        for _ in 0..40 {
            if player.film() != film {
                return;
            }
            if let Some(found) = rate(player.property("estimated-vf-fps").await) {
                cadence = Some((found, "estimated-vf-fps"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    match cadence {
        Some((cadence, from)) => {
            eprintln!("mediaboxd-rs: film {film}: {} fps from {from}", cadence.label());
            match output.content_start(film, cadence) {
                Ok(chosen) if !chosen.applied => {
                    // The owner commits on its next frame; the television
                    // then re-locks, which is a second or two of black.
                    for _ in 0..50 {
                        if player.film() != film || output.content_applied(film) {
                            break;
                        }
                        tokio::time::sleep(tick).await;
                    }
                    if output.content_applied(film) {
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                    } else {
                        eprintln!("mediaboxd-rs: film {film}: the owner did not report the matched mode");
                    }
                }
                Ok(_) => {}
                Err(why) => eprintln!("mediaboxd-rs: film {film}: display not matched: {why}"),
            }
        }
        None => eprintln!("mediaboxd-rs: film {film}: no frame rate to match"),
    }
    player.release(film).await;

    // Until it ends. The player going quiet twice running is the end too:
    // a film that finishes exits by itself and nobody calls stop.
    let mut silent = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if player.film() != film {
            break;
        }
        if player.answers().await {
            silent = 0;
        } else {
            silent += 1;
            if silent >= 2 {
                break;
            }
        }
    }
    output.content_end(film);
}
