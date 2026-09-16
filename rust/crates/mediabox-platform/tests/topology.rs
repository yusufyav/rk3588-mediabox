//! Discovery, asked about boards that are not here.
//!
//! Every assertion below is written so that it would fail if the answer were
//! hard-coded: the fixtures deliberately move the card numbers, the render-node
//! numbers, the ALSA card numbers and the connector-to-sound-card mapping away
//! from the values the product was first written against.

mod common;

use common::{Board, edid, one_hdmi, three_outputs};
use mediabox_platform::{Binding, ConnectorKind, Overrides, Platform, Roots, SelectionReason};

fn inspect(board: &Board) -> Platform {
    Platform::inspect(&Roots::under(board.root()), &Overrides::default())
}

fn inspect_with(board: &Board, overrides: Overrides) -> Platform {
    Platform::inspect(&Roots::under(board.root()), &overrides)
}

// ---------------------------------------------------------------- one output

#[test]
fn a_board_with_one_hdmi_resolves_all_the_way_through() {
    let platform = inspect(&one_hdmi());

    let kms = platform.kms.as_ref().expect("a modesetting device");
    assert_eq!(kms.driver.as_deref(), Some("rockchip-drm"));
    let render = platform.render.as_ref().expect("a render device");
    assert_eq!(
        render.parent, kms.parent,
        "render and scanout are one device"
    );

    let selected = platform.selected.as_ref().expect("an output");
    assert_eq!(selected.reason, SelectionReason::OnlyConnected);
    assert_eq!(selected.output.connector.name, "HDMI-A-1");
    assert_eq!(selected.output.binding, Binding::Measured);
    assert_eq!(
        selected.output.audio.as_ref().map(|a| a.card_id.as_str()),
        Some("rockchiphdmi1")
    );
    assert_eq!(
        selected.output.cec.as_ref().map(|c| c.name.as_str()),
        Some("cec0")
    );
    assert!(platform.warnings.is_empty(), "{:?}", platform.warnings);
}

#[test]
fn the_sink_is_identified_by_its_edid_and_not_by_the_connector() {
    let platform = inspect(&one_hdmi());
    let sink = platform
        .selected_output()
        .and_then(|output| output.connector.sink.clone())
        .expect("a sink identity");
    assert_eq!(sink.manufacturer, "SNY");
    assert_eq!(sink.name.as_deref(), Some("BRAVIA"));
}

// -------------------------------------------------------------- three outputs

#[test]
fn the_first_hdmi_connector_is_not_the_first_sound_card() {
    // The trap this whole module exists for. On this board `HDMI-A-1` belongs
    // to the transmitter the device tree calls hdmi0, so its sound card is
    // `rockchiphdmi0` — while on the one-output board the only connector's card
    // is `rockchiphdmi1`. Same connector name, different card.
    let platform = inspect(&three_outputs(&["HDMI-A-1"]));
    let selected = platform.selected_output().expect("an output");
    assert_eq!(selected.connector.name, "HDMI-A-1");
    assert_eq!(
        selected.audio.as_ref().map(|a| a.card_id.as_str()),
        Some("rockchiphdmi0"),
    );
    assert_eq!(selected.cec.as_ref().map(|c| c.name.as_str()), Some("cec0"));
}

#[test]
fn the_second_hdmi_connector_carries_the_second_transmitters_card() {
    let platform = inspect(&three_outputs(&["HDMI-A-2"]));
    let selected = platform.selected_output().expect("an output");
    assert_eq!(selected.connector.name, "HDMI-A-2");
    assert_eq!(
        selected.audio.as_ref().map(|a| a.card_id.as_str()),
        Some("rockchiphdmi1"),
    );
    assert_eq!(selected.cec.as_ref().map(|c| c.name.as_str()), Some("cec1"));
    assert_eq!(selected.binding, Binding::Measured);
}

#[test]
fn the_alsa_card_config_name_follows_the_selected_output() {
    // alsa-lib finds the compressed-audio PCM definition by the card's driver
    // name, so installing one file called `rockchip-hdmi1.conf` is right on one
    // board and addresses the wrong socket on the other.
    let first = inspect(&three_outputs(&["HDMI-A-1"]));
    let second = inspect(&three_outputs(&["HDMI-A-2"]));
    let driver = |platform: &Platform| {
        platform
            .selected_output()
            .and_then(|output| output.audio.as_ref())
            .and_then(|audio| audio.driver.clone())
    };
    assert_eq!(driver(&first).as_deref(), Some("rockchip-hdmi0"));
    assert_eq!(driver(&second).as_deref(), Some("rockchip-hdmi1"));
}

#[test]
fn the_connector_carries_the_path_the_kernel_publishes_it_at() {
    // The things done to a connector before anything draws on it are sysfs
    // writes, and the path contains the card's number — which is exactly the
    // number a caller must not have to know.
    let board = one_hdmi();
    let platform = inspect(&board);
    let connector = &platform.selected_output().expect("an output").connector;
    assert!(
        connector.sysfs.ends_with("card0-HDMI-A-1"),
        "{}",
        connector.sysfs.display()
    );
    assert!(connector.sysfs.join("status").is_file());
}

#[test]
fn displayport_has_no_cec_and_that_is_not_a_failure() {
    let platform = inspect(&three_outputs(&["DP-1"]));
    let selected = platform.selected_output().expect("an output");
    assert_eq!(selected.connector.kind, ConnectorKind::DisplayPort);
    assert_eq!(
        selected.audio.as_ref().map(|a| a.card_id.as_str()),
        Some("rockchipdp0"),
        "DisplayPort audio still resolves"
    );
    assert!(selected.cec.is_none(), "DisplayPort has no CEC adapter");
    assert!(
        platform.warnings.is_empty(),
        "a missing CEC adapter is a capability, not a fault: {:?}",
        platform.warnings
    );
}

#[test]
fn nothing_plugged_in_is_reported_rather_than_guessed() {
    let platform = inspect(&three_outputs(&[]));
    assert!(platform.selected.is_none());
    assert!(
        platform.kms.is_some(),
        "the board still has a display device"
    );
    assert_eq!(platform.outputs.len(), 3, "all three are still enumerated");
    assert!(
        platform
            .warnings
            .iter()
            .any(|warning| warning.contains("no connected output")),
    );
}

// ------------------------------------------------------------- several at once

#[test]
fn two_connected_outputs_resolve_deterministically_to_the_television() {
    let platform = inspect(&three_outputs(&["HDMI-A-2", "DP-1"]));
    let selected = platform.selected.as_ref().expect("an output");
    assert_eq!(selected.reason, SelectionReason::Policy);
    assert_eq!(
        selected.output.connector.name, "HDMI-A-2",
        "HDMI is preferred over DisplayPort on a television appliance"
    );
    // And the audio and CEC that came with it, not the other output's.
    assert_eq!(
        selected.output.audio.as_ref().map(|a| a.card_id.as_str()),
        Some("rockchiphdmi1")
    );
    assert_eq!(
        selected.output.cec.as_ref().map(|c| c.name.as_str()),
        Some("cec1")
    );
}

#[test]
fn two_cables_of_the_same_kind_fall_back_to_order_rather_than_guessing() {
    // Extcon says "a cable is in me" and nothing more, so with two HDMI
    // transmitters both reporting one it cannot say which connector is which.
    // The binding falls back to order and says so, instead of picking one of
    // the two measurements and calling it measured.
    let platform = inspect(&three_outputs(&["HDMI-A-1", "HDMI-A-2"]));
    let by_name = |name: &str| {
        platform
            .outputs
            .iter()
            .find(|output| output.connector.name == name)
            .expect("the connector")
    };
    assert_eq!(by_name("HDMI-A-1").binding, Binding::Ordered);
    assert_eq!(by_name("HDMI-A-2").binding, Binding::Ordered);
    assert_eq!(
        by_name("HDMI-A-1")
            .audio
            .as_ref()
            .map(|a| a.card_id.as_str()),
        Some("rockchiphdmi0")
    );
    assert_eq!(
        by_name("HDMI-A-2")
            .audio
            .as_ref()
            .map(|a| a.card_id.as_str()),
        Some("rockchiphdmi1")
    );
    assert!(platform.warnings.is_empty(), "{:?}", platform.warnings);
}

#[test]
fn a_remembered_choice_wins_while_it_is_plugged_in() {
    let board = three_outputs(&["HDMI-A-1", "HDMI-A-2"]);
    board.remember_output("HDMI-A-2");
    let platform = inspect(&board);
    let selected = platform.selected.as_ref().expect("an output");
    assert_eq!(selected.reason, SelectionReason::Configured);
    assert_eq!(selected.output.connector.name, "HDMI-A-2");
    assert_eq!(
        selected.output.audio.as_ref().map(|a| a.card_id.as_str()),
        Some("rockchiphdmi1")
    );
}

#[test]
fn a_remembered_choice_that_is_unplugged_says_so() {
    let board = three_outputs(&["HDMI-A-1"]);
    board.remember_output("DP-1");
    let platform = inspect(&board);
    let selected = platform.selected.as_ref().expect("an output");
    assert_eq!(selected.reason, SelectionReason::ConfiguredUnavailable);
    assert_eq!(selected.output.connector.name, "HDMI-A-1");
    assert!(
        platform
            .warnings
            .iter()
            .any(|warning| warning.contains("DP-1")),
        "{:?}",
        platform.warnings
    );
}

#[test]
fn an_output_can_be_remembered_by_the_television_rather_than_the_socket() {
    let board = Board::new();
    board.platform_device("display-subsystem", "rockchip-drm", None);
    board.drm_card("card0", "display-subsystem");
    board.render_node("renderD128", "display-subsystem");
    board.connector(
        "card0",
        "display-subsystem",
        "HDMI-A-1",
        true,
        &["1920x1080"],
        &edid(*b"SAM", 0x0C0C, 0x0000_1234, "Q90R"),
    );
    board.connector(
        "card0",
        "display-subsystem",
        "HDMI-A-2",
        true,
        &["3840x2160"],
        &edid(*b"SNY", 0x0101, 0x0000_2A2A, "BRAVIA"),
    );
    board.transmitter("fde80000.hdmi", "hdmi@fde80000", 0x100, Some(true));
    board.transmitter("fdea0000.hdmi", "hdmi@fdea0000", 0x101, Some(true));
    board.remember_output("BRAVIA");

    let platform = inspect(&board);
    let selected = platform.selected.as_ref().expect("an output");
    assert_eq!(selected.reason, SelectionReason::Configured);
    assert_eq!(
        selected.output.connector.name, "HDMI-A-2",
        "the set was named, and it is in the second socket"
    );
}

// --------------------------------------------------- enumeration moves around

#[test]
fn the_display_device_is_not_assumed_to_be_card0() {
    // The NPU probed first this boot, so the display is card1 and the Mali
    // render node is renderD129. Nothing in the product may notice.
    let board = Board::new();
    board.platform_device("fdab0000.npu", "RKNPU", None);
    board.platform_device("display-subsystem", "rockchip-drm", None);
    board.drm_card("card0", "fdab0000.npu");
    board.render_node("renderD128", "fdab0000.npu");
    board.drm_card("card1", "display-subsystem");
    board.render_node("renderD129", "display-subsystem");
    board.connector(
        "card1",
        "display-subsystem",
        "HDMI-A-1",
        true,
        &["3840x2160"],
        &[],
    );
    board.transmitter("fdea0000.hdmi", "hdmi@fdea0000", 0x100, Some(true));
    board.cec("fdea0000.hdmi", "cec0");
    board.sound_card(0, "rockchiphdmi1", "hdmi1-sound", Some(0x100));

    let platform = inspect(&board);
    assert_eq!(platform.kms.as_ref().unwrap().name, "card1");
    assert!(
        platform.kms.as_ref().unwrap().device.ends_with("dri/card1"),
        "the device path follows the device, not the number the product started life on"
    );
    assert_eq!(platform.render.as_ref().unwrap().name, "renderD129");
    assert_eq!(
        platform.selected_output().unwrap().connector.name,
        "HDMI-A-1"
    );
}

#[test]
fn alsa_card_numbers_may_be_reordered_without_changing_anything() {
    // Same board, same card *ids*, different numbers. The product addresses
    // cards by id for exactly this reason.
    let board = one_hdmi();
    let platform_before = inspect(&board);

    let shuffled = Board::new();
    shuffled.platform_device("display-subsystem", "rockchip-drm", None);
    shuffled.drm_card("card0", "display-subsystem");
    shuffled.render_node("renderD128", "display-subsystem");
    shuffled.connector(
        "card0",
        "display-subsystem",
        "HDMI-A-1",
        true,
        &["3840x2160"],
        &[],
    );
    shuffled.transmitter("fdea0000.hdmi", "hdmi@fdea0000", 0x100, Some(true));
    shuffled.cec("fdea0000.hdmi", "cec0");
    // The board's own codec came up first this time; HDMI is card 3.
    shuffled.sound_card(0, "rockchipes8388", "es8388-sound", None);
    shuffled.sound_card(3, "rockchiphdmi1", "hdmi1-sound", Some(0x100));

    let platform_after = inspect(&shuffled);
    let card = |platform: &Platform| {
        platform
            .selected_output()
            .and_then(|output| output.audio.clone())
    };
    assert_eq!(
        card(&platform_before).map(|a| a.card_id),
        card(&platform_after).map(|a| a.card_id),
        "the id is the identity"
    );
    assert_eq!(card(&platform_before).and_then(|a| a.card_index), Some(0));
    assert_eq!(card(&platform_after).and_then(|a| a.card_index), Some(3));
    assert_eq!(
        card(&platform_after).and_then(|a| a.pcm),
        Some("hw:CARD=rockchiphdmi1,DEV=0".into()),
        "and what is addressed is built from the id, never from the number"
    );
}

#[test]
fn connectors_bind_by_order_when_nothing_is_plugged_in_to_measure() {
    let platform = inspect(&three_outputs(&[]));
    let by_name = |name: &str| {
        platform
            .outputs
            .iter()
            .find(|output| output.connector.name == name)
            .expect("the connector")
    };
    assert_eq!(
        by_name("HDMI-A-1").controller.as_deref(),
        Some("fde80000.hdmi")
    );
    assert_eq!(by_name("HDMI-A-1").binding, Binding::Ordered);
    assert_eq!(
        by_name("HDMI-A-2").controller.as_deref(),
        Some("fdea0000.hdmi")
    );
    assert_eq!(by_name("DP-1").controller.as_deref(), Some("fde50000.dp"));
}

// ------------------------------------------------------------------ overrides

#[test]
fn overrides_are_honoured_and_reported_as_overrides() {
    let board = three_outputs(&["HDMI-A-1", "HDMI-A-2"]);
    board.remember_output("HDMI-A-1");
    let platform = inspect_with(
        &board,
        Overrides {
            output: Some("HDMI-A-2".into()),
            ..Overrides::default()
        },
    );
    let selected = platform.selected.as_ref().expect("an output");
    assert_eq!(selected.reason, SelectionReason::Override);
    assert_eq!(selected.output.connector.name, "HDMI-A-2");
}

#[test]
fn an_override_naming_a_device_that_is_not_there_falls_back_to_discovery() {
    let platform = inspect_with(
        &one_hdmi(),
        Overrides {
            kms: Some("/dev/dri/card7".into()),
            render: Some("/dev/dri/renderD200".into()),
            ..Overrides::default()
        },
    );
    assert_eq!(platform.kms.as_ref().unwrap().name, "card0");
    assert_eq!(platform.render.as_ref().unwrap().name, "renderD128");
}

#[test]
fn a_machine_with_no_display_hardware_at_all_is_an_answer_not_a_panic() {
    let board = Board::new();
    let platform = inspect(&board);
    assert!(platform.kms.is_none());
    assert!(platform.selected.is_none());
    assert!(!platform.warnings.is_empty());
}
