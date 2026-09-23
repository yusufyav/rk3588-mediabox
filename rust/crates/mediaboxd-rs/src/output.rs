//! The display setting: what a person chose, the display it was chosen on, and
//! a new choice on trial.
//!
//! What can be chosen is not worked out here. The interface holds the display
//! and is the only reader of its EDID -- this vendor driver leaves
//! `/sys/class/drm/*/edid` empty -- so it reports the offer
//! (`mediabox_platform::output`) whenever it sets a mode. What is here is the
//! part that has to outlive the interface:
//!
//! * the kept setting, one record bound to the display's EDID checkvalue --
//!   the reference Android box's model (`hdmimode`, `<mode>_deepcolor`,
//!   `hdmichecksum`): a different display is `Auto`, and the record moves to
//!   it;
//! * the trial: a new setting is applied and not written down; unless it is
//!   kept within [`OUTPUT_TRIAL_SECONDS`] the earlier one is put back. The
//!   clock is the daemon's, so a display that shows nothing, or an interface
//!   that dies, still comes back to the earlier setting -- and one that
//!   restarts reads the kept setting from disk, which the trial never
//!   reached;
//! * the plan the other owners of the display start from, written for
//!   `mediabox-hdmi-prepare`: Kodi's mode and refresh list, and the browser's
//!   mode.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mediabox_core::{
    ColorMode, OUTPUT_TRIAL_SECONDS, OutputEvent, OutputOffer, OutputSetting, OutputStatus,
    OutputTrial, OutputWire, ResolutionChoice,
};
use tokio::sync::broadcast;

/// The kept setting, in the daemon's own `StateDirectory`.
pub const SETTING_FILE: &str = "/var/lib/mediabox/output.json";

/// The plan `mediabox-hdmi-prepare` gives Kodi and the browser, as shell
/// assignments. Runtime state: it is written again from the next report.
pub const PLAN_FILE: &str = "/run/mediabox/output-plan";

/// The display controller's own account of what is on the wire.
pub const SUMMARY_FILE: &str = "/sys/kernel/debug/dri/0/summary";

struct Trial {
    setting: OutputSetting,
    previous: OutputSetting,
    deadline: Instant,
    id: u64,
}

#[derive(Default)]
struct State {
    kept: OutputSetting,
    offer: Option<OutputOffer>,
    wire: Option<OutputWire>,
    trial: Option<Trial>,
    next_trial: u64,
}

pub struct Output {
    setting_path: PathBuf,
    plan_path: PathBuf,
    summary_path: PathBuf,
    state: Mutex<State>,
    events: broadcast::Sender<OutputEvent>,
}

impl Output {
    /// Restores the kept setting. A file that cannot be read is a box that
    /// has never been told, which is `Auto` -- never an error that stops the
    /// daemon starting.
    pub fn new(
        setting_path: impl Into<PathBuf>,
        plan_path: impl Into<PathBuf>,
        summary_path: impl Into<PathBuf>,
    ) -> Arc<Self> {
        let setting_path = setting_path.into();
        let kept = read_setting(&setting_path).unwrap_or_default();
        let (events, _) = broadcast::channel(16);
        Arc::new(Self {
            setting_path,
            plan_path: plan_path.into(),
            summary_path: summary_path.into(),
            state: Mutex::new(State {
                kept,
                ..Default::default()
            }),
            events,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OutputEvent> {
        self.events.subscribe()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> OutputStatus {
        let state = self.state();
        let mut status = OutputStatus {
            setting: match &state.offer {
                Some(offer) => state.kept.for_sink(&offer.sink),
                None => state.kept.clone(),
            },
            offer: state.offer.clone(),
            trial: state.trial.as_ref().map(|trial| OutputTrial {
                setting: trial.setting.clone(),
                previous: trial.previous.clone(),
                seconds_left: trial
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f32()
                    .ceil() as u32,
            }),
            wire: state.wire.clone(),
            error: None,
        };
        // The bus format as the display controller reports it now, whoever
        // set it: Kodi changes the wire without telling anybody.
        if let (Some(wire), Some(offer)) = (status.wire.as_mut(), status.offer.as_ref())
            && let Ok(summary) = std::fs::read_to_string(&self.summary_path)
            && let Some((bus, colour)) =
                mediabox_platform::output::wire_bus_format(&summary, &offer.connector)
        {
            wire.bus_format = Some(bus);
            if colour.is_some() {
                wire.colour = colour;
            }
        }
        if status.offer.is_none() {
            status.error = Some("Arayüz bir ekran bildirmedi".into());
        }
        status
    }

    /// The interface set a mode and says what it sees.
    ///
    /// A display whose EDID is not the one the kept setting was made on is
    /// the reference box's "tv sink changed": the setting becomes `Auto` for
    /// the new display and is written down as such, and a trial for the old
    /// one is dropped.
    pub fn report(&self, offer: OutputOffer, wire: OutputWire) {
        let mut state = self.state();
        if state.kept.sink != offer.sink {
            let fresh = state.kept.for_sink(&offer.sink);
            if !state.kept.sink.is_empty() {
                eprintln!(
                    "mediaboxd-rs: output sink changed ({} -> {}); the setting is Auto for it",
                    state.kept.sink, offer.sink
                );
            }
            if let Err(error) = write_setting(&self.setting_path, &fresh) {
                eprintln!("mediaboxd-rs: output setting not written: {error}");
            }
            state.kept = fresh;
            state.trial = None;
        }
        state.offer = Some(offer);
        state.wire = Some(wire);
        let active = state
            .trial
            .as_ref()
            .map(|trial| trial.setting.clone())
            .unwrap_or_else(|| state.kept.clone());
        self.write_plan(&state, &active);
        drop(state);
        let _ = self.events.send(OutputEvent::Changed);
    }

    /// Put a mode, and a colour at it, on the wire on trial.
    ///
    /// Refused unless the interface is the one holding the display -- only it
    /// can set a mode -- and unless the display lists the mode and the colour
    /// can be sent there.
    pub fn try_setting(
        self: &Arc<Self>,
        resolution: ResolutionChoice,
        colour: Option<ColorMode>,
        interface_holds_display: bool,
    ) -> Result<OutputStatus, String> {
        if !interface_holds_display {
            return Err("Ekranı şu an başka bir uygulama kullanıyor; ayar arayüzden değiştirilir.".into());
        }
        let (setting, id) = {
            let mut state = self.state();
            let Some(offer) = state.offer.clone() else {
                return Err("Arayüz henüz bir ekran bildirmedi.".into());
            };
            if let ResolutionChoice::Fixed { .. } = resolution
                && !offer
                    .modes()
                    .any(|mode| mode.is(resolution) && mode.allowed().next().is_some())
            {
                return Err("Bu ekran bu modu listelemiyor.".into());
            }
            let Some(mode) = offer.resolve(resolution) else {
                return Err("Bu ekranda gönderilebilecek bir mod yok.".into());
            };
            if let Some(chosen) = colour
                && let Some(cell) = mode.cells.iter().find(|cell| cell.mode == chosen)
                && let Some(refusal) = cell.refused
            {
                return Err(refusal.text(chosen.format));
            }
            if let Some(chosen) = colour
                && !mode.cells.iter().any(|cell| cell.mode == chosen)
            {
                return Err(format!("{} bu kartta yok.", chosen.label()));
            }
            // A second change during a trial is measured against the setting
            // that was kept, not against the one still on trial.
            let previous = state.kept.for_sink(&offer.sink);
            let mut setting = state
                .trial
                .as_ref()
                .map(|trial| trial.setting.clone())
                .unwrap_or_else(|| previous.clone());
            setting.sink = offer.sink.clone();
            setting.resolution = resolution;
            match colour {
                Some(chosen) => {
                    setting.colours.insert(mode.label.clone(), chosen);
                }
                None => {
                    setting.colours.remove(&mode.label);
                }
            }
            state.next_trial += 1;
            let id = state.next_trial;
            state.trial = Some(Trial {
                setting: setting.clone(),
                previous,
                deadline: Instant::now() + Duration::from_secs(OUTPUT_TRIAL_SECONDS.into()),
                id,
            });
            (setting, id)
        };
        let _ = self.events.send(OutputEvent::Apply {
            setting,
            trial_seconds: Some(OUTPUT_TRIAL_SECONDS),
        });
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(OUTPUT_TRIAL_SECONDS.into())).await;
            this.take_back(Some(id), true);
        });
        Ok(self.status())
    }

    /// Keep the setting on trial.
    pub fn keep(&self) -> Result<OutputStatus, String> {
        {
            let mut state = self.state();
            let Some(trial) = state.trial.take() else {
                return Err("Onay bekleyen bir ayar yok.".into());
            };
            write_setting(&self.setting_path, &trial.setting)?;
            state.kept = trial.setting.clone();
            self.write_plan(&state, &trial.setting);
        }
        let _ = self.events.send(OutputEvent::Kept);
        Ok(self.status())
    }

    /// Take the setting on trial back now.
    pub fn revert(&self) -> Result<OutputStatus, String> {
        if !self.take_back(None, false) {
            return Err("Onay bekleyen bir ayar yok.".into());
        }
        Ok(self.status())
    }

    /// End the trial `id` (or whichever is running) and put the earlier
    /// setting back on the wire.
    fn take_back(&self, id: Option<u64>, timed_out: bool) -> bool {
        let previous = {
            let mut state = self.state();
            match &state.trial {
                Some(trial) if id.is_none_or(|id| trial.id == id) => {}
                _ => return false,
            }
            let trial = state.trial.take().expect("checked above");
            self.write_plan(&state, &trial.previous);
            trial.previous
        };
        if timed_out {
            eprintln!("mediaboxd-rs: output trial not kept in time; the earlier setting is back");
        }
        let _ = self.events.send(OutputEvent::Apply {
            setting: previous,
            trial_seconds: None,
        });
        let _ = self.events.send(OutputEvent::Reverted { timed_out });
        true
    }

    fn write_plan(&self, state: &State, setting: &OutputSetting) {
        let Some(offer) = &state.offer else { return };
        let Some(plan) = mediabox_platform::output::plan(offer, &setting.for_sink(&offer.sink))
        else {
            return;
        };
        let text = format!(
            "kodi_screenmode={}\nkodi_whitelist={}\nbrowser_mode={}\n",
            plan.kodi_screenmode,
            plan.kodi_whitelist.join(","),
            plan.browser_mode
        );
        if let Err(error) = write_atomic(&self.plan_path, &text) {
            eprintln!("mediaboxd-rs: output plan not written: {error}");
        }
    }
}

fn read_setting(path: &Path) -> Option<OutputSetting> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim()).ok()
}

fn write_setting(path: &Path, setting: &OutputSetting) -> Result<(), String> {
    let text = serde_json::to_string(setting).map_err(|error| error.to_string())?;
    write_atomic(path, &format!("{text}\n"))
}

/// Written beside and renamed over, so a reader never sees half a file.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let partial = path.with_extension("partial");
    std::fs::write(&partial, text).map_err(|error| error.to_string())?;
    std::fs::rename(&partial, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::{ColorFormat, ColourCell, OutputGroup, OutputModeOffer};

    fn mode(label: &str, width: u16, height: u16, refresh_mhz: u32, cells: &[(ColorMode, bool)]) -> OutputModeOffer {
        OutputModeOffer {
            label: label.into(),
            width,
            height,
            refresh_mhz,
            interlaced: false,
            pixel_clock_khz: 297_000,
            htotal: 4400,
            vtotal: 2250,
            preferred: false,
            vic: Some(95),
            cells: cells
                .iter()
                .map(|(mode, ok)| ColourCell {
                    mode: *mode,
                    rate_khz: 297_000,
                    refused: (!ok).then_some(mediabox_core::Refusal::Only420),
                })
                .collect(),
            auto_sdr: cells.iter().find(|(_, ok)| *ok).map(|(mode, _)| *mode),
            auto_hdr: None,
        }
    }

    fn offer(sink: &str) -> OutputOffer {
        let rgb = ColorMode::new(ColorFormat::Rgb, 8);
        let y422 = ColorMode::new(ColorFormat::Ycbcr422, 10);
        let y420 = ColorMode::new(ColorFormat::Ycbcr420, 8);
        OutputOffer {
            sink: sink.into(),
            connector: "HDMI-A-2".into(),
            auto: "3840x2160p60".into(),
            groups: vec![OutputGroup {
                width: 3840,
                height: 2160,
                name: "4K UHD".into(),
                computer: false,
                modes: vec![
                    mode("3840x2160p60", 3840, 2160, 60_000, &[(rgb, false), (y420, true)]),
                    mode("3840x2160p30", 3840, 2160, 30_000, &[(rgb, true), (y422, true), (y420, false)]),
                ],
            }],
            ..Default::default()
        }
    }

    fn thirty() -> ResolutionChoice {
        ResolutionChoice::Fixed {
            width: 3840,
            height: 2160,
            refresh_mhz: 30_000,
            interlaced: false,
        }
    }

    fn output(dir: &tempfile::TempDir) -> Arc<Output> {
        Output::new(
            dir.path().join("output.json"),
            dir.path().join("output-plan"),
            dir.path().join("summary"),
        )
    }

    #[tokio::test]
    async fn a_trial_is_not_written_down_until_it_is_kept() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = output(&dir);
        output.report(offer("d3d7"), OutputWire::default());
        let mut events = output.subscribe();

        let y422 = ColorMode::new(ColorFormat::Ycbcr422, 10);
        let status = output.try_setting(thirty(), Some(y422), true).expect("a mode it lists");
        assert!(status.trial.is_some());
        assert_eq!(status.setting.resolution, ResolutionChoice::Auto, "kept is still Auto");
        match events.recv().await.unwrap() {
            OutputEvent::Apply { setting, trial_seconds } => {
                assert_eq!(setting.resolution, thirty());
                assert_eq!(setting.colours.get("3840x2160p30"), Some(&y422));
                assert_eq!(trial_seconds, Some(OUTPUT_TRIAL_SECONDS));
            }
            other => panic!("{other:?}"),
        }
        // A daemon started now reads what was kept, which is not the trial.
        assert_eq!(read_setting(&dir.path().join("output.json")).unwrap().resolution, ResolutionChoice::Auto);

        output.keep().expect("a trial to keep");
        let kept = read_setting(&dir.path().join("output.json")).unwrap();
        assert_eq!(kept.resolution, thirty());
        assert_eq!(kept.sink, "d3d7");
        let plan = std::fs::read_to_string(dir.path().join("output-plan")).unwrap();
        assert!(plan.contains("kodi_screenmode=0384002160030.00000pstd"), "{plan}");
        assert!(plan.contains("browser_mode=3840x2160@30.000Hz"), "{plan}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_trial_nobody_keeps_is_taken_back() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = output(&dir);
        output.report(offer("d3d7"), OutputWire::default());
        let mut events = output.subscribe();
        output.try_setting(thirty(), None, true).unwrap();
        let _apply = events.recv().await.unwrap();

        tokio::time::sleep(Duration::from_secs(u64::from(OUTPUT_TRIAL_SECONDS) + 1)).await;
        match events.recv().await.unwrap() {
            OutputEvent::Apply { setting, trial_seconds: None } => {
                assert_eq!(setting.resolution, ResolutionChoice::Auto)
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(events.recv().await.unwrap(), OutputEvent::Reverted { timed_out: true });
        assert!(output.status().trial.is_none());
        assert!(output.keep().is_err(), "nothing left to keep");
    }

    #[tokio::test]
    async fn what_cannot_be_sent_is_refused_with_the_reason() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = output(&dir);
        output.report(offer("d3d7"), OutputWire::default());
        let sixty = ResolutionChoice::Fixed {
            width: 3840,
            height: 2160,
            refresh_mhz: 60_000,
            interlaced: false,
        };
        let error = output
            .try_setting(sixty, Some(ColorMode::new(ColorFormat::Rgb, 8)), true)
            .unwrap_err();
        assert!(error.contains("4:2:0"), "{error}");
        let error = output
            .try_setting(
                ResolutionChoice::Fixed { width: 1920, height: 1080, refresh_mhz: 60_000, interlaced: false },
                None,
                true,
            )
            .unwrap_err();
        assert!(error.contains("listelemiyor"), "{error}");
        assert!(output.try_setting(thirty(), None, false).is_err(), "only while the interface holds the display");
        assert!(output.status().trial.is_none());
    }

    #[tokio::test]
    async fn another_display_is_auto_and_the_record_moves_to_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = output(&dir);
        output.report(offer("d3d7"), OutputWire::default());
        output.try_setting(thirty(), None, true).unwrap();
        output.keep().unwrap();

        output.report(offer("7171"), OutputWire::default());
        let status = output.status();
        assert_eq!(status.setting.sink, "7171");
        assert_eq!(status.setting.resolution, ResolutionChoice::Auto);
        let kept = read_setting(&dir.path().join("output.json")).unwrap();
        assert_eq!(kept.sink, "7171");
        assert_eq!(kept.resolution, ResolutionChoice::Auto);
    }

    #[test]
    fn a_box_that_has_never_been_told_is_on_auto() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("output.json"), "not json\n").unwrap();
        let output = output(&dir);
        let status = output.status();
        assert_eq!(status.setting, OutputSetting::default());
        assert!(status.error.is_some(), "no display reported yet");
    }
}
