//! A pointer on the television: where it is allowed, and what it is over.
//!
//! Whatever libinput reports pointer events for is a pointer -- a mouse, a
//! touchpad, a trackball, an air-mouse remote, an absolute tablet-like
//! device. A remote that is a keyboard and a pointer at once is a remote for
//! its keys and a pointer for its motion and its buttons, whichever it sends.
//! Nothing here asks what a device is called.
//!
//! Where a pointer means anything is one table, `route_policy`, and one
//! context, `Context`: a screen nobody has made pointer-ready is `Disabled`,
//! and there a pointer does nothing at all -- no arrow, no hover, and a click
//! is not turned into Ok. On a screen that is `Navigation` the arrow over a
//! target only lights it; a click puts the remote's focus on it through the
//! screen's own focus model (`App::pointer_point`) and runs the screen's own
//! Select once: there is no second road for what a click does.
//!
//! What is under the arrow is Slint's to say: the shared components carry a
//! `PointerTarget` (ui/pointer.slint) naming a `Zone` and an index, and a
//! zone is taken only while its layer is the one on top (`accepts`).
//!
//! What the button and the hand do is one machine, `Gesture`: a press is a
//! candidate until it is a click (let go where it went down) or a drag (moved
//! further than `slop`), which moves the surface under it -- a `Lane` -- and
//! never the remote's focus. The wheel moves the same surfaces (`Scroller`).

use crate::route::Route;

/// What a pointer may do on what is on the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Nothing: motion, buttons and the wheel are ignored and no arrow is
    /// drawn. A click is not Ok.
    Disabled,
    /// Hover lights a target, a click is the remote's Ok on what is under
    /// the arrow, a drag and the wheel scroll what is under it.
    Navigation,
    /// Navigation, and a screen's own pointer gesture besides: the film's
    /// position bar takes a click as a place to go and a drag as a scrub
    /// (`PointerScrub`, `App::pointer_scrub`).
    Full,
}

impl Policy {
    pub fn enabled(self) -> bool {
        self != Policy::Disabled
    }
}

/// The table. A new route does not compile until it is given a policy here.
pub fn route_policy(route: Route) -> Policy {
    match route {
        Route::Home => Policy::Navigation,
        Route::Settings => Policy::Navigation,
        Route::Diagnostics => Policy::Navigation,
        Route::Media => Policy::Navigation,
        Route::Discover => Policy::Navigation,
        Route::Library => Policy::Navigation,
        Route::Search => Policy::Navigation,
        Route::MediaSettings => Policy::Navigation,
        Route::Detail => Policy::Navigation,
        Route::Account => Policy::Navigation,
        Route::Wifi => Policy::Navigation,
        Route::Bluetooth => Policy::Navigation,
        // The screen that mirrors a film Kodi plays. A film this interface
        // plays is the player, whatever route is under it (`policy`).
        Route::NowPlaying => Policy::Navigation,
        // Live television's own overlays -- its menu, guide, side panels,
        // number entry and setup -- and the picture's ground, a click on
        // which is the remote's Ok there (the menu). The broadcast itself is
        // on the video plane and is no target.
        Route::Live => Policy::Navigation,
        // The boot screen has no control and no focus: it is drawn before
        // the box has anything to offer, and nothing on it can be chosen.
        Route::Boot => Policy::Disabled,
    }
}

/// Whether a wheel's turn moves along rather than down: a screen that is
/// one row, where Up and Down go nowhere.
pub fn wheel_is_along(route: Route) -> bool {
    route == Route::Home
}

/// What a wheel does where nothing that scrolls is under the arrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelElsewhere {
    /// The remote's arrows, as on a screen that is all focus.
    Arrows,
    /// The catalogue's page scrolls -- its board, its grid, its controls --
    /// or, with none, nothing does: Stremio's pages are scrolled by the
    /// browser, which never moves the focus (stremio-web has no wheel
    /// handler of its own), and neither does a wheel here.
    Page(Option<Lane>),
}

/// The table, as `route_policy` is: a new route says what its wheel does.
pub fn wheel_elsewhere(route: Route) -> WheelElsewhere {
    match route {
        Route::Media | Route::Search => WheelElsewhere::Page(Some(Lane::vertical(Zone::Shelves))),
        Route::Discover | Route::Library => WheelElsewhere::Page(Some(Lane::vertical(Zone::Grid))),
        Route::MediaSettings => WheelElsewhere::Page(Some(Lane::vertical(Zone::MsControls))),
        // A title's page: its lists scroll under the arrow, and nothing else.
        Route::Detail => WheelElsewhere::Page(None),
        Route::Home
        | Route::Settings
        | Route::Diagnostics
        | Route::Account
        | Route::Wifi
        | Route::Bluetooth
        | Route::NowPlaying
        | Route::Live
        | Route::Boot => WheelElsewhere::Arrows,
    }
}

/// Whether a click on a target that is not the focused one only selects
/// it: Discover's grid, as Stremio's (`metaItemOnClick` prevents the link
/// and focuses the poster unless it is the selected one), where the record
/// beside the grid is the focused title's. The second click opens it.
pub fn click_selects_first(route: Route, zone: Zone) -> bool {
    route == Route::Discover && zone == Zone::Grid
}

/// Which layer of the panel is on top, and so the only one a pointer reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// The screen itself.
    Screen,
    /// A short list the screen opened over itself.
    Choice,
    /// The box's own sheet -- power, a confirmation -- over everything.
    Sheet,
}

/// Everything the policy is decided from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub route: Route,
    pub layer: Layer,
    /// A film this interface plays is on the panel, and the panel is its
    /// player (live television is its own route).
    pub film: bool,
    /// The panel has been handed to another application (Kodi, the
    /// browser), which reads its own input.
    pub handed_over: bool,
}

/// The policy for what is on the panel. A sheet takes the route's: a screen
/// that is not pointer-ready does not become one because a sheet is over it.
/// Another application on the panel reads its own input; a film is its
/// player, which is `Full`.
pub fn policy(context: Context) -> Policy {
    if context.handed_over {
        return Policy::Disabled;
    }
    if context.film {
        return Policy::Full;
    }
    route_policy(context.route)
}

/// What a change of what is on the panel does to the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    /// Nothing changed.
    Keep,
    /// Another screen, layer or film: what the pointer was over and pressing
    /// belongs to what was there, and is forgotten. The arrow stays.
    Forget,
    /// The pointer is disabled now: the arrow goes as well.
    Hide,
}

/// From what the policy was last decided from to what it is decided from
/// now.
pub fn transition(before: Option<Context>, now: Context) -> Transition {
    if before == Some(now) {
        return Transition::Keep;
    }
    if !policy(now).enabled() {
        return Transition::Hide;
    }
    Transition::Forget
}

/// What putting the remote's focus on a target did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aim {
    /// The focus moved onto it: there is a change to draw.
    Moved,
    /// The focus was already on it.
    Here,
    /// It cannot take the focus -- disabled, out of range, under a modal.
    /// A click on it does nothing; it is never Ok on whatever had the focus.
    Refused,
}

impl Aim {
    /// Puts `slot` at `want`, if `allowed`.
    pub fn set<T: PartialEq>(slot: &mut T, want: T, allowed: bool) -> Aim {
        if !allowed {
            Aim::Refused
        } else if *slot == want {
            Aim::Here
        } else {
            *slot = want;
            Aim::Moved
        }
    }

    /// Whether the focus is on the target now, and a click may press it.
    pub fn taken(self) -> bool {
        self != Aim::Refused
    }
}

/// A grid target's index, as the interface sends it: row * 100 + column.
pub fn cell(index: usize) -> (usize, usize) {
    (index / 100, index % 100)
}

/// A group of pointer targets, as ui/pointer.slint names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// The launcher's tiles.
    HomeApps,
    /// The same tiles as Quiet OS draws them: a column that may be taller
    /// than the panel, so it scrolls up and down under a hand and a wheel
    /// (the classic row never does, and its wheel stays the arrows).
    QuietHome,
    /// The settings' category column.
    SettingsSections,
    /// A way back: the remote's Back under the arrow. The settings'
    /// column's heading and its parent row, the catalogue frame's mark, a
    /// screen's title bar, the close of a sheet or a panel -- whichever is up.
    Back,
    /// The way home: the remote's Home under the arrow. Quiet OS's settings
    /// family draws it at the top left, where a page's way out is looked
    /// for; Back stays the right button and the remote's own key.
    GoHome,
    /// The settings' cards, a category's or a list page's.
    SettingsRows,
    /// A short list opened from a settings card.
    SettingsChoice,
    /// An editor page's cards (sound, a wired port, the display's simple
    /// face).
    EditorCards,
    /// An editor page's buttons along the bottom.
    EditorActions,
    /// The display's sizes, rates and colour cells on its advanced face (0 is
    /// "Otomatik" in sizes and colours).
    OutputResolutions,
    OutputRates,
    OutputColours,
    /// The fan curve's preset chips, and its table's cells (row * 100 + col).
    CoolingChips,
    CoolingCells,
    /// An editor's own list over the page.
    EditorPicker,
    /// An editor's number pad over the page (row * 100 + col).
    EditorPad,
    /// An editor's question over the page: keep or go back, restart now or
    /// later, close.
    EditorConfirm,
    /// The diagnostics' group column.
    DiagnosticsGroups,
    /// The catalogue's shared frame (ui/chrome.slint): its places down the
    /// left and its search box across the top, on whichever of its screens
    /// is up.
    FramePlaces,
    FrameSearch,
    /// A poster of a shelf (row * 100 + column; one past the last poster is
    /// "Tümünü Gör"): the board's, or the search's results.
    Shelves,
    /// A poster of a grid: Discover's, or the library's.
    Grid,
    /// The search's letters (row * 100 + column) and its list beside them.
    SearchKeys,
    SearchList,
    /// The catalogue's settings: its categories, its controls, and the
    /// options of a control's open list.
    MsCategories,
    MsControls,
    MsOptions,
    /// A title's page: its action row, its sources, its episodes; the
    /// provider filter above the sources and its list; the season bar's
    /// "Önceki" (0), "Sonraki" (1) and the season's name (2); an episode's
    /// watched button.
    DetailActions,
    DetailSources,
    DetailEpisodes,
    DetailFilter,
    DetailProviders,
    DetailSeasons,
    DetailMarks,
    /// The ground of whatever is over the screen -- a sheet, a list, a menu,
    /// a pad, a question: a click on it is Back, which closes it as the
    /// remote's Back does.
    Shield,
    /// A poster's menu on the board, and the cross and the menu mark on a
    /// focused "İzlemeye devam edin" poster (row * 100 + column).
    MediaMenu,
    PosterDismiss,
    PosterMenu,
    /// Discover's dropdowns, the open one's options, and the buttons under
    /// the focused title's record.
    DiscoverFilters,
    DiscoverOptions,
    DiscoverPanel,
    /// The library's type dropdown (0) and order chips (1..), the open
    /// dropdown's options, and a poster's menu.
    LibraryFilters,
    LibraryOptions,
    LibraryMenu,
    /// The menu mark on a library poster (its grid index): the poster's
    /// menu, as a held Ok opens it.
    LibraryMark,
    /// The account mark in the catalogue's frame, and the button of the menu
    /// it drops down.
    FrameAccount,
    FrameAccountMenu,
    /// The sound page's volume bar: its lower half (0) and upper half (1),
    /// a step each way, as Left and Right on the volume card.
    EditorVolume,
    /// The box's sheet's rows or buttons.
    Sheet,
    /// The account form: the address (0), the password (1), "Göster" (2),
    /// "Giriş yap" (3); and its letters (row * 100 + column).
    AccountForm,
    AccountKeys,
    /// The radios' lists: a row by its place among the rows the remote
    /// stops on, a network's (i), a network page's buttons; the password
    /// face's name (0), security (1), field (2), "Göster" (3) and button
    /// (4), and its letters.
    WifiRows,
    WifiInfo,
    WifiButtons,
    WifiForm,
    WifiKeys,
    BtRows,
    /// The control row of a film Kodi plays.
    NpControls,
    /// The film's player: its position bar (dragged, `PointerScrub`), its
    /// row of buttons, the way out of the film, and a panel's languages,
    /// tracks and delay.
    PlayerBar,
    PlayerButtons,
    PlayerExit,
    PlayerMenu,
    PlayerTracks,
    PlayerSide,
    /// Live television: the ground over the picture (Ok: the menu), the
    /// menu's cards (row * 100 + column; column 99 is a row's title), the
    /// guide's programmes (row * 100 + cell), a side panel's rows, the
    /// number entry's channels, the setup's rows and its card's buttons.
    LiveSurface,
    LiveMenu,
    LiveGuide,
    LivePanel,
    LiveKeypad,
    LiveSetup,
    LiveActions,
}

/// A live menu target's column that is its row's title.
pub const ROW_TITLE: usize = 99;

impl Zone {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "home" => Zone::HomeApps,
            "quiet-home" => Zone::QuietHome,
            "settings-sections" => Zone::SettingsSections,
            "back" => Zone::Back,
            "go-home" => Zone::GoHome,
            "settings-rows" => Zone::SettingsRows,
            "settings-choice" => Zone::SettingsChoice,
            "editor-cards" => Zone::EditorCards,
            "editor-actions" => Zone::EditorActions,
            "output-resolutions" => Zone::OutputResolutions,
            "output-rates" => Zone::OutputRates,
            "output-colours" => Zone::OutputColours,
            "cooling-chips" => Zone::CoolingChips,
            "cooling-cells" => Zone::CoolingCells,
            "editor-picker" => Zone::EditorPicker,
            "editor-pad" => Zone::EditorPad,
            "editor-confirm" => Zone::EditorConfirm,
            "diagnostics-groups" => Zone::DiagnosticsGroups,
            "frame-places" => Zone::FramePlaces,
            "frame-search" => Zone::FrameSearch,
            "shelves" => Zone::Shelves,
            "grid" => Zone::Grid,
            "search-keys" => Zone::SearchKeys,
            "search-list" => Zone::SearchList,
            "ms-categories" => Zone::MsCategories,
            "ms-controls" => Zone::MsControls,
            "ms-options" => Zone::MsOptions,
            "detail-actions" => Zone::DetailActions,
            "detail-sources" => Zone::DetailSources,
            "detail-episodes" => Zone::DetailEpisodes,
            "detail-filter" => Zone::DetailFilter,
            "detail-providers" => Zone::DetailProviders,
            "detail-seasons" => Zone::DetailSeasons,
            "detail-marks" => Zone::DetailMarks,
            "shield" => Zone::Shield,
            "media-menu" => Zone::MediaMenu,
            "poster-dismiss" => Zone::PosterDismiss,
            "poster-menu" => Zone::PosterMenu,
            "discover-filters" => Zone::DiscoverFilters,
            "discover-options" => Zone::DiscoverOptions,
            "discover-panel" => Zone::DiscoverPanel,
            "library-filters" => Zone::LibraryFilters,
            "library-options" => Zone::LibraryOptions,
            "library-menu" => Zone::LibraryMenu,
            "library-mark" => Zone::LibraryMark,
            "frame-account" => Zone::FrameAccount,
            "frame-account-menu" => Zone::FrameAccountMenu,
            "editor-volume" => Zone::EditorVolume,
            "sheet" => Zone::Sheet,
            "account-form" => Zone::AccountForm,
            "account-keys" => Zone::AccountKeys,
            "wifi-rows" => Zone::WifiRows,
            "wifi-info" => Zone::WifiInfo,
            "wifi-buttons" => Zone::WifiButtons,
            "wifi-form" => Zone::WifiForm,
            "wifi-keys" => Zone::WifiKeys,
            "bt-rows" => Zone::BtRows,
            "np-controls" => Zone::NpControls,
            "player-bar" => Zone::PlayerBar,
            "player-buttons" => Zone::PlayerButtons,
            "player-exit" => Zone::PlayerExit,
            "player-menu" => Zone::PlayerMenu,
            "player-tracks" => Zone::PlayerTracks,
            "player-side" => Zone::PlayerSide,
            "live-surface" => Zone::LiveSurface,
            "live-menu" => Zone::LiveMenu,
            "live-guide" => Zone::LiveGuide,
            "live-panel" => Zone::LivePanel,
            "live-keypad" => Zone::LiveKeypad,
            "live-setup" => Zone::LiveSetup,
            "live-actions" => Zone::LiveActions,
            _ => return None,
        })
    }

    /// Where the zone is drawn: its route (none for the sheet, which is over
    /// any) and its layer.
    fn place(self) -> (Option<Route>, Layer) {
        match self {
            Zone::HomeApps | Zone::QuietHome => (Some(Route::Home), Layer::Screen),
            Zone::SettingsSections
            | Zone::SettingsRows
            | Zone::EditorCards
            | Zone::EditorActions
            | Zone::OutputResolutions
            | Zone::OutputRates
            | Zone::OutputColours
            | Zone::CoolingChips
            | Zone::CoolingCells => (Some(Route::Settings), Layer::Screen),
            Zone::SettingsChoice | Zone::EditorPicker | Zone::EditorPad | Zone::EditorConfirm => {
                (Some(Route::Settings), Layer::Choice)
            }
            Zone::DiagnosticsGroups => (Some(Route::Diagnostics), Layer::Screen),
            // Shared by several screens: which one is up decides
            // (`App::pointer_point`), and one without them refuses.
            Zone::FramePlaces | Zone::FrameSearch | Zone::Shelves | Zone::Grid => {
                (None, Layer::Screen)
            }
            // The panel under the search box is over the results.
            Zone::SearchKeys | Zone::SearchList => (Some(Route::Search), Layer::Choice),
            Zone::MsCategories | Zone::MsControls => (Some(Route::MediaSettings), Layer::Screen),
            Zone::MsOptions => (Some(Route::MediaSettings), Layer::Choice),
            Zone::DetailActions
            | Zone::DetailSources
            | Zone::DetailEpisodes
            | Zone::DetailFilter
            | Zone::DetailSeasons
            | Zone::DetailMarks => (Some(Route::Detail), Layer::Screen),
            Zone::DetailProviders => (Some(Route::Detail), Layer::Choice),
            // Whichever modal is up: see `accepts`.
            Zone::Shield => (None, Layer::Choice),
            Zone::PosterDismiss | Zone::PosterMenu => (Some(Route::Media), Layer::Screen),
            Zone::MediaMenu => (Some(Route::Media), Layer::Choice),
            Zone::DiscoverFilters | Zone::DiscoverPanel => (Some(Route::Discover), Layer::Screen),
            Zone::DiscoverOptions => (Some(Route::Discover), Layer::Choice),
            Zone::LibraryFilters | Zone::LibraryMark => (Some(Route::Library), Layer::Screen),
            Zone::LibraryOptions | Zone::LibraryMenu => (Some(Route::Library), Layer::Choice),
            Zone::FrameAccount => (None, Layer::Screen),
            Zone::FrameAccountMenu => (None, Layer::Choice),
            Zone::EditorVolume => (Some(Route::Settings), Layer::Screen),
            Zone::Sheet => (None, Layer::Sheet),
            // Whichever layer is on top: see `accepts`.
            Zone::Back => (None, Layer::Screen),
            // Any screen, and only with nothing over it: Home under a sheet
            // is refused as the remote's is (`actions::intent_for`).
            Zone::GoHome => (None, Layer::Screen),
            Zone::AccountForm | Zone::AccountKeys => (Some(Route::Account), Layer::Screen),
            Zone::WifiRows
            | Zone::WifiInfo
            | Zone::WifiButtons
            | Zone::WifiForm
            | Zone::WifiKeys => (Some(Route::Wifi), Layer::Screen),
            Zone::BtRows => (Some(Route::Bluetooth), Layer::Screen),
            Zone::NpControls => (Some(Route::NowPlaying), Layer::Screen),
            // Over whatever route the film was started from: see `accepts`.
            Zone::PlayerBar | Zone::PlayerButtons | Zone::PlayerExit => (None, Layer::Screen),
            Zone::PlayerMenu | Zone::PlayerTracks | Zone::PlayerSide => (None, Layer::Choice),
            Zone::LiveSurface => (Some(Route::Live), Layer::Screen),
            Zone::LiveMenu
            | Zone::LiveGuide
            | Zone::LivePanel
            | Zone::LiveKeypad
            | Zone::LiveSetup
            | Zone::LiveActions => (Some(Route::Live), Layer::Choice),
        }
    }

    /// Whether the zone is the film's player's, drawn only while a film
    /// this interface plays is on the panel.
    pub fn player(self) -> bool {
        matches!(
            self,
            Zone::PlayerBar
                | Zone::PlayerButtons
                | Zone::PlayerExit
                | Zone::PlayerMenu
                | Zone::PlayerTracks
                | Zone::PlayerSide
        )
    }

    /// What a press of the main button on target `index` of this zone is,
    /// once the remote's focus is on it: the remote's own key -- through the
    /// remote's own road, Ok's hold included -- or one of the few things a
    /// remote reaches by a hold and a mouse by a mark of its own, or nothing.
    pub fn activation(self, index: usize) -> Activation {
        use mediabox_core::InputAction;
        match self {
            Zone::Back | Zone::Shield => Activation::Key(InputAction::Back),
            Zone::GoHome => Activation::Key(InputAction::Home),
            // The way out of the film is the remote's Stop, which leaves it
            // for the page it was started from: Back on a film whose
            // controls are up only takes the controls away, and a pointer
            // that moves brings them back.
            Zone::PlayerExit => Activation::Key(InputAction::Stop),
            // A menu row's title: the row comes forward, nothing is chosen.
            Zone::LiveMenu if index % 100 == ROW_TITLE => Activation::Nothing,
            // The bar is the scrub's (`PointerScrub`); hover only reaches it.
            Zone::PlayerBar => Activation::Nothing,
            // "Önceki" and "Sonraki" are Left and Right on the season bar;
            // the volume bar's halves are Left and Right on the volume card.
            // The season's own name between them (2) is where the remote
            // stands on the bar, and Ok there is the remote's: nothing.
            Zone::DetailSeasons if index == 2 => Activation::Key(InputAction::Ok),
            Zone::DetailSeasons | Zone::EditorVolume => Activation::Key(if index == 0 {
                InputAction::Left
            } else {
                InputAction::Right
            }),
            // The poster's menu mark opens its menu, as a held Ok does; its
            // cross is the menu's "Vazgeç".
            Zone::PosterMenu | Zone::LibraryMark => Activation::OpenMenu,
            Zone::PosterDismiss => Activation::DismissPoster,
            // Read, not operated: hover moves the column, a click is nothing.
            Zone::DiagnosticsGroups => Activation::Nothing,
            _ => Activation::Key(InputAction::Ok),
        }
    }

    /// Whether the arrow over target `index` of this zone lights it. Not a
    /// modal's ground, nor the picture under live television: they fill the
    /// panel, and are no control.
    pub fn lights(self) -> bool {
        !matches!(self, Zone::Shield | Zone::LiveSurface)
    }

    /// Whether a press held still on a target of this zone may be the
    /// remote's long Ok -- the poster's, a network's, a paired device's menu
    /// (`App::wants_ok_hold` decides, once the focus is on it).
    pub fn holds(self) -> bool {
        matches!(
            self,
            Zone::Shelves | Zone::Grid | Zone::WifiRows | Zone::BtRows
        )
    }

    /// The surfaces a target of this zone lies on, which a drag starting on
    /// it moves: a list, a grid, a shelf and the board it is on. A target on
    /// nothing that scrolls has none, and a drag from it is only not a click.
    pub fn lanes(self, index: usize) -> Lanes {
        let vertical = |zone| Lanes {
            vertical: Some(Lane::vertical(zone)),
            horizontal: None,
        };
        match self {
            Zone::Shelves | Zone::PosterDismiss | Zone::PosterMenu => Lanes {
                vertical: Some(Lane::vertical(Zone::Shelves)),
                horizontal: Some(Lane {
                    zone: Zone::Shelves,
                    lane: i32::try_from(cell(index).0).unwrap_or(-1),
                    axis: Axis::Horizontal,
                }),
            },
            Zone::Grid | Zone::LibraryMark => vertical(Zone::Grid),
            Zone::WifiRows | Zone::WifiInfo => vertical(Zone::WifiRows),
            Zone::DetailEpisodes | Zone::DetailMarks => vertical(Zone::DetailEpisodes),
            Zone::QuietHome
            | Zone::SettingsRows
            | Zone::SettingsChoice
            | Zone::EditorPicker
            | Zone::OutputResolutions
            | Zone::BtRows
            | Zone::MsControls
            | Zone::MsOptions
            | Zone::DetailSources
            | Zone::DiscoverOptions
            | Zone::PlayerMenu
            | Zone::PlayerTracks
            | Zone::LiveGuide
            | Zone::LivePanel => vertical(self),
            _ => Lanes::default(),
        }
    }
}

/// The name each zone has in the interface (`Zone::parse` the other way);
/// every one is used by a .slint file (`every_zone_is_named_in_the_interface`).
pub fn zone_name(zone: Zone) -> &'static str {
    match zone {
        Zone::HomeApps => "home",
        Zone::QuietHome => "quiet-home",
        Zone::SettingsSections => "settings-sections",
        Zone::Back => "back",
        Zone::GoHome => "go-home",
        Zone::SettingsRows => "settings-rows",
        Zone::SettingsChoice => "settings-choice",
        Zone::EditorCards => "editor-cards",
        Zone::EditorActions => "editor-actions",
        Zone::OutputResolutions => "output-resolutions",
        Zone::OutputRates => "output-rates",
        Zone::OutputColours => "output-colours",
        Zone::CoolingChips => "cooling-chips",
        Zone::CoolingCells => "cooling-cells",
        Zone::EditorPicker => "editor-picker",
        Zone::EditorPad => "editor-pad",
        Zone::EditorConfirm => "editor-confirm",
        Zone::DiagnosticsGroups => "diagnostics-groups",
        Zone::FramePlaces => "frame-places",
        Zone::FrameSearch => "frame-search",
        Zone::Shelves => "shelves",
        Zone::Grid => "grid",
        Zone::SearchKeys => "search-keys",
        Zone::SearchList => "search-list",
        Zone::MsCategories => "ms-categories",
        Zone::MsControls => "ms-controls",
        Zone::MsOptions => "ms-options",
        Zone::DetailActions => "detail-actions",
        Zone::DetailSources => "detail-sources",
        Zone::DetailEpisodes => "detail-episodes",
        Zone::DetailFilter => "detail-filter",
        Zone::DetailProviders => "detail-providers",
        Zone::DetailSeasons => "detail-seasons",
        Zone::DetailMarks => "detail-marks",
        Zone::Shield => "shield",
        Zone::MediaMenu => "media-menu",
        Zone::PosterDismiss => "poster-dismiss",
        Zone::PosterMenu => "poster-menu",
        Zone::DiscoverFilters => "discover-filters",
        Zone::DiscoverOptions => "discover-options",
        Zone::DiscoverPanel => "discover-panel",
        Zone::LibraryFilters => "library-filters",
        Zone::LibraryOptions => "library-options",
        Zone::LibraryMenu => "library-menu",
        Zone::LibraryMark => "library-mark",
        Zone::FrameAccount => "frame-account",
        Zone::FrameAccountMenu => "frame-account-menu",
        Zone::EditorVolume => "editor-volume",
        Zone::Sheet => "sheet",
        Zone::AccountForm => "account-form",
        Zone::AccountKeys => "account-keys",
        Zone::WifiRows => "wifi-rows",
        Zone::WifiInfo => "wifi-info",
        Zone::WifiButtons => "wifi-buttons",
        Zone::WifiForm => "wifi-form",
        Zone::WifiKeys => "wifi-keys",
        Zone::BtRows => "bt-rows",
        Zone::NpControls => "np-controls",
        Zone::PlayerBar => "player-bar",
        Zone::PlayerButtons => "player-buttons",
        Zone::PlayerExit => "player-exit",
        Zone::PlayerMenu => "player-menu",
        Zone::PlayerTracks => "player-tracks",
        Zone::PlayerSide => "player-side",
        Zone::LiveSurface => "live-surface",
        Zone::LiveMenu => "live-menu",
        Zone::LiveGuide => "live-guide",
        Zone::LivePanel => "live-panel",
        Zone::LiveKeypad => "live-keypad",
        Zone::LiveSetup => "live-setup",
        Zone::LiveActions => "live-actions",
    }
}

/// Which way a surface scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// A surface a pointer scrolls, named as the interface names it: by its
/// targets' zone, and a lane -- a shelf's own row on the board, -1 for the
/// surface itself (`PanState`, ui/pointer.slint).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lane {
    pub zone: Zone,
    pub lane: i32,
    pub axis: Axis,
}

impl Lane {
    pub fn vertical(zone: Zone) -> Self {
        Lane {
            zone,
            lane: -1,
            axis: Axis::Vertical,
        }
    }

    /// A list drawn as a window of rows Rust keeps rather than one Slint
    /// scrolls: it moves by whole rows (`App::pointer_shift`).
    pub fn stepped(self) -> bool {
        matches!(
            self.zone,
            Zone::MsOptions
                | Zone::PlayerMenu
                | Zone::PlayerTracks
                | Zone::LiveGuide
                | Zone::LivePanel
        )
    }

    /// Of a pair of numbers along both axes, the one along this lane.
    pub fn along(self, (x, y): (f32, f32)) -> f32 {
        match self.axis {
            Axis::Vertical => y,
            Axis::Horizontal => x,
        }
    }
}

/// What scrolls at a point: a surface each way at most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lanes {
    pub vertical: Option<Lane>,
    pub horizontal: Option<Lane>,
}

impl Lanes {
    /// The ground of a surface where it has no target (`PointerPanArea`),
    /// by the name and lane the interface gives it: a shelf's row lies on
    /// the board, which a drag up or down moves instead.
    pub fn of_area(surface: &str, lane: i32) -> Self {
        let Some(zone) = Zone::parse(surface) else {
            return Lanes::default();
        };
        match usize::try_from(lane) {
            Ok(row) => zone.lanes(row * 100),
            Err(_) => Lanes {
                vertical: zone.lanes(0).vertical,
                horizontal: None,
            },
        }
    }

    pub fn is_empty(self) -> bool {
        self.vertical.is_none() && self.horizontal.is_none()
    }

    /// Whether these surfaces may be moved now: each is on the layer on top
    /// (`accepts`), so with a modal up only the modal's own list moves and
    /// nothing under it does.
    pub fn taken(self, context: Context) -> bool {
        !self.is_empty()
            && [self.vertical, self.horizontal]
                .into_iter()
                .flatten()
                .all(|lane| accepts(context, lane.zone))
    }

    /// The surface a movement of (dx, dy) takes: the one along the way it
    /// mostly goes -- a grid's dominant axis, a shelf sideways and the board
    /// up and down -- or, with nothing that way, the one there is.
    pub fn along(self, dx: f32, dy: f32) -> Option<Lane> {
        if dx.abs() > dy.abs() {
            self.horizontal.or(self.vertical)
        } else {
            self.vertical.or(self.horizontal)
        }
    }

    /// The surface along one axis only: a wheel turned up and down moves
    /// what scrolls up and down, and a sideways one what scrolls sideways.
    pub fn on(self, axis: Axis) -> Option<Lane> {
        match axis {
            Axis::Vertical => self.vertical,
            Axis::Horizontal => self.horizontal,
        }
    }
}

/// What a click on a target does, after its focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// The remote's key, pressed and released with the button.
    Key(mediabox_core::InputAction),
    OpenMenu,
    DismissPoster,
    Nothing,
}

/// Whether a target in `zone` may take the pointer now: the policy allows a
/// pointer, the zone is on this route, and its layer is the one on top -- a
/// sheet or a list over a screen keeps the screen under it from the pointer.
pub fn accepts(context: Context, zone: Zone) -> bool {
    let (route, layer) = zone.place();
    if !policy(context).enabled() {
        return false;
    }
    // A modal's ground belongs to whichever modal is up, and only while one is.
    if zone == Zone::Shield {
        return context.layer != Layer::Screen;
    }
    // A way back is the remote's Back on whatever is on top; one drawn under
    // a modal is under its ground, which is Back as well.
    if zone == Zone::Back {
        return true;
    }
    // Quiet OS draws the categories beside the radios as well, which are
    // pages of "Bağlantılar": a click there leaves for that category.
    if zone == Zone::SettingsSections
        && matches!(context.route, Route::Wifi | Route::Bluetooth)
        && !context.film
    {
        return layer == context.layer;
    }
    // While a film plays the panel is its player: the screen it was started
    // from is not drawn, and the player is drawn nowhere else.
    if zone.player() != context.film && zone != Zone::Sheet {
        return false;
    }
    layer == context.layer && route.is_none_or(|route| route == context.route)
}

/// What a pointer reported since the loop last handed it over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// A relative move, in libinput's (accelerated) units, taken as the
    /// window's logical pixels.
    Relative(f32, f32),
    /// An absolute position, as a fraction of the device's area (0..=1 on
    /// each axis), mapped onto the whole window.
    Absolute(f32, f32),
}

impl Motion {
    /// Adds `next` to what is pending. Relative moves add up; an absolute
    /// position replaces whatever came before it. Returns what must be handed
    /// over first to keep the order, and what is pending now: a relative
    /// move after an absolute position starts from that position, so the
    /// position goes first.
    pub fn coalesce(pending: Option<Motion>, next: Motion) -> (Option<Motion>, Motion) {
        match (pending, next) {
            (Some(Motion::Relative(x, y)), Motion::Relative(dx, dy)) => {
                (None, Motion::Relative(x + dx, y + dy))
            }
            (Some(absolute @ Motion::Absolute(..)), Motion::Relative(..)) => (Some(absolute), next),
            (_, next) => (None, next),
        }
    }
}

/// The pointer, in the window's logical pixels: where it is and whether its
/// arrow is drawn.
///
/// The position is kept to the fraction, so slow movement adds up; the arrow
/// is drawn at a whole pixel that follows it only once it is a pixel away, so
/// a pointer held still -- an air-mouse trembles -- draws nothing.
#[derive(Default)]
pub struct Pointer {
    /// Where the motion has brought it, to the fraction.
    exact: Option<(f32, f32)>,
    /// Where the arrow is drawn.
    drawn: (f32, f32),
    /// Whether the arrow is on the panel: on at a motion, off at a key.
    shown: bool,
}

impl Pointer {
    /// Moves it, kept inside a window of `width` by `height` logical pixels,
    /// and puts the arrow up. A first relative move starts from the middle
    /// of the panel. Returns whether there is anything new to draw.
    pub fn apply(&mut self, motion: Motion, width: f32, height: f32) -> bool {
        let first = self.exact.is_none();
        let (x, y) = match motion {
            Motion::Relative(dx, dy) => {
                let (x, y) = self.exact.unwrap_or((width / 2.0, height / 2.0));
                (x + dx, y + dy)
            }
            Motion::Absolute(fx, fy) => (fx.clamp(0.0, 1.0) * width, fy.clamp(0.0, 1.0) * height),
        };
        let clamp = |v: f32, max: f32| v.clamp(0.0, (max - 1.0).max(0.0));
        let (x, y) = (clamp(x, width), clamp(y, height));
        self.exact = Some((x, y));
        let follow = |exact: f32, drawn: f32| {
            if first || (exact - drawn).abs() >= 1.0 {
                exact.round()
            } else {
                drawn
            }
        };
        let drawn = (follow(x, self.drawn.0), follow(y, self.drawn.1));
        let moved = first || drawn != self.drawn;
        self.drawn = drawn;
        let appeared = !std::mem::replace(&mut self.shown, true);
        moved || appeared
    }

    /// Puts the arrow away. Returns whether it was up.
    pub fn hide(&mut self) -> bool {
        std::mem::replace(&mut self.shown, false)
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Where the motion has brought it, to the fraction, while it is up.
    pub fn exact(&self) -> Option<(f32, f32)> {
        self.exact.filter(|_| self.shown)
    }

    /// Where the arrow is, while it is up.
    pub fn at(&self) -> Option<(f32, f32)> {
        self.exact.filter(|_| self.shown).map(|_| self.drawn)
    }
}

/// How far a hand moves before a press is a drag, in the window's logical
/// pixels: a few, and more on a larger panel, so an air-mouse's tremor and
/// a finger's roll under a click stay a click.
pub fn slop(height: f32) -> f32 {
    (height * 0.012).max(6.0)
}

/// How far a wheel's detent scrolls a surface, in logical pixels: a short
/// row's worth of the panel.
pub fn detent(height: f32) -> f32 {
    (height * 0.075).max(24.0)
}

/// What a press landed on: a target, or a surface's ground.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Target(Zone, usize),
    Area(Lanes),
}

impl Hit {
    pub fn lanes(self) -> Lanes {
        match self {
            Hit::Target(zone, index) => zone.lanes(index),
            Hit::Area(lanes) => lanes,
        }
    }
}

/// Where the button and the hand are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// The button is down: not yet a click, nor a drag. `hit` is what it
    /// went down on, once Slint's hit testing says; `moved` that the hand
    /// has gone further than the slop; `held` that a hold opened a menu.
    Pressed {
        start: (f32, f32),
        at: (f32, f32),
        hit: Option<Hit>,
        moved: bool,
        released: bool,
        held: bool,
    },
    /// The hand moves a surface: `total` is how far along it since `origin`.
    Panning {
        lane: Lane,
        origin: (f32, f32),
        total: f32,
    },
    /// The film's bar has the button (`PointerScrub`): nothing else does.
    Scrub,
    /// Let go while moving: the surface runs on and slows to a stop.
    Coasting {
        lane: Lane,
        total: f32,
        velocity: f32,
        since: f32,
    },
    /// A wheel turns a surface; turns close together are one gesture.
    Wheeling {
        lane: Lane,
        total: f32,
        at: std::time::Instant,
    },
}

/// What the application does about a step of the gesture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    Nothing,
    /// A surface moved: the gesture's number and its running total along
    /// the lane, content following the hand (`PointerPan`).
    Pan {
        lane: Lane,
        gesture: i32,
        total: f32,
    },
    /// A click on a target: its focus, then its Select, once.
    Click(Zone, usize),
}

/// A coast lasts no longer than this, however fast it began.
const COAST_LONGEST: f32 = 1.4;
/// How fast a coast slows: its speed falls by e every this many seconds.
const COAST_TAU: f32 = 0.28;
/// How far back the hand's speed is measured at release, and how long a
/// still hand must have been still to let go without a coast.
const TRAIL: std::time::Duration = std::time::Duration::from_millis(90);

/// The pointer's button and hand, in one place: a press, a drag of a
/// surface, the film's scrub, a coast, a wheel. Everything that ends a
/// gesture -- a cancel, another screen, the remote -- is `cancel`.
pub struct Gesture {
    phase: Phase,
    id: i32,
    context: Option<Context>,
    /// The panel's logical height when the gesture began: the slop, the
    /// coast's speeds, a wheel's detent are fractions of it.
    height: f32,
    trail: Vec<(std::time::Instant, f32)>,
}

impl Default for Gesture {
    fn default() -> Self {
        Gesture {
            phase: Phase::Idle,
            id: 0,
            context: None,
            height: 720.0,
            trail: Vec::new(),
        }
    }
}

impl Gesture {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn id(&self) -> i32 {
        self.id
    }

    /// Whether nothing is pressed nor moving: hover may light a target.
    pub fn idle(&self) -> bool {
        matches!(
            self.phase,
            Phase::Idle | Phase::Coasting { .. } | Phase::Wheeling { .. }
        )
    }

    pub fn coasting(&self) -> bool {
        matches!(self.phase, Phase::Coasting { .. })
    }

    /// The button went down at `at`. Whatever was coasting stops there, as
    /// a finger stops a list.
    pub fn press(&mut self, at: (f32, f32), context: Context, height: f32) {
        self.id += 1;
        self.context = Some(context);
        self.height = height.max(1.0);
        self.trail.clear();
        self.phase = Phase::Pressed {
            start: at,
            at,
            hit: None,
            moved: false,
            released: false,
            held: false,
        };
    }

    /// What the press went down on, as Slint's hit testing says it a little
    /// after the button (the events reach it once the loop hands them
    /// over). Only for the press that is down, on the panel it went down on.
    pub fn hit(&mut self, hit: Hit, context: Context) -> Step {
        if self.context != Some(context) {
            return Step::Nothing;
        }
        let Phase::Pressed {
            hit: slot @ None,
            moved,
            released,
            at,
            ..
        } = &mut self.phase
        else {
            return Step::Nothing;
        };
        *slot = Some(hit);
        let (moved, released, at) = (*moved, *released, *at);
        // The hand was already away before Slint said what was under it:
        // the drag starts from where the hand is now.
        if moved && !released {
            self.begin_pan(hit, at, (0.0, 0.0));
        }
        if matches!(hit, Hit::Area(_)) && released {
            self.phase = Phase::Idle;
        }
        Step::Nothing
    }

    fn begin_pan(&mut self, hit: Hit, at: (f32, f32), away: (f32, f32)) {
        match hit.lanes().along(away.0, away.1) {
            Some(lane) => {
                self.trail.clear();
                self.phase = Phase::Panning {
                    lane,
                    origin: at,
                    total: 0.0,
                };
            }
            // Nothing to move: no click either.
            None => {
                if let Phase::Pressed { moved, .. } = &mut self.phase {
                    *moved = true;
                }
            }
        }
    }

    /// The hand moved to `at`, in the window's logical pixels.
    pub fn motion(&mut self, at: (f32, f32), now: std::time::Instant) -> Step {
        let slop = slop(self.height);
        match &mut self.phase {
            Phase::Pressed {
                start,
                at: here,
                hit,
                moved,
                released,
                ..
            } => {
                *here = at;
                if *moved || *released {
                    return Step::Nothing;
                }
                let away = (at.0 - start.0, at.1 - start.1);
                if away.0.hypot(away.1) <= slop {
                    return Step::Nothing;
                }
                *moved = true;
                if let Some(hit) = *hit {
                    self.begin_pan(hit, at, away);
                }
                Step::Nothing
            }
            Phase::Panning {
                lane,
                origin,
                total,
            } => {
                let lane = *lane;
                *total = lane.along((at.0 - origin.0, at.1 - origin.1));
                let total = *total;
                self.sample(now, total);
                Step::Pan {
                    lane,
                    gesture: self.id,
                    total,
                }
            }
            _ => Step::Nothing,
        }
    }

    /// The button came up (the platform's word, before Slint's). A drag let
    /// go while moving coasts -- not a short list drawn a row at a time,
    /// which stops where the hand left it; a press on a target waits for the
    /// target's own release, which is the click (`target_up`).
    pub fn release(&mut self, now: std::time::Instant) {
        match self.phase {
            Phase::Panning { lane, total, .. } => {
                let velocity = self.velocity(now);
                let height = self.height;
                self.phase = if velocity.abs() >= height * 0.3 && !lane.stepped() {
                    Phase::Coasting {
                        lane,
                        total,
                        velocity: velocity.clamp(-height * 4.0, height * 4.0),
                        since: 0.0,
                    }
                } else {
                    Phase::Idle
                };
            }
            Phase::Pressed {
                hit: Some(Hit::Area(_)),
                ..
            } => self.phase = Phase::Idle,
            Phase::Pressed {
                ref mut released, ..
            } => *released = true,
            _ => {}
        }
    }

    /// The hand's speed along its lane over the last moment, in logical
    /// pixels a second; nothing if it had stopped before it let go.
    fn velocity(&self, now: std::time::Instant) -> f32 {
        let (Some(first), Some(last)) = (self.trail.first(), self.trail.last()) else {
            return 0.0;
        };
        if now.duration_since(last.0) > TRAIL / 2 {
            return 0.0;
        }
        let span = last.0.duration_since(first.0).as_secs_f32();
        if span < 0.008 {
            return 0.0;
        }
        (last.1 - first.1) / span
    }

    /// A target's own release: a click if the button went down on it, the
    /// hand stayed within the slop, and no hold took it.
    pub fn target_up(&mut self, zone: Zone, index: usize, context: Context) -> Step {
        let Phase::Pressed {
            hit: Some(Hit::Target(down_zone, down_index)),
            moved,
            held,
            ..
        } = self.phase
        else {
            return Step::Nothing;
        };
        if (down_zone, down_index) != (zone, index) {
            return Step::Nothing;
        }
        self.phase = Phase::Idle;
        if moved || held || self.context != Some(context) {
            return Step::Nothing;
        }
        Step::Click(zone, index)
    }

    /// The press is the film's bar's: no click, no drag of anything else.
    pub fn scrub(&mut self) {
        self.phase = Phase::Scrub;
    }

    /// The target a press of gesture `id` is still held on, without having
    /// moved or come up: a long press may be the remote's long Ok there.
    pub fn holding(&self, id: i32) -> Option<(Zone, usize)> {
        match self.phase {
            Phase::Pressed {
                hit: Some(Hit::Target(zone, index)),
                moved: false,
                released: false,
                held: false,
                ..
            } if id == self.id => Some((zone, index)),
            _ => None,
        }
    }

    /// A hold opened a menu: the release that follows is no click.
    pub fn held(&mut self) {
        if let Phase::Pressed { held, .. } = &mut self.phase {
            *held = true;
        }
    }

    /// Slint has seen every pointer event handed to it so far: a press it
    /// put on no target and no surface's ground -- it said nothing about it
    /// -- that has come up is over, and was nothing. Only after the queue is
    /// drained (`platform::deliver_pointer`), never in `release`: the
    /// press's hit may still be on its way in the same queue, and its
    /// target's release is the click. Returns whether it ended one.
    pub fn settle(&mut self) -> bool {
        if matches!(
            self.phase,
            Phase::Pressed {
                hit: None,
                released: true,
                ..
            }
        ) {
            self.phase = Phase::Idle;
            return true;
        }
        false
    }

    /// Everything is let go of: nothing clicks, nothing coasts. Returns
    /// whether anything was going on.
    pub fn cancel(&mut self) -> bool {
        let busy = self.phase != Phase::Idle;
        self.phase = Phase::Idle;
        busy
    }

    /// A coast's next moment, `dt` seconds on. The last step is the one
    /// that stops it.
    pub fn coast(&mut self, dt: f32) -> Step {
        let Phase::Coasting {
            lane,
            total,
            velocity,
            since,
        } = &mut self.phase
        else {
            return Step::Nothing;
        };
        let dt = dt.clamp(0.0, 0.1);
        let decay = (-dt / COAST_TAU).exp();
        // The distance the decaying speed covers over dt.
        *total += *velocity * COAST_TAU * (1.0 - decay);
        *velocity *= decay;
        *since += dt;
        let step = Step::Pan {
            lane: *lane,
            gesture: self.id,
            total: *total,
        };
        if velocity.abs() < self.height * 0.05 || *since >= COAST_LONGEST {
            self.phase = Phase::Idle;
        }
        step
    }

    /// The surface a coast moves ran into its end: it stops there.
    pub fn edge(&mut self, at: Lane) {
        if matches!(self.phase, Phase::Coasting { lane, .. } if lane == at) {
            self.phase = Phase::Idle;
        }
    }

    /// A wheel moved `lane` by `pixels` (positive: further down or along).
    /// Turns close together are one gesture, so the surface eases on from
    /// where the last left it. Not while the button is down.
    pub fn wheel(&mut self, lane: Lane, pixels: f32, now: std::time::Instant) -> Step {
        let total = match self.phase {
            Phase::Wheeling {
                lane: was,
                total,
                at,
            } if was == lane && now.duration_since(at) < std::time::Duration::from_millis(400) => {
                total
            }
            Phase::Idle | Phase::Coasting { .. } | Phase::Wheeling { .. } => {
                self.id += 1;
                self.trail.clear();
                0.0
            }
            _ => return Step::Nothing,
        };
        // Content follows the hand: scrolling down moves it up.
        let total = total - pixels;
        self.phase = Phase::Wheeling {
            lane,
            total,
            at: now,
        };
        // The fingers' speed, for a coast when they are lifted (`lift`).
        self.sample(now, total);
        Step::Pan {
            lane,
            gesture: self.id,
            total,
        }
    }

    /// A scroll sequence ended (`Scroll::Ended`) on the axes `stopped`
    /// (horizontal, vertical). Fingers lifted off a touchpad while moving
    /// leave a pixel surface coasting, as a drag let go while moving does
    /// (`release`); a list drawn a row at a time stops where they left it,
    /// and so does anything else (a continuous scroll). The panel is
    /// `height` logical pixels tall: the coast's speeds are fractions of
    /// it. Returns whether the surface coasts now.
    pub fn lift(
        &mut self,
        stopped: (bool, bool),
        fingers: bool,
        height: f32,
        now: std::time::Instant,
    ) -> bool {
        let Phase::Wheeling { lane, total, .. } = self.phase else {
            return false;
        };
        let along = match lane.axis {
            Axis::Horizontal => stopped.0,
            Axis::Vertical => stopped.1,
        };
        if !along {
            return false;
        }
        self.height = height.max(1.0);
        let velocity = self.velocity(now);
        self.phase = if fingers && !lane.stepped() && velocity.abs() >= self.height * 0.3 {
            Phase::Coasting {
                lane,
                total,
                velocity: velocity.clamp(-self.height * 4.0, self.height * 4.0),
                since: 0.0,
            }
        } else {
            Phase::Idle
        };
        self.coasting()
    }

    /// Where the hand or the fingers have taken the surface, at `now`: the
    /// last moment of it is kept for its speed (`velocity`).
    fn sample(&mut self, now: std::time::Instant, total: f32) {
        self.trail.push((now, total));
        if let Some(keep) = self
            .trail
            .iter()
            .position(|(t, _)| now.duration_since(*t) <= TRAIL)
        {
            // Keep one sample before the window, for a speed across it.
            self.trail.drain(..keep.saturating_sub(1));
        }
    }
}

/// What a pointer scrolled, as libinput reports it, positive down and
/// right: the device's own direction, natural scrolling included, as the
/// platform configured it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scroll {
    /// A wheel's detents, as libinput's v120 values (120 is one detent; a
    /// high-resolution wheel reports fractions of one).
    Wheel(f64, f64),
    /// Fingers on a touchpad, in libinput's scroll units -- its motion's.
    Finger(f64, f64),
    /// A scroll that is neither: a trackball's, a button held while moving.
    Continuous(f64, f64),
    /// A scroll sequence ended on the axes `stopped` (horizontal,
    /// vertical): libinput's terminating event, a value of 0 on every axis
    /// it has. Guaranteed for fingers and for a continuous scroll, never
    /// for a wheel (`Scroll::of_axes`). `fingers`: they were lifted off a
    /// touchpad, which may leave the surface coasting (`Gesture::lift`).
    Ended { fingers: bool, stopped: (bool, bool) },
}

impl Scroll {
    /// A finger or continuous scroll event as libinput reports it: each
    /// axis's value, or `None` where the event does not have the axis. An
    /// event whose every axis is 0 is the sequence's end; otherwise an axis
    /// it does not have is no movement on it.
    pub fn of_axes(fingers: bool, horizontal: Option<f64>, vertical: Option<f64>) -> Scroll {
        let has = (horizontal.is_some(), vertical.is_some());
        let zero = |value: Option<f64>| value.is_none_or(|value| value == 0.0);
        if (has.0 || has.1) && zero(horizontal) && zero(vertical) {
            return Scroll::Ended {
                fingers,
                stopped: has,
            };
        }
        let (h, v) = (horizontal.unwrap_or(0.0), vertical.unwrap_or(0.0));
        if fingers {
            Scroll::Finger(h, v)
        } else {
            Scroll::Continuous(h, v)
        }
    }
}

/// A scroll, in what a surface moves by and what the arrows step by.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scrolled {
    /// Logical pixels, positive down and right.
    pub pixels: (f32, f32),
    /// Whole steps completed: a detent each, or a detent's worth of
    /// fingers. For a list that moves a row at a time, and for the arrows
    /// where nothing scrolls.
    pub steps: (i32, i32),
    /// A wheel's step, eased; fingers are followed.
    pub eased: bool,
}

/// Scrolls, normalised: a wheel's detents and a touchpad's fingers move a
/// surface the same way, and add up to the same whole steps.
#[derive(Default)]
pub struct Scroller {
    /// Part-detents, in v120.
    wheel: (i32, i32),
    /// Part-steps of fingers, in pixels.
    fingers: (f32, f32),
}

impl Scroller {
    /// Adds a report, with a detent `detent` logical pixels long.
    pub fn add(&mut self, scroll: Scroll, detent: f32) -> Scrolled {
        match scroll {
            Scroll::Wheel(horizontal, vertical) => {
                let take = |pending: &mut i32, value: f64| {
                    *pending += value.round() as i32;
                    let steps = *pending / 120;
                    *pending -= steps * 120;
                    steps
                };
                Scrolled {
                    pixels: (
                        (horizontal / 120.0) as f32 * detent,
                        (vertical / 120.0) as f32 * detent,
                    ),
                    steps: (
                        take(&mut self.wheel.0, horizontal),
                        take(&mut self.wheel.1, vertical),
                    ),
                    eased: true,
                }
            }
            Scroll::Finger(horizontal, vertical) | Scroll::Continuous(horizontal, vertical) => {
                let (horizontal, vertical) = (horizontal as f32, vertical as f32);
                let take = |pending: &mut f32, value: f32| {
                    *pending += value;
                    let steps = (*pending / detent).trunc();
                    *pending -= steps * detent;
                    steps as i32
                };
                Scrolled {
                    pixels: (horizontal, vertical),
                    steps: (
                        take(&mut self.fingers.0, horizontal),
                        take(&mut self.fingers.1, vertical),
                    ),
                    eased: false,
                }
            }
            // A part-step left over is the sequence's own: the next one
            // starts from nothing.
            Scroll::Ended { .. } => {
                self.fingers = (0.0, 0.0);
                Scrolled::default()
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Scroller::default();
    }
}

/// Where a list drawn as a window of rows Rust keeps starts while a pointer
/// is in use: where the remote's focus put it when the pointer came, moved
/// only by the hand and the wheel, never by a click moving the focus. The
/// remote picked up again forgets it, and the focus's window is drawn.
#[derive(Default)]
pub struct Windows {
    /// A list's first row, once drawn under a pointer, and rows the hand
    /// moved it by since.
    lists: Vec<(Zone, Option<usize>, i32)>,
}

impl Windows {
    /// The first row to draw of `total`, `shown` at a time, where the
    /// remote's focus would start it at `focus_first`.
    pub fn first(
        &mut self,
        zone: Zone,
        focus_first: usize,
        total: usize,
        shown: usize,
        pinned: bool,
    ) -> usize {
        let last = total.saturating_sub(shown);
        if !pinned {
            self.lists.retain(|(list, ..)| *list != zone);
            return focus_first.min(last);
        }
        let at = match self.lists.iter().position(|(list, ..)| *list == zone) {
            Some(at) => at,
            None => {
                self.lists.push((zone, None, 0));
                self.lists.len() - 1
            }
        };
        let (_, first, moved) = &mut self.lists[at];
        let wanted = first.unwrap_or(focus_first) as i64 + *moved as i64;
        let drawn = wanted.clamp(0, last as i64) as usize;
        *first = Some(drawn);
        *moved = 0;
        drawn
    }

    /// The hand moved a list by `rows`, positive further down.
    pub fn step(&mut self, zone: Zone, rows: i32) {
        match self.lists.iter_mut().find(|(list, ..)| *list == zone) {
            Some((_, _, moved)) => *moved += rows,
            None => self.lists.push((zone, None, rows)),
        }
    }

    pub fn clear(&mut self) {
        self.lists.clear();
    }
}

/// What the surfaces a pointer moved show, by their first item: the window
/// artwork is loaded around, and a catalogue's next page asked for at, are
/// where the panel is, not where the remote's focus was left. A surface
/// back with the remote's focus is not here.
#[derive(Default)]
pub struct Views {
    shown: Vec<(Zone, i32, usize)>,
}

impl Views {
    /// A surface now shows `first` onwards; negative: it is the focus's
    /// again. Returns whether anything changed.
    pub fn set(&mut self, zone: Zone, lane: i32, first: i32) -> bool {
        let at = self
            .shown
            .iter()
            .position(|(z, l, _)| (*z, *l) == (zone, lane));
        match (at, usize::try_from(first)) {
            (Some(at), Ok(first)) if self.shown[at].2 == first => false,
            (Some(at), Ok(first)) => {
                self.shown[at].2 = first;
                true
            }
            (None, Ok(first)) => {
                self.shown.push((zone, lane, first));
                true
            }
            (Some(at), Err(_)) => {
                self.shown.remove(at);
                true
            }
            (None, Err(_)) => false,
        }
    }

    /// The first item `zone`'s lane shows, if a pointer moved it.
    pub fn first(&self, zone: Zone, lane: i32) -> Option<usize> {
        self.shown
            .iter()
            .find(|(z, l, _)| (*z, *l) == (zone, lane))
            .map(|(.., first)| *first)
    }

    pub fn clear(&mut self) {
        self.shown.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 1920.0;
    const H: f32 = 1080.0;

    const ALL_ZONES: [Zone; 71] = [
        Zone::DetailActions,
        Zone::DetailSources,
        Zone::DetailEpisodes,
        Zone::DetailFilter,
        Zone::DetailProviders,
        Zone::DetailSeasons,
        Zone::DetailMarks,
        Zone::Shield,
        Zone::MediaMenu,
        Zone::PosterDismiss,
        Zone::PosterMenu,
        Zone::DiscoverFilters,
        Zone::DiscoverOptions,
        Zone::DiscoverPanel,
        Zone::LibraryFilters,
        Zone::LibraryOptions,
        Zone::LibraryMenu,
        Zone::LibraryMark,
        Zone::FrameAccount,
        Zone::FrameAccountMenu,
        Zone::EditorVolume,
        Zone::HomeApps,
        Zone::QuietHome,
        Zone::SettingsSections,
        Zone::Back,
        Zone::GoHome,
        Zone::SettingsRows,
        Zone::SettingsChoice,
        Zone::EditorCards,
        Zone::EditorActions,
        Zone::OutputResolutions,
        Zone::OutputRates,
        Zone::OutputColours,
        Zone::CoolingChips,
        Zone::CoolingCells,
        Zone::EditorPicker,
        Zone::EditorPad,
        Zone::EditorConfirm,
        Zone::DiagnosticsGroups,
        Zone::FramePlaces,
        Zone::FrameSearch,
        Zone::Shelves,
        Zone::Grid,
        Zone::SearchKeys,
        Zone::SearchList,
        Zone::MsCategories,
        Zone::MsControls,
        Zone::MsOptions,
        Zone::Sheet,
        Zone::AccountForm,
        Zone::AccountKeys,
        Zone::WifiRows,
        Zone::WifiInfo,
        Zone::WifiButtons,
        Zone::WifiForm,
        Zone::WifiKeys,
        Zone::BtRows,
        Zone::NpControls,
        Zone::PlayerBar,
        Zone::PlayerButtons,
        Zone::PlayerExit,
        Zone::PlayerMenu,
        Zone::PlayerTracks,
        Zone::PlayerSide,
        Zone::LiveSurface,
        Zone::LiveMenu,
        Zone::LiveGuide,
        Zone::LivePanel,
        Zone::LiveKeypad,
        Zone::LiveSetup,
        Zone::LiveActions,
    ];

    /// A zone nobody draws is a part of a screen the pointer cannot reach.
    #[test]
    fn every_zone_is_named_in_the_interface() {
        let ui = [
            include_str!("../ui/home.slint"),
            include_str!("../ui/settings.slint"),
            include_str!("../ui/kit.slint"),
            include_str!("../ui/components.slint"),
            include_str!("../ui/audio.slint"),
            include_str!("../ui/ethernet.slint"),
            include_str!("../ui/output.slint"),
            include_str!("../ui/cooling.slint"),
            include_str!("../ui/diagnostics.slint"),
            include_str!("../ui/chrome.slint"),
            include_str!("../ui/media.slint"),
            include_str!("../ui/search.slint"),
            include_str!("../ui/discover.slint"),
            include_str!("../ui/library.slint"),
            include_str!("../ui/media_settings.slint"),
            include_str!("../ui/detail.slint"),
            include_str!("../ui/pointer.slint"),
            include_str!("../ui/account.slint"),
            include_str!("../ui/wireless.slint"),
            include_str!("../ui/now_playing.slint"),
            include_str!("../ui/player.slint"),
            include_str!("../ui/live.slint"),
            include_str!("../ui/quiet/home.slint"),
            include_str!("../ui/quiet/settings.slint"),
            include_str!("../ui/quiet/controls.slint"),
        ]
        .concat();
        for zone in ALL_ZONES {
            assert!(
                ui.contains(&format!("\"{}\"", zone_name(zone))),
                "{zone:?} is drawn nowhere"
            );
        }
    }

    /// What a click is, zone by zone: the remote's own key through its own
    /// road. A modal's ground and a way back are Back; the season bar's ends
    /// and the volume bar's halves are Left and Right; a poster's cross and
    /// menu mark are never Ok (which would open the title); a diagnostics
    /// group is read, so a click on it is nothing.
    #[test]
    fn a_click_is_the_remote_s_own_key_for_what_is_under_it() {
        use mediabox_core::InputAction;
        assert_eq!(
            Zone::Shield.activation(0),
            Activation::Key(InputAction::Back)
        );
        assert_eq!(Zone::Back.activation(0), Activation::Key(InputAction::Back));
        // Out of the film is the remote's Stop: its Back over raised
        // controls only lowers them.
        assert_eq!(
            Zone::PlayerExit.activation(0),
            Activation::Key(InputAction::Stop)
        );
        // A live menu row's title brings the row forward and chooses nothing.
        assert_eq!(Zone::LiveMenu.activation(ROW_TITLE), Activation::Nothing);
        assert_eq!(
            Zone::LiveMenu.activation(102),
            Activation::Key(InputAction::Ok)
        );
        // The bar is dragged; a press on it is the scrub's, not a key.
        assert_eq!(Zone::PlayerBar.activation(0), Activation::Nothing);
        assert_eq!(
            Zone::DetailSeasons.activation(0),
            Activation::Key(InputAction::Left)
        );
        assert_eq!(
            Zone::DetailSeasons.activation(1),
            Activation::Key(InputAction::Right)
        );
        assert_eq!(
            Zone::EditorVolume.activation(0),
            Activation::Key(InputAction::Left)
        );
        assert_eq!(
            Zone::EditorVolume.activation(1),
            Activation::Key(InputAction::Right)
        );
        assert_eq!(Zone::PosterDismiss.activation(0), Activation::DismissPoster);
        assert_eq!(Zone::PosterMenu.activation(0), Activation::OpenMenu);
        assert_eq!(Zone::LibraryMark.activation(0), Activation::OpenMenu);
        assert_eq!(Zone::DiagnosticsGroups.activation(0), Activation::Nothing);
        for zone in [
            Zone::Shelves,
            Zone::DetailMarks,
            Zone::DetailFilter,
            Zone::DetailProviders,
            Zone::MediaMenu,
            Zone::LibraryMenu,
            Zone::DiscoverOptions,
            Zone::FrameAccount,
            Zone::FrameAccountMenu,
            Zone::EditorConfirm,
            Zone::AccountForm,
            Zone::AccountKeys,
            Zone::WifiRows,
            Zone::WifiInfo,
            Zone::WifiButtons,
            Zone::WifiForm,
            Zone::WifiKeys,
            Zone::BtRows,
            Zone::NpControls,
            Zone::PlayerButtons,
            Zone::PlayerMenu,
            Zone::PlayerTracks,
            Zone::PlayerSide,
            Zone::LiveSurface,
            Zone::LiveGuide,
            Zone::LivePanel,
            Zone::LiveKeypad,
            Zone::LiveSetup,
            Zone::LiveActions,
        ] {
            assert_eq!(
                zone.activation(0),
                Activation::Key(InputAction::Ok),
                "{zone:?}"
            );
        }
        // The arrow lights controls, never a ground that fills the panel.
        assert!(Zone::FrameAccount.lights() && Zone::Back.lights() && Zone::PlayerExit.lights());
        assert!(!Zone::Shield.lights() && !Zone::LiveSurface.lights());
    }

    /// Every modal of every pointer screen has a ground that takes the
    /// pointer, and a click on it is Back; with nothing over the screen
    /// there is no ground, and where the pointer is disabled not even that.
    #[test]
    fn every_modal_has_a_ground_that_is_back_and_only_while_it_is_up() {
        for route in NAVIGATION {
            assert!(
                !accepts(on(route), Zone::Shield),
                "{route:?}: no modal, no ground"
            );
            for layer in [Layer::Choice, Layer::Sheet] {
                assert!(
                    accepts(Context { layer, ..on(route) }, Zone::Shield),
                    "{route:?} {layer:?}"
                );
            }
        }
        for route in DISABLED {
            assert!(
                !accepts(
                    Context {
                        layer: Layer::Sheet,
                        ..on(route)
                    },
                    Zone::Shield
                ),
                "{route:?}"
            );
        }
    }

    /// A target of its own inside a card -- a poster's cross and menu mark,
    /// an episode's watched button -- is drawn after (over) the card's own
    /// target, which Slint's hit testing takes as the one under the arrow.
    /// The other way round the card would take every click.
    #[test]
    fn a_card_s_own_buttons_are_over_the_card_s_target() {
        let component = |file: &str, name: &str| {
            let at = file
                .find(&format!("component {name} inherits"))
                .expect(name);
            file[at..at + file[at..].find("\n}\n").expect("its end")].to_string()
        };
        let components = include_str!("../ui/components.slint");
        let poster = component(components, "Poster");
        let main = poster
            .find("zone: root.pointer-zone;")
            .expect("the poster's own target");
        for own in ["\"poster-dismiss\"", "\"poster-menu\""] {
            assert!(
                poster.find(own).expect(own) > main,
                "{own} is under the poster's target"
            );
        }
        let detail = include_str!("../ui/detail.slint");
        let episode = component(detail, "EpisodeLine");
        let main = episode
            .find("zone: root.pointer-zone;")
            .expect("the episode's own target");
        assert!(
            episode
                .find("\"detail-marks\"")
                .expect("the watched button")
                > main
        );
    }

    /// Every modal drawn over a pointer screen has a ground: the shared
    /// dialog, and each screen's own menus and lists.
    #[test]
    fn every_modal_is_drawn_with_a_ground() {
        let kit = include_str!("../ui/kit.slint");
        let dialog = &kit[kit.find("component Dialog inherits").expect("Dialog")..];
        assert!(
            dialog[..dialog.find("\n}\n").unwrap()].contains("PointerShield"),
            "Dialog"
        );
        for (file, source) in [
            ("media", include_str!("../ui/media.slint")),
            ("library", include_str!("../ui/library.slint")),
            ("discover", include_str!("../ui/discover.slint")),
            ("detail", include_str!("../ui/detail.slint")),
            ("search", include_str!("../ui/search.slint")),
            ("media_settings", include_str!("../ui/media_settings.slint")),
            ("chrome", include_str!("../ui/chrome.slint")),
            ("cooling", include_str!("../ui/cooling.slint")),
            ("live", include_str!("../ui/live.slint")),
            ("player", include_str!("../ui/player.slint")),
        ] {
            assert!(
                source.contains("PointerShield"),
                "{file} draws a modal without a ground"
            );
        }
    }

    /// Each screen's modal zones are taken only while that modal is up, and
    /// its screen's zones only while it is not.
    #[test]
    fn a_screen_s_menus_and_lists_belong_to_the_modal_layer() {
        let modal = |route| Context {
            layer: Layer::Choice,
            ..on(route)
        };
        for (route, over, under) in [
            (Route::Media, Zone::MediaMenu, Zone::Shelves),
            (Route::Media, Zone::MediaMenu, Zone::PosterDismiss),
            (
                Route::Discover,
                Zone::DiscoverOptions,
                Zone::DiscoverFilters,
            ),
            (Route::Discover, Zone::DiscoverOptions, Zone::DiscoverPanel),
            (Route::Library, Zone::LibraryOptions, Zone::LibraryFilters),
            (Route::Library, Zone::LibraryMenu, Zone::Grid),
            (Route::Detail, Zone::DetailProviders, Zone::DetailSources),
            (Route::Search, Zone::SearchKeys, Zone::Shelves),
            (Route::Search, Zone::SearchList, Zone::FramePlaces),
            (Route::MediaSettings, Zone::MsOptions, Zone::MsControls),
            (Route::Media, Zone::FrameAccountMenu, Zone::FrameAccount),
            (Route::Library, Zone::LibraryMenu, Zone::LibraryMark),
            (Route::Live, Zone::LiveMenu, Zone::LiveSurface),
            (Route::Live, Zone::LiveGuide, Zone::LiveSurface),
            (Route::Live, Zone::LivePanel, Zone::LiveSurface),
            (Route::Live, Zone::LiveKeypad, Zone::LiveSurface),
            (Route::Live, Zone::LiveSetup, Zone::LiveSurface),
            (Route::Live, Zone::LiveActions, Zone::LiveSurface),
        ] {
            assert!(
                accepts(modal(route), over) && !accepts(on(route), over),
                "{route:?} {over:?}"
            );
            assert!(
                accepts(on(route), under) && !accepts(modal(route), under),
                "{route:?} {under:?}"
            );
        }
    }

    #[test]
    fn a_target_that_cannot_take_the_focus_is_refused_and_not_pressed() {
        let mut at = 2;
        assert_eq!(Aim::set(&mut at, 2, true), Aim::Here);
        assert_eq!(Aim::set(&mut at, 3, true), Aim::Moved);
        assert_eq!(at, 3);
        assert_eq!(Aim::set(&mut at, 1, false), Aim::Refused);
        assert_eq!(at, 3);
        assert!(Aim::Moved.taken() && Aim::Here.taken() && !Aim::Refused.taken());
        assert_eq!(cell(304), (3, 4));
    }

    fn on(route: Route) -> Context {
        Context {
            route,
            layer: Layer::Screen,
            film: false,
            handed_over: false,
        }
    }

    const NAVIGATION: [Route; 14] = [
        Route::Account,
        Route::Wifi,
        Route::Bluetooth,
        Route::NowPlaying,
        Route::Live,
        Route::Home,
        Route::Settings,
        Route::Diagnostics,
        Route::Media,
        Route::Discover,
        Route::Library,
        Route::Search,
        Route::MediaSettings,
        Route::Detail,
    ];
    const DISABLED: [Route; 1] = [Route::Boot];

    #[test]
    fn the_table_says_which_screens_take_a_pointer() {
        for route in NAVIGATION {
            assert_eq!(policy(on(route)), Policy::Navigation, "{route:?}");
        }
        for route in DISABLED {
            assert_eq!(policy(on(route)), Policy::Disabled, "{route:?}");
        }
    }

    /// Every Navigation screen takes at least one zone, and a Disabled one
    /// none -- so a screen is never pointer-enabled with nothing to point
    /// at, nor drawn targets on a screen the table switched off.
    #[test]
    fn every_navigation_screen_has_targets_and_no_disabled_one_does() {
        for route in NAVIGATION {
            assert!(
                ALL_ZONES.iter().any(|zone| accepts(on(route), *zone)),
                "{route:?} has no target"
            );
        }
        for route in DISABLED {
            for layer in [Layer::Screen, Layer::Choice, Layer::Sheet] {
                assert!(
                    !ALL_ZONES
                        .iter()
                        .any(|zone| accepts(Context { layer, ..on(route) }, *zone)),
                    "{route:?}"
                );
            }
        }
    }

    /// A film this interface plays is its player, whatever screen is
    /// under it, and the player is Full: its bar is dragged. Another
    /// application on the panel reads its own input.
    #[test]
    fn a_film_is_its_player_and_another_application_disables_the_pointer() {
        let film = Context {
            film: true,
            ..on(Route::Detail)
        };
        assert_eq!(policy(film), Policy::Full);
        for zone in [Zone::PlayerBar, Zone::PlayerButtons, Zone::PlayerExit] {
            assert!(accepts(film, zone), "{zone:?}");
            assert!(!accepts(on(Route::Detail), zone), "{zone:?}");
        }
        // The page the film was started from is not drawn under it.
        assert!(!accepts(film, Zone::DetailActions));
        // A panel over the film is its modal layer.
        let panel = Context {
            layer: Layer::Choice,
            ..film
        };
        for zone in [Zone::PlayerMenu, Zone::PlayerTracks, Zone::PlayerSide] {
            assert!(accepts(panel, zone) && !accepts(film, zone), "{zone:?}");
        }
        assert!(!accepts(panel, Zone::PlayerButtons));
        assert!(accepts(panel, Zone::Shield));
        assert_eq!(
            policy(Context {
                handed_over: true,
                film: true,
                ..on(Route::Home)
            }),
            Policy::Disabled
        );
        assert_eq!(
            policy(Context {
                handed_over: true,
                ..on(Route::Home)
            }),
            Policy::Disabled
        );
        assert!(!accepts(
            Context {
                handed_over: true,
                ..on(Route::Home)
            },
            Zone::HomeApps
        ));
    }

    /// Disabled is no target at all: no hover, no click, whatever is under
    /// the arrow.
    #[test]
    fn a_disabled_screen_takes_no_zone() {
        for zone in ALL_ZONES {
            assert!(!accepts(on(Route::Boot), zone), "{zone:?}");
            assert!(
                !accepts(
                    Context {
                        layer: Layer::Sheet,
                        ..on(Route::Boot)
                    },
                    zone
                ),
                "{zone:?}"
            );
        }
    }

    /// The native screens that were switched off take their own targets
    /// now, and only their own.
    #[test]
    fn the_radios_the_account_and_kodi_s_film_take_their_own_targets() {
        for (route, zones) in [
            (Route::Account, vec![Zone::AccountForm, Zone::AccountKeys]),
            (
                Route::Wifi,
                vec![
                    Zone::WifiRows,
                    Zone::WifiInfo,
                    Zone::WifiButtons,
                    Zone::WifiForm,
                    Zone::WifiKeys,
                ],
            ),
            (Route::Bluetooth, vec![Zone::BtRows]),
            (Route::NowPlaying, vec![Zone::NpControls]),
        ] {
            for zone in &zones {
                assert!(accepts(on(route), *zone), "{route:?} {zone:?}");
                assert!(!accepts(on(Route::Home), *zone), "{zone:?} off its screen");
                // A sheet over it (the radios' peer menu, power) keeps it.
                assert!(!accepts(
                    Context {
                        layer: Layer::Sheet,
                        ..on(route)
                    },
                    *zone
                ));
            }
            assert!(accepts(on(route), Zone::Back), "{route:?} has a way back");
        }
    }

    #[test]
    fn a_zone_is_taken_on_its_own_screen_only() {
        assert!(accepts(on(Route::Home), Zone::HomeApps));
        assert!(!accepts(on(Route::Home), Zone::SettingsRows));
        assert!(accepts(on(Route::Settings), Zone::SettingsSections));
        assert!(accepts(on(Route::Settings), Zone::SettingsRows));
        assert!(!accepts(on(Route::Settings), Zone::HomeApps));
    }

    /// A sheet or a list over a screen is the only thing the pointer reaches:
    /// the screen under it takes neither hover nor a click.
    #[test]
    fn a_modal_keeps_the_screen_under_it_from_the_pointer() {
        let sheet = Context {
            layer: Layer::Sheet,
            ..on(Route::Home)
        };
        assert!(accepts(sheet, Zone::Sheet));
        assert!(!accepts(sheet, Zone::HomeApps));
        let choice = Context {
            layer: Layer::Choice,
            ..on(Route::Settings)
        };
        assert!(accepts(choice, Zone::SettingsChoice));
        assert!(!accepts(choice, Zone::SettingsRows));
        assert!(!accepts(choice, Zone::SettingsSections));
        assert!(!accepts(on(Route::Settings), Zone::SettingsChoice));
        // An editor's own list, pad or question is the same: over its page.
        for over in [Zone::EditorPicker, Zone::EditorPad, Zone::EditorConfirm] {
            assert!(accepts(choice, over), "{over:?}");
            assert!(!accepts(on(Route::Settings), over), "{over:?}");
        }
        for under in [
            Zone::EditorCards,
            Zone::EditorActions,
            Zone::OutputResolutions,
            Zone::OutputRates,
            Zone::OutputColours,
            Zone::CoolingChips,
            Zone::CoolingCells,
        ] {
            assert!(!accepts(choice, under), "{under:?}");
            assert!(accepts(on(Route::Settings), under), "{under:?}");
        }
        // A way back is Back on whatever is on top: under a modal it is
        // under the modal's ground, which is Back too.
        assert!(accepts(choice, Zone::Back) && accepts(sheet, Zone::Back));
        assert!(!accepts(on(Route::Home), Zone::Sheet));
    }

    /// Another screen, a sheet coming up or going, a film: nothing the
    /// pointer was over or pressing is carried across, and where the
    /// pointer is disabled the arrow goes too.
    #[test]
    fn a_change_of_screen_or_policy_forgets_the_pointer_s_state() {
        let home = on(Route::Home);
        assert_eq!(transition(Some(home), home), Transition::Keep);
        assert_eq!(
            transition(Some(home), on(Route::Settings)),
            Transition::Forget
        );
        assert_eq!(
            transition(
                Some(home),
                Context {
                    layer: Layer::Sheet,
                    ..home
                }
            ),
            Transition::Forget
        );
        assert_eq!(transition(Some(home), on(Route::Boot)), Transition::Hide);
        assert_eq!(
            transition(Some(home), Context { film: true, ..home }),
            Transition::Forget
        );
        assert_eq!(
            transition(
                Some(home),
                Context {
                    handed_over: true,
                    ..home
                }
            ),
            Transition::Hide
        );
        assert_eq!(transition(None, on(Route::Boot)), Transition::Hide);
        assert_eq!(transition(None, home), Transition::Forget);
    }

    /// The table names every route: a new one does not compile until it has
    /// a policy, because there is no catch-all to fall into.
    #[test]
    fn the_table_has_no_catch_all() {
        let module = include_str!("pointer.rs");
        let begin = module.find("pub fn route_policy").expect("the table");
        let end = module[begin..].find("\n}\n").expect("its end") + begin;
        assert!(
            !module[begin..end].contains("_ =>"),
            "route_policy has a catch-all"
        );
    }

    #[test]
    fn zones_are_named_as_the_interface_names_them() {
        for zone in ALL_ZONES {
            assert_eq!(Zone::parse(zone_name(zone)), Some(zone));
        }
        assert_eq!(Zone::parse(""), None);
        assert_eq!(Zone::parse("media"), None);
    }

    #[test]
    fn a_relative_pointer_starts_in_the_middle_and_the_edges_hold() {
        let mut pointer = Pointer::default();
        assert_eq!(pointer.at(), None);
        assert!(!pointer.shown());
        assert!(pointer.apply(Motion::Relative(0.0, 0.0), W, H));
        assert_eq!(pointer.at(), Some((960.0, 540.0)));
        assert!(pointer.apply(Motion::Relative(-5000.0, 5000.0), W, H));
        assert_eq!(pointer.at(), Some((0.0, 1079.0)));
        // Pushed further into the corner, nothing moves and nothing is drawn.
        assert!(!pointer.apply(Motion::Relative(-10.0, 10.0), W, H));
    }

    /// A pointer held still trembles. Under a pixel is not a move, and a
    /// move is not a frame; but the fractions add up.
    #[test]
    fn a_tremor_under_a_pixel_draws_nothing_until_it_adds_up() {
        let mut pointer = Pointer::default();
        pointer.apply(Motion::Relative(0.0, 0.0), W, H);
        assert!(!pointer.apply(Motion::Relative(0.3, -0.2), W, H));
        assert!(!pointer.apply(Motion::Relative(0.3, 0.2), W, H));
        assert!(!pointer.apply(Motion::Relative(-0.4, 0.4), W, H));
        assert!(pointer.apply(Motion::Relative(0.9, 0.0), W, H));
        assert_eq!(pointer.at(), Some((961.0, 540.0)));
    }

    #[test]
    fn an_absolute_pointer_maps_onto_the_whole_window_and_stays_on_it() {
        let mut pointer = Pointer::default();
        assert!(pointer.apply(Motion::Absolute(0.5, 0.25), W, H));
        assert_eq!(pointer.at(), Some((960.0, 270.0)));
        pointer.apply(Motion::Absolute(0.0, 0.0), W, H);
        assert_eq!(pointer.at(), Some((0.0, 0.0)));
        pointer.apply(Motion::Absolute(1.0, 1.0), W, H);
        assert_eq!(pointer.at(), Some((1919.0, 1079.0)));
        pointer.apply(Motion::Absolute(-0.5, 7.0), W, H);
        assert_eq!(pointer.at(), Some((0.0, 1079.0)));
        // The window's own size, whatever it is: a 1280 x 720 logical panel.
        pointer.apply(Motion::Absolute(0.5, 0.5), 1280.0, 720.0);
        assert_eq!(pointer.at(), Some((640.0, 360.0)));
        // And a relative move goes on from there.
        pointer.apply(Motion::Relative(10.0, -10.0), 1280.0, 720.0);
        assert_eq!(pointer.at(), Some((650.0, 350.0)));
    }

    /// A thousand reports between two passes of the loop are one motion;
    /// the order of an absolute position and the moves after it is kept.
    #[test]
    fn motion_is_coalesced_and_kept_in_order() {
        let (flush, pending) = Motion::coalesce(None, Motion::Relative(1.0, 2.0));
        assert_eq!((flush, pending), (None, Motion::Relative(1.0, 2.0)));
        let (flush, pending) = Motion::coalesce(Some(pending), Motion::Relative(3.0, -1.0));
        assert_eq!((flush, pending), (None, Motion::Relative(4.0, 1.0)));
        let (flush, pending) = Motion::coalesce(Some(pending), Motion::Absolute(0.5, 0.5));
        assert_eq!((flush, pending), (None, Motion::Absolute(0.5, 0.5)));
        let (flush, pending) = Motion::coalesce(Some(pending), Motion::Relative(1.0, 0.0));
        assert_eq!(
            (flush, pending),
            (Some(Motion::Absolute(0.5, 0.5)), Motion::Relative(1.0, 0.0))
        );
    }

    /// A key puts the arrow away -- there is then no place for a click -- and
    /// the next motion puts it up again where it was.
    #[test]
    fn a_key_hides_the_arrow_and_the_next_motion_brings_it_back() {
        let mut pointer = Pointer::default();
        pointer.apply(Motion::Relative(0.0, 0.0), W, H);
        assert!(pointer.hide());
        assert!(!pointer.hide(), "hidden twice is nothing to draw");
        assert!(!pointer.shown());
        assert_eq!(pointer.at(), None, "a hidden arrow is nowhere to click");
        // Even a motion under a pixel brings it back: there is a frame to draw.
        assert!(pointer.apply(Motion::Relative(0.2, 0.0), W, H));
        assert_eq!(pointer.at(), Some((960.0, 540.0)));
    }

    #[test]
    fn a_wheel_steps_once_per_detent_and_adds_up_fractions() {
        let mut wheel = Scroller::default();
        let steps = |s: Scrolled| s.steps;
        assert_eq!(steps(wheel.add(Scroll::Wheel(0.0, 120.0), 50.0)), (0, 1));
        assert_eq!(steps(wheel.add(Scroll::Wheel(0.0, -240.0), 50.0)), (0, -2));
        assert_eq!(steps(wheel.add(Scroll::Wheel(60.0, 30.0), 50.0)), (0, 0));
        assert_eq!(steps(wheel.add(Scroll::Wheel(60.0, 30.0), 50.0)), (1, 0));
        wheel.reset();
        assert_eq!(
            steps(wheel.add(Scroll::Wheel(0.0, 90.0), 50.0)),
            (0, 0),
            "a reset forgets the fraction"
        );
    }

    /// A wheel's detent, a touchpad's fingers and a continuous scroll are
    /// one scroll: pixels for a surface, whole steps for the arrows, with
    /// libinput's own direction (natural scrolling is its to apply).
    #[test]
    fn wheel_fingers_and_continuous_scroll_are_normalised_alike() {
        let mut scroller = Scroller::default();
        let detent = 60.0;
        let wheel = scroller.add(Scroll::Wheel(0.0, 120.0), detent);
        assert_eq!(wheel.pixels, (0.0, 60.0));
        assert_eq!(wheel.steps, (0, 1));
        assert!(wheel.eased, "a wheel's step is eased");
        // A high-resolution wheel's half detent is half the pixels, no step.
        let half = scroller.add(Scroll::Wheel(0.0, -60.0), detent);
        assert_eq!(half.pixels, (0.0, -30.0));
        assert_eq!(half.steps, (0, 0));
        // Fingers: followed, pixel for pixel, and a detent's worth of them
        // is a step.
        let mut scroller = Scroller::default();
        let fingers = scroller.add(Scroll::Finger(-12.5, 40.0), detent);
        assert_eq!(fingers.pixels, (-12.5, 40.0));
        assert_eq!(fingers.steps, (0, 0));
        assert!(!fingers.eased);
        let more = scroller.add(Scroll::Finger(0.0, 25.0), detent);
        assert_eq!(more.steps, (0, 1), "40 + 25 is one 60-pixel step");
        // Natural scrolling reaches here already reversed: a negative
        // report scrolls back, whichever device sent it.
        let mut scroller = Scroller::default();
        let back = scroller.add(Scroll::Continuous(0.0, -130.0), detent);
        assert_eq!(back.pixels, (0.0, -130.0));
        assert_eq!(back.steps, (0, -2));
        assert!(detent > 0.0 && super::detent(720.0) >= 24.0);
    }

    /// The pointer is decided by the event, never the device: the code that
    /// reads it -- this module and the platform's pointer section -- names no
    /// device, model or ID, and asks no device anything.
    #[test]
    fn the_pointer_names_no_device() {
        let module = include_str!("pointer.rs");
        let module = &module[..module.find("#[cfg(test)]").expect("tests")];
        let platform = include_str!("platform.rs");
        let begin = platform
            .find("// -- pointer: ")
            .expect("platform: no pointer section");
        let end = platform[begin..]
            .find("// -- pointer: end --")
            .expect("platform: unterminated")
            + begin;
        for (file, section) in [
            ("pointer.rs", module),
            ("platform.rs", &platform[begin..end]),
        ] {
            let section = section.to_ascii_lowercase();
            for word in [
                ".device()",
                ".name()",
                "sysname",
                "vendor",
                "product",
                "bustype",
                "id_",
                "ur-02",
                "ur02",
            ] {
                assert!(
                    !section.contains(word),
                    "{file}'s pointer code mentions {word:?}"
                );
            }
        }
    }

    // ------------------------------------------------- the gesture machine

    use std::time::{Duration, Instant};

    const SHELF: Hit = Hit::Target(Zone::Shelves, 203);

    fn pressed(at: (f32, f32), hit: Hit) -> (Gesture, Context) {
        let context = on(Route::Media);
        let mut gesture = Gesture::default();
        gesture.press(at, context, 720.0);
        assert_eq!(gesture.hit(hit, context), Step::Nothing);
        (gesture, context)
    }

    /// A press and its release where it went down is a click on that
    /// target -- once: its focus, then its Select (`App::pointer_click`).
    #[test]
    fn a_click_is_the_target_s_release_and_only_once() {
        let (mut gesture, context) = pressed((100.0, 100.0), SHELF);
        gesture.release(Instant::now());
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Click(Zone::Shelves, 203)
        );
        assert_eq!(gesture.phase(), Phase::Idle);
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Nothing,
            "a second release is nothing"
        );
    }

    /// An air-mouse trembles under a click: a few pixels either way is
    /// still the click, and moves nothing.
    #[test]
    fn a_tremor_inside_the_slop_is_still_a_click() {
        let (mut gesture, context) = pressed((100.0, 100.0), SHELF);
        let now = Instant::now();
        for at in [(103.0, 98.0), (97.5, 102.0), (101.0, 104.5)] {
            assert_eq!(gesture.motion(at, now), Step::Nothing);
        }
        assert!(slop(720.0) > 6.0_f32.hypot(4.5));
        gesture.release(now);
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Click(Zone::Shelves, 203)
        );
    }

    /// Past the slop the press is a drag: no click, no long press, and the
    /// surface it is on follows the hand along its own axis -- the board up
    /// and down, the shelf sideways -- from where the drag began.
    #[test]
    fn past_the_slop_a_press_is_a_drag_of_its_surface_and_never_a_click() {
        let (mut gesture, context) = pressed((100.0, 300.0), SHELF);
        let id = gesture.id();
        let now = Instant::now();
        assert_eq!(gesture.motion((100.0, 280.0), now), Step::Nothing);
        assert!(
            matches!(gesture.phase(), Phase::Panning { lane, .. } if lane == Lane::vertical(Zone::Shelves))
        );
        assert_eq!(gesture.holding(id), None, "no long press once it moves");
        assert_eq!(
            gesture.motion((101.0, 250.0), now),
            Step::Pan {
                lane: Lane::vertical(Zone::Shelves),
                gesture: id,
                total: -30.0
            }
        );
        gesture.release(now + Duration::from_millis(200));
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Nothing,
            "the poster under the drag is not opened"
        );
        // Sideways, the shelf -- row 2 of the board -- and not the board.
        let (mut gesture, _) = pressed((100.0, 300.0), SHELF);
        gesture.motion((60.0, 305.0), now);
        let row = Lane {
            zone: Zone::Shelves,
            lane: 2,
            axis: Axis::Horizontal,
        };
        assert!(matches!(gesture.phase(), Phase::Panning { lane, .. } if lane == row));
        assert_eq!(
            gesture.motion((20.0, 340.0), now),
            Step::Pan {
                lane: row,
                gesture: gesture.id(),
                total: -40.0
            },
            "a lane takes its own axis only"
        );
    }

    /// A grid scrolls one way: a drag mostly sideways on it still moves it,
    /// along the axis it has. A target on nothing that scrolls is no drag,
    /// and no click once the hand has left it.
    #[test]
    fn a_grid_takes_its_own_axis_and_a_fixed_target_takes_no_drag() {
        let (mut gesture, context) = pressed((300.0, 300.0), Hit::Target(Zone::Grid, 9));
        gesture.motion((260.0, 290.0), Instant::now());
        assert!(
            matches!(gesture.phase(), Phase::Panning { lane, .. } if lane == Lane::vertical(Zone::Grid))
        );
        let (mut gesture, _) = pressed((300.0, 300.0), Hit::Target(Zone::HomeApps, 1));
        assert_eq!(
            gesture.motion((300.0, 250.0), Instant::now()),
            Step::Nothing
        );
        assert!(matches!(
            gesture.phase(),
            Phase::Pressed { moved: true, .. }
        ));
        gesture.release(Instant::now());
        assert_eq!(gesture.target_up(Zone::HomeApps, 1, context), Step::Nothing);
    }

    /// The release belongs to the target the press went down on: another
    /// one under the button at release is never opened.
    #[test]
    fn a_release_on_another_target_does_nothing() {
        let (mut gesture, context) = pressed((100.0, 100.0), SHELF);
        gesture.release(Instant::now());
        assert_eq!(
            gesture.target_up(Zone::Shelves, 204, context),
            Step::Nothing
        );
        assert_eq!(gesture.target_up(Zone::Grid, 203, context), Step::Nothing);
        // And the press is still waiting for its own target, which is the
        // only click it can be.
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Click(Zone::Shelves, 203)
        );
    }

    /// The loop hands a fast click's down and up to Slint together, after
    /// both reached the gesture: the click still lands. A fast flick does
    /// not become one.
    #[test]
    fn a_click_whose_target_is_named_after_the_release_still_lands_and_a_flick_does_not() {
        let context = on(Route::Media);
        let mut gesture = Gesture::default();
        gesture.press((10.0, 10.0), context, 720.0);
        gesture.release(Instant::now());
        gesture.hit(SHELF, context);
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Click(Zone::Shelves, 203)
        );
        gesture.press((10.0, 10.0), context, 720.0);
        gesture.motion((10.0, 90.0), Instant::now());
        gesture.release(Instant::now());
        gesture.hit(SHELF, context);
        assert!(
            !matches!(gesture.phase(), Phase::Panning { .. }),
            "let go before the drag began"
        );
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Nothing
        );
    }

    /// Another screen or layer between the press and its release: the
    /// press is not what it went down on any more.
    #[test]
    fn a_press_across_a_change_of_screen_is_no_click() {
        let (mut gesture, _) = pressed((100.0, 100.0), SHELF);
        gesture.release(Instant::now());
        let other = Context {
            layer: Layer::Choice,
            ..on(Route::Media)
        };
        assert_eq!(gesture.target_up(Zone::Shelves, 203, other), Step::Nothing);
        // And a target named on another panel is not taken.
        let mut gesture = Gesture::default();
        gesture.press((1.0, 1.0), on(Route::Media), 720.0);
        gesture.hit(SHELF, other);
        assert!(matches!(gesture.phase(), Phase::Pressed { hit: None, .. }));
    }

    /// A press on nothing -- no target, no surface's ground: Slint says
    /// nothing about it -- is over once Slint has seen its release, and
    /// was nothing: no click, and hover and the wheel are free again. It
    /// was once left pressed until the next press, and the arrow lit
    /// nothing and the wheel moved nothing meanwhile.
    #[test]
    fn a_press_on_nothing_settles_to_idle_and_frees_hover_and_the_wheel() {
        let context = on(Route::Media);
        let mut gesture = Gesture::default();
        gesture.press((100.0, 100.0), context, 720.0);
        assert!(!gesture.settle(), "still down: a drag may yet come");
        gesture.release(Instant::now());
        assert!(!gesture.idle(), "until Slint has seen it, it is pressed");
        assert!(gesture.settle());
        assert_eq!(gesture.phase(), Phase::Idle);
        assert!(gesture.idle(), "hover may light a target again");
        assert!(!gesture.settle(), "settled once");
        // Whatever a target's release says afterwards is no click.
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Nothing
        );
        let lane = Lane::vertical(Zone::Shelves);
        assert!(matches!(
            gesture.wheel(lane, 40.0, Instant::now()),
            Step::Pan { total, .. } if total == -40.0
        ));
    }

    /// The press's own hit arrives in the same queue as its release: a
    /// settle after Slint has seen both leaves the click to the target, and
    /// it is one click.
    #[test]
    fn a_settle_after_the_queue_leaves_a_target_s_click_to_it() {
        let context = on(Route::Media);
        let mut gesture = Gesture::default();
        gesture.press((100.0, 100.0), context, 720.0);
        gesture.release(Instant::now());
        // Slint, in queue order: the press's target, then its release.
        assert_eq!(gesture.hit(SHELF, context), Step::Nothing);
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Click(Zone::Shelves, 203)
        );
        assert!(!gesture.settle(), "nothing left to settle");
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Nothing,
            "once"
        );
        // A target that has said it went down is never settled away
        // before its release.
        let (mut gesture, context) = pressed((100.0, 100.0), SHELF);
        gesture.release(Instant::now());
        assert!(!gesture.settle());
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Click(Zone::Shelves, 203)
        );
        // A surface's ground, the film's bar: not this settle's either.
        let (mut gesture, _) = pressed((100.0, 100.0), Hit::Area(Lanes::default()));
        assert!(!gesture.settle());
        let mut gesture = Gesture::default();
        gesture.press((1.0, 1.0), context, 720.0);
        gesture.scrub();
        gesture.release(Instant::now());
        assert!(!gesture.settle());
        assert_eq!(gesture.phase(), Phase::Scrub);
    }

    /// libinput ends a finger and a continuous scroll with an event that is
    /// 0 on every axis it has; that end is kept, never taken for no
    /// movement. An axis the event does not have is no movement on it.
    #[test]
    fn a_scroll_s_end_is_its_own_event() {
        assert_eq!(
            Scroll::of_axes(true, None, Some(12.5)),
            Scroll::Finger(0.0, 12.5)
        );
        assert_eq!(
            Scroll::of_axes(false, Some(-3.0), Some(0.0)),
            Scroll::Continuous(-3.0, 0.0),
            "one axis still moving is no end"
        );
        assert_eq!(
            Scroll::of_axes(true, None, Some(0.0)),
            Scroll::Ended {
                fingers: true,
                stopped: (false, true)
            }
        );
        assert_eq!(
            Scroll::of_axes(true, Some(0.0), Some(0.0)),
            Scroll::Ended {
                fingers: true,
                stopped: (true, true)
            }
        );
        assert_eq!(
            Scroll::of_axes(false, Some(0.0), None),
            Scroll::Ended {
                fingers: false,
                stopped: (true, false)
            }
        );
        // An event with no axis at all says nothing.
        assert_eq!(Scroll::of_axes(true, None, None), Scroll::Finger(0.0, 0.0));
        // The end moves nothing, and the part-step left over is forgotten.
        let steps = |s: Scrolled| s.steps;
        let mut scroller = Scroller::default();
        let ended = Scroll::Ended {
            fingers: true,
            stopped: (false, true),
        };
        assert_eq!(steps(scroller.add(Scroll::Finger(0.0, 30.0), 50.0)), (0, 0));
        assert_eq!(scroller.add(ended, 50.0), Scrolled::default());
        assert_eq!(steps(scroller.add(Scroll::Finger(0.0, 30.0), 50.0)), (0, 0));
    }

    /// Fingers lifted off a touchpad while moving leave a pixel surface
    /// coasting the way they went, slowing to a stop, as a drag let go
    /// while moving does. Fingers that had stopped, a list drawn a row at a
    /// time, a continuous scroll and the end of the other axis do not.
    #[test]
    fn fingers_lifted_while_moving_coast_and_nothing_else_does() {
        use std::time::Duration;
        let lane = Lane::vertical(Zone::Shelves);
        let start = Instant::now();
        let flick = |gesture: &mut Gesture, lane: Lane| {
            for n in 0..6 {
                gesture.wheel(lane, 30.0, start + Duration::from_millis(10 * n));
            }
            start + Duration::from_millis(55)
        };
        let both = (true, true);

        let mut gesture = Gesture::default();
        let lifted = flick(&mut gesture, lane);
        assert!(gesture.lift(both, true, 720.0, lifted));
        let Phase::Coasting { velocity, total, .. } = gesture.phase() else {
            panic!("{:?}", gesture.phase());
        };
        // Content follows the fingers: scrolled down, it went up, and runs on.
        assert!(velocity < 0.0 && total == -180.0, "{velocity} {total}");
        let id = gesture.id();
        let Step::Pan {
            total: on,
            gesture: same,
            ..
        } = gesture.coast(0.016)
        else {
            panic!("a coast moves the surface");
        };
        assert!(on < total && same == id, "on the way it went, one gesture");
        let mut frames = 0;
        while gesture.coasting() {
            gesture.coast(0.016);
            frames += 1;
            assert!(frames < 200, "a coast ends");
        }
        assert_eq!(gesture.phase(), Phase::Idle);

        // Fingers that had stopped before they were lifted.
        let mut gesture = Gesture::default();
        let lifted = flick(&mut gesture, lane);
        assert!(!gesture.lift(both, true, 720.0, lifted + Duration::from_millis(200)));
        assert_eq!(gesture.phase(), Phase::Idle);

        // A list drawn a row at a time stops where they left it.
        let mut gesture = Gesture::default();
        let lifted = flick(&mut gesture, Lane::vertical(Zone::LiveGuide));
        assert!(!gesture.lift(both, true, 720.0, lifted));
        assert_eq!(gesture.phase(), Phase::Idle);

        // A continuous scroll's end is an end, never a coast.
        let mut gesture = Gesture::default();
        let lifted = flick(&mut gesture, lane);
        assert!(!gesture.lift(both, false, 720.0, lifted));
        assert_eq!(gesture.phase(), Phase::Idle);

        // The other axis ending leaves this one scrolling.
        let mut gesture = Gesture::default();
        let lifted = flick(&mut gesture, lane);
        assert!(!gesture.lift((true, false), true, 720.0, lifted));
        assert!(matches!(gesture.phase(), Phase::Wheeling { .. }));

        // Nothing scrolling: nothing to lift.
        assert!(!Gesture::default().lift(both, true, 720.0, lifted));
    }

    /// Held still, a press may be the remote's long Ok (the application
    /// decides whether one means anything there); once a hold took it, the
    /// release is no click. A press that moved, or came up, holds nothing.
    #[test]
    fn a_hold_is_offered_only_to_a_still_press_and_then_is_no_click() {
        let (mut gesture, context) = pressed((100.0, 100.0), SHELF);
        let id = gesture.id();
        assert_eq!(gesture.holding(id), Some((Zone::Shelves, 203)));
        assert_eq!(gesture.holding(id - 1), None, "an older press's timer");
        gesture.held();
        assert_eq!(gesture.holding(id), None);
        gesture.release(Instant::now());
        assert_eq!(
            gesture.target_up(Zone::Shelves, 203, context),
            Step::Nothing
        );
        let (mut gesture, _) = pressed((100.0, 100.0), SHELF);
        gesture.release(Instant::now());
        assert_eq!(
            gesture.holding(gesture.id()),
            None,
            "let go before the hold"
        );
    }

    /// The film's bar takes the press: nothing is dragged or clicked.
    #[test]
    fn a_scrub_keeps_the_hand_from_every_surface() {
        let context = Context {
            film: true,
            ..on(Route::Detail)
        };
        let mut gesture = Gesture::default();
        gesture.press((500.0, 650.0), context, 720.0);
        gesture.scrub();
        assert_eq!(
            gesture.motion((300.0, 600.0), Instant::now()),
            Step::Nothing
        );
        assert_eq!(
            gesture.hit(Hit::Target(Zone::PlayerMenu, 0), context),
            Step::Nothing
        );
        gesture.release(Instant::now());
        assert_eq!(gesture.phase(), Phase::Scrub);
        assert!(gesture.cancel());
        assert_eq!(gesture.phase(), Phase::Idle);
    }

    /// A press on a surface's ground drags it and never clicks; a ground
    /// with nothing that may move (under a modal) is a press of nothing.
    #[test]
    fn a_ground_drags_its_surface_and_never_clicks() {
        let lanes = Lanes::of_area("settings-rows", -1);
        assert_eq!(lanes.vertical, Some(Lane::vertical(Zone::SettingsRows)));
        let (mut gesture, _) = pressed((50.0, 50.0), Hit::Area(lanes));
        gesture.motion((50.0, 10.0), Instant::now());
        assert!(matches!(gesture.phase(), Phase::Panning { .. }));
        let (mut gesture, _) = pressed((50.0, 50.0), Hit::Area(lanes));
        gesture.release(Instant::now());
        assert_eq!(
            gesture.phase(),
            Phase::Idle,
            "a still press on a ground is nothing"
        );
        // A shelf's ground is on the board: up and down moves the board.
        let shelf = Lanes::of_area("shelves", 3);
        assert_eq!(shelf.vertical, Some(Lane::vertical(Zone::Shelves)));
        assert_eq!(shelf.horizontal.map(|lane| lane.lane), Some(3));
        assert_eq!(Lanes::of_area("shelves", -1).horizontal, None);
        assert!(Lanes::of_area("nothing", -1).is_empty());
    }

    /// With a modal up only the modal's own lists move; the screen under it
    /// takes no drag and no wheel.
    #[test]
    fn with_a_modal_up_only_the_modal_scrolls() {
        let screen = on(Route::Settings);
        let modal = Context {
            layer: Layer::Choice,
            ..screen
        };
        assert!(Zone::SettingsRows.lanes(0).taken(screen));
        assert!(!Zone::SettingsRows.lanes(0).taken(modal));
        assert!(Zone::SettingsChoice.lanes(0).taken(modal));
        assert!(!Zone::SettingsChoice.lanes(0).taken(screen));
        let media = on(Route::Media);
        let menu = Context {
            layer: Layer::Choice,
            ..media
        };
        assert!(Lanes::of_area("shelves", 1).taken(media));
        assert!(!Lanes::of_area("shelves", 1).taken(menu));
        let film = Context {
            film: true,
            layer: Layer::Choice,
            ..on(Route::Detail)
        };
        assert!(Zone::PlayerMenu.lanes(0).taken(film));
        assert!(!Zone::PlayerMenu.lanes(0).taken(Context {
            layer: Layer::Screen,
            ..film
        }));
        assert!(!Lanes::default().taken(screen));
    }

    fn fling(speed: f32) -> Gesture {
        let (mut gesture, _) = pressed((100.0, 400.0), Hit::Target(Zone::Grid, 0));
        let start = Instant::now();
        // Past the slop at once; then `speed` pixels a second, for 80 ms.
        gesture.motion((100.0, 380.0), start);
        for ms in (0..=80).step_by(8) {
            let at = start + Duration::from_millis(ms);
            gesture.motion((100.0, 380.0 - speed * ms as f32 / 1000.0), at);
        }
        gesture.release(start + Duration::from_millis(84));
        gesture
    }

    /// Let go while moving fast, a surface coasts on, slower and slower, and
    /// stops by itself: no frame is asked for once it has. A hand that let
    /// go still, or slowly, leaves the surface where it is.
    #[test]
    fn a_fling_coasts_to_a_stop_and_a_still_release_does_not() {
        let mut gesture = fling(1500.0);
        assert!(gesture.coasting());
        let mut last = match gesture.phase() {
            Phase::Coasting {
                total, velocity, ..
            } => {
                assert!(velocity < 0.0, "up, as the hand went");
                total
            }
            other => panic!("{other:?}"),
        };
        let mut frames = 0;
        while gesture.coasting() {
            let Step::Pan { total, .. } = gesture.coast(0.016) else {
                panic!("a coast moves its surface");
            };
            assert!(total <= last, "it runs on the way it went");
            last = total;
            frames += 1;
            assert!(frames < 200, "a coast ends by itself");
        }
        assert!(frames as f32 * 0.016 <= COAST_LONGEST + 0.02);
        assert_eq!(gesture.coast(0.016), Step::Nothing, "nothing after its end");
        assert!(!fling(50.0).coasting(), "slow is no fling");
        // A list drawn a row at a time stops where the hand left it.
        let (mut stepped, _) = pressed((100.0, 400.0), Hit::Target(Zone::PlayerMenu, 0));
        let start = Instant::now();
        stepped.motion((100.0, 380.0), start);
        stepped.motion((100.0, 300.0), start + Duration::from_millis(40));
        stepped.release(start + Duration::from_millis(44));
        assert!(!stepped.coasting());
        // A hand held still before letting go.
        let (mut gesture, _) = pressed((100.0, 400.0), Hit::Target(Zone::Grid, 0));
        let start = Instant::now();
        gesture.motion((100.0, 300.0), start);
        gesture.motion((100.0, 200.0), start + Duration::from_millis(16));
        gesture.release(start + Duration::from_millis(300));
        assert!(!gesture.coasting());
    }

    /// A coast stops at its surface's end (`PanState` says so), at a new
    /// press, and at anything that cancels the gesture.
    #[test]
    fn a_coast_stops_at_the_end_a_press_or_a_cancel() {
        let mut gesture = fling(2000.0);
        gesture.edge(Lane::vertical(Zone::SettingsRows));
        assert!(gesture.coasting(), "another surface's end");
        gesture.edge(Lane::vertical(Zone::Grid));
        assert!(!gesture.coasting());
        let mut gesture = fling(2000.0);
        gesture.press((5.0, 5.0), on(Route::Library), 720.0);
        assert!(!gesture.coasting());
        let mut gesture = fling(2000.0);
        assert!(gesture.cancel());
        assert_eq!(gesture.coast(0.016), Step::Nothing);
    }

    /// Turns of a wheel close together are one gesture that adds up, so
    /// the surface eases on from where the last left it; a pause, another
    /// surface, or a press, starts anew. Not while the button is down.
    #[test]
    fn a_wheel_s_turns_add_up_into_one_gesture() {
        let mut gesture = Gesture::default();
        let lane = Lane::vertical(Zone::SettingsRows);
        let now = Instant::now();
        let first = gesture.wheel(lane, 40.0, now);
        let Step::Pan {
            gesture: id, total, ..
        } = first
        else {
            panic!("{first:?}");
        };
        assert_eq!(total, -40.0, "down the list: the content goes up");
        assert_eq!(
            gesture.wheel(lane, 40.0, now + Duration::from_millis(50)),
            Step::Pan {
                lane,
                gesture: id,
                total: -80.0
            }
        );
        let later = gesture.wheel(lane, -10.0, now + Duration::from_secs(2));
        assert!(
            matches!(later, Step::Pan { gesture, total, .. } if gesture == id + 1 && total == 10.0)
        );
        let other = Lane::vertical(Zone::Grid);
        assert!(
            matches!(gesture.wheel(other, 5.0, now + Duration::from_secs(2)), Step::Pan { gesture, .. } if gesture == id + 2)
        );
        gesture.press((0.0, 0.0), on(Route::Settings), 720.0);
        assert_eq!(gesture.wheel(lane, 40.0, now), Step::Nothing);
    }

    /// A list drawn as a window of rows stays where it was while a pointer
    /// is in use, whatever the remote's focus does under a click; the hand
    /// moves it, kept to the list's ends; the remote picked up again draws
    /// the focus's window.
    #[test]
    fn a_window_of_rows_stays_put_under_a_pointer_and_returns_to_the_focus() {
        let mut windows = Windows::default();
        let zone = Zone::PlayerMenu;
        assert_eq!(windows.first(zone, 4, 30, 9, true), 4);
        // A click moved the focus: the remote's window would go to 10.
        assert_eq!(windows.first(zone, 10, 30, 9, true), 4);
        windows.step(zone, 3);
        assert_eq!(windows.first(zone, 10, 30, 9, true), 7);
        windows.step(zone, 100);
        assert_eq!(windows.first(zone, 10, 30, 9, true), 21, "the list's end");
        windows.step(zone, -100);
        assert_eq!(windows.first(zone, 10, 30, 9, true), 0, "its start");
        // The remote: the focus's window, and nothing remembered.
        assert_eq!(windows.first(zone, 10, 30, 9, false), 10);
        assert_eq!(windows.first(zone, 12, 30, 9, true), 12);
        windows.clear();
        assert_eq!(windows.first(zone, 2, 30, 9, true), 2);
        // A step before the list was drawn under the pointer starts from
        // the focus's window.
        let mut windows = Windows::default();
        windows.step(Zone::MsOptions, 2);
        assert_eq!(windows.first(Zone::MsOptions, 1, 20, 7, true), 3);
        assert_eq!(
            windows.first(Zone::MsOptions, 0, 3, 7, true),
            0,
            "a short list does not move"
        );
    }

    /// Which surfaces a target lies on: a poster on its shelf and the
    /// board, a grid poster on its grid, a list's row on its list; the
    /// lists drawn as windows of rows move by rows.
    #[test]
    fn targets_lie_on_their_surfaces() {
        let poster = Zone::Shelves.lanes(305);
        assert_eq!(poster.vertical, Some(Lane::vertical(Zone::Shelves)));
        assert_eq!(
            poster.horizontal.map(|lane| (lane.lane, lane.axis)),
            Some((3, Axis::Horizontal))
        );
        assert_eq!(Zone::PosterMenu.lanes(305), poster);
        assert_eq!(
            Zone::LibraryMark.lanes(7).vertical,
            Some(Lane::vertical(Zone::Grid))
        );
        assert_eq!(
            Zone::WifiInfo.lanes(2).vertical,
            Some(Lane::vertical(Zone::WifiRows))
        );
        assert_eq!(
            Zone::DetailMarks.lanes(2).vertical,
            Some(Lane::vertical(Zone::DetailEpisodes))
        );
        for fixed in [
            Zone::HomeApps,
            Zone::Shield,
            Zone::Back,
            Zone::PlayerBar,
            Zone::SearchKeys,
        ] {
            assert!(fixed.lanes(0).is_empty(), "{fixed:?}");
        }
        for stepped in [
            Zone::MsOptions,
            Zone::PlayerMenu,
            Zone::PlayerTracks,
            Zone::LiveGuide,
            Zone::LivePanel,
        ] {
            assert!(stepped.lanes(0).vertical.unwrap().stepped(), "{stepped:?}");
        }
        assert!(!Lane::vertical(Zone::SettingsRows).stepped());
        // The film's bar is the scrub's alone: no surface lies under it.
        assert!(Zone::PlayerBar.lanes(0).is_empty());
    }

    /// Every surface a target or a ground names is drawn with a `PanState`
    /// of that name: a lane Rust moves that nothing reads would be a drag
    /// that does nothing.
    #[test]
    fn every_surface_has_a_pan_state_in_the_interface() {
        let ui = [
            include_str!("../ui/components.slint"),
            include_str!("../ui/kit.slint"),
            include_str!("../ui/media.slint"),
            include_str!("../ui/search.slint"),
            include_str!("../ui/library.slint"),
            include_str!("../ui/discover.slint"),
            include_str!("../ui/settings.slint"),
            include_str!("../ui/wireless.slint"),
            include_str!("../ui/output.slint"),
            include_str!("../ui/media_settings.slint"),
            include_str!("../ui/detail.slint"),
            include_str!("../ui/player.slint"),
            include_str!("../ui/live.slint"),
            include_str!("../ui/quiet/home.slint"),
            include_str!("../ui/quiet/settings.slint"),
            include_str!("../ui/quiet/controls.slint"),
        ]
        .concat();
        for zone in ALL_ZONES {
            let Some(lane) = zone.lanes(0).vertical else {
                continue;
            };
            let name = zone_name(lane.zone);
            // Shared lists name their surface by the zone they are given.
            let shared = matches!(
                lane.zone,
                Zone::SettingsChoice | Zone::EditorPicker | Zone::WifiRows | Zone::BtRows
            );
            assert!(
                shared || ui.contains(&format!("surface: \"{name}\"")),
                "{name} has no PanState"
            );
        }
        assert!(
            ui.contains("surface: root.pointer-zone;"),
            "ChoiceSheet and Rail"
        );
        assert!(ui.contains("surface: root.rows-zone;"), "the radios' list");
    }

    /// What a moved surface shows is remembered by surface and lane, and
    /// forgotten once it is the focus's again.
    #[test]
    fn views_follow_what_a_moved_surface_shows() {
        let mut views = Views::default();
        assert_eq!(views.first(Zone::Shelves, 2), None);
        assert!(views.set(Zone::Shelves, 2, 14));
        assert!(!views.set(Zone::Shelves, 2, 14), "the same is no change");
        assert!(views.set(Zone::Shelves, -1, 1));
        assert_eq!(views.first(Zone::Shelves, 2), Some(14));
        assert_eq!(views.first(Zone::Shelves, -1), Some(1));
        assert_eq!(views.first(Zone::Shelves, 3), None, "another shelf");
        assert!(views.set(Zone::Shelves, 2, -1));
        assert_eq!(views.first(Zone::Shelves, 2), None);
        assert!(!views.set(Zone::Grid, -1, -1));
        views.clear();
        assert_eq!(views.first(Zone::Shelves, -1), None);
    }

    /// The catalogue's wheel never moves the focus: off a surface it
    /// scrolls the page, or nothing. Elsewhere it is still the arrows.
    #[test]
    fn a_catalogue_s_wheel_scrolls_its_page_and_never_the_focus() {
        for route in [
            Route::Media,
            Route::Search,
            Route::Discover,
            Route::Library,
            Route::MediaSettings,
            Route::Detail,
        ] {
            assert!(
                matches!(wheel_elsewhere(route), WheelElsewhere::Page(_)),
                "{route:?}"
            );
        }
        assert_eq!(
            wheel_elsewhere(Route::Media),
            WheelElsewhere::Page(Some(Lane::vertical(Zone::Shelves)))
        );
        assert_eq!(
            wheel_elsewhere(Route::Discover),
            WheelElsewhere::Page(Some(Lane::vertical(Zone::Grid)))
        );
        assert_eq!(wheel_elsewhere(Route::Detail), WheelElsewhere::Page(None));
        for route in [Route::Home, Route::Settings, Route::Wifi, Route::Live] {
            assert_eq!(wheel_elsewhere(route), WheelElsewhere::Arrows, "{route:?}");
        }
        // The page's surface is on the screen, and only with no modal up.
        let menu = Context {
            layer: Layer::Choice,
            ..on(Route::Discover)
        };
        let WheelElsewhere::Page(Some(grid)) = wheel_elsewhere(Route::Discover) else {
            unreachable!()
        };
        assert!(accepts(on(Route::Discover), grid.zone) && !accepts(menu, grid.zone));
    }

    /// Discover's posters: a first click selects, a second opens -- and
    /// nowhere else.
    #[test]
    fn on_discover_a_first_click_selects_and_only_there() {
        assert!(click_selects_first(Route::Discover, Zone::Grid));
        assert!(!click_selects_first(Route::Library, Zone::Grid));
        assert!(!click_selects_first(Route::Discover, Zone::DiscoverPanel));
        assert!(!click_selects_first(Route::Media, Zone::Shelves));
    }
}
