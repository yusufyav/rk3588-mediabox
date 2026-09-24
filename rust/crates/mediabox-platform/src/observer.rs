//! The display, observed from outside whoever owns it. Shadow mode.
//!
//! `mediabox-display-observer` watches the selected output and publishes what
//! it sees as one snapshot. It decides nothing: the control plane and the
//! interface still make every display decision exactly as before, and nothing
//! reads this snapshot to act on it yet. It is here to be compared with them,
//! so that when decisions do move to it (Display Architecture v2, P6) they move
//! onto something that has been watched agreeing with the product first.
//!
//! How it looks (Gate 0, G0-1): sysfs, uevents, debugfs, the device tree. It
//! keeps no descriptor on the display device. The kernel's own mode list and
//! the connector's properties are read through `drm_query`, under AM-2, only
//! when what the observer sees materially changes (or an earlier read was
//! deferred) -- never on every pass, and never by taking the display.
//!
//! How it keeps up: level-triggered. Every pass reads everything again and
//! compares the result with the last one; a uevent only says "look now", and a
//! timer looks anyway every [`PERIOD`]. A lost uevent costs at most one period.
//!
//! What a generation is: `(boot_id, seq)`, where `seq` moves when the
//! *material* content of the snapshot changes -- the KMS device, the
//! connector, whether it is connected, the EDID's identity, the transmitter
//! and what hangs off it, and the capability and mode fingerprints. An event
//! is not a generation; the same content seen twice is the same generation,
//! across restarts of the observer within a boot.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::debugfs::{self, Debugfs, Observed};
use crate::drm_query::{self, Access, Query};
use crate::edid::EdidReport;
use crate::source::{self, SourceSignature};
use crate::{Confidence, Overrides, Platform, Roots};

/// The upper bound between two full looks.
pub const PERIOD: Duration = Duration::from_secs(5);
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub boot_id: String,
    pub seq: u64,
}

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
    pub audio: Option<String>,
    pub cec: Option<String>,
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

/// What the control plane's own report says, beside the observer's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    pub daemon_reachable: bool,
    pub daemon_connector: Option<String>,
    pub daemon_edid_sha256: Option<String>,
    pub daemon_source_profile: Option<String>,
    /// Every field both sides have, equal.
    pub agrees: bool,
    pub differences: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    /// Always true in this wave: nothing acts on this snapshot.
    pub shadow: bool,
    pub generation: Generation,
    pub material_digest: String,
    pub reason: Reason,
    pub boottime_ns: u64,
    pub realtime_ms: u64,
    pub material: Material,
    pub primary_minor: Option<u32>,
    pub physical_address: Option<String>,
    pub edid_status: Option<crate::edid::EdidStatus>,
    pub edid_legacy_checkvalue: Option<String>,
    pub source_profile: String,
    pub topology_evidence: Vec<String>,
    pub audio_confidence: Option<Confidence>,
    pub cec_confidence: Option<Confidence>,
    pub current_mode: Observed<String>,
    pub bus_format: Observed<String>,
    pub debugfs: Debugfs,
    pub kernel_modes: KernelModes,
    pub drm_master: Observed<Master>,
    pub comparison: Option<Comparison>,
    pub warnings: Vec<String>,
}

/// Where the observer keeps its generation, so a restart continues it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    boot_id: String,
    seq: u64,
    digest: String,
    /// The last kernel mode list read, and the connector and EDID it was
    /// read for. A read that is deferred later -- a restart in a masterless
    /// window -- is not a change in what the display offers, so it carries
    /// this forward instead of moving the generation.
    #[serde(default)]
    modes_for: String,
    #[serde(default)]
    modes_fingerprint: Option<String>,
}

/// The observer, between passes.
pub struct Observer {
    pub roots: Roots,
    pub runtime: PathBuf,
    pub daemon_socket: Option<PathBuf>,
    pub drm: Option<Box<dyn Fn(&Platform) -> Box<dyn Access>>>,
    kept: Kept,
    /// The last kernel mode read, the connector properties read with it, and
    /// what they were read for (connector and EDID).
    modes: Option<(String, KernelModes, Option<crate::source::PropertySignature>)>,
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
            daemon_socket: None,
            drm: None,
            kept,
            modes: None,
            last_query_note: None,
        }
    }

    pub fn snapshot_path(&self) -> PathBuf {
        self.runtime.join("snapshot.json")
    }

    /// One full look, published.
    pub fn reconcile(&mut self, reason: Reason) -> Snapshot {
        let platform = Platform::inspect(&self.roots, &Overrides::default());
        let boot_id = crate::roots::read_trimmed(self.roots.proc("sys/kernel/random/boot_id"))
            .unwrap_or_else(|| "unknown".into());
        let selected = platform.selected_output();
        let edid_bytes = selected
            .map(|output| std::fs::read(output.connector.sysfs.join("edid")).unwrap_or_default());
        let edid = edid_bytes.as_deref().map(EdidReport::of);

        // The kernel's mode list and the connector's properties: read again
        // only for a connector and EDID not read before, or after a deferral.
        let modes_for = format!(
            "{}|{}",
            selected.map(|o| o.connector.name.as_str()).unwrap_or("-"),
            edid.as_ref()
                .and_then(|report| report.sha256.as_ref())
                .map(|id| id.0.as_str())
                .unwrap_or("-")
        );
        let current_modes_for = modes_for.clone();
        let stale = !matches!(&self.modes, Some((key, KernelModes::Known { .. }, _)) if *key == modes_for);
        if stale {
            let mut properties = None;
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
            let modes = match result {
                Query::Done(read) => {
                    properties = Some(read.properties.clone());
                    kernel_modes(&read)
                }
                Query::Deferred { reason } => KernelModes::Deferred { reason },
            };
            if let KernelModes::Deferred { reason } = &modes
                && self.last_query_note.as_deref() != Some(reason)
            {
                eprintln!("mediabox-display-observer: {reason}");
                self.last_query_note = Some(reason.clone());
            }
            if matches!(modes, KernelModes::Known { .. }) {
                self.last_query_note = None;
            }
            self.modes = Some((modes_for, modes, properties));
        }
        let kernel_modes = self
            .modes
            .as_ref()
            .map(|(_, modes, _)| modes.clone())
            .unwrap_or(KernelModes::Deferred {
                reason: "not read yet".into(),
            });
        let properties = self.modes.as_ref().and_then(|(_, _, properties)| properties.clone());

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

        let material = Material {
            kms_device: platform.kms.as_ref().map(|node| {
                format!("{} ({})", node.name, node.parent.as_deref().unwrap_or("?"))
            }),
            connector: selected.map(|output| output.connector.name.clone()),
            connected: selected.map(|output| output.connector.connected),
            edid_sha256: edid.as_ref().and_then(|report| report.sha256.as_ref()).map(|id| id.0.clone()),
            transmitter: selected.and_then(|output| output.controller.clone()),
            topology: selected.map(|output| output.binding),
            audio: selected.and_then(|output| output.audio.as_ref()).map(|audio| audio.card_id.clone()),
            cec: selected
                .and_then(|output| output.cec.as_ref())
                .map(|cec| cec.device.display().to_string()),
            capability_fingerprint,
            kernel_modes_fingerprint: match &kernel_modes {
                KernelModes::Known { fingerprint, .. } => Some(fingerprint.clone()),
                KernelModes::Deferred { .. } if self.kept.modes_for == current_modes_for => {
                    self.kept.modes_fingerprint.clone()
                }
                KernelModes::Deferred { .. } => None,
            },
        };
        let digest = material.digest();
        if self.kept.boot_id != boot_id {
            self.kept = Kept {
                boot_id: boot_id.clone(),
                ..Kept::default()
            };
        }
        let mut changed_modes = false;
        if let KernelModes::Known { fingerprint, .. } = &kernel_modes
            && (self.kept.modes_for != current_modes_for
                || self.kept.modes_fingerprint.as_ref() != Some(fingerprint))
        {
            self.kept.modes_for = current_modes_for.clone();
            self.kept.modes_fingerprint = Some(fingerprint.clone());
            changed_modes = true;
        }
        let new_generation = self.kept.digest != digest;
        if new_generation {
            self.kept.seq += 1;
            self.kept.digest = digest.clone();
            eprintln!(
                "mediabox-display-observer: generation {}:{} ({reason:?}) connector={} connected={} edid={} transmitter={} ({:?}) cec={} modes={}",
                boot_id,
                self.kept.seq,
                material.connector.as_deref().unwrap_or("-"),
                material.connected.map(|c| c.to_string()).unwrap_or("-".into()),
                material.edid_sha256.as_deref().map(|s| &s[..16.min(s.len())]).unwrap_or("-"),
                material.transmitter.as_deref().unwrap_or("-"),
                material.topology,
                material.cec.as_deref().unwrap_or("-"),
                match &kernel_modes {
                    KernelModes::Known { count, .. } => count.to_string(),
                    KernelModes::Deferred { reason } => format!("deferred ({reason})"),
                }
            );
        }
        if new_generation || changed_modes {
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
        let drm_master = match platform.debugfs.read("clients") {
            Observed::Known(table) => match current_master(&table) {
                Some(master) => Observed::Known(master),
                None => Observed::Unknown("no client is master".into()),
            },
            Observed::Unknown(why) => Observed::Unknown(why),
        };

        let (boottime_ns, realtime_ms) = clocks();
        let mut snapshot = Snapshot {
            schema: SCHEMA,
            shadow: true,
            generation: Generation {
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
            audio_confidence: selected.map(|output| output.audio_confidence()),
            cec_confidence: selected.map(|output| output.cec_confidence()),
            current_mode,
            bus_format,
            debugfs: platform.debugfs.clone(),
            kernel_modes,
            drm_master,
            comparison: None,
            warnings: platform.warnings.clone(),
            material,
        };
        if let Some(socket) = &self.daemon_socket {
            snapshot.comparison = Some(compare(&snapshot, socket));
        }
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

fn kernel_modes(read: &drm_query::ConnectorQuery) -> KernelModes {
    let mut keys: Vec<String> = read
        .modes
        .iter()
        .map(|mode| mode.timing.key().to_string())
        .collect();
    let preferred = read
        .modes
        .iter()
        .find(|mode| mode.preferred)
        .map(|mode| mode.timing.key().to_string());
    keys.sort();
    keys.dedup();
    let text = format!("{}|{}", keys.join(","), preferred.as_deref().unwrap_or(""));
    KernelModes::Known {
        count: read.modes.len(),
        fingerprint: hex(&Sha256::digest(text.as_bytes())),
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

/// Ask the control plane what it believes, and say where it differs. The
/// control plane's belief is what the interface last reported to it.
fn compare(snapshot: &Snapshot, socket: &Path) -> Comparison {
    use std::io::{BufRead, BufReader, Write};
    let asked = (|| -> Option<serde_json::Value> {
        let stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
        stream.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
        stream.set_write_timeout(Some(Duration::from_millis(800))).ok()?;
        (&stream).write_all(b"{\"command\":\"output_status\"}\n").ok()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    })();
    let Some(answer) = asked else {
        return Comparison {
            daemon_reachable: false,
            daemon_connector: None,
            daemon_edid_sha256: None,
            daemon_source_profile: None,
            agrees: false,
            differences: vec!["the control plane did not answer".into()],
        };
    };
    let offer = answer.pointer("/result/offer");
    let text = |pointer: &str| {
        offer
            .and_then(|offer| offer.pointer(pointer))
            .and_then(|value| value.as_str())
            .map(str::to_string)
    };
    let daemon_connector = text("/connector");
    let daemon_edid_sha256 = text("/edid_sha256");
    let daemon_source_profile = text("/link/source_profile");
    let mut differences = Vec::new();
    let mut check = |what: &str, ours: Option<&str>, theirs: Option<&str>| {
        if let (Some(ours), Some(theirs)) = (ours, theirs)
            && ours != theirs
        {
            differences.push(format!("{what}: observer {ours}, control plane {theirs}"));
        }
    };
    check(
        "connector",
        snapshot.material.connector.as_deref(),
        daemon_connector.as_deref(),
    );
    check(
        "EDID SHA-256",
        snapshot.material.edid_sha256.as_deref(),
        daemon_edid_sha256.as_deref(),
    );
    // The profile the interface resolved with the connector's properties in
    // hand, against the observer's: compared by name, not by how verified.
    let name = |text: &str| text.split_whitespace().next().unwrap_or_default().to_string();
    check(
        "source profile",
        Some(name(&snapshot.source_profile)).as_deref(),
        daemon_source_profile.as_deref().map(name).as_deref(),
    );
    Comparison {
        daemon_reachable: true,
        agrees: differences.is_empty(),
        daemon_connector,
        daemon_edid_sha256,
        daemon_source_profile,
        differences,
    }
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
