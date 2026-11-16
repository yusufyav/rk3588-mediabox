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
//! is not turned into Ok. A screen that is `Navigation` takes hover as the
//! remote's focus and a click as the remote's Ok, through the screen's own
//! focus model (`App::pointer_point`) and its own Select: there is no second
//! road for what a click does.
//!
//! What is under the arrow is Slint's to say: the shared components carry a
//! `PointerTarget` (ui/pointer.slint) naming a `Zone` and an index, and a
//! zone is taken only while its layer is the one on top (`accepts`).

use crate::route::Route;

/// What a pointer may do on what is on the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Nothing: motion, buttons and the wheel are ignored and no arrow is
    /// drawn. A click is not Ok.
    Disabled,
    /// Hover is the remote's focus, a click is the remote's Ok on what is
    /// under the arrow, the wheel is the arrows.
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
    /// The settings' category column.
    SettingsSections,
    /// A way back: the remote's Back under the arrow. The settings'
    /// column's heading and its parent row, the catalogue frame's mark, a
    /// screen's title bar, the close of a sheet or a panel -- whichever is up.
    Back,
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
            "settings-sections" => Zone::SettingsSections,
            "back" => Zone::Back,
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
            Zone::HomeApps => (Some(Route::Home), Layer::Screen),
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

    /// Whether hovering target `index` of this zone moves the remote's
    /// focus. Not the account mark: on it the remote's focus drops the
    /// account's menu over the page, and an arrow passing over it must not
    /// put a modal up. Not a way back, a ground, or the way out of a film,
    /// which take no focus. Not a live menu row's title: the row it brings
    /// forward moves the rows under a still arrow, which would bring the
    /// next one forward in turn.
    pub fn hovers(self, index: usize) -> bool {
        match self {
            Zone::FrameAccount
            | Zone::Shield
            | Zone::Back
            | Zone::PlayerExit
            | Zone::LiveSurface => false,
            Zone::LiveMenu => index % 100 != ROW_TITLE,
            _ => true,
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

    /// Where the arrow is, while it is up.
    pub fn at(&self) -> Option<(f32, f32)> {
        self.exact.filter(|_| self.shown).map(|_| self.drawn)
    }
}

/// A wheel's detents, as libinput's v120 values (120 is one detent; a
/// high-resolution wheel reports fractions of one), turned into steps.
#[derive(Default)]
pub struct Wheel {
    pending: (i32, i32),
}

impl Wheel {
    /// Adds a report; returns the whole steps it completes on each axis,
    /// positive down and right.
    pub fn add(&mut self, horizontal: f64, vertical: f64) -> (i32, i32) {
        let take = |pending: &mut i32, value: f64| {
            *pending += value.round() as i32;
            let steps = *pending / 120;
            *pending -= steps * 120;
            steps
        };
        (
            take(&mut self.pending.0, horizontal),
            take(&mut self.pending.1, vertical),
        )
    }

    pub fn reset(&mut self) {
        self.pending = (0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 1920.0;
    const H: f32 = 1080.0;

    const ALL_ZONES: [Zone; 69] = [
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
        Zone::SettingsSections,
        Zone::Back,
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

    /// The name each zone has in the interface; every one is used by a
    /// .slint file (`every_zone_is_named_in_the_interface`).
    fn zone_name(zone: Zone) -> &'static str {
        match zone {
            Zone::HomeApps => "home",
            Zone::SettingsSections => "settings-sections",
            Zone::Back => "back",
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
        // Passing over the account mark must not drop its menu (a modal).
        assert!(!Zone::FrameAccount.hovers(0));
        assert!(Zone::Shelves.hovers(0));
        // A way back takes no focus; a live menu title does not move the
        // rows under a still arrow.
        assert!(!Zone::Back.hovers(0));
        assert!(!Zone::PlayerExit.hovers(0));
        assert!(!Zone::LiveMenu.hovers(ROW_TITLE));
        assert!(Zone::LiveMenu.hovers(1));
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
        let mut wheel = Wheel::default();
        assert_eq!(wheel.add(0.0, 120.0), (0, 1));
        assert_eq!(wheel.add(0.0, -240.0), (0, -2));
        assert_eq!(wheel.add(60.0, 30.0), (0, 0));
        assert_eq!(wheel.add(60.0, 30.0), (1, 0));
        wheel.reset();
        assert_eq!(wheel.add(0.0, 90.0), (0, 0), "a reset forgets the fraction");
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
}
