//! Reading a connector's modes and properties off the display device without
//! ever taking the display.
//!
//! Opening a DRM primary node when nobody is master makes the opener master
//! (`drm_master_open`, drivers/gpu/drm/drm_auth.c). Gate 0 measured windows of
//! 1.4 to 5.0 seconds with no master while the display changed hands, and a
//! process that becomes master in one of them is a process the next owner's
//! `SET_MASTER` fails against. So an observer that wants the kernel's own mode
//! list or the connector's properties asks for them this way, and no other
//! (Gate 0, AM-2):
//!
//! 1. take the display transition lock, shared and without waiting -- a
//!    transition in progress is a reason to come back later, not to wait,
//!    and a shared hold is never mistaken for a transition;
//! 2. check, just before opening, that a DRM master exists (debugfs `clients`)
//!    -- and if none does, defer;
//! 3. open the primary node read-only, as a non-master client (a master
//!    exists, so the kernel does not hand this one the role);
//! 4. check again that this process is not master -- if a master vanished in
//!    the instant between (2) and (3), close at once and defer;
//! 5. read with `GETRESOURCES`, `GETCONNECTOR` asking for at least one mode (a
//!    zero count asks for a forced probe) and `GETPROPERTY`; set nothing;
//! 6. close, and let the lock go.
//!
//! Never `open`, become master, `DROP_MASTER`. And never a descriptor kept
//! between queries.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::source::{EnumProperty, PropertySignature};
use mediabox_core::ModeTiming;

/// One mode as `GETCONNECTOR` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelMode {
    pub timing: ModeTiming,
    pub preferred: bool,
}

/// What a query of one connector read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorQuery {
    pub connector: String,
    pub connected: bool,
    pub modes: Vec<KernelMode>,
    pub properties: PropertySignature,
}

/// The answer to a query: read, or not read and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum Query {
    Done(ConnectorQuery),
    Deferred { reason: String },
}

/// What the query needs from the machine. The production implementation is
/// [`System`]; the tests put a fake in its place and count what it was asked.
pub trait Access {
    /// Take the transition lock without waiting. `Ok(None)` when it is held
    /// by somebody else: a transition is running.
    fn lock_transition(&self) -> Result<Option<Box<dyn std::any::Any>>, String>;
    /// The display device's debugfs `clients` table.
    fn clients(&self) -> Result<String, String>;
    /// Open the primary node, read-only.
    fn open_primary(&self) -> Result<OwnedFd, String>;
    /// Read one connector on an open descriptor.
    fn read_connector(&self, fd: &OwnedFd, connector: &str) -> Result<ConnectorQuery, String>;
}

/// Is any client of this device master? `None` when the table cannot be read.
pub fn master_exists(clients: &str) -> Option<bool> {
    let mut rows = clients.lines();
    let header = rows.next()?;
    if !header.contains("master") {
        return None;
    }
    Some(rows.any(|row| row_is_master(row).is_some_and(|(_, master)| master)))
}

/// `(pid, master)` of one `clients` row: `command pid dev master a uid magic`.
/// The command name can contain spaces, so the columns are read from the end.
fn row_is_master(row: &str) -> Option<(u32, bool)> {
    let fields: Vec<&str> = row.split_whitespace().collect();
    if fields.len() < 7 {
        return None;
    }
    let n = fields.len();
    let pid = fields[n - 6].parse().ok()?;
    let master = match fields[n - 4] {
        "y" => true,
        "n" => false,
        _ => return None,
    };
    Some((pid, master))
}

/// Whether `pid` holds master on this device, by the same table.
pub fn is_master(clients: &str, pid: u32) -> bool {
    clients
        .lines()
        .skip(1)
        .filter_map(row_is_master)
        .any(|(row_pid, master)| row_pid == pid && master)
}

/// Read `connector` under the rules above.
pub fn query(access: &dyn Access, connector: &str) -> Query {
    let defer = |reason: String| Query::Deferred { reason };
    let _lock = match access.lock_transition() {
        Ok(Some(lock)) => lock,
        Ok(None) => return defer("DRM query deferred: a display transition is in progress".into()),
        Err(error) => return defer(format!("DRM query deferred: transition lock: {error}")),
    };
    match access.clients().map(|table| master_exists(&table)) {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return defer("DRM query deferred: no active master".into()),
        Ok(None) => return defer("DRM query deferred: the clients table is not readable as one".into()),
        Err(error) => return defer(format!("DRM query deferred: clients: {error}")),
    }
    let fd = match access.open_primary() {
        Ok(fd) => fd,
        Err(error) => return defer(format!("DRM query deferred: open: {error}")),
    };
    // The one race left: a master that went away between the check and the
    // open. Then this descriptor is master, and is closed before it does
    // anything at all.
    let me = std::process::id();
    match access.clients() {
        Ok(table) if !is_master(&table, me) => {}
        Ok(_) => {
            drop(fd);
            return defer("DRM query deferred: the master went away as the device was opened; closed at once".into());
        }
        Err(error) => {
            drop(fd);
            return defer(format!("DRM query deferred: clients after open: {error}"));
        }
    }
    let read = access.read_connector(&fd, connector);
    drop(fd);
    match read {
        Ok(result) => Query::Done(result),
        Err(error) => defer(format!("DRM query failed: {error}")),
    }
}

// ------------------------------------------------------------ the system

/// The machine itself.
pub struct System {
    pub transition_lock: PathBuf,
    pub clients: PathBuf,
    pub primary: PathBuf,
}

struct Flock(OwnedFd);
impl Drop for Flock {
    fn drop(&mut self) {
        // SAFETY: a descriptor this value owns; closing it (next) would
        // release the lock anyway, this only makes the order explicit.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn open_raw(path: &Path, flags: libc::c_int) -> Result<OwnedFd, String> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| format!("{}: not a path", path.display()))?;
    // SAFETY: a valid C string; the result is checked before use.
    let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error()));
    }
    // SAFETY: a descriptor just returned by open, owned from here on.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl Access for System {
    fn lock_transition(&self) -> Result<Option<Box<dyn std::any::Any>>, String> {
        // Read-only: the lock file is the control plane's; the observer does
        // not create it, and a missing one is reported, not made.
        let fd = open_raw(&self.transition_lock, libc::O_RDONLY)?;
        // Shared, not exclusive. A transition holds it exclusively, so this
        // still fails while one runs and keeps one from starting while the
        // node is open. But a shared hold is not a transition: whoever asks
        // "is somebody changing the display?" -- kodi.service's guard, when
        // Kodi ends by itself -- asks with a shared lock and gets it, and the
        // recovery this query happened to coincide with is not skipped.
        // SAFETY: a live descriptor owned above.
        let taken = unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0;
        Ok(taken.then(|| Box::new(Flock(fd)) as Box<dyn std::any::Any>))
    }

    fn clients(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.clients)
            .map_err(|error| format!("{}: {error}", self.clients.display()))
    }

    fn open_primary(&self) -> Result<OwnedFd, String> {
        open_raw(&self.primary, libc::O_RDONLY)
    }

    fn read_connector(&self, fd: &OwnedFd, connector: &str) -> Result<ConnectorQuery, String> {
        read_connector(fd.as_raw_fd(), connector)
    }
}

// ------------------------------------------------------------ the ioctls
//
// include/uapi/drm/drm.h and drm_mode.h. The layouts are fixed ABI; the sizes
// are asserted below so a mistake is a build failure, not a wild write.

#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetConnector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ModeInfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    type_: u32,
    name: [u8; 32],
}

#[repr(C)]
struct GetProperty {
    values_ptr: u64,
    enum_blob_ptr: u64,
    prop_id: u32,
    flags: u32,
    name: [u8; 32],
    count_values: u32,
    count_enum_blobs: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PropertyEnum {
    value: u64,
    name: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<CardRes>() == 64);
const _: () = assert!(std::mem::size_of::<GetConnector>() == 80);
const _: () = assert!(std::mem::size_of::<ModeInfo>() == 68);
const _: () = assert!(std::mem::size_of::<GetProperty>() == 64);
const _: () = assert!(std::mem::size_of::<PropertyEnum>() == 40);

/// `_IOWR('d', nr, size)` (asm-generic ioctl encoding, as on arm64 and x86).
const fn iowr(nr: u64, size: usize) -> libc::c_ulong {
    ((3u64 << 30) | ((size as u64) << 16) | ((b'd' as u64) << 8) | nr) as libc::c_ulong
}

const GETRESOURCES: libc::c_ulong = iowr(0xA0, std::mem::size_of::<CardRes>());
const GETCONNECTOR: libc::c_ulong = iowr(0xA7, std::mem::size_of::<GetConnector>());
const GETPROPERTY: libc::c_ulong = iowr(0xAA, std::mem::size_of::<GetProperty>());

const PROP_ENUM: u32 = 1 << 3;
const MODE_TYPE_PREFERRED: u32 = 1 << 3;
const CONNECTED: u32 = 1;

fn ioctl<T>(fd: i32, request: libc::c_ulong, arg: &mut T) -> Result<(), String> {
    loop {
        // SAFETY: `arg` is a live, correctly sized #[repr(C)] struct for this
        // request (sizes asserted above); every pointer inside it points at a
        // buffer at least as long as the count beside it.
        let result = unsafe { libc::ioctl(fd, request as _, arg as *mut T) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EINTR) | Some(libc::EAGAIN)) {
            continue;
        }
        return Err(error.to_string());
    }
}

/// `drm_connector_enum_list`, for the types a television is on.
fn connector_type_name(kind: u32) -> &'static str {
    match kind {
        10 => "DP",
        11 => "HDMI-A",
        12 => "HDMI-B",
        14 => "eDP",
        18 => "Writeback",
        _ => "Unknown",
    }
}

fn c_name(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn read_connector(fd: i32, wanted: &str) -> Result<ConnectorQuery, String> {
    let mut res = CardRes::default();
    ioctl(fd, GETRESOURCES, &mut res)?;
    let mut ids = vec![0u32; res.count_connectors as usize];
    let mut res2 = CardRes {
        connector_id_ptr: ids.as_mut_ptr() as u64,
        count_connectors: ids.len() as u32,
        ..Default::default()
    };
    ioctl(fd, GETRESOURCES, &mut res2)?;
    ids.truncate(res2.count_connectors.min(ids.len() as u32) as usize);

    for id in ids {
        // First pass: one mode's room, never zero -- zero asks for a probe.
        let mut one = [ModeInfo {
            clock: 0,
            hdisplay: 0,
            hsync_start: 0,
            hsync_end: 0,
            htotal: 0,
            hskew: 0,
            vdisplay: 0,
            vsync_start: 0,
            vsync_end: 0,
            vtotal: 0,
            vscan: 0,
            vrefresh: 0,
            flags: 0,
            type_: 0,
            name: [0; 32],
        }; 1];
        let mut head = GetConnector {
            connector_id: id,
            modes_ptr: one.as_mut_ptr() as u64,
            count_modes: 1,
            ..Default::default()
        };
        ioctl(fd, GETCONNECTOR, &mut head)?;
        let name = format!("{}-{}", connector_type_name(head.connector_type), head.connector_type_id);
        if name != wanted {
            continue;
        }
        // Second pass, sized from the first. The counts can only have grown
        // if the connector changed in between; then the kernel reports the
        // new count and copies nothing, and the read is retried once.
        for _ in 0..2 {
            let modes_len = (head.count_modes as usize).max(1);
            let mut modes = vec![one[0]; modes_len];
            let mut props = vec![0u32; head.count_props as usize];
            let mut values = vec![0u64; head.count_props as usize];
            let mut full = GetConnector {
                connector_id: id,
                modes_ptr: modes.as_mut_ptr() as u64,
                count_modes: modes_len as u32,
                props_ptr: props.as_mut_ptr() as u64,
                prop_values_ptr: values.as_mut_ptr() as u64,
                count_props: props.len() as u32,
                ..Default::default()
            };
            ioctl(fd, GETCONNECTOR, &mut full)?;
            if full.count_modes as usize > modes_len || full.count_props as usize > props.len() {
                head = full;
                continue;
            }
            modes.truncate(full.count_modes as usize);
            props.truncate(full.count_props as usize);
            return Ok(ConnectorQuery {
                connector: name,
                connected: full.connection == CONNECTED,
                modes: modes
                    .iter()
                    .map(|mode| KernelMode {
                        timing: ModeTiming::new(
                            mode.clock,
                            mode.hdisplay,
                            mode.hsync_start,
                            mode.hsync_end,
                            mode.htotal,
                            mode.hskew,
                            mode.vdisplay,
                            mode.vsync_start,
                            mode.vsync_end,
                            mode.vtotal,
                            mode.vscan,
                            mode.flags,
                        ),
                        preferred: mode.type_ & MODE_TYPE_PREFERRED != 0,
                    })
                    .collect(),
                properties: property_signature(fd, &props)?,
            });
        }
        return Err(format!("{wanted}: its mode list kept changing while it was read"));
    }
    Err(format!("{wanted}: no such connector on this device"))
}

fn property_signature(fd: i32, props: &[u32]) -> Result<PropertySignature, String> {
    let mut signature = PropertySignature::default();
    for &prop_id in props {
        let mut head = GetProperty {
            values_ptr: 0,
            enum_blob_ptr: 0,
            prop_id,
            flags: 0,
            name: [0; 32],
            count_values: 0,
            count_enum_blobs: 0,
        };
        ioctl(fd, GETPROPERTY, &mut head)?;
        let name = c_name(&head.name);
        let wanted = matches!(
            name.as_str(),
            "color_format" | "color_depth" | "Colorspace" | "HDR_OUTPUT_METADATA"
        );
        if !wanted {
            continue;
        }
        if name == "HDR_OUTPUT_METADATA" {
            signature.hdr_output_metadata = true;
            continue;
        }
        let mut values = EnumProperty {
            present: true,
            values: Vec::new(),
        };
        if head.flags & PROP_ENUM != 0 && head.count_enum_blobs > 0 {
            let mut enums = vec![
                PropertyEnum {
                    value: 0,
                    name: [0; 32]
                };
                head.count_enum_blobs as usize
            ];
            let mut raw = vec![0u64; head.count_values as usize];
            let mut full = GetProperty {
                values_ptr: raw.as_mut_ptr() as u64,
                enum_blob_ptr: enums.as_mut_ptr() as u64,
                prop_id,
                flags: 0,
                name: [0; 32],
                count_values: raw.len() as u32,
                count_enum_blobs: enums.len() as u32,
            };
            ioctl(fd, GETPROPERTY, &mut full)?;
            enums.truncate((full.count_enum_blobs as usize).min(enums.len()));
            values.values = enums
                .iter()
                .map(|entry| (c_name(&entry.name), entry.value))
                .collect();
        }
        match name.as_str() {
            "color_format" => signature.color_format = values,
            "color_depth" => signature.color_depth = values,
            _ => signature.colorspace = values,
        }
    }
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const WITH_MASTER: &str = "\
             command   pid dev master a   uid      magic
         mediabox-tv 37480 128   n    y     0          0
         mediabox-tv 37480   0   y    y     0          0
";
    const MASTERLESS: &str = "\
             command   pid dev master a   uid      magic
         mediabox-tv 37480 128   n    y     0          0
";

    #[test]
    fn the_clients_table_says_whether_there_is_a_master() {
        assert_eq!(master_exists(WITH_MASTER), Some(true));
        assert_eq!(master_exists(MASTERLESS), Some(false));
        assert_eq!(master_exists("garbage"), None);
        assert!(is_master(WITH_MASTER, 37480));
        assert!(!is_master(WITH_MASTER, 1));
        // A command name with a space in it.
        let spaced = "command pid dev master a uid magic\n  Xwayland helper  812   0   y    y     0   0\n";
        assert!(is_master(spaced, 812));
    }

    struct Fake {
        lock_free: bool,
        tables: RefCell<Vec<String>>,
        opened: Cell<u32>,
    }
    struct NoLock;
    impl Access for Fake {
        fn lock_transition(&self) -> Result<Option<Box<dyn std::any::Any>>, String> {
            Ok(self.lock_free.then(|| Box::new(NoLock) as Box<dyn std::any::Any>))
        }
        fn clients(&self) -> Result<String, String> {
            let mut tables = self.tables.borrow_mut();
            Ok(if tables.len() > 1 { tables.remove(0) } else { tables[0].clone() })
        }
        fn open_primary(&self) -> Result<OwnedFd, String> {
            self.opened.set(self.opened.get() + 1);
            // Any descriptor will do: nothing is read through it here.
            std::fs::File::open("/dev/null")
                .map(OwnedFd::from)
                .map_err(|error| error.to_string())
        }
        fn read_connector(&self, _: &OwnedFd, connector: &str) -> Result<ConnectorQuery, String> {
            Ok(ConnectorQuery {
                connector: connector.into(),
                connected: true,
                modes: Vec::new(),
                properties: PropertySignature::default(),
            })
        }
    }

    fn fake(lock_free: bool, tables: &[&str]) -> Fake {
        Fake {
            lock_free,
            tables: RefCell::new(tables.iter().map(|t| t.to_string()).collect()),
            opened: Cell::new(0),
        }
    }

    #[test]
    fn masterless_the_primary_node_is_not_opened() {
        let access = fake(true, &[MASTERLESS]);
        let result = query(&access, "HDMI-A-2");
        assert_eq!(
            result,
            Query::Deferred {
                reason: "DRM query deferred: no active master".into()
            }
        );
        assert_eq!(access.opened.get(), 0, "never opened without a master");
    }

    #[test]
    fn during_a_transition_the_primary_node_is_not_opened() {
        let access = fake(false, &[WITH_MASTER]);
        assert!(matches!(query(&access, "HDMI-A-2"), Query::Deferred { reason } if reason.contains("transition")));
        assert_eq!(access.opened.get(), 0);
    }

    #[test]
    fn with_a_master_the_connector_is_read_and_the_node_closed() {
        let access = fake(true, &[WITH_MASTER]);
        assert!(matches!(query(&access, "HDMI-A-2"), Query::Done(result) if result.connector == "HDMI-A-2"));
        assert_eq!(access.opened.get(), 1);
    }

    #[test]
    fn a_master_that_vanished_as_the_node_opened_makes_this_process_close_at_once() {
        let me = std::process::id();
        let became = format!(
            "command pid dev master a uid magic\n  observer {me}   0   y    y     0   0\n"
        );
        let access = fake(true, &[WITH_MASTER, &became]);
        assert!(matches!(query(&access, "HDMI-A-2"), Query::Deferred { reason } if reason.contains("closed at once")));
    }
}
