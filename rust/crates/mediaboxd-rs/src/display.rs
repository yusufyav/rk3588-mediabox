//! What the television can be sent, and what it has been told to be sent.
//!
//! The measuring is not here -- `mediabox_platform::video` reads the EDID and
//! does the arithmetic. What is here is the part that has to be the daemon's:
//! finding the connected output without anybody naming a connector, and
//! remembering a person's choice somewhere that survives a power cut.
//!
//! It is the daemon's for the same two reasons the indicator lights are. The
//! unit that draws the television mounts /sys read-only, so the interface
//! cannot read an EDID back after a hotplug; and a choice made from the sofa
//! belongs in the daemon's state directory, which systemd creates with the
//! right owner before anything runs.
//!
//! Why this exists at all: a Sony KD-65XE9005 reports its HDMI 1 as a 300 MHz
//! port and its HDMI 3 as a 600 MHz one. At 4K a film needs ten bits to carry
//! HDR10 without banding, and ten-bit RGB does not fit down the first of those.
//! Ask for it anyway and the driver subsamples to 4:2:2 without saying so,
//! while the sink is still told RGB -- and the television decodes YCbCr pixels
//! with an RGB matrix. The picture that comes back is the reason every number
//! below is measured rather than assumed.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mediabox_core::{
    ColorChoice, ColorModeOption, DisplayColorStatus, ResolutionChoice, TimingOption,
};
use mediabox_platform::video::{parse_sink_video, parse_timings};
use mediabox_platform::{Devices, Platform};

/// Where the chosen colour mode is remembered. Beside the indicator-light mode,
/// in the daemon's own `StateDirectory`; this is not a new store.
pub const STATE_FILE: &str = "/var/lib/mediabox/color-mode";

/// Where the chosen resolution is remembered, beside the colour mode. The
/// interface reads it when it sets its mode.
pub const RESOLUTION_FILE: &str = "/var/lib/mediabox/resolution";

pub struct DisplayColor {
    state: PathBuf,
    choice: Mutex<ColorChoice>,
    resolution_state: PathBuf,
    resolution: Mutex<ResolutionChoice>,
}

impl DisplayColor {
    /// Restores the remembered choice. A state file that cannot be read is a
    /// box that has never been told, which is `Auto` -- never an error that
    /// stops the daemon starting.
    pub fn new(state: impl Into<PathBuf>) -> Self {
        let state = state.into();
        let choice = read_state(&state).unwrap_or_default();
        let resolution_state = state.with_file_name("resolution");
        let resolution = read_resolution(&resolution_state).unwrap_or_default();
        Self {
            state,
            choice: Mutex::new(choice),
            resolution_state,
            resolution: Mutex::new(resolution),
        }
    }

    pub fn choice(&self) -> ColorChoice {
        *self.choice.lock().expect("colour choice")
    }

    pub fn resolution(&self) -> ResolutionChoice {
        *self.resolution.lock().expect("resolution choice")
    }

    /// Remembers a resolution choice. The interface applies it when it next
    /// sets its mode; the daemon restarts it for that.
    pub fn set_resolution(&self, choice: ResolutionChoice) -> Result<(), String> {
        write_resolution(&self.resolution_state, &choice)?;
        *self.resolution.lock().expect("resolution choice") = choice;
        Ok(())
    }

    /// Remembers a choice.
    ///
    /// Applying it is not this function's job and deliberately so: the link is
    /// established by whatever is drawing at the time, and a daemon that wrote
    /// connector properties underneath a running player would be fighting it.
    /// What is stored here is the policy the player reads.
    pub fn set(&self, choice: ColorChoice) -> Result<(), String> {
        if let Some(parent) = self.state.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let text = serde_json::to_string(&choice).map_err(|error| error.to_string())?;
        std::fs::write(&self.state, format!("{text}\n")).map_err(|error| error.to_string())?;
        *self.choice.lock().expect("colour choice") = choice;
        Ok(())
    }

    /// The measured answer for whatever is plugged in right now.
    pub fn status(&self) -> DisplayColorStatus {
        self.status_for(&Platform::discover())
    }

    /// The same, against a platform that has already been inspected -- or a
    /// captured one, which is how this is tested without a television.
    pub fn status_for(&self, platform: &Platform) -> DisplayColorStatus {
        let choice = self.choice();
        let devices = Devices::from(platform);
        let mut status = DisplayColorStatus {
            choice,
            resolution: self.resolution(),
            connector: devices.connector.clone(),
            ..Default::default()
        };

        let Some(path) = devices.connector_sysfs.clone() else {
            status.error = Some("bağlı bir çıkış yok".into());
            return status;
        };
        let edid = std::fs::read(path.join("edid")).unwrap_or_default();
        let Some(sink) = parse_sink_video(&edid) else {
            status.error = Some(format!(
                "{} üzerinde okunabilir bir EDID yok",
                path.display()
            ));
            return status;
        };

        status.sink = platform
            .selected_output()
            .and_then(|output| output.connector.sink.as_ref())
            .map(|sink| match &sink.name {
                Some(name) => format!("{} {}", sink.manufacturer, name.trim()),
                None => sink.manufacturer.clone(),
            });
        status.max_character_rate_khz = sink.max_character_rate_khz;
        status.rate_is_declared = sink.rate_is_declared;
        status.st2084 = sink.st2084;
        status.hlg = sink.hlg;

        for timing in parse_timings(&edid) {
            let allowed: Vec<ColorModeOption> = sink
                .modes_for(&timing)
                .into_iter()
                .map(|mode| ColorModeOption {
                    label: mode.label(),
                    character_rate_khz: mode.character_rate_khz(timing.pixel_clock_khz),
                    carries_hdr: mode.carries_hdr(),
                    mode,
                })
                .collect();
            // A timing nothing can be sent at is a timing worth leaving out
            // rather than drawing as an empty row.
            if allowed.is_empty() {
                continue;
            }
            status.timings.push(TimingOption {
                label: timing.label(),
                width: timing.width,
                height: timing.height,
                refresh_mhz: timing.refresh_mhz,
                pixel_clock_khz: timing.pixel_clock_khz,
                hdr10_fits: sink.hdr10_fits(&timing),
                auto_sdr: sink.best_for(&timing, false),
                auto_hdr: sink.best_for(&timing, true),
                allowed,
            });
        }

        status.available = !status.timings.is_empty();
        if !status.available {
            status.error = Some("bu bağlantıya gönderilebilecek bir mod yok".into());
        }
        status
    }
}

fn read_state(path: &Path) -> Option<ColorChoice> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim()).ok()
}

fn read_resolution(path: &Path) -> Option<ResolutionChoice> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim()).ok()
}

fn write_resolution(path: &Path, value: &ResolutionChoice) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string(value).map_err(|error| error.to_string())?;
    std::fs::write(path, format!("{text}\n")).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::{ColorFormat, ColorMode};

    #[test]
    fn a_box_that_has_never_been_told_is_on_auto() {
        let dir = tempfile::TempDir::new().unwrap();
        let colour = DisplayColor::new(dir.path().join("color-mode"));
        assert_eq!(colour.choice(), ColorChoice::Auto);
    }

    #[test]
    fn a_choice_survives_a_restart() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("color-mode");
        let colour = DisplayColor::new(&path);
        let chosen = ColorChoice::Fixed {
            format: ColorFormat::Ycbcr422,
            bits: 12,
        };
        colour.set(chosen).expect("write");

        // A second daemon, started fresh against the same state.
        let again = DisplayColor::new(&path);
        assert_eq!(again.choice(), chosen);
        assert_eq!(
            again.choice().mode(),
            Some(ColorMode::new(ColorFormat::Ycbcr422, 12))
        );
    }

    #[test]
    fn rubbish_in_the_state_file_is_auto_rather_than_a_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("color-mode");
        std::fs::write(&path, "not json at all\n").unwrap();
        assert_eq!(DisplayColor::new(&path).choice(), ColorChoice::Auto);
    }

    #[test]
    fn a_board_with_nothing_plugged_in_says_so() {
        let dir = tempfile::TempDir::new().unwrap();
        let colour = DisplayColor::new(dir.path().join("color-mode"));
        // An empty captured tree: no DRM devices, so no connector.
        let roots = mediabox_platform::Roots::under(dir.path());
        let platform = Platform::inspect(&roots, &Default::default());
        let status = colour.status_for(&platform);
        assert!(!status.available);
        assert!(status.error.is_some());
        assert_eq!(status.choice, ColorChoice::Auto);
    }
}
