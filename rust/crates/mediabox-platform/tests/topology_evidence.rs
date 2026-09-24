//! Connectors tied to transmitters by what can be measured, and the display
//! device's debugfs found rather than assumed -- on boards built to be the
//! awkward cases: two DRM devices with the display not at minor 0, two HDMI
//! transmitters with two live CEC adapters, and two sinks that cannot be told
//! apart.

mod common;

use common::{Board, edid_with_address};
use mediabox_platform::debugfs::Debugfs;
use mediabox_platform::{Confidence, Devices, Overrides, Platform, Roots};

const SUMMARY_HDMI2: &str = "\
Video Port0: ACTIVE
    Connector:HDMI-A-2\tEncoder: TMDS-235
\tbus_format[100a]: RGB888_1X24
    Display mode: 3840x2160p60
\tdclk[594000 kHz] real_dclk[594000 kHz] aclk[750000 kHz] type[48] flag[5]
Video Port1: DISABLED
";

/// A decoy: what `dri/0` would say if something else lived there.
const SUMMARY_DECOY: &str = "\
Video Port0: ACTIVE
    Connector:HDMI-A-2\tEncoder: TMDS-235
\tbus_format[2025]: UYYVYY8_0_5X24
";

/// Two HDMI transmitters, each with its CEC adapter and PHY; the NPU
/// registered first, so the display device is `card1`, minor 1.
fn two_hdmi(sinks: [Option<u16>; 2]) -> Board {
    let board = Board::new();
    board.platform_device("display-subsystem", "rockchip-drm", None);
    board.platform_device("fdab0000.npu", "RKNPU", None);
    board.drm_card("card0", "fdab0000.npu");
    board.drm_card("card1", "display-subsystem");
    board.drm_dev("card0", "226:0");
    board.drm_dev("card1", "226:1");
    board.render_node("renderD128", "fdab0000.npu");
    board.render_node("renderD129", "display-subsystem");
    for (index, (name, address)) in [("HDMI-A-1", sinks[0]), ("HDMI-A-2", sinks[1])]
        .into_iter()
        .enumerate()
    {
        let edid = address
            .map(|pa| edid_with_address(*b"AAA", 0x0100 + index as u16, 7, name, pa))
            .unwrap_or_default();
        let modes: &[&str] = if address.is_some() { &["3840x2160"] } else { &[] };
        board.connector("card1", "display-subsystem", name, address.is_some(), modes, &edid);
    }
    board.transmitter("fde80000.hdmi", "hdmi@fde80000", 0x100, Some(sinks[0].is_some()));
    board.transmitter("fdea0000.hdmi", "hdmi@fdea0000", 0x101, Some(sinks[1].is_some()));
    board.cec("fde80000.hdmi", "cec0");
    board.cec("fdea0000.hdmi", "cec1");
    board.sound_card(0, "rockchiphdmi0", "hdmi0-sound", Some(0x100));
    board.sound_card(1, "rockchiphdmi1", "hdmi1-sound", Some(0x101));
    board
}

fn pa(address: Option<u16>) -> String {
    match address {
        Some(a) => format!("{:x}.{:x}.{:x}.{:x}", a >> 12, (a >> 8) & 0xF, (a >> 4) & 0xF, a & 0xF),
        None => "f.f.f.f".into(),
    }
}

fn inspect(board: &Board) -> Platform {
    Platform::inspect(&Roots::under(board.root()), &Overrides::default())
}

fn output<'a>(platform: &'a Platform, name: &str) -> &'a mediabox_platform::Output {
    platform
        .outputs
        .iter()
        .find(|output| output.connector.name == name)
        .unwrap_or_else(|| panic!("{name}"))
}

// ------------------------------------------------------------ debugfs

#[test]
fn debugfs_is_the_display_devices_minor_not_dri_0() {
    let board = two_hdmi([None, Some(0x3000)]);
    board.dri_debugfs(0, "rknpu dev=fdab0000.npu unique=fdab0000.npu", Some(SUMMARY_DECOY));
    board.dri_debugfs(1, "rockchip dev=display-subsystem unique=display-subsystem", Some(SUMMARY_HDMI2));
    let platform = inspect(&board);
    assert_eq!(platform.kms.as_ref().unwrap().name, "card1");
    match &platform.debugfs {
        Debugfs::Resolved { dir, minor } => {
            assert_eq!(*minor, 1);
            assert!(dir.ends_with("kernel/debug/dri/1"), "{}", dir.display());
        }
        other => panic!("{other:?}"),
    }
    let summary = platform.debugfs.read("summary").known().unwrap();
    assert!(summary.contains("RGB888_1X24"), "the display's summary, not dri/0's");
}

#[test]
fn a_debugfs_directory_that_names_another_device_is_not_used() {
    let board = two_hdmi([None, Some(0x3000)]);
    board.dri_debugfs(1, "rknpu dev=fdab0000.npu unique=fdab0000.npu", Some(SUMMARY_DECOY));
    let platform = inspect(&board);
    assert!(matches!(&platform.debugfs, Debugfs::Unknown { reason } if reason.contains("not display-subsystem")));
    assert!(platform.debugfs.read("summary").known().is_none());
}

#[test]
fn no_debugfs_is_unknown_and_nothing_else_breaks() {
    let board = two_hdmi([None, Some(0x3000)]);
    let platform = inspect(&board);
    assert!(matches!(platform.debugfs, Debugfs::Unknown { .. }));
    // Discovery still answers, from the cable alone.
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.connector.name, "HDMI-A-2");
    assert_eq!(selected.controller.as_deref(), Some("fdea0000.hdmi"));
    assert_eq!(selected.binding, Confidence::Measured);
}

// ------------------------------------------------------------ CEC

#[test]
fn the_plus_as_it_is_selected_hdmi_a_2_is_fdea0000_and_cec1() {
    // HDMI-A-2 connected, its sink at 3.0.0.0; the other socket empty.
    let board = two_hdmi([None, Some(0x3000)]);
    board.cec_status("cec0", "f.f.f.f");
    board.cec_status("cec1", "3.0.0.0");
    board.dri_debugfs(1, "rockchip dev=display-subsystem unique=display-subsystem", Some(SUMMARY_HDMI2));
    board.phy("fde80000.hdmi", "fed60000.hdmiphy", "clk_hdmiphy_pixel0", 0, false);
    board.phy("fdea0000.hdmi", "fed70000.hdmiphy", "clk_hdmiphy_pixel1", 594_000_000, true);
    let platform = inspect(&board);
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.connector.name, "HDMI-A-2");
    assert_eq!(selected.connector.physical_address, Some(0x3000));
    assert_eq!(selected.controller.as_deref(), Some("fdea0000.hdmi"));
    assert_eq!(selected.binding, Confidence::Measured);
    assert_eq!(selected.cec.as_ref().unwrap().name, "cec1");
    assert_eq!(selected.cec_confidence(), Confidence::Measured);
    assert!(
        selected.evidence.iter().any(|line| line.contains("3.0.0.0")),
        "{:?}",
        selected.evidence
    );
    assert!(
        selected.evidence.iter().any(|line| line.contains("clk_hdmiphy_pixel1") && line.contains("dclk")),
        "{:?}",
        selected.evidence
    );
    assert_eq!(
        Devices::from(&platform).cec.unwrap().file_name().unwrap(),
        "cec1"
    );
    // The empty socket: order-derived, and its adapter is not live.
    assert_eq!(output(&platform, "HDMI-A-1").binding, Confidence::Derived);
}

#[test]
fn two_live_adapters_the_selected_output_is_the_second_transmitters() {
    // Two televisions, on two sockets, at two different inputs.
    let board = two_hdmi([Some(0x1000), Some(0x3000)]);
    board.cec_status("cec0", "1.0.0.0");
    board.cec_status("cec1", "3.0.0.0");
    board.remember_output("HDMI-A-2");
    let platform = inspect(&board);
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.connector.name, "HDMI-A-2");
    assert_eq!(selected.binding, Confidence::Measured);
    assert_eq!(selected.cec.as_ref().unwrap().name, "cec1");
    assert_eq!(Devices::from(&platform).cec.unwrap().file_name().unwrap(), "cec1");
    // And the other one is the first transmitter's, by the same measurement.
    let first = output(&platform, "HDMI-A-1");
    assert_eq!(first.binding, Confidence::Measured);
    assert_eq!(first.cec.as_ref().unwrap().name, "cec0");
}

#[test]
fn the_physical_address_outranks_connector_order() {
    // The sink on HDMI-A-1 declares 3.0.0.0 and the adapter holding 3.0.0.0
    // is the second transmitter's: order is wrong on this board, the
    // measurement is not.
    let board = two_hdmi([Some(0x3000), Some(0x1000)]);
    board.cec_status("cec0", "1.0.0.0");
    board.cec_status("cec1", "3.0.0.0");
    let platform = inspect(&board);
    let first = output(&platform, "HDMI-A-1");
    assert_eq!(first.controller.as_deref(), Some("fdea0000.hdmi"));
    assert_eq!(first.binding, Confidence::Measured);
    assert!(first.evidence.iter().any(|line| line.contains("connector order says")));
}

#[test]
fn two_live_adapters_that_cannot_be_told_apart_are_ambiguous_and_cec_is_withheld() {
    // Two televisions, both on their input 1: both adapters hold 1.0.0.0.
    let board = two_hdmi([Some(0x1000), Some(0x1000)]);
    board.cec_status("cec0", "1.0.0.0");
    board.cec_status("cec1", "1.0.0.0");
    board.remember_output("HDMI-A-2");
    let platform = inspect(&board);
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.connector.name, "HDMI-A-2");
    assert_eq!(selected.binding, Confidence::Ambiguous);
    assert!(!selected.cec_confidence().actionable());
    // No adapter is offered for sending, rather than the first live one.
    assert_eq!(Devices::from(&platform).cec, None);
    assert!(
        platform.warnings.iter().any(|warning| warning.contains("more than one CEC adapter")),
        "{:?}",
        platform.warnings
    );
}

#[test]
fn an_ambiguous_topology_sends_neither_sound_nor_cec_to_the_likely_transmitter() {
    // Both sinks connected, nothing tells the transmitters apart. The
    // connector-order candidate for HDMI-A-2 is fdea0000.hdmi and its card is
    // rockchiphdmi1 -- which is exactly the guess that must not be acted on.
    let board = two_hdmi([Some(0x1000), Some(0x1000)]);
    board.cec_status("cec0", "1.0.0.0");
    board.cec_status("cec1", "1.0.0.0");
    board.remember_output("HDMI-A-2");
    let platform = inspect(&board);
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.binding, Confidence::Ambiguous);
    assert!(selected.audio.is_some(), "the candidate's card is known, for the report");
    let audio = selected.audio_route().unwrap_err();
    let cec = selected.cec_route().unwrap_err();
    assert!(audio.contains("belirsiz") && cec.contains("belirsiz"), "{audio} / {cec}");
    let devices = Devices::from(&platform);
    assert_eq!(devices.alsa_card_id, None, "no card rather than the likely one");
    assert_eq!(devices.alsa_driver, None);
    assert_eq!(devices.cec, None);
}

#[test]
fn a_sinks_adapter_losing_its_address_for_a_moment_does_not_unbind_it() {
    // Mid-handover: nobody drives the panel, the set drops CEC for a moment.
    // The cable is still in the one transmitter and nothing says otherwise.
    let board = two_hdmi([None, Some(0x3000)]);
    board.cec_status("cec0", "f.f.f.f");
    board.cec_status("cec1", "f.f.f.f");
    let platform = inspect(&board);
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.controller.as_deref(), Some("fdea0000.hdmi"));
    assert_eq!(selected.binding, Confidence::Measured, "{:?}", selected.evidence);
    assert_eq!(selected.audio_route().unwrap().card_id, "rockchiphdmi1");
}

#[test]
fn a_stale_no_cable_does_not_outweigh_the_sinks_own_address() {
    // After a forced `off` and `detect` the vendor driver leaves extcon at
    // HDMI=0 with the sink plugged in; its adapter still holds the sink's
    // address and its PHY still runs at the connector's clock.
    let board = two_hdmi([None, Some(0x3000)]);
    board.transmitter("fdea0000.hdmi", "hdmi@fdea0000", 0x101, Some(false));
    board.cec_status("cec0", "f.f.f.f");
    board.cec_status("cec1", "3.0.0.0");
    let platform = inspect(&board);
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.controller.as_deref(), Some("fdea0000.hdmi"));
    assert_eq!(selected.binding, Confidence::Measured, "{:?}", selected.evidence);
    assert!(selected.evidence.iter().any(|line| line.contains("extcon says no cable")));
    // And with nothing live at all, "no cable" everywhere is still not a guess.
    board.cec_status("cec1", "f.f.f.f");
    let platform = inspect(&board);
    assert_eq!(platform.selected_output().unwrap().binding, Confidence::Ambiguous);
}

#[test]
fn a_firm_binding_routes_sound_and_cec_to_the_same_transmitter() {
    let board = two_hdmi([None, Some(0x3000)]);
    board.cec_status("cec0", "f.f.f.f");
    board.cec_status("cec1", "3.0.0.0");
    let platform = inspect(&board);
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.controller.as_deref(), Some("fdea0000.hdmi"));
    assert_eq!(selected.audio_route().unwrap().card_id, "rockchiphdmi1");
    assert!(selected.cec_route().unwrap().device.ends_with("cec1"));
    let devices = Devices::from(&platform);
    assert_eq!(devices.alsa_card_id.as_deref(), Some("rockchiphdmi1"));
    assert!(devices.cec.unwrap().ends_with("cec1"));
}

#[test]
fn a_connected_sink_that_no_transmitter_measures_as_its_own_is_ambiguous() {
    // The cable is in, but the only adapter with an address holds a
    // different one than the sink declares.
    let board = two_hdmi([None, Some(0x3000)]);
    board.cec_status("cec0", "f.f.f.f");
    board.cec_status("cec1", "2.0.0.0");
    let platform = inspect(&board);
    let selected = platform.selected_output().unwrap();
    assert_eq!(selected.binding, Confidence::Ambiguous);
    assert_eq!(Devices::from(&platform).cec, None);
}

#[test]
fn a_lit_connector_whose_phy_clock_is_off_is_not_that_transmitters() {
    let board = two_hdmi([None, Some(0x3000)]);
    board.dri_debugfs(1, "rockchip dev=display-subsystem unique=display-subsystem", Some(SUMMARY_HDMI2));
    board.phy("fdea0000.hdmi", "fed70000.hdmiphy", "clk_hdmiphy_pixel1", 0, false);
    let platform = inspect(&board);
    assert_eq!(platform.selected_output().unwrap().binding, Confidence::Ambiguous);
}

#[test]
fn confidence_is_ordered_and_a_chain_is_its_weakest_link() {
    assert!(Confidence::Exact < Confidence::Measured);
    assert!(Confidence::Measured < Confidence::Derived);
    assert!(Confidence::Derived < Confidence::Ambiguous);
    assert!(Confidence::Ambiguous < Confidence::Unavailable);
    assert_eq!(Confidence::Measured.and(Confidence::Exact), Confidence::Measured);
    assert_eq!(Confidence::Exact.and(Confidence::Ambiguous), Confidence::Ambiguous);
    assert!(!Confidence::Ambiguous.actionable() && !Confidence::Unavailable.actionable());
    let _ = pa(None);
}
