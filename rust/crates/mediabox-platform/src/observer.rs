//! The display, observed from outside whoever owns it: the one account of
//! the hardware the rest of the product works from.
//!
//! `mediabox-display-observer` watches the selected output and publishes what
//! it sees as one snapshot: the generation, the sink's identity, the topology
//! and where audio and CEC may go, the kernel's mode list and the offer
//! computed from it, and what the display controller says is on the wire. The
//! control plane reads it and nothing else as hardware truth; it decides
//! nothing itself.
//!
//! How it looks (Gate 0, G0-1): sysfs, uevents, debugfs, the device tree. It
//! keeps no descriptor on the display device. The kernel's own mode list and
//! the connector's properties are read through `drm_query`, under AM-2, only
//! for a connector and EDID it has not read before -- never on every pass, and
//! never by taking the display. What it read is kept in its runtime directory,
//! so a restart does not read again.
//!
//! How it keeps up: level-triggered. Every pass reads everything again and
//! compares the result with the last one; a uevent only says "look now", and a
//! timer looks anyway every [`PERIOD`]. A lost uevent costs at most one period.
//!
//! What a generation is: `(boot_id, seq)`, where `seq` moves when the
//! *material* content of the snapshot changes -- the KMS device, the
//! connector, whether it is connected, the EDID's identity, the transmitter
//! and what hangs off it, the source profile, and the capability and mode
//! fingerprints. An event is not a generation; the same content seen twice is
//! the same generation, across restarts of the observer within a boot.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::debugfs::{self, Debugfs, Observed};
use crate::drm_query::{self, Access, KernelMode, Query};
use crate::edid::EdidReport;
use crate::source::{self, PropertySignature, SourceSignature};
use crate::{Confidence, Overrides, Platform, Roots};
use mediabox_core::{DisplayGeneration, DisplayIdentity, DisplayState, OutputOffer, Route};

/// The upper bound between two full looks.
pub const PERIOD: Duration = Duration::from_secs(2);
pub const SCHEMA: u32 = 2;

/// A snapshot older than this is not the display now: the observer has
/// stopped looking, and what it last saw is not taken as current.
pub const STALE_AFTER: Duration = Duration::from_secs(10);

/// The observer's runtime directory, under `/run` ([`Roots::run`]).
pub const RUNTIME: &str = "mediabox-display-observer";
const SNAPSHOT: &str = "snapshot.json";

/// Why a pass ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Startup,
    Uevent,
    Periodic,
}

/// The kernel's mode list for the connector, as far as it could be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KernelModes {
    Known {
        count: usize,
        fingerprint: String,
        preferred: Option<String>,
        keys: Vec<String>,
    },
    /// Not read, and why: no master, a transition, no connector.
    Deferred { reason: String },
}

/// The part of a snapshot a generation is made of.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Material {
    pub kms_device: Option<String>,
    pub connector: Option<String>,
    pub connected: Option<bool>,
    pub edid_sha256: Option<String>,
    pub transmitter: Option<String>,
    pub topology: Option<Confidence>,
    /// Where audio and CEC go: only a route [`crate::Output::audio_route`]
    /// and [`crate::Output::cec_route`] allow.
    pub audio: Option<String>,
    pub cec: Option<String>,
    pub source_profile: String,
    pub capability_fingerprint: Option<String>,
    pub kernel_modes_fingerprint: Option<String>,
}

impl Material {
    pub fn digest(&self) -> String {
        let text = serde_json::to_string(self).unwrap_or_default();
        hex(&Sha256::digest(text.as_bytes()))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Master {
    pub command: String,
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub generation: DisplayGeneration,
    pub material_digest: String,
    pub reason: Reason,
    pub boottime_ns: u64,
    pub realtime_ms: u64,
    pub material: Material,
    pub primary_minor: Option<u32>,
    pub physical_address: Option<String>,
    pub edid_status: Option<crate::edid::EdidStatus>,
    pub edid_legacy_checkvalue: Option<String>,
    /// The profile and how it was matched: `rk3588-vendor61-dw-hdmi-qp@1 (matched)`.
    pub source_profile: String,
    pub topology_evidence: Vec<String>,
    pub audio: Route,
    pub cec: Route,
    /// What the display controller reports for the connector (debugfs).
    pub current_mode: Observed<String>,
    pub bus_format: Observed<String>,
    /// The PHY clock of the transmitter the connector is bound to, when that
    /// binding is firm and the board publishes the clock.
    pub phy_clock_khz: Observed<u64>,
    pub debugfs: Debugfs,
    pub kernel_modes: KernelModes,
    /// What can be sent to this sink, from the kernel's mode list and its
    /// EDID under the source profile. `None` until the list has been read
    /// for this connector and EDID.
    pub offer: Option<OutputOffer>,
    pub drm_master: Observed<Master>,
    pub warnings: Vec<String>,
}

impl Snapshot {
    /// The output and sink this snapshot is about, when one is connected
    /// with a valid EDID.
    pub fn identity(&self) -> Option<DisplayIdentity> {
        Some(DisplayIdentity {
            connector: self.material.connector.clone()?,
            edid_sha256: self.material.edid_sha256.clone()?,
        })
        .filter(|_| self.material.connected == Some(true))
    }

    /// The part the control plane reports.
    pub fn display_state(&self) -> DisplayState {
        DisplayState {
            generation: self.generation.clone(),
            connector: self.material.connector.clone(),
            connected: self.material.connected == Some(true),
            edid_sha256: self.material.edid_sha256.clone(),
            transmitter: self.material.transmitter.clone(),
            topology: self
                .material
                .topology
                .map(|confidence| format!("{confidence:?}").to_lowercase())
                .unwrap_or_else(|| "unavailable".into()),
            audio: self.audio.clone(),
            cec: self.cec.clone(),
            source_profile: self.source_profile.clone(),
        }
    }
}

/// The snapshot the observer published under `runtime`, if there is one this
/// build can read and it is recent: the file outlives an observer that
/// stopped, and a sink can change while nobody is looking.
pub fn published(runtime: &Path) -> Option<Snapshot> {
    let text = std::fs::read_to_string(runtime.join(SNAPSHOT)).ok()?;
    let now = boottime_ns();
    serde_json::from_str::<Snapshot>(&text).ok().filter(|snapshot| {
        snapshot.schema == SCHEMA
            && now.saturating_sub(snapshot.boottime_ns) <= STALE_AFTER.as_nanos() as u64
    })
}

/// `CLOCK_BOOTTIME`, in nanoseconds: what a snapshot's age is measured in.
pub fn boottime_ns() -> u64 {
    clocks().0
}

/// Its generation alone.
pub fn published_generation(runtime: &Path) -> Option<DisplayGeneration> {
    published(runtime).map(|snapshot| snapshot.generation)
}

/// Where the observer keeps its generation and the last mode list it read,
/// so a restart continues both.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    boot_id: String,
    seq: u64,
    digest: String,
    /// The last kernel mode list read, the connector properties read with
    /// it, and the connector and EDID they were read for. A restart reuses
    /// them for the same connector and EDID instead of opening the display
    /// device again -- and a read that is deferred later is not a change in
    /// what the display offers.
    #[serde(default)]
    modes_for: String,
    #[serde(default)]
    modes_fingerprint: Option<String>,
    #[serde(default)]
    modes: Vec<KernelMode>,
    #[serde(default)]
    properties: Option<PropertySignature>,
}

/// The observer, between passes.
pub struct Observer {
    pub roots: Roots,
    pub runtime: PathBuf,
    pub drm: Option<Box<dyn Fn(&Platform) -> Box<dyn Access>>>,
    kept: Kept,
    last_query_note: Option<String>,
}

impl Observer {
    pub fn new(roots: Roots, runtime: PathBuf) -> Self {
        let kept = std::fs::read_to_string(runtime.join("generation.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            roots,
            runtime,
            drm: None,
            kept,
            last_query_note: None,
        }
    }

    pub fn snapshot_path(&self) -> PathBuf {
        self.runtime.join(SNAPSHOT)
    }

    /// One full look, published.
    pub fn reconcile(&mut self, reason: Reason) -> Snapshot {
        let platform = Platform::inspect(&self.roots, &Overrides::default());
        let boot_id = crate::roots::read_trimmed(self.roots.proc("sys/kernel/random/boot_id"))
            .unwrap_or_else(|| "unknown".into());
        if self.kept.boot_id != boot_id {
            self.kept = Kept {
                boot_id: boot_id.clone(),
                ..Kept::default()
            };
        }
        let selected = platform.selected_output();
        let edid_bytes = selected
            .map(|output| std::fs::read(output.connector.sysfs.join("edid")).unwrap_or_default());
        let edid = edid_bytes.as_deref().map(EdidReport::of);

        // The kernel's mode list and the connector's properties: read only
        // for a connector and EDID not read before.
        let modes_for = format!(
            "{}|{}",
            selected.map(|o| o.connector.name.as_str()).unwrap_or("-"),
            edid.as_ref()
                .and_then(|report| report.sha256.as_ref())
                .map(|id| id.0.as_str())
                .unwrap_or("-")
        );
        let mut deferred = None;
        let mut read_now = false;
        // A kept read with no modes in it is one written before the list was
        // kept (an upgrade): it is read again, not taken as a list of none.
        if self.kept.modes_for != modes_for
            || self.kept.modes_fingerprint.is_none()
            || self.kept.modes.is_empty()
        {
            let result = match (selected, &self.drm) {
                (Some(output), Some(open)) if output.connector.connected => {
                    let access = open(&platform);
                    drm_query::query(access.as_ref(), &output.connector.name)
                }
                (None, _) => Query::Deferred {
                    reason: "no selected output".into(),
                },
                (Some(_), None) => Query::Deferred {
                    reason: "DRM query disabled".into(),
                },
                (Some(_), Some(_)) => Query::Deferred {
                    reason: "the selected output is not connected".into(),
                },
            };
            match result {
                Query::Done(read) => {
                    self.kept.modes_for = modes_for.clone();
                    self.kept.modes_fingerprint = Some(fingerprint(&read.modes));
                    self.kept.modes = read.modes;
                    self.kept.properties = Some(read.properties);
                    self.last_query_note = None;
                    read_now = true;
                }
                Query::Deferred { reason } => {
                    if self.last_query_note.as_deref() != Some(&reason) {
                        eprintln!("mediabox-display-observer: {reason}");
                        self.last_query_note = Some(reason.clone());
                    }
                    deferred = Some(reason);
                }
            }
        }
        // What was read for another connector or EDID is not this one's.
        let current = self.kept.modes_for == modes_for
            && self.kept.modes_fingerprint.is_some()
            && !self.kept.modes.is_empty();
        let kernel_modes = match (&deferred, current) {
            (_, true) => kernel_modes(&self.kept.modes),
            (Some(reason), false) => KernelModes::Deferred {
                reason: reason.clone(),
            },
            (None, false) => KernelModes::Deferred {
                reason: "not read yet".into(),
            },
        };
        let properties = current.then(|| self.kept.properties.clone()).flatten();

        let resolved = source::resolve(&SourceSignature {
            static_part: source::static_signature(&self.roots, &platform),
            properties,
        });
        let capability_fingerprint = edid.as_ref().filter(|report| report.status.usable()).map(|report| {
            let text = serde_json::json!({
                "source": resolved.profile.name(),
                "cta": report.cta,
            })
            .to_string();
            hex(&Sha256::digest(text.as_bytes()))
        });

        let offer = match (selected, edid_bytes.as_deref()) {
            (Some(output), Some(bytes)) if current && output.connector.connected => {
                let timings: Vec<crate::video::Timing> = self
                    .kept
                    .modes
                    .iter()
                    .map(|mode| crate::video::Timing::from_mode(mode.timing, mode.preferred))
                    .collect();
                crate::output::offer(
                    &timings,
                    bytes,
                    &output.connector.name,
                    crate::output::edid_name(bytes),
                    &resolved.profile.caps,
                )
                .map(|mut offer| {
                    offer.link.source_profile = resolved.describe();
                    offer
                })
            }
            _ => None,
        };

        let route = |result: Result<String, String>| match result {
            Ok(device) => Route {
                device: Some(device),
                refused: None,
            },
            Err(why) => Route {
                device: None,
                refused: Some(why),
            },
        };
        let no_output = || Err("seçili bir ekran çıkışı yok".to_string());
        let audio = route(selected.map_or_else(no_output, |output| {
            output.audio_route().map(|audio| audio.card_id.clone())
        }));
        let cec = route(selected.map_or_else(no_output, |output| {
            output
                .cec_route()
                .map(|adapter| adapter.device.display().to_string())
        }));

        let material = Material {
            kms_device: platform.kms.as_ref().map(|node| {
                format!("{} ({})", node.name, node.parent.as_deref().unwrap_or("?"))
            }),
            connector: selected.map(|output| output.connector.name.clone()),
            connected: selected.map(|output| output.connector.connected),
            edid_sha256: edid.as_ref().and_then(|report| report.sha256.as_ref()).map(|id| id.0.clone()),
            transmitter: selected.and_then(|output| output.controller.clone()),
            topology: selected.map(|output| output.binding),
            audio: audio.device.clone(),
            cec: cec.device.clone(),
            source_profile: resolved.profile.name(),
            capability_fingerprint,
            kernel_modes_fingerprint: current.then(|| self.kept.modes_fingerprint.clone()).flatten(),
        };
        let digest = material.digest();
        let new_generation = self.kept.digest != digest;
        if new_generation {
            self.kept.seq += 1;
            self.kept.digest = digest.clone();
            eprintln!(
                "mediabox-display-observer: generation {}:{} ({reason:?}) connector={} connected={} edid={} transmitter={} ({:?}) audio={} cec={} modes={}",
                boot_id,
                self.kept.seq,
                material.connector.as_deref().unwrap_or("-"),
                material.connected.map(|c| c.to_string()).unwrap_or("-".into()),
                material.edid_sha256.as_deref().map(|s| &s[..16.min(s.len())]).unwrap_or("-"),
                material.transmitter.as_deref().unwrap_or("-"),
                material.topology,
                material.audio.as_deref().unwrap_or("-"),
                material.cec.as_deref().unwrap_or("-"),
                match &kernel_modes {
                    KernelModes::Known { count, .. } => count.to_string(),
                    KernelModes::Deferred { reason } => format!("deferred ({reason})"),
                }
            );
            // Why a binding is not firm, for whoever reads the journal: it is
            // what takes sound and CEC away.
            if let Some(output) = selected.filter(|output| !output.binding.actionable()) {
                eprintln!(
                    "mediabox-display-observer: {} is not firmly tied to a transmitter: {}",
                    output.connector.name,
                    output.evidence.join("; ")
                );
            }
        }
        if new_generation || read_now {
            let _ = write_atomic(
                &self.runtime.join("generation.json"),
                &serde_json::to_string(&self.kept).unwrap_or_default(),
            );
        }

        let summary = platform.debugfs.read("summary").known();
        let activity = selected.and_then(|output| {
            summary
                .as_deref()
                .and_then(|text| debugfs::connector_activity(text, &output.connector.name))
        });
        let observed = |value: Option<String>, what: &str| match (&summary, value) {
            (_, Some(value)) => Observed::Known(value),
            (None, None) => Observed::Unknown("debugfs summary not readable".into()),
            (Some(_), None) => Observed::Unknown(format!("the summary names no {what} for this connector")),
        };
        let current_mode = observed(activity.as_ref().and_then(|a| a.mode.clone()), "mode");
        let bus_format = observed(activity.as_ref().and_then(|a| a.bus_format.clone()), "bus format");
        let phy_clock_khz = match selected {
            None => Observed::Unknown("seçili bir ekran çıkışı yok".into()),
            Some(output) if !output.binding.actionable() => {
                Observed::Unknown(format!("the transmitter is not firmly known ({:?})", output.binding))
            }
            Some(output) => platform
                .measured
                .iter()
                .find(|(device, _)| Some(device) == output.controller.as_ref())
                .and_then(|(_, evidence)| evidence.phy_clock.as_ref())
                .map(|clock| match clock.enabled {
                    true => Observed::Known(clock.rate_hz / 1000),
                    false => Observed::Unknown(format!("{} is off", clock.name)),
                })
                .unwrap_or_else(|| Observed::Unknown("no PHY clock is published for this transmitter".into())),
        };
        let drm_master = match platform.debugfs.read("clients") {
            Observed::Known(table) => match current_master(&table) {
                Some(master) => Observed::Known(master),
                None => Observed::Unknown("no client is master".into()),
            },
            Observed::Unknown(why) => Observed::Unknown(why),
        };

        let (boottime_ns, realtime_ms) = clocks();
        let snapshot = Snapshot {
            schema: SCHEMA,
            generation: DisplayGeneration {
                boot_id,
                seq: self.kept.seq,
            },
            material_digest: digest,
            reason,
            boottime_ns,
            realtime_ms,
            primary_minor: match &platform.debugfs {
                Debugfs::Resolved { minor, .. } => Some(*minor),
                Debugfs::Unknown { .. } => None,
            },
            physical_address: selected
                .and_then(|output| output.connector.physical_address)
                .map(format_pa),
            edid_status: edid.as_ref().map(|report| report.status),
            edid_legacy_checkvalue: edid.as_ref().map(|report| report.legacy_checkvalue.clone()),
            source_profile: resolved.describe(),
            topology_evidence: selected.map(|output| output.evidence.clone()).unwrap_or_default(),
            audio,
            cec,
            current_mode,
            bus_format,
            phy_clock_khz,
            debugfs: platform.debugfs.clone(),
            kernel_modes,
            offer,
            drm_master,
            warnings: platform.warnings.clone(),
            material,
        };
        let _ = write_atomic(
            &self.snapshot_path(),
            &serde_json::to_string_pretty(&snapshot).unwrap_or_default(),
        );
        snapshot
    }
}

fn format_pa(address: u16) -> String {
    format!(
        "{:x}.{:x}.{:x}.{:x}",
        address >> 12,
        (address >> 8) & 0xF,
        (address >> 4) & 0xF,
        address & 0xF
    )
}

fn keys(modes: &[KernelMode]) -> (Vec<String>, Option<String>) {
    let mut keys: Vec<String> = modes.iter().map(|mode| mode.timing.key().to_string()).collect();
    let preferred = modes
        .iter()
        .find(|mode| mode.preferred)
        .map(|mode| mode.timing.key().to_string());
    keys.sort();
    keys.dedup();
    (keys, preferred)
}

fn fingerprint(modes: &[KernelMode]) -> String {
    let (keys, preferred) = keys(modes);
    let text = format!("{}|{}", keys.join(","), preferred.as_deref().unwrap_or(""));
    hex(&Sha256::digest(text.as_bytes()))
}

fn kernel_modes(modes: &[KernelMode]) -> KernelModes {
    let (keys, preferred) = keys(modes);
    KernelModes::Known {
        count: modes.len(),
        fingerprint: fingerprint(modes),
        preferred,
        keys,
    }
}

/// The client that holds master, from debugfs `clients`.
pub fn current_master(clients: &str) -> Option<Master> {
    for row in clients.lines().skip(1) {
        let fields: Vec<&str> = row.split_whitespace().collect();
        if fields.len() < 7 {
            continue;
        }
        let n = fields.len();
        if fields[n - 4] == "y" {
            return Some(Master {
                command: fields[..n - 6].join(" "),
                pid: fields[n - 6].parse().ok()?,
            });
        }
    }
    None
}

fn clocks() -> (u64, u64) {
    let read = |clock: libc::clockid_t| {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a valid clock id and a live timespec.
        unsafe { libc::clock_gettime(clock, &mut now) };
        now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
    };
    (read(libc::CLOCK_BOOTTIME), read(libc::CLOCK_REALTIME) / 1_000_000)
}

/// Written beside and renamed over, so a reader never sees half a file.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, path)
}

// ------------------------------------------------------------ waiting

/// What woke the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// A uevent from the DRM subsystem: a hint to look now.
    Hint,
    /// The period elapsed with nothing heard.
    Timeout,
    Stop,
}

/// Run passes until told to stop: one at start, one per hint, and one per
/// period regardless -- so that correctness does not depend on an event
/// arriving.
pub fn run(observer: &mut Observer, mut wait: impl FnMut(Duration) -> Wake) {
    observer.reconcile(Reason::Startup);
    loop {
        match wait(PERIOD) {
            Wake::Hint => {
                observer.reconcile(Reason::Uevent);
            }
            Wake::Timeout => {
                observer.reconcile(Reason::Periodic);
            }
            Wake::Stop => return,
        }
    }
}

/// The kernel's uevent broadcast, for the DRM subsystem's messages.
pub struct Uevents {
    fd: std::os::fd::OwnedFd,
}

impl Uevents {
    pub fn open() -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        // SAFETY: plain socket creation; the result is checked.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                libc::NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a descriptor just created, owned from here on.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        // SAFETY: zeroed sockaddr_nl is a valid value; fields set below.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address.nl_groups = 1; // the kernel's own broadcast, not udev's
        // SAFETY: a valid descriptor and a correctly sized address.
        let bound = unsafe {
            libc::bind(
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                &address as *const libc::sockaddr_nl as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bound != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    /// Wait up to `timeout` for a DRM uevent. Other subsystems' messages are
    /// read and dropped. A burst is coalesced: after the first, anything
    /// arriving within a short settle time is folded into it.
    pub fn wait(&self, timeout: Duration) -> Wake {
        let deadline = std::time::Instant::now() + timeout;
        let mut heard = false;
        loop {
            let now = std::time::Instant::now();
            let left = if heard {
                Duration::from_millis(250)
            } else if now >= deadline {
                return Wake::Timeout;
            } else {
                deadline - now
            };
            let mut poll = libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&self.fd),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one live pollfd.
            let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis().min(i32::MAX as u128) as i32) };
            if ready < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    if crate::observer::stop_requested() {
                        return Wake::Stop;
                    }
                    continue;
                }
                return Wake::Timeout;
            }
            if crate::observer::stop_requested() {
                return Wake::Stop;
            }
            if ready == 0 {
                return if heard { Wake::Hint } else { Wake::Timeout };
            }
            let mut buffer = [0u8; 8192];
            loop {
                // SAFETY: a live descriptor and a buffer of the stated size.
                let read = unsafe {
                    libc::recv(
                        std::os::fd::AsRawFd::as_raw_fd(&self.fd),
                        buffer.as_mut_ptr() as *mut libc::c_void,
                        buffer.len(),
                        libc::MSG_DONTWAIT,
                    )
                };
                if read <= 0 {
                    break;
                }
                if is_display_uevent(&buffer[..read as usize]) {
                    heard = true;
                }
            }
        }
    }
}

/// A uevent the display cares about: the DRM subsystem, or a CEC adapter.
pub fn is_display_uevent(message: &[u8]) -> bool {
    message
        .split(|byte| *byte == 0)
        .any(|field| field == b"SUBSYSTEM=drm" || field == b"SUBSYSTEM=cec")
}

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_stop() {
    STOP.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn stop_requested() -> bool {
    STOP.load(std::sync::atomic::Ordering::SeqCst)
}
