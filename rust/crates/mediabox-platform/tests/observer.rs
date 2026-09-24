//! The observer, on a captured board: generations, the offer it publishes, a
//! hotplug whose uevent never arrives, a sink replaced without a disconnect,
//! a display with no master, and restarts.

mod common;

use std::fs;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use common::{Board, edid_with_address};
use mediabox_core::{ModeTiming, mode_flags};
use mediabox_platform::drm_query::{Access, ConnectorQuery, KernelMode};
use mediabox_platform::observer::{
    self, KernelModes, Observer, Reason, Snapshot, Wake, is_display_uevent,
};
use mediabox_platform::source::PropertySignature;
use mediabox_platform::{Confidence, Platform, Roots};

const WITH_MASTER: &str = "\
             command   pid dev master a   uid      magic
         mediabox-tv 37480 128   n    y     0          0
         mediabox-tv 37480   0   y    y     0          0
";
const MASTERLESS: &str = "\
             command   pid dev master a   uid      magic
         mediabox-tv 37480 128   n    y     0          0
";

/// The Plus as it stands: HDMI-A-2 connected at 3.0.0.0, HDMI-A-1 empty.
fn plus() -> Board {
    let board = Board::new();
    board.platform_device("display-subsystem", "rockchip-drm", None);
    board.drm_card("card0", "display-subsystem");
    board.drm_dev("card0", "226:0");
    board.render_node("renderD128", "display-subsystem");
    board.connector("card0", "display-subsystem", "HDMI-A-1", false, &[], &[]);
    board.connector(
        "card0",
        "display-subsystem",
        "HDMI-A-2",
        true,
        &["3840x2160"],
        &edid_with_address(*b"SNY", 0xF903, 0x0101_0101, "TV", 0x3000),
    );
    board.transmitter("fde80000.hdmi", "hdmi@fde80000", 0x100, Some(false));
    board.transmitter("fdea0000.hdmi", "hdmi@fdea0000", 0x101, Some(true));
    board.cec("fde80000.hdmi", "cec0");
    board.cec("fdea0000.hdmi", "cec1");
    board.cec_status("cec0", "f.f.f.f");
    board.cec_status("cec1", "3.0.0.0");
    board.dri_debugfs(0, "rockchip dev=display-subsystem unique=display-subsystem", Some(
        "Video Port0: ACTIVE\n    Connector:HDMI-A-2\tEncoder: TMDS-235\n\tbus_format[100a]: RGB888_1X24\n    Display mode: 3840x2160p60\n\tdclk[594000 kHz] real_dclk[594000 kHz]\n",
    ));
    clients(&board, WITH_MASTER);
    fs::create_dir_all(board.root().join("proc/sys/kernel/random")).unwrap();
    fs::write(board.root().join("proc/sys/kernel/random/boot_id"), "b0071d\n").unwrap();
    board
}

fn clients(board: &Board, table: &str) {
    fs::write(board.root().join("sys/kernel/debug/dri/0/clients"), table).unwrap();
}

fn unplug(board: &Board) {
    let status = board
        .root()
        .join("sys/devices/platform/display-subsystem/drm/card0/card0-HDMI-A-2/status");
    fs::write(status, "disconnected\n").unwrap();
}

/// An `Access` that reads the fixture's clients table and counts the times
/// the primary node was opened. It never opens anything real.
struct Counted {
    board_clients: std::path::PathBuf,
    opened: Arc<AtomicU32>,
}

struct Held;

impl Access for Counted {
    fn lock_transition(&self) -> Result<Option<Box<dyn std::any::Any>>, String> {
        Ok(Some(Box::new(Held)))
    }
    fn clients(&self) -> Result<String, String> {
        fs::read_to_string(&self.board_clients).map_err(|error| error.to_string())
    }
    fn open_primary(&self) -> Result<OwnedFd, String> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        fs::File::open("/dev/null").map(OwnedFd::from).map_err(|e| e.to_string())
    }
    fn read_connector(&self, _: &OwnedFd, connector: &str) -> Result<ConnectorQuery, String> {
        let uhd = |clock| {
            ModeTiming::new(clock, 3840, 4016, 4104, 4400, 0, 2160, 2168, 2178, 2250, 0,
                mode_flags::PHSYNC | mode_flags::PVSYNC)
        };
        Ok(ConnectorQuery {
            connector: connector.into(),
            connected: true,
            modes: vec![
                KernelMode { timing: uhd(594_000), preferred: true },
                KernelMode { timing: uhd(297_000), preferred: false },
            ],
            properties: PropertySignature::default(),
        })
    }
}

fn observer(board: &Board, opened: &Arc<AtomicU32>) -> Observer {
    let mut observer = Observer::new(Roots::under(board.root()), board.root().join("run/observer"));
    let path = board.root().join("sys/kernel/debug/dri/0/clients");
    let opened = Arc::clone(opened);
    observer.drm = Some(Box::new(move |_: &Platform| -> Box<dyn Access> {
        Box::new(Counted {
            board_clients: path.clone(),
            opened: Arc::clone(&opened),
        })
    }));
    observer
}

fn published(board: &Board) -> Snapshot {
    let text = fs::read_to_string(board.root().join("run/observer/snapshot.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn the_snapshot_says_what_the_display_is() {
    let board = plus();
    let opened = Arc::new(AtomicU32::new(0));
    let snapshot = observer(&board, &opened).reconcile(Reason::Startup);
    assert_eq!(snapshot.generation.boot_id, "b0071d");
    assert_eq!(snapshot.generation.seq, 1);
    let m = &snapshot.material;
    assert_eq!(m.kms_device.as_deref(), Some("card0 (display-subsystem)"));
    assert_eq!(m.connector.as_deref(), Some("HDMI-A-2"));
    assert_eq!(m.connected, Some(true));
    assert_eq!(m.edid_sha256.as_ref().map(String::len), Some(64));
    assert_eq!(m.transmitter.as_deref(), Some("fdea0000.hdmi"));
    assert_eq!(m.topology, Some(Confidence::Measured));
    assert!(m.cec.as_deref().unwrap().ends_with("cec1"));
    assert!(m.capability_fingerprint.is_some());
    assert_eq!(snapshot.physical_address.as_deref(), Some("3.0.0.0"));
    assert_eq!(snapshot.primary_minor, Some(0));
    assert_eq!(snapshot.current_mode.clone().known().as_deref(), Some("3840x2160p60"));
    assert_eq!(snapshot.bus_format.clone().known().as_deref(), Some("RGB888_1X24"));
    assert_eq!(snapshot.drm_master.clone().known().unwrap().command, "mediabox-tv");
    assert!(matches!(&snapshot.kernel_modes, KernelModes::Known { count: 2, .. }));
    // Where audio and CEC go, from the firm binding: the transmitter's own.
    assert!(snapshot.cec.device.as_deref().unwrap().ends_with("cec1"));
    // And what can be sent to this sink, from the kernel's list and the EDID.
    let offer = snapshot.offer.as_ref().expect("an offer once the modes are read");
    assert_eq!(offer.connector, "HDMI-A-2");
    assert_eq!(Some(&offer.edid_sha256), m.edid_sha256.as_ref());
    assert_eq!(offer.modes().count(), 2);
    assert_eq!(snapshot.identity().unwrap().connector, "HDMI-A-2");
    assert_eq!(published(&board), snapshot, "what is published is what was seen");
    assert_eq!(
        observer::published_generation(&board.root().join("run/observer")),
        Some(snapshot.generation.clone())
    );
}

#[test]
fn the_same_display_is_the_same_generation_across_passes_and_restarts() {
    let board = plus();
    let opened = Arc::new(AtomicU32::new(0));
    let mut first = observer(&board, &opened);
    let one = first.reconcile(Reason::Startup);
    let two = first.reconcile(Reason::Periodic);
    let three = first.reconcile(Reason::Uevent);
    assert_eq!(one.generation, two.generation);
    assert_eq!(two.generation, three.generation, "an event is not a generation");
    // Normal passes read sysfs and debugfs only: the primary node was opened
    // once, for the first look at this display, and not again.
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    drop(first);
    let again = observer(&board, &opened).reconcile(Reason::Startup);
    assert_eq!(again.generation, one.generation, "a restart continues the generation");
}

#[test]
fn a_hotplug_whose_uevent_is_lost_is_caught_by_the_period() {
    let board = plus();
    let opened = Arc::new(AtomicU32::new(0));
    let mut watched = observer(&board, &opened);
    let mut script = vec![Wake::Timeout, Wake::Timeout, Wake::Stop].into_iter();
    let mut passes = 0;
    observer::run(&mut watched, |_| {
        passes += 1;
        if passes == 1 {
            // The cable comes out; the kernel's uevent never reaches us.
            unplug(&board);
        }
        script.next().unwrap()
    });
    let seen = published(&board);
    assert_eq!(seen.reason, Reason::Periodic);
    assert_eq!(seen.material.connected, None, "nothing selected once unplugged");
    assert_eq!(seen.material.connector, None);
    assert_eq!(seen.generation.seq, 2, "one change, one generation");
}

#[test]
fn a_uevent_is_a_hint_to_look_now() {
    let board = plus();
    let opened = Arc::new(AtomicU32::new(0));
    let mut watched = observer(&board, &opened);
    let mut script = vec![Wake::Hint, Wake::Stop].into_iter();
    let mut first = true;
    observer::run(&mut watched, |_| {
        if std::mem::take(&mut first) {
            unplug(&board);
        }
        script.next().unwrap()
    });
    let seen = published(&board);
    assert_eq!(seen.reason, Reason::Uevent);
    assert_eq!(seen.generation.seq, 2);
    assert!(is_display_uevent(b"change@/devices/platform/display-subsystem/drm/card0\0ACTION=change\0SUBSYSTEM=drm\0HOTPLUG=1\0"));
    assert!(!is_display_uevent(b"add@/devices/virtual/net/tun0\0SUBSYSTEM=net\0"));
}

#[test]
fn with_no_master_the_primary_node_is_not_opened_and_the_modes_are_deferred() {
    let board = plus();
    clients(&board, MASTERLESS);
    let opened = Arc::new(AtomicU32::new(0));
    let mut watched = observer(&board, &opened);
    let snapshot = watched.reconcile(Reason::Periodic);
    assert_eq!(
        snapshot.kernel_modes,
        KernelModes::Deferred {
            reason: "DRM query deferred: no active master".into()
        }
    );
    assert_eq!(opened.load(Ordering::SeqCst), 0, "no open in a masterless window");
    assert!(snapshot.drm_master.clone().known().is_none());
    // The rest of the snapshot does not need the device and is all there.
    assert_eq!(snapshot.material.connector.as_deref(), Some("HDMI-A-2"));

    // An owner takes the display: the deferred read is tried again, once.
    clients(&board, WITH_MASTER);
    let later = watched.reconcile(Reason::Periodic);
    assert!(matches!(later.kernel_modes, KernelModes::Known { count: 2, .. }));
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    // The mode list is material: it moved the generation.
    assert_eq!(later.generation.seq, snapshot.generation.seq + 1);
    watched.reconcile(Reason::Periodic);
    assert_eq!(opened.load(Ordering::SeqCst), 1);
}

#[test]
fn a_different_edid_on_the_same_connector_is_a_new_generation() {
    let board = plus();
    let opened = Arc::new(AtomicU32::new(0));
    let mut watched = observer(&board, &opened);
    let before = watched.reconcile(Reason::Startup);
    board.connector(
        "card0",
        "display-subsystem",
        "HDMI-A-2",
        true,
        &["3840x2160"],
        &edid_with_address(*b"SNY", 0xF903, 0x0202_0202, "TV", 0x3000),
    );
    // No uevent at all: connected before, connected after, a different sink.
    let after = watched.reconcile(Reason::Periodic);
    assert_eq!(after.material.connected, Some(true));
    assert_ne!(before.material.edid_sha256, after.material.edid_sha256);
    assert_eq!(after.generation.seq, before.generation.seq + 1);
    assert_ne!(before.identity(), after.identity(), "connected to connected is still a new sink");
    // A new display is looked at afresh, once, and its offer is its own.
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    assert_eq!(after.offer.unwrap().edid_sha256, after.material.edid_sha256.unwrap());
}

#[test]
fn a_restart_keeps_what_was_read_and_does_not_open_the_device_again() {
    let board = plus();
    let opened = Arc::new(AtomicU32::new(0));
    let first = observer(&board, &opened).reconcile(Reason::Startup);
    assert!(matches!(first.kernel_modes, KernelModes::Known { .. }));
    // Restarted inside a masterless window: nothing about the display
    // changed, so nothing is read again -- and nothing is lost.
    clients(&board, MASTERLESS);
    let restarted = observer(&board, &opened).reconcile(Reason::Startup);
    assert_eq!(restarted.kernel_modes, first.kernel_modes);
    assert_eq!(restarted.generation, first.generation);
    assert_eq!(restarted.offer, first.offer, "the offer survives the restart");
    assert_eq!(opened.load(Ordering::SeqCst), 1, "not opened again");
}

#[test]
fn what_was_read_for_one_sink_is_not_another_sinks() {
    let board = plus();
    let opened = Arc::new(AtomicU32::new(0));
    let mut watched = observer(&board, &opened);
    watched.reconcile(Reason::Startup);
    // A different sink arrives while nobody is master: its mode list cannot
    // be read, and the old one is not passed off as its.
    clients(&board, MASTERLESS);
    board.connector(
        "card0",
        "display-subsystem",
        "HDMI-A-2",
        true,
        &["1920x1080"],
        &edid_with_address(*b"HWP", 0x3412, 0x0303_0303, "Monitor", 0x3000),
    );
    let seen = watched.reconcile(Reason::Uevent);
    assert!(matches!(seen.kernel_modes, KernelModes::Deferred { .. }));
    assert!(seen.offer.is_none(), "no offer until this sink's modes are read");
    assert_eq!(seen.material.kernel_modes_fingerprint, None);
}

#[test]
fn a_kept_read_from_before_the_list_was_kept_is_read_again() {
    let board = plus();
    let opened = Arc::new(AtomicU32::new(0));
    let first = observer(&board, &opened).reconcile(Reason::Startup);
    // What an observer of the previous version left: the connector and EDID
    // it read for and a fingerprint, but no list.
    let path = board.root().join("run/observer/generation.json");
    let mut kept: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    kept.as_object_mut().unwrap().remove("modes");
    kept.as_object_mut().unwrap().remove("properties");
    fs::write(&path, kept.to_string()).unwrap();
    let upgraded = observer(&board, &opened).reconcile(Reason::Startup);
    assert_eq!(opened.load(Ordering::SeqCst), 2, "read again, once");
    assert!(matches!(upgraded.kernel_modes, KernelModes::Known { count: 2, .. }));
    assert_eq!(upgraded.offer, first.offer);
}
