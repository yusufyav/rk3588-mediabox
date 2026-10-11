//! Sound: the kept setting, and putting it into effect.
//!
//! What sound devices exist and what their sinks declare is the platform's to
//! say (`mediabox_platform::sound`), asked afresh on every request, so a USB
//! DAC plugged in a moment ago is there and one pulled out is not. What a
//! person chose is kept here, in `audio.json` beside the display's record,
//! and checked against those devices before it is written: nothing the sink
//! does not declare can be chosen. What the choice means for a player is
//! [`mediabox_core::audio::plan`], the one function the player, the browser
//! and Kodi are all started from.
//!
//! Volume is two things, because sound leaves this box two ways. The
//! interface's own player applies it as its own software gain, which mpv
//! gives PCM and never a bitstream; the browser and Kodi play PCM through
//! `mediabox_volume`, an ALSA `softvol` PCM on the same card
//! (`mediabox-hdmi-prepare` writes it), whose control this sets. Neither is a
//! mixer anybody else owns, and neither touches an IEC 61937 burst.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mediabox_core::{AudioDevice, AudioPlan, AudioSetting};

/// The softvol control `mediabox-hdmi-prepare` defines on the chosen card.
pub const SOFTVOL_CONTROL: &str = "MediaBox";

/// A change to the volume alone: the remote's keys, the slider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeChange {
    Set(u8),
    Step(i16),
    Mute(bool),
    ToggleMute,
}

pub struct Audio {
    setting_path: PathBuf,
    setting: Mutex<AudioSetting>,
    /// Every device found now. The platform in production; a list in tests.
    discover: Box<dyn Fn() -> Vec<AudioDevice> + Send + Sync>,
    /// Put a volume on the softvol control of this plan's card.
    softvol: Box<dyn Fn(&AudioPlan) + Send + Sync>,
    /// What the interface should show that it did not ask for.
    events: tokio::sync::broadcast::Sender<mediabox_core::AudioEvent>,
    /// The device the last plan worked out sends sound to.
    active: std::sync::Mutex<Option<AudioDevice>>,
    /// One change to the setting at a time, read and written under it: a held
    /// key's steps arrive together, and each read the same level and wrote
    /// the same next one when they were not serialised.
    serial: std::sync::Mutex<()>,
}

impl Audio {
    pub fn new(
        setting_path: impl Into<PathBuf>,
        discover: impl Fn() -> Vec<AudioDevice> + Send + Sync + 'static,
        softvol: impl Fn(&AudioPlan) + Send + Sync + 'static,
    ) -> Self {
        let setting_path = setting_path.into();
        let setting = std::fs::read_to_string(&setting_path)
            .ok()
            .and_then(|text| AudioSetting::parse(&text))
            .unwrap_or_default();
        Self {
            setting_path,
            setting: Mutex::new(setting),
            discover: Box::new(discover),
            softvol: Box::new(softvol),
            events: tokio::sync::broadcast::channel(16).0,
            active: std::sync::Mutex::new(None),
            serial: std::sync::Mutex::new(()),
        }
    }

    /// The device sound goes to, as last worked out.
    pub fn active_device(&self) -> Option<AudioDevice> {
        self.active.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<mediabox_core::AudioEvent> {
        self.events.subscribe()
    }

    /// The headset playing the sound moved its own volume to `level` (AVRCP,
    /// 0-127): the box's volume follows, as Android's does, and the
    /// interface is told so it can show it. A level that is the box's own
    /// coming back from the headset changes nothing and says nothing.
    /// Returns the plan before and after when it changed.
    pub fn headset_volume(&self, address: &str, level: u8) -> Option<(AudioPlan, AudioPlan)> {
        let _serial = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        let (setting, _, before) = self.now();
        let playing = before
            .device
            .as_ref()
            .is_some_and(|device| device.id == format!("bluetooth:{address}"));
        let volume = mediabox_core::audio::from_avrcp(level);
        if !playing || volume == setting.volume {
            return None;
        }
        let mut wanted = setting.clone();
        wanted.volume = volume;
        // Turning it up is unmuting it, as on every television.
        if volume > setting.volume {
            wanted.muted = false;
        }
        // Android's FLAG_BLUETOOTH_ABS_VOLUME: a level that came from the
        // headset is kept and shown, and never said back to the headset --
        // AudioService posts a level to AVRCP only when that flag is absent.
        // Saying it back is what made the loop: the headset answered, the
        // answer looked like a press, and it was said back again.
        let (before, after) = self.keep(wanted).ok()?;
        if before.muted != after.muted
            && let Some(device) = &after.device
        {
            set_headset_mute(device, after.muted);
        }
        let _ = self.events.send(mediabox_core::AudioEvent::Volume {
            volume: after.volume,
            muted: after.muted,
        });
        Some((before, after))
    }

    /// The appliance's own: the platform's discovery, and `amixer` on the
    /// card the plan names.
    pub fn system() -> Self {
        let roots = mediabox_platform::Roots::system();
        Self::new(
            roots.state("audio.json"),
            move || {
                let roots = mediabox_platform::Roots::system();
                let platform = mediabox_platform::Platform::discover();
                let mut found = mediabox_platform::sound::devices(
                    &platform,
                    &roots,
                    Path::new(mediabox_platform::sound::ALSA_CARDS),
                );
                found.extend(mediabox_platform::sound::bluetooth_devices());
                found
            },
            set_softvol,
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AudioSetting> {
        self.setting.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn setting(&self) -> AudioSetting {
        self.lock().clone()
    }

    /// The kept setting, the devices found now, and what the one means on
    /// the other.
    pub fn now(&self) -> (AudioSetting, Vec<AudioDevice>, AudioPlan) {
        let setting = self.setting();
        let devices = (self.discover)();
        let plan = mediabox_core::audio::plan(&setting, &devices);
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = plan.device.clone();
        (setting, devices, plan)
    }

    /// Keep `wanted`, if it can be chosen on the devices here: written down
    /// first, then in force. Returns the plan before and after.
    pub fn set(&self, wanted: AudioSetting) -> Result<(AudioPlan, AudioPlan), String> {
        let _serial = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        self.set_serialised(wanted)
    }

    fn set_serialised(&self, wanted: AudioSetting) -> Result<(AudioPlan, AudioPlan), String> {
        let (before, after) = self.keep(wanted)?;
        (self.softvol)(&after);
        Ok((before, after))
    }

    /// `set` without putting the level on the card or the headset: for a
    /// level that is already there because the headset itself set it.
    fn keep(&self, wanted: AudioSetting) -> Result<(AudioPlan, AudioPlan), String> {
        let devices = (self.discover)();
        let mut kept = self.lock();
        mediabox_core::audio::check(&wanted, &devices, &kept)?;
        let before = mediabox_core::audio::plan(&kept, &devices);
        crate::output::write_atomic(&self.setting_path, &wanted.to_stored())
            .map_err(|error| format!("Ses ayarı kaydedilemedi: {error}"))?;
        *kept = wanted;
        let after = mediabox_core::audio::plan(&kept, &devices);
        drop(kept);
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = after.device.clone();
        Ok((before, after))
    }

    /// The volume alone. Kept like the rest: a volume that comes back as
    /// something else after a restart is not a volume.
    pub fn volume(&self, change: VolumeChange) -> Result<AudioPlan, String> {
        let _serial = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        let mut wanted = self.setting();
        match change {
            VolumeChange::Set(volume) => wanted.volume = volume.min(100),
            VolumeChange::Step(step) => {
                wanted.volume = (i16::from(wanted.volume) + step).clamp(0, 100) as u8;
                // Turning it up is unmuting it, as on every television.
                if step > 0 {
                    wanted.muted = false;
                }
            }
            VolumeChange::Mute(muted) => wanted.muted = muted,
            VolumeChange::ToggleMute => wanted.muted = !wanted.muted,
        }
        self.set_serialised(wanted).map(|(_, after)| after)
    }

    /// Put the kept volume back on the softvol control: at start, and when
    /// the device it lives on has come back.
    pub fn restore(&self) {
        let (_, _, plan) = self.now();
        (self.softvol)(&plan);
    }
}

/// What this box and a headset have said to each other about its volume,
/// after Android's `avrcp::Device` (system/profile/avrcp/device.cc).
///
/// bluez gives one `Volume` for two different things: the headset's answer to
/// a SetAbsoluteVolume this box sent (`avrcp_handle_set_volume`) and the
/// headset's own EVENT_VOLUME_CHANGED (`avrcp_volume_changed`) -- both end in
/// `media_transport_update_device_volume` (bluez 5.82, profiles/audio/avrcp.c).
/// Android ignores the answer ("we don't care about the response to this
/// message") and acts on the event. Here the two are told apart by what was
/// sent: a report of a level sent in the last [`HEADSET_ANSWER`] is an answer,
/// however many times it comes. Measured on the Plus with the Arctis GameBuds:
/// each level sent came back three times -- bluez-alsa's own change at once,
/// bluez's 6 ms later and the headset's answer 20 to 80 ms later -- so with a
/// held key the third copy arrived after the next level had been sent, and
/// taken as a press it moved the volume back.
#[derive(Debug, Default)]
struct HeadsetSync {
    /// The level both sides last agreed on (Android's `volume_`).
    last: Option<(String, u8)>,
    /// Levels sent and not yet answered, oldest first.
    pending: std::collections::VecDeque<(String, u8, std::time::Instant)>,
}

/// How long an answer to a SetAbsoluteVolume is waited for.
const HEADSET_ANSWER: std::time::Duration = std::time::Duration::from_secs(2);

impl HeadsetSync {
    /// Whether `level` is to be sent: not when it is the level already
    /// agreed (device.cc `SetVolume`: "Ignoring volume change same as current
    /// volume level").
    fn send(&mut self, address: &str, level: u8, now: std::time::Instant) -> bool {
        if self.last.as_ref().is_some_and(|(at, last)| at == address && *last == level) {
            return false;
        }
        self.last = Some((address.to_string(), level));
        self.pending.push_back((address.to_string(), level, now));
        true
    }

    /// Whether a reported level is the headset's own change (`true`) or an
    /// answer to one this box sent (`false`).
    fn reported(&mut self, address: &str, level: u8, now: std::time::Instant) -> bool {
        self.pending.retain(|(_, _, sent)| now.duration_since(*sent) < HEADSET_ANSWER);
        if self.pending.iter().any(|(to, sent, _)| to == address && *sent == level) {
            return false;
        }
        self.last = Some((address.to_string(), level));
        true
    }
}

static HEADSET: std::sync::Mutex<HeadsetSync> = std::sync::Mutex::new(HeadsetSync {
    last: None,
    pending: std::collections::VecDeque::new(),
});

/// bluez-alsa's PCM for a headset, as `bluealsa-cli` names it.
fn headset_pcm(device: &mediabox_core::AudioDevice) -> Option<(String, String)> {
    let address = device.id.strip_prefix("bluetooth:")?;
    let wanted = format!("dev_{}/a2dpsrc", address.replace(':', "_"));
    let list = std::process::Command::new(mediabox_platform::sound::BLUEALSA_CLI)
        .arg("list-pcms")
        .output()
        .ok()?;
    let list = String::from_utf8_lossy(&list.stdout);
    let path = list.lines().map(str::trim).find(|line| line.contains(&wanted))?;
    Some((address.to_string(), path.to_string()))
}

fn bluealsa(args: &[&str]) {
    match std::process::Command::new(mediabox_platform::sound::BLUEALSA_CLI).args(args).output() {
        Ok(output) if !output.status.success() => eprintln!(
            "mediaboxd-rs: bluealsa {}: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => eprintln!("mediaboxd-rs: bluealsa-cli: {error}"),
        _ => {}
    }
}

fn set_headset_mute(device: &mediabox_core::AudioDevice, muted: bool) {
    if let Some((_, path)) = headset_pcm(device) {
        bluealsa(&["mute", &path, if muted { "on" } else { "off" }]);
    }
}

/// The volume of a Bluetooth headset is bluez-alsa's own, 0 to 127 on its
/// PCM, applied in software (`SoftVolume: true`, as bluez-alsa reports it for
/// a headset that does not take AVRCP absolute volume).
fn set_headset_volume(device: &mediabox_core::AudioDevice, plan: &AudioPlan) {
    let Some((address, path)) = headset_pcm(device) else { return };
    let level = mediabox_core::audio::to_avrcp(plan.volume);
    let send = HEADSET
        .lock()
        .expect("headset sync")
        .send(&address, level, std::time::Instant::now());
    if send {
        bluealsa(&["volume", &path, &level.to_string()]);
    }
    bluealsa(&["mute", &path, if plan.muted { "on" } else { "off" }]);
}

/// `amixer -c <card> cset name='MediaBox' <n>`: 0 to 100 on a control
/// declared with that resolution, and 0 when muted -- which softvol plays as
/// silence. Quiet when the control is not there yet: it is created by the
/// first stream through `mediabox_volume`, and `mediabox-hdmi-prepare` opens
/// one before anything plays so that it is.
fn set_softvol(plan: &AudioPlan) {
    let Some(device) = &plan.device else { return };
    if device.kind == mediabox_core::AudioKind::Bluetooth {
        set_headset_volume(device, plan);
        return;
    }
    let value = if plan.muted { 0 } else { plan.volume };
    let amixer = || {
        std::process::Command::new("/usr/bin/amixer")
            .args([
                "-q",
                "-c",
                &device.card_id,
                "cset",
                &format!("name={SOFTVOL_CONTROL}"),
                &value.to_string(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
    };
    let mut result = amixer();
    if result.as_ref().is_ok_and(|output| !output.status.success()) {
        // Not there yet: softvol makes its control on the first stream
        // through it. One stream of nothing -- a card busy with a film
        // already has one -- and again.
        let dev = device.pcm.rsplit_once("DEV=").map(|(_, n)| n).unwrap_or("0");
        let _ = std::process::Command::new("/usr/bin/aplay")
            .args([
                "-q",
                "-D",
                &format!("mediabox_volume:CARD={},DEV={dev}", device.card_id),
                "-t",
                "raw",
                "-f",
                "S16_LE",
                "-c",
                "2",
                "-r",
                "48000",
                "/dev/null",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        result = amixer();
    }
    match result {
        Ok(output) if !output.status.success() => eprintln!(
            "mediaboxd-rs: softvol on {}: {}",
            device.card_id,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => eprintln!("mediaboxd-rs: amixer: {error}"),
        _ => {}
    }
}

/// Sound follows a Bluetooth headset in and out, while everything plays.
///
/// bluez-alsa announces each A2DP PCM as it comes and goes (`bluealsa-cli
/// monitor`: `PCMAdded <path>`, `PCMRemoved <path>`, measured on the Plus).
/// On each, the plan is worked out again on the devices found now, and when
/// the device it names has changed, the film playing in mpv, Kodi and the
/// volume are moved to it -- the way Android moves media to a headset that
/// connects and back to the television when it leaves.
pub async fn follow_headsets(
    audio: std::sync::Arc<Audio>,
    player: std::sync::Arc<crate::player::PlayerManager>,
    kodi: std::sync::Arc<crate::kodi::KodiClient>,
) {
    use tokio::io::AsyncBufReadExt;
    let cli = mediabox_platform::sound::BLUEALSA_CLI;
    let mut last = plan_now(&audio).await;
    loop {
        if !Path::new(cli).is_file() {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            continue;
        }
        let child = tokio::process::Command::new(cli)
            .args(["monitor", "--properties"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn();
        if let Ok(mut child) = child
            && let Some(stdout) = child.stdout.take()
        {
            // Whatever changed while nothing was listening.
            last = reroute(&audio, &player, &kodi, last).await;
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.starts_with("PCMAdded") || line.starts_with("PCMRemoved") {
                    last = reroute(&audio, &player, &kodi, last).await;
                } else if let Some((address, level)) = headset_level(&line) {
                    let own = HEADSET
                        .lock()
                        .expect("headset sync")
                        .reported(&address, level, std::time::Instant::now());
                    if !own {
                        continue;
                    }
                    let follow = std::sync::Arc::clone(&audio);
                    let moved = tokio::task::spawn_blocking(move || follow.headset_volume(&address, level))
                        .await
                        .ok()
                        .flatten();
                    if let Some((before, after)) = moved {
                        player.apply_audio(&before, &after).await;
                        kodi_says_volume(&kodi, &after);
                        last = Some(after);
                    }
                }
            }
        }
        // bluez-alsa went away or restarted; its PCMs went with it.
        last = reroute(&audio, &player, &kodi, last).await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// A headset's volume in a `bluealsa-cli monitor --properties` line, measured
/// on the Plus: `PropertyChanged /org/bluealsa/hci0/dev_20_AF_1B_19_E8_D2/
/// a2dpsrc/sink Volume 0x4C4C` -- left in the high byte, right in the low,
/// each 0-127 with the top bit for muted.
fn headset_level(line: &str) -> Option<(String, u8)> {
    let mut fields = line.split_whitespace();
    if fields.next()? != "PropertyChanged" {
        return None;
    }
    let path = fields.next()?;
    if fields.next()? != "Volume" || !path.contains("/a2dpsrc/") {
        return None;
    }
    let raw = u16::from_str_radix(fields.next()?.trim_start_matches("0x"), 16).ok()?;
    let level = ((raw >> 8) as u8 & 0x7F).max(raw as u8 & 0x7F);
    let device = path.split('/').find(|part| part.starts_with("dev_"))?;
    Some((device.trim_start_matches("dev_").replace('_', ":"), level))
}

async fn plan_now(audio: &std::sync::Arc<Audio>) -> Option<AudioPlan> {
    let audio = std::sync::Arc::clone(audio);
    tokio::task::spawn_blocking(move || audio.now().2).await.ok()
}

async fn reroute(
    audio: &std::sync::Arc<Audio>,
    player: &crate::player::PlayerManager,
    kodi: &crate::kodi::KodiClient,
    before: Option<AudioPlan>,
) -> Option<AudioPlan> {
    let after = plan_now(audio).await?;
    let id = |plan: &AudioPlan| plan.device.as_ref().map(|device| device.id.clone());
    let Some(before) = before else { return Some(after) };
    if id(&before) == id(&after) {
        return Some(after);
    }
    let label = |plan: &AudioPlan| plan.device.as_ref().map(|d| d.label.clone()).unwrap_or_else(|| "yok".into());
    eprintln!("mediaboxd.audio route: {} -> {}", label(&before), label(&after));
    player.apply_audio(&before, &after).await;
    kodi_follow(kodi, &after).await;
    let restore = std::sync::Arc::clone(audio);
    let _ = tokio::task::spawn_blocking(move || restore.restore()).await;
    Some(after)
}

/// Kodi's keymap for a Bluetooth headset (packaging/mediabox-kodi-bluetooth-keymap.xml).
pub const KODI_BLUETOOTH_KEYMAP: &str = "/opt/rk3588-mediabox/share/kodi/mediabox-bluetooth-keymap.xml";
/// Where it is put for Kodi to load: `userdata/keymaps/mediabox-bluetooth.xml`
/// is a link to this, made by kodi.service, filled by mediabox-hdmi-prepare
/// when Kodi starts and by [`follow_route`] while it runs.
pub const KODI_BLUETOOTH_KEYMAP_LIVE: &str = "/run/mediabox/kodi-bluetooth-keymap.xml";
/// A keymap that binds nothing: what the link points at when sound is not on
/// a headset, so it is never left dangling.
const EMPTY_KEYMAP: &str = "<keymap/>\n";
/// The step a volume key moves the box's volume by: the interface's own.
pub const KEY_STEP: i16 = 5;

/// Kodi's volume keys while sound is on a Bluetooth headset.
///
/// As Kodi does on Android (xbmc/platform/android/activity/AndroidKey.cpp:
/// AKEYCODE_VOLUME_UP, _DOWN and _MUTE map to XBMCK_LAST, left to the system),
/// the keys move the system's volume -- here the box's, which the headset
/// applies -- and Kodi's own gain is left as it is (XBMCApp.cpp
/// `onVolumeChanged`: "User wants to use kodi's internal volume freely").
/// The headset keymap hands them over as NotifyAll(mediabox, volume_*), and
/// the level is said on Kodi's screen, where the interface cannot draw.
/// With sound on HDMI or any other card the keymap binds nothing and Kodi
/// answers its keys itself, as it always did; a key that still arrives here
/// then is handed back to Kodi as its own action.
pub async fn follow_kodi_keys(
    audio: std::sync::Arc<Audio>,
    kodi: std::sync::Arc<crate::kodi::KodiClient>,
    events: std::net::SocketAddr,
) {
    use tokio::io::AsyncReadExt;
    loop {
        let Ok(mut stream) = tokio::net::TcpStream::connect(events).await else {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            continue;
        };
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(read) = stream.read(&mut chunk).await {
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            for message in crate::playback::split_json(&mut buffer) {
                let Some(key) = kodi_volume_key_of(&message) else { continue };
                let on_headset = audio
                    .active_device()
                    .is_some_and(|device| device.kind == mediabox_core::AudioKind::Bluetooth);
                if on_headset {
                    let change = match key {
                        "volume_up" => VolumeChange::Step(KEY_STEP),
                        "volume_down" => VolumeChange::Step(-KEY_STEP),
                        _ => VolumeChange::ToggleMute,
                    };
                    let step = std::sync::Arc::clone(&audio);
                    if let Ok(Ok(after)) = tokio::task::spawn_blocking(move || step.volume(change)).await {
                        kodi_says_volume(&kodi, &after);
                    }
                } else {
                    let action = match key {
                        "volume_up" => "volumeup",
                        "volume_down" => "volumedown",
                        _ => "mute",
                    };
                    let _ = kodi
                        .call("Input.ExecuteAction", Some(serde_json::json!({"action": action})))
                        .await;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// The volume key in one of Kodi's notifications, when it is one of ours.
fn kodi_volume_key_of(message: &serde_json::Value) -> Option<&'static str> {
    if message.pointer("/params/sender").and_then(serde_json::Value::as_str) != Some("mediabox") {
        return None;
    }
    match message.get("method").and_then(serde_json::Value::as_str)? {
        "Other.volume_up" => Some("volume_up"),
        "Other.volume_down" => Some("volume_down"),
        "Other.volume_mute" => Some("volume_mute"),
        _ => None,
    }
}

/// How often a level is said on Kodi's screen: Kodi shows a notification
/// for at least 1.5 s (TOAST_MESSAGE_TIME + 500) and queues every one that
/// differs from the last (xbmc/dialogs/GUIDialogKaiToast.cpp `AddToQueue`), so
/// a held key's twenty-odd levels played on for half a minute after it was
/// let go. At most one per this, and the latest at the end.
const KODI_NOTICE_EVERY: std::time::Duration = std::time::Duration::from_millis(1600);

/// The level, on Kodi's screen while Kodi holds the display: a notification,
/// since the system's indicator is the interface's and cannot be drawn there.
/// Throttled with the latest kept: the first change is said at once, later
/// ones at most every [`KODI_NOTICE_EVERY`], ending on the level reached.
fn kodi_says_volume(kodi: &std::sync::Arc<crate::kodi::KodiClient>, plan: &AudioPlan) {
    static NOTICE: std::sync::OnceLock<tokio::sync::watch::Sender<(u8, bool)>> = std::sync::OnceLock::new();
    let sender = NOTICE.get_or_init(|| {
        let (sender, mut levels) = tokio::sync::watch::channel((0u8, false));
        let kodi = std::sync::Arc::clone(kodi);
        tokio::spawn(async move {
            while levels.changed().await.is_ok() {
                let (volume, muted) = *levels.borrow_and_update();
                if kodi.ping().await.unwrap_or(false) {
                    let message = if muted { "Sessiz".to_string() } else { format!("%{volume}") };
                    let _ = kodi
                        .call(
                            "GUI.ShowNotification",
                            Some(serde_json::json!({"title": "Ses", "message": message, "displaytime": 1500})),
                        )
                        .await;
                }
                tokio::time::sleep(KODI_NOTICE_EVERY).await;
            }
        });
        sender
    });
    sender.send_replace((plan.volume, plan.muted));
}

/// What follows from where sound goes, kept in step with it every two
/// seconds: Kodi's headset keymap, and the headsets' media keys.
///
/// The keys: bluez makes each headset's AVRCP passthrough an input device,
/// `<name> (AVRCP)`, read by the interface and by Kodi alike. Android acts on
/// a headset's keys only while it is the active device (system/profile/avrcp/
/// device.cc: `if (IsActive()) SendKeyEvent`), so the others are inhibited
/// through the kernel's own switch, `/sys/class/input/inputN/inhibited`
/// (Linux 5.11), which silences a device for every reader. Polled, because
/// the input device comes a moment after the headset connects.
pub async fn follow_route(audio: std::sync::Arc<Audio>, kodi: std::sync::Arc<crate::kodi::KodiClient>) {
    let first = std::sync::Arc::clone(&audio);
    let _ = tokio::task::spawn_blocking(move || first.now()).await;
    let mut keymap_for: Option<Option<String>> = None;
    loop {
        let headset = audio
            .active_device()
            .filter(|device| device.kind == mediabox_core::AudioKind::Bluetooth)
            .map(|device| device.label);
        if keymap_for.as_ref() != Some(&headset) {
            let content = match headset {
                Some(_) => std::fs::read_to_string(KODI_BLUETOOTH_KEYMAP).unwrap_or_else(|_| EMPTY_KEYMAP.into()),
                None => EMPTY_KEYMAP.into(),
            };
            if crate::output::write_atomic(Path::new(KODI_BLUETOOTH_KEYMAP_LIVE), &content).is_ok() {
                keymap_for = Some(headset.clone());
                if kodi.ping().await.unwrap_or(false) {
                    let _ = kodi
                        .call("Input.ExecuteAction", Some(serde_json::json!({"action": "reloadkeymaps"})))
                        .await;
                }
            }
        }
        inhibit_idle_headsets(Path::new("/sys/class/input"), headset.as_deref());
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Inhibit every headset's AVRCP input but the one sound goes to.
fn inhibit_idle_headsets(inputs: &Path, active: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(inputs) else { return };
    for entry in entries.flatten() {
        let device = entry.path();
        let Ok(name) = std::fs::read_to_string(device.join("name")) else { continue };
        let Some(owner) = name.trim().strip_suffix(" (AVRCP)") else { continue };
        let wanted = if Some(owner) == active { "0" } else { "1" };
        let switch = device.join("inhibited");
        let now = std::fs::read_to_string(&switch).unwrap_or_default();
        if now.trim() != wanted {
            match std::fs::write(&switch, wanted) {
                Ok(()) => eprintln!(
                    "mediaboxd.audio {} media keys {}",
                    name.trim(),
                    if wanted == "1" { "inhibited: sound is not on it" } else { "live: sound is on it" }
                ),
                Err(error) => eprintln!("mediaboxd.audio {}: inhibit not set: {error}", name.trim()),
            }
        }
    }
}

/// Kodi's own settings for `plan`, through its JSON-RPC: nothing is patched.
///
/// The value of each device is taken from the options Kodi itself lists, by
/// the PCM it opens, so its own spelling of the name after `|` is kept. On a
/// headset passthrough is switched off -- Kodi sends a bitstream to its
/// passthrough device whatever its PCM device is, and that is the television
/// -- and back on when the plan sends a bitstream again.
async fn kodi_follow(kodi: &crate::kodi::KodiClient, plan: &AudioPlan) {
    use serde_json::{Value, json};
    let Some(device) = &plan.device else { return };
    if !kodi.ping().await.unwrap_or(false) {
        // Not running: mediabox-hdmi-prepare writes the plan when it starts.
        return;
    }
    let Ok(settings) = kodi
        .call(
            "Settings.GetSettings",
            Some(json!({"filter": {"section": "system", "category": "audio"}})),
        )
        .await
    else {
        return;
    };
    let options = |id: &str| -> Vec<String> {
        settings["settings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|setting| setting["id"] == id)
            .flat_map(|setting| setting["options"].as_array().cloned().unwrap_or_default())
            .filter_map(|option| option["value"].as_str().map(str::to_owned))
            .collect()
    };
    let pick = |id: &str, pcm: &str| {
        let wanted = format!("ALSA:{pcm}|");
        options(id).into_iter().find(|value| value.starts_with(&wanted))
    };
    let set = |setting: &str, value: Value| {
        kodi.call(
            "Settings.SetSettingValue",
            Some(json!({"setting": setting, "value": value})),
        )
    };
    let bluetooth = device.kind == mediabox_core::AudioKind::Bluetooth;
    let pcm = if bluetooth {
        "mediabox_bluetooth".to_string()
    } else {
        let dev = device.pcm.rsplit_once("DEV=").map(|(_, n)| n).unwrap_or("0");
        format!("mediabox_volume:CARD={},DEV={dev}", device.card_id)
    };
    if bluetooth {
        let _ = set("audiooutput.passthrough", json!(false)).await;
    }
    match pick("audiooutput.audiodevice", &pcm) {
        Some(value) => {
            if let Err(error) = set("audiooutput.audiodevice", json!(value)).await {
                eprintln!("mediaboxd.audio kodi device not set: {error}");
            }
        }
        None => eprintln!("mediaboxd.audio kodi does not list {pcm}"),
    }
    if !bluetooth && plan.bitstream() {
        if let Some(value) = pick("audiooutput.passthroughdevice", &device.pcm) {
            let _ = set("audiooutput.passthroughdevice", json!(value)).await;
        }
        let _ = set("audiooutput.passthrough", json!(true)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::{AudioCodec, AudioDeviceChoice, AudioKind, AudioMode, CapsSource, SadEntry, SinkAudio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn hdmi() -> AudioDevice {
        AudioDevice {
            id: "display:fdea0000.hdmi".into(),
            label: "HDMI-A-2 · SONY TV".into(),
            kind: AudioKind::Hdmi,
            card_id: "rockchiphdmi1".into(),
            card_index: Some(2),
            pcm: "hdmi:CARD=rockchiphdmi1,DEV=0".into(),
            bitstream: true,
            connector: Some("HDMI-A-2".into()),
            display: true,
            sink: Some(SinkAudio {
                source: CapsSource::Edid,
                entries: vec![SadEntry {
                    coding: 2,
                    name: "AC-3".into(),
                    channels: 6,
                    rates: vec![48_000],
                    max_kbps: Some(640),
                }],
                speakers: Vec::new(),
            }),
        }
    }

    fn rig(dir: &Path) -> (Audio, Arc<AtomicU32>) {
        let softvol = Arc::new(AtomicU32::new(u32::MAX));
        let seen = Arc::clone(&softvol);
        let audio = Audio::new(dir.join("audio.json"), || vec![hdmi()], move |plan: &AudioPlan| {
            seen.store(if plan.muted { 0 } else { u32::from(plan.volume) }, Ordering::SeqCst);
        });
        (audio, softvol)
    }

    #[test]
    fn kodis_volume_keys_are_ours_only() {
        let key = |method: &str, sender: &str| {
            kodi_volume_key_of(&serde_json::json!({"jsonrpc": "2.0", "method": method, "params": {"sender": sender, "data": null}}))
        };
        assert_eq!(key("Other.volume_up", "mediabox"), Some("volume_up"));
        assert_eq!(key("Other.volume_mute", "mediabox"), Some("volume_mute"));
        assert_eq!(key("Other.volume_up", "script.foo"), None);
        assert_eq!(key("Application.OnVolumeChanged", "xbmc"), None);
    }

    #[test]
    fn only_the_headset_sound_is_on_keeps_its_media_keys() {
        let dir = tempfile::tempdir().unwrap();
        let input = |n: &str, name: &str| {
            let d = dir.path().join(n);
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(d.join("inhibited"), "0\n").unwrap();
            d
        };
        let buds = input("input35", "Arctis GameBuds (AVRCP)");
        let other = input("input36", "Speaker (AVRCP)");
        let keyboard = input("input20", "ROG AZOTH Keyboard");
        let read = |d: &std::path::Path| std::fs::read_to_string(d.join("inhibited")).unwrap().trim().to_string();
        inhibit_idle_headsets(dir.path(), Some("Arctis GameBuds"));
        assert_eq!((read(&buds), read(&other), read(&keyboard)), ("0".into(), "1".into(), "0".into()));
        // Sound back on HDMI: every headset's keys are silenced, nothing else.
        inhibit_idle_headsets(dir.path(), None);
        assert_eq!((read(&buds), read(&other), read(&keyboard)), ("1".into(), "1".into(), "0".into()));
    }

    #[test]
    fn answers_to_what_was_sent_are_not_presses_even_late() {
        let mut sync = HeadsetSync::default();
        let t0 = std::time::Instant::now();
        let at = |ms: u64| t0 + std::time::Duration::from_millis(ms);
        let a = "20:AF:1B:19:E8:D2";
        // Five levels sent as a held key sends them, each reported three
        // times as the Plus reported them: at once, 6 ms and 60 ms later --
        // the last copy after the next level has gone out.
        let levels = [51u8, 44, 38, 32, 25];
        for (i, level) in levels.iter().enumerate() {
            let sent = i as u64 * 130;
            assert!(sync.send(a, *level, at(sent)));
            for late in [0, 6, 160] {
                assert!(!sync.reported(a, *level, at(sent + late)), "{level} +{late} ms");
            }
        }
        // The headset's own press is one, and is not sent back to it.
        assert!(sync.reported(a, 19, at(1000)));
        assert!(!sync.send(a, 19, at(1010)));
        // A level sent more than two seconds ago is no longer an answer.
        assert!(sync.reported(a, 51, at(2500)));
    }

    #[test]
    fn steps_that_arrive_together_are_all_taken() {
        let dir = tempfile::tempdir().unwrap();
        let audio = std::sync::Arc::new(Audio::new(dir.path().join("audio.json"), || vec![hdmi()], |_| {}));
        audio.volume(VolumeChange::Set(100)).unwrap();
        // A held key: twenty steps down at once.
        let threads: Vec<_> = (0..20)
            .map(|_| {
                let audio = std::sync::Arc::clone(&audio);
                std::thread::spawn(move || audio.volume(VolumeChange::Step(-5)).unwrap())
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(audio.setting().volume, 0);
    }

    #[test]
    fn a_headset_press_is_kept_and_not_said_back() {
        let dir = tempfile::tempdir().unwrap();
        let headset = AudioDevice {
            id: "bluetooth:20:AF:1B:19:E8:D2".into(),
            label: "Arctis GameBuds".into(),
            kind: mediabox_core::AudioKind::Bluetooth,
            card_id: "bluealsa".into(),
            card_index: None,
            pcm: "bluealsa:DEV=20:AF:1B:19:E8:D2,PROFILE=a2dp".into(),
            bitstream: false,
            connector: None,
            display: false,
            sink: None,
        };
        let writes = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = std::sync::Arc::clone(&writes);
        let audio = Audio::new(dir.path().join("audio.json"), move || vec![hdmi(), headset.clone()], move |_: &AudioPlan| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(audio.headset_volume("20:AF:1B:19:E8:D2", 76).is_some());
        assert_eq!(audio.setting().volume, 60);
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing said back");
    }

    #[test]
    fn a_headsets_volume_is_read_off_bluealsa() {
        // As `bluealsa-cli monitor --properties` printed it on the Plus.
        let line = "PropertyChanged /org/bluealsa/hci0/dev_20_AF_1B_19_E8_D2/a2dpsrc/sink Volume 0x4C4C";
        assert_eq!(headset_level(line), Some(("20:AF:1B:19:E8:D2".into(), 76)));
        // Muted channels carry the top bit; the level is what is under it.
        let muted = "PropertyChanged /org/bluealsa/hci0/dev_20_AF_1B_19_E8_D2/a2dpsrc/sink Volume 0xCCCC";
        assert_eq!(headset_level(muted).unwrap().1, 76);
        assert!(headset_level("PropertyChanged /org/bluealsa/hci0/dev_20_AF_1B_19_E8_D2/a2dpsrc/sink Running true").is_none());
        assert!(headset_level("PCMAdded /org/bluealsa/hci0/dev_20_AF_1B_19_E8_D2/a2dpsrc/sink").is_none());
    }

    #[test]
    fn a_headset_moves_the_volume_only_when_it_is_the_one_playing() {
        let dir = tempfile::tempdir().unwrap();
        let headset = AudioDevice {
            id: "bluetooth:20:AF:1B:19:E8:D2".into(),
            label: "Arctis GameBuds".into(),
            kind: mediabox_core::AudioKind::Bluetooth,
            card_id: "bluealsa".into(),
            card_index: None,
            pcm: "bluealsa:DEV=20:AF:1B:19:E8:D2,PROFILE=a2dp".into(),
            bitstream: false,
            connector: None,
            display: false,
            sink: None,
        };
        let devices = vec![hdmi(), headset];
        let audio = Audio::new(dir.path().join("audio.json"), move || devices.clone(), |_| {});
        let mut events = audio.subscribe();
        // Playing on it ("Otomatik"): 76 of 127 is 60 here, and is said.
        assert!(audio.headset_volume("20:AF:1B:19:E8:D2", 76).is_some());
        assert_eq!(audio.setting().volume, 60);
        assert_eq!(events.try_recv().unwrap(), mediabox_core::AudioEvent::Volume { volume: 60, muted: false });
        // Its own level coming back changes nothing and says nothing.
        assert!(audio.headset_volume("20:AF:1B:19:E8:D2", mediabox_core::audio::to_avrcp(60)).is_none());
        assert!(events.try_recv().is_err());
        // Another device's buttons do not move this one.
        assert!(audio.headset_volume("11:22:33:44:55:66", 10).is_none());
        // Nor the headset's while the sound is on HDMI by choice.
        let mut hdmi_chosen = audio.setting();
        hdmi_chosen.device = mediabox_core::AudioDeviceChoice::Device { id: hdmi().id };
        audio.set(hdmi_chosen).unwrap();
        assert!(audio.headset_volume("20:AF:1B:19:E8:D2", 17).is_none());
        assert_eq!(audio.setting().volume, 60);
    }

    #[test]
    fn a_setting_is_written_before_it_is_in_force_and_survives_a_restart() {
        let dir = tempfile::TempDir::new().unwrap();
        let (audio, softvol) = rig(dir.path());
        assert_eq!(audio.setting(), AudioSetting::default());
        let wanted = AudioSetting { ac3_transcode: true, volume: 60, ..Default::default() };
        let (before, after) = audio.set(wanted.clone()).unwrap();
        assert!(before.ac3_encode.is_none());
        assert!(after.ac3_encode.is_some());
        assert_eq!(softvol.load(Ordering::SeqCst), 60);
        let (again, _) = rig(dir.path());
        assert_eq!(again.setting(), wanted);
    }

    #[test]
    fn what_the_sink_does_not_declare_is_refused_and_nothing_changes() {
        let dir = tempfile::TempDir::new().unwrap();
        let (audio, _) = rig(dir.path());
        let dts = AudioSetting {
            mode: AudioMode::Passthrough,
            formats: Some(vec![AudioCodec::Dts]),
            ..Default::default()
        };
        assert!(audio.set(dts).is_err());
        let gone = AudioSetting {
            device: AudioDeviceChoice::Device { id: "usb:nothing".into() },
            ..Default::default()
        };
        assert!(audio.set(gone).is_err());
        assert_eq!(audio.setting(), AudioSetting::default());
        assert!(!dir.path().join("audio.json").exists());
    }

    #[test]
    fn the_volume_steps_mutes_and_turning_it_up_unmutes() {
        let dir = tempfile::TempDir::new().unwrap();
        let (audio, softvol) = rig(dir.path());
        audio.volume(VolumeChange::Step(-30)).unwrap();
        assert_eq!(audio.setting().volume, 70);
        audio.volume(VolumeChange::ToggleMute).unwrap();
        assert!(audio.setting().muted);
        assert_eq!(softvol.load(Ordering::SeqCst), 0, "muted is silence on the control");
        audio.volume(VolumeChange::Step(5)).unwrap();
        assert!(!audio.setting().muted);
        assert_eq!(audio.setting().volume, 75);
        audio.volume(VolumeChange::Step(200)).unwrap();
        assert_eq!(audio.setting().volume, 100);
        audio.volume(VolumeChange::Set(250)).unwrap();
        assert_eq!(audio.setting().volume, 100);
    }
}
