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
        }
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
                mediabox_platform::sound::devices(
                    &platform,
                    &roots,
                    Path::new(mediabox_platform::sound::ALSA_CARDS),
                )
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
        (setting, devices, plan)
    }

    /// Keep `wanted`, if it can be chosen on the devices here: written down
    /// first, then in force. Returns the plan before and after.
    pub fn set(&self, wanted: AudioSetting) -> Result<(AudioPlan, AudioPlan), String> {
        let devices = (self.discover)();
        let mut kept = self.lock();
        mediabox_core::audio::check(&wanted, &devices, &kept)?;
        let before = mediabox_core::audio::plan(&kept, &devices);
        crate::output::write_atomic(&self.setting_path, &wanted.to_stored())
            .map_err(|error| format!("Ses ayarı kaydedilemedi: {error}"))?;
        *kept = wanted;
        let after = mediabox_core::audio::plan(&kept, &devices);
        drop(kept);
        (self.softvol)(&after);
        Ok((before, after))
    }

    /// The volume alone. Kept like the rest: a volume that comes back as
    /// something else after a restart is not a volume.
    pub fn volume(&self, change: VolumeChange) -> Result<AudioPlan, String> {
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
        self.set(wanted).map(|(_, after)| after)
    }

    /// Put the kept volume back on the softvol control: at start, and when
    /// the device it lives on has come back.
    pub fn restore(&self) {
        let (_, _, plan) = self.now();
        (self.softvol)(&plan);
    }
}

/// `amixer -c <card> cset name='MediaBox' <n>`: 0 to 100 on a control
/// declared with that resolution, and 0 when muted -- which softvol plays as
/// silence. Quiet when the control is not there yet: it is created by the
/// first stream through `mediabox_volume`, and `mediabox-hdmi-prepare` opens
/// one before anything plays so that it is.
fn set_softvol(plan: &AudioPlan) {
    let Some(device) = &plan.device else { return };
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
