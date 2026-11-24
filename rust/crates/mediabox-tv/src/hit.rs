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

    // ------------------------------------------- the same two, Quiet OS
    // Another face, the same targets: what the classic face reaches above,
    // under the same zones and indices, and nothing the classic does not.
    ui.set_layout("quiet".into());
    let reached = panel.reach();
    assert!(has(&reached, "settings-sections", 0) && has(&reached, "settings-sections", 1));
    assert!(has(&reached, "settings-rows", 0) && has(&reached, "settings-rows", 2));
    assert!(!has(&reached, "settings-rows", 1), "a disabled card is no target");
    // Quiet's top left is the way home (the remote's Home); one level back
    // is the remote's Back and the right button.
    assert!(zones(&reached).contains("go-home"), "the way home");
    assert_eq!(
        zones(&reached),
        BTreeSet::from(["go-home", "settings-rows", "settings-sections"]),
        "{reached:?}"
    );
    ui.set_settings_choice_open(true);
    let reached = panel.reach();
    assert!(has(&reached, "settings-choice", 0) && !has(&reached, "settings-choice", 1));
    for under in ["settings-rows", "settings-sections"] {
        assert!(!zones(&reached).contains(under), "{under} is under the list");
    }
    ui.set_settings_choice_open(false);
    ui.set_screen("home".into());
    let tile = |id: &str| crate::AppTile {
        id: id.into(),
        name: id.into(),
        ready: true,
        ..Default::default()
    };
    ui.set_apps(model(vec![tile("media"), tile("livetv"), tile("browser"), tile("settings"), tile("kodi")]));
    let reached = panel.reach();
    assert_eq!(indices(&reached, "quiet-home"), (0..5).collect(), "every application: {reached:?}");
    assert_eq!(zones(&reached), BTreeSet::from(["quiet-home"]), "home is the root: no way back");

    // Five, ten, twenty applications: every row keeps its height (a target
    // a hand can hit, a name that can be read), the column shows as many as
    // fit, the remote's focus is always among them, and a hand moves the
    // column to its other end and no further.
    for count in [5usize, 10, 20] {
        let many: Vec<crate::AppTile> = (0..count)
            .map(|n| crate::AppTile {
                id: format!("app-{n}").into(),
                name: format!("Uygulama {n}").into(),
                ready: true,
                ..Default::default()
            })
            .collect();
        ui.set_apps(model(many));
        ui.global::<PointerPan>().set_pinned(false);
        for focus in [0, count / 2, count - 1] {
            ui.set_focus_col(focus as i32);
            panel.settle_eased();
            let shown = indices(&panel.reach(), "quiet-home");
            assert!(shown.contains(&(focus as i32)), "{count} apps, focus {focus}: {shown:?}");
            // 83 design px a row in 592 of column: seven rows whole, and
            // at most the edges of one more each way.
            assert!(shown.len() >= count.min(7) && shown.len() <= 9, "{count}: {shown:?}");
            let lo = *shown.iter().min().unwrap();
            let hi = *shown.iter().max().unwrap();
            assert_eq!(shown.len() as i32, hi - lo + 1, "one run of rows: {shown:?}");
        }
        // A pointer takes over: dragged a long way up, the last row; a long
        // way down, the first, and no further either way.
        ui.set_focus_col(0);
        panel.settle_eased();
        ui.global::<PointerPan>().set_pinned(true);
        panel.edges.borrow_mut().clear();
        panel.pan("quiet-home", -1, 100 + count as i32, 0.0, -10_000.0);
        let bottom = indices(&panel.reach(), "quiet-home");
        assert!(bottom.contains(&(count as i32 - 1)), "{count}: {bottom:?}");
        assert_eq!(
            bottom.contains(&0),
            count <= 7,
            "{count}: only a column that does not fit scrolls: {bottom:?}"
        );
        if count > 7 {
            assert!(panel.edges.borrow().contains(&("quiet-home".to_string(), -1)));
        }
        panel.pan("quiet-home", -1, 200 + count as i32, 0.0, 10_000.0);
        assert!(indices(&panel.reach(), "quiet-home").contains(&0), "{count}: back to the top");
    }
    ui.global::<PointerPan>().set_pinned(true);
    ui.set_focus_col(0);
    ui.set_apps(model(vec![]));
    let reached = panel.reach();
    assert!(reached.is_empty(), "an empty launcher draws no target: {reached:?}");
    ui.set_apps(model(vec![tile("media"), tile("livetv"), tile("browser"), tile("settings"), tile("kodi")]));

    ui.set_layout("classic".into());
    let reached = panel.reach();
    assert_eq!(indices(&reached, "home"), (0..5).collect(), "and the classic row");

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
/// The test's own place for what an `App` keeps, removed when the test ends.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn driven() -> (MediaBoxWindow, Scratch) {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(slint::PhysicalSize::new(W, H));
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
    }))
    .expect("one platform");
    SHOT_WINDOW.with(|slot| *slot.borrow_mut() = Some(window.clone()));
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
        interface: dir.join("interface-preferences.json"),
        live_kept: crate::LiveKept::default(),
    };
    let app = crate::App::new(&ui, std::time::Instant::now(), false, files);
    crate::APP.with(|slot| *slot.borrow_mut() = Some(Rc::new(RefCell::new(app))));
    crate::wire_pointer(&ui);
    (ui, Scratch(dir))
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
    let (ui, _scratch) = driven();
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
    let (ui, _scratch) = driven();
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

/// Two fingers on a touchpad over the settings rows, moving, then lifted:
/// libinput's end of the sequence reaches the application as its own
/// event and the rows coast on the way the fingers went, the focus where
/// it was. Before, the end was read as no movement and thrown away.
#[test]
fn fingers_lifted_off_a_touchpad_leave_the_rows_coasting() {
    use crate::pointer::{Phase, Scroll, Zone};
    let (ui, _scratch) = driven();
    app(|app| app.open(crate::route::Route::Settings));
    let rows = find(&ui, Some(Zone::SettingsRows));
    point(&ui, rows.0, rows.1);
    let focus = settings_focus();
    for _ in 0..6 {
        crate::platform::hand_scroll(ui.window(), Scroll::of_axes(true, None, Some(30.0)));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(matches!(
        app(|app| app.gesture.phase()),
        Phase::Wheeling { lane, .. } if lane.zone == Zone::SettingsRows
    ));
    crate::platform::hand_scroll(ui.window(), Scroll::of_axes(true, None, Some(0.0)));
    let phase = app(|app| app.gesture.phase());
    assert!(
        matches!(phase, Phase::Coasting { lane, velocity, .. }
            if lane.zone == Zone::SettingsRows && velocity < 0.0),
        "{phase:?}"
    );
    assert!(app(|app| app.coast_timer.running()), "its frames are timed");
    assert_eq!(settings_focus(), focus);
    // A key ends it, as it ends every gesture.
    app(|app| {
        app.remote_key(
            mediabox_core::InputAction::Back,
            true,
            crate::input::Origin::Keyboard,
        )
    });
    assert!(!app(|app| app.gesture.coasting()));
}

/// Quiet OS: a wheel's detent takes the rows over for the focus, so what the
/// fingers left coasting there stops, and its frames with it. Left running,
/// the coast's next frame took the list away from the focus again. A button
/// held down keeps its drag: a detent is not the end of it.
#[test]
fn quiet_wheel_cancels_scroll_inertia_before_reclaiming_focus() {
    use crate::pointer::{Hit, Lane, Phase, Scroll, Zone};
    use crate::screens::settings::Pane;
    use std::time::{Duration, Instant};

    let (ui, _scratch) = driven();
    app(|app| {
        app.layout = crate::layout::Layout::Quiet;
        app.settings.set_layout(app.layout);
        app.open(crate::route::Route::Settings);
        app.pointer_lit = Some((Zone::SettingsRows, 0));

        let lane = Lane::vertical(Zone::SettingsRows);
        let now = Instant::now();
        for step in (0..6).rev() {
            app.gesture.wheel(lane, 30.0, now - Duration::from_millis(step * 10));
        }
        assert!(app.gesture.lift((false, true), true, 1080.0, now));
        app.pointer_coast_start();
        assert!(app.gesture.coasting());
        assert!(app.coast_timer.running());
        assert_eq!(app.settings.pane, Pane::Sections);

        app.pointer_scroll(Scroll::Wheel(0.0, 120.0));
        assert_eq!(app.settings.pane, Pane::Rows);
        assert!(
            !app.gesture.coasting(),
            "a wheel detent moved focus, but the previous pan still owns the next frame"
        );
        assert!(!app.coast_timer.running(), "its frames are timed no more");
    });
    // Frames later, nothing has moved the surface on.
    let pan = || {
        let pan = ui.global::<crate::PointerPan>();
        (pan.get_gesture(), pan.get_dy())
    };
    let after = pan();
    std::thread::sleep(Duration::from_millis(50));
    slint::platform::update_timers_and_animations();
    assert_eq!(pan(), after, "the list stays with the focus");

    // The hand's own drag goes on through a detent.
    app(|app| {
        app.gesture.press((900.0, 600.0), app.pointer_context(), 1080.0);
        app.gesture
            .hit(Hit::Target(Zone::SettingsRows, 0), app.pointer_context());
        app.gesture.motion((900.0, 500.0), Instant::now());
        assert!(matches!(app.gesture.phase(), Phase::Panning { .. }));
        app.pointer_scroll(Scroll::Wheel(0.0, 120.0));
        assert!(matches!(app.gesture.phase(), Phase::Panning { .. }));
    });
}

/// Slint's clock on, and the pointer's targets with it: animations over.
fn settle(ui: &MediaBoxWindow) {
    for _ in 0..6 {
        slint::platform::update_timers_and_animations();
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    slint::platform::update_timers_and_animations();
    crate::platform::deliver_pointer(ui.window());
}

/// The targets of `zone` drawn on the panel, from where the arrow is to the
/// right: what hit testing reaches, so what a list clips away is not here.
/// The arrow is back where it was when this ends.
fn drawn(ui: &MediaBoxWindow, zone: crate::pointer::Zone, at: (f32, f32)) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    for x in (at.0 as usize..W as usize).step_by(STEP * 5) {
        for y in (0..H as usize).step_by(STEP) {
            point(ui, x as f32, y as f32);
            if let Some((lit, index)) = app(|app| app.pointer_lit)
                && lit == zone
            {
                seen.insert(index);
            }
        }
    }
    point(ui, at.0, at.1);
    seen
}

/// The button down at (x, from), and the hand to (x, to) with it.
fn drag(ui: &MediaBoxWindow, x: f32, from: f32, to: f32) {
    point(ui, x, from);
    app(|app| app.pointer_button(true));
    crate::platform::deliver_pointer(ui.window());
    for step in 1..=6 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        point(ui, x, from + (to - from) * step as f32 / 6.0);
        slint::platform::update_timers_and_animations();
    }
}

/// And up.
fn lift(ui: &MediaBoxWindow) {
    app(|app| app.pointer_button(false));
    crate::platform::deliver_pointer(ui.window());
}

/// One list of Quiet OS's settings family under the wheel. A hand takes it
/// away from the focus and leaves it coasting; the first detent ends that,
/// moves the focus a row and shows it, and so does every one after it, each
/// as far as the remote's own key goes. A drag moves the list alone.
fn wheel_walks_the_focus(
    ui: &MediaBoxWindow,
    zone: crate::pointer::Zone,
    last: usize,
    focus: impl Fn() -> usize,
) {
    use crate::pointer::Scroll;
    let at = find(ui, Some(zone));
    let wheel = |detents: i32| {
        crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, f64::from(detents * 120)));
    };
    let shown = |what: &str| {
        settle(ui);
        let seen = drawn(ui, zone, at);
        assert!(seen.contains(&focus()), "{zone:?} {what}: {} of {seen:?}", focus());
    };
    let pan = || ui.global::<PointerPan>().get_dy();
    let start = focus();
    let hand = |from: f32, to: f32| drag(ui, at.0, from, to);
    let drop = || lift(ui);
    // A hand takes the list away until the focus is off the panel.
    hand(at.1 + 300.0, at.1 + 60.0);
    std::thread::sleep(std::time::Duration::from_millis(120));
    drop();
    settle(ui);
    let seen = drawn(ui, zone, at);
    assert!(!seen.contains(&start), "{zone:?} dragged away: {start} of {seen:?}");
    // Let go while moving: the list runs on, the focus stays.
    hand(at.1 + 300.0, at.1 + 40.0);
    drop();
    assert!(
        app(|app| app.gesture.coasting() && app.coast_timer.running()),
        "{zone:?}: the list runs on"
    );
    assert_eq!(focus(), start, "{zone:?}: a hand leaves the focus");
    // A frame of it the surface hears of only with the wheel.
    slint::platform::update_timers_and_animations();
    app(|app| app.pointer_coast(0.016));
    wheel(1);
    assert!(!app(|app| app.gesture.coasting() || app.coast_timer.running()), "{zone:?}");
    assert_eq!(focus(), start + 1, "{zone:?}: a row a detent");
    let after = pan();
    shown("after a coast");
    assert_eq!(pan(), after, "{zone:?}: nothing moves it on");
    wheel(-1);
    assert_eq!(focus(), start, "{zone:?}: and back");
    // Part of a detent moves nothing until the rest of it comes.
    for part in [40.0, 40.0] {
        crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, part));
        assert_eq!(focus(), start, "{zone:?}: part of a detent");
    }
    crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, 40.0));
    assert_eq!(focus(), start + 1, "{zone:?}: the whole of it");
    // Several detents in one report are as many rows, none dropped.
    wheel(2);
    assert_eq!(focus(), (start + 3).min(last), "{zone:?}: each detent counts");
    // To the end, a detent at a time, the focus in view all along.
    for turn in 0..last + 2 {
        wheel(1);
        if turn % 6 == 0 {
            shown("on the way down");
        }
    }
    assert_eq!(focus(), last, "{zone:?}: the end");
    shown("at the end");
    // A held button's drag moves the list alone, and the wheel waits for it.
    hand(at.1 + 60.0, at.1 + 200.0);
    let drag = app(|app| app.gesture.phase());
    assert!(
        matches!(drag, crate::pointer::Phase::Panning { lane, .. } if lane.zone == zone),
        "{zone:?}: {drag:?}"
    );
    wheel(-1);
    assert_eq!(focus(), last, "{zone:?}: the hand has the list");
    assert_eq!(app(|app| app.gesture.phase()), drag);
    std::thread::sleep(std::time::Duration::from_millis(120));
    drop();
    point(ui, at.0, at.1);
    for _ in 0..last + 2 {
        wheel(-1);
    }
    assert_eq!(focus(), 0, "{zone:?}: the top");
    shown("at the top");
}

/// A box with a Wi-Fi and a Bluetooth list longer than the panel.
fn with_radios() {
    app(|app| {
        app.wifi.open();
        app.wifi.known = true;
        app.wifi.present = true;
        app.wifi.powered = true;
        app.wifi.take_networks(
            (0..30)
                .map(|n| crate::screens::wireless::Network {
                    ssid: format!("Ağ {n:02}"),
                    signal: -50,
                    secured: true,
                    heard: true,
                    ..Default::default()
                })
                .collect(),
        );
        let devices: Vec<_> = (0..24)
            .map(|n| {
                serde_json::json!({
                    "address": format!("00:00:00:00:00:{n:02}"), "name": format!("Aygıt {n:02}"),
                    "paired": n < 6, "connected": n == 0,
                })
            })
            .collect();
        app.bt.open();
        app.bt.take_status(&serde_json::json!({
            "present": true, "controller": {"address": "00", "powered": true}, "devices": devices,
        }));
    });
}

fn wear(layout: crate::layout::Layout) {
    app(|app| {
        app.layout = layout;
        app.settings.set_layout(layout);
        app.paint();
    });
}

/// Quiet OS's "Sistem" with thirty cards, the focus on the first.
fn long_settings() {
    wear(crate::layout::Layout::Quiet);
    app(|app| {
        app.open(crate::route::Route::Settings);
        let system = app.settings.groups.iter().position(|g| g.title == "Sistem").expect("Sistem");
        app.settings.point_section(system);
        let row = app.settings.groups[system].rows.iter().find(|r| r.action.is_some()).cloned();
        app.settings.groups[system].rows = vec![row.expect("a card"); 30];
        app.settings.point_row(0);
        app.paint();
    });
}

/// A coast's frame the surface has not been told of when a wheel, the
/// remote or a mouse's Back takes the list over: it is never applied, nor is
/// one after it, and the row the focus is on is on the panel.
#[test]
fn a_frame_not_yet_drawn_never_takes_the_list_from_the_focus() {
    use crate::input::Origin;
    use crate::pointer::{Scroll, Zone};
    use crate::screens::settings::Pane;
    use mediabox_core::InputAction;
    let (ui, _scratch) = driven();
    long_settings();
    let at = find(&ui, Some(Zone::SettingsRows));
    for (input, pane, row) in [
        ("wheel", Pane::Rows, 2),
        ("remote", Pane::Rows, 1),
        ("back", Pane::Sections, 0),
    ] {
        app(|app| {
            app.settings.point_row(0);
            app.paint();
        });
        drag(&ui, at.0, at.1 + 300.0, at.1 + 60.0);
        std::thread::sleep(std::time::Duration::from_millis(120));
        lift(&ui);
        settle(&ui);
        assert!(!drawn(&ui, Zone::SettingsRows, at).contains(&0), "{input}: dragged away");
        drag(&ui, at.0, at.1 + 300.0, at.1 + 40.0);
        lift(&ui);
        slint::platform::update_timers_and_animations();
        app(|app| {
            assert!(app.gesture.coasting() && app.coast_timer.running(), "{input}");
            app.pointer_coast(0.016);
        });
        match input {
            "wheel" => crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, 240.0)),
            "remote" => app(|app| app.remote_key(InputAction::Down, true, Origin::Keyboard)),
            _ => app(|app| app.pointer_back(true)),
        }
        app(|app| {
            assert_eq!((app.settings.pane, app.settings.row_index()), (pane, row), "{input}");
            assert!(!app.gesture.coasting() && !app.coast_timer.running(), "{input}");
            app.pointer_coast(0.016);
        });
        settle(&ui);
        let seen = drawn(&ui, Zone::SettingsRows, at);
        assert!(seen.contains(&row), "{input}: {row} of {seen:?}");
    }
}

/// The rows of a settings category longer than the panel.
#[test]
fn quiet_wheel_walks_the_settings_rows() {
    use crate::pointer::Zone;
    let (ui, _scratch) = driven();
    long_settings();
    wheel_walks_the_focus(&ui, Zone::SettingsRows, 29, || app(|app| app.settings.row_index()));
    // From the column the first detent goes into the rows, the rest on.
    app(|app| {
        app.settings.back();
        app.settings.wheel_rows(3);
        assert_eq!(app.settings.pane, crate::screens::settings::Pane::Rows);
        assert_eq!(app.settings.row_index(), 2);
    });
}

#[test]
fn quiet_wheel_walks_a_long_wifi_list() {
    use crate::pointer::Zone;
    let (ui, _scratch) = driven();
    with_radios();
    wear(crate::layout::Layout::Quiet);
    app(|app| app.open(crate::route::Route::Wifi));
    // The switch, "Diğer ağlar"'s "Yenile", thirty networks, and the two
    // rows under them; headings are stepped over.
    wheel_walks_the_focus(&ui, Zone::WifiRows, 33, || app(|app| app.wifi.index));
    // A network's page and the password are no list: their wheel is the
    // arrows, at the remote's own pace, as it was.
    app(|app| {
        app.wifi.face = crate::screens::wireless::Face::Password;
        app.paint();
        assert_eq!(app.focus_list(), None);
        app.wifi.face = crate::screens::wireless::Face::Details;
        assert_eq!(app.focus_list(), None);
    });
}

#[test]
fn quiet_wheel_walks_the_bluetooth_list() {
    use crate::pointer::Zone;
    let (ui, _scratch) = driven();
    with_radios();
    wear(crate::layout::Layout::Quiet);
    app(|app| app.open(crate::route::Route::Bluetooth));
    wheel_walks_the_focus(&ui, Zone::BtRows, 25, || app(|app| app.bt.index));
}

/// The classic face's radios scroll under the wheel and keep the focus.
#[test]
fn the_classic_wheel_scrolls_the_radios_and_leaves_the_focus() {
    use crate::pointer::{Phase, Scroll, Zone};
    let (ui, _scratch) = driven();
    with_radios();
    for (route, zone) in [
        (crate::route::Route::Wifi, Zone::WifiRows),
        (crate::route::Route::Bluetooth, Zone::BtRows),
    ] {
        app(|app| app.open(route));
        find(&ui, Some(zone));
        crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, 240.0));
        let phase = app(|app| app.gesture.phase());
        assert!(
            matches!(phase, Phase::Wheeling { lane, .. } if lane.zone == zone),
            "{phase:?}"
        );
        assert_eq!(app(|app| (app.wifi.index, app.bt.index)), (0, 0));
    }
}

/// A list open over the settings takes the whole wheel: on it, its focus;
/// beside it, its focus as well; the rows under it, never.
#[test]
fn quiet_wheel_stays_in_an_open_choice() {
    use crate::pointer::{Scroll, Zone};
    let (ui, _scratch) = driven();
    wear(crate::layout::Layout::Quiet);
    let wheel = |detents: i32| {
        std::thread::sleep(std::time::Duration::from_millis(150));
        crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, f64::from(detents * 120)));
    };
    let choice = || app(|app| app.settings.choice_view().map(|view| view.focus));
    let rows = app(|app| {
        app.open(crate::route::Route::Settings);
        let system = app.settings.groups.iter().position(|g| g.title == "Sistem").expect("Sistem");
        app.settings.point_section(system);
        app.settings.point_row(0);
        app.settings.open_layout_choice();
        app.settings.point_choice(0);
        app.paint();
        (app.settings.section, app.settings.row_index(), app.settings.pane)
    });
    let released = || {
        let pan = ui.global::<PointerPan>();
        (pan.get_release(), pan.get_release_surface().to_string())
    };
    let at = find(&ui, Some(Zone::SettingsChoice));
    wheel(1);
    assert_eq!(choice(), Some(1));
    assert_eq!(released().1, "settings-choice", "only the list on top");
    settle(&ui);
    assert!(drawn(&ui, Zone::SettingsChoice, at).contains(&1));
    wheel(3);
    assert_eq!(choice(), Some(crate::layout::Layout::ALL.len() - 1));
    wheel(-3);
    assert_eq!(choice(), Some(0));
    // Off the list, on the ground over the page: the arrows, as it was, and
    // no list given back.
    point(&ui, 4.0, H as f32 - 4.0);
    let before = released();
    wheel(1);
    assert_eq!(choice(), Some(1));
    assert_eq!(released(), before);
    assert_eq!(
        app(|app| (app.settings.section, app.settings.row_index(), app.settings.pane)),
        rows,
        "the rows under it stay"
    );
}

/// An editor's cards and the list one opens, longer than its window: each
/// detent a card, and an option, as the remote's keys have it.
#[test]
fn quiet_wheel_walks_an_editor_and_its_open_list() {
    use crate::pointer::{Scroll, Zone};
    use crate::screens::settings::Page;
    let (ui, _scratch) = driven();
    wear(crate::layout::Layout::Quiet);
    let mut audio = crate::screens::audio::tests::status_for_tests(Default::default());
    let device = audio.devices[0].clone();
    audio.devices = (0..14)
        .map(|n| {
            let mut device = device.clone();
            device.id = format!("usb:{n}");
            device
        })
        .collect();
    let status = serde_json::json!({
        "audio": audio,
        "output": crate::screens::output::tests::status_for_tests(),
    });
    let open = |page: Page| {
        app(|app| {
            app.open(crate::route::Route::Settings);
            while app.settings.back() {}
            app.settings.compose(Some(&status), None, None);
            let (g, r) = (0..app.settings.groups.len())
                .find_map(|g| {
                    let rows = &app.settings.groups[g].rows;
                    rows.iter().position(|row| row.page == Some(page)).map(|r| (g, r))
                })
                .expect("the page's card");
            app.settings.point_section(g);
            app.settings.point_row(r);
            assert!(app.settings.open(page));
            app.paint();
        });
    };
    let lit = |zone: Zone| {
        for y in (STEP / 2..H as usize).step_by(STEP) {
            point(&ui, W as f32 * 0.6, y as f32);
            if app(|app| app.pointer_lit.is_some_and(|(lit, _)| lit == zone)) {
                return;
            }
        }
        panic!("no {zone:?}");
    };
    let wheel = |detents: i32| {
        crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, f64::from(detents * 120)));
    };
    open(Page::Audio);
    lit(Zone::EditorCards);
    let card = || app(|app| app.settings.audio.focused());
    let first = card();
    let mut walked = Vec::new();
    for _ in 0..2 {
        app(|app| app.act_on_settings(crate::actions::Intent::Move(0, 1)));
        walked.push(card());
    }
    for _ in 0..2 {
        app(|app| app.act_on_settings(crate::actions::Intent::Move(0, -1)));
    }
    assert_eq!(card(), first);
    wheel(1);
    assert_eq!(card(), walked[0], "as Down");
    wheel(1);
    assert_eq!(card(), walked[1], "and at once again, none dropped");
    wheel(-2);
    assert_eq!(card(), first, "two in one report");
    app(|app| app.act_on_settings(crate::actions::Intent::Select));
    let option = || {
        app(|app| app.settings.audio.view())
            .picker
            .iter()
            .position(|option| option.focused)
            .unwrap()
    };
    app(|app| app.settings.point_editor(crate::screens::settings::EditorTarget::Picker(0)));
    app(|app| app.paint());
    wheel_walks_the_focus(&ui, Zone::EditorPicker, 14, option);
    assert_eq!(card(), first, "the cards under it stay");

    // The display's sizes on its advanced face, with the focus on them.
    open(Page::Output);
    app(|app| {
        // The last card is the advanced face's.
        for _ in 0..3 {
            app.act_on_settings(crate::actions::Intent::Move(0, 1));
        }
        app.act_on_settings(crate::actions::Intent::Select);
        app.paint();
    });
    let focus = || app(|app| app.settings.output.focus());
    find(&ui, Some(Zone::OutputResolutions));
    let (zone, at, ..) = focus();
    wheel(if at == 0 { 1 } else { -1 });
    assert_eq!((focus().0, focus().1.abs_diff(at)), (zone, 1), "a size a detent");
    // With the focus on the rates, a wheel over the sizes leaves it there.
    if focus().1 == 0 {
        wheel(1);
    }
    app(|app| app.act_on_settings(crate::actions::Intent::Move(1, 0)));
    assert_eq!(app(|app| app.focus_list()), Some(Zone::OutputRates));
    let rates = focus();
    for detents in [1, -1] {
        wheel(detents);
        assert_eq!(focus(), rates, "the focus keeps its column");
    }
}

/// The Wi-Fi scan's spinner is drawn and runs on the animation clock: half
/// an arc cycle (1333 ms) apart, its arc is another. The ring's own turn --
/// 720 degrees every 4444 ms, on the arc's Path now that it is a child of
/// the spinner's box (Slint applies no transform to a component's root) --
/// is not drawn here: the software renderer this test has leaves
/// `rotate` unimplemented (i-slint-renderer-software 1.17, slint #6068),
/// so two frames one arc cycle apart draw alike with or without it. That
/// the turn is applied is the build's to say (no warning), and the
/// panel's (FemtoVG).
#[test]
fn the_wifi_scan_spinner_runs_on_the_animation_clock() {
    use slint::platform::software_renderer::PremultipliedRgbaColor;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(slint::PhysicalSize::new(W, H));
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
    }))
    .expect("one platform");
    let ui = MediaBoxWindow::new().expect("the interface");
    ui.window().set_size(slint::PhysicalSize::new(W, H));
    ui.show().expect("shown");
    ui.set_screen("wifi".into());
    ui.set_wifi_face("list".into());
    ui.set_wifi_known(true);
    ui.set_wifi_present(true);
    ui.set_wifi_rows(model(vec![WifiRow {
        kind: "header".into(),
        label: "Kullanılabilir ağlar".into(),
        on: true,
        bars: -1,
        slot: -1,
        ..Default::default()
    }]));
    let frame = || {
        slint::platform::update_timers_and_animations();
        ui.window().request_redraw();
        let mut pixels = vec![PremultipliedRgbaColor::default(); (W * H) as usize];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, W as usize);
        });
        pixels
    };
    let differ = |a: &[PremultipliedRgbaColor], b: &[PremultipliedRgbaColor]| {
        a.iter()
            .zip(b)
            .filter(|(a, b)| (a.red, a.green, a.blue, a.alpha) != (b.red, b.green, b.blue, b.alpha))
            .count()
    };
    let _ = frame();
    let start = std::time::Instant::now();
    let first = frame();
    std::thread::sleep(std::time::Duration::from_millis(667).saturating_sub(start.elapsed()));
    let half = frame();
    assert!(
        differ(&first, &half) > 0,
        "the spinner's arc is drawn and moves with the animation clock"
    );
    // And it is the spinner that moves: with the scan over, nothing does.
    ui.set_wifi_rows(model(vec![WifiRow {
        kind: "header".into(),
        label: "Kullanılabilir ağlar".into(),
        on: false,
        bars: -1,
        slot: -1,
        ..Default::default()
    }]));
    let still = frame();
    std::thread::sleep(std::time::Duration::from_millis(667));
    assert_eq!(differ(&still, &frame()), 0, "no spinner, nothing moving");
}


/// Quiet OS is chosen where the classic face's settings are, takes the
/// screen at once -- the same screen, the same row, no restart -- and is
/// what the next start comes up in. Its launcher is a column: Down walks it.
#[test]
fn the_layout_is_chosen_in_the_settings_applied_in_place_and_kept() {
    use mediabox_core::InputAction;
    let (ui, scratch) = driven();
    // Paced as a hand presses: the dispatcher takes a repeat of a key this
    // close to the last as one.
    let key = |action| {
        std::thread::sleep(std::time::Duration::from_millis(200));
        app(|app| {
            app.remote_key(action, true, crate::input::Origin::Keyboard);
            app.remote_key(action, false, crate::input::Origin::Keyboard);
        });
    };
    assert_eq!(ui.get_layout(), "classic", "a box that never chose");
    app(|app| app.open(crate::route::Route::Settings));
    let system = app(|app| {
        app.settings
            .groups
            .iter()
            .position(|g| g.title == "Sistem")
            .expect("Sistem")
    });
    app(|app| {
        app.settings.point_section(system);
        app.paint();
    });
    key(InputAction::Right);
    let card = app(|app| app.settings.focused().map(|r| (r.label.clone(), r.value.clone())));
    assert_eq!(card, Some(("Arayüz düzeni".into(), "Klasik MediaBox".into())));
    key(InputAction::Ok);
    assert!(ui.get_settings_choice_open(), "the two faces, as a list");
    key(InputAction::Down);
    key(InputAction::Ok);
    assert!(!ui.get_settings_choice_open());
    assert_eq!(ui.get_layout(), "quiet", "at once");
    assert_eq!(ui.get_screen(), "settings", "on the same screen");
    let place = app(|app| (app.settings.section, app.settings.pane, app.settings.row_index()));
    assert_eq!(place.0, system, "in the same category");
    assert_eq!(
        app(|app| app.settings.focused().map(|r| r.value.clone())),
        Some("Quiet OS".into()),
        "on the same card, which says so"
    );
    // Kept, off the event loop.
    let kept = scratch.0.join("interface-preferences.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while crate::layout::read(&kept) != crate::layout::Layout::Quiet {
        assert!(std::time::Instant::now() < deadline, "never kept");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // And the column of applications walks with Up and Down.
    app(|app| {
        let display = crate::model::DisplayStatus { owner: None, applications: Vec::new() };
        app.home.set_apps(crate::state::app_tiles_from(&display));
        app.go_home();
    });
    let first = app(|app| app.home.column());
    key(InputAction::Down);
    assert_eq!(app(|app| app.home.column()), first + 1);
    key(InputAction::Up);
    assert_eq!(app(|app| app.home.column()), first);
    // Back the other way, from the Quiet face's own settings.
    app(|app| app.open(crate::route::Route::Settings));
    assert_eq!(app(|app| app.settings.focused().map(|r| r.label.clone())), Some("Arayüz düzeni".into()));
    key(InputAction::Ok);
    key(InputAction::Up);
    key(InputAction::Ok);
    assert_eq!(ui.get_layout(), "classic");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while crate::layout::read(&kept) != crate::layout::Layout::Classic {
        assert!(std::time::Instant::now() < deadline, "never kept");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Quiet OS's Motion Lab is a development tool: without
/// MEDIABOX_TV_MOTION_LAB the Menu key on the home screen does what it did
/// (nothing), and the launcher keeps the remote. With it, Menu opens the
/// panel, which then takes the remote -- the launcher's focus stays where it
/// was -- turns its parameters within their bounds, replays, and closes on
/// Back. Nothing it turns outlives the process.
#[test]
fn the_motion_lab_is_inert_unless_asked_for_and_owns_the_remote_while_open() {
    use crate::QuietMotion;
    use mediabox_core::InputAction;
    let (ui, _scratch) = driven();
    let key = |action| {
        std::thread::sleep(std::time::Duration::from_millis(200));
        app(|app| {
            app.remote_key(action, true, crate::input::Origin::Keyboard);
            app.remote_key(action, false, crate::input::Origin::Keyboard);
        });
    };
    let motion = ui.global::<QuietMotion>();
    wear(crate::layout::Layout::Quiet);
    app(|app| {
        let none = crate::model::DisplayStatus {
            owner: None,
            applications: Vec::new(),
        };
        app.home.set_apps(crate::state::app_tiles_from(&none));
        ui.set_apps(slint::ModelRc::from(app.home.app_tiles.clone()));
        app.go_home();
    });
    let first = app(|app| app.home.column());

    key(InputAction::Menu);
    assert!(
        !motion.get_lab_open(),
        "not asked for: Menu means nothing here"
    );
    key(InputAction::Down);
    assert_ne!(
        app(|app| app.home.column()),
        first,
        "the launcher kept the remote"
    );
    key(InputAction::Up);

    app(|app| app.lab = crate::motion_lab::Lab::enabled());
    key(InputAction::Menu);
    assert!(motion.get_lab_open(), "Menu opens it");
    assert_eq!(ui.get_screen(), "home");
    key(InputAction::Down);
    assert_eq!(motion.get_lab_row(), 1, "Down chooses the next parameter");
    assert_eq!(
        app(|app| app.home.column()),
        first,
        "the launcher did not move"
    );
    key(InputAction::Up);
    for _ in 0..40 {
        key(InputAction::Right);
    }
    assert_eq!(motion.get_pace(), 3.0, "turned no further than its bound");
    key(InputAction::Up);
    assert_eq!(motion.get_lab_row(), 7, "the rows wrap");
    let replay = motion.get_replay();
    key(InputAction::Ok);
    assert_eq!(motion.get_replay(), replay + 1, "Ok on its row replays");
    assert_eq!(ui.get_screen(), "home", "and opens nothing");
    key(InputAction::Up);
    key(InputAction::Left);
    assert!(
        motion.get_reduced() && !motion.get_on(),
        "reduced motion, from the lab"
    );
    key(InputAction::Back);
    assert!(!motion.get_lab_open(), "Back closes it");
    key(InputAction::Down);
    assert_ne!(
        app(|app| app.home.column()),
        first,
        "the launcher has the remote again"
    );
}

/// The interface is told which screen a screen replaced, with the screen
/// itself, so Quiet OS can carry the settings' emblem between the home
/// screen and the settings in both directions.
#[test]
fn a_screen_comes_up_knowing_the_one_it_replaced() {
    let (ui, _scratch) = driven();
    wear(crate::layout::Layout::Quiet);
    app(|app| app.go_home());
    app(|app| app.open(crate::route::Route::Settings));
    assert_eq!(
        (ui.get_screen().as_str(), ui.get_came_from().as_str()),
        ("settings", "home")
    );
    app(|app| app.go_home());
    assert_eq!(
        (ui.get_screen().as_str(), ui.get_came_from().as_str()),
        ("home", "settings")
    );
    // A repaint of the same screen is not a change of screen.
    app(|app| app.paint());
    assert_eq!(ui.get_came_from(), "settings");
}

/// Quiet OS at rest asks for no frames when the control plane is polled
/// again: the launcher's applications and the settings' rows arrive as new
/// models with the same contents, and nothing that moves may take that for
/// a change (measured on the Ultra: a bound colour replayed its 900 ms on
/// every poll, ~12 frames a second at rest).
#[test]
fn quiet_rests_when_the_same_data_is_polled_again() {
    use slint::platform::software_renderer::PremultipliedRgbaColor;
    let (ui, _scratch) = driven();
    wear(crate::layout::Layout::Quiet);
    let frame = || {
        slint::platform::update_timers_and_animations();
        SHOT_WINDOW.with(|slot| {
            let slot = slot.borrow();
            let window = slot.as_ref().expect("the software window");
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                let mut pixels = vec![PremultipliedRgbaColor::default(); (W * H) as usize];
                renderer.render(&mut pixels, W as usize);
            });
        });
    };
    let settle = || {
        for _ in 0..4 {
            frame();
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        frame();
    };
    let none = crate::model::DisplayStatus {
        owner: None,
        applications: Vec::new(),
    };
    let poll = || {
        app(|app| {
            app.home.set_apps(crate::state::app_tiles_from(&none));
            ui.set_apps(slint::ModelRc::from(app.home.app_tiles.clone()));
            app.paint();
        });
        frame();
        frame();
    };
    poll();
    app(|app| {
        app.go_home();
        app.home.point_at(3);
        app.paint();
    });
    settle();
    assert!(!ui.window().has_active_animations(), "the home screen came to rest");
    poll();
    assert!(!ui.window().has_active_animations(), "home: the same applications again");

    app(|app| {
        app.open(crate::route::Route::Settings);
        let system = app.settings.groups.iter().position(|g| g.title == "Sistem").unwrap();
        app.settings.point_section(system);
        app.settings.point_row(0);
        app.paint();
    });
    settle();
    assert!(!ui.window().has_active_animations(), "the settings came to rest");
    poll();
    assert!(!ui.window().has_active_animations(), "settings: the same rows again");
}

/// Pictures of both faces at the panel sizes the appliance drives, for a
/// person to look at: 1080p (scale 1), 1440p and 4K (scale 2, as
/// `platform::scale_factor` chooses). Only with MEDIABOX_TV_SHOTS set to a
/// directory; the software renderer draws them, so a rotation or a
/// GPU-only effect is not in them.
#[test]
#[ignore]
fn shots() {
    use slint::platform::software_renderer::PremultipliedRgbaColor;
    let Some(out) = std::env::var_os("MEDIABOX_TV_SHOTS").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let (ui, _scratch) = driven();
    let display = crate::model::DisplayStatus {
        owner: None,
        applications: serde_json::from_value(serde_json::json!([
            {"id": "kodi", "name": "KODI", "summary": "Donanım kod çözücüyle oynatma", "installed": true}
        ]))
        .unwrap(),
    };
    let status = serde_json::json!({
        "version": "3.0.0", "hostname": "mediabox", "uptime_seconds": 7260,
        "leds": {"mode": "off"},
        "fan": {
            "available": true, "temperature_c": 52.4, "pwm": 50, "pwm_percent": 19.6,
            "rpm_available": false, "control_backend": "kernel-pwm-fan",
            "curve": mediabox_core::FanCurve::balanced(), "pending_reboot": false,
            "board_fix": "active", "boot_config_ready": true,
        },
        "output": crate::screens::output::tests::status_for_tests(),
        "audio": crate::screens::audio::tests::status_for_tests(Default::default()),
    });
    let vitals = || crate::VitalsModel {
        time: "13:34".into(),
        cpu_text: "%1".into(),
        memory_text: "%5".into(),
        storage_text: "5.5 / 56.7 GB".into(),
        storage_free: "51.2 GB".into(),
        network: "Ethernet".into(),
        temperature: "56 °C".into(),
        machine: "Orange Pi 5 Plus".into(),
        eth_up: true,
        ..Default::default()
    };
    let window = ui.window();
    for (w, h, scale, name) in [(1920u32, 1080u32, 1.0f32, "1080p"), (2560, 1440, 2.0, "1440p"), (3840, 2160, 2.0, "4k")] {
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        window.set_size(slint::PhysicalSize::new(w, h));
        // Long enough for Quiet OS's slowest move (the far plane, 760 ms) to
        // come to rest: these are pictures of where things settle.
        let settle = std::time::Duration::from_millis(900);
        // A screen's conditional component is built by the first frame
        // that draws it, and its entrance starts there: one frame first,
        // then the wait.
        let prime = || {
            let mut pixels = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
            SHOT_WINDOW.with(|slot| {
                let slot = slot.borrow();
                let window = slot.as_ref().expect("the software window");
                window.request_redraw();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, w as usize);
                })
            })
        };
        let shoot = |file: String| {
            ui.set_vitals(vitals());
            slint::platform::update_timers_and_animations();
            prime();
            std::thread::sleep(settle);
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            let mut pixels = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
            let rendered = SHOT_WINDOW.with(|slot| {
                let slot = slot.borrow();
                let window = slot.as_ref().expect("the software window");
                window.request_redraw();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, w as usize);
                })
            });
            assert!(rendered, "a frame");
            let rgb: Vec<u8> = pixels.iter().flat_map(|p| [p.red, p.green, p.blue]).collect();
            image::RgbImage::from_raw(w, h, rgb).unwrap().save(out.join(file)).unwrap();
        };
        for layout in [crate::layout::Layout::Classic, crate::layout::Layout::Quiet] {
            app(|app| {
                app.layout = layout;
                app.settings.set_layout(layout);
                app.home.set_apps(crate::state::app_tiles_from(&display));
                ui.set_apps(slint::ModelRc::from(app.home.app_tiles.clone()));
                app.settings.compose(Some(&status), None, None);
                app.go_home();
                app.home.point_at(2);
                app.paint();
            });
            shoot(format!("{name}-{}-home.png", layout.name()));
            app(|app| {
                app.home.point_at(0);
                app.paint();
            });
            shoot(format!("{name}-{}-home-media.png", layout.name()));
            // Quiet OS: every application's own scene, at rest.
            if layout == crate::layout::Layout::Quiet {
                for (at, id) in [(1, "livetv"), (3, "settings"), (4, "kodi")] {
                    app(|app| {
                        app.home.point_at(at);
                        app.paint();
                    });
                    shoot(format!("{name}-quiet-home-{id}.png"));
                }
            }
            app(|app| {
                app.open(crate::route::Route::Settings);
                let system = app.settings.groups.iter().position(|g| g.title == "Sistem").unwrap();
                app.settings.point_section(system);
                app.settings.point_row(0);
                app.paint();
            });
            shoot(format!("{name}-{}-settings.png", layout.name()));
            app(|app| {
                app.settings.open_layout_choice();
                app.paint();
            });
            shoot(format!("{name}-{}-settings-choice.png", layout.name()));
            app(|app| {
                app.settings.back();
                let links = app.settings.groups.iter().position(|g| g.title == "Bağlantılar").unwrap();
                app.settings.point_section(links);
                app.paint();
            });
            shoot(format!("{name}-{}-settings-list.png", layout.name()));

            // The editors, inside each face's frame: their panes are the
            // classic ones on both (`QuietSettings` places them).
            for (page, file) in [
                (crate::screens::settings::Page::Output, "editor-output"),
                (crate::screens::settings::Page::Cooling, "editor-cooling"),
                (crate::screens::settings::Page::Audio, "editor-audio"),
            ] {
                let opened = app(|app| {
                    app.open(crate::route::Route::Settings);
                    while app.settings.back() {}
                    let mut opened = false;
                    for g in 0..app.settings.groups.len() {
                        app.settings.point_section(g);
                        if let Some(r) = app.settings.rows().iter().position(|row| row.page == Some(page)) {
                            app.settings.point_row(r);
                            opened = app.settings.open(page);
                            break;
                        }
                    }
                    app.paint();
                    opened
                });
                assert!(opened, "{file}");
                shoot(format!("{name}-{}-{file}.png", layout.name()));
                app(|app| {
                    while app.settings.back() {}
                    app.paint();
                });
            }
        }

        // Quiet OS at its edges: a long name and summary, a long network
        // name, twenty applications, none at all and no network.
        let long = crate::model::DisplayStatus {
            owner: None,
            applications: serde_json::from_value(serde_json::json!([
                {"id": "kodi", "name": "KODI", "summary": "Donanım kod çözücüyle oynatma", "installed": true},
                {"id": "longname", "name": "Çok Uzun Adlı Bir Uygulama Örneği Gerçekten Uzun",
                 "summary": "Bu açıklama, bir uygulamanın kendini anlatırken ne kadar uzun bir cümle kurabileceğini göstermek için yazıldı; üç satırı aşan metnin ekranda nasıl durduğunu gösterir.",
                 "installed": true}
            ]))
            .unwrap(),
        };
        let many = crate::model::DisplayStatus {
            owner: None,
            applications: serde_json::from_value(serde_json::Value::Array(
                (1..=16)
                    .map(|n| serde_json::json!({"id": format!("app{n}"), "name": format!("Uygulama {n}"), "summary": "", "installed": true}))
                    .collect(),
            ))
            .unwrap(),
        };
        let edge_vitals = |wifi: bool, online: bool| crate::VitalsModel {
            time: "13:34".into(),
            cpu_text: "%1".into(),
            memory_text: "%5".into(),
            storage_text: "5.5 / 56.7 GB".into(),
            storage_free: "51.2 GB".into(),
            network: if online { "Bağlı" } else { "Bağlı değil" }.into(),
            temperature: "—".into(),
            machine: "Orange Pi 5 Plus".into(),
            wifi_up: online && wifi,
            wifi_ssid: "Çok-Uzun-Kablosuz-Ağ-Adı-5GHz-Misafir-Katı-Ofis".into(),
            eth_up: online && !wifi,
            ..Default::default()
        };
        let edge_shoot = |file: String, vitals: crate::VitalsModel| {
            slint::platform::update_timers_and_animations();
            ui.set_vitals(vitals);
            prime();
            std::thread::sleep(settle);
            slint::platform::update_timers_and_animations();
            let mut pixels = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
            let rendered = SHOT_WINDOW.with(|slot| {
                let slot = slot.borrow();
                let window = slot.as_ref().expect("the software window");
                window.request_redraw();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, w as usize);
                })
            });
            assert!(rendered, "a frame");
            let rgb: Vec<u8> = pixels.iter().flat_map(|p| [p.red, p.green, p.blue]).collect();
            image::RgbImage::from_raw(w, h, rgb).unwrap().save(out.join(file)).unwrap();
        };
        let home_with = |display: &crate::model::DisplayStatus, focus: usize| {
            app(|app| {
                app.layout = crate::layout::Layout::Quiet;
                app.settings.set_layout(crate::layout::Layout::Quiet);
                app.home.set_apps(crate::state::app_tiles_from(display));
                ui.set_apps(slint::ModelRc::from(app.home.app_tiles.clone()));
                app.go_home();
                app.home.point_at(focus);
                app.paint();
            });
        };
        home_with(&long, 5);
        edge_shoot(format!("{name}-quiet-edge-long.png"), edge_vitals(true, true));
        home_with(&many, 17);
        edge_shoot(format!("{name}-quiet-edge-many.png"), edge_vitals(false, true));
        home_with(&crate::model::DisplayStatus { owner: None, applications: Vec::new() }, 0);
        app(|app| {
            app.home.set_apps(Vec::new());
            ui.set_apps(slint::ModelRc::from(app.home.app_tiles.clone()));
            app.paint();
        });
        edge_shoot(format!("{name}-quiet-edge-empty.png"), edge_vitals(false, false));
    }
}

/// One pass of the loop, at once: what changed is told, nothing waits.
fn step(ui: &MediaBoxWindow) {
    for _ in 0..2 {
        ui.window().dispatch_event(WindowEvent::PointerExited);
        slint::platform::update_timers_and_animations();
    }
}

/// The apps of a launcher `count` long, the first five the product's own.
fn launcher(count: usize) -> Vec<crate::AppTile> {
    (0..count)
        .map(|n| crate::AppTile {
            id: ["media", "livetv", "browser", "settings", "kodi"].get(n).map_or(format!("app-{n}"), |id| id.to_string()).into(),
            name: format!("Uygulama {n}").into(),
            ready: true,
            ..Default::default()
        })
        .collect()
}

/// How many rows' scenes the home screen has built.
fn scenes(ui: &MediaBoxWindow, count: i32) -> usize {
    let cam = ui.get_quiet_camera();
    (0..count).filter(|i| ui.global::<crate::QuietReach>().invoke_scene(cam.clone(), *i)).count()
}

/// Quiet OS's camera: one scene at rest; a held key, a reversal and a leap
/// keep the scene close behind the focus and never build more than a few;
/// a launcher that changed under the focus moves nothing; reduced motion
/// lands what was moving; the Motion Lab replays the last move, its way.
#[test]
fn the_quiet_camera_keeps_up_with_the_focus() {
    use crate::QuietMotion;
    let (ui, _scratch) = driven();
    wear(crate::layout::Layout::Quiet);
    let apps = Rc::new(VecModel::from(launcher(20)));
    app(|app| app.go_home());
    ui.set_apps(ModelRc::from(apps.clone()));
    ui.set_focus_col(0);
    let tick = |ms: u64| {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        step(&ui);
    };
    tick(10);
    tick(900);
    let cam = || ui.get_quiet_camera();
    assert_eq!((cam().row, cam().cam), (0, 0.0));
    assert_eq!(scenes(&ui, 20), 1, "at rest, the chosen scene alone");

    // A held key: eight rows, 80 ms apart.
    for row in 1..=8 {
        ui.set_focus_col(row);
        step(&ui);
        for _ in 0..4 {
            tick(20);
            assert!((cam().cam - row as f32).abs() < 2.2, "row {row}: the scene is at {}", cam().cam);
            assert!((cam().cam_type - row as f32).abs() < 1.5, "row {row}: the name is at {}", cam().cam_type);
            assert!(scenes(&ui, 20) <= 4, "row {row}: {} scenes", scenes(&ui, 20));
        }
    }
    tick(420);
    assert_eq!(cam().cam, 8.0, "it has caught up as soon as the key is let go");
    // Turned round half way: from where it is, not from where it was going.
    ui.set_focus_col(9);
    step(&ui);
    tick(200);
    let half = cam().cam;
    assert!(half > 8.0 && half < 9.0, "{half}");
    ui.set_focus_col(8);
    step(&ui);
    assert!((cam().cam - half).abs() < 0.1, "{half} -> {}", cam().cam);
    tick(900);
    assert_eq!((cam().cam, cam().cam_far, scenes(&ui, 20)), (8.0, 8.0, 1));
    // A leap: not everything between.
    ui.set_focus_col(19);
    step(&ui);
    for _ in 0..12 {
        tick(25);
        assert!(scenes(&ui, 20) <= 3, "{} scenes", scenes(&ui, 20));
    }
    tick(900);

    // The same application under another row: the camera is simply there.
    let mut list = launcher(20);
    list.rotate_right(3);
    apps.set_vec(list);
    ui.set_focus_col(2);
    step(&ui);
    assert_eq!((cam().row, cam().cam, cam().cam_type), (2, 2.0, 2.0), "nothing moved for the person");
    assert_eq!(scenes(&ui, 20), 1);
    // Removed from under the focus: another application, and one travels.
    apps.remove(2);
    step(&ui);
    tick(900);
    assert_eq!((cam().row, scenes(&ui, 19)), (2, 1));

    // The lab's replay: the last move, the way it went.
    let replay = || {
        let motion = ui.global::<QuietMotion>();
        motion.set_replay(motion.get_replay() + 1);
        step(&ui);
    };
    for (from, to) in [(2, 3), (3, 2), (2, 1)] {
        ui.set_focus_col(to);
        step(&ui);
        tick(900);
        replay();
        assert!((cam().cam - from as f32).abs() < 0.2, "{from}->{to}: replayed from {}", cam().cam);
        tick(900);
        assert_eq!(cam().cam, to as f32);
    }
    // Reduced motion, half way: there, now.
    ui.set_focus_col(2);
    step(&ui);
    tick(120);
    assert_ne!(cam().cam, 2.0);
    ui.global::<QuietMotion>().set_reduced(true);
    step(&ui);
    assert_eq!((cam().cam, cam().cam_far, cam().cam_late), (2.0, 2.0, 2.0));
    ui.global::<QuietMotion>().set_reduced(false);
}

/// The settings' emblem is one thing in two places: opened and left half way
/// through its flight, it turns round where it is -- position, size and
/// strength are all read from the flight -- whatever the screens are.
#[test]
fn the_settings_emblem_turns_round_where_it_is() {
    let (ui, _scratch) = driven();
    wear(crate::layout::Layout::Quiet);
    let none = crate::model::DisplayStatus {
        owner: None,
        applications: Vec::new(),
    };
    app(|app| {
        app.home.set_apps(crate::state::app_tiles_from(&none));
        ui.set_apps(ModelRc::from(app.home.app_tiles.clone()));
        app.go_home();
        app.home.point_at(3);
        app.paint();
    });
    let tick = |ms: u64| {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        step(&ui);
    };
    let flight = || ui.get_quiet_camera().flight;
    // Where it was is where it is, but for what it moved since it was read:
    // a busy machine may take its time between the two.
    let taken_up = |was: f32, read: std::time::Instant, now: f32| {
        let since = read.elapsed().as_secs_f32();
        assert!((now - was).abs() < 0.12 + since * 10.0, "left at {was}, taken up at {now} after {since} s");
    };
    tick(10);
    tick(900);
    assert_eq!(flight(), 0.0, "home");
    for _ in 0..3 {
        app(|app| app.open(crate::route::Route::Settings));
        step(&ui);
        tick(200);
        let (out, read) = (flight(), std::time::Instant::now());
        assert!(out > 0.0 && out < 1.0, "on its way: {out}");
        app(|app| app.go_home());
        step(&ui);
        taken_up(out, read, flight());
        tick(120);
        let (back, read) = (flight(), std::time::Instant::now());
        assert!(back < out, "on its way back");
        app(|app| app.open(crate::route::Route::Settings));
        step(&ui);
        taken_up(back, read, flight());
        tick(700);
        assert_eq!(flight(), 1.0);
        app(|app| app.go_home());
        step(&ui);
        tick(700);
        assert_eq!(flight(), 0.0);
    }
    // Ok before the camera has arrived: the emblem owes what it had to go.
    app(|app| {
        app.home.point_at(2);
        app.paint();
    });
    step(&ui);
    tick(900);
    app(|app| {
        app.home.point_at(3);
        app.paint();
    });
    step(&ui);
    tick(150);
    let (owed, read) = (3.0 - ui.get_quiet_camera().cam, std::time::Instant::now());
    assert!(owed > 0.05, "{owed}");
    app(|app| app.open(crate::route::Route::Settings));
    step(&ui);
    taken_up(owed, read, 3.0 - ui.get_quiet_camera().cam);
}

/// A word is legible all through a change: the chosen one before it is half
/// way, and the one it replaces gone by then.
#[test]
fn quiet_words_stay_legible_while_they_change() {
    let (ui, _scratch) = driven();
    let motion = ui.global::<crate::QuietMotion>();
    for step in 0..=100 {
        let d = step as f32 / 100.0;
        let (coming, leaving) = (motion.invoke_word(d, 1.0), motion.invoke_word(d - 1.0, 0.0));
        assert!(d > 0.7 || coming >= 0.5, "{d}: {coming}");
        assert!(d > 0.58 || leaving == 0.0, "{d}: {leaving} under {coming}");
        assert!(coming + motion.invoke_word(d - 1.0, 1.0) >= 0.5, "{d}: the first frames, before the focus is told apart");
    }
}

/// The family at rest asks for no frame when the same data is painted again,
/// whatever is drawn on it (see `quiet_rests_when_the_same_data_is_polled_again`).
#[test]
fn the_quiet_family_rests_when_it_is_painted_again() {
    use slint::platform::software_renderer::PremultipliedRgbaColor;
    let (ui, _scratch) = driven();
    wear(crate::layout::Layout::Quiet);
    // A panel small enough to draw often.
    ui.window().set_size(slint::PhysicalSize::new(480, 270));
    ui.window().dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 0.25 });
    let frame = || {
        slint::platform::update_timers_and_animations();
        SHOT_WINDOW.with(|slot| {
            let slot = slot.borrow();
            let window = slot.as_ref().expect("the software window");
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                let mut pixels = vec![PremultipliedRgbaColor::default(); 480 * 270];
                renderer.render(&mut pixels, 480);
            });
        });
    };
    for name in FAMILY {
        // A search is the one thing that moves by itself.
        if *name == "bluetooth-scan" {
            continue;
        }
        family_show(&ui, name);
        for _ in 0..5 {
            frame();
            std::thread::sleep(std::time::Duration::from_millis(220));
        }
        frame();
        assert!(!ui.window().has_active_animations(), "{name}: came to rest");
        app(|app| app.paint());
        ui.set_vitals(ui.get_vitals());
        frame();
        frame();
        assert!(!ui.window().has_active_animations(), "{name}: painted again");
    }
}

/// Quiet OS's editors and radios are other pictures of the same controls:
/// every target the classic face reaches, under the same zone and index, and
/// nothing more but its own way home and its column.
#[test]
fn both_faces_of_the_family_reach_the_same_controls() {
    let (ui, _scratch) = driven();
    let log: Rc<RefCell<Reached>> = Rc::default();
    // What a press reaches, and nothing of what it would do.
    let hub = ui.global::<PointerHub>();
    let pressed = log.clone();
    hub.on_press(move |zone, index, down| {
        if down {
            pressed.borrow_mut().insert((zone.to_string(), index));
        }
    });
    hub.on_hover(|_, _, _| {});
    hub.on_press_area(|_, _| {});
    hub.on_area(|_, _, _| {});
    hub.set_shown(true);
    ui.global::<PointerPan>().set_pinned(false);
    let walk = || {
        log.borrow_mut().clear();
        let window = ui.window();
        window.dispatch_event(WindowEvent::PointerExited);
        for y in (STEP / 2..H as usize).step_by(12) {
            for x in (STEP / 2..W as usize).step_by(26) {
                let position = LogicalPosition::new(x as f32, y as f32);
                let button = PointerEventButton::Left;
                window.dispatch_event(WindowEvent::PointerMoved { position });
                window.dispatch_event(WindowEvent::PointerPressed { position, button });
                window.dispatch_event(WindowEvent::PointerReleased { position, button });
            }
        }
        window.dispatch_event(WindowEvent::PointerExited);
        log.borrow()
            .iter()
            .filter(|(zone, _)| !["back", "go-home", "settings-sections"].contains(&zone.as_str()))
            .cloned()
            .collect::<Reached>()
    };
    for name in [
        "output", "output-picker", "output-advanced", "output-trial", "audio", "audio-picker", "ethernet",
        "ethernet-static", "ethernet-entry", "cooling", "cooling-prompt", "wifi-details", "wifi-add", "account",
    ] {
        let mut faces = Vec::new();
        for layout in [crate::layout::Layout::Classic, crate::layout::Layout::Quiet] {
            wear(layout);
            family_show(&ui, name);
            settle_panel(&ui);
            faces.push(walk());
        }
        assert!(!faces[0].is_empty(), "{name}");
        assert_eq!(faces[0], faces[1], "{name}: classic, quiet");
    }
}

fn settle_panel(ui: &MediaBoxWindow) {
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        settle(ui);
    }
}

/// The Motion Lab is a modal: under it the launcher is not pointed at, the
/// wheel walks its rows, and a click on its ground is Back.
#[test]
fn the_motion_lab_keeps_a_pointer_to_itself() {
    use crate::QuietMotion;
    use crate::pointer::{Scroll, Zone};
    let (ui, _scratch) = driven();
    wear(crate::layout::Layout::Quiet);
    let none = crate::model::DisplayStatus {
        owner: None,
        applications: Vec::new(),
    };
    app(|app| {
        app.home.set_apps(crate::state::app_tiles_from(&none));
        ui.set_apps(ModelRc::from(app.home.app_tiles.clone()));
        app.go_home();
        app.lab = crate::motion_lab::Lab::enabled();
        app.options();
    });
    let motion = ui.global::<QuietMotion>();
    assert!(motion.get_lab_open());
    settle(&ui);
    let (focus, pace) = (app(|app| app.home.column()), motion.get_pace());
    // Over a row of the launcher, beside the panel.
    point(&ui, 400.0, 300.0);
    assert!(app(|app| app.pointer_lit).is_none_or(|(zone, _)| zone == Zone::Shield), "{:?}", app(|app| app.pointer_lit));
    crate::platform::hand_scroll(ui.window(), Scroll::Wheel(0.0, 120.0));
    assert_eq!((motion.get_lab_row(), motion.get_pace()), (1, pace), "the wheel walks the rows");
    assert_eq!(app(|app| app.home.column()), focus, "and not the launcher");
    // On the panel itself: nothing.
    let click = |x: f32, y: f32| {
        point(&ui, x, y);
        let position = LogicalPosition::new(x, y);
        let button = PointerEventButton::Left;
        ui.window().dispatch_event(WindowEvent::PointerPressed { position, button });
        ui.window().dispatch_event(WindowEvent::PointerReleased { position, button });
        crate::platform::deliver_pointer(ui.window());
        settle(&ui);
    };
    click(300.0, 800.0);
    assert!(motion.get_lab_open(), "a click on the panel is nothing");
    assert_eq!((ui.get_screen().as_str(), app(|app| app.home.column())), ("home", focus));
}

/// No element of Quiet OS both fades by `opacity` and has a `transform-*`:
/// Slint wraps it as `Opacity { Transform { leaf } }`, which is an offscreen
/// layer per element per frame (ui/quiet/materials.slint). Five of them were
/// three quarters of a category step's first frame.
#[test]
fn nothing_in_quiet_fades_and_turns_at_once() {
    for (file, source) in [
        ("controls", include_str!("../ui/quiet/controls.slint")),
        ("home", include_str!("../ui/quiet/home.slint")),
        ("instruments", include_str!("../ui/quiet/instruments.slint")),
        ("lab", include_str!("../ui/quiet/lab.slint")),
        ("materials", include_str!("../ui/quiet/materials.slint")),
        ("scenes", include_str!("../ui/quiet/scenes.slint")),
        ("settings", include_str!("../ui/quiet/settings.slint")),
    ] {
        // What each open element has said of itself: (its line, fades, turns).
        let mut open: Vec<(usize, bool, bool)> = Vec::new();
        for (at, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            let said = code.trim_start();
            if let Some(element) = open.last_mut() {
                element.1 |= said.starts_with("opacity:");
                element.2 |= said.starts_with("transform-rotation:") || said.starts_with("transform-scale");
                assert!(!(element.1 && element.2), "quiet/{file}.slint:{}: fades and turns", element.0);
            }
            for c in code.chars() {
                match c {
                    '{' => open.push((at + 1, false, false)),
                    '}' => {
                        open.pop();
                    }
                    _ => {}
                }
            }
        }
        assert!(open.is_empty(), "{file}");
    }
}

/// Every screen of the settings family, in the states a person meets them
/// in, for the tests that walk the whole family under both faces.
const FAMILY: &[&str] = &[
    "output", "output-draft", "output-picker", "output-advanced", "output-trial", "output-edid",
    "audio", "audio-muted", "audio-picker",
    "ethernet", "ethernet-static", "ethernet-picker", "ethernet-entry", "ethernet-trial",
    "cooling", "cooling-prompt", "cooling-info",
    "wifi", "wifi-details", "wifi-password", "wifi-add", "wifi-off",
    "bluetooth", "bluetooth-scan", "account", "cec", "system",
    "diagnostics-0", "diagnostics-1", "diagnostics-3",
];

/// What the control plane would have said, for `FAMILY`.
fn family_status() -> serde_json::Value {
    serde_json::json!({
        "version": "3.0.0", "hostname": "mediabox", "uptime_seconds": 7260, "kernel": "6.1.115",
        "leds": {"mode": "off"},
        "fan": {
            "available": true, "board": "Orange Pi 5 Plus", "temperature_c": 52.4, "pwm": 68, "pwm_percent": 26.7,
            "rpm_available": false, "control_backend": "kernel-pwm-fan",
            "curve": mediabox_core::FanCurve::balanced(), "pending_reboot": false,
            "board_fix": "active", "boot_config_ready": true,
        },
        "output": crate::screens::output::tests::status_for_tests(),
        "audio": crate::screens::audio::tests::status_for_tests(Default::default()),
        "ethernet": {"ports": [
            {"name": "end0", "mac": "c0:74:2b:00:00:01", "carrier": true,
             "addresses": ["192.168.2.50/24"], "gateway": "192.168.2.1", "config": {"mode": "dhcp"}},
            {"name": "end1", "mac": "c0:74:2b:00:00:02", "carrier": false, "addresses": [], "config": {"mode": "dhcp"}},
        ]},
        "cec": mediabox_core::CecStatus::default(),
    })
}

fn family_diagnostics() -> serde_json::Value {
    serde_json::json!({
        "machine": "Orange Pi 5 Plus · RK3588",
        "cpu": {"count": 8, "usage": 0.1975, "load": {"one": 0.78, "five": 0.31, "fifteen": 0.27}},
        "memory": {"usedBytes": 1099964416u64, "totalBytes": 16734621696u64},
        "storage": [{"path": "/", "usedBytes": 6336831488u64, "totalBytes": 60933312512u64},
                    {"path": "/var/tmp", "usedBytes": 6336831488u64, "totalBytes": 60933312512u64}],
        "temperatures": [
            {"celsius": 44.4, "name": "bigcore0-thermal"}, {"celsius": 45.3, "name": "bigcore1-thermal"},
            {"celsius": 43.5, "name": "center-thermal"}, {"celsius": 61.5, "name": "gpu-thermal"},
            {"celsius": 54.0, "name": "iwlwifi_1"}, {"celsius": 45.3, "name": "littlecore-thermal"},
            {"celsius": 44.4, "name": "npu-thermal"}, {"celsius": 46.4, "name": "soc-thermal"}],
        "network": {"default": {"interface": "end0", "address": "192.168.2.50", "gateway": "192.168.2.1"},
                    "interfaces": [{"name": "end0", "state": "up", "wireless": false, "address": "192.168.2.50"}]},
        "wireless": {"wifi": {"ssid": "Ev", "signal": -60, "address": "192.168.2.51"}},
    })
}

/// Puts one of `FAMILY` on the panel, as Rust would have painted it.
fn family_show(ui: &MediaBoxWindow, name: &str) {
    use crate::screens::settings::Page;
    let status = family_status();
    let diagnostics = family_diagnostics();
    let page = |page: Page, nth: usize| {
        app(|app| {
            app.open(crate::route::Route::Settings);
            while app.settings.back() {}
            app.settings.compose(Some(&status), None, None);
            let (g, r) = (0..app.settings.groups.len())
                .flat_map(|g| {
                    let rows = &app.settings.groups[g].rows;
                    rows.iter().enumerate().filter(|(_, row)| row.page == Some(page)).map(move |(r, _)| (g, r)).collect::<Vec<_>>()
                })
                .nth(nth)
                .unwrap_or_else(|| panic!("{page:?}"));
            app.settings.point_section(g);
            app.settings.point_row(r);
            assert!(app.settings.open(page), "{page:?}");
            app.paint();
        });
    };
    app(|app| {
        app.wifi.face = crate::screens::wireless::Face::List;
    });
    ui.set_settings_choice_open(false);
    match name {
        n if n.starts_with("output") => {
            page(Page::Output, 0);
            match n {
                "output-draft" => app(|app| {
                    use crate::actions::Intent;
                    app.act_on_settings(Intent::Select);
                    app.act_on_settings(Intent::Move(0, 1));
                    app.act_on_settings(Intent::Move(0, 1));
                    app.act_on_settings(Intent::Select);
                    app.paint();
                }),
                "output-picker" => app(|app| {
                    app.act_on_settings(crate::actions::Intent::Select);
                    app.paint();
                }),
                "output-advanced" => app(|app| {
                    for _ in 0..3 {
                        app.act_on_settings(crate::actions::Intent::Move(0, 1));
                    }
                    app.act_on_settings(crate::actions::Intent::Select);
                    app.paint();
                }),
                "output-trial" => {
                    let mut view = ui.get_settings_output_view();
                    view.sheet = 1;
                    view.seconds = 11;
                    view.arc = crate::vitals::arc(11.0 / 15.0).into();
                    view.confirm_ready = true;
                    view.trial = "1920 × 1080 @ 60 Hz · RGB 8 bit".into();
                    view.previous = "3840 × 2160 @ 60 Hz · YCbCr 4:2:0 8 bit".into();
                    ui.set_settings_output_view(view);
                }
                "output-edid" => {
                    let mut view = ui.get_settings_output_view();
                    view.sheet = 2;
                    ui.set_settings_output_view(view);
                }
                _ => {}
            }
        }
        n if n.starts_with("audio") => {
            page(Page::Audio, 0);
            match n {
                "audio-muted" => {
                    let mut view = ui.get_settings_audio_view();
                    view.muted = true;
                    ui.set_settings_audio_view(view);
                }
                "audio-picker" => app(|app| {
                    app.act_on_settings(crate::actions::Intent::Select);
                    app.paint();
                }),
                _ => {}
            }
        }
        n if n.starts_with("ethernet") => {
            page(Page::Ethernet, usize::from(n == "ethernet"));
            let press = |count: i32| {
                app(|app| {
                    use crate::actions::Intent;
                    app.act_on_settings(Intent::Select);
                    for _ in 0..count {
                        app.act_on_settings(Intent::Move(0, 1));
                    }
                    if count > 0 {
                        app.act_on_settings(Intent::Select);
                    }
                    app.paint();
                })
            };
            match n {
                "ethernet-picker" => press(0),
                "ethernet-static" => press(1),
                "ethernet-entry" => {
                    press(1);
                    app(|app| {
                        app.act_on_settings(crate::actions::Intent::Move(0, 1));
                        app.act_on_settings(crate::actions::Intent::Select);
                        app.paint();
                    });
                }
                "ethernet-trial" => {
                    let mut view = ui.get_settings_ethernet_view();
                    view.sheet = 1;
                    view.seconds = 40;
                    view.arc = crate::vitals::arc(0.66).into();
                    view.confirm_ready = true;
                    view.trial = "Elle · 192.168.2.80/24".into();
                    view.previous = "Otomatik (DHCP)".into();
                    view.trial_now = "Şu an: 192.168.2.80 · kablo takılı".into();
                    ui.set_settings_ethernet_view(view);
                }
                _ => {}
            }
        }
        n if n.starts_with("cooling") => {
            page(Page::Cooling, 0);
            let mut view = ui.get_settings_cooling_view();
            view.prompt = n == "cooling-prompt";
            if n == "cooling-info" {
                view.advanced = true;
                view.info = model(
                    (0..10).map(|i| crate::CoolInfo { label: format!("Bilgi {i}").into(), value: "Etkin".into() }).collect(),
                );
            }
            ui.set_settings_cooling_view(view);
        }
        n if n.starts_with("wifi") => {
            use crate::screens::wireless::Face;
            with_radios();
            app(|app| {
                app.open(crate::route::Route::Wifi);
                app.wifi.powered = n != "wifi-off";
                match n {
                    "wifi-details" => {
                        app.wifi.details = Some("Ağ 03".into());
                        app.wifi.face = Face::Details;
                    }
                    "wifi-password" | "wifi-add" => {
                        app.wifi.adding = n == "wifi-add";
                        app.wifi.secure = true;
                        app.wifi.face = Face::Password;
                    }
                    _ => {}
                }
                app.paint();
            });
        }
        n if n.starts_with("bluetooth") => {
            with_radios();
            app(|app| {
                app.open(crate::route::Route::Bluetooth);
                app.bt.scanning = n == "bluetooth-scan";
                app.paint();
            });
        }
        "account" => app(|app| {
            app.open(crate::route::Route::Account);
            app.paint();
        }),
        "cec" => page(Page::Cec, 0),
        "system" => app(|app| {
            app.open(crate::route::Route::Settings);
            while app.settings.back() {}
            app.settings.compose(Some(&status), None, None);
            let at = app.settings.groups.iter().position(|g| g.title == "Sistem").unwrap();
            app.settings.point_section(at);
            app.settings.point_row(0);
            app.paint();
        }),
        n => {
            let at: usize = n.rsplit('-').next().unwrap().parse().unwrap();
            app(|app| {
                app.open(crate::route::Route::Diagnostics);
                app.diagnostics.compose(Some(&status), Some(&diagnostics), None);
                app.diagnostics.group = at;
                app.paint();
            });
        }
    }
}

/// Pictures of the whole settings family, for a person to look at. As
/// `shots`, only with MEDIABOX_TV_SHOTS set.
#[test]
#[ignore]
fn shots_family() {
    use slint::platform::software_renderer::PremultipliedRgbaColor;
    let Some(out) = std::env::var_os("MEDIABOX_TV_SHOTS").map(std::path::PathBuf::from) else {
        return;
    };
    let only = std::env::var("MEDIABOX_TV_SHOTS_ONLY").unwrap_or_default();
    std::fs::create_dir_all(&out).unwrap();
    let (ui, _scratch) = driven();
    let window = ui.window();
    for (w, h, scale, size) in [(1920u32, 1080u32, 1.0f32, "1080p"), (2560, 1440, 2.0, "1440p"), (3840, 2160, 2.0, "4k")] {
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        window.set_size(slint::PhysicalSize::new(w, h));
        let frame = || {
            let mut pixels = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
            slint::platform::update_timers_and_animations();
            SHOT_WINDOW.with(|slot| {
                let slot = slot.borrow();
                let window = slot.as_ref().expect("the software window");
                window.request_redraw();
                window.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, w as usize);
                })
            });
            pixels
        };
        for layout in [crate::layout::Layout::Classic, crate::layout::Layout::Quiet] {
            wear(layout);
            for name in FAMILY {
                if !format!("{size}-{}-{name}", layout.name()).contains(&only) {
                    continue;
                }
                family_show(&ui, name);
                ui.set_vitals(crate::vitals::read(Some(&family_diagnostics())));
                let mut vitals = ui.get_vitals();
                vitals.time = "13:34".into();
                vitals.date = "24 Kasım".into();
                ui.set_vitals(vitals);
                frame();
                std::thread::sleep(std::time::Duration::from_millis(900));
                let pixels = frame();
                let rgb: Vec<u8> = pixels.iter().flat_map(|p| [p.red, p.green, p.blue]).collect();
                image::RgbImage::from_raw(w, h, rgb)
                    .unwrap()
                    .save(out.join(format!("{size}-{}-family-{name}.png", layout.name())))
                    .unwrap();
            }
        }
    }
}

thread_local! {
    static SHOT_WINDOW: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
}
