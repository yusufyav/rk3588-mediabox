//! What a pointer is over, asked of the real interface.
//!
//! The rest of the pointer's tests check the table (src/pointer.rs) and each
//! screen's focus model; these open the interface itself -- `MediaBoxWindow`,
//! every .slint file as the appliance draws it -- on a window that needs no
//! GPU, put a screen up, and walk a pointer across the whole panel, pressing
//! the main button at every step. What Slint's own hit testing hands to
//! `PointerHub` is what a pointer on the television would reach: which
//! targets a screen draws, that a modal's ground is over the screen under it,
//! that a disabled control is no target, that a way back is drawn where one
//! is promised.
//!
//! What only a television says -- that the arrow is drawn where the hand
//! expects, that a press on a real mouse arrives as one -- is not here.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, VecModel};

use crate::{
    LiveCard, LiveMenuRow, LivePanelRow, MediaBoxWindow, MenuRow, PointerHub, PosterItem,
    SettingNav, SettingRow, WifiRow,
};

const W: u32 = 1920;
const H: u32 = 1080;
/// The walk's step, in logical pixels: finer than the smallest target drawn
/// at this size (a key of the letter grid, a menu mark).
const STEP: usize = 14;

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

/// What a press reached: a target's zone and index, or a scrub bar's zone
/// (index -1).
type Reached = BTreeSet<(String, i32)>;

struct Panel {
    ui: MediaBoxWindow,
    log: Rc<RefCell<Reached>>,
}

impl Panel {
    fn open() -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        window.set_size(slint::PhysicalSize::new(W, H));
        slint::platform::set_platform(Box::new(Headless {
            window: window.clone(),
        }))
        .expect("one platform");
        let ui = MediaBoxWindow::new().expect("the interface");
        ui.window().set_size(slint::PhysicalSize::new(W, H));
        ui.show().expect("shown");
        let log: Rc<RefCell<Reached>> = Rc::default();
        let hub = ui.global::<PointerHub>();
        let pressed = log.clone();
        hub.on_press(move |zone, index, down| {
            if down {
                pressed.borrow_mut().insert((zone.to_string(), index));
            }
        });
        let scrubbed = log.clone();
        hub.on_scrub(move |zone, phase, _| {
            if phase == 0 {
                scrubbed.borrow_mut().insert((zone.to_string(), -1));
            }
        });
        // A pointer is in use: the marks only a pointer needs are drawn.
        hub.set_shown(true);
        Self { ui, log }
    }

    /// Every target a press reaches, the pointer walked over the panel.
    fn reach(&self) -> Reached {
        self.log.borrow_mut().clear();
        let window = self.ui.window();
        for y in (STEP / 2..H as usize).step_by(STEP) {
            for x in (STEP / 2..W as usize).step_by(STEP) {
                let position = LogicalPosition::new(x as f32, y as f32);
                let button = PointerEventButton::Left;
                window.dispatch_event(WindowEvent::PointerMoved { position });
                window.dispatch_event(WindowEvent::PointerPressed { position, button });
                window.dispatch_event(WindowEvent::PointerReleased { position, button });
            }
        }
        window.dispatch_event(WindowEvent::PointerExited);
        self.log.borrow().clone()
    }
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn zones(reached: &Reached) -> BTreeSet<&str> {
    reached.iter().map(|(zone, _)| zone.as_str()).collect()
}

fn has(reached: &Reached, zone: &str, index: i32) -> bool {
    reached.contains(&(zone.to_string(), index))
}

/// One test, one window: Slint takes one platform per thread.
#[test]
fn a_pointer_reaches_what_each_screen_draws_and_nothing_else() {
    let panel = Panel::open();
    let ui = &panel.ui;

    // ------------------------------------------------------------- home
    ui.set_screen("home".into());
    let reached = panel.reach();
    assert!(
        !zones(&reached).contains("back"),
        "home is the root: no way back {reached:?}"
    );

    // ---------------------------------------------------------- account
    ui.set_screen("account".into());
    ui.set_account_keys(crate::key_rows(&crate::keyboard::Grid::text()));
    let reached = panel.reach();
    for index in 0..4 {
        assert!(
            has(&reached, "account-form", index),
            "account-form {index}: {reached:?}"
        );
    }
    assert!(has(&reached, "account-keys", 0), "the first key");
    assert!(has(&reached, "account-keys", 502), "a key of the last row");
    assert!(zones(&reached).contains("back"), "the title bar's way back");
    // While a sign-in runs the button takes nothing.
    ui.set_account_busy(true);
    let reached = panel.reach();
    assert!(
        !has(&reached, "account-form", 3),
        "the busy button is no target"
    );
    ui.set_account_busy(false);

    // ---------------------------------------------------------- settings
    ui.set_screen("settings".into());
    ui.set_settings_sections(model(vec![
        SettingNav {
            label: "Ağ".into(),
            icon: "wifi".into(),
            blurb: "".into(),
        },
        SettingNav {
            label: "Ekran".into(),
            icon: "display".into(),
            blurb: "".into(),
        },
    ]));
    let card = |label: &str, enabled: bool, n_card: i32| SettingRow {
        label: label.into(),
        kind: "link".into(),
        selectable: true,
        enabled,
        toggle: -1,
        n_card,
        ..Default::default()
    };
    ui.set_settings_rows(model(vec![
        card("Açık", true, 0),
        card("Kapalı", false, 1),
        card("Açık 2", true, 2),
    ]));
    let reached = panel.reach();
    assert!(has(&reached, "settings-sections", 0) && has(&reached, "settings-sections", 1));
    assert!(has(&reached, "settings-rows", 0) && has(&reached, "settings-rows", 2));
    assert!(
        !has(&reached, "settings-rows", 1),
        "a disabled card is no target"
    );
    assert!(
        zones(&reached).contains("back"),
        "the column's heading is the way back to Home"
    );
    // A list over the page: its rows, its ground, its close -- nothing under.
    ui.set_settings_choice_open(true);
    ui.set_settings_choice(model(vec![
        crate::ChoiceItem {
            title: "A".into(),
            enabled: true,
            ..Default::default()
        },
        crate::ChoiceItem {
            title: "B".into(),
            enabled: false,
            ..Default::default()
        },
    ]));
    let reached = panel.reach();
    assert!(has(&reached, "settings-choice", 0));
    assert!(
        !has(&reached, "settings-choice", 1),
        "a disabled choice is no target"
    );
    assert!(zones(&reached).contains("shield") && zones(&reached).contains("back"));
    for under in ["settings-rows", "settings-sections"] {
        assert!(
            !zones(&reached).contains(under),
            "{under} is under the list"
        );
    }
    ui.set_settings_choice_open(false);

    // ------------------------------------------------------- diagnostics
    ui.set_screen("diagnostics".into());
    ui.set_diag_nav(model(vec![SettingNav {
        label: "Sistem".into(),
        icon: "bars".into(),
        blurb: "".into(),
    }]));
    let reached = panel.reach();
    assert!(has(&reached, "diagnostics-groups", 0));
    assert!(
        zones(&reached).contains("back"),
        "the column's heading is the way out"
    );

    // -------------------------------------------------------------- wifi
    ui.set_screen("wifi".into());
    ui.set_wifi_face("list".into());
    ui.set_wifi_known(true);
    ui.set_wifi_present(false);
    let flat = |kind: &str, label: &str, slot: i32, flats: i32, info: bool| WifiRow {
        kind: kind.into(),
        label: label.into(),
        bars: -1,
        slot,
        flats,
        info,
        ..Default::default()
    };
    ui.set_wifi_rows(model(vec![
        flat("toggle", "Wi-Fi", 0, 0, false),
        flat("card", "Ev", 1, 1, true),
        flat("card", "Ağ ekleyin", 2, 2, false),
    ]));
    let reached = panel.reach();
    assert!(
        !has(&reached, "wifi-rows", 0),
        "the switch of a radio the board lacks is no target"
    );
    assert!(has(&reached, "wifi-rows", 1) && has(&reached, "wifi-rows", 2));
    assert!(
        has(&reached, "wifi-info", 1),
        "a network's (i) is a target of its own"
    );
    assert!(zones(&reached).contains("back"), "the column's way back");
    ui.set_wifi_face("password".into());
    ui.set_wifi_keys(crate::key_rows(&crate::keyboard::Grid::text()));
    let reached = panel.reach();
    for index in [2, 3, 4] {
        assert!(has(&reached, "wifi-form", index), "wifi-form {index}");
    }
    assert!(has(&reached, "wifi-keys", 0));
    assert!(zones(&reached).contains("back"), "the title bar's way back");

    // --------------------------------------------------------- bluetooth
    ui.set_screen("bluetooth".into());
    ui.set_bt_known(true);
    ui.set_bt_present(true);
    ui.set_bt_rows(model(vec![
        flat("toggle", "Bluetooth", 0, 0, false),
        flat("card", "Kulaklık", 1, 1, false),
    ]));
    let reached = panel.reach();
    assert!(has(&reached, "bt-rows", 0) && has(&reached, "bt-rows", 1));
    assert!(zones(&reached).contains("back"));

    // ----------------------------------------------------------- library
    ui.set_screen("library".into());
    ui.set_media_places(model(vec![
        "home".into(),
        "discover".into(),
        "library".into(),
    ]));
    ui.set_library_zone(1);
    ui.set_library_index(0);
    ui.set_library_items(model(vec![
        PosterItem {
            title: "Bir".into(),
            ..Default::default()
        },
        PosterItem {
            title: "İki".into(),
            ..Default::default()
        },
    ]));
    let reached = panel.reach();
    assert!(has(&reached, "grid", 0) && has(&reached, "grid", 1));
    assert!(
        has(&reached, "library-mark", 0),
        "the focused poster's menu mark"
    );
    assert!(
        !has(&reached, "library-mark", 1),
        "only the focused poster carries it"
    );
    assert!(zones(&reached).contains("frame-places"));
    assert!(
        zones(&reached).contains("back"),
        "the frame's mark is the way back"
    );

    // ------------------------------------- the board, the search, settings
    for screen in ["media", "discover", "media-settings"] {
        ui.set_screen(screen.into());
        let reached = panel.reach();
        assert!(
            zones(&reached).contains("back"),
            "{screen}: the frame's mark is the way back"
        );
        assert!(
            zones(&reached).contains("frame-places"),
            "{screen}: the places"
        );
    }
    ui.set_screen("search".into());
    ui.set_search_keys(crate::key_rows(&crate::keyboard::Grid::text()));
    ui.set_search_editing(true);
    let reached = panel.reach();
    assert!(has(&reached, "search-keys", 0), "the letters in the panel");
    assert!(zones(&reached).contains("shield"), "the panel's ground");
    assert!(
        !zones(&reached).contains("frame-places"),
        "the places are under the panel"
    );
    ui.set_search_editing(false);
    let reached = panel.reach();
    assert!(!zones(&reached).contains("search-keys") && !zones(&reached).contains("shield"));
    assert!(zones(&reached).contains("back") && zones(&reached).contains("frame-search"));

    // ------------------------------------------------- a film Kodi plays
    ui.set_screen("now-playing".into());
    ui.set_np_controls(model(vec![
        crate::NpControl {
            label: "10 sn geri".into(),
            mark: "M11 6 5 12l6 6V6z".into(),
        },
        crate::NpControl {
            label: "Durdur".into(),
            mark: "M6 6h12v12H6z".into(),
        },
    ]));
    let reached = panel.reach();
    assert!(has(&reached, "np-controls", 0) && has(&reached, "np-controls", 1));
    assert!(zones(&reached).contains("back"), "the title bar's way back");

    // ------------------------------------------------------------ detail
    ui.set_screen("detail".into());
    let reached = panel.reach();
    assert!(
        zones(&reached).contains("back"),
        "the chevron is the way back"
    );

    // -------------------------------------------------------------- film
    ui.set_screen("film".into());
    ui.set_film_controls(true);
    ui.set_np_marks(model(vec![
        "M8 5l11.5 7L8 19z".into(),
        "".into(),
        "".into(),
    ]));
    ui.set_np_lines(model(vec!["".into(), "M3 3h18".into(), "M3 3h18".into()]));
    let reached = panel.reach();
    for index in 0..3 {
        assert!(
            has(&reached, "player-buttons", index),
            "player-buttons {index}"
        );
    }
    assert!(has(&reached, "player-bar", -1), "the bar is dragged");
    assert!(
        zones(&reached).contains("player-exit"),
        "the way out of the film"
    );
    // A panel over the film: its rows, its ground, its close -- not the row.
    ui.set_np_menu("speed".into());
    ui.set_np_menu_rows(model(vec![
        MenuRow {
            label: "0.5×".into(),
            ..Default::default()
        },
        MenuRow {
            label: "1×".into(),
            active: true,
            ..Default::default()
        },
    ]));
    let reached = panel.reach();
    assert!(has(&reached, "player-menu", 0) && has(&reached, "player-menu", 1));
    assert!(zones(&reached).contains("shield") && zones(&reached).contains("back"));
    for under in ["player-buttons", "player-bar", "player-exit"] {
        assert!(
            !zones(&reached).contains(under),
            "{under} is under the panel"
        );
    }
    ui.set_np_menu("".into());

    // -------------------------------------------------------------- live
    ui.set_screen("live".into());
    let reached = panel.reach();
    assert!(
        zones(&reached).contains("live-surface"),
        "the picture's ground is Ok"
    );
    assert!(
        zones(&reached).contains("back"),
        "the way out of live television"
    );
    let card = |title: &str| LiveCard {
        title: title.into(),
        ..Default::default()
    };
    ui.set_live_menu_shown(true);
    ui.set_live_menu_row(0);
    ui.set_live_menu_rows(model(vec![
        LiveMenuRow {
            title: "Kanallar".into(),
            cards: model(vec![card("Rehber"), card("Liste")]),
        },
        LiveMenuRow {
            title: "TV seçenekleri".into(),
            cards: model(vec![card("Ses")]),
        },
    ]));
    let reached = panel.reach();
    assert!(
        has(&reached, "live-menu", 0) && has(&reached, "live-menu", 1),
        "the row's cards"
    );
    assert!(has(&reached, "live-menu", 199), "the next row's title");
    assert!(zones(&reached).contains("shield") && zones(&reached).contains("back"));
    assert!(
        !zones(&reached).contains("live-surface"),
        "the picture is under the menu"
    );
    ui.set_live_menu_shown(false);
    ui.set_live_panel_shown(true);
    ui.set_live_panel_rows(model(vec![
        LivePanelRow {
            label: "Kaynak".into(),
            enabled: false,
            ..Default::default()
        },
        LivePanelRow {
            label: "Kanal kurulumu".into(),
            enabled: true,
            ..Default::default()
        },
    ]));
    let reached = panel.reach();
    assert!(
        !has(&reached, "live-panel", 0),
        "a row that cannot be chosen is no target"
    );
    assert!(has(&reached, "live-panel", 1));
    assert!(zones(&reached).contains("shield"));
    ui.set_live_panel_shown(false);
}
