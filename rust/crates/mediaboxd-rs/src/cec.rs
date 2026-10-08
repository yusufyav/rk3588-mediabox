//! Which CEC adapter a command is sent down.
//!
//! Every adapter on the board is opened and listened to -- the remote's keys
//! arrive on whichever socket the television is on, and receiving from all of
//! them is harmless. Sending is not. A board with two HDMI transmitters and two
//! televisions has two live adapters, and "the first one with an address" is
//! whichever television happens to be on the lower-numbered socket, not the one
//! the picture is on.
//!
//! So a command goes where the picture goes:
//!
//! ```text
//! selected output -> its transmitter -> that transmitter's CEC adapter
//! ```
//!
//! resolved by `mediabox-platform` with a confidence, and refused -- with the
//! reason, never with a quiet fallback to another adapter -- when the chain is
//! ambiguous, when the adapter has no physical address or logical address, or
//! when the address it holds is not the one the selected sink's EDID declares.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mediabox_platform::{Confidence, Platform};

/// Where the next command should go, as the platform resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CecTarget {
    Adapter {
        path: PathBuf,
        /// The physical address the selected sink's EDID declares, when it
        /// declares one; the adapter must hold the same.
        expected_address: Option<u16>,
        confidence: Confidence,
    },
    Refused(String),
}

/// The target for the output this box is on.
pub fn target(platform: &Platform) -> CecTarget {
    let Some(output) = platform.selected_output() else {
        return CecTarget::Refused("seçili bir ekran çıkışı yok".into());
    };
    // The same rule the sound card follows (`Output::audio_route`): one
    // answer to "which transmitter is the picture on", or none.
    match output.cec_route() {
        Ok(adapter) => CecTarget::Adapter {
            path: adapter.device.clone(),
            expected_address: output.connector.physical_address,
            confidence: output.cec_confidence(),
        },
        Err(why) => CecTarget::Refused(why),
    }
}

/// What a sending path needs to know of an adapter.
pub trait CecPort {
    fn path(&self) -> &Path;
    /// The physical and logical address the kernel holds for it now.
    fn addresses(&self) -> (u16, Option<u8>);
}

impl CecPort for mediabox_cec::Adapter {
    fn path(&self) -> &Path {
        mediabox_cec::Adapter::path(self)
    }
    fn addresses(&self) -> (u16, Option<u8>) {
        mediabox_cec::Adapter::addresses(self)
    }
}

const NO_ADDRESS: u16 = 0xFFFF;

/// The open adapter a command may be sent down, or why none may.
pub fn choose<P: CecPort>(ports: &[Arc<P>], target: &CecTarget) -> Result<Arc<P>, String> {
    let (path, expected) = match target {
        CecTarget::Refused(why) => return Err(why.clone()),
        CecTarget::Adapter {
            path,
            expected_address,
            ..
        } => (path, *expected_address),
    };
    let port = ports
        .iter()
        .find(|port| port.path() == path.as_path())
        .ok_or_else(|| format!("seçili çıkışın CEC bağdaştırıcısı {} açık değil", path.display()))?;
    let (physical, logical) = port.addresses();
    if physical == NO_ADDRESS {
        return Err(format!(
            "{}: fiziksel adres yok (f.f.f.f); televizyon bu soketi görmüyor",
            path.display()
        ));
    }
    if logical.is_none() {
        return Err(format!("{}: mantıksal adres alınamadı", path.display()));
    }
    if let Some(expected) = expected
        && expected != physical
    {
        return Err(format!(
            "{}: bağdaştırıcı {} tutuyor, seçili ekranın EDID'i {} bildiriyor",
            path.display(),
            mediabox_cec::format_physical_address(physical),
            mediabox_cec::format_physical_address(expected)
        ));
    }
    Ok(Arc::clone(port))
}


// ------------------------------------------------------------------ policy

/// Where the HDMI-CEC panel is kept. The daemon's state directory, beside
/// the lights' mode.
pub const SETTINGS_FILE: &str = "/var/lib/mediabox/cec.json";
/// What this boot did, tagged with the boot it belongs to. Not under /run:
/// the unit's `RuntimeDirectory` is removed when the daemon stops, and a
/// deploy would then look like a boot and wake the television again.
pub const SESSION_FILE: &str = "/var/lib/mediabox/cec-session.json";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SessionRecord {
    boot_id: String,
    session: mediabox_core::CecSession,
}

/// The kept settings and this boot's session, and the files they live in.
pub struct Policy {
    settings_file: PathBuf,
    session_file: PathBuf,
    boot_id: String,
    settings: std::sync::Mutex<mediabox_core::CecSettings>,
    session: std::sync::Mutex<mediabox_core::CecSession>,
}

impl Policy {
    /// A session file from another boot is a fresh session: the start
    /// sequence runs again, and nothing is believed about the television.
    pub fn load(settings_file: impl Into<PathBuf>, session_file: impl Into<PathBuf>, boot_id: &str) -> Self {
        let settings_file = settings_file.into();
        let session_file = session_file.into();
        let settings = std::fs::read(&settings_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<mediabox_core::CecSettings>(&bytes).ok())
            .filter(|settings| settings.validate().is_ok())
            .unwrap_or_default();
        let session = std::fs::read(&session_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SessionRecord>(&bytes).ok())
            .filter(|record| record.boot_id == boot_id)
            .map(|record| record.session)
            .unwrap_or_default();
        Self {
            settings_file,
            session_file,
            boot_id: boot_id.to_string(),
            settings: std::sync::Mutex::new(settings),
            session: std::sync::Mutex::new(session),
        }
    }

    pub fn system() -> Self {
        let boot_id = std::fs::read_to_string(BOOT_ID).unwrap_or_default();
        Self::load(SETTINGS_FILE, SESSION_FILE, boot_id.trim())
    }

    pub fn settings(&self) -> mediabox_core::CecSettings {
        *self.settings.lock().expect("cec settings")
    }

    pub fn session(&self) -> mediabox_core::CecSession {
        *self.session.lock().expect("cec session")
    }

    pub fn enabled(&self) -> bool {
        self.settings().enabled
    }

    /// What the adapters are told to be under the kept settings.
    pub fn claim(&self) -> mediabox_cec::Claim {
        let settings = self.settings();
        if settings.enabled {
            mediabox_cec::Claim::Playback { remote_control: settings.remote_control }
        } else {
            mediabox_cec::Claim::Released
        }
    }

    /// Validated, written, and only then in force. Returns the settings
    /// it replaced.
    pub fn set(&self, settings: mediabox_core::CecSettings) -> Result<mediabox_core::CecSettings, SetError> {
        settings.validate().map_err(|error| SetError::Invalid(error.to_string()))?;
        let mut kept = self.settings.lock().expect("cec settings");
        write_json(&self.settings_file, &settings)
            .map_err(|error| SetError::Write(format!("{}: {error}", self.settings_file.display())))?;
        Ok(std::mem::replace(&mut *kept, settings))
    }

    /// Changes the session, and writes it when it changed.
    pub fn session_update<T>(&self, change: impl FnOnce(&mut mediabox_core::CecSession) -> T) -> T {
        let mut session = self.session.lock().expect("cec session");
        let before = *session;
        let out = change(&mut session);
        if *session != before {
            let record = SessionRecord { boot_id: self.boot_id.clone(), session: *session };
            if let Err(error) = write_json(&self.session_file, &record) {
                eprintln!("mediaboxd-rs: CEC session not written: {error}");
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetError {
    Invalid(String),
    Write(String),
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let temporary = path.with_extension("json.part");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

/// Why the daemon is stopping, from systemd's job queue: a poweroff or a
/// reboot is a job on its target while every unit is being stopped.
/// `requested` is what this daemon was itself asked for, for a queue that
/// could not be read.
pub fn stop_reason(jobs: Option<&str>, requested: Option<mediabox_core::PowerAction>) -> mediabox_cec::policy::Stop {
    use mediabox_cec::policy::Stop;
    if let Some(jobs) = jobs {
        for unit in jobs.lines().filter_map(|line| line.split_whitespace().nth(1)) {
            match unit {
                "poweroff.target" | "halt.target" => return Stop::PowerOff,
                "reboot.target" | "kexec.target" | "soft-reboot.target" => return Stop::Reboot,
                _ => {}
            }
        }
    }
    match requested {
        Some(mediabox_core::PowerAction::Shutdown) => Stop::PowerOff,
        Some(mediabox_core::PowerAction::Restart) => Stop::Reboot,
        None => Stop::Daemon,
    }
}

/// The few messages the policy sends, for one adapter.
pub trait Bus {
    fn power_status(&self) -> Result<mediabox_core::TvPower, String>;
    fn wake(&self) -> Result<(), String>;
    fn active_source(&self) -> Result<(), String>;
    fn standby(&self, destination: u8) -> Result<(), String>;
    fn inactive_source(&self) -> Result<(), String>;
}

impl Bus for mediabox_cec::Adapter {
    fn power_status(&self) -> Result<mediabox_core::TvPower, String> {
        mediabox_cec::Adapter::power_status(self, mediabox_cec::policy::TV, 1000).map_err(|e| e.to_string())
    }
    fn wake(&self) -> Result<(), String> {
        self.wake_tv().map_err(|e| e.to_string())
    }
    fn active_source(&self) -> Result<(), String> {
        mediabox_cec::Adapter::active_source(self).map_err(|e| e.to_string())
    }
    fn standby(&self, destination: u8) -> Result<(), String> {
        mediabox_cec::Adapter::standby(self, destination).map_err(|e| e.to_string())
    }
    fn inactive_source(&self) -> Result<(), String> {
        mediabox_cec::Adapter::inactive_source(self).map_err(|e| e.to_string())
    }
}

impl<B: Bus> Bus for Arc<B> {
    fn power_status(&self) -> Result<mediabox_core::TvPower, String> {
        B::power_status(self)
    }
    fn wake(&self) -> Result<(), String> {
        B::wake(self)
    }
    fn active_source(&self) -> Result<(), String> {
        B::active_source(self)
    }
    fn standby(&self, destination: u8) -> Result<(), String> {
        B::standby(self, destination)
    }
    fn inactive_source(&self) -> Result<(), String> {
        B::inactive_source(self)
    }
}

/// How long the start sequence waits for the selected adapter to hold an
/// address, and how often it looks. A television that is off may keep its
/// hotplug line low for a while; a boot is never held up by it either way,
/// because this runs beside the daemon, not in front of it.
pub const START_WAIT_SECONDS: u32 = 30;
/// Attempts per message. The kernel already retransmits on the wire; this
/// covers an arbitration lost to a busy bus or an address being re-claimed.
pub const SEND_ATTEMPTS: u32 = 3;

/// `f`, up to [`SEND_ATTEMPTS`] times, `pause` between.
pub fn attempt<T>(mut f: impl FnMut() -> Result<T, String>, pause: &mut impl FnMut()) -> Result<T, String> {
    let mut last = String::new();
    for round in 0..SEND_ATTEMPTS {
        if round > 0 {
            pause();
        }
        match f() {
            Ok(value) => return Ok(value),
            Err(why) => last = why,
        }
    }
    Err(last)
}

/// What the start sequence did, for the log and for the tests.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartReport {
    pub tv_power: mediabox_core::TvPower,
    pub woke: bool,
    pub announced: bool,
    pub notes: Vec<String>,
}

/// One Touch Play, bounded: wait for the selected adapter (`ready`, once a
/// second up to [`START_WAIT_SECONDS`]), ask the television its state,
/// wake it, say this is the active source. `enabled` is read before each
/// wait, so turning CEC off meanwhile ends it.
pub fn run_start<B: Bus>(
    plan: mediabox_cec::policy::Start,
    mut ready: impl FnMut(u32) -> Result<B, String>,
    enabled: impl Fn() -> bool,
    mut second: impl FnMut(),
    mut pause: impl FnMut(),
) -> Result<StartReport, String> {
    let mut report = StartReport::default();
    let mut bus = None;
    let mut last = String::new();
    for round in 0..START_WAIT_SECONDS {
        if !enabled() {
            return Err("HDMI-CEC kapatıldı".into());
        }
        match ready(round) {
            Ok(found) => {
                bus = Some(found);
                break;
            }
            Err(why) => last = why,
        }
        second();
    }
    let bus = bus.ok_or_else(|| format!("{START_WAIT_SECONDS} sn içinde hazır olmadı: {last}"))?;
    if plan.ask_power {
        match attempt(|| bus.power_status(), &mut pause) {
            Ok(power) => report.tv_power = power,
            Err(why) => report.notes.push(format!("güç durumu okunamadı: {why}")),
        }
    }
    if mediabox_cec::policy::wake_needed(&plan, report.tv_power) {
        match attempt(|| bus.wake(), &mut pause) {
            Ok(()) => report.woke = true,
            Err(why) => report.notes.push(format!("Image View On gitmedi: {why}")),
        }
    }
    if plan.active_source {
        match attempt(|| bus.active_source(), &mut pause) {
            Ok(()) => report.announced = true,
            Err(why) => report.notes.push(format!("Active Source gitmedi: {why}")),
        }
    }
    Ok(report)
}

/// What the shutdown plan says, while `time_left` says there is time: a
/// shutdown is never held by the bus, and the unit gives the daemon ten
/// seconds in all. `<Standby>` to each destination first, then
/// `<Inactive Source>`. Each result is named by the message it was.
pub fn run_shutdown<B: Bus>(
    bus: &B,
    plan: &mediabox_cec::policy::Shutdown,
    time_left: impl Fn() -> bool,
    mut pause: impl FnMut(),
) -> Vec<(String, Result<(), String>)> {
    let mut sent = Vec::new();
    let mut send = |name: String, f: &dyn Fn() -> Result<(), String>, pause: &mut dyn FnMut()| {
        let mut result = Err("süre doldu".to_string());
        for round in 0..SEND_ATTEMPTS {
            if !time_left() {
                break;
            }
            if round > 0 {
                pause();
            }
            result = f();
            if result.is_ok() {
                break;
            }
        }
        sent.push((name, result));
    };
    for destination in &plan.standby {
        send(format!("standby -> {destination}"), &|| bus.standby(*destination), &mut pause);
    }
    if plan.inactive_source {
        send("inactive source".into(), &|| bus.inactive_source(), &mut pause);
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Fake {
        path: PathBuf,
        physical: u16,
        logical: Option<u8>,
    }

    impl CecPort for Fake {
        fn path(&self) -> &Path {
            &self.path
        }
        fn addresses(&self) -> (u16, Option<u8>) {
            (self.physical, self.logical)
        }
    }

    fn port(name: &str, physical: u16, logical: Option<u8>) -> Arc<Fake> {
        Arc::new(Fake {
            path: PathBuf::from(format!("/dev/{name}")),
            physical,
            logical,
        })
    }

    fn to(name: &str, expected: Option<u16>) -> CecTarget {
        CecTarget::Adapter {
            path: PathBuf::from(format!("/dev/{name}")),
            expected_address: expected,
            confidence: Confidence::Measured,
        }
    }

    #[test]
    fn two_live_adapters_the_command_goes_to_the_selected_outputs() {
        // cec0 live, cec1 live; the selected transmitter's adapter is cec1.
        let ports = [port("cec0", 0x1000, Some(4)), port("cec1", 0x3000, Some(4))];
        let chosen = choose(&ports, &to("cec1", Some(0x3000))).unwrap();
        assert_eq!(chosen.path(), Path::new("/dev/cec1"));
    }

    #[test]
    fn an_ambiguous_topology_sends_nothing() {
        let ports = [port("cec0", 0x1000, Some(4)), port("cec1", 0x1000, Some(4))];
        let error = choose(&ports, &CecTarget::Refused("belirsiz".into())).unwrap_err();
        assert_eq!(error, "belirsiz");
    }

    #[test]
    fn a_selected_adapter_with_no_address_is_an_error_not_a_fallback() {
        // The other adapter is live; it is still not where the command goes.
        let ports = [port("cec0", 0x1000, Some(4)), port("cec1", NO_ADDRESS, None)];
        let error = choose(&ports, &to("cec1", None)).unwrap_err();
        assert!(error.contains("f.f.f.f"), "{error}");
        let ports = [port("cec0", 0x1000, Some(4)), port("cec1", 0x3000, None)];
        assert!(choose(&ports, &to("cec1", None)).unwrap_err().contains("mantıksal"));
    }

    #[test]
    fn an_adapter_holding_another_sinks_address_is_refused() {
        let ports = [port("cec1", 0x2000, Some(4))];
        let error = choose(&ports, &to("cec1", Some(0x3000))).unwrap_err();
        assert!(error.contains("2.0.0.0") && error.contains("3.0.0.0"), "{error}");
    }

    #[test]
    fn a_selected_adapter_that_is_not_open_is_refused() {
        let ports = [port("cec0", 0x1000, Some(4))];
        assert!(choose(&ports, &to("cec1", None)).unwrap_err().contains("/dev/cec1"));
    }

    // ------------------------------------------------------------ policy

    use mediabox_cec::policy::{Start, Stop};
    use mediabox_core::{CecSettings, TvPower};
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct Recorder {
        power: Option<TvPower>,
        /// How many times each kind fails before it succeeds.
        failures: Cell<u32>,
        sent: RefCell<Vec<String>>,
    }

    impl Recorder {
        fn go(&self, what: String) -> Result<(), String> {
            if self.failures.get() > 0 {
                self.failures.set(self.failures.get() - 1);
                self.sent.borrow_mut().push(format!("{what}!"));
                return Err("tx_status=0x12".into());
            }
            self.sent.borrow_mut().push(what);
            Ok(())
        }
    }

    impl Bus for &Recorder {
        fn power_status(&self) -> Result<TvPower, String> {
            self.go("8f".into())?;
            Ok(self.power.unwrap_or_default())
        }
        fn wake(&self) -> Result<(), String> {
            self.go("04".into())
        }
        fn active_source(&self) -> Result<(), String> {
            self.go("82".into())
        }
        fn standby(&self, destination: u8) -> Result<(), String> {
            self.go(format!("36>{destination}"))
        }
        fn inactive_source(&self) -> Result<(), String> {
            self.go("9d".into())
        }
    }

    fn everything() -> Start {
        Start { ask_power: true, wake: true, active_source: true }
    }

    #[test]
    fn start_waits_for_the_adapter_a_bounded_time() {
        let bus = Recorder { power: Some(TvPower::Standby), ..Default::default() };
        let seconds = Cell::new(0);
        // The address arrives on the fourth look.
        let report = run_start(
            everything(),
            |round| if round < 3 { Err("mantıksal adres alınamadı".into()) } else { Ok(&bus) },
            || true,
            || seconds.set(seconds.get() + 1),
            || {},
        )
        .unwrap();
        assert_eq!(seconds.get(), 3);
        assert_eq!(*bus.sent.borrow(), ["8f", "04", "82"]);
        assert!(report.woke && report.announced);
        assert_eq!(report.tv_power, TvPower::Standby);

        // Never arrives: it gives up after the bound, having sent nothing.
        let seconds = Cell::new(0);
        let error = run_start(
            everything(),
            |_| Err::<&Recorder, _>("f.f.f.f".into()),
            || true,
            || seconds.set(seconds.get() + 1),
            || {},
        )
        .unwrap_err();
        assert_eq!(seconds.get(), START_WAIT_SECONDS);
        assert!(error.contains("f.f.f.f"), "{error}");
    }

    #[test]
    fn turning_cec_off_during_the_wait_ends_it() {
        let looks = Cell::new(0);
        let result = run_start(
            everything(),
            |_| Err::<&Recorder, _>("yok".into()),
            || looks.get() < 2,
            || looks.set(looks.get() + 1),
            || {},
        );
        assert!(result.unwrap_err().contains("kapatıldı"));
        assert_eq!(looks.get(), 2);
    }

    #[test]
    fn a_failed_transmit_is_tried_a_bounded_number_of_times() {
        let bus = Recorder { failures: Cell::new(2), ..Default::default() };
        let plan = Start { active_source: true, ..Default::default() };
        let report = run_start(plan, |_| Ok(&bus), || true, || {}, || {}).unwrap();
        assert!(report.announced);
        assert_eq!(*bus.sent.borrow(), ["82!", "82!", "82"]);

        let bus = Recorder { failures: Cell::new(10), ..Default::default() };
        let report = run_start(plan, |_| Ok(&bus), || true, || {}, || {}).unwrap();
        assert!(!report.announced);
        assert_eq!(bus.sent.borrow().len(), SEND_ATTEMPTS as usize);
        assert!(report.notes[0].contains("Active Source"));
    }

    #[test]
    fn a_television_already_on_is_asked_but_not_woken() {
        let bus = Recorder { power: Some(TvPower::On), ..Default::default() };
        let report = run_start(everything(), |_| Ok(&bus), || true, || {}, || {}).unwrap();
        assert_eq!(*bus.sent.borrow(), ["8f", "82"]);
        assert!(!report.woke);
    }

    #[test]
    fn shutdown_sends_to_each_target_and_stops_when_time_is_up() {
        let bus = Recorder::default();
        let plan = |standby: Vec<u8>, inactive_source| mediabox_cec::policy::Shutdown { standby, inactive_source };
        let sent = run_shutdown(&&bus, &plan(vec![0, 5], false), || true, || {});
        assert_eq!(*bus.sent.borrow(), ["36>0", "36>5"]);
        assert!(sent.iter().all(|(_, result)| result.is_ok()));

        let bus = Recorder { failures: Cell::new(100), ..Default::default() };
        let budget = Cell::new(4u32);
        let sent = run_shutdown(
            &&bus,
            &plan(vec![0, 5], false),
            || {
                budget.set(budget.get().saturating_sub(1));
                budget.get() > 0
            },
            || {},
        );
        assert!(bus.sent.borrow().len() <= 3, "{:?}", bus.sent.borrow());
        assert!(sent.iter().all(|(_, result)| result.is_err()));

        // The television left on: only Inactive Source.
        let bus = Recorder::default();
        let sent = run_shutdown(&&bus, &plan(vec![], true), || true, || {});
        assert_eq!(*bus.sent.borrow(), ["9d"]);
        assert_eq!(sent[0].0, "inactive source");
    }

    #[test]
    fn the_stop_reason_comes_from_systemd_s_queue() {
        let poweroff = "123 poweroff.target start waiting\n124 systemd-poweroff.service start waiting\n";
        let reboot = "130 reboot.target start waiting\n";
        let deploy = "200 mediaboxd-rs.service stop running\n";
        assert_eq!(stop_reason(Some(poweroff), None), Stop::PowerOff);
        assert_eq!(stop_reason(Some(reboot), None), Stop::Reboot);
        assert_eq!(stop_reason(Some(deploy), None), Stop::Daemon);
        assert_eq!(stop_reason(Some(""), None), Stop::Daemon);
        // The queue wins over a request; an unreadable queue falls back to it.
        assert_eq!(stop_reason(Some(reboot), Some(mediabox_core::PowerAction::Shutdown)), Stop::Reboot);
        assert_eq!(stop_reason(None, Some(mediabox_core::PowerAction::Shutdown)), Stop::PowerOff);
        assert_eq!(stop_reason(None, Some(mediabox_core::PowerAction::Restart)), Stop::Reboot);
        assert_eq!(stop_reason(None, None), Stop::Daemon);
    }

    #[test]
    fn settings_survive_a_restart_and_a_bad_pair_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, session) = (dir.path().join("cec.json"), dir.path().join("s.json"));
        let policy = Policy::load(&settings, &session, "boot-a");
        assert_eq!(policy.settings(), CecSettings::default());
        let chosen = CecSettings { wake_tv_on_start: true, remote_control: false, ..Default::default() };
        policy.set(chosen).unwrap();
        let bad = CecSettings { standby_tv_on_shutdown: true, restore_power_on_shutdown: true, ..chosen };
        assert!(matches!(policy.set(bad), Err(SetError::Invalid(_))));
        assert_eq!(policy.settings(), chosen);
        let again = Policy::load(&settings, &session, "boot-a");
        assert_eq!(again.settings(), chosen);
        assert_eq!(again.claim(), mediabox_cec::Claim::Playback { remote_control: false });
        let off = Policy::load(&settings, &session, "boot-a");
        off.set(chosen.with(mediabox_core::CecChange::Enabled(false))).unwrap();
        assert_eq!(off.claim(), mediabox_cec::Claim::Released);
    }

    #[test]
    fn the_session_belongs_to_one_boot() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, session) = (dir.path().join("cec.json"), dir.path().join("s.json"));
        let policy = Policy::load(&settings, &session, "boot-a");
        policy.session_update(|s| {
            s.woke_tv = true;
            s.tv_power_at_start = TvPower::Standby;
            s.start_done = true;
        });
        // A daemon restart in the same boot remembers it woke the television.
        let restarted = Policy::load(&settings, &session, "boot-a");
        assert!(restarted.session().woke_tv && restarted.session().start_done);
        // The next boot starts over.
        let next = Policy::load(&settings, &session, "boot-b");
        assert_eq!(next.session(), mediabox_core::CecSession::default());
    }
}
