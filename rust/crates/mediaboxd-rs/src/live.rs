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

use mediabox_core::{LiveChannel, LiveEvent, LiveScanKeep, LiveSource, SatellitePolarisation};
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
    /// Up, booted and locked, its stream stopped: reading TKGS for
    /// `update_tkgs` while no channel plays.
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
    /// The channel setup reading TKGS, while it does.
    setup: Option<Setup>,
    /// The DiSEqC port the receiver up now was started with.
    diseqc_port: Option<u8>,
    /// The manual scan form's signal reading: how to put the receiver back,
    /// and the carrier it is on.
    probe: Option<(ScanRestore, LiveSource)>,
}

/// How far the channel setup has got, for its progress, and whether it was
/// called off.
#[derive(Debug, Clone, Copy, Default)]
struct Setup {
    /// A manual scan, not the TKGS read.
    scan: bool,
    /// The receiver is on the TKGS multiplex and reading the table.
    reading: bool,
    sections: u64,
    expected: u64,
    /// A scan: the services its SDT has named so far, and the demodulator's
    /// word on the carrier.
    found: u64,
    locked: bool,
    strength_pct: Option<u64>,
    snr_db: Option<f64>,
    /// A satellite scan: the carrier on now (from 1) of `carriers`, and
    /// the channels it has added so far.
    carrier: u64,
    carriers: u64,
    frequency_khz: u64,
    added: u64,
    /// The channels the carriers before this one gave.
    found_before: u64,
    /// The named services found, by AOSP's service types (TvContract
    /// SERVICE_TYPE_AUDIO_VIDEO, _AUDIO, _OTHER): on the carrier on now,
    /// and on those before it.
    kinds: [u64; 3],
    kinds_before: [u64; 3],
    cancelled: bool,
}

/// Türksat 42°E's home carrier, where its NIT and TKGS are read.
const HOME_CARRIER: LiveSource =
    LiveSource::Satellite { frequency_khz: 12_380_000, symbol_rate_ksps: 27_500, polarisation: SatellitePolarisation::Vertical };

/// What a scan keeps: Sony's Scan type (All, Only Free, Only Encrypted)
/// and Store type (All, Only Digital, Only Radio).
#[derive(Debug, Clone, Copy)]
pub struct ScanFilter {
    pub free_only: bool,
    pub scrambled_only: bool,
    pub keep: LiveScanKeep,
}

/// How the receiver is put back after a scan.
/// A satellite scan's account of the transport streams it has had: a
/// carrier that locks one the home carrier's NIT puts on another frequency
/// has locked its neighbour (the receiver searches ±10 MHz around the
/// nominal IF); of two carriers that lock the same one, the one locked
/// further from its nominal frequency has.
#[derive(Default)]
struct CarrierCheck {
    /// The NIT's carriers and their (original network, transport stream).
    network: Vec<(LiveSource, (u16, u16))>,
    /// Each transport stream had: on which carrier, how far off nominal.
    had: Vec<((Option<u16>, u16), LiveSource, u32)>,
}

impl CarrierCheck {
    /// Ok with the carrier this one shows to have been a neighbour's lock,
    /// if one was taken before; Err when this one is.
    fn carrier(&mut self, source: &LiveSource, onid: Option<u16>, tsid: Option<u16>, offset_khz: u32) -> Result<Option<LiveSource>, String> {
        let LiveSource::Satellite { frequency_khz, polarisation, .. } = *source else { return Ok(None) };
        let Some(tsid) = tsid else { return Ok(None) };
        let near = |other: &LiveSource| matches!(*other, LiveSource::Satellite { frequency_khz: f, polarisation: p, .. }
            if p == polarisation && f.abs_diff(frequency_khz) <= 3_000);
        let same = |(n, t): (u16, u16)| t == tsid && onid.is_none_or(|onid| onid == n);
        let named: Vec<&LiveSource> = self.network.iter().filter(|(_, ids)| same(*ids)).map(|(s, _)| s).collect();
        let neighbour = || format!("{} MHz: komşu taşıyıcı kilitlendi (TSID {tsid}).", frequency_khz / 1000);
        if !named.is_empty() && !named.iter().any(|s| near(s)) {
            return Err(neighbour());
        }
        match self.had.iter_mut().find(|(ids, _, _)| *ids == (onid, tsid)) {
            Some((_, _, before)) if *before <= offset_khz => Err(neighbour()),
            Some((_, carrier, before)) => {
                *before = offset_khz;
                Ok(Some(std::mem::replace(carrier, source.clone())))
            }
            None => {
                self.had.push(((onid, tsid), source.clone(), offset_khz));
                Ok(None)
            }
        }
    }
}

struct ScanRestore {
    started: bool,
    playing: Option<(Option<u16>, Option<LiveSource>)>,
}

pub struct LiveManager {
    paths: LivePaths,
    inner: Mutex<Inner>,
    starts: AtomicU64,
}

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
                diseqc_port: None,
                probe: None,
                setup: None,
            }),
            starts: AtomicU64::new(0),
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
                // The receiver itself, chosen explicitly, and the LNB's
                // DiSEqC port from the setup.
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
                let stored = self.stored_guide();
                if let Some(offset) = stored.as_ref().and_then(|guide| guide.multiplex(source)?.carrier_offset_khz) {
                    argv.push(format!("--carrier-first-khz={offset}"));
                }
                if let Some(port) = stored.map_or(default_diseqc_port(), |guide| guide.diseqc_port) {
                    argv.push(format!("--diseqc-port={port}"));
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
        let port = self.stored_guide().map_or(default_diseqc_port(), |guide| guide.diseqc_port);
        let token = format!("{}-{}", std::process::id(), self.starts.fetch_add(1, Ordering::SeqCst) + 1);
        {
            let mut inner = self.lock();
            inner.service_id = Some(service_id);
            inner.source = Some(source.clone());
            inner.token = Some(token.clone());
            inner.failure = None;
            inner.last = None;
            inner.fifo = None;
            inner.diseqc_port = port;
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
        let port = self.stored_guide().map_or(default_diseqc_port(), |guide| guide.diseqc_port);
        {
            let inner = self.lock();
            if inner.diseqc_port != port {
                return false;
            }
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

    /// Wait until the receiver says `done`, or has ended; given up as soon
    /// as `called_off` says a newer request has
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
                    Some("reading") => {
                        if let Some(setup) = self.lock().setup.as_mut() {
                            setup.reading = true;
                            setup.sections = value.get("sections").and_then(Value::as_u64).unwrap_or(0);
                            setup.expected = value.get("expected").and_then(Value::as_u64).unwrap_or(0);
                        }
                    }
                    _ => {}
                }
            }
            if self.lock().setup.is_some_and(|setup| setup.cancelled) {
                break Err("Kanal kurulumu iptal edildi.".into());
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
            "setup": self.setup_progress_of(&inner),
        })
    }

    fn setup_progress_of(&self, inner: &Inner) -> Value {
        match inner.setup {
            Some(setup) => json!({
                "running": true, "scan": setup.scan, "reading": setup.reading,
                "sections": setup.sections, "expected": setup.expected,
                "found": setup.found, "locked": setup.locked,
                "strength_pct": setup.strength_pct, "snr_db": setup.snr_db,
                "carrier": setup.carrier, "carriers": setup.carriers,
                "frequency_khz": setup.frequency_khz, "added": setup.added,
                "found_total": setup.found_before + setup.found,
                "television_total": setup.kinds_before[0] + setup.kinds[0],
                "radio_total": setup.kinds_before[1] + setup.kinds[1],
                "data_total": setup.kinds_before[2] + setup.kinds[2],
            }),
            None => json!({"running": false}),
        }
    }

    /// The channel setup's progress: whether it runs, whether the receiver
    /// is reading the table yet, and its sections had and expected.
    pub fn setup_progress(&self) -> Value {
        let inner = self.lock();
        self.setup_progress_of(&inner)
    }

    /// Call the channel setup off. False when none runs.
    pub fn cancel_setup(&self) -> bool {
        match self.lock().setup.as_mut() {
            Some(setup) => {
                setup.cancelled = true;
                true
            }
            None => false,
        }
    }
}

// ------------------------------------------------------------------ guide

/// Playable service types (EN 300 468 table 87): digital television,
/// digital radio, MPEG-2 HD, and advanced codec SD/HD/UHD. Private data
/// services such as TKGS itself stay out of the channel list.
fn is_playable(service_type: Option<u8>) -> bool {
    matches!(service_type, Some(0x01 | 0x02 | 0x11 | 0x16 | 0x19 | 0x1F))
}

/// AOSP's service type of an EN 300 468 one (table 87): 0 television
/// (SERVICE_TYPE_AUDIO_VIDEO), 1 radio (SERVICE_TYPE_AUDIO), 2 data and
/// the rest (SERVICE_TYPE_OTHER).
fn service_kind(service_type: Option<u8>) -> usize {
    match service_type {
        Some(0x01 | 0x11 | 0x16 | 0x19 | 0x1F | 0x20) => 0,
        Some(0x02 | 0x07 | 0x0A) => 1,
        _ => 2,
    }
}

/// Digital radio (EN 300 468 table 87): sound, no picture.
fn is_radio(service_type: Option<u8>) -> bool {
    service_type == Some(0x02)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredService {
    pub service_id: u16,
    pub name: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub service_type: Option<u8>,
    /// EN 300 468 free_CA_mode: scrambled.
    #[serde(default)]
    pub free_ca_mode: bool,
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
    /// The services a manual scan of this multiplex put in the channel
    /// list, by service id: the form before the channel table, read only to
    /// be moved into it.
    #[serde(default, skip_serializing)]
    pub manual: Vec<u16>,
}

/// The receiver's `guide.json`, as `mediabox_dvb::service::Guide` writes it.
#[derive(Deserialize)]
struct ReceiverGuide {
    token: Option<String>,
    transport_stream_id: Option<u16>,
    #[serde(default)]
    original_network_id: Option<u16>,
    /// Every section of the SDT actual is in; of the NIT actual too.
    #[serde(default)]
    sdt_complete: bool,
    #[serde(default)]
    nit_complete: bool,
    /// The NIT's carriers (a network scan's list).
    #[serde(default)]
    network: Vec<ReceiverCarrier>,
    services: Vec<StoredService>,
    events: Vec<ReceiverEvents>,
}

#[derive(Deserialize)]
struct ReceiverCarrier {
    #[serde(default)]
    transport_stream_id: u16,
    #[serde(default)]
    original_network_id: u16,
    #[serde(default)]
    orbital_position_tenths: u16,
    #[serde(default)]
    east: bool,
    frequency_khz: u32,
    symbol_rate_ksps: u32,
    polarisation: String,
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
    /// Its header's text (descriptor 0x90), as Türksat words it.
    #[serde(default)]
    title: String,
    transponders: Vec<ReceiverTkgsTransponder>,
    #[serde(default)]
    orderings: Vec<StoredTkgsOrdering>,
    #[serde(default)]
    categories: Vec<StoredTkgsCategory>,
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
    #[serde(default)]
    categories: Vec<u16>,
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
    /// The channel table (AOSP TvContract.Channels): every channel the
    /// setup put in the list, TKGS's and the scans', one row each.
    #[serde(default)]
    pub channels: Vec<StoredChannel>,
    /// The DiSEqC 1.0 committed switch port the LNB is on: 0 A .. 3 D;
    /// none (null) for an LNB on its own, sent no command. Port A unless
    /// set: this box's own line answers to the switch command only.
    #[serde(default = "default_diseqc_port")]
    pub diseqc_port: Option<u8>,
    #[serde(default)]
    pub tkgs: Option<StoredTkgs>,
    #[serde(default = "default_hd_first")]
    pub tkgs_hd_first: bool,
    /// "TKGS Kapalı": the channel list is the manual scans' alone.
    #[serde(default)]
    pub tkgs_off: bool,
    /// TKGS's categories are not the channel list's groups.
    #[serde(default)]
    pub categories_hidden: bool,
}

impl Default for GuideStore {
    fn default() -> Self {
        Self {
            multiplexes: Vec::new(), channels: Vec::new(), diseqc_port: default_diseqc_port(),
            tkgs: None, tkgs_hd_first: true, tkgs_off: false, categories_hidden: false,
        }
    }
}

fn default_hd_first() -> bool { true }

fn default_diseqc_port() -> Option<u8> { Some(0) }

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredTkgs {
    /// The table's own text (its header's descriptor 0x90).
    #[serde(default)]
    pub title: String,
    pub updated: i64,
    pub transponders: usize,
    /// The form before the channel table, read only to be moved into it.
    #[serde(default, skip_serializing)]
    pub channels: Vec<StoredTkgsChannel>,
    #[serde(default)]
    pub orderings: Vec<StoredTkgsOrdering>,
    /// TKGS's channel categories (its descriptor 0x92): id and name.
    #[serde(default)]
    pub categories: Vec<StoredTkgsCategory>,
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
    /// The ids of the TKGS categories the channel is in.
    #[serde(default)]
    pub categories: Vec<u16>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredTkgsOrdering { pub name: String, pub lcns: Vec<u16> }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredTkgsCategory { pub id: u16, pub name: String }

/// The list a channel came in by (TvContract.Channels.COLUMN_CHANNEL_LIST_ID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelList {
    Tkgs,
    Scan,
}

/// A row of the channel table (TvContract.Channels): the DVB triplet
/// (COLUMN_ORIGINAL_NETWORK_ID, COLUMN_TRANSPORT_STREAM_ID,
/// COLUMN_SERVICE_ID), where it is tuned, and its number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredChannel {
    pub list: ChannelList,
    /// COLUMN_DISPLAY_NUMBER: TKGS's LCN; 0 for a scanned channel, numbered
    /// in the table's order after TKGS's.
    #[serde(default)]
    pub number: u16,
    #[serde(default)]
    pub original_network_id: Option<u16>,
    /// The transport stream's own id, once a tune or scan has read it.
    #[serde(default)]
    pub transport_stream_id: Option<u16>,
    pub service_id: u16,
    pub name: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub service_type: Option<u8>,
    pub source: LiveSource,
    pub free_ca_mode: bool,
    /// TKGS's category ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<u16>,
}

impl StoredChannel {
    fn scanned(s: &StoredService, source: &LiveSource, transport_stream_id: Option<u16>, original_network_id: Option<u16>) -> Self {
        Self {
            list: ChannelList::Scan, number: 0, original_network_id, transport_stream_id,
            service_id: s.service_id, name: s.name.clone(), provider: s.provider.clone(),
            service_type: s.service_type, source: source.clone(), free_ca_mode: s.free_ca_mode,
            categories: Vec::new(),
        }
    }

    fn listed(&self) -> bool {
        is_playable(self.service_type) && !self.name.trim().is_empty()
    }
}

impl GuideStore {
    /// The file, in this form or in the one-multiplex form it had first.
    fn parse(text: &str) -> Option<Self> {
        // The first form is one multiplex (it has a `source`); this one may
        // have none yet and still hold the setup's settings.
        if let Ok(one) = serde_json::from_str::<StoredGuide>(text) {
            return Some(GuideStore { multiplexes: vec![one], ..GuideStore::default() }.migrated());
        }
        serde_json::from_str::<GuideStore>(text).ok().map(GuideStore::migrated)
    }

    /// TKGS's list and the scans' marks of the form before, moved into the
    /// channel table; one multiplex per source.
    fn migrated(mut self) -> Self {
        let mut seen = Vec::new();
        self.multiplexes.retain(|m| {
            let first = !seen.contains(&m.source);
            seen.push(m.source.clone());
            first
        });
        let tkgs = self.tkgs.as_mut().map(|t| std::mem::take(&mut t.channels)).unwrap_or_default();
        for c in tkgs {
            let transport_stream_id = self.multiplex(&c.source).and_then(|m| m.transport_stream_id);
            self.channels.push(StoredChannel {
                list: ChannelList::Tkgs, number: c.lcn, original_network_id: None, transport_stream_id,
                service_id: c.service_id, name: c.name, provider: c.provider, service_type: c.service_type,
                source: c.source, free_ca_mode: c.free_ca_mode, categories: c.categories,
            });
        }
        let mut rows = Vec::new();
        for mux in &mut self.multiplexes {
            for sid in std::mem::take(&mut mux.manual) {
                if let Some(s) = mux.services.iter().find(|s| s.service_id == sid) {
                    rows.push(StoredChannel::scanned(s, &mux.source, mux.transport_stream_id, None));
                }
            }
        }
        for row in rows {
            if !self.channels.iter().any(|c| c.service_id == row.service_id && c.source == row.source) {
                self.channels.push(row);
            }
        }
        self
    }

    /// The channel list, from the channel table: with TKGS on, its rows in
    /// the HD or SD ordering it gives, then the scans' rows numbered on after
    /// its last number; with TKGS off, the scans' rows alone, from 1. A
    /// service id already listed is left out: the interface knows a channel
    /// by its service id.
    pub fn channels(&self) -> Vec<LiveChannel> {
        let mut out: Vec<LiveChannel> = Vec::new();
        let row = |c: &StoredChannel, number: u32, categories: Vec<String>| LiveChannel {
            number, service_id: c.service_id, name: c.name.clone(), provider: c.provider.clone(),
            source: c.source.clone(), scrambled: c.free_ca_mode, radio: is_radio(c.service_type), categories,
        };
        if let Some(tkgs) = self.tkgs.as_ref().filter(|_| !self.tkgs_off) {
            let wanted = if self.tkgs_hd_first { "hd" } else { "sd" };
            let ordering = tkgs.orderings.iter().find(|o| o.name.to_lowercase().contains(wanted));
            let positions: std::collections::BTreeMap<u16, usize> = ordering
                .into_iter().flat_map(|o| o.lcns.iter().copied().enumerate().map(|(i, lcn)| (lcn, i))).collect();
            let mut listed: Vec<&StoredChannel> =
                self.channels.iter().filter(|c| c.list == ChannelList::Tkgs && c.listed()).collect();
            listed.sort_by_key(|c| (positions.get(&c.number).copied().unwrap_or(usize::MAX), c.number));
            let category = |id: &u16| tkgs.categories.iter().find(|k| k.id == *id).map(|k| k.name.clone());
            for c in listed {
                let categories = if self.categories_hidden { Vec::new() } else { c.categories.iter().filter_map(category).collect() };
                out.push(row(c, c.number as u32, categories));
            }
        }
        let mut number = out.iter().map(|c| c.number).max().unwrap_or(0);
        for c in self.channels.iter().filter(|c| c.list == ChannelList::Scan && c.listed()) {
            if out.iter().any(|o| o.service_id == c.service_id) {
                continue;
            }
            number += 1;
            out.push(row(c, number, Vec::new()));
        }
        out
    }

    /// What a scan of `source` found: the multiplex's services as its SDT
    /// has them, and a row in the channel table for each of `kept` the table
    /// does not have yet (by its transport stream, else its carrier). The
    /// service ids of the rows added.
    fn absorb_scan(
        &mut self, source: &LiveSource, transport_stream_id: Option<u16>, original_network_id: Option<u16>,
        services: Vec<StoredService>, kept: &[u16], now: i64,
    ) -> Vec<u16> {
        let at = self
            .multiplexes
            .iter()
            .position(|m| m.source == *source)
            .unwrap_or_else(|| {
                self.multiplexes.push(StoredGuide::empty(source.clone()));
                self.multiplexes.len() - 1
            });
        let mux = &mut self.multiplexes[at];
        if transport_stream_id.is_some() {
            mux.transport_stream_id = transport_stream_id;
        }
        mux.services = services.clone();
        mux.updated = now;
        // The rows of this carrier are now known by their transport stream;
        // a scanned one the carrier no longer has is gone.
        for c in self.channels.iter_mut().filter(|c| c.source == *source) {
            c.transport_stream_id = transport_stream_id.or(c.transport_stream_id);
            c.original_network_id = original_network_id.or(c.original_network_id);
        }
        self.channels.retain(|c| {
            c.list != ChannelList::Scan || c.source != *source || services.iter().any(|s| s.service_id == c.service_id)
        });
        let mut added = Vec::new();
        let mut order: Vec<&StoredService> = services.iter().filter(|s| kept.contains(&s.service_id)).collect();
        order.sort_by_key(|s| s.service_id);
        for s in order {
            let present = self.channels.iter().any(|c| {
                c.service_id == s.service_id
                    && (c.source == *source || (transport_stream_id.is_some() && c.transport_stream_id == transport_stream_id))
            });
            if !present {
                self.channels.push(StoredChannel::scanned(s, source, transport_stream_id, original_network_id));
                added.push(s.service_id);
            }
        }
        added
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
                        free_ca_mode: service.free_ca_mode,
                    });
                }
                listed.push(StoredChannel {
                    list: ChannelList::Tkgs,
                    number: service.lcn,
                    original_network_id: None,
                    transport_stream_id: mux.transport_stream_id,
                    service_id: service.service_id,
                    name: service.name,
                    provider: service.provider,
                    service_type: service.service_type,
                    source: source.clone(),
                    free_ca_mode: service.free_ca_mode,
                    categories: service.categories,
                });
            }
        }
        listed.sort_by_key(|c| c.number);
        listed.dedup_by_key(|c| c.number);
        // TKGS's rows are the table's new ones; the scans' stay.
        self.channels.retain(|c| c.list != ChannelList::Tkgs);
        self.channels.extend(listed);
        self.tkgs = Some(StoredTkgs {
            title: receiver.table.title.trim().to_string(),
            updated: now, transponders: transponder_count, channels: Vec::new(), orderings: receiver.table.orderings,
            categories: receiver.table.categories,
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
            manual: Vec::new(),
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
            .and_then(|guide| {
                guide.channels.into_iter().find(|channel| {
                    channel.list == ChannelList::Tkgs
                        && channel.service_id == service_id
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
        {
            let mut inner = self.lock();
            if inner.setup.is_some() {
                return Err("Kanal listesi zaten alınıyor.".into());
            }
            inner.setup = Some(Setup::default());
        }
        let result = self.read_and_keep_tkgs().await;
        self.lock().setup = None;
        result
    }

    async fn read_and_keep_tkgs(&self) -> Result<Value, String> {
        // No channel playing, no receiver: one is started on the TKGS
        // multiplex for the table, and stopped -- cleanup and all -- once it
        // is read. None is kept running for it.
        let started = self.lock().token.is_none();
        if started {
            let source = LiveSource::Satellite {
                frequency_khz: 12_380_000,
                symbol_rate_ksps: 27_500,
                polarisation: SatellitePolarisation::Vertical,
            };
            let service_id = self
                .stored_guide()
                .and_then(|guide| guide.multiplex(&source)?.services.first().map(|service| service.service_id))
                .unwrap_or(0);
            if let Err(error) = self.start_with(service_id, &source, true).await {
                // A start called off half way leaves nothing running.
                self.stop().await;
                return Err(error);
            }
            self.set(Lifecycle::Standby);
            if self.lock().setup.is_some_and(|setup| setup.cancelled) {
                self.stop().await;
                return Err("Kanal kurulumu iptal edildi.".into());
            }
        }
        let value = self.read_tkgs().await;
        if started {
            self.stop().await;
        }
        let value = value?;
        let receiver: ReceiverTkgs = serde_json::from_value(value).map_err(|error| format!("TKGS sonucu okunamadı: {error}"))?;
        let mut stored = self.stored_guide().unwrap_or_default();
        stored.absorb_tkgs(receiver, now_utc());
        self.keep_guide(&stored)?;
        let tkgs = stored.tkgs.as_ref().expect("TKGS was just stored");
        let channels = stored.channels();
        let radio = channels.iter().filter(|channel| channel.radio).count();
        Ok(json!({
            "channels": stored.channels.iter().filter(|c| c.list == ChannelList::Tkgs).count(),
            "transponders": tkgs.transponders, "updated": tkgs.updated,
            "television": channels.len() - radio, "radio": radio, "title": tkgs.title,
        }))
    }

    pub fn set_tkgs_ordering(&self, hd_first: bool) -> Result<Value, String> {
        let mut stored = self.stored_guide().unwrap_or_default();
        stored.tkgs_hd_first = hd_first;
        self.keep_guide(&stored)?;
        Ok(json!({"hd_first": hd_first}))
    }

    pub fn set_tkgs_mode(&self, off: bool) -> Result<Value, String> {
        let mut stored = self.stored_guide().unwrap_or_default();
        stored.tkgs_off = off;
        self.keep_guide(&stored)?;
        Ok(json!({"off": off}))
    }

    pub fn set_tkgs_categories(&self, shown: bool) -> Result<Value, String> {
        let mut stored = self.stored_guide().unwrap_or_default();
        stored.categories_hidden = !shown;
        self.keep_guide(&stored)?;
        Ok(json!({"shown": shown}))
    }

    /// The channel setup's manual scan: `source` streamed for what it
    /// carries, and its playable services that `free_only` and `keep` let
    /// through put in the channel list. One setup at a time; its progress
    /// and cancel are the TKGS read's.
    pub async fn scan_transponder(&self, source: LiveSource, filter: ScanFilter) -> Result<Value, String> {
        self.end_probe().await;
        if !matches!(source, LiveSource::Satellite { .. }) {
            return Err("Yalnız uydu taşıyıcısı taranır.".into());
        }
        source.validate()?;
        {
            let mut inner = self.lock();
            if inner.setup.is_some() {
                return Err("Kanal kurulumu zaten sürüyor.".into());
            }
            inner.setup = Some(Setup { scan: true, ..Setup::default() });
        }
        let result = self.scan_and_keep(&source, filter).await;
        self.lock().setup = None;
        result
    }

    /// The channel setup's satellite scan: every carrier TKGS names and
    /// every one scanned before, in frequency order, each one's channels
    /// kept as `scan_transponder` keeps them. A carrier without signal is
    /// passed over.
    pub async fn scan_satellite(&self, filter: ScanFilter, add_only: bool) -> Result<Value, String> {
        self.end_probe().await;
        let stored = self.stored_guide().unwrap_or_default();
        // A network scan (Sony's Scan mode "Network"): the home carrier
        // first, then every carrier the NIT names, with those known already.
        let mut carriers: Vec<LiveSource> = vec![HOME_CARRIER];
        let listed = stored.channels.iter().map(|c| c.source.clone());
        for source in listed.chain(stored.multiplexes.iter().map(|m| m.source.clone())) {
            if matches!(source, LiveSource::Satellite { .. }) && !carriers.contains(&source) {
                carriers.push(source);
            }
        }
        carriers[1..].sort_by_key(|s| match s { LiveSource::Satellite { frequency_khz, .. } => *frequency_khz, _ => 0 });
        {
            let mut inner = self.lock();
            if inner.setup.is_some() {
                return Err("Kanal kurulumu zaten sürüyor.".into());
            }
            inner.setup = Some(Setup { scan: true, carriers: carriers.len() as u64, ..Setup::default() });
        }
        let (mut found, mut television, mut radio, mut added, mut first, mut locked) = (0, 0, 0, 0, None, 0);
        let mut outcome = Ok(());
        // A re-scan (Sony's Satellite Re-scan): once it has gone through
        // every carrier, the list is what it found and nothing else.
        let mut kept_by_carrier: Vec<(LiveSource, Vec<u16>)> = Vec::new();
        let mut check = CarrierCheck::default();
        let restore = match self.scan_receiver(&carriers[0]).await {
            Ok(restore) => restore,
            Err(error) => {
                self.lock().setup = None;
                return Err(error);
            }
        };
        let mut at = 0;
        while at < carriers.len() {
            let source = carriers[at].clone();
            let source = &source;
            at += 1;
            if let Some(setup) = self.lock().setup.as_mut() {
                setup.carrier = at as u64;
                setup.carriers = carriers.len() as u64;
                setup.frequency_khz = match source { LiveSource::Satellite { frequency_khz, .. } => *frequency_khz as u64, _ => 0 };
                setup.found = 0;
                setup.kinds = [0; 3];
                setup.locked = false;
            }
            let result = self.scan_carrier(source, filter, at == 1, &mut check).await;
            // The home carrier's NIT is the network's; the others' NITs are
            // other networks', which name other satellites as well.
            if at == 1 {
                check.network = self.read_network();
                for (carrier, _) in &check.network {
                    if !carriers.contains(carrier) {
                        carriers.push(carrier.clone());
                    }
                }
            }
            match result {
                Ok(result) => {
                    locked += 1;
                    let n = |key: &str| result.get(key).and_then(Value::as_u64).unwrap_or(0);
                    found += n("found");
                    television += n("television");
                    radio += n("radio");
                    added += n("added");
                    first = first.or(result.get("first").and_then(Value::as_u64));
                    let kept = result.get("kept").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as u16).collect()).unwrap_or_default();
                    kept_by_carrier.push((source.clone(), kept));
                    if let Some(setup) = self.lock().setup.as_mut() {
                        setup.added = added;
                        setup.found_before = found;
                        setup.found = 0;
                        for (before, now) in setup.kinds_before.iter_mut().zip(setup.kinds) {
                            *before += now;
                        }
                        setup.kinds = [0; 3];
                    }
                }
                Err(_) if self.lock().setup.is_some_and(|setup| setup.cancelled) => {
                    outcome = Err("Kanal kurulumu iptal edildi.".to_string());
                    break;
                }
                // The receiver gone is not a carrier without signal: the
                // scan ends there, and the list stays as it was.
                Err(error) if !Self::unit_active().await => {
                    outcome = Err(error);
                    break;
                }
                Err(_) => {}
            }
        }
        self.scan_restore(restore).await;
        self.lock().setup = None;
        outcome?;
        let mut stored = self.stored_guide().unwrap_or_default();
        if !add_only {
            stored.channels.retain(|c| {
                c.list != ChannelList::Scan
                    || kept_by_carrier.iter().any(|(source, kept)| *source == c.source && kept.contains(&c.service_id))
            });
        }
        self.keep_guide(&stored)?;
        Ok(json!({
            "found": found, "television": television, "radio": radio, "added": added, "first": first,
            "carriers": carriers.len(), "locked": locked,
        }))
    }

    /// "Kanal listesini sil" (Vestel 55UA9090 kılavuzu, Kanallar): every
    /// row of the channel table, TKGS's and the scans'. The setup's
    /// settings stay as they are. The channel on goes with them.
    pub async fn clear_channels(&self) -> Result<Value, String> {
        if self.lock().setup.is_some() {
            return Err("Kanal kurulumu sürüyor.".into());
        }
        self.end_probe().await;
        self.stop().await;
        let mut stored = self.stored_guide().unwrap_or_default();
        // The channels the list showed, not the table's rows.
        let removed = stored.channels().len();
        stored.channels.clear();
        self.keep_guide(&stored)?;
        Ok(json!({"removed": removed}))
    }

    /// The LNB's DiSEqC 1.0 port (0 A .. 3 D), or none; a receiver up on
    /// another is not reused (`can_reuse`).
    pub fn set_diseqc_port(&self, port: Option<u8>) -> Result<Value, String> {
        if port.is_some_and(|port| port > 3) {
            return Err("DiSEqC portu A, B, C ya da D olur.".into());
        }
        let mut stored = self.stored_guide().unwrap_or_default();
        stored.diseqc_port = port;
        self.keep_guide(&stored)?;
        Ok(json!({"diseqc_port": port}))
    }

    /// The manual scan form's signal level and quality (Sony's Manual
    /// Tuning shows them as the transponder is set): the receiver on
    /// `source`, streaming it as a scan does, and what it says of the
    /// carrier. None ends it and puts the receiver back as it was.
    pub async fn probe_signal(&self, source: Option<LiveSource>) -> Result<Value, String> {
        if self.lock().setup.is_some() {
            return Err("Kanal kurulumu sürüyor.".into());
        }
        let Some(source) = source else {
            self.end_probe().await;
            return Ok(json!({"probe": false}));
        };
        source.validate()?;
        let LiveSource::Satellite { frequency_khz, symbol_rate_ksps, polarisation } = source.clone() else {
            return Err("Yalnız uydu taşıyıcısı ölçülür.".into());
        };
        // The DiSEqC setting changed under the reading: the receiver is
        // started again on it.
        // What leaving puts back stays as it was before.
        let port = self.stored_guide().map_or(default_diseqc_port(), |guide| guide.diseqc_port);
        if self.lock().diseqc_port != port {
            let probe = self.lock().probe.take();
            if let Some((restore, _)) = probe {
                self.scan_receiver(&source).await?;
                self.lock().probe = Some((restore, LiveSource::Recording));
            }
        }
        let on = self.lock().probe.as_ref().map(|(_, on)| on.clone());
        if on.is_none() {
            let restore = self.scan_receiver(&source).await?;
            self.lock().probe = Some((restore, source.clone()));
        }
        if on.as_ref() != Some(&source) {
            self.select(json!({
                "kind": "scan",
                "transponder": {"frequency_khz": frequency_khz, "symbol_rate_ksps": symbol_rate_ksps, "polarisation": polarisation},
            }))?;
            if let Some((_, on)) = self.lock().probe.as_mut() {
                *on = source.clone();
            }
        }
        let status = self.read_status();
        let locked = status.as_ref().and_then(|s| s.get("state")).and_then(Value::as_str) == Some("streaming");
        Ok(json!({
            "probe": true, "locked": locked,
            "strength_pct": status.as_ref().and_then(|s| s.pointer("/signal/strength_pct")).and_then(Value::as_u64),
            "snr_db": status.as_ref().and_then(|s| s.pointer("/signal/snr_db")).and_then(Value::as_f64),
        }))
    }

    /// A scan takes the receiver over from the form's signal reading.
    async fn end_probe(&self) {
        let probe = self.lock().probe.take();
        if let Some((restore, _)) = probe {
            self.scan_restore(restore).await;
        }
    }

    /// The carriers the NIT of the multiplex just scanned names.
    fn read_network(&self) -> Vec<(LiveSource, (u16, u16))> {
        let Some(guide) = std::fs::read_to_string(self.paths.guide())
            .ok()
            .and_then(|text| serde_json::from_str::<ReceiverGuide>(&text).ok())
        else {
            return Vec::new();
        };
        guide
            .network
            .into_iter()
            // Only Türksat 42.0°E's: a NIT may name other satellites' too.
            .filter(|c| c.east && c.orbital_position_tenths == 420)
            .filter_map(|c| {
                let polarisation = match c.polarisation.as_str() {
                    "horizontal" => SatellitePolarisation::Horizontal,
                    "vertical" => SatellitePolarisation::Vertical,
                    _ => return None,
                };
                let source = LiveSource::Satellite { frequency_khz: c.frequency_khz, symbol_rate_ksps: c.symbol_rate_ksps, polarisation };
                Some((source, (c.original_network_id, c.transport_stream_id)))
            })
            .collect()
    }

    async fn scan_and_keep(&self, source: &LiveSource, filter: ScanFilter) -> Result<Value, String> {
        let restore = self.scan_receiver(source).await?;
        let result = self.scan_carrier(source, filter, false, &mut CarrierCheck::default()).await;
        self.scan_restore(restore).await;
        result
    }

    /// The receiver for a scan: one already up is retuned (and put back by
    /// `scan_restore`), else one is started on `first` in standby. It stays
    /// on the scan from carrier to carrier, as AOSP's ScanFragment keeps its
    /// streamer for the whole channel map.
    async fn scan_receiver(&self, first: &LiveSource) -> Result<ScanRestore, String> {
        let reuse = self.can_reuse(first);
        let restore = {
            let inner = self.lock();
            ScanRestore {
                started: !reuse,
                playing: (reuse && inner.lifecycle == Lifecycle::Playing).then(|| (inner.service_id, inner.source.clone())),
            }
        };
        if !reuse {
            if let Err(error) = self.start_with(0, first, true).await {
                self.stop().await;
                return Err(error);
            }
            self.set(Lifecycle::Standby);
        }
        Ok(restore)
    }

    async fn scan_restore(&self, restore: ScanRestore) {
        if restore.started {
            self.stop().await;
        } else if let Some((Some(service_id), Some(LiveSource::Satellite { frequency_khz, symbol_rate_ksps, polarisation }))) = restore.playing {
            let _ = self.select(json!({
                "kind": "play", "service_id": service_id,
                "transponder": {"frequency_khz": frequency_khz, "symbol_rate_ksps": symbol_rate_ksps, "polarisation": polarisation}, "new_fifo": null,
            }));
        } else {
            let _ = self.select(json!({"kind": "standby"}));
        }
    }

    /// One carrier scanned and what `filter` lets through kept.
    async fn scan_carrier(&self, source: &LiveSource, filter: ScanFilter, network: bool, check: &mut CarrierCheck) -> Result<Value, String> {
        let LiveSource::Satellite { frequency_khz, symbol_rate_ksps, polarisation } = *source else {
            return Err("Yalnız uydu taşıyıcısı taranır.".into());
        };
        // Only what this scan's multiplex says counts.
        let _ = std::fs::remove_file(self.paths.guide());
        self.select(json!({
            "kind": "scan",
            "transponder": {"frequency_khz": frequency_khz, "symbol_rate_ksps": symbol_rate_ksps, "polarisation": polarisation},
        }))?;
        let receiver = self.scan_wait(frequency_khz, network).await?;
        let offset_khz = self
            .read_status()
            .and_then(|status| status.pointer("/tune/search_offset_khz").and_then(Value::as_i64))
            .map_or(0, |offset| offset.unsigned_abs() as u32);
        let wrong = match check.carrier(source, receiver.original_network_id, receiver.transport_stream_id, offset_khz) {
            Ok(earlier) => earlier.map(|earlier| (earlier, None)),
            Err(error) => Some((source.clone(), Some(error))),
        };
        if let Some((wrong, error)) = wrong {
            // What a neighbour's lock put under that carrier goes with it.
            let mut stored = self.stored_guide().unwrap_or_default();
            stored.multiplexes.retain(|m| !(m.source == wrong && m.transport_stream_id == receiver.transport_stream_id));
            stored.channels.retain(|c| !(c.list == ChannelList::Scan && c.source == wrong));
            self.keep_guide(&stored)?;
            if let Some(error) = error {
                return Err(error);
            }
        }
        let wanted = |s: &StoredService| {
            is_playable(s.service_type)
                && !s.name.trim().is_empty()
                && !(filter.free_only && s.free_ca_mode)
                && !(filter.scrambled_only && !s.free_ca_mode)
                && match filter.keep {
                    LiveScanKeep::All => true,
                    LiveScanKeep::Television => !is_radio(s.service_type),
                    LiveScanKeep::Radio => is_radio(s.service_type),
                }
        };
        let kept: Vec<u16> = receiver.services.iter().filter(|s| wanted(s)).map(|s| s.service_id).collect();
        let radio = receiver.services.iter().filter(|s| wanted(s) && is_radio(s.service_type)).count();
        let mut stored = self.stored_guide().unwrap_or_default();
        let services = receiver.services;
        let added = stored.absorb_scan(
            source, receiver.transport_stream_id, receiver.original_network_id, services.clone(), &kept, now_utc(),
        );
        self.keep_guide(&stored)?;
        let is_tv = |sid: &u16| services.iter().any(|s| s.service_id == *sid && !is_radio(s.service_type));
        let first = added.iter().find(|sid| is_tv(sid)).or(added.first()).copied();
        Ok(json!({
            "found": kept.len(), "television": kept.len() - radio, "radio": radio,
            "added": added.len(), "first": first, "kept": kept,
        }))
    }

    /// Until the scanned multiplex's SDT has stopped naming new services: the
    /// services and NIT carriers in its `guide.json` unchanged for
    /// `SCAN_SETTLE` after the first services in it (the file itself is
    /// rewritten with every EIT and NIT repetition), at most `SCAN_MOST`. No signal, nothing on it, the receiver gone, or the setup called
    /// off end it with the reason.
    async fn scan_wait(&self, frequency_khz: u32, network: bool) -> Result<ReceiverGuide, String> {
        const SCAN_SETTLE: Duration = Duration::from_secs(5);
        const NIT_WAIT: Duration = Duration::from_secs(12);
        const SCAN_MOST: Duration = Duration::from_secs(45);
        let started = Instant::now();
        let mut last: Option<((Vec<StoredService>, usize), Instant)> = None;
        loop {
            if self.lock().setup.is_some_and(|setup| setup.cancelled) {
                return Err("Kanal kurulumu iptal edildi.".into());
            }
            if !Self::unit_active().await {
                return Err("Tarama sırasında uydu alıcısı kapandı.".into());
            }
            let status = self.read_status();
            let locked = status.as_ref().and_then(|s| s.get("state")).and_then(Value::as_str) == Some("streaming");
            let token = self.lock().token.clone();
            let text = std::fs::read_to_string(self.paths.guide()).ok();
            let guide = text.as_deref().and_then(|text| serde_json::from_str::<ReceiverGuide>(text).ok())
                .filter(|guide| guide.token.is_some() && guide.token == token);
            let found = guide.as_ref().map_or(0, |g| g.services.iter().filter(|s| is_playable(s.service_type)).count());
            let mut kinds = [0u64; 3];
            for s in guide.iter().flat_map(|g| g.services.iter()).filter(|s| !s.name.trim().is_empty()) {
                kinds[service_kind(s.service_type)] += 1;
            }
            if let Some(setup) = self.lock().setup.as_mut() {
                setup.kinds = kinds;
                setup.locked = locked;
                setup.found = found as u64;
                setup.strength_pct = status.as_ref().and_then(|s| s.pointer("/signal/strength_pct")).and_then(Value::as_u64);
                setup.snr_db = status.as_ref().and_then(|s| s.pointer("/signal/snr_db")).and_then(Value::as_f64);
            }
            // The whole SDT is in (and on the home carrier the NIT, sent at
            // least every 10 s, EN 300 468 5.1.4): nothing more will come.
            let whole = guide.as_ref().is_some_and(|guide| {
                !guide.services.is_empty()
                    && guide.sdt_complete
                    && (!network || guide.nit_complete || started.elapsed() >= NIT_WAIT)
            });
            if whole && let Some(guide) = guide {
                return Ok(guide);
            }
            if let Some(guide) = guide
                && !guide.services.is_empty()
            {
                let seen_now = (guide.services.clone(), guide.network.len());
                match &last {
                    Some((seen, since)) if *seen == seen_now => {
                        // The home carrier's NIT, repeated at most every
                        // 10 s (EN 300 468 5.1.4), is waited for too.
                        if since.elapsed() >= SCAN_SETTLE && (!network || !guide.network.is_empty() || started.elapsed() >= NIT_WAIT) {
                            return Ok(guide);
                        }
                    }
                    _ if started.elapsed() >= SCAN_MOST => return Ok(guide),
                    _ => last = Some((seen_now, Instant::now())),
                }
            }
            let elapsed = started.elapsed();
            // The receiver has tried the carrier and its offsets and says
            // nothing is there: on to the next (AOSP's Tuner framework moves
            // on when LOCKED does not come). Locks here take under 2.5 s.
            let no_signal = status.as_ref().and_then(|s| s.get("state")).and_then(Value::as_str) == Some("no_signal");
            if !locked && last.is_none() && (elapsed >= Duration::from_secs(20) || (no_signal && elapsed >= Duration::from_secs(3))) {
                return Err(format!("{} MHz: sinyal yok.", frequency_khz / 1000));
            }
            if last.is_none() && elapsed >= Duration::from_secs(30) {
                return Err(format!("{} MHz: yayın bulunamadı.", frequency_khz / 1000));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
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
        let running = self.lock().setup.is_some();
        match self.stored_guide() {
            Some(guide) => json!({
                "channels": guide.channels(),
                "multiplexes": guide.multiplexes.len(),
                "updated": guide.tkgs.as_ref().map(|t| t.updated).or_else(|| guide.multiplexes.iter().map(|m| m.updated).max()),
                "tkgs": guide.tkgs.as_ref().map(|t| json!({
                    "channels": guide.channels.iter().filter(|c| c.list == ChannelList::Tkgs).count(),
                    "transponders": t.transponders,
                    "updated": t.updated, "hd_first": guide.tkgs_hd_first, "title": t.title,
                })),
                "setup": {
                    "tkgs_off": guide.tkgs_off, "categories_hidden": guide.categories_hidden, "hd_first": guide.tkgs_hd_first,
                    "running": running, "diseqc_port": guide.diseqc_port,
                },
            }),
            None => json!({
                "channels": [], "multiplexes": 0, "updated": null, "tkgs": null,
                "setup": {"tkgs_off": false, "categories_hidden": false, "hd_first": true, "running": running, "diseqc_port": 0},
            }),
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
             --hardware --transponder 12188000 --sr 27500 --polarisation=vertical --firmware-dir /var/lib/mediabox-dvb/firmware --reconnect-s 20 --hold-reader-s 30 --diseqc-port=0",
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
        // A tune keeps the multiplex; only the setup makes channels.
        assert_eq!(live.stored_guide().unwrap().multiplexes[0].services.len(), 3);
        assert_eq!(live.channels()["channels"].as_array().unwrap().len(), 0);
        let window = live.guide_window(now - 3600, now + 3600);
        assert_eq!(window["events"]["10801"][0]["title"], "Haber");
        assert_eq!(window["events"]["10801"].as_array().unwrap().len(), 1, "an event long ended is let go of");
        // Another start's guide is not this one's.
        live.lock().token = Some("1-7".into());
        assert!(!live.absorb_guide());
    }

    fn receiver(tsid: u16, services: &[(u16, &str)]) -> ReceiverGuide {
        ReceiverGuide { original_network_id: None, network: vec![], sdt_complete: false, nit_complete: false,
            token: None,
            transport_stream_id: Some(tsid),
            services: services
                .iter()
                .map(|&(service_id, name)| StoredService {
                    service_id,
                    name: name.into(),
                    provider: None,
                    service_type: Some(0x19),
                    free_ca_mode: false,
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
        // Scanned, the channel table has their rows, numbered in its order.
        let mux = |at: usize| store.multiplexes[at].services.clone();
        let (first, second) = (mux(0), mux(1));
        store.absorb_scan(&s12188, Some(41306), Some(1070), first, &[10800, 10801], 0);
        store.absorb_scan(&s11794, Some(40301), Some(1070), second, &[10601, 10800], 0);
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
        store.channels.push(StoredChannel {
            list: ChannelList::Tkgs,
            number: 42,
            original_network_id: None,
            transport_stream_id: None,
            service_id: 52202,
            name: "H CHANNEL".into(),
            provider: None,
            service_type: Some(0x19),
            source: exact.clone(),
            free_ca_mode: false,
            categories: vec![],
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
        let first = ReceiverGuide { original_network_id: None, network: vec![], sdt_complete: false, nit_complete: false,
            token: None,
            transport_stream_id: Some(1),
            services: vec![],
            events: vec![ReceiverEvents { service_id: 5, events: vec![event(now, 60, "A"), event(now + 3600, 60, "B")] }],
        };
        assert!(stored.absorb(first, &source, now));
        let moved = ReceiverGuide { original_network_id: None, network: vec![], sdt_complete: false, nit_complete: false,
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

