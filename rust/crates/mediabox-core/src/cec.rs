//! What the HDMI-CEC panel can be set to, and the rule that keeps it coherent.
//!
//! The settings are AOSP's for a playback device (`HdmiCecConfig`), renamed
//! into what a person reads on the television:
//!
//! | here                        | AOSP                                                  |
//! |-----------------------------|-------------------------------------------------------|
//! | `enabled`                   | `hdmi_cec_enabled`                                    |
//! | `remote_control`            | none: AOSP always takes `<User Control Pressed>`      |
//! | `wake_tv_on_start`          | One Touch Play's `<Image View On>` half, on boot      |
//! | `active_source_on_start`    | One Touch Play's `<Active Source>` half, on boot      |
//! | `standby_tv_on_shutdown`    | `power_control_mode` other than `none`                |
//! | `restore_power_on_shutdown` | none: ours, built on `<Give Device Power Status>`     |
//! | `power_target`              | `power_control_mode` (`to_tv`, `..._audio_system`, `broadcast`) |
//! | `on_active_source_lost`     | `power_state_change_on_active_source_lost`            |
//!
//! Every sub-setting is kept while `enabled` is off: turning CEC back on
//! brings every one of them back as it was, which is why the master switch is
//! a field beside them rather than a reset of them.

use serde::{Deserialize, Serialize};

/// Who `<Standby>` goes to when this box sends one.
///
/// AOSP's `power_control_mode`. A television and an audio system are the two
/// devices a playback device may address by type: logical address 0 is always
/// the television, 5 always the audio system. "Everything" is the broadcast
/// address, which every device on the bus acts on — another player, a
/// recorder in the middle of a recording — so it is never a default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CecPowerTarget {
    #[default]
    Tv,
    TvAndAudioSystem,
    Broadcast,
}

impl CecPowerTarget {
    pub const ALL: [CecPowerTarget; 3] = [
        CecPowerTarget::Tv,
        CecPowerTarget::TvAndAudioSystem,
        CecPowerTarget::Broadcast,
    ];

    /// The destination logical addresses of a `<Standby>`.
    pub fn destinations(self) -> &'static [u8] {
        match self {
            CecPowerTarget::Tv => &[0],
            CecPowerTarget::TvAndAudioSystem => &[0, 5],
            CecPowerTarget::Broadcast => &[15],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CecPowerTarget::Tv => "TV",
            CecPowerTarget::TvAndAudioSystem => "TV + ses sistemi",
            CecPowerTarget::Broadcast => "Tüm cihazlar",
        }
    }

    pub fn line(self) -> &'static str {
        match self {
            CecPowerTarget::Tv => "Yalnız televizyon",
            CecPowerTarget::TvAndAudioSystem => "Televizyon ve ses sistemi",
            CecPowerTarget::Broadcast => "HDMI'daki tüm cihazlar",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tv" => Some(CecPowerTarget::Tv),
            "tv_and_audio_system" | "tv-audio" | "tv+audio" => {
                Some(CecPowerTarget::TvAndAudioSystem)
            }
            "broadcast" | "all" => Some(CecPowerTarget::Broadcast),
            _ => None,
        }
    }
}

/// What this box does when the television is switched to another input.
///
/// AOSP's `power_state_change_on_active_source_lost`: `none` or
/// `standby_now`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CecSourceLost {
    #[default]
    StayOn,
    Standby,
}

impl CecSourceLost {
    pub fn label(self) -> &'static str {
        match self {
            CecSourceLost::StayOn => "Açık kal",
            CecSourceLost::Standby => "Beklemeye geç",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stay_on" | "stay-on" | "none" => Some(CecSourceLost::StayOn),
            "standby" | "standby_now" => Some(CecSourceLost::Standby),
            _ => None,
        }
    }
}

pub const CEC_SETTINGS_SCHEMA: u32 = 1;

/// The panel, as kept on disk and as every interface sends it.
///
/// The defaults are the box's behaviour before this panel existed: CEC on,
/// the remote on, and no power or routing message this box sends on its own.
/// A message that turns a television on or off, or switches its input, is
/// sent only when somebody has said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CecSettings {
    pub enabled: bool,
    pub remote_control: bool,
    pub wake_tv_on_start: bool,
    pub active_source_on_start: bool,
    pub standby_tv_on_shutdown: bool,
    pub restore_power_on_shutdown: bool,
    pub power_target: CecPowerTarget,
    pub on_active_source_lost: CecSourceLost,
}

impl Default for CecSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            remote_control: true,
            wake_tv_on_start: false,
            active_source_on_start: false,
            standby_tv_on_shutdown: false,
            restore_power_on_shutdown: false,
            power_target: CecPowerTarget::Tv,
            on_active_source_lost: CecSourceLost::StayOn,
        }
    }
}

/// Why a set of settings was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CecSettingsError {
    /// "Turn the television off" and "put it back as it was" are two answers
    /// to the same question; both on would make the second meaningless.
    ShutdownConflict,
}

impl std::fmt::Display for CecSettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CecSettingsError::ShutdownConflict => f.write_str(
                "'MediaBox kapanırken TV'yi kapat' ile 'Kapanırken önceki güç durumuna dön' \
                 aynı anda açık olamaz",
            ),
        }
    }
}

impl std::error::Error for CecSettingsError {}

impl CecSettings {
    pub fn validate(&self) -> Result<(), CecSettingsError> {
        if self.standby_tv_on_shutdown && self.restore_power_on_shutdown {
            return Err(CecSettingsError::ShutdownConflict);
        }
        Ok(())
    }

    /// One field changed, the way a panel row changes it: turning one of the
    /// two shutdown answers on turns the other off, so a row press never
    /// produces a pair the daemon refuses.
    pub fn with(mut self, change: CecChange) -> Self {
        match change {
            CecChange::Enabled(on) => self.enabled = on,
            CecChange::RemoteControl(on) => self.remote_control = on,
            CecChange::WakeTvOnStart(on) => self.wake_tv_on_start = on,
            CecChange::ActiveSourceOnStart(on) => self.active_source_on_start = on,
            CecChange::StandbyTvOnShutdown(on) => {
                self.standby_tv_on_shutdown = on;
                if on {
                    self.restore_power_on_shutdown = false;
                }
            }
            CecChange::RestorePowerOnShutdown(on) => {
                self.restore_power_on_shutdown = on;
                if on {
                    self.standby_tv_on_shutdown = false;
                }
            }
            CecChange::PowerTarget(target) => self.power_target = target,
            CecChange::OnActiveSourceLost(lost) => self.on_active_source_lost = lost,
        }
        self
    }

    /// Whether this box sends a power message at shutdown at all, and so
    /// whether "power target" means anything.
    pub fn powers_off(&self) -> bool {
        self.standby_tv_on_shutdown || self.restore_power_on_shutdown
    }
}

/// A single row's change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CecChange {
    Enabled(bool),
    RemoteControl(bool),
    WakeTvOnStart(bool),
    ActiveSourceOnStart(bool),
    StandbyTvOnShutdown(bool),
    RestorePowerOnShutdown(bool),
    PowerTarget(CecPowerTarget),
    OnActiveSourceLost(CecSourceLost),
}

/// The television's power state as `<Report Power Status>` gives it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TvPower {
    On,
    Standby,
    /// It did not answer, answered with a transition (2: standby to on,
    /// 3: on to standby), or was never asked. Nothing is restored from this.
    #[default]
    Unknown,
}

impl TvPower {
    /// CEC 1.4 table 26, `[Power Status]`.
    pub fn from_report(status: u8) -> Self {
        match status {
            0 => TvPower::On,
            1 => TvPower::Standby,
            _ => TvPower::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TvPower::On => "Açık",
            TvPower::Standby => "Beklemede",
            TvPower::Unknown => "Bilinmiyor",
        }
    }
}

/// Who the television is showing, as far as the bus has said.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveSource {
    /// This box: it announced itself, or the television routed to it.
    Us,
    /// Another device announced itself, or the television routed away.
    Other,
    #[default]
    Unknown,
}

/// What this boot has done, kept so a restart of the daemon does not forget
/// that it was this box that woke the television.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CecSession {
    /// The television's state before this box sent anything, this boot.
    pub tv_power_at_start: TvPower,
    /// Whether this box sent `<Image View On>` at start.
    pub woke_tv: bool,
    /// Whether the start sequence has run (or was not wanted) this boot.
    pub start_done: bool,
    pub active_source: ActiveSource,
    /// The last `<Report Power Status>` the television sent.
    pub tv_power: TvPower,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_send_nothing_on_their_own() {
        let settings = CecSettings::default();
        assert!(settings.enabled && settings.remote_control);
        assert!(!settings.wake_tv_on_start && !settings.active_source_on_start);
        assert!(!settings.powers_off());
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn both_shutdown_answers_are_refused_with_the_reason() {
        let both = CecSettings {
            standby_tv_on_shutdown: true,
            restore_power_on_shutdown: true,
            ..Default::default()
        };
        let error = both.validate().unwrap_err();
        assert!(error.to_string().contains("aynı anda"));
    }

    #[test]
    fn a_row_press_turns_the_other_shutdown_answer_off() {
        let one = CecSettings::default().with(CecChange::StandbyTvOnShutdown(true));
        let other = one.with(CecChange::RestorePowerOnShutdown(true));
        assert!(other.restore_power_on_shutdown && !other.standby_tv_on_shutdown);
        assert!(other.validate().is_ok());
        let back = other.with(CecChange::StandbyTvOnShutdown(true));
        assert!(back.standby_tv_on_shutdown && !back.restore_power_on_shutdown);
    }

    #[test]
    fn the_master_switch_keeps_every_preference() {
        let chosen = CecSettings {
            wake_tv_on_start: true,
            active_source_on_start: true,
            restore_power_on_shutdown: true,
            power_target: CecPowerTarget::TvAndAudioSystem,
            on_active_source_lost: CecSourceLost::Standby,
            remote_control: false,
            ..Default::default()
        };
        let off = chosen.with(CecChange::Enabled(false));
        assert!(!off.enabled);
        let on = off.with(CecChange::Enabled(true));
        assert_eq!(on, chosen);
    }

    #[test]
    fn targets_are_the_standard_addresses() {
        assert_eq!(CecPowerTarget::Tv.destinations(), [0]);
        assert_eq!(CecPowerTarget::TvAndAudioSystem.destinations(), [0, 5]);
        assert_eq!(CecPowerTarget::Broadcast.destinations(), [15]);
    }

    #[test]
    fn a_file_from_an_older_build_reads_with_defaults() {
        let settings: CecSettings = serde_json::from_str(r#"{"wake_tv_on_start":true}"#).unwrap();
        assert!(settings.enabled && settings.wake_tv_on_start);
        let round: CecSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(round, settings);
    }

    #[test]
    fn only_definite_reports_are_states() {
        assert_eq!(TvPower::from_report(0), TvPower::On);
        assert_eq!(TvPower::from_report(1), TvPower::Standby);
        assert_eq!(TvPower::from_report(2), TvPower::Unknown);
        assert_eq!(TvPower::from_report(3), TvPower::Unknown);
    }
}
