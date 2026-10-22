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

use mediabox_core::{LiveChannel, LiveEvent, LiveSource};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::playback::SourceEnd;

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
            }),
            starts: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn fifo(&self) -> PathBuf {
        self.paths.fifo()
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
            LiveSource::Satellite { frequency_khz, symbol_rate_ksps } => {
                // The receiver itself, chosen explicitly. Never an LNB flag:
                // the RF source is prepared outside MediaBox.
                argv.push("--hardware".into());
                argv.extend(["--transponder".into(), frequency_khz.to_string()]);
                argv.extend(["--sr".into(), symbol_rate_ksps.to_string()]);
                argv.extend(["--firmware-dir".into(), self.paths.firmware_dir.display().to_string()]);
                // A receiver unplugged and plugged back in carries on.
                argv.extend(["--reconnect-s".into(), "20".into()]);
                // Warm between players: a channel change is a new service
                // for the selector (`switch`), not a new receiver.
                argv.extend(["--hold-reader-s".into(), HOLD_READER_S.to_string()]);
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
        }
        self.set(Lifecycle::Starting);
        let output = Command::new("systemd-run")
            .args(self.argv(service_id, source, &token))
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
    /// running: same multiplex source, this start's, streaming.
    pub fn can_switch(&self, source: &LiveSource) -> bool {
        {
            let inner = self.lock();
            if inner.token.is_none()
                || inner.source.as_ref() != Some(source)
                || !matches!(source, LiveSource::Satellite { .. })
                || inner.lifecycle != Lifecycle::Playing
            {
                return false;
            }
        }
        self.receiver_state().as_deref() == Some("streaming")
    }

    /// Ask the running receiver for `service_id` instead (`select.json`),
    /// and wait until it says it has moved.
    pub async fn switch(&self, service_id: u16) -> Result<(), String> {
        let token = self.lock().token.clone().ok_or("uydu alıcısı çalışmıyor")?;
        let path = self.paths.run_dir.join("select.json");
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json!({"token": token, "service_id": service_id}).to_string())
            .and_then(|()| std::fs::rename(&temporary, &path))
            .map_err(|error| format!("kanal seçimi yazılamadı: {error}"))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = self.read_status() {
                if Self::final_of(&status).is_some() {
                    return Err("uydu alıcısı kapandı".into());
                }
                if status.get("service_id").and_then(Value::as_u64) == Some(service_id as u64) {
                    self.lock().service_id = Some(service_id);
                    return Ok(());
                }
            }
            if Instant::now() > deadline {
                return Err("uydu alıcısı kanal değişimini onaylamadı".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
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

// ------------------------------------------------------------------ guide

/// TV service types (EN 300 468 table 87): digital television, MPEG-2 HD,
/// advanced codec SD/HD/UHD. Radio and data services are not channels here:
/// the player has nothing to put on the plane for them.
fn is_television(service_type: Option<u8>) -> bool {
    matches!(service_type, Some(0x01 | 0x11 | 0x16 | 0x19 | 0x1F))
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GuideStore {
    #[serde(default)]
    pub multiplexes: Vec<StoredGuide>,
}

impl GuideStore {
    /// The file, in this form or in the one-multiplex form it had first.
    fn parse(text: &str) -> Option<Self> {
        if let Ok(store) = serde_json::from_str::<GuideStore>(text)
            && !store.multiplexes.is_empty()
        {
            return Some(store);
        }
        serde_json::from_str::<StoredGuide>(text).ok().map(|one| GuideStore { multiplexes: vec![one] })
    }

    /// The channels of every multiplex, numbered on from one multiplex to
    /// the next, each multiplex's in service-id order. Numbers are
    /// MediaBox's until TKGS gives the real ones. A service id another
    /// multiplex already has is left out: the interface knows a channel by
    /// its service id.
    pub fn channels(&self) -> Vec<LiveChannel> {
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
        Self { source, transport_stream_id: None, updated: 0, services: Vec::new(), events: Default::default() }
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

    /// Take in what this start's receiver has said of its multiplex; written
    /// to the store when it changed anything.
    pub fn absorb_guide(&self) -> bool {
        let (token, source) = {
            let inner = self.lock();
            match (inner.token.clone(), inner.source.clone()) {
                (Some(token), Some(source)) => (token, source),
                _ => return false,
            }
        };
        let Some(receiver) = std::fs::read_to_string(self.paths.guide())
            .ok()
            .and_then(|text| serde_json::from_str::<ReceiverGuide>(&text).ok())
            .filter(|guide| guide.token.as_deref() == Some(token.as_str()))
        else {
            return false;
        };
        let mut stored = self.stored_guide().unwrap_or_default();
        if !stored.absorb(receiver, &source, now_utc()) {
            return false;
        }
        let Ok(text) = serde_json::to_string(&stored) else { return false };
        let temporary = self.paths.guide_store.with_extension("json.tmp");
        let written = std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, &self.paths.guide_store));
        if let Err(error) = written {
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
                "updated": guide.multiplexes.iter().map(|m| m.updated).max(),
            }),
            None => json!({"channels": [], "multiplexes": 0, "updated": null}),
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
        let satellite = LiveSource::Satellite { frequency_khz: 12_188_000, symbol_rate_ksps: 27_500 };
        let argv = live.argv(10801, &satellite, "7-1");
        let joined = argv.join(" ");
        assert!(joined.contains("--unit=mediabox-dvb.service"));
        assert!(joined.contains("--property=BindsTo=mediabox-tv-ui.service"));
        assert!(joined.contains("--property=RuntimeDirectoryPreserve=yes"));
        assert!(joined.ends_with(&format!(
            "play --service 10801 --fifo {0}/live.ts --status {0}/status.json --token 7-1 \
             --hardware --transponder 12188000 --sr 27500 --firmware-dir /var/lib/mediabox-dvb/firmware --reconnect-s 20 --hold-reader-s 30",
            dir.path().display()
        )));
        let recording = live.argv(10801, &LiveSource::Recording, "7-2").join(" ");
        assert!(recording.ends_with("--recording /var/lib/mediabox-dvb/recording.ts --pace"));
        assert!(!recording.contains("--hardware"), "a recording never opens the receiver");
        for argv in [joined.as_str(), recording.as_str()] {
            assert!(!argv.contains("lnb") && !argv.contains("--pol"), "no LNB control: {argv}");
        }
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
        let source = LiveSource::Satellite { frequency_khz: 12_188_000, symbol_rate_ksps: 27_500 };
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
        let s12188 = LiveSource::Satellite { frequency_khz: 12_188_000, symbol_rate_ksps: 27_500 };
        let s11794 = LiveSource::Satellite { frequency_khz: 11_794_000, symbol_rate_ksps: 30_000 };
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
}
