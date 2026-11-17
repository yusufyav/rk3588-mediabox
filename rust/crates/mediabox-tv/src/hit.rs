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
//! The pointer's surfaces are here too: a `PanState` moved the way Rust
//! moves it (`PointerPan`) shows other targets under the same walk, only
//! the surface it names moves, it stops at its ends, and it goes back to
//! the remote's focus once the pointer is put away.
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
    LiveCard, LiveMenuRow, LivePanelRow, MediaBoxWindow, MenuRow, PointerHub, PointerPan,
    PosterItem, RailModel, SettingNav, SettingRow, WifiRow,
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
    /// Hover's comings and goings, in order: (zone, index, over).
    hovers: Rc<RefCell<Vec<(String, i32, bool)>>>,
    /// What the surfaces said back: a window of rows moved by whole rows,
    /// and a surface run into its end.
    steps: Rc<RefCell<Vec<(String, i32, i32)>>>,
    edges: Rc<RefCell<Vec<(String, i32)>>>,
    /// What moved surfaces said they show: (surface, lane, first item).
    views: Rc<RefCell<Vec<(String, i32, i32)>>>,
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
        // Hover lights a target as Rust does (`App::pointer_hover`): the one
        // the arrow came over, until it leaves it.
        let hovers: Rc<RefCell<Vec<(String, i32, bool)>>> = Rc::default();
        let heard = hovers.clone();
        let weak = ui.as_weak();
        hub.on_hover(move |zone, index, over| {
            heard.borrow_mut().push((zone.to_string(), index, over));
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let hub = ui.global::<PointerHub>();
            if over && zone != "shield" {
                hub.set_lit_zone(zone);
                hub.set_lit_index(index);
            } else if hub.get_lit_zone() == zone && hub.get_lit_index() == index {
                hub.set_lit_zone("".into());
                hub.set_lit_index(-1);
            }
        });
        let steps: Rc<RefCell<Vec<(String, i32, i32)>>> = Rc::default();
        let stepped = steps.clone();
        hub.on_step(move |surface, lane, rows| {
            stepped.borrow_mut().push((surface.to_string(), lane, rows));
        });
        let edges: Rc<RefCell<Vec<(String, i32)>>> = Rc::default();
        let edged = edges.clone();
        hub.on_edge(move |surface, lane| edged.borrow_mut().push((surface.to_string(), lane)));
        let views: Rc<RefCell<Vec<(String, i32, i32)>>> = Rc::default();
        let viewed = views.clone();
        hub.on_view(move |surface, lane, first| {
            viewed.borrow_mut().push((surface.to_string(), lane, first))
        });
        // A pointer is in use: the marks only a pointer needs are drawn, and
        // the surfaces stay where they are under a click.
        hub.set_shown(true);
        ui.global::<PointerPan>().set_pinned(true);
        Self {
            ui,
            log,
            hovers,
            steps,
            edges,
            views,
        }
    }

    /// The hand moves `surface` (lane `lane`) by a running total, as Rust
    /// sends a gesture, and the interface catches up.
    fn pan(&self, surface: &str, lane: i32, gesture: i32, dx: f32, dy: f32) {
        let pan = self.ui.global::<PointerPan>();
        pan.set_eased(false);
        pan.set_surface(surface.into());
        pan.set_lane(lane);
        pan.set_dx(dx);
        pan.set_dy(dy);
        pan.set_gesture(gesture);
        self.settle();
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

impl Panel {
    /// Runs what changed, as a frame would: what a screen draws is made
    /// (an event puts the tree up to date), and the surfaces' change
    /// handlers run.
    fn settle(&self) {
        self.ui.window().dispatch_event(WindowEvent::PointerExited);
        slint::platform::update_timers_and_animations();
    }

    /// After the remote's own easing has run its course.
    fn settle_eased(&self) {
        self.settle();
        std::thread::sleep(std::time::Duration::from_millis(260));
        self.settle();
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
        has(&reached, "library-mark", 1),
        "the poster the arrow is over carries it too: a pointer reaches the \
         menu without moving the remote's focus"
    );
    // Hover came and went: every target the arrow came over it also left.
    {
        let hovers = panel.hovers.borrow();
        assert!(hovers.contains(&("grid".to_string(), 1, true)));
        assert!(hovers.contains(&("grid".to_string(), 1, false)));
    }
    assert_eq!(
        ui.global::<PointerHub>().get_lit_index(),
        -1,
        "the arrow off the panel lights nothing"
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

fn indices(reached: &Reached, zone: &str) -> BTreeSet<i32> {
    reached
        .iter()
        .filter(|(z, _)| z == zone)
        .map(|(_, index)| *index)
        .collect()
}

/// The surfaces a pointer moves, on the real interface: a hand's drag (as
/// Rust hands it over, `PointerPan`) brings other targets under the same
/// walk; only the surface it names moves; it stops at its ends and turns
/// back at once; a modal's list moves and the screen under it does not; a
/// shelf moves along and the board and the other shelves stay; a list Rust
/// keeps as a window of rows is told whole rows; and the remote picked up
/// again brings every surface back to its focus. One window per thread, as
/// above.
#[test]
fn a_pointer_moves_only_the_surface_it_drags_and_the_remote_brings_it_back() {
    let panel = Panel::open();
    let ui = &panel.ui;

    // ------------------------------------------------- a long settings list
    ui.set_screen("settings".into());
    ui.set_settings_on_sections(false);
    ui.set_settings_sections(model(vec![SettingNav {
        label: "Ağ".into(),
        icon: "wifi".into(),
        blurb: "".into(),
    }]));
    let rows: Vec<SettingRow> = (0..30)
        .map(|n| SettingRow {
            label: format!("Ayar {n}").into(),
            kind: "link".into(),
            selectable: true,
            enabled: true,
            toggle: -1,
            n_card: n,
            ..Default::default()
        })
        .collect();
    ui.set_settings_rows(model(rows));
    ui.set_settings_row(0);
    panel.settle();
    let reached = panel.reach();
    let shown = indices(&reached, "settings-rows");
    assert!(shown.contains(&0) && !shown.contains(&29), "{shown:?}");

    // Dragged up a long way: the list's end, and no further.
    panel.pan("settings-rows", -1, 1, 0.0, -5000.0);
    let reached = panel.reach();
    let bottom = indices(&reached, "settings-rows");
    assert!(bottom.contains(&29) && !bottom.contains(&0), "{bottom:?}");
    assert!(
        panel
            .edges
            .borrow()
            .contains(&("settings-rows".to_string(), -1)),
        "the end is said, for a coast to stop at"
    );
    assert!(
        has(&reached, "settings-sections", 0),
        "the column beside it did not move"
    );
    // Turned back by 400 pixels: it moves at once, not after the 5000
    // pixels past its end are undone.
    panel.pan("settings-rows", -1, 1, 0.0, -4600.0);
    let turned = indices(&panel.reach(), "settings-rows");
    assert!(
        !turned.contains(&29) && turned.iter().max() < bottom.iter().max(),
        "{turned:?}"
    );
    // A new gesture down past the top: the start, and no further.
    panel.pan("settings-rows", -1, 2, 0.0, 100_000.0);
    let top = indices(&panel.reach(), "settings-rows");
    assert_eq!(top.iter().min(), Some(&0), "{top:?}");

    // The remote's focus moving under a pointer does not move the list: a
    // click's focus is not a scroll.
    panel.pan("settings-rows", -1, 3, 0.0, -5000.0);
    ui.set_settings_row(3);
    panel.settle();
    assert!(indices(&panel.reach(), "settings-rows").contains(&29));
    // The remote picked up again: the list is the focus's once more.
    ui.global::<PointerPan>().set_pinned(false);
    panel.settle_eased();
    let back = indices(&panel.reach(), "settings-rows");
    assert!(
        back.contains(&3) && !back.contains(&29),
        "the focus in view again: {back:?}"
    );
    ui.set_settings_row(29);
    panel.settle_eased();
    assert!(
        indices(&panel.reach(), "settings-rows").contains(&29),
        "the remote scrolls it as it always did"
    );
    ui.global::<PointerPan>().set_pinned(true);
    panel.settle();

    // ------------------------------------------- a modal's list over it
    ui.set_settings_row(0);
    ui.global::<PointerPan>().set_pinned(false);
    panel.settle_eased();
    ui.global::<PointerPan>().set_pinned(true);
    ui.set_settings_choice_open(true);
    ui.set_settings_choice(model(
        (0..20)
            .map(|n| crate::ChoiceItem {
                title: format!("Seçenek {n}").into(),
                enabled: true,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    ui.set_settings_choice_focus(0);
    panel.settle();
    let choices = indices(&panel.reach(), "settings-choice");
    assert!(
        choices.contains(&0) && !choices.contains(&19),
        "{choices:?}"
    );
    panel.pan("settings-choice", -1, 4, 0.0, -5000.0);
    let choices = indices(&panel.reach(), "settings-choice");
    assert!(
        choices.contains(&19) && !choices.contains(&0),
        "{choices:?}"
    );
    ui.set_settings_choice_open(false);
    panel.settle();
    let under = indices(&panel.reach(), "settings-rows");
    assert!(
        under.contains(&0) && !under.contains(&29),
        "the list under the modal stayed where it was: {under:?}"
    );

    // ------------------------------------------------- the board's shelves
    ui.set_screen("media".into());
    let poster = |n: usize| PosterItem {
        title: format!("Film {n}").into(),
        ..Default::default()
    };
    let rail = |title: &str| RailModel {
        title: title.into(),
        items: model((0..25).map(poster).collect()),
        ..Default::default()
    };
    ui.set_media_rails(model(vec![
        rail("Bir"),
        rail("İki"),
        rail("Üç"),
        rail("Dört"),
    ]));
    ui.set_media_columns(model(vec![0, 0, 0, 0]));
    ui.set_media_row(0);
    ui.set_media_zone(0);
    panel.settle();
    let shelves = indices(&panel.reach(), "shelves");
    assert!(
        shelves.contains(&0) && shelves.contains(&100),
        "{shelves:?}"
    );
    assert!(!shelves.contains(&24), "{shelves:?}");
    // The first shelf along to its end: its last poster in reach; the
    // second shelf and the board did not move.
    panel.views.borrow_mut().clear();
    panel.pan("shelves", 0, 5, -20_000.0, 0.0);
    let shelves = indices(&panel.reach(), "shelves");
    assert!(
        shelves.contains(&24) && !shelves.contains(&0),
        "{shelves:?}"
    );
    // The shelf says what it shows, so its artwork is loaded there and not
    // around the remote's column (0).
    let shown = panel
        .views
        .borrow()
        .iter()
        .rev()
        .find(|(surface, lane, _)| surface == "shelves" && *lane == 0)
        .map(|(.., first)| *first);
    assert!(
        shown.is_some_and(|first| first >= 10),
        "the shelf's end is what is shown: {shown:?}"
    );
    assert!(
        shelves.contains(&100) && !shelves.contains(&124),
        "{shelves:?}"
    );
    // The board down: the last shelf in reach, the first gone.
    panel.pan("shelves", -1, 6, 0.0, -20_000.0);
    let shelves = indices(&panel.reach(), "shelves");
    assert!(shelves.iter().any(|i| i / 100 == 3), "{shelves:?}");
    assert!(!shelves.iter().any(|i| i / 100 == 0), "{shelves:?}");

    // -------------------------------------------------- the library's grid
    ui.set_screen("library".into());
    ui.set_library_zone(1);
    ui.set_library_index(0);
    ui.set_library_items(model((0..60).map(poster).collect()));
    panel.settle();
    let grid = indices(&panel.reach(), "grid");
    assert!(grid.contains(&0) && !grid.contains(&59), "{grid:?}");
    panel.pan("grid", -1, 7, 0.0, -50_000.0);
    let grid = indices(&panel.reach(), "grid");
    assert!(grid.contains(&59) && !grid.contains(&0), "{grid:?}");

    // A pointer's own Back is the remote's Back: every surface goes back to
    // the focus (here the first poster), the pointer still in use.
    let pan = ui.global::<PointerPan>();
    pan.set_release_surface("*".into());
    pan.set_release_lane(-1);
    pan.set_release(pan.get_release() + 1);
    panel.settle_eased();
    let grid = indices(&panel.reach(), "grid");
    assert!(grid.contains(&0) && !grid.contains(&59), "{grid:?}");
    assert!(pan.get_pinned(), "the pointer is still in use");

    // A screen opened under a pointer, whose items and remembered focus
    // arrive after it is drawn: it shows the focus, not the top it had
    // before they came (where no artwork is wanted).
    ui.set_screen("home".into());
    ui.set_library_items(model(Vec::new()));
    ui.set_library_index(0);
    panel.settle();
    ui.set_screen("library".into());
    panel.settle();
    ui.set_library_items(model((0..60).map(poster).collect()));
    ui.set_library_index(55);
    panel.settle_eased();
    let grid = indices(&panel.reach(), "grid");
    assert!(grid.contains(&55) && !grid.contains(&0), "{grid:?}");

    // ------------------------------- a window of rows: the film's panel
    ui.set_screen("film".into());
    ui.set_np_menu("speed".into());
    ui.set_np_menu_rows(model(
        (0..9)
            .map(|n| MenuRow {
                label: format!("{n}").into(),
                ..Default::default()
            })
            .collect(),
    ));
    panel.settle();
    panel.steps.borrow_mut().clear();
    let pitch = H as f32 * 0.058;
    // Two and a half rows up: two whole rows further down the list...
    panel.pan("player-menu", -1, 8, 0.0, -pitch * 2.5);
    // ...and on by one more: the half carried over makes another row.
    panel.pan("player-menu", -1, 8, 0.0, -pitch * 3.6);
    // A new gesture back up.
    panel.pan("player-menu", -1, 9, 0.0, pitch * 1.2);
    assert_eq!(
        *panel.steps.borrow(),
        vec![
            ("player-menu".to_string(), -1, 2),
            ("player-menu".to_string(), -1, 1),
            ("player-menu".to_string(), -1, -1),
        ]
    );
    // The column beside it was not told anything.
    assert!(
        !panel
            .steps
            .borrow()
            .iter()
            .any(|(surface, ..)| surface == "player-tracks")
    );
    ui.set_np_menu("".into());
}

// ------------------------------------------------- the application, driven

/// The application itself on the real interface: `App` with the product's
/// own pointer callbacks (`wire_pointer`), its events handed over the way
/// the platform hands them (`platform::deliver_pointer`,
/// `platform::hand_scroll`). What is checked is what the application does
/// in that order -- where the remote's focus is, what the gesture moves --
/// not what a source file says.
fn driven() -> MediaBoxWindow {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(slint::PhysicalSize::new(W, H));
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
    }))
    .expect("one platform");
    let ui = MediaBoxWindow::new().expect("the interface");
    ui.window().set_size(slint::PhysicalSize::new(W, H));
    ui.show().expect("shown");
    // Nothing of the product's is read or written: a place of the test's own.
    let dir = std::env::temp_dir().join(format!(
        "mediabox-tv-driven-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let files = crate::AppFiles {
        cache: dir.join("cache").display().to_string(),
        state: dir.join("state.json").display().to_string(),
        prefs: dir.join("media-preferences.json"),
        live_kept: crate::LiveKept::default(),
    };
    let app = crate::App::new(&ui, std::time::Instant::now(), false, files);
    crate::APP.with(|slot| *slot.borrow_mut() = Some(Rc::new(RefCell::new(app))));
    crate::wire_pointer(&ui);
    ui
}

fn app<R>(f: impl FnOnce(&mut crate::App) -> R) -> R {
    let app = crate::APP.with(|slot| slot.borrow().clone()).expect("the application");
    f(&mut app.borrow_mut())
}

/// The arrow to (x, y), and everything it says delivered, as one pass of
/// the platform's loop: what the wheel would scroll there.
fn point(ui: &MediaBoxWindow, x: f32, y: f32) -> crate::pointer::Lanes {
    app(|app| {
        app.pointer_motion(crate::pointer::Motion::Absolute(
            x / W as f32,
            y / H as f32,
        ))
    });
    crate::platform::deliver_pointer(ui.window());
    app(|app| {
        app.pointer_lit
            .map(|(zone, index)| zone.lanes(index))
            .or(app.pointer_area)
            .unwrap_or_default()
    })
}

/// The first place, walking the panel, where the arrow is over `want`'s
/// surface (or, with `None`, over nothing that scrolls).
fn find(ui: &MediaBoxWindow, want: Option<crate::pointer::Zone>) -> (f32, f32) {
    for y in (STEP / 2..H as usize).step_by(STEP * 3) {
        for x in (STEP / 2..W as usize).step_by(STEP * 3) {
            let (x, y) = (x as f32, y as f32);
            let lanes = point(ui, x, y);
            let found = match want {
                Some(zone) => lanes.vertical.is_some_and(|lane| lane.zone == zone),
                None => lanes.is_empty() && app(|app| app.pointer_lit.is_none()),
            };
            if found {
                return (x, y);
            }
        }
    }
    panic!("nowhere on the panel is {want:?}");
}

/// Where the remote's focus is on the settings screen.
fn settings_focus() -> (usize, crate::screens::settings::Pane) {
    app(|app| (app.settings.section, app.settings.pane))
}

/// A key puts the arrow away and forgets what it was over; the first wheel
/// after it brings the arrow back where it was and scrolls what is under
/// it -- never the remote's focus -- and the second does the same. Where
/// nothing scrolls under the arrow, the wheel is the arrows, as before.
/// Handed over in one call (no `hand_scroll`), the first turn was decided
/// before Slint had said what was under the arrow, and moved the focus.
#[test]
fn the_first_wheel_after_a_key_scrolls_what_is_under_the_arrow() {
    use crate::pointer::{Phase, Scroll, Zone};
    use mediabox_core::InputAction;
    let ui = driven();
    app(|app| app.open(crate::route::Route::Settings));
    let rows = find(&ui, Some(Zone::SettingsRows));
    // The remote's own pace (`input::Dispatcher`): a step sooner than this
    // after the last is dropped, and a dropped arrow would pass for a focus
    // that stayed.
    let pace = || std::thread::sleep(std::time::Duration::from_millis(200));
    let key = |action| {
        pace();
        app(|app| {
            app.remote_key(action, true, crate::input::Origin::Keyboard);
            app.remote_key(action, false, crate::input::Origin::Keyboard);
        });
        crate::platform::deliver_pointer(ui.window());
    };
    let wheel = || {
        pace();
        crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, 120.0));
        app(|app| app.gesture.phase())
    };
    let over_rows = |phase: Phase| {
        matches!(phase, Phase::Wheeling { lane, .. } if lane.zone == Zone::SettingsRows)
    };

    key(InputAction::Down);
    assert!(!app(|app| app.pointer.shown()), "a key puts the arrow away");
    let focus = settings_focus();
    let first = wheel();
    assert!(app(|app| app.pointer.shown()), "the wheel brings it back");
    assert_eq!(
        app(|app| app.pointer.at()),
        Some(rows),
        "where it was"
    );
    assert!(over_rows(first), "the first turn: {first:?}");
    assert_eq!(settings_focus(), focus, "the focus stays");
    let second = wheel();
    assert!(over_rows(second), "the second turn: {second:?}");
    assert_eq!(settings_focus(), focus, "the focus stays");

    // Over nothing that scrolls, the wheel is the arrows, first turn too.
    let nothing = find(&ui, None);
    key(InputAction::Up);
    let focus = settings_focus();
    assert!(!over_rows(wheel()));
    assert_eq!(app(|app| app.pointer.at()), Some(nothing));
    assert_ne!(settings_focus(), focus, "the arrows moved the focus");
}

/// A press on nothing, delivered as the platform delivers it, is nothing:
/// no click, the focus where it was, and the next hover and wheel work
/// at once -- not only after another press.
#[test]
fn a_press_on_nothing_leaves_the_application_free() {
    use crate::pointer::{Phase, Scroll, Zone};
    let ui = driven();
    app(|app| app.open(crate::route::Route::Settings));
    let nothing = find(&ui, None);
    let rows = find(&ui, Some(Zone::SettingsRows));
    point(&ui, nothing.0, nothing.1);
    let focus = settings_focus();
    for pressed in [true, false] {
        app(|app| app.pointer_button(pressed));
        crate::platform::deliver_pointer(ui.window());
    }
    assert_eq!(app(|app| app.gesture.phase()), Phase::Idle);
    assert_eq!(settings_focus(), focus, "no click, no focus");
    // The arrow onto the rows: their surface is what it is over again.
    let lanes = point(&ui, rows.0, rows.1);
    assert!(lanes.vertical.is_some_and(|lane| lane.zone == Zone::SettingsRows));
    crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, 120.0));
    assert!(matches!(
        app(|app| app.gesture.phase()),
        Phase::Wheeling { lane, .. } if lane.zone == Zone::SettingsRows
    ));
    assert_eq!(settings_focus(), focus);
}
