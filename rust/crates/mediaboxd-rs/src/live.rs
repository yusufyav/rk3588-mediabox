//! Live television: the DVB receiver's process, started and stopped here.
//!
//! The receiver -- the USB device, the LNB supply, the transport stream -- is
//! `mediabox-dvb`'s, not this daemon's. This daemon is sandboxed away from
//! USB (`DevicePolicy=closed` in mediaboxd-rs.service) and stays that way:
//! it starts the receiver as a transient unit, as it starts the player, for
//! the same reason (a child would inherit the sandbox), and reads what it
//! says in its status file.
//!
//! ```text
//! mediaboxd-rs ──systemd-run──> mediabox-dvb.service  (USB, LNB, TS, root)
//!      │                          └─ FIFO /run/mediabox-dvb/live.ts ─┐
//!      └──systemd-run──> mediabox-player.service (mpv) <─────────────┘
//!                          └─ dma-buf -> mediabox-tv-ui -> VOP2
//! ```
//!
//! Both units are bound to the interface (`BindsTo=mediabox-tv-ui.service`):
//! the picture is the interface's, and neither may outlive it. The receiver
//! ends by itself when the player closes the FIFO (EPIPE), and is stopped
//! with SIGTERM otherwise; either way its own cleanup (LNB off, EP4 off,
//! GPIO restored) runs before it exits, and `systemctl stop` waits for it.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mediabox_core::{LiveChannel, LiveEvent, LiveSource, SatellitePolarisation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::playback::SourceEnd;

/// What a wait answers when a newer channel has taken over from it.
pub const CALLED_OFF: &str = "yeni bir kanal istendi";

pub const UNIT: &str = "mediabox-dvb.service";

/// How long a warm receiver waits for the next player before it ends.
pub const HOLD_READER_S: u64 = 30;

#[derive(Debug, Clone)]
pub struct LivePaths {
    pub binary: PathBuf,
    /// The unit's RuntimeDirectory: the FIFO and the status file.
    pub run_dir: PathBuf,
    pub recording: PathBuf,
    pub firmware_dir: PathBuf,
    /// The channels and programme guide of the last multiplex the receiver
    /// was on, kept across receivers and boots.
    pub guide_store: PathBuf,
}

impl LivePaths {
    pub fn system() -> Self {
        Self {
            binary: "/opt/rk3588-mediabox/bin/mediabox-dvb".into(),
            run_dir: "/run/mediabox-dvb".into(),
            recording: "/var/lib/mediabox-dvb/recording.ts".into(),
            firmware_dir: "/var/lib/mediabox-dvb/firmware".into(),
            guide_store: "/var/lib/mediabox/live-guide.json".into(),
        }
    }

    /// The receiver's own account of the multiplex's services and events.
    pub fn guide(&self) -> PathBuf {
        self.run_dir.join("guide.json")
    }

    pub fn fifo(&self) -> PathBuf {
        self.run_dir.join("live.ts")
    }

    pub fn status(&self) -> PathBuf {
        self.run_dir.join("status.json")
    }

    pub fn tkgs(&self) -> PathBuf {
        self.run_dir.join("tkgs.json")
    }
}

/// Where live television is in its life, as the control plane drives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Idle,
    /// The receiver's unit is starting and has not yet opened its output.
    Starting,
    /// The player is on it.
    Playing,
    Stopping,
    /// Up, booted and locked, its stream stopped: nobody is watching.
    Standby,
    /// It ended without being asked to; `failure` says why.
    Failed,
    /// Stopped, and the receiver's cleanup has run.
    Cleaned,
}

struct Inner {
    lifecycle: Lifecycle,
    service_id: Option<u16>,
    source: Option<LiveSource>,
    token: Option<String>,
    failure: Option<String>,
    /// The receiver's last status, kept after it is gone.
    last: Option<Value>,
    /// The FIFO the receiver writes now: `live.ts` from a start, a new name
    /// each time a channel is opened in the same player (`switch`).
    fifo: Option<PathBuf>,
    /// For those names.
    fifos: u64,
}

pub struct LiveManager {
    paths: LivePaths,
    inner: Mutex<Inner>,
    starts: AtomicU64,
    /// When preparing the receiver last failed: not tried again for a while.
    prepare_failed: Mutex<Option<Instant>>,
}

/// How long a failed preparation waits before the next.
const PREPARE_RETRY: Duration = Duration::from_secs(600);

/// What the receiver's ending means to the viewer.
fn describe(end: &str, detail: Option<&str>) -> String {
    let text = match end {
        "usb_disconnected" => "Uydu alıcısının USB bağlantısı koptu.",
        "device_not_found" => "Uydu alıcısı bulunamadı.",
        "no_lock" => "Uydu sinyaline kilitlenilemedi.",
        "tune_failed" => "Uydu alıcısı ayarlanamadı.",
        "service_not_found" => "Kanal bu yayında bulunamadı.",
        "source_failed" => "Canlı yayın kaynağı açılamadı.",
        "player_never_opened" | "output_failed" => "Canlı yayın oynatıcıya aktarılamadı.",
        "player_closed" => "Oynatıcı kapandı.",
        _ => "Canlı yayın kesildi.",
    };
    match detail.filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("{text} ({detail})"),
        None => text.to_string(),
    }
}

impl LiveManager {
    pub fn new(paths: LivePaths) -> Self {
        Self {
            paths,
            inner: Mutex::new(Inner {
                lifecycle: Lifecycle::Idle,
                service_id: None,
                source: None,
                token: None,
                failure: None,
                last: None,
                fifo: None,
                fifos: 0,
            }),
            starts: AtomicU64::new(0),
            prepare_failed: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The FIFO the receiver writes now.
    pub fn fifo(&self) -> PathBuf {
        self.lock().fifo.clone().unwrap_or_else(|| self.paths.fifo())
    }

    pub fn lifecycle(&self) -> Lifecycle {
        self.lock().lifecycle
    }

    fn set(&self, lifecycle: Lifecycle) {
        let mut inner = self.lock();
        if inner.lifecycle != lifecycle {
            eprintln!("mediaboxd-rs: live {:?} -> {lifecycle:?}", inner.lifecycle);
            inner.lifecycle = lifecycle;
        }
    }

    /// The command line the receiver's unit is started with. Every argument
    /// comes from a typed value; nothing a caller sent is passed through.
    pub fn argv(&self, service_id: u16, source: &LiveSource, token: &str) -> Vec<String> {
        self.argv_with(service_id, source, token, false)
    }

    fn argv_with(&self, service_id: u16, source: &LiveSource, token: &str, standby: bool) -> Vec<String> {
        let mut argv: Vec<String> = [
            "--collect".to_string(),
            format!("--unit={UNIT}"),
            "--property=Type=exec".into(),
            // The picture is the interface's; the receiver may not outlive it.
            "--property=BindsTo=mediabox-tv-ui.service".into(),
            "--property=After=mediabox-tv-ui.service".into(),
            "--property=RuntimeDirectory=mediabox-dvb".into(),
            // The status outlives the unit: how it ended is read after it is gone.
            "--property=RuntimeDirectoryPreserve=yes".into(),
            // SIGTERM to the receiver alone; it cleans up (LNB off, EP4 off,
            // GPIO restored) and exits. 15 s before systemd stops waiting.
            "--property=KillMode=mixed".into(),
            "--property=TimeoutStopSec=15s".into(),
            "--property=LogRateLimitIntervalSec=30s".into(),
            "--property=LogRateLimitBurst=500".into(),
        ]
        .into();
        argv.push(self.paths.binary.display().to_string());
        argv.extend(["play".into(), "--service".into(), service_id.to_string()]);
        argv.extend(["--fifo".into(), self.paths.fifo().display().to_string()]);
        argv.extend(["--status".into(), self.paths.status().display().to_string()]);
        argv.extend(["--token".into(), token.to_string()]);
        match *source {
            LiveSource::Recording => {
                argv.extend(["--recording".into(), self.paths.recording.display().to_string(), "--pace".into()]);
            }
            LiveSource::Satellite { frequency_khz, symbol_rate_ksps, polarisation } => {
                // The receiver itself, chosen explicitly. Never an LNB flag:
                // the RF source is prepared outside MediaBox.
                argv.push("--hardware".into());
                argv.extend(["--transponder".into(), frequency_khz.to_string()]);
                argv.extend(["--sr".into(), symbol_rate_ksps.to_string()]);
                argv.push(format!(
                    "--polarisation={}",
                    match polarisation { SatellitePolarisation::Horizontal => "horizontal", SatellitePolarisation::Vertical => "vertical" }
                ));
                argv.extend(["--firmware-dir".into(), self.paths.firmware_dir.display().to_string()]);
                // A receiver unplugged and plugged back in carries on.
                argv.extend(["--reconnect-s".into(), "20".into()]);
                // Warm between players: a channel change is a new service
                // for the selector (`switch`), not a new receiver.
                argv.extend(["--hold-reader-s".into(), HOLD_READER_S.to_string()]);
                if standby {
                    argv.push("--standby".into());
                }
                if let Some(offset) = self.stored_guide().and_then(|guide| guide.multiplex(source)?.carrier_offset_khz) {
                    argv.push(format!("--carrier-first-khz={offset}"));
                }
            }
        }
        argv
    }

    /// The receiver's status, if it is this start's.
    pub fn read_status(&self) -> Option<Value> {
        let token = self.lock().token.clone()?;
        let text = std::fs::read_to_string(self.paths.status()).ok()?;
        let status: Value = serde_json::from_str(&text).ok()?;
        (status.get("token").and_then(Value::as_str) == Some(token.as_str())).then_some(status)
    }

    fn final_of(status: &Value) -> Option<(String, Option<String>)> {
        matches!(status.get("state").and_then(Value::as_str), Some("ended" | "failed")).then(|| {
            (
                status.get("end").and_then(Value::as_str).unwrap_or("").to_string(),
                status.get("message").and_then(Value::as_str).map(str::to_owned),
            )
        })
    }

    pub async fn unit_active() -> bool {
        Command::new("systemctl")
            .args(["is-active", "--quiet", UNIT])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    /// Start the receiver for `service_id` of `source`, replacing whatever
    /// receiver ran before; returns once its output (the FIFO) exists.
    pub async fn start(&self, service_id: u16, source: &LiveSource) -> Result<(), String> {
        self.start_with(service_id, source, false).await
    }

    async fn start_with(&self, service_id: u16, source: &LiveSource, standby: bool) -> Result<(), String> {
        // A UI that was already running before polarisation was added sends
        // the old wire shape (which deserialises as vertical).  The TKGS list
        // is authoritative, so recover the full source without restarting UI.
        let source = self.canonical_source(service_id, source);
        let source = &source;
        source.validate()?;
        self.stop().await;
        let token = format!("{}-{}", std::process::id(), self.starts.fetch_add(1, Ordering::SeqCst) + 1);
        {
            let mut inner = self.lock();
            inner.service_id = Some(service_id);
            inner.source = Some(source.clone());
            inner.token = Some(token.clone());
            inner.failure = None;
            inner.last = None;
            inner.fifo = None;
        }
        self.set(Lifecycle::Starting);
        let output = Command::new("systemd-run")
            .args(self.argv_with(service_id, source, &token, standby))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| format!("uydu alıcısı başlatılamadı: {error}"))?;
        if !output.status.success() {
            let reason = format!("uydu alıcısı başlatılamadı: {}", String::from_utf8_lossy(&output.stderr).trim());
            self.fail(reason.clone());
            return Err(reason);
        }
        // Up to the FIFO: the player opens it, and must not find it missing.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.read_status() {
                if let Some((end, message)) = Self::final_of(&status) {
                    let reason = describe(&end, message.as_deref());
                    self.fail(reason.clone());
                    return Err(reason);
                }
                if status.get("state").and_then(Value::as_str) != Some("starting") && self.fifo().exists() {
                    return Ok(());
                }
            }
            if Instant::now() > deadline {
                let reason = "uydu alıcısı çıkışını açmadı".to_string();
                self.stop().await;
                self.fail(reason.clone());
                return Err(reason);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Whether a channel of `source` can be had from the receiver already
    /// up -- streaming, or in standby -- with its demodulator as it is: any
    /// satellite transponder, the one tuned (a new service) or another (a
    /// retune without the 3.5 s boot).
    pub fn can_reuse(&self, source: &LiveSource) -> bool {
        {
            let inner = self.lock();
            let satellite = |source: Option<&LiveSource>| matches!(source, Some(LiveSource::Satellite { .. }));
            if inner.token.is_none()
                || !satellite(Some(source))
                || !satellite(inner.source.as_ref())
                // Starting is a channel being put on: the receiver is up, and a
                // newer channel retunes it rather than starting another.
                || !matches!(inner.lifecycle, Lifecycle::Playing | Lifecycle::Standby | Lifecycle::Starting)
            {
                return false;
            }
        }
        // Without a signal the receiver is still up and tuned, and the next
        // channel is a retune like any other (VDR keeps its tuner thread
        // for every channel; a lost lock is a state of it).
        matches!(self.receiver_state().as_deref(), Some("streaming" | "standby" | "no_signal" | "tuning"))
    }

    fn select(&self, want: Value) -> Result<(), String> {
        let token = self.lock().token.clone().ok_or("uydu alıcısı çalışmıyor")?;
        let path = self.paths.run_dir.join("select.json");
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json!({"token": token, "want": want}).to_string())
            .and_then(|()| std::fs::rename(&temporary, &path))
            .map_err(|error| format!("alıcıya istek yazılamadı: {error}"))
    }

    /// Wait until the receiver says `done`, or has ended.
    async fn until(&self, within: Duration, done: impl Fn(&Value) -> bool) -> Result<(), String> {
        self.until_or(within, done, || false).await
    }

    /// `until`, given up as soon as `called_off` says a newer request has
    /// taken over: the answer is `CALLED_OFF`.
    async fn until_or(
        &self,
        within: Duration,
        done: impl Fn(&Value) -> bool,
        called_off: impl Fn() -> bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + within;
        loop {
            if called_off() {
                return Err(CALLED_OFF.into());
            }
            if let Some(status) = self.read_status() {
                if Self::final_of(&status).is_some() {
                    return Err("uydu alıcısı kapandı".into());
                }
                if done(&status) {
                    return Ok(());
                }
            }
            if Instant::now() > deadline {
                return Err("uydu alıcısı isteği onaylamadı".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Ask the receiver already up for `service_id` of `source`
    /// (`select.json`), and wait until it streams it. With `new_fifo`, the
    /// stream moves to a FIFO of its own for the player that is already
    /// reading, which opens it as its next file (`fifo()` says which).
    /// Another channel, without a restart. Given up with `CALLED_OFF` once
    /// `called_off` says a newer channel was asked for: the receiver follows
    /// whatever `select.json` says last, so waiting on for this one would
    /// only hold the newer one back.
    pub async fn switch(
        &self,
        service_id: u16,
        source: &LiveSource,
        new_fifo: bool,
        called_off: impl Fn() -> bool,
    ) -> Result<(), String> {
        let source = self.canonical_source(service_id, source);
        let source = &source;
        let LiveSource::Satellite { frequency_khz, symbol_rate_ksps, polarisation } = *source else {
            return Err("yalnız uydu kaynağı".into());
        };
        let retune = self.lock().source.as_ref() != Some(source);
        let name = new_fifo.then(|| {
            let mut inner = self.lock();
            inner.fifos += 1;
            format!("live-{}.ts", inner.fifos)
        });
        self.select(json!({
            "kind": "play",
            "service_id": service_id,
            "transponder": {"frequency_khz": frequency_khz, "symbol_rate_ksps": symbol_rate_ksps, "polarisation": polarisation},
            "new_fifo": name,
        }))?;
        if let Some(name) = name {
            self.lock().fifo = Some(self.paths.run_dir.join(name));
        }
        // A service of the multiplex tuned is a selector's restart; another
        // transponder is a lock, seconds at most.
        let within = Duration::from_secs(if retune { 8 } else { 8 });
        self.until_or(
            within,
            // Streaming, or tuned with no signal: either is the receiver on
            // this channel, and no signal is shown, not taken for a failure.
            |status| {
                status.get("service_id").and_then(Value::as_u64) == Some(service_id as u64)
                    && matches!(status.get("state").and_then(Value::as_str), Some("streaming" | "no_signal"))
            },
            called_off,
        )
        .await?;
        let mut inner = self.lock();
        inner.service_id = Some(service_id);
        inner.source = Some(source.clone());
        inner.failure = None;
        drop(inner);
        self.set(Lifecycle::Starting);
        Ok(())
    }

    /// Nobody is watching: the receiver stops its stream and keeps its
    /// demodulator booted and locked. A receiver that cannot is stopped.
    pub async fn standby(&self) {
        let up = {
            let inner = self.lock();
            inner.token.is_some() && matches!(inner.source, Some(LiveSource::Satellite { .. }))
        };
        let ready = up && matches!(self.receiver_state().as_deref(), Some("streaming" | "standby"));
        if ready
            && self.select(json!({"kind": "standby"})).is_ok()
            && self
                .until(Duration::from_secs(2), |status| status.get("state").and_then(Value::as_str) == Some("standby"))
                .await
                .is_ok()
        {
            self.set(Lifecycle::Standby);
            return;
        }
        self.stop().await;
    }

    /// Retune the already running receiver and read TKGS without restarting
    /// its process or reloading the demodulator patch.
    pub async fn read_tkgs(&self) -> Result<Value, String> {
        let (token, was_playing, service_id, source) = {
            let inner = self.lock();
            if inner.token.is_none() || !matches!(inner.source, Some(LiveSource::Satellite { .. })) {
                return Err("hazır uydu alıcısı yok".into());
            }
            (inner.token.clone().unwrap_or_default(), inner.lifecycle == Lifecycle::Playing, inner.service_id, inner.source.clone())
        };
        self.select(json!({
            "kind": "tkgs",
            "transponder": {"frequency_khz": 12_380_000u32, "symbol_rate_ksps": 27_500u32, "polarisation": "vertical"},
        }))?;
        let deadline = Instant::now() + Duration::from_secs(85);
        let result = loop {
            if let Ok(text) = std::fs::read_to_string(self.paths.tkgs())
                && let Ok(value) = serde_json::from_str::<Value>(&text)
                && value.get("token").and_then(Value::as_str) == Some(token.as_str())
            {
                match value.get("state").and_then(Value::as_str) {
                    Some("complete") => break Ok(value),
                    Some("failed") => break Err(value.get("error").and_then(Value::as_str).unwrap_or("TKGS alınamadı").to_string()),
                    _ => {}
                }
            }
            if !Self::unit_active().await {
                break Err("TKGS alınırken uydu alıcısı kapandı".into());
            }
            if Instant::now() >= deadline {
                break Err("TKGS tablosu 85 saniyede tamamlanmadı".into());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        if was_playing
            && let (Some(service_id), Some(LiveSource::Satellite { frequency_khz, symbol_rate_ksps, polarisation })) = (service_id, source)
        {
            let _ = self.select(json!({
                "kind": "play", "service_id": service_id,
                "transponder": {"frequency_khz": frequency_khz, "symbol_rate_ksps": symbol_rate_ksps, "polarisation": polarisation}, "new_fifo": null,
            }));
        }
        result
    }

    /// Make the receiver ready before anybody watches, as a television's
    /// tuner is: booted and locked on the first satellite multiplex known,
    /// in standby. Only when the receiver is plugged in and no receiver runs
    /// (one that went with the interface leaves only its token behind).
    pub async fn prepare(&self) -> bool {
        if !receiver_plugged_in()
            || matches!(self.lifecycle(), Lifecycle::Starting | Lifecycle::Playing | Lifecycle::Stopping)
            || self.prepare_failed.lock().unwrap_or_else(|p| p.into_inner()).is_some_and(|at| at.elapsed() < PREPARE_RETRY)
            || Self::unit_active().await
            || !interface_active().await
        {
            return false;
        }
        let ready_source = LiveSource::Satellite {
            frequency_khz: 12_380_000,
            symbol_rate_ksps: 27_500,
            polarisation: SatellitePolarisation::Vertical,
        };
        let service_id = self.stored_guide()
            .and_then(|guide| guide.multiplex(&ready_source)?.services.first().map(|service| service.service_id))
            .unwrap_or(0);
        let source = ready_source;
        match self.start_with(service_id, &source, true).await {
            Ok(()) => {
                self.set(Lifecycle::Standby);
                eprintln!("mediaboxd-rs: live receiver prepared in standby on {source:?}");
                true
            }
            Err(error) => {
                eprintln!("mediaboxd-rs: live receiver not prepared: {error}");
                *self.prepare_failed.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
                false
            }
        }
    }

    pub fn playing(&self) {
        self.set(Lifecycle::Playing);
    }

    fn fail(&self, reason: String) {
        self.lock().failure = Some(reason);
        self.set(Lifecycle::Failed);
    }

    /// How the source ended, once the player has reached the end of its
    /// stream: the receiver writes its last status before it closes the
    /// FIFO, so it is there, or about to be. None while the receiver still
    /// runs (the player went first).
    pub async fn final_end(&self) -> Option<SourceEnd> {
        self.lock().token.as_ref()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.read_status()
                && let Some((end, message)) = Self::final_of(&status)
            {
                self.lock().last = Some(status);
                let natural = end == "recording_end";
                let text = (!natural).then(|| describe(&end, message.as_deref()));
                // The player going, or a stop that was asked for, is not the
                // receiver failing: only its own faults are recorded as one.
                if let Some(text) = &text
                    && !matches!(end.as_str(), "stopped" | "player_closed")
                {
                    self.fail(text.clone());
                }
                return Some(SourceEnd { natural, message: text });
            }
            if Instant::now() > deadline {
                return (!Self::unit_active().await).then(|| {
                    let text = "Uydu alıcısı süreci beklenmedik biçimde kapandı.".to_string();
                    self.fail(text.clone());
                    SourceEnd { natural: false, message: Some(text) }
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Stop the receiver and wait for its cleanup. Quiet when none runs.
    pub async fn stop(&self) -> Option<Value> {
        if self.lock().token.is_none() && !Self::unit_active().await {
            return None;
        }
        self.set(Lifecycle::Stopping);
        let _ = Command::new("systemctl")
            .args(["stop", UNIT])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        let last = self.read_status();
        // The receiver's last word on its multiplex, while it is still this start's.
        self.absorb_guide();
        let mut inner = self.lock();
        if last.is_some() {
            inner.last = last.clone();
        }
        inner.token = None;
        let failed = inner.lifecycle == Lifecycle::Failed;
        drop(inner);
        if !failed {
            self.set(Lifecycle::Cleaned);
        }
        last
    }

    /// A receiver left running by a daemon that is gone: nobody follows it,
    /// and the player on it is nobody's either.
    pub async fn sweep(&self) -> bool {
        if !Self::unit_active().await {
            return false;
        }
        eprintln!("mediaboxd-rs: {UNIT} running with nobody following it; stopping it");
        let _ = Command::new("systemctl").args(["stop", UNIT]).status().await;
        self.set(Lifecycle::Cleaned);
        true
    }

    /// Where the receiver is (`mediabox_dvb::service::State`), if it is this start's.
    pub fn receiver_state(&self) -> Option<String> {
        self.read_status()?.get("state")?.as_str().map(str::to_owned)
    }

    /// The rate the player will put frames out at, from the receiver's
    /// reading of the video's SPS: an interlaced stream's field rate (MPP
    /// deinterlaces to one frame per field), a progressive one's frame rate.
    pub fn output_rate(&self) -> Option<f64> {
        let video = self.read_status()?.pointer("/selection/video")?.clone();
        let rate = |key: &str| -> Option<f64> {
            let pair = video.get(key)?.as_array()?;
            let (num, den) = (pair.first()?.as_f64()?, pair.get(1)?.as_f64()?);
            (den > 0.0).then(|| num / den)
        };
        if video.get("interlaced").and_then(Value::as_bool) == Some(true) {
            rate("field_rate")
        } else {
            rate("frame_rate")
        }
    }

    /// The service the receiver was asked for.
    pub fn live_service(&self) -> Option<u16> {
        self.lock().service_id
    }

    /// The rate `service_id` of `source` played at last time, if known.
    pub fn known_rate(&self, service_id: u16) -> Option<f64> {
        let source = self.lock().source.clone()?;
        self.stored_guide()?.multiplex(&source)?.rates.get(&service_id).copied()
    }

    /// What the interface and `mediaboxctl` read.
    pub fn status(&self) -> Value {
        let receiver = self.read_status();
        let inner = self.lock();
        json!({
            "lifecycle": inner.lifecycle,
            "service_id": inner.service_id,
            "source": inner.source,
            "failure": inner.failure,
            "receiver": receiver.or_else(|| inner.last.clone()),
        })
    }
}

/// Whether the interface the receiver's unit is bound to is up: started
/// without it, the receiver would be stopped at once.
async fn interface_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", "mediabox-tv-ui.service"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// Whether the Kanky Link-4K is on the USB bus: read from sysfs, which this
/// daemon may read although it may not open the device.
fn receiver_plugged_in() -> bool {
    let Ok(devices) = std::fs::read_dir("/sys/bus/usb/devices") else { return false };
    devices.flatten().any(|device| {
        let read = |name: &str| std::fs::read_to_string(device.path().join(name)).unwrap_or_default();
        read("idVendor").trim() == "23e2" && read("idProduct").trim() == "5270"
    })
}

// ------------------------------------------------------------------ guide

/// Playable TKGS service types (EN 300 468 table 87): digital television,
/// digital radio, MPEG-2 HD, and advanced codec SD/HD/UHD. Private data
/// services such as TKGS itself stay out of the channel list.
fn is_television(service_type: Option<u8>) -> bool {
    matches!(service_type, Some(0x01 | 0x02 | 0x11 | 0x16 | 0x19 | 0x1F))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredService {
    pub service_id: u16,
    pub name: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub service_type: Option<u8>,
}

/// One multiplex in `/var/lib/mediabox/live-guide.json`: the source it came
/// from, its services and every event its EIT carried that has not long
/// ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredGuide {
    pub source: LiveSource,
    #[serde(default)]
    pub transport_stream_id: Option<u16>,
    /// When the receiver last said something new, UTC seconds.
    pub updated: i64,
    pub services: Vec<StoredService>,
    #[serde(default)]
    pub events: std::collections::BTreeMap<u16, Vec<LiveEvent>>,
    /// Where the carrier was found last time, from the nominal IF (kHz): the
    /// next cold start tries it first.
    #[serde(default)]
    pub carrier_offset_khz: Option<i32>,
    /// Each service's video rate as its SPS said it (frames, or fields of an
    /// interlaced stream): the display is matched to it before the next
    /// cold start of that channel has a frame to read it from.
    #[serde(default)]
    pub rates: std::collections::BTreeMap<u16, f64>,
}

/// The receiver's `guide.json`, as `mediabox_dvb::service::Guide` writes it.
#[derive(Deserialize)]
struct ReceiverGuide {
    token: Option<String>,
    transport_stream_id: Option<u16>,
    services: Vec<StoredService>,
    events: Vec<ReceiverEvents>,
}

#[derive(Deserialize)]
struct ReceiverEvents {
    service_id: u16,
    events: Vec<LiveEvent>,
}

#[derive(Deserialize)]
struct ReceiverTkgs { table: ReceiverTkgsTable }
#[derive(Deserialize)]
struct ReceiverTkgsTable {
    transponders: Vec<ReceiverTkgsTransponder>,
    #[serde(default)]
    orderings: Vec<StoredTkgsOrdering>,
}
#[derive(Deserialize)]
struct ReceiverTkgsTransponder {
    delivery: ReceiverTkgsDelivery,
    services: Vec<ReceiverTkgsService>,
}
#[derive(Deserialize)]
struct ReceiverTkgsDelivery { frequency_khz: u32, symbol_rate_ksps: u32, polarisation: String }
#[derive(Deserialize)]
struct ReceiverTkgsService {
    service_id: u16,
    lcn: u16,
    name: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    service_type: Option<u8>,
    #[serde(default)]
    free_ca_mode: bool,
}

/// Events that ended this long ago are let go of.
const GUIDE_KEEPS_ENDED_S: i64 = 3 * 3600;

pub fn now_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Every multiplex the receiver has been on, in the order it was first on
/// them. Until the TKGS channel search is in, this is the whole of the
/// channel list: what the broadcasts themselves said, nothing scanned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideStore {
    #[serde(default)]
    pub multiplexes: Vec<StoredGuide>,
    #[serde(default)]
    pub tkgs: Option<StoredTkgs>,
    #[serde(default = "default_hd_first")]
    pub tkgs_hd_first: bool,
}

impl Default for GuideStore {
    fn default() -> Self {
        Self { multiplexes: Vec::new(), tkgs: None, tkgs_hd_first: true }
    }
}

fn default_hd_first() -> bool { true }

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredTkgs {
    pub updated: i64,
    pub transponders: usize,
    pub channels: Vec<StoredTkgsChannel>,
    #[serde(default)]
    pub orderings: Vec<StoredTkgsOrdering>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredTkgsChannel {
    pub lcn: u16,
    pub service_id: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub service_type: Option<u8>,
    pub source: LiveSource,
    pub free_ca_mode: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredTkgsOrdering { pub name: String, pub lcns: Vec<u16> }

impl GuideStore {
    /// The file, in this form or in the one-multiplex form it had first.
    fn parse(text: &str) -> Option<Self> {
        if let Ok(store) = serde_json::from_str::<GuideStore>(text)
            && !store.multiplexes.is_empty()
        {
            return Some(store);
        }
        serde_json::from_str::<StoredGuide>(text)
            .ok()
            .map(|one| GuideStore { multiplexes: vec![one], tkgs: None, tkgs_hd_first: true })
    }

    /// The channels of every multiplex, numbered on from one multiplex to
    /// the next, each multiplex's in service-id order. Numbers are
    /// MediaBox's until TKGS gives the real ones. A service id another
    /// multiplex already has is left out: the interface knows a channel by
    /// its service id.
    pub fn channels(&self) -> Vec<LiveChannel> {
        if let Some(tkgs) = &self.tkgs {
            let wanted = if self.tkgs_hd_first { "hd" } else { "sd" };
            let ordering = tkgs.orderings.iter().find(|o| o.name.to_lowercase().contains(wanted));
            let positions: std::collections::BTreeMap<u16, usize> = ordering
                .into_iter().flat_map(|o| o.lcns.iter().copied().enumerate().map(|(i, lcn)| (lcn, i))).collect();
            let mut listed: Vec<&StoredTkgsChannel> = tkgs.channels.iter().collect();
            listed.sort_by_key(|c| (positions.get(&c.lcn).copied().unwrap_or(usize::MAX), c.lcn));
            return listed.into_iter().filter_map(|c| {
                (is_television(c.service_type) && !c.name.trim().is_empty()).then(|| LiveChannel {
                    number: c.lcn as u32, service_id: c.service_id, name: c.name.clone(),
                    provider: c.provider.clone(), source: c.source.clone(), scrambled: c.free_ca_mode,
                })
            }).collect();
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for mux in &self.multiplexes {
            let mut services: Vec<&StoredService> = mux
                .services
                .iter()
                .filter(|s| is_television(s.service_type) && !s.name.trim().is_empty())
                .collect();
            services.sort_by_key(|s| s.service_id);
            for s in services {
                if !seen.insert(s.service_id) {
                    continue;
                }
                out.push(LiveChannel {
                    number: out.len() as u32 + 1,
                    service_id: s.service_id,
                    name: s.name.clone(),
                    provider: s.provider.clone(),
                    source: mux.source.clone(),
                    scrambled: false,
                });
            }
        }
        out
    }

    /// The receiver's account of its multiplex, into the multiplex it is: the
    /// one with its transport stream id, else the one from the same source,
    /// else a new one.
    fn absorb(&mut self, receiver: ReceiverGuide, source: &LiveSource, now: i64) -> bool {
        let at = self
            .multiplexes
            .iter()
            .position(|m| receiver.transport_stream_id.is_some() && m.transport_stream_id == receiver.transport_stream_id)
            .or_else(|| self.multiplexes.iter().position(|m| m.source == *source));
        let at = match at {
            Some(at) => at,
            None => {
                self.multiplexes.push(StoredGuide::empty(source.clone()));
                self.multiplexes.len() - 1
            }
        };
        self.multiplexes[at].absorb(receiver, source, now)
    }

    fn absorb_tkgs(&mut self, receiver: ReceiverTkgs, now: i64) {
        let mut listed = Vec::new();
        let transponder_count = receiver.table.transponders.len();
        for transponder in receiver.table.transponders {
            let polarisation = match transponder.delivery.polarisation.as_str() {
                "horizontal" => SatellitePolarisation::Horizontal,
                "vertical" => SatellitePolarisation::Vertical,
                _ => continue,
            };
            let source = LiveSource::Satellite {
                frequency_khz: transponder.delivery.frequency_khz,
                symbol_rate_ksps: transponder.delivery.symbol_rate_ksps,
                polarisation,
            };
            let incoming: std::collections::BTreeSet<u16> =
                transponder.services.iter().map(|service| service.service_id).collect();
            // TKGS's embedded SDT copies use a table-local sequence in the
            // TSID field. Match the real guide by delivery parameters so an
            // existing multiplex's EIT/SDT data is preserved.
            let at = self.multiplexes.iter()
                .position(|m| m.source == source)
                .unwrap_or_else(|| {
                    self.multiplexes.push(StoredGuide::empty(source.clone()));
                    self.multiplexes.len() - 1
                });
            let mux = &mut self.multiplexes[at];
            if mux.transport_stream_id.is_some()
                && !mux.services.iter().any(|service| incoming.contains(&service.service_id))
            {
                // A wide acquisition window may have caught a neighbouring
                // carrier.  Do not retain that mux under this TKGS source.
                mux.transport_stream_id = None;
                mux.services.clear();
                mux.events.clear();
                mux.rates.clear();
                mux.carrier_offset_khz = None;
            }
            mux.source = source.clone();
            mux.updated = now;
            for service in transponder.services {
                if let Some(existing) = mux.services.iter_mut().find(|s| s.service_id == service.service_id) {
                    if !service.name.trim().is_empty() { existing.name = service.name.clone(); }
                    if service.provider.is_some() { existing.provider = service.provider.clone(); }
                    if service.service_type.is_some() { existing.service_type = service.service_type; }
                } else {
                    mux.services.push(StoredService {
                        service_id: service.service_id, name: service.name.clone(),
                        provider: service.provider.clone(), service_type: service.service_type,
                    });
                }
                listed.push(StoredTkgsChannel {
                    lcn: service.lcn,
                    service_id: service.service_id,
                    name: service.name,
                    provider: service.provider,
                    service_type: service.service_type,
                    source: source.clone(),
                    free_ca_mode: service.free_ca_mode,
                });
            }
        }
        listed.sort_by_key(|c| c.lcn);
        listed.dedup_by_key(|c| c.lcn);
        self.tkgs = Some(StoredTkgs {
            updated: now, transponders: transponder_count, channels: listed, orderings: receiver.table.orderings,
        });
    }

    /// The multiplex reached by `source`.
    pub fn multiplex(&self, source: &LiveSource) -> Option<&StoredGuide> {
        self.multiplexes.iter().find(|m| m.source == *source)
    }

    /// What a tune of `source` taught: the offset it locked at, and the
    /// rate of the service it played. True when anything changed.
    fn learn(&mut self, source: &LiveSource, offset_khz: Option<i32>, rate: Option<(u16, f64)>) -> bool {
        let Some(mux) = self.multiplexes.iter_mut().find(|m| m.source == *source) else { return false };
        let mut changed = false;
        if offset_khz.is_some() && mux.carrier_offset_khz != offset_khz {
            mux.carrier_offset_khz = offset_khz;
            changed = true;
        }
        if let Some((sid, rate)) = rate
            && mux.rates.get(&sid).is_none_or(|known| (known - rate).abs() > 0.001)
        {
            mux.rates.insert(sid, rate);
            changed = true;
        }
        changed
    }

    /// The events of every channel that overlap `from..to`.
    pub fn window(&self, from: i64, to: i64) -> std::collections::BTreeMap<u16, Vec<LiveEvent>> {
        let mut out = std::collections::BTreeMap::new();
        for mux in &self.multiplexes {
            for (sid, events) in mux.window(from, to) {
                out.entry(sid).or_insert(events);
            }
        }
        out
    }
}

impl StoredGuide {
    /// Merge what the receiver said of this multiplex: an event it now says
    /// overlaps one kept replaces it. The source is the latest one it was
    /// reached by -- the recorded multiplex, tuned live, is the live one.
    /// True when anything changed.
    fn absorb(&mut self, receiver: ReceiverGuide, source: &LiveSource, now: i64) -> bool {
        let before = self.clone();
        self.source = source.clone();
        if receiver.transport_stream_id.is_some() {
            self.transport_stream_id = receiver.transport_stream_id;
        }
        if !receiver.services.is_empty() {
            self.services = receiver.services;
        }
        for incoming in receiver.events {
            let kept = self.events.entry(incoming.service_id).or_default();
            for event in incoming.events {
                kept.retain(|other| {
                    other.start == event.start || other.end() <= event.start || other.start >= event.end()
                });
                match kept.iter_mut().find(|other| other.start == event.start) {
                    Some(other) => *other = event,
                    None => kept.push(event),
                }
            }
            kept.sort_by_key(|e| e.start);
        }
        for kept in self.events.values_mut() {
            kept.retain(|e| e.end() >= now - GUIDE_KEEPS_ENDED_S);
        }
        self.events.retain(|_, kept| !kept.is_empty());
        let changed = *self != StoredGuide { updated: self.updated, ..before };
        if changed {
            self.updated = now;
        }
        changed
    }

    fn empty(source: LiveSource) -> Self {
        Self {
            source,
            transport_stream_id: None,
            updated: 0,
            services: Vec::new(),
            events: Default::default(),
            carrier_offset_khz: None,
            rates: Default::default(),
        }
    }

    /// The events of every channel that overlap `from..to`.
    pub fn window(&self, from: i64, to: i64) -> std::collections::BTreeMap<u16, Vec<LiveEvent>> {
        self.events
            .iter()
            .map(|(&sid, events)| {
                (sid, events.iter().filter(|e| e.end() > from && e.start < to).cloned().collect::<Vec<_>>())
            })
            .filter(|(_, events)| !events.is_empty())
            .collect()
    }
}

impl LiveManager {
    /// The guide as kept on disk, if there is one.
    pub fn stored_guide(&self) -> Option<GuideStore> {
        GuideStore::parse(&std::fs::read_to_string(&self.paths.guide_store).ok()?)
    }

    /// Replace a legacy/incomplete satellite source with the exact TKGS
    /// delivery for this service. Frequency and symbol rate are also matched
    /// because service ids are only unique within a transport stream.
    fn canonical_source(&self, service_id: u16, source: &LiveSource) -> LiveSource {
        let LiveSource::Satellite { frequency_khz, symbol_rate_ksps, .. } = source else {
            return source.clone();
        };
        self.stored_guide()
            .and_then(|guide| guide.tkgs)
            .and_then(|tkgs| {
                tkgs.channels.into_iter().find(|channel| {
                    channel.service_id == service_id
                        && matches!(
                            channel.source,
                            LiveSource::Satellite {
                                frequency_khz: frequency,
                                symbol_rate_ksps: rate,
                                ..
                            } if frequency == *frequency_khz && rate == *symbol_rate_ksps
                        )
                })
            })
            .map_or_else(|| source.clone(), |channel| channel.source)
    }

    fn keep_guide(&self, stored: &GuideStore) -> Result<(), String> {
        let text = serde_json::to_string(stored).map_err(|error| format!("kanal listesi yazılamadı: {error}"))?;
        let temporary = self.paths.guide_store.with_extension("json.tmp");
        std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, &self.paths.guide_store))
            .map_err(|error| format!("kanal listesi yazılamadı: {error}"))
    }

    pub async fn update_tkgs(&self) -> Result<Value, String> {
        let value = self.read_tkgs().await?;
        let receiver: ReceiverTkgs = serde_json::from_value(value).map_err(|error| format!("TKGS sonucu okunamadı: {error}"))?;
        let mut stored = self.stored_guide().unwrap_or_default();
        stored.absorb_tkgs(receiver, now_utc());
        self.keep_guide(&stored)?;
        let tkgs = stored.tkgs.as_ref().expect("TKGS was just stored");
        Ok(json!({"channels": tkgs.channels.len(), "transponders": tkgs.transponders, "updated": tkgs.updated}))
    }

    pub fn set_tkgs_ordering(&self, hd_first: bool) -> Result<Value, String> {
        let mut stored = self.stored_guide().unwrap_or_default();
        stored.tkgs_hd_first = hd_first;
        self.keep_guide(&stored)?;
        Ok(json!({"hd_first": hd_first}))
    }

    /// Take in what this start's receiver has said of its multiplex; written
    /// to the store when it changed anything.
    pub fn absorb_guide(&self) -> bool {
        let (token, source, service_id) = {
            let inner = self.lock();
            match (inner.token.clone(), inner.source.clone()) {
                (Some(token), Some(source)) => (token, source, inner.service_id),
                _ => return false,
            }
        };
        let receiver = std::fs::read_to_string(self.paths.guide())
            .ok()
            .and_then(|text| serde_json::from_str::<ReceiverGuide>(&text).ok())
            .filter(|guide| guide.token.as_deref() == Some(token.as_str()))
            .filter(|guide| service_id.is_none_or(|wanted| {
                guide.services.iter().any(|service| service.service_id == wanted)
            }));
        let mut stored = self.stored_guide().unwrap_or_default();
        let mut changed = receiver.is_some_and(|receiver| stored.absorb(receiver, &source, now_utc()));
        // What the tune taught, once the receiver streams: where the
        // carrier locked, and the rate of the service on now.
        if let Some(status) = self.read_status()
            && status.get("state").and_then(Value::as_str) == Some("streaming")
            && status.pointer("/selection/pmt_pid").is_some_and(|pid| !pid.is_null())
        {
            let offset = status.pointer("/tune/search_offset_khz").and_then(Value::as_i64).map(|o| o as i32);
            let sid = status.get("service_id").and_then(Value::as_u64).map(|sid| sid as u16);
            let rate = sid.zip(self.output_rate());
            changed |= stored.learn(&source, offset, rate);
        }
        if !changed {
            return false;
        }
        if let Err(error) = self.keep_guide(&stored) {
            eprintln!("mediaboxd-rs: live guide not kept: {error}");
            return false;
        }
        true
    }

    /// `LiveChannels`.
    pub fn channels(&self) -> Value {
        match self.stored_guide() {
            Some(guide) => json!({
                "channels": guide.channels(),
                "multiplexes": guide.multiplexes.len(),
                "updated": guide.tkgs.as_ref().map(|t| t.updated).or_else(|| guide.multiplexes.iter().map(|m| m.updated).max()),
                "tkgs": guide.tkgs.as_ref().map(|t| json!({
                    "channels": t.channels.len(), "transponders": t.transponders,
                    "updated": t.updated, "hd_first": guide.tkgs_hd_first,
                })),
            }),
            None => json!({"channels": [], "multiplexes": 0, "updated": null, "tkgs": null}),
        }
    }

    /// `LiveGuide`: two days at most.
    pub fn guide_window(&self, from: i64, to: i64) -> Value {
        let to = to.min(from.saturating_add(2 * 86_400));
        let events = self.stored_guide().map(|guide| guide.window(from, to)).unwrap_or_default();
        json!({"now": now_utc(), "from": from, "to": to, "events": events})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(dir: &std::path::Path) -> LiveManager {
        LiveManager::new(LivePaths {
            binary: "/opt/rk3588-mediabox/bin/mediabox-dvb".into(),
            run_dir: dir.into(),
            recording: "/var/lib/mediabox-dvb/recording.ts".into(),
            firmware_dir: "/var/lib/mediabox-dvb/firmware".into(),
            guide_store: dir.join("live-guide.json"),
        })
    }

    #[test]
    fn the_receiver_is_a_bound_unit_with_typed_arguments_only() {
        let dir = tempfile::tempdir().unwrap();
        let live = manager(dir.path());
        let satellite = LiveSource::Satellite {
            frequency_khz: 12_188_000, symbol_rate_ksps: 27_500,
            polarisation: SatellitePolarisation::Vertical,
        };
        let argv = live.argv(10801, &satellite, "7-1");
        let joined = argv.join(" ");
        assert!(joined.contains("--unit=mediabox-dvb.service"));
        assert!(joined.contains("--property=BindsTo=mediabox-tv-ui.service"));
        assert!(joined.contains("--property=RuntimeDirectoryPreserve=yes"));
        assert!(joined.ends_with(&format!(
            "play --service 10801 --fifo {0}/live.ts --status {0}/status.json --token 7-1 \
             --hardware --transponder 12188000 --sr 27500 --polarisation=vertical --firmware-dir /var/lib/mediabox-dvb/firmware --reconnect-s 20 --hold-reader-s 30",
            dir.path().display()
        )));
        let recording = live.argv(10801, &LiveSource::Recording, "7-2").join(" ");
        assert!(recording.ends_with("--recording /var/lib/mediabox-dvb/recording.ts --pace"));
        assert!(!recording.contains("--hardware"), "a recording never opens the receiver");
        assert!(!recording.contains("--polarisation"));
    }

    #[test]
    fn a_status_left_by_another_start_is_not_this_ones() {
        let dir = tempfile::tempdir().unwrap();
        let live = manager(dir.path());
        std::fs::write(dir.path().join("status.json"), r#"{"token":"1-1","state":"failed","end":"no_lock"}"#).unwrap();
        assert!(live.read_status().is_none(), "no start of ours yet");
        live.lock().token = Some("1-2".into());
        assert!(live.read_status().is_none(), "an earlier start's status");
        std::fs::write(dir.path().join("status.json"), r#"{"token":"1-2","state":"streaming"}"#).unwrap();
        assert_eq!(live.read_status().unwrap()["state"], "streaming");
    }

    #[tokio::test]
    async fn the_end_of_a_recording_is_natural_and_a_lost_receiver_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let live = manager(dir.path());
        live.lock().token = Some("1-3".into());
        std::fs::write(dir.path().join("status.json"), r#"{"token":"1-3","state":"ended","end":"recording_end"}"#).unwrap();
        assert_eq!(live.final_end().await, Some(SourceEnd { natural: true, message: None }));
        std::fs::write(
            dir.path().join("status.json"),
            r#"{"token":"1-3","state":"failed","end":"usb_disconnected","message":"cihaz geri gelmedi"}"#,
        )
        .unwrap();
        let end = live.final_end().await.unwrap();
        assert!(!end.natural);
        assert_eq!(end.message.as_deref(), Some("Uydu alıcısının USB bağlantısı koptu. (cihaz geri gelmedi)"));
        assert_eq!(live.lifecycle(), Lifecycle::Failed);
    }

    #[test]
    fn the_rate_to_match_is_the_field_rate_of_an_interlaced_stream() {
        let dir = tempfile::tempdir().unwrap();
        let live = manager(dir.path());
        live.lock().token = Some("1-5".into());
        let write = |video: &str| {
            std::fs::write(
                dir.path().join("status.json"),
                format!(r#"{{"token":"1-5","state":"streaming","selection":{{"video":{video}}}}}"#),
            )
            .unwrap()
        };
        write(r#"{"interlaced":true,"frame_rate":[25,1],"field_rate":[50,1]}"#);
        assert_eq!(live.output_rate(), Some(50.0));
        write(r#"{"interlaced":false,"frame_rate":[60000,1001],"field_rate":null}"#);
        assert!((live.output_rate().unwrap() - 59.94).abs() < 0.01);
        write("null");
        assert_eq!(live.output_rate(), None);
    }

    #[tokio::test]
    async fn the_player_going_is_not_a_receiver_fault() {
        let dir = tempfile::tempdir().unwrap();
        let live = manager(dir.path());
        live.lock().token = Some("1-4".into());
        std::fs::write(dir.path().join("status.json"), r#"{"token":"1-4","state":"ended","end":"player_closed"}"#).unwrap();
        let end = live.final_end().await.unwrap();
        assert!(!end.natural, "a live film whose player went away has not ended");
        assert_eq!(end.message.as_deref(), Some("Oynatıcı kapandı."));
        assert_eq!(live.lifecycle(), Lifecycle::Idle, "but the receiver did not fail");
        assert!(live.status()["failure"].is_null());
    }

    fn event(start: i64, minutes: u32, title: &str) -> LiveEvent {
        LiveEvent { start, duration: minutes * 60, title: title.into(), text: String::new() }
    }

    #[test]
    fn the_receivers_guide_is_kept_and_its_channels_are_the_television_services() {
        let dir = tempfile::tempdir().unwrap();
        let live = manager(dir.path());
        let source = LiveSource::Satellite {
            frequency_khz: 12_188_000, symbol_rate_ksps: 27_500,
            polarisation: SatellitePolarisation::Vertical,
        };
        {
            let mut inner = live.lock();
            inner.token = Some("1-6".into());
            inner.source = Some(source.clone());
        }
        let now = now_utc();
        let receiver = json!({
            "token": "1-6",
            "transport_stream_id": 41306,
            "services": [
                {"service_id": 10810, "name": "TRT World Radio", "service_type": 10, "free_ca_mode": false},
                {"service_id": 10801, "name": "TRT HABER HD", "provider": "TRT", "service_type": 25, "free_ca_mode": false},
                {"service_id": 10800, "name": "TRT1 HD", "provider": "TRT", "service_type": 25, "free_ca_mode": false},
            ],
            "events": [{"service_id": 10801, "events": [
                {"event_id": 1, "start": now - 600, "duration": 3600, "title": "Haber", "running": 4},
                {"event_id": 2, "start": now - 20_000, "duration": 600, "title": "Long gone", "running": 0},
            ]}],
        });
        std::fs::write(dir.path().join("guide.json"), receiver.to_string()).unwrap();
        assert!(live.absorb_guide());
        assert!(!live.absorb_guide(), "nothing new the second time");
        let channels = live.channels();
        let names: Vec<_> = channels["channels"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["TRT1 HD", "TRT HABER HD"], "radio is not a channel; numbered by service id");
        assert_eq!(channels["channels"][1]["number"], 2);
        let window = live.guide_window(now - 3600, now + 3600);
        assert_eq!(window["events"]["10801"][0]["title"], "Haber");
        assert_eq!(window["events"]["10801"].as_array().unwrap().len(), 1, "an event long ended is let go of");
        // Another start's guide is not this one's.
        live.lock().token = Some("1-7".into());
        assert!(!live.absorb_guide());
    }

    fn receiver(tsid: u16, services: &[(u16, &str)]) -> ReceiverGuide {
        ReceiverGuide {
            token: None,
            transport_stream_id: Some(tsid),
            services: services
                .iter()
                .map(|&(service_id, name)| StoredService {
                    service_id,
                    name: name.into(),
                    provider: None,
                    service_type: Some(0x19),
                })
                .collect(),
            events: vec![],
        }
    }

    #[test]
    fn every_multiplex_tuned_is_kept_and_their_channels_numbered_on() {
        let s12188 = LiveSource::Satellite {
            frequency_khz: 12_188_000, symbol_rate_ksps: 27_500,
            polarisation: SatellitePolarisation::Vertical,
        };
        let s11794 = LiveSource::Satellite {
            frequency_khz: 11_794_000, symbol_rate_ksps: 30_000,
            polarisation: SatellitePolarisation::Vertical,
        };
        let mut store = GuideStore::default();
        // The recording first; the same multiplex tuned live takes its place.
        assert!(store.absorb(receiver(41306, &[(10801, "TRT HABER HD"), (10800, "TRT1 HD")]), &LiveSource::Recording, 0));
        assert!(store.absorb(receiver(41306, &[(10801, "TRT HABER HD"), (10800, "TRT1 HD")]), &s12188, 0));
        assert_eq!(store.multiplexes.len(), 1);
        assert_eq!(store.multiplexes[0].source, s12188);
        assert!(store.absorb(receiver(40301, &[(10601, "TRT 1"), (10800, "Duplicate")]), &s11794, 0));
        let channels = store.channels();
        let shown: Vec<_> = channels.iter().map(|c| (c.number, c.service_id, c.source == s11794)).collect();
        assert_eq!(shown, [(1, 10800, false), (2, 10801, false), (3, 10601, true)], "a service id already listed is left out");
        // The file it had first is still read.
        let old = serde_json::to_string(&store.multiplexes[0]).unwrap();
        assert_eq!(GuideStore::parse(&old).unwrap().multiplexes.len(), 1);
    }

    #[test]
    fn tkgs_repairs_polarisation_from_an_already_running_legacy_ui() {
        let dir = tempfile::tempdir().unwrap();
        let live = manager(dir.path());
        let exact = LiveSource::Satellite {
            frequency_khz: 12_345_000,
            symbol_rate_ksps: 30_000,
            polarisation: SatellitePolarisation::Horizontal,
        };
        let mut store = GuideStore::default();
        store.multiplexes.push(StoredGuide::empty(exact.clone()));
        store.tkgs = Some(StoredTkgs {
            updated: 1,
            transponders: 1,
            channels: vec![StoredTkgsChannel {
                lcn: 42,
                service_id: 52202,
                name: "H CHANNEL".into(),
                provider: None,
                service_type: Some(0x19),
                source: exact.clone(),
                free_ca_mode: false,
            }],
            orderings: vec![],
        });
        std::fs::write(dir.path().join("live-guide.json"), serde_json::to_string(&store).unwrap()).unwrap();
        let legacy = LiveSource::Satellite {
            frequency_khz: 12_345_000,
            symbol_rate_ksps: 30_000,
            polarisation: SatellitePolarisation::Vertical,
        };
        assert_eq!(live.canonical_source(52202, &legacy), exact);
    }

    #[test]
    fn a_rescheduled_event_replaces_the_one_it_overlaps() {
        let source = LiveSource::Recording;
        let mut stored = StoredGuide::empty(source.clone());
        let now = 1_000_000;
        let first = ReceiverGuide {
            token: None,
            transport_stream_id: Some(1),
            services: vec![],
            events: vec![ReceiverEvents { service_id: 5, events: vec![event(now, 60, "A"), event(now + 3600, 60, "B")] }],
        };
        assert!(stored.absorb(first, &source, now));
        let moved = ReceiverGuide {
            token: None,
            transport_stream_id: Some(1),
            services: vec![],
            events: vec![ReceiverEvents { service_id: 5, events: vec![event(now + 1800, 60, "B'")] }],
        };
        assert!(stored.absorb(moved, &source, now));
        let titles: Vec<_> = stored.events[&5].iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["B'"]);
    }

    #[test]
    fn a_tune_teaches_the_carrier_offset_and_the_channel_rate() {
        let dir = tempfile::tempdir().unwrap();
        let live = manager(dir.path());
        let satellite = LiveSource::Satellite {
            frequency_khz: 12_188_000, symbol_rate_ksps: 27_500,
            polarisation: SatellitePolarisation::Vertical,
        };
        let mut store = GuideStore::default();
        assert!(store.absorb(receiver(41306, &[(10801, "TRT HABER HD")]), &satellite, 0));
        assert!(store.learn(&satellite, Some(10_000), Some((10801, 50.0))));
        assert!(!store.learn(&satellite, Some(10_000), Some((10801, 50.0))), "nothing new");
        std::fs::write(dir.path().join("live-guide.json"), serde_json::to_string(&store).unwrap()).unwrap();
        // The next start tries where it locked.
        assert!(live.argv(10801, &satellite, "1-1").contains(&"--carrier-first-khz=10000".to_string()));
        {
            let mut inner = live.lock();
            inner.source = Some(satellite.clone());
            inner.service_id = Some(10801);
        }
        assert_eq!(live.known_rate(10801), Some(50.0));
        assert_eq!(live.known_rate(10800), None);
    }
}

