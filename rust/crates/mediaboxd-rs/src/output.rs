//! The display setting: what a person chose, the display it was chosen on, a
//! new choice on trial, and the plan the other owners start from.
//!
//! What the display *is* is not decided here and is not taken from any
//! client. The observer (`mediabox_platform::observer`) publishes the
//! hardware -- the generation, the sink's EDID identity, the topology, the
//! kernel's mode list and the offer computed from it, and what the display
//! controller reports on the wire -- in a directory only it writes, and this
//! reads that, level-triggered: on every request and once a second. What the
//! owner of the display committed arrives as an [`OwnerReport`] on the local
//! socket, and is taken only for the output and sink the observer sees now.
//! A controller on the LAN can ask for a setting and read all of it; the
//! request vocabulary has no way to say what the hardware is.
//!
//! What is kept here is the part that has to outlive every owner:
//!
//! * the kept setting, one record bound to the SHA-256 of the sink's EDID --
//!   the reference Android box's model (`hdmimode`, `<mode>_deepcolor`,
//!   `hdmichecksum`): a different display is `Auto`, and the record moves to
//!   it. A record written before (bound to block checksums, a mode by size
//!   and millihertz) is migrated once, when its sink is seen;
//! * the trial: a new setting is sent to the owner and not written down. It
//!   has an id, is bound to the output and sink it was made for, and is
//!   journalled on disk before it is sent, so a daemon that restarts takes it
//!   back rather than finding it kept. It can be kept only once the owner has
//!   reported it committed, on that same sink; anything that ends it --
//!   [`OUTPUT_TRIAL_SECONDS`], a revert, the owner restarting or handing the
//!   display over, the sink changing, the owner failing to apply it -- puts
//!   the earlier setting back;
//! * the plan (`mediabox_platform::output::Plan`) Kodi and the browser start
//!   from, written for the observer's current generation and removed the
//!   moment that is no longer the generation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mediabox_core::{
    AppliedOutput, ColorMode, DisplayIdentity, ModeTiming, OUTPUT_TRIAL_SECONDS, ObservedOutput,
    OutputEvent, OutputSetting, OutputStatus, OutputTrial, OwnerReport, ResolutionChoice,
    StoredSetting, TimingKey,
};
use mediabox_platform::observer::{self, KernelModes, Snapshot};
use mediabox_platform::output::{Plan, Provenance};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Where everything this keeps lives. The production paths are
/// [`Paths::system`]; a test puts them in a directory of its own.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The kept setting, in the daemon's own `StateDirectory`.
    pub setting: PathBuf,
    /// The trial in progress, beside it: what to take back if this daemon
    /// does not live to see the trial end.
    pub trial: PathBuf,
    /// The plan `mediabox-hdmi-prepare` gives Kodi and the browser.
    pub plan: PathBuf,
    /// The observer's runtime directory.
    pub observer: PathBuf,
}

impl Paths {
    pub fn system() -> Self {
        let roots = mediabox_platform::Roots::system();
        Self {
            setting: roots.state("output.json"),
            trial: roots.state("output-trial.json"),
            plan: roots.run(mediabox_platform::output::PLAN_FILE),
            observer: roots.run(observer::RUNTIME),
        }
    }
}

/// The trial as it is journalled: enough to know, after a restart, that it
/// was never kept and what was in force before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Journal {
    id: u64,
    identity: DisplayIdentity,
    setting: OutputSetting,
    previous: OutputSetting,
}

struct Trial {
    journal: Journal,
    deadline: Instant,
    /// The owner reported it committed, on this sink.
    applied: bool,
}

#[derive(Default)]
struct State {
    /// What `output.json` holds.
    stored: Option<StoredSetting>,
    /// The kept setting, for the sink in `snapshot`.
    kept: OutputSetting,
    notes: Vec<String>,
    /// The observer's last snapshot, and the identity it was about.
    snapshot: Option<Snapshot>,
    identity: Option<DisplayIdentity>,
    trial: Option<Trial>,
    /// What the owner last reported committing, for `identity`.
    applied: Option<AppliedOutput>,
    /// The plan file's contents as last written, empty for no plan.
    plan: String,
    error: Option<String>,
}

pub struct Output {
    paths: Paths,
    state: Mutex<State>,
    events: broadcast::Sender<OutputEvent>,
    /// Asks for the display's owner to be put back on a sink that changed:
    /// `mediabox-display-changed`, which compares against its own record and
    /// does nothing when there is nothing to do.
    recover: Box<dyn Fn() + Send + Sync>,
}

impl Output {
    /// Reads the kept setting, and takes back a trial a previous run left:
    /// the journal says one was sent and never kept, so what is kept on disk
    /// is what is in force. The owner is told on its next look at the status.
    pub fn new(paths: Paths, recover: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        let stored = std::fs::read_to_string(&paths.setting)
            .ok()
            .and_then(|text| StoredSetting::parse(&text));
        let mut notes = Vec::new();
        if let Some(journal) = std::fs::read_to_string(&paths.trial)
            .ok()
            .and_then(|text| serde_json::from_str::<Journal>(&text).ok())
        {
            let kept = matches!(&stored, Some(StoredSetting::Current(setting)) if *setting == journal.setting);
            eprintln!(
                "mediaboxd-rs: output trial {} found at start; {}",
                journal.id,
                if kept {
                    "it had been written down, so it was kept"
                } else {
                    "it was never kept and the kept setting is in force"
                }
            );
            if !kept {
                notes.push("Onaylanmamış ekran denemesi yeniden başlatmada geri alındı.".into());
            }
        }
        let _ = std::fs::remove_file(&paths.trial);
        let (events, _) = broadcast::channel(16);
        Arc::new(Self {
            paths,
            state: Mutex::new(State {
                stored,
                notes,
                ..Default::default()
            }),
            events,
            recover: Box::new(recover),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OutputEvent> {
        self.events.subscribe()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, events: Vec<OutputEvent>) {
        for event in events {
            let _ = self.events.send(event);
        }
    }

    /// Look at the observer's snapshot again, and follow it.
    pub fn reconcile(&self) {
        let events = {
            let mut state = self.state();
            self.follow(&mut state)
        };
        self.send(events);
    }

    /// Everything that follows from a new snapshot, under the lock; the
    /// events to send once it is let go.
    fn follow(&self, state: &mut State) -> Vec<OutputEvent> {
        let mut events = Vec::new();
        let Some(snapshot) = observer::published(&self.paths.observer) else {
            if state.snapshot.take().is_some() {
                eprintln!("mediaboxd-rs: the display observer's snapshot is gone");
            }
            state.identity = None;
            self.write_plan(state);
            return events;
        };
        if state.snapshot.as_ref().is_some_and(|old| {
            old.generation == snapshot.generation && old.offer.is_some() == snapshot.offer.is_some()
        }) {
            state.snapshot = Some(snapshot);
            return events;
        }
        let identity = snapshot.identity();
        let first = state.snapshot.is_none();
        let sink_changed = !first && identity != state.identity;
        if sink_changed {
            eprintln!(
                "mediaboxd-rs: display generation {}:{}: the sink changed ({} -> {})",
                snapshot.generation.boot_id,
                snapshot.generation.seq,
                describe(state.identity.as_ref()),
                describe(identity.as_ref())
            );
            state.applied = None;
            if let Some(trial) = state.trial.take() {
                events.extend(self.taken_back(trial, false, "ekran değişti"));
            }
            (self.recover)();
        }
        state.identity = identity;
        state.snapshot = Some(snapshot);
        self.resolve_kept(state);
        self.write_plan(state);
        events.push(OutputEvent::Changed);
        events
    }

    /// The kept setting for the sink seen now, migrating a legacy record the
    /// first time its sink is seen. A record that moves -- to a new sink, or
    /// out of the legacy form -- is written down.
    fn resolve_kept(&self, state: &mut State) {
        let Some(offer) = state.snapshot.as_ref().and_then(|snapshot| snapshot.offer.clone()) else {
            return;
        };
        let kernel = kernel_timings(state.snapshot.as_ref());
        let (kept, notes) = match &state.stored {
            Some(stored) => stored.for_offer(&offer, &kernel),
            None => (OutputSetting::default().for_sink(&offer.edid_sha256), Vec::new()),
        };
        let moved = match &state.stored {
            Some(StoredSetting::Current(current)) => current.edid_sha256 != kept.edid_sha256,
            Some(StoredSetting::Legacy(_)) => true,
            None => false,
        };
        for note in &notes {
            eprintln!("mediaboxd-rs: output setting: {note}");
        }
        state.notes.extend(notes);
        if moved {
            match write_atomic(&self.paths.setting, &kept.to_stored()) {
                Ok(()) => state.stored = Some(StoredSetting::Current(kept.clone())),
                Err(error) => {
                    eprintln!("mediaboxd-rs: output setting not written: {error}");
                    state.error = Some(format!("Ekran ayarı kaydedilemedi: {error}"));
                }
            }
        }
        state.kept = kept;
    }

    /// The plan for the generation seen now, from the kept setting; no plan
    /// at all when there is no generation, sink or offer to make it for.
    fn write_plan(&self, state: &mut State) {
        let plan = state.snapshot.as_ref().and_then(|snapshot| {
            let offer = snapshot.offer.as_ref()?;
            mediabox_platform::output::plan(
                offer,
                &state.kept,
                Provenance {
                    generation: snapshot.generation.clone(),
                    identity: state.identity.clone()?,
                    transmitter: snapshot.material.transmitter.clone().unwrap_or_else(|| "-".into()),
                    source_profile: snapshot.material.source_profile.clone(),
                },
            )
        });
        let text = plan.as_ref().map(Plan::render).unwrap_or_default();
        if text == state.plan {
            return;
        }
        let written = match &plan {
            Some(_) => write_atomic(&self.paths.plan, &text),
            None => match std::fs::remove_file(&self.paths.plan) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
                _ => Ok(()),
            },
        };
        match written {
            Ok(()) => state.plan = text,
            Err(error) => eprintln!("mediaboxd-rs: output plan not written: {error}"),
        }
    }

    pub fn status(&self) -> OutputStatus {
        let events;
        let status = {
            let mut state = self.state();
            events = self.follow(&mut state);
            self.status_of(&state)
        };
        self.send(events);
        status
    }

    fn status_of(&self, state: &State) -> OutputStatus {
        let snapshot = state.snapshot.as_ref();
        let offer = snapshot.and_then(|snapshot| snapshot.offer.clone());
        let on_wire = state
            .trial
            .as_ref()
            .map(|trial| &trial.journal.setting)
            .unwrap_or(&state.kept);
        let error = match snapshot {
            None => Some("Ekran gözlemcisi henüz bir durum yayımlamadı".to_string()),
            Some(snapshot) if snapshot.identity().is_none() => {
                Some("Bağlı, EDID'i okunabilen bir ekran yok".to_string())
            }
            Some(_) if offer.is_none() => Some("Ekranın mod listesi henüz okunmadı".to_string()),
            Some(_) => None,
        };
        OutputStatus {
            display: snapshot.map(Snapshot::display_state),
            selected: offer.as_ref().and_then(|offer| offer.select(on_wire)),
            offer,
            setting: state.kept.clone(),
            trial: state.trial.as_ref().map(|trial| OutputTrial {
                id: trial.journal.id,
                setting: trial.journal.setting.clone(),
                previous: trial.journal.previous.clone(),
                seconds_left: trial
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f32()
                    .ceil() as u32,
                applied: trial.applied,
            }),
            applied: state
                .applied
                .clone()
                .filter(|applied| Some(&applied.identity) == state.identity.as_ref()),
            observed: snapshot.map(|snapshot| ObservedOutput {
                colour: snapshot
                    .bus_format
                    .as_known()
                    .and_then(|name| mediabox_platform::output::bus_colour(name)),
                mode: snapshot.current_mode.clone(),
                bus_format: snapshot.bus_format.clone(),
                phy_clock_khz: snapshot.phy_clock_khz.clone(),
            }),
            notes: state.notes.clone(),
            error: state.error.clone().or(error),
        }
    }

    /// Put a mode, and a colour at it, on the wire on trial.
    ///
    /// Refused unless the interface is the one holding the display -- only it
    /// sets a mode on request -- unless the observer sees a sink with an
    /// offer, and unless that offer lists the mode and the colour can be sent
    /// there. The trial is journalled before it is sent; if it cannot be,
    /// nothing is sent.
    pub fn try_setting(
        self: &Arc<Self>,
        resolution: ResolutionChoice,
        colour: Option<ColorMode>,
        interface_holds_display: bool,
    ) -> Result<OutputStatus, String> {
        if !interface_holds_display {
            return Err("Ekranı şu an başka bir uygulama kullanıyor; ayar arayüzden değiştirilir.".into());
        }
        let (id, events) = {
            let mut state = self.state();
            let mut events = self.follow(&mut state);
            let (Some(offer), Some(identity)) = (
                state.snapshot.as_ref().and_then(|snapshot| snapshot.offer.clone()),
                state.identity.clone(),
            ) else {
                return Err("Ekranın mod listesi henüz okunmadı.".into());
            };
            if let ResolutionChoice::Timing { key } = resolution
                && !offer
                    .mode_by_key(key)
                    .is_some_and(|mode| mode.allowed().next().is_some())
            {
                return Err("Bu ekran bu modu listelemiyor.".into());
            }
            let Some(mode) = offer.resolve(resolution) else {
                return Err("Bu ekranda gönderilebilecek bir mod yok.".into());
            };
            if let Some(chosen) = colour {
                match mode.cells.iter().find(|cell| cell.mode == chosen) {
                    None => return Err(format!("{} bu kartta yok.", chosen.label())),
                    Some(cell) => {
                        if let Some(refusal) = cell.refused {
                            return Err(refusal.text(chosen.format));
                        }
                    }
                }
            }
            // A second change during a trial is measured against the setting
            // that was kept, not against the one still on trial.
            let previous = state.kept.clone();
            let mut setting = state
                .trial
                .as_ref()
                .map(|trial| trial.journal.setting.clone())
                .unwrap_or_else(|| previous.clone());
            setting.edid_sha256 = offer.edid_sha256.clone();
            setting.resolution = resolution;
            match colour {
                Some(chosen) => {
                    setting.colours.insert(mode.timing_key, chosen);
                }
                None => {
                    setting.colours.remove(&mode.timing_key);
                }
            }
            let journal = Journal {
                id: fresh_id(),
                identity: identity.clone(),
                setting: setting.clone(),
                previous,
            };
            write_atomic(
                &self.paths.trial,
                &serde_json::to_string(&journal).map_err(|error| error.to_string())?,
            )
            .map_err(|error| format!("Deneme kaydedilemedi, ayar gönderilmedi: {error}"))?;
            if let Some(replaced) = state.trial.take() {
                eprintln!(
                    "mediaboxd-rs: output trial {} replaced by {}",
                    replaced.journal.id, journal.id
                );
            }
            let id = journal.id;
            state.trial = Some(Trial {
                journal,
                deadline: Instant::now() + Duration::from_secs(OUTPUT_TRIAL_SECONDS.into()),
                applied: false,
            });
            events.push(OutputEvent::Apply {
                identity,
                setting,
                trial: Some(id),
                trial_seconds: Some(OUTPUT_TRIAL_SECONDS),
            });
            (id, events)
        };
        self.send(events);
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(OUTPUT_TRIAL_SECONDS.into())).await;
            this.end_trial(id, true, "onay gelmedi");
        });
        Ok(self.status())
    }

    /// Keep trial `id`: write it down, and use it from now on.
    ///
    /// Only the trial running now, only once the owner has reported it
    /// committed, and only while the sink is the one it was made for. A write
    /// that fails is reported and keeps nothing: the trial runs on and is
    /// taken back when its time is up.
    pub fn keep(&self, id: u64) -> Result<OutputStatus, String> {
        let events = {
            let mut state = self.state();
            let mut events = self.follow(&mut state);
            let Some(trial) = state.trial.as_ref() else {
                return Err("Onay bekleyen bir ayar yok.".into());
            };
            if trial.journal.id != id {
                return Err("Bu deneme artık geçerli değil; onaylanmadı.".into());
            }
            if Some(&trial.journal.identity) != state.identity.as_ref() {
                return Err("Ekran değişti; deneme onaylanamaz.".into());
            }
            if !trial.applied {
                return Err("Ayar ekrana henüz uygulanmadı; onaylanamaz.".into());
            }
            let setting = trial.journal.setting.clone();
            write_atomic(&self.paths.setting, &setting.to_stored())
                .map_err(|error| format!("Ayar kaydedilemedi: {error}"))?;
            // Written down: from here the journal only says what the file
            // already does, and a restart that finds it treats it as kept.
            let _ = std::fs::remove_file(&self.paths.trial);
            state.trial = None;
            state.stored = Some(StoredSetting::Current(setting.clone()));
            state.kept = setting;
            state.error = None;
            self.write_plan(&mut state);
            events.push(OutputEvent::Kept { trial: id });
            events
        };
        self.send(events);
        Ok(self.status())
    }

    /// Take trial `id` back now.
    pub fn revert(&self, id: u64) -> Result<OutputStatus, String> {
        if !self.end_trial(id, false, "geri alındı") {
            return Err("Bu deneme artık geçerli değil.".into());
        }
        Ok(self.status())
    }

    /// The display is changing hands. A trial belongs to the owner it was
    /// sent to; the next one starts from the kept setting.
    pub fn owner_changing(&self) {
        let events = {
            let mut state = self.state();
            state.applied = None;
            match state.trial.take() {
                Some(trial) => self.taken_back(trial, false, "ekran başka bir uygulamaya geçti"),
                None => Vec::new(),
            }
        };
        self.send(events);
    }

    /// What the owner committed, or could not.
    ///
    /// Taken only for the output and sink the observer sees now, and -- when
    /// it names a mode -- only for a mode of the offer. A commit that is not
    /// the trial running means the owner is not running the trial (it
    /// restarted, and put the kept setting back): the trial is over.
    pub fn owner_report(&self, report: OwnerReport) -> Result<OutputStatus, String> {
        let events = {
            let mut state = self.state();
            let mut events = self.follow(&mut state);
            let identity = match &report {
                OwnerReport::Applied(applied) => &applied.identity,
                OwnerReport::Failed { identity, .. } => identity,
            };
            if Some(identity) != state.identity.as_ref() {
                return Err(format!(
                    "rapor {} için; gözlemci {} görüyor",
                    describe(Some(identity)),
                    describe(state.identity.as_ref())
                ));
            }
            let offer = state.snapshot.as_ref().and_then(|snapshot| snapshot.offer.clone());
            match report {
                OwnerReport::Applied(applied) => {
                    if let Some(offer) = &offer
                        && offer.mode_by_key(applied.timing_key).is_none()
                    {
                        return Err(format!("{} bu ekranın mod listesinde yok", applied.label));
                    }
                    let running = state.trial.as_ref().map(|trial| trial.journal.id);
                    match (running, applied.trial) {
                        (Some(running), Some(reported)) if running == reported => {
                            let target = offer.as_ref().and_then(|offer| {
                                offer.select(&state.trial.as_ref()?.journal.setting)
                            });
                            let matches = target.is_some_and(|target| {
                                target.timing_key == applied.timing_key
                                    && (applied.colour.is_none()
                                        || applied.colour == target.sdr
                                        || applied.colour == target.hdr)
                            });
                            if matches {
                                if let Some(trial) = state.trial.as_mut() {
                                    trial.applied = true;
                                }
                            } else if let Some(trial) = state.trial.take() {
                                events.extend(self.taken_back(
                                    trial,
                                    false,
                                    "ekrana istenenden başka bir ayar uygulandı",
                                ));
                            }
                        }
                        // About an earlier trial, overtaken by this one on the
                        // way: it says nothing about the one running.
                        (Some(_), Some(_)) => {}
                        (Some(_), None) => {
                            if let Some(trial) = state.trial.take() {
                                events.extend(self.taken_back(
                                    trial,
                                    false,
                                    "ekranın sahibi denemeyi sürdürmedi",
                                ));
                            }
                        }
                        (None, _) => {}
                    }
                    state.applied = Some(applied);
                }
                OwnerReport::Failed { trial, error, .. } => {
                    eprintln!("mediaboxd-rs: the display's owner could not apply a setting: {error}");
                    state.error = Some(format!("Çekirdek bu ayarı kabul etmedi: {error}"));
                    if let (Some(running), Some(failed)) =
                        (state.trial.as_ref().map(|trial| trial.journal.id), trial)
                        && running == failed
                        && let Some(trial) = state.trial.take()
                    {
                        events.extend(self.taken_back(trial, false, "uygulanamadı"));
                    }
                }
            }
            events.push(OutputEvent::Changed);
            events
        };
        self.send(events);
        Ok(self.status())
    }

    /// End trial `id` if it is still the one running.
    fn end_trial(&self, id: u64, timed_out: bool, reason: &str) -> bool {
        let events = {
            let mut state = self.state();
            match state.trial.as_ref() {
                Some(trial) if trial.journal.id == id => {}
                _ => return false,
            }
            let trial = state.trial.take().expect("checked above");
            self.taken_back(trial, timed_out, reason)
        };
        self.send(events);
        true
    }

    /// A trial that ends without being kept: the journal goes, and the owner
    /// is told to put the earlier setting back -- on the sink the trial was
    /// for, and no other.
    fn taken_back(&self, trial: Trial, timed_out: bool, reason: &str) -> Vec<OutputEvent> {
        let _ = std::fs::remove_file(&self.paths.trial);
        eprintln!(
            "mediaboxd-rs: output trial {} taken back: {reason}",
            trial.journal.id
        );
        vec![
            OutputEvent::Apply {
                identity: trial.journal.identity,
                setting: trial.journal.previous,
                trial: None,
                trial_seconds: None,
            },
            OutputEvent::Reverted {
                trial: trial.journal.id,
                timed_out,
                reason: Some(reason.to_string()),
            },
        ]
    }
}

/// The kernel's mode list for the sink in `snapshot`, as the timings a legacy
/// record is migrated against.
fn kernel_timings(snapshot: Option<&Snapshot>) -> Vec<ModeTiming> {
    match snapshot.map(|snapshot| &snapshot.kernel_modes) {
        Some(KernelModes::Known { keys, .. }) => keys
            .iter()
            .filter_map(|key| TimingKey::parse(key))
            .map(|key| key.timing())
            .collect(),
        _ => Vec::new(),
    }
}

fn describe(identity: Option<&DisplayIdentity>) -> String {
    match identity {
        Some(identity) => format!(
            "{} {}",
            identity.connector,
            &identity.edid_sha256[..identity.edid_sha256.len().min(12)]
        ),
        None => "ekran yok".into(),
    }
}

/// A trial id no earlier run of this daemon can have used. Fifty-three bits,
/// so a web page reading it as a JavaScript number reads it exactly.
fn fresh_id() -> u64 {
    let mut bytes = [0u8; 8];
    // SAFETY: eight bytes into a live buffer of eight.
    let got = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    if got != bytes.len() as isize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        return (now.as_nanos() as u64 ^ u64::from(std::process::id())) >> 11;
    }
    (u64::from_ne_bytes(bytes) >> 11).max(1)
}

/// Written beside, flushed, and renamed over, so a reader never sees half a
/// file and a power cut leaves the old one or the new one.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let partial = path.with_extension("partial");
    let mut file = std::fs::File::create(&partial).map_err(|error| error.to_string())?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())?;
    std::fs::rename(&partial, path).map_err(|error| error.to_string())?;
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::{
        ColorFormat, DisplayGeneration, Observed, OutputOffer, Route, StoredSetting,
    };
    use mediabox_platform::observer::{Material, Reason};
    use mediabox_platform::video::Timing;

    /// Sony KD-65XE9005, the 600 MHz input: the EDID HDMI-A-2 on the Plus
    /// published on 2026-09-24.
    const SONY: &str = "\
00ffffffffffff004dd903f901010101011b0103809051780a0dc9a05747982712484c2108008180a9c0714f\
b300010101010101010108e80030f2705a80b0588a009f295300001e023a801871382d40582c45009f295300\
001e000000fc00534f4e5920545620202a30300a000000fd00173e0e883c000a2020202020200171020359f0\
5b61605d5e5f621f101405130420223c3e12160307111502060165662c0d7f071507503d07bc570400830f00\
006e030c003000b83c2f00800102030467d85dc401788001e200cbe305ff01e50f03000006e3060d01011d00\
7251d01e206e2855009f295300001e00000000000000000000000000000000000000006b";
    /// The same set's 300 MHz input: another sink.
    const SONY_300: &str = "\
00ffffffffffff004dd903f301010101011a0103809051780a0dc9a05747982712484c2108008180a9c0714f\
b3000101010101010101023a801871382d40582c45009f295300001e011d007251d01e206e2855009f295300\
001e000000fc00534f4e5920545620202a30300a000000fd00303e0e461e000a20202020202001d302034df0\
575d5e5f621f101405130420223c3e1216030711150206012c0d7f071507503d07bc570600830f00006e030c\
004000b83c2f008001020304e200f9e305ff01e50e60616566e3060d01011d8018711c1620582c25009f2953\
00009e0000000000000000000000000000000000000000000000000000000000000000d7";

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect()
    }

    /// The Sony with its serial moved +1 in one byte and -1 in the next: every
    /// block checksum is the same, the EDID is not.
    fn sony_twin() -> Vec<u8> {
        let mut twin = bytes(SONY);
        twin[12] = twin[12].wrapping_add(1);
        twin[13] = twin[13].wrapping_sub(1);
        twin
    }

    fn kernel() -> Vec<Timing> {
        vec![
            Timing::new(3840, 2160, 594_000, 4400, 2250, false, true),
            Timing::new(3840, 2160, 297_000, 4400, 2250, false, false),
            Timing::new(3840, 2160, 297_000, 5500, 2250, false, false),
            Timing::new(3840, 2160, 296_703, 5500, 2250, false, false),
            Timing::new(1920, 1080, 148_500, 2200, 1125, false, false),
            Timing::new(1920, 1080, 74_250, 2200, 1125, true, false),
        ]
    }

    struct Rig {
        dir: tempfile::TempDir,
        recovered: Arc<std::sync::atomic::AtomicU32>,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                dir: tempfile::TempDir::new().unwrap(),
                recovered: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            }
        }

        fn paths(&self) -> Paths {
            Paths {
                setting: self.dir.path().join("output.json"),
                trial: self.dir.path().join("output-trial.json"),
                plan: self.dir.path().join("output-plan"),
                observer: self.dir.path().join("observer"),
            }
        }

        fn output(&self) -> Arc<Output> {
            let recovered = Arc::clone(&self.recovered);
            Output::new(self.paths(), move || {
                recovered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        }

        /// The observer publishes generation `seq`: `edid` on HDMI-A-2, or
        /// nothing connected.
        fn observe(&self, seq: u64, edid: Option<&[u8]>) -> Option<OutputOffer> {
            let offer = edid.and_then(|edid| {
                mediabox_platform::output::offer(
                    &kernel(),
                    edid,
                    "HDMI-A-2",
                    None,
                    &mediabox_platform::video::RK3588_HDMI,
                )
            });
            let report = edid.map(mediabox_platform::edid::EdidReport::of);
            let keys: Vec<String> = kernel().iter().map(|timing| timing.key().to_string()).collect();
            let snapshot = Snapshot {
                schema: observer::SCHEMA,
                generation: DisplayGeneration { boot_id: "b0071d".into(), seq },
                material_digest: format!("{seq}"),
                reason: Reason::Periodic,
                boottime_ns: observer::boottime_ns(),
                realtime_ms: 0,
                material: Material {
                    connector: edid.map(|_| "HDMI-A-2".into()),
                    connected: edid.map(|_| true),
                    edid_sha256: report.as_ref().and_then(|r| r.sha256.clone()).map(|id| id.0),
                    transmitter: Some("fdea0000.hdmi".into()),
                    source_profile: "rk3588-vendor61-dw-hdmi-qp@1".into(),
                    ..Default::default()
                },
                primary_minor: Some(0),
                physical_address: None,
                edid_status: report.as_ref().map(|r| r.status),
                edid_legacy_checkvalue: report.as_ref().map(|r| r.legacy_checkvalue.clone()),
                source_profile: "rk3588-vendor61-dw-hdmi-qp@1 (matched)".into(),
                topology_evidence: Vec::new(),
                audio: Route::default(),
                cec: Route::default(),
                current_mode: Observed::Known("3840x2160p60".into()),
                bus_format: Observed::Known("RGB888_1X24".into()),
                phy_clock_khz: Observed::Unknown("not measured here".into()),
                debugfs: mediabox_platform::debugfs::Debugfs::Unknown { reason: "test".into() },
                kernel_modes: KernelModes::Known {
                    count: keys.len(),
                    fingerprint: "f".into(),
                    preferred: None,
                    keys,
                },
                offer: offer.clone(),
                drm_master: Observed::Unknown("test".into()),
                warnings: Vec::new(),
            };
            std::fs::create_dir_all(self.paths().observer).unwrap();
            std::fs::write(
                self.paths().observer.join("snapshot.json"),
                serde_json::to_string(&snapshot).unwrap(),
            )
            .unwrap();
            offer
        }

        fn stored(&self) -> Option<StoredSetting> {
            StoredSetting::parse(&std::fs::read_to_string(self.paths().setting).ok()?)
        }

        fn plan(&self) -> Option<String> {
            std::fs::read_to_string(self.paths().plan).ok()
        }
    }

    fn identity(offer: &OutputOffer) -> DisplayIdentity {
        DisplayIdentity {
            connector: offer.connector.clone(),
            edid_sha256: offer.edid_sha256.clone(),
        }
    }

    fn thirty(offer: &OutputOffer) -> ResolutionChoice {
        offer.mode("3840x2160p30").unwrap().choice()
    }

    /// What the owner reports once it has put `setting` on the wire.
    fn applied(offer: &OutputOffer, setting: &OutputSetting, trial: Option<u64>) -> OwnerReport {
        let selected = offer.select(setting).unwrap();
        OwnerReport::Applied(AppliedOutput {
            identity: identity(offer),
            trial,
            timing_key: selected.timing_key,
            label: selected.label,
            colour: selected.sdr,
            hdr: false,
        })
    }

    /// Try `thirty` and have the owner commit it.
    fn applied_trial(output: &Arc<Output>, offer: &OutputOffer) -> u64 {
        let status = output.try_setting(thirty(offer), None, true).unwrap();
        let trial = status.trial.unwrap();
        output
            .owner_report(applied(offer, &trial.setting, Some(trial.id)))
            .unwrap();
        trial.id
    }

    #[tokio::test]
    async fn a_trial_is_journalled_not_kept_and_is_kept_only_once_applied() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let mut events = output.subscribe();
        let status = output.try_setting(thirty(&offer), None, true).expect("a mode it lists");
        let trial = status.trial.clone().unwrap();
        assert!(!trial.applied);
        assert_eq!(status.setting.resolution, ResolutionChoice::Auto, "kept is still Auto");
        loop {
            if let OutputEvent::Apply { identity: to, setting, trial: id, trial_seconds } =
                events.recv().await.unwrap()
            {
                assert_eq!(to, identity(&offer));
                assert_eq!(setting.resolution, thirty(&offer));
                assert_eq!(id, Some(trial.id));
                assert_eq!(trial_seconds, Some(OUTPUT_TRIAL_SECONDS));
                break;
            }
        }
        // Journalled before it was sent; not written down.
        assert!(rig.paths().trial.exists());
        assert_ne!(rig.stored(), Some(StoredSetting::Current(trial.setting.clone())));
        // Not applied yet: not keepable.
        assert!(output.keep(trial.id).unwrap_err().contains("uygulanmadı"));
        output.owner_report(applied(&offer, &trial.setting, Some(trial.id))).unwrap();
        assert!(output.status().trial.unwrap().applied);
        output.keep(trial.id).expect("applied, the same sink, the running trial");
        assert_eq!(rig.stored(), Some(StoredSetting::Current(trial.setting.clone())));
        assert!(!rig.paths().trial.exists(), "the journal goes once it is written down");
        assert!(rig.plan().unwrap().contains("kodi_screenmode=0384002160030.00000pstd"));
    }

    #[tokio::test]
    async fn a_late_report_about_an_earlier_trial_does_not_end_the_next() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let first = output.try_setting(thirty(&offer), None, true).unwrap().trial.unwrap();
        let second = output.try_setting(ResolutionChoice::Auto, None, true).unwrap().trial.unwrap();
        // The owner's report of the first arrives after the second started.
        output
            .owner_report(applied(&offer, &first.setting, Some(first.id)))
            .unwrap();
        let running = output.status().trial.unwrap();
        assert_eq!(running.id, second.id);
        assert!(!running.applied, "and the second is not applied by it");
        output
            .owner_report(applied(&offer, &second.setting, Some(second.id)))
            .unwrap();
        output.keep(second.id).unwrap();
    }

    #[tokio::test]
    async fn a_stale_keep_keeps_nothing() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let first = applied_trial(&output, &offer);
        // A second change replaces the first trial; a Keep sent for the first
        // -- from a page that had not caught up -- is not a Keep of this one.
        let second = output.try_setting(ResolutionChoice::Auto, None, true).unwrap().trial.unwrap().id;
        assert_ne!(first, second);
        assert!(output.keep(first).unwrap_err().contains("geçerli değil"));
        assert!(output.revert(first).is_err(), "nor a revert");
        assert_eq!(output.status().trial.unwrap().id, second);
        assert_eq!(rig.stored(), None, "nothing was written");
    }

    #[tokio::test(start_paused = true)]
    async fn a_trial_nobody_keeps_is_taken_back() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let id = applied_trial(&output, &offer);
        let mut events = output.subscribe();
        tokio::time::sleep(Duration::from_secs(u64::from(OUTPUT_TRIAL_SECONDS) + 1)).await;
        match events.recv().await.unwrap() {
            OutputEvent::Apply { setting, trial: None, .. } => {
                assert_eq!(setting.resolution, ResolutionChoice::Auto)
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            events.recv().await.unwrap(),
            OutputEvent::Reverted { trial, timed_out: true, .. } if trial == id
        ));
        assert!(output.status().trial.is_none());
        assert!(output.keep(id).is_err(), "nothing left to keep");
        assert!(!rig.paths().trial.exists());
    }

    #[tokio::test]
    async fn a_daemon_that_restarts_mid_trial_takes_it_back() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let id = {
            let output = rig.output();
            applied_trial(&output, &offer)
        };
        assert!(rig.paths().trial.exists(), "the journal outlives the daemon");
        // The next daemon finds the journal: nothing was kept, and the kept
        // setting is what is in force.
        let output = rig.output();
        assert!(!rig.paths().trial.exists());
        let status = output.status();
        assert!(status.trial.is_none());
        assert_eq!(status.setting.resolution, ResolutionChoice::Auto);
        assert!(!status.notes.is_empty(), "and it says so");
        assert!(output.keep(id).is_err(), "a Keep for the old trial keeps nothing");
        // The owner, still on the trial, reports it: that trial is not the
        // daemon's any more; the status tells the owner to go back.
        let wanted = status.setting.clone();
        assert_eq!(wanted.resolution, ResolutionChoice::Auto);
    }

    #[tokio::test]
    async fn a_journal_the_kept_file_already_says_is_kept() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        applied_trial(&output, &offer);
        let setting = output.status().trial.unwrap().setting;
        // The file was written, and the daemon died before the journal went.
        std::fs::write(rig.paths().setting, setting.to_stored()).unwrap();
        drop(output);
        let output = rig.output();
        let status = output.status();
        assert_eq!(status.setting, setting);
        assert!(status.notes.is_empty(), "nothing was taken back");
    }

    #[tokio::test]
    async fn an_owner_that_restarts_mid_trial_ends_it() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let id = applied_trial(&output, &offer);
        let mut events = output.subscribe();
        // The interface crashed and came back: it put the kept setting on the
        // wire and says so, with no trial.
        output
            .owner_report(applied(&offer, &OutputSetting::default(), None))
            .unwrap();
        assert!(output.status().trial.is_none());
        assert!(output.keep(id).is_err());
        let mut reverted = false;
        while let Ok(event) = events.try_recv() {
            reverted |= matches!(event, OutputEvent::Reverted { trial, .. } if trial == id);
        }
        assert!(reverted);
        assert_eq!(rig.stored(), None);
    }

    #[tokio::test]
    async fn handing_the_display_over_mid_trial_ends_it() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let id = applied_trial(&output, &offer);
        output.owner_changing();
        assert!(output.status().trial.is_none());
        assert!(output.keep(id).is_err());
        assert!(!rig.paths().trial.exists());
        assert!(rig.plan().unwrap().contains("kodi_screenmode=0384002160060.00000pstd"), "the next owner starts from the kept setting");
        assert!(
            output.try_setting(thirty(&offer), None, false).is_err(),
            "and only the interface starts a trial"
        );
    }

    #[tokio::test]
    async fn a_sink_replaced_mid_trial_ends_it_and_the_old_plan_goes() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let id = applied_trial(&output, &offer);
        let old_plan = rig.plan().unwrap();
        assert!(old_plan.contains(&format!("edid_sha256={}", offer.edid_sha256)));

        // Connected to connected, another EDID, no unplug in between.
        let other = rig.observe(2, Some(&bytes(SONY_300))).unwrap();
        let status = output.status();
        assert!(status.trial.is_none(), "a trial for another sink is over");
        assert!(output.keep(id).unwrap_err().len() > 0);
        assert_eq!(status.setting.edid_sha256, other.edid_sha256);
        assert_eq!(status.setting.resolution, ResolutionChoice::Auto);
        assert!(status.applied.is_none(), "what was applied was applied to the other sink");
        let plan = rig.plan().unwrap();
        assert!(plan.contains(&format!("edid_sha256={}", other.edid_sha256)));
        assert!(plan.contains("generation=2"));
        assert!(!plan.contains(&offer.edid_sha256));
        assert_eq!(rig.recovered.load(std::sync::atomic::Ordering::SeqCst), 1, "recovery asked for");
    }

    #[tokio::test]
    async fn a_failed_apply_ends_the_trial() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let trial = output.try_setting(thirty(&offer), None, true).unwrap().trial.unwrap();
        output
            .owner_report(OwnerReport::Failed {
                identity: identity(&offer),
                trial: Some(trial.id),
                error: "TEST_ONLY: Invalid argument".into(),
            })
            .unwrap();
        let status = output.status();
        assert!(status.trial.is_none());
        assert!(status.error.unwrap().contains("Invalid argument"));
        assert!(output.keep(trial.id).is_err());
    }

    #[tokio::test]
    async fn a_keep_that_cannot_be_written_keeps_nothing() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let mut paths = rig.paths();
        // A file where the state directory should be: nothing can be written
        // under it.
        std::fs::write(rig.dir.path().join("blocked"), "").unwrap();
        paths.setting = rig.dir.path().join("blocked/output.json");
        let output = Output::new(paths, || {});
        let id = applied_trial(&output, &offer);
        let error = output.keep(id).unwrap_err();
        assert!(error.contains("kaydedilemedi"), "{error}");
        let status = output.status();
        assert_eq!(status.trial.map(|trial| trial.id), Some(id), "the trial runs on");
        assert_eq!(status.setting.resolution, ResolutionChoice::Auto, "nothing kept");
    }

    #[tokio::test]
    async fn a_trial_that_cannot_be_journalled_is_not_sent() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let mut paths = rig.paths();
        std::fs::write(rig.dir.path().join("blocked"), "").unwrap();
        paths.trial = rig.dir.path().join("blocked/output-trial.json");
        let output = Output::new(paths, || {});
        let mut events = output.subscribe();
        let error = output.try_setting(thirty(&offer), None, true).unwrap_err();
        assert!(error.contains("gönderilmedi"), "{error}");
        assert!(output.status().trial.is_none());
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, OutputEvent::Apply { .. }), "nothing sent: {event:?}");
        }
    }

    #[tokio::test]
    async fn what_cannot_be_sent_is_refused_with_the_reason() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY_300))).unwrap();
        let output = rig.output();
        let sixty = offer.mode("3840x2160p60").unwrap().choice();
        let error = output
            .try_setting(sixty, Some(mediabox_core::ColorMode::new(ColorFormat::Rgb, 8)), true)
            .unwrap_err();
        assert!(error.contains("4:2:0"), "{error}");
        // A key the display does not list.
        let foreign = Timing::new(2560, 1440, 241_500, 2720, 1481, false, false).key();
        let error = output
            .try_setting(ResolutionChoice::Timing { key: foreign }, None, true)
            .unwrap_err();
        assert!(error.contains("listelemiyor"), "{error}");
        assert!(output.status().trial.is_none());
    }

    #[tokio::test]
    async fn an_owner_report_about_another_sink_or_mode_is_refused() {
        let rig = Rig::new();
        let offer = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let other = mediabox_platform::output::offer(
            &kernel(),
            &bytes(SONY_300),
            "HDMI-A-2",
            None,
            &mediabox_platform::video::RK3588_HDMI,
        )
        .unwrap();
        let output = rig.output();
        assert!(output.owner_report(applied(&other, &OutputSetting::default(), None)).is_err());
        let mut report = applied(&offer, &OutputSetting::default(), None);
        if let OwnerReport::Applied(applied) = &mut report {
            applied.timing_key = Timing::new(2560, 1440, 241_500, 2720, 1481, false, false).key();
            applied.label = "2560x1440p60".into();
        }
        assert!(output.owner_report(report).is_err());
        assert!(output.status().applied.is_none());
        output.owner_report(applied(&offer, &OutputSetting::default(), None)).unwrap();
        assert_eq!(output.status().applied.unwrap().label, "3840x2160p60");
    }

    #[tokio::test]
    async fn an_observer_that_stopped_is_not_the_display_now() {
        let rig = Rig::new();
        rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        assert!(output.status().offer.is_some());
        assert!(rig.plan().is_some());
        // The file is still there; the observer stopped writing it.
        let path = rig.paths().observer.join("snapshot.json");
        let mut old: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stale = observer::boottime_ns().saturating_sub(observer::STALE_AFTER.as_nanos() as u64 + 1_000_000_000);
        old["boottime_ns"] = stale.into();
        std::fs::write(&path, old.to_string()).unwrap();
        let status = output.status();
        assert!(status.offer.is_none() && status.display.is_none());
        assert!(status.error.is_some());
        assert_eq!(rig.plan(), None, "no plan from a display nobody is watching");
    }

    #[tokio::test]
    async fn the_plan_follows_the_generation_and_is_never_another_sinks() {
        let rig = Rig::new();
        let output = rig.output();
        // No observer yet: no plan, and the status says why.
        let status = output.status();
        assert!(status.error.is_some());
        assert_eq!(rig.plan(), None);

        let sony = rig.observe(4, Some(&bytes(SONY))).unwrap();
        output.reconcile();
        let plan = mediabox_platform::output::Plan::parse(&rig.plan().unwrap()).unwrap();
        assert_eq!(plan.provenance.generation.seq, 4);
        assert_eq!(plan.provenance.identity, identity(&sony));
        assert_eq!(plan.provenance.transmitter, "fdea0000.hdmi");
        assert_eq!(plan.provenance.source_profile, "rk3588-vendor61-dw-hdmi-qp@1");
        assert_eq!(plan.timing_key, sony.mode("3840x2160p60").unwrap().timing_key);

        // A generation that moved for any reason: the plan is rewritten for it.
        rig.observe(5, Some(&bytes(SONY)));
        output.reconcile();
        assert!(rig.plan().unwrap().contains("generation=5"));

        // Unplugged: no plan at all rather than the last sink's.
        rig.observe(6, None);
        output.reconcile();
        assert_eq!(rig.plan(), None);
        assert!(output.status().error.is_some());
    }

    #[tokio::test]
    async fn another_display_is_auto_and_the_record_moves_to_it() {
        let rig = Rig::new();
        let sony = rig.observe(1, Some(&bytes(SONY))).unwrap();
        let output = rig.output();
        let id = applied_trial(&output, &sony);
        output.keep(id).unwrap();
        let other = rig.observe(2, Some(&bytes(SONY_300))).unwrap();
        let status = output.status();
        assert_eq!(status.setting.edid_sha256, other.edid_sha256);
        assert_eq!(status.setting.resolution, ResolutionChoice::Auto);
        match rig.stored() {
            Some(StoredSetting::Current(kept)) => {
                assert_eq!(kept.edid_sha256, other.edid_sha256);
                assert_eq!(kept.resolution, ResolutionChoice::Auto);
            }
            other => panic!("{other:?}"),
        }
        assert!(rig.plan().unwrap().contains("kodi_screenmode=0384002160060.00000pstd"), "not the other sink's 30 Hz");
    }

    #[tokio::test]
    async fn a_legacy_record_is_migrated_once_by_its_checksums_and_then_bound_to_the_sha() {
        let rig = Rig::new();
        let sony = rig.observe(1, Some(&bytes(SONY))).unwrap();
        // What the daemon before this one wrote: block checksums, size and
        // millihertz, a colour by label.
        std::fs::write(
            rig.paths().setting,
            format!(
                r#"{{"sink":"{}","resolution":{{"kind":"fixed","width":3840,"height":2160,"refresh_mhz":30000,"interlaced":false}},"colours":{{"3840x2160p30":{{"format":"ycbcr422","bits":10}}}}}}"#,
                sony.legacy_checkvalue
            ),
        )
        .unwrap();
        let output = rig.output();
        let status = output.status();
        assert_eq!(status.setting.edid_sha256, sony.edid_sha256);
        assert_eq!(status.setting.resolution, thirty(&sony));
        let key = sony.mode("3840x2160p30").unwrap().timing_key;
        assert_eq!(
            status.setting.colours.get(&key),
            Some(&mediabox_core::ColorMode::new(ColorFormat::Ycbcr422, 10))
        );
        assert!(status.notes.is_empty(), "{:?}", status.notes);
        // Written back in the new form, bound to the SHA-256.
        assert!(matches!(rig.stored(), Some(StoredSetting::Current(ref kept)) if kept.edid_sha256 == sony.edid_sha256));

        // A second display with the same checksums and a different EDID is a
        // different display now: Auto, whatever the checksums say.
        let twin = rig.observe(2, Some(&sony_twin())).unwrap();
        assert_eq!(twin.legacy_checkvalue, sony.legacy_checkvalue);
        assert_ne!(twin.edid_sha256, sony.edid_sha256);
        let status = output.status();
        assert_eq!(status.setting.edid_sha256, twin.edid_sha256);
        assert_eq!(status.setting.resolution, ResolutionChoice::Auto);
    }

    #[tokio::test]
    async fn a_legacy_choice_that_names_more_than_one_timing_is_not_guessed() {
        let rig = Rig::new();
        let sony = rig.observe(1, Some(&bytes(SONY))).unwrap();
        // The kernel lists 3840x2160p24 twice, with different sync: the old
        // record's size and millihertz fit both.
        let legacy = mediabox_core::LegacySetting {
            sink: sony.legacy_checkvalue.clone(),
            resolution: mediabox_core::LegacyResolution::Fixed {
                width: 3840,
                height: 2160,
                refresh_mhz: 24_000,
                interlaced: false,
            },
            colours: Default::default(),
        };
        let mut kernel: Vec<ModeTiming> = kernel().iter().map(|timing| timing.mode).collect();
        let mut twin = Timing::new(3840, 2160, 297_000, 5500, 2250, false, false).mode;
        twin.hsync_start += 8;
        twin.hsync_end += 8;
        kernel.push(twin);
        let (setting, notes) = legacy.migrate(&sony, &kernel);
        assert_eq!(setting.resolution, ResolutionChoice::Auto);
        assert_eq!(setting.edid_sha256, sony.edid_sha256);
        assert!(notes.iter().any(|note| note.contains("tahmin edilmedi")), "{notes:?}");
    }
}
