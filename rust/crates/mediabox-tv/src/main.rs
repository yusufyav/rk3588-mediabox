//! The television's interface.
//!
//! It is a client of mediaboxd-rs and of nothing else. The control plane owns
//! which application holds the display, the media core owns what is playable
//! and how, and Kodi owns playback; this process draws, listens to the remote,
//! and asks.
//!
//! Everything a press can mean is in `actions.rs`, everything a screen's focus
//! can do is in `screens/`, and both are plain data with tests. What is left
//! here is the wiring: which screen is on the panel, what a chosen intent does
//! about it, and how an answer from the control plane reaches the right one.

mod actions;
mod detail;
mod fdstore;
mod images;
mod input;
mod keyboard;
mod metrics;
mod model;
mod platform;
mod route;
mod rpc;
mod screens;
mod session;
mod state;
mod video;
mod vitals;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mediabox_core::{InputAction, InputEvent, PowerAction};
use serde_json::Value;
use slint::ComponentHandle;

use actions::{Intent, Transport};
use route::Route;
use screens::power::{Press, Sheet};
use screens::settings::Action;

slint::include_modules!();

/// Where the daemon publishes normalised input. The television asks for `tv=1`
/// because navigation actions are only sent to the client that says it is the
/// one in the living room — without it a phone on the network moves in lockstep
/// with whoever holds the remote.
const EVENTS_URL: &str = "http://127.0.0.1:8787/v1/events?tv=1";

const CACHE_DIR: &str = "/var/lib/mediabox-ui/tv-imgcache";
const STATE_FILE: &str = "/var/lib/mediabox-ui/tv-state.json";
const SNAPSHOT_DIR: &str = "/var/lib/mediabox-ui/snapshots";

/// How long after the last keystroke a search is actually sent. A remote types
/// one letter at a time and the media core fans a search out over every addon;
/// asking on each letter would put four searches in flight for a four-letter
/// word and draw the answer to the shortest one last.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(350);

/// The appliance is the only place this runs in earnest, and there the defaults
/// above are right. The overrides exist so the interface can be driven against
/// a forwarded socket from a developer's machine — which is how a shelf layout
/// gets looked at without a deploy.
fn socket_path() -> String {
    std::env::var("MEDIABOX_TV_SOCKET").unwrap_or_else(|_| rpc::DEFAULT_SOCKET.to_string())
}

fn events_url() -> String {
    std::env::var("MEDIABOX_TV_EVENTS").unwrap_or_else(|_| EVENTS_URL.to_string())
}

fn cache_dir() -> String {
    std::env::var("MEDIABOX_TV_CACHE").unwrap_or_else(|_| CACHE_DIR.to_string())
}

fn state_file() -> String {
    std::env::var("MEDIABOX_TV_STATE").unwrap_or_else(|_| STATE_FILE.to_string())
}

fn snapshot_dir() -> String {
    std::env::var("MEDIABOX_TV_SNAPSHOTS").unwrap_or_else(|_| SNAPSHOT_DIR.to_string())
}

thread_local! {
    /// How the background threads reach the interface. They post a closure to
    /// the event loop and it finds everything here, because nothing the
    /// interface owns is safe to send across a thread.
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&mut App)) {
    let app = APP.with(|slot| slot.borrow().clone());
    if let Some(app) = app {
        f(&mut app.borrow_mut());
    }
}

struct App {
    window: slint::Weak<MediaBoxWindow>,
    stack: route::Stack,

    home: state::Home,
    media: screens::media::Media,
    search: screens::search::Search,
    library: screens::library::Library,
    settings: screens::settings::Settings,
    account: screens::account::Account,
    wifi: screens::wireless::Wifi,
    bt: screens::wireless::Bluetooth,
    diagnostics: screens::diagnostics::Diagnostics,
    now: screens::now_playing::NowPlaying,

    detail: Option<detail::Detail>,
    /// The power sheet, or a confirmation, over anything.
    sheet: Option<Sheet>,

    images: images::ImageManager,
    meter: metrics::Metrics,
    dispatcher: input::Dispatcher,
    store: session::Store,

    /// The last answers from the control plane, kept so the settings and
    /// diagnostics screens can be composed without asking again.
    status: Option<Value>,
    diag: Option<Value>,
    display: Option<model::DisplayStatus>,

    /// The indicator-light mode the viewer just chose, until the daemon has
    /// confirmed it.
    ///
    /// The machine poll runs every ten seconds, so a row drawn only from its
    /// answer sits on the old value for up to ten seconds after a press. That
    /// is not a slow daemon — it writes sysfs in about a millisecond — it is a
    /// row waiting for a timer it has no reason to wait for. So the press
    /// changes the row on the same frame, the daemon's answer replaces it, and
    /// a poll that was already in flight when the press happened is ignored
    /// for this one field: it cannot know about a choice made after it left.
    leds_pending: Option<mediabox_core::LedMode>,
    /// The colour mode a press chose, kept until the daemon answers. Without
    /// it a poll that left before the press would redraw the old value for a
    /// frame.
    color_mode_pending: Option<Option<(mediabox_core::ColorFormat, u8)>>,
    /// A fan curve save or reset is in flight. The same guard as the two
    /// above: a poll that left before it cannot know the curve was saved.
    fan_pending: bool,

    /// Whether the film's controls were up when the key being acted on was
    /// pressed. Read only by Back; see there.
    controls_were_open: bool,

    /// Whether a handover to Kodi is in flight.
    ///
    /// The player takes a moment to let go of the plane, and for that moment
    /// the watcher below sees a picture with no film state behind it and
    /// adopts it — which would put the interface back on the now-playing
    /// screen in the middle of handing the television to Kodi.
    handing_over: bool,

    /// The film this interface started on its own video plane, if there is
    /// one.
    ///
    /// Which player a transport key reaches is decided by this and nothing
    /// else: the two are never both running, and asking the control plane what
    /// is playing would answer a frame too late.
    here: Option<Playing>,

    detail_backdrop: Option<String>,
    /// When the line along the bottom stops being true. See `say`.
    notice_until: Option<Instant>,
    detail_fade: f32,
    /// Bumped every time a detail screen is opened, so an answer for a title
    /// the viewer has already left is dropped instead of overwriting the one
    /// they are looking at.
    epoch: u64,
    /// What to restore once the shelves land, read from disk at startup.
    resume: Option<session::Snapshot>,
}

impl App {
    fn route(&self) -> Route {
        self.stack.current()
    }

    fn modal(&self) -> bool {
        self.sheet.is_some()
    }

    // ------------------------------------------------------------------ input

    /// A press that survived the dispatcher.
    fn act(&mut self, action: InputAction) {
        self.meter.key_accepted();
        let intent = actions::intent_for(self.route(), self.modal(), action);

        // A film is watched, not operated: the controls are down until somebody
        // presses something, and then they are up for a few seconds. Any key
        // does it, including the one that is about to be acted on — pressing
        // Right on a film that shows nothing must both reveal the controls and
        // move the focus, or the second press is the one that appears to work.
        self.controls_were_open = self.controls_open();
        if self.here.is_some() && !matches!(intent, Intent::Ignore) {
            self.open_controls();
        }

        if self.sheet.is_some() {
            self.act_on_sheet(intent);
            return;
        }
        match intent {
            Intent::GoHome => self.go_home(),
            Intent::OfferPower => self.open_sheet(Sheet::power()),
            Intent::Transport(transport) => self.transport(transport),
            Intent::Ignore => {}
            _ => match self.route() {
                Route::Home | Route::Boot => self.act_on_home(intent),
                Route::Media => self.act_on_media(intent),
                Route::Search => self.act_on_search(intent),
                Route::Library => self.act_on_library(intent),
                Route::Detail => self.act_on_detail(intent),
                Route::NowPlaying => self.act_on_now_playing(intent),
                Route::Settings => self.act_on_settings(intent),
                Route::Account => self.act_on_account(intent),
                Route::Diagnostics => self.act_on_diagnostics(intent),
                Route::Wifi => self.act_on_wifi(intent),
                Route::Bluetooth => self.act_on_bluetooth(intent),
            },
        }
    }

    /// Backspace, which is a deletion on a screen that holds text and the way
    /// out everywhere else.
    fn backspace(&mut self) {
        if self.modal() {
            self.act(InputAction::Back);
            return;
        }
        match self.route() {
            Route::Search => {
                if self.search.typed('\u{8}') {
                    self.search_soon();
                    self.paint();
                }
            }
            Route::Account => {
                if self.account.typed('\u{8}') {
                    self.paint();
                }
            }
            Route::Wifi => {
                if self.wifi.typed('\u{8}') {
                    self.paint();
                } else {
                    self.act(InputAction::Back);
                }
            }
            // A digit typed into a fan curve cell, then the edit itself.
            Route::Settings if self.settings.typing_cooling() => {
                if self.settings.cooling.backspace() {
                    self.paint();
                } else {
                    self.act(InputAction::Back);
                }
            }
            _ => self.act(InputAction::Back),
        }
    }

    /// Text from a real keyboard.
    ///
    /// Two screens have somewhere to put it — the search box and the account
    /// form — and on both it types without moving the focus, so a person who
    /// starts on the keyboard and then reaches for the remote finds it where
    /// they left it. Everywhere else a letter is not a command and is dropped.
    fn typed(&mut self, c: char) {
        if self.modal() {
            return;
        }
        match self.route() {
            Route::Search => {
                if self.search.typed(c) {
                    self.search_soon();
                    self.paint();
                }
            }
            Route::Account => {
                if c == '\r' || c == '\n' {
                    match self.account.typed_enter() {
                        screens::account::Press::SignIn => self.sign_in(),
                        screens::account::Press::Changed => self.paint(),
                        screens::account::Press::Nothing => {}
                    }
                } else if self.account.typed(c) {
                    self.paint();
                }
            }
            // The Wi-Fi password. A viewer with a keyboard plugged in should
            // never have to walk a letter grid to type one.
            Route::Wifi => {
                if c == '\r' || c == '\n' {
                    if self.wifi.typed_enter() == screens::wireless::Press::Join {
                        self.join_wifi();
                    } else {
                        self.paint();
                    }
                } else if self.wifi.typed(c) {
                    self.paint();
                }
            }
            // Digits into the fan curve table: a temperature or a percent,
            // without walking a value up one step at a time.
            Route::Settings if self.settings.typing_cooling() => {
                if self.settings.cooling.typed(c) {
                    self.paint();
                }
            }
            _ => {}
        }
    }

    // ----------------------------------------------------------------- sheets

    fn open_sheet(&mut self, sheet: Sheet) {
        self.sheet = Some(sheet);
        self.paint();
    }

    fn close_sheet(&mut self) {
        self.sheet = None;
        self.paint();
    }

    fn act_on_sheet(&mut self, intent: Intent) {
        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        match intent {
            Intent::Move(dx, dy) => {
                if sheet.step(dx, dy) {
                    self.paint();
                }
            }
            Intent::Dismiss => self.close_sheet(),
            Intent::Select => match sheet.press() {
                Press::Close => self.close_sheet(),
                Press::Ask(action, question) => {
                    self.sheet = Some(Sheet::confirm(action, &question));
                    self.paint();
                }
                Press::Do(action) => {
                    self.close_sheet();
                    self.run(action);
                }
            },
            _ => {}
        }
    }

    // --------------------------------------------------------------- the home

    fn go_home(&mut self) {
        self.stack.reset(Route::Home);
        self.detail = None;
        self.paint();
        self.remember();
    }

    fn act_on_home(&mut self, intent: Intent) {
        match intent {
            Intent::Move(dx, dy) => {
                if self.home.step(dx as isize, dy as isize) {
                    self.paint();
                }
            }
            Intent::Select => self.choose_on_home(),
            // There is no screen behind this one — the television was turned on
            // here — so Back goes back up to the launcher, which is the top of
            // it. From the launcher itself Back does nothing, on purpose: a
            // home screen that reacts to Back by changing is a home screen
            // nobody can tell they have reached.
            Intent::Dismiss => {
                if self.home.row != 0 {
                    self.home.row = 0;
                    self.paint();
                }
            }
            _ => {}
        }
    }

    fn choose_on_home(&mut self) {
        if self.home.focused_app().is_some() {
            self.launch();
            return;
        }
        self.open_detail();
    }

    /// One of this interface's own screens, from wherever it was chosen.
    fn open_screen(&mut self, nav: state::Nav) {
        match nav {
            state::Nav::Search => self.open(Route::Search),
            state::Nav::Library => self.open_library(),
            state::Nav::Settings => self.open(Route::Settings),
        }
    }

    /// Ok on the launcher.
    fn launch(&mut self) {
        let Some(tile) = self.home.focused_app() else {
            return;
        };
        let (action, name, ready) = (tile.action.clone(), tile.name.clone(), tile.ready);

        if !ready {
            self.say(format!("{name} henüz yok"));
            return;
        }

        match action {
            // The catalogue is not on this screen. It is what this tile is
            // for.
            state::AppAction::Shelves => self.open_media(),
            state::AppAction::Screen(nav) => self.open_screen(nav),
            // The control plane stops this unit as part of starting the other
            // one, so nothing after this call is guaranteed to run.
            state::AppAction::Launch(id) => {
                self.say(format!("{name} açılıyor…"));
                self.remember();
                self.store.flush();
                spawn_launch(id, name);
            }
            state::AppAction::Absent => self.say(format!("{name} henüz yok")),
        }
    }

    /// One line along the bottom of the home screen. The interface has nowhere
    /// else to say anything, and taking the screen away for a message about a
    /// tile would be worse than the message.
    /// Put one line along the bottom of the panel, for a few seconds.
    ///
    /// **The only way to set that line.** `window.set_notice` is called here
    /// and in `clear_notice` and nowhere else, and `tests/run-host-tests.sh`
    /// holds it to exactly those two: a caller that writes to the window
    /// directly gets a line with no lifetime, which then sits on the panel
    /// until something else happens to replace it. That is how a failed
    /// source left "Kaynak açılamadı" up over a film that was playing fine.
    fn say(&mut self, message: String) {
        if let Some(window) = self.window.upgrade() {
            window.set_notice(message.into());
        }
        self.notice_until = Some(Instant::now() + NOTICE_LIFETIME);
    }

    /// Takes the line away once it has been up long enough to read.
    ///
    /// A line that never leaves is worse than no line: the appliance said
    /// "Televizyon uyandırılıyor…" and then stood there saying it, over
    /// whatever the viewer did next, until something happened to replace it.
    /// Called from the same quarter-second tick that watches the film.
    fn expire_notice(&mut self) {
        if self
            .notice_until
            .is_some_and(|until| until <= Instant::now())
        {
            self.clear_notice();
        }
    }

    /// Takes that line away again.
    ///
    /// Nothing else in this interface does: a notice is set and stays set
    /// until something replaces it. That is tolerable for a message about a
    /// tile that could not open, and wrong for a screen a viewer is working
    /// in, so a setting that acts immediately clears it rather than adding to
    /// it.
    fn clear_notice(&mut self) {
        self.notice_until = None;
        if let Some(window) = self.window.upgrade() {
            window.set_notice(Default::default());
        }
    }

    // --------------------------------------------------------- the lights

    /// Puts a mode on the settings row and draws it.
    ///
    /// Writes into the kept status rather than beside it, so there is one
    /// answer the screen is composed from and not two that can disagree.
    fn show_leds(&mut self, mode: mediabox_core::LedMode) {
        let text = Value::String(
            serde_json::to_value(mode)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "off".into()),
        );
        match self.status.as_mut().and_then(Value::as_object_mut) {
            Some(status) => {
                let leds = status
                    .entry("leds")
                    .or_insert_with(|| serde_json::json!({"available": true, "leds": []}));
                if let Some(leds) = leds.as_object_mut() {
                    leds.insert("mode".into(), text);
                }
            }
            // No status yet: the daemon has not answered once. The row is a
            // reading until it does, and its answer will carry the mode.
            None => return,
        }
        self.recompose_settings();
    }

    /// The daemon's answer to a light being set.
    ///
    /// Whatever it says is what the row shows, including a refusal: a board
    /// that would not take the write must not be left displaying the mode
    /// somebody asked for.
    /// Draw the chosen colour mode now, on this frame, before the daemon has
    /// answered. The same trick the lights use.
    fn show_color_mode(&mut self, mode: Option<(mediabox_core::ColorFormat, u8)>) {
        let choice = match mode {
            None => serde_json::json!({"kind": "auto"}),
            Some((format, bits)) => serde_json::json!({
                "kind": "fixed",
                "format": serde_json::to_value(format).unwrap_or(Value::Null),
                "bits": bits,
            }),
        };
        if let Some(status) = self.status.as_mut().and_then(Value::as_object_mut) {
            if let Some(colour) = status
                .get_mut("display_color")
                .and_then(Value::as_object_mut)
            {
                colour.insert("choice".into(), choice);
            }
        }
        self.recompose_settings();
    }

    fn color_mode_answered(&mut self, answer: Option<Value>) {
        self.color_mode_pending = None;
        if let Some(choice) = answer
            .as_ref()
            .and_then(|colour| colour.get("choice"))
            .and_then(|choice| serde_json::from_value::<mediabox_core::ColorChoice>(choice.clone()).ok())
        {
            platform::set_colour_choice(choice);
        }
        if let Some(colour) = answer {
            if let Some(status) = self.status.as_mut().and_then(Value::as_object_mut) {
                status.insert("display_color".into(), colour);
            }
        }
        self.recompose_settings();
    }

    /// The daemon's answer to a fan curve being saved or reset.
    ///
    /// The status it returns replaces the kept one, so the rows say what the
    /// daemon wrote rather than what was asked for. A refusal is said along
    /// the bottom in the daemon's own words: an unsafe curve, a board whose
    /// boot configuration does not load the overlay.
    fn fan_answered(&mut self, reset: bool, answer: Result<Value, String>) {
        self.fan_pending = false;
        match answer {
            Ok(fan) => {
                let pending = fan
                    .get("pending_reboot")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let saved = fan
                    .get("configured")
                    .cloned()
                    .and_then(|curve| serde_json::from_value(curve).ok());
                match (reset, saved) {
                    (true, _) => self.settings.cooling.reset_done(pending),
                    (false, Some(curve)) => self.settings.cooling.saved(curve, pending),
                    // Saved, and the daemon did not say what: keep the draft
                    // as it is and let the next poll tell.
                    (false, None) => {}
                }
                if let Some(status) = self.status.as_mut().and_then(Value::as_object_mut) {
                    status.insert("fan".into(), fan);
                }
                self.say(
                    match (reset, pending) {
                        (false, true) => "Fan eğrisi kaydedildi",
                        (false, false) => "Fan eğrisi zaten bu",
                        (true, true) => "Kartın kendi eğrisine dönülecek",
                        (true, false) => "Zaten kartın kendi eğrisi",
                    }
                    .into(),
                );
            }
            Err(error) => self.say(format!("Fan eğrisi kaydedilemedi — {error}")),
        }
        self.recompose_settings();
    }

    fn leds_answered(&mut self, answer: Option<Value>) {
        self.leds_pending = None;
        if let Some(leds) = answer {
            if let Some(status) = self.status.as_mut().and_then(Value::as_object_mut) {
                status.insert("leds".into(), leds);
            }
        }
        self.recompose_settings();
    }

    /// Re-composes the settings screen from the kept answers and paints it, on
    /// its own — the machine poll does this for every screen at once, and a
    /// press must not wait for the poll.
    fn recompose_settings(&mut self) {
        self.settings.compose(
            self.status.as_ref(),
            self.diag.as_ref(),
            self.display.as_ref(),
        );
        self.paint();
    }

    // ---------------------------------------------------------- the catalogue

    fn open_media(&mut self) {
        self.media.set_shelves(self.home.shelves.clone());
        self.open(Route::Media);
    }

    fn act_on_media(&mut self, intent: Intent) {
        match intent {
            Intent::Move(dx, dy) => {
                if self.media.step(dx, dy) {
                    self.paint();
                }
            }
            Intent::Select => {
                if let Some(nav) = self.media.focused_nav() {
                    self.open_screen(nav);
                    return;
                }
                let item = self.media.focused().cloned();
                if let Some(item) = item {
                    self.open_detail_for(&item);
                }
            }
            Intent::Dismiss => self.back(),
            _ => {}
        }
    }

    fn paint_media(&mut self, window: &MediaBoxWindow) {
        self.media.sync_artwork(&mut self.images);
        window.set_media_row(self.media.row as i32);
        window.set_media_col(self.media.column() as i32);

        let (title, facts, summary) = match self.media.focused() {
            Some(item) => (
                item.title.clone(),
                hero_facts(item),
                item.summary.clone().unwrap_or_default(),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        window.set_media_title(title.into());
        window.set_media_facts(facts.into());
        window.set_media_summary(summary.into());
    }

    // ------------------------------------------------------------- the search

    fn act_on_search(&mut self, intent: Intent) {
        match intent {
            Intent::Move(dx, dy) => {
                if self.search.step(dx, dy) {
                    self.paint();
                }
            }
            Intent::Select => {
                if self.search.pane == screens::search::Pane::Results {
                    let item = self.search.focused_result().cloned();
                    if let Some(item) = item {
                        self.open_detail_for(&item);
                    }
                } else if self.search.press() {
                    self.search_soon();
                    self.paint();
                }
            }
            Intent::Dismiss => self.back(),
            _ => {}
        }
    }

    fn search_soon(&mut self) {
        if self.search.query.trim().is_empty() {
            return;
        }
        spawn_search(self.search.generation, self.search.query.clone());
    }

    fn searched(&mut self, generation: u64, answer: Result<Vec<state::Item>, String>) {
        match answer {
            Ok(items) => self.search.take(generation, items),
            Err(why) => self.search.fail(generation, &why),
        }
        if self.route() == Route::Search {
            self.paint();
        }
    }

    // ------------------------------------------------------------ the library

    fn open_library(&mut self) {
        self.library.build(&self.home.shelves);
        self.open(Route::Library);
    }

    fn act_on_library(&mut self, intent: Intent) {
        match intent {
            Intent::Move(dx, dy) => {
                if self.library.step(dx, dy) {
                    self.paint();
                }
            }
            Intent::Select => {
                if self.library.on_tabs {
                    if self.library.step(0, 1) {
                        self.paint();
                    }
                    return;
                }
                let item = self.library.focused().cloned();
                if let Some(item) = item {
                    self.open_detail_for(&item);
                }
            }
            Intent::Dismiss => {
                if !self.library.on_tabs {
                    self.library.on_tabs = true;
                    self.paint();
                } else {
                    self.back();
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- the detail

    fn open_detail(&mut self) {
        let Some(item) = self.home.focused().cloned() else {
            return;
        };
        self.open_detail_for(&item);
    }

    fn open_detail_for(&mut self, item: &state::Item) {
        let detail = detail::Detail::seeded(item);
        let (kind, id) = (detail.kind.clone(), detail.id.clone());
        self.detail = Some(detail);
        self.detail_backdrop = None;
        self.epoch += 1;
        // Nothing of the last film may stay on this screen.
        //
        // The backdrop is drawn as a cross-fade between two slots, and
        // `paint_detail` flips to the other slot as soon as the film changes
        // but writes the new picture into it only once the picture has been
        // decoded. When it had not been -- which is every film that was not
        // already on a shelf the viewer had just scrolled past -- the slot it
        // faded to still held the *previous* film's backdrop, at full
        // opacity, for as long as the decode took. Opening a film showed the
        // one opened before it. The poster and the logo had the same shape:
        // set only on a cache hit, so left over otherwise.
        //
        // Emptied here, where the film changes, rather than guarded in the
        // painter: there is exactly one moment when the screen stops being
        // about one title and starts being about another, and this is it.
        if let Some(window) = self.window.upgrade() {
            let blank = slint::Image::default();
            window.set_detail_art_a(blank.clone());
            window.set_detail_art_b(blank.clone());
            window.set_detail_poster(blank.clone());
            window.set_detail_logo(blank);
        }
        self.open(Route::Detail);
        spawn_detail_load(self.epoch, kind, id);
    }

    fn act_on_detail(&mut self, intent: Intent) {
        match intent {
            Intent::Move(dx, dy) => {
                let moved = match self.detail.as_mut() {
                    Some(detail) => detail.step(dx, dy),
                    None => false,
                };
                if moved {
                    self.paint();
                }
            }
            Intent::Select => self.choose_on_detail(),
            Intent::Dismiss => {
                // Back closes the filter's list before it does anything else:
                // one press, one step.
                if self
                    .detail
                    .as_mut()
                    .is_some_and(detail::Detail::close_filter)
                {
                    self.paint();
                    return;
                }
                // Back out of the source column before backing out of the
                // screen: one press, one step, and never two things at once.
                let left = self
                    .detail
                    .as_mut()
                    .filter(|detail| detail.pane == detail::Pane::Sources)
                    .map(|detail| {
                        detail.pane = detail::Pane::Record;
                        detail.settle();
                    })
                    .is_some();
                if left {
                    self.paint();
                } else {
                    self.back();
                }
            }
            _ => {}
        }
    }

    fn choose_on_detail(&mut self) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };

        // In the source column, Ok is "play this one". The reference plays on
        // the click rather than selecting and waiting for a second decision,
        // and a viewer who has just read a release name does not want to be
        // told to now press the other button.
        if detail.pane == detail::Pane::Sources {
            if detail.filter_open {
                if detail.choose_provider() {
                    self.paint();
                }
                return;
            }
            if detail.on_filter {
                if detail.open_filter() {
                    self.paint();
                }
                return;
            }
            if detail.choose_source().is_some() {
                self.analyse();
                self.play_here();
                self.paint();
            }
            return;
        }

        // The record carries one button, and it is the trailer. A film is
        // played by choosing where it comes from, which is Ok in the column.
        if detail.action == detail::ACTION_TRAILER {
            self.trailer();
        }
    }

    fn detail_loaded(&mut self, epoch: u64, answer: Result<DetailAnswer, String>) {
        if epoch != self.epoch {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };

        match answer {
            Ok(DetailAnswer::Library(envelope)) => detail.take_library(*envelope),
            Ok(DetailAnswer::Catalogue { meta, listing, raw }) => {
                detail.take_meta(*meta);
                detail.take_streams(*listing, raw);
            }
            Err(why) => detail.fail(&why),
        }

        self.analyse();
        self.paint();
    }

    /// Asks the media core what it would do with the chosen source.
    fn analyse(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let Some(source) = detail.selected_source() else {
            return;
        };
        if !source.parsed.playable {
            return;
        }
        spawn_plan(self.epoch, source.parsed.url.clone(), source.raw.clone());
    }

    fn planned(&mut self, epoch: u64, plan: Option<model::Plan>) {
        if epoch != self.epoch {
            return;
        }
        if let Some(detail) = self.detail.as_mut() {
            detail.plan = plan;
            self.paint();
        }
    }

    /// Starts the film here, in this interface's own player.
    ///
    /// Nothing is handed over: the decoder puts its frames on the video port's
    /// second window and this process keeps DRM master and keeps drawing
    /// underneath. Back stops it and the catalogue is still where it was.
    fn play_here(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let Some(source) = detail.selected_source() else {
            return;
        };
        if !source.parsed.playable {
            return;
        }

        let url = source.parsed.url.clone();
        let raw = source.raw.clone();
        // Everything read off the detail screen, before anything borrows self
        // mutably: the film's name and its length go to the player with it.
        let name = detail.meta.name.clone();
        let runtime = detail.runtime_seconds();

        self.now.title = detail.meta.name.clone();
        self.now.artwork = detail.meta.poster.clone();
        self.now.backdrop = detail.meta.background.clone();
        self.now.logo = detail.meta.logo.clone().filter(|url| !url.is_empty());
        // Same reason as the detail screen's four slots: `paint_now_playing`
        // writes the picture only when it is already decoded, so without this
        // the film that is starting is announced over the last film's still.
        if let Some(window) = self.window.upgrade() {
            window.set_np_art(slint::Image::default());
            window.set_np_logo(slint::Image::default());
        }
        self.now.set_technical(detail.technical_pairs());

        self.remember();
        self.store.flush();
        self.here = Some(Playing::new());
        self.now.film = true;
        // Between here and the first frame there is nothing on the panel: the
        // decoder has to open the source, and on a torrent behind a debrid
        // that is seconds, not milliseconds. The screen used to be the film's
        // name over a black rectangle with a progress bar at 0:00, which is
        // indistinguishable from a player that has failed. Cleared by
        // `watch_the_film` the moment a frame actually lands.
        self.now.note = "Video yükleniyor…".into();
        self.now.focus = 0;
        self.now.row = screens::now_playing::Row::Bar;
        self.now.close_menu();

        if let Some(window) = self.window.upgrade() {
            window.set_detail_note("Oynatılıyor…".into());
        }
        spawn_play_here(url, raw, name, runtime);
        // The film covers the panel — the video window sits above the primary —
        // so the screen behind it is the one a remote should already be on when
        // it comes back.
        self.open(Route::NowPlaying);
    }

    fn controls_open(&self) -> bool {
        self.here
            .as_ref()
            .and_then(|playing| playing.controls_until)
            .is_some_and(|until| until > std::time::Instant::now())
    }

    fn open_controls(&mut self) {
        let Some(playing) = self.here.as_mut() else {
            return;
        };
        playing.controls_until = Some(std::time::Instant::now() + CONTROLS_LINGER);
        self.paint();
    }

    fn close_controls(&mut self) {
        let Some(playing) = self.here.as_mut() else {
            return;
        };
        playing.controls_until = None;
        self.paint();
    }

    /// Where the film has got to, as the player itself answers it.
    fn film_moved(&mut self, status: Value) {
        if self.here.is_none() {
            return;
        }
        self.now.take_here(&status);
        if self.controls_open() {
            self.paint();
        }
    }

    /// Whether the film this interface started is still on the panel.
    ///
    /// Called four times a second, and it is the only thing that clears the
    /// film state: a film that ends by itself, a player that dies, and a source
    /// that never opens all look the same from here — no picture — and are told
    /// apart by whether there ever was one.
    fn watch_the_film(&mut self) {
        // A film this interface did not start is still a film on this
        // interface's plane: the web interface can open one, and a viewer in
        // front of the television should be able to pause it without being
        // told which device asked for it.
        if self.here.is_none() {
            if self.handing_over || !platform::video_showing() {
                return;
            }
            eprintln!("mediabox-tv.play adopted a film this interface did not start");
            self.here = Some(Playing {
                seen: true,
                ..Playing::new()
            });
            // An adopted film is still this interface's own player, and it
            // carries the same row as one this interface started.
            self.now.film = true;
            if self.route() != Route::NowPlaying {
                self.open(Route::NowPlaying);
            }
            self.paint();
        }

        let Some(playing) = self.here.as_mut() else {
            return;
        };
        let showing = platform::video_showing();

        if showing {
            // The frame that ends the wait. Noticed on the edge, because the
            // note belongs to the player from here on and repainting on every
            // tick would be a frame's work for nothing.
            let first = !playing.seen;
            playing.seen = true;
            // The position and whether it is paused are the player's to answer,
            // and it is asked only while it is up.
            let due = playing
                .polled
                .is_none_or(|last| last.elapsed() >= POSITION_INTERVAL);
            if due {
                playing.polled = Some(std::time::Instant::now());
                spawn_here_status();
            }
            if first {
                self.now.note = String::new();
                self.paint();
            }
            if self.now.pill_expired() {
                self.paint();
            }
            // A panel that is down holds the row up with it: the reference's
            // controls do not time out from under an open menu.
            if self.now.menu == screens::now_playing::Menu::None
                && self
                    .here
                    .as_ref()
                    .and_then(|playing| playing.controls_until)
                    .is_some_and(|until| until <= std::time::Instant::now())
            {
                self.close_controls();
            }
            return;
        }
        if !playing.seen && playing.asked.elapsed() < FIRST_FRAME_GRACE {
            return;
        }

        let gave_up = !playing.seen;
        if gave_up {
            eprintln!("mediabox-tv.play here gave up: no frame in {FIRST_FRAME_GRACE:?}");
            spawn_here(HereCommand::Stop);
        }
        self.here = None;
        // Through `say`, so it goes away by itself. Written straight to the
        // window it had no lifetime, and one source that failed left "Kaynak
        // açılamadı" sitting in the corner of the panel — through the next
        // attempt, which worked, and over the film that was then playing.
        if gave_up {
            self.say("Kaynak açılamadı".into());
        }
        self.now.film = false;
        self.now.close_menu();
        if self.route() == Route::NowPlaying {
            self.back();
        }
        self.paint();
    }

    /// Hands the film playing here to Kodi, where it has got to.
    ///
    /// The position, the clean stop and the start on Kodi are the control
    /// plane's to sequence — it is the one holding the player's socket — so
    /// this asks for the handover as one call rather than stopping the player
    /// from here and hoping the plane is free by the time Kodi wants it.
    ///
    /// What is left behind matters as much: the remote goes back to the film's
    /// page and that is what is written to the session file, so when Kodi is
    /// closed the interface comes back up where the viewer left it rather than
    /// on a now-playing screen with nothing playing.
    fn hand_to_kodi(&mut self) {
        if self.here.is_none() || self.handing_over {
            return;
        }
        self.handing_over = true;
        self.here = None;
        self.now.film = false;
        self.now.close_menu();
        if self.route() == Route::NowPlaying {
            self.stack.pop();
        }
        self.now.note = "Kodi'ye aktarılıyor…".into();
        self.remember();
        self.store.flush();
        self.paint();
        spawn_handoff();
    }

    /// The handover did not happen, and the television is still this
    /// interface's. Said where it can be read, and the watcher let go of.
    fn handoff_failed(&mut self, why: String) {
        self.handing_over = false;
        // `say`, not the window: see the note in `watch_the_film`.
        self.say(format!("Kodi'ye aktarılamadı — {why}"));
        self.paint();
    }

    fn trailer(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let Some(id) = detail.meta.trailer.clone().filter(|id| !id.is_empty()) else {
            return;
        };
        let url = format!("https://www.youtube.com/tv#/watch?v={id}");
        spawn_open(url);
    }

    // -------------------------------------------------------- what is playing

    fn act_on_now_playing(&mut self, intent: Intent) {
        use screens::now_playing::{Control, Film, Menu, Row};

        // A panel down over the film owns every key while it is down.
        if self.now.menu != Menu::None {
            match intent {
                Intent::Move(dx, dy) => {
                    // On the delay, Up and Down are the value: a tenth of a
                    // second a press, the way the reference's own stepper
                    // moves it. Left and Right stay what they are everywhere
                    // else on the panel — the way between its two columns.
                    if self.now.menu_column == 2 && dy != 0 {
                        self.nudge_delay(if dy < 0 { 0.1 } else { -0.1 });
                        return;
                    }
                    if self.now.step_menu(dx, dy) {
                        self.paint();
                    }
                }
                Intent::Select => self.choose_in_menu(),
                Intent::Dismiss => {
                    self.now.close_menu();
                    self.paint();
                }
                _ => {}
            }
            return;
        }

        match intent {
            Intent::Move(dx, dy) => {
                if self.now.step(dx, dy) {
                    self.paint();
                }
            }
            Intent::Select => {
                if self.now.row == Row::Bar {
                    // Ok on the bar goes where the scrub got to. Ok on a bar
                    // nobody has moved is the ordinary thing a remote does to a
                    // film: stop it for a moment, or start it again.
                    match self.now.take_scrub() {
                        Some(seconds) => self.transport(Transport::SeekTo(seconds)),
                        None => self.transport(Transport::PlayPause),
                    }
                    return;
                }
                // Two rows of buttons, for two players. The film this
                // interface is playing carries the reference's own row; Kodi
                // carries the four a remote needs for a player it does not
                // own.
                if self.here.is_some() {
                    match self.now.focused_film() {
                        Film::PlayPause => self.transport(Transport::PlayPause),
                        Film::Restart => self.transport(Transport::SeekTo(0)),
                        Film::Subtitles => self.open_menu(Menu::Subtitles),
                        Film::Audio => self.open_menu(Menu::Audio),
                        Film::Speed => self.open_menu(Menu::Speed),
                        Film::Scale => {
                            let (mode, _) = self.now.next_scale();
                            spawn_here(HereCommand::Scale(mode));
                            self.paint();
                        }
                        Film::Player => self.open_menu(Menu::Player),
                    }
                    return;
                }
                match self.now.focused() {
                    Control::SeekBack => self.transport(Transport::Seek(-10)),
                    Control::PlayPause => self.transport(Transport::PlayPause),
                    Control::SeekForward => self.transport(Transport::Seek(30)),
                    Control::Stop => self.transport(Transport::Stop),
                }
            }
            Intent::Dismiss => {
                // A film is on the window above this one, so there is no
                // "leave it running and go back": going back is stopping. But
                // Back is also a key, and every key on a bare film brings the
                // controls up first — so the question is whether they were
                // already up when this one was pressed, not whether they are up
                // now, which they always are by the time this runs.
                if self.here.is_some() {
                    // A scrub in progress is what Back is about first: it
                    // abandons the move rather than the film.
                    if self.now.cancel_scrub() {
                        self.paint();
                        return;
                    }
                    // Back takes away what is on the film, one layer a press:
                    // the controls if they are up, and the film itself only
                    // when there is nothing left over it. Every key brings the
                    // controls up, this one included, so the question is
                    // whether they were up when it was pressed.
                    if self.controls_were_open {
                        self.close_controls();
                        return;
                    }
                    self.transport(Transport::Stop);
                }
                self.back()
            }
            _ => {}
        }
    }

    fn open_menu(&mut self, menu: screens::now_playing::Menu) {
        if self.now.open_menu(menu) {
            self.open_controls();
            self.paint();
        }
    }

    /// Ok on a row of the open panel. Every one of them closes it: the
    /// reference applies the choice and gets out of the way.
    ///
    /// Two columns can be chosen from. A language on the left takes that
    /// language's first track, which is what a viewer who does not care which
    /// English track it is means by pressing it; a row on the right takes that
    /// exact one.
    fn choose_in_menu(&mut self) {
        use screens::now_playing::{Menu, SPEEDS};
        let focus = self.now.menu_focus;
        match self.now.menu {
            Menu::Subtitles | Menu::Audio => {
                let subtitles = self.now.menu == Menu::Subtitles;
                // The subtitle panel's first row is "off" and belongs to no
                // language.
                if subtitles && self.now.menu_column == 0 && focus == 0 {
                    for track in self.now.subtitles.iter_mut() {
                        track.selected = false;
                    }
                    spawn_here(HereCommand::Subtitle(-1));
                } else {
                    let of_language = self.now.tracks_of_language();
                    let wanted = if self.now.menu_column == 1 {
                        of_language.get(self.now.menu_track_focus).copied()
                    } else {
                        of_language.first().copied()
                    };
                    let Some(index) = wanted else {
                        return;
                    };
                    let tracks = if subtitles {
                        &mut self.now.subtitles
                    } else {
                        &mut self.now.audio
                    };
                    let Some(id) = tracks.get(index).map(|track| track.id) else {
                        return;
                    };
                    for (at, track) in tracks.iter_mut().enumerate() {
                        track.selected = at == index;
                    }
                    spawn_here(if subtitles {
                        HereCommand::Subtitle(id)
                    } else {
                        HereCommand::Audio(id)
                    });
                }
            }
            Menu::Speed => {
                let value = SPEEDS[focus.min(SPEEDS.len() - 1)];
                self.now.speed = value;
                spawn_here(HereCommand::Speed(value));
            }
            Menu::Player => {
                self.now.close_menu();
                if focus == 1 {
                    self.hand_to_kodi();
                    return;
                }
                self.paint();
                return;
            }
            Menu::None => return,
        }
        self.now.close_menu();
        self.open_controls();
        self.paint();
    }

    /// One press on the delay beside a panel's list.
    fn nudge_delay(&mut self, by: f64) {
        use screens::now_playing::Menu;
        match self.now.menu {
            Menu::Subtitles => {
                self.now.sub_delay = (self.now.sub_delay + by).clamp(-60.0, 60.0);
                spawn_here(HereCommand::SubtitleDelay(self.now.sub_delay));
            }
            Menu::Audio => {
                self.now.audio_delay = (self.now.audio_delay + by).clamp(-60.0, 60.0);
                spawn_here(HereCommand::AudioDelay(self.now.audio_delay));
            }
            _ => return,
        }
        self.open_controls();
        self.paint();
    }

    /// The transport keys. They belong to whatever is playing, which is Kodi,
    /// so they are forwarded and the screen follows the answer rather than
    /// guessing at it.
    fn transport(&mut self, transport: Transport) {
        // Two players, one set of buttons. Which one a press reaches is decided
        // by which one this interface started, not by asking: a remote pressed
        // while a film is on our own plane must never reach Kodi, which is not
        // running and whose start would take the television.
        if self.here.is_some() {
            match transport {
                Transport::PlayPause => spawn_here(HereCommand::PlayPause),
                Transport::Seek(seconds) => spawn_here(HereCommand::Seek(seconds)),
                Transport::SeekTo(seconds) => {
                    // Drawn immediately rather than when the player answers:
                    // a bar that snaps back to where it was for a moment is
                    // what makes a scrub feel like it did not work.
                    self.now.elapsed_seconds = seconds;
                    spawn_here(HereCommand::SeekTo(seconds));
                }
                Transport::Stop => {
                    self.here = None;
                    spawn_here(HereCommand::Stop);
                }
                Transport::VolumeUp | Transport::VolumeDown | Transport::Mute => return,
            }
            if self.now.active() && self.route() != Route::NowPlaying {
                self.open(Route::NowPlaying);
            }
            return;
        }
        match transport {
            Transport::PlayPause => spawn_kodi(KodiCommand::PlayPause),
            Transport::Stop => spawn_kodi(KodiCommand::Stop),
            Transport::Seek(seconds) => spawn_kodi(KodiCommand::Seek(seconds)),
            // Kodi is asked in the only terms this interface has for it: the
            // distance from where it says it is to where the scrub landed.
            Transport::SeekTo(seconds) => {
                let from = self.now.elapsed_seconds as i64;
                self.now.elapsed_seconds = seconds;
                spawn_kodi(KodiCommand::Seek(seconds as i64 - from));
            }
            // Volume is the television's, over CEC, and the daemon owns that
            // adapter. Nothing to do here until there is a mixer to move.
            Transport::VolumeUp | Transport::VolumeDown | Transport::Mute => return,
        }
        // A transport key pressed anywhere opens the screen it belongs to. It
        // is the one place the position and the state are visible.
        if self.now.active() && self.route() != Route::NowPlaying {
            self.open(Route::NowPlaying);
        }
    }

    // ------------------------------------------------------------- the settings

    fn act_on_settings(&mut self, intent: Intent) {
        match intent {
            Intent::Move(dx, dy) => {
                if self.settings.step(dx, dy) {
                    self.paint();
                }
            }
            Intent::Select => {
                if self.settings.pane == screens::settings::Pane::Sections {
                    if self.settings.step(1, 0) {
                        self.paint();
                    }
                    return;
                }
                if self.settings.is_cooling() {
                    self.press_cooling();
                    return;
                }
                let Some(action) = self.settings.focused().and_then(|row| row.action) else {
                    return;
                };
                if action.confirms() {
                    self.open_sheet(Sheet::confirm(action, action.question()));
                } else {
                    self.run(action);
                }
            }
            Intent::Dismiss => {
                // Back in the fan editor closes an open cell or the restart
                // question before it leaves the section.
                if self.settings.typing_cooling() && self.settings.cooling.back() {
                    self.paint();
                    return;
                }
                if self.settings.pane == screens::settings::Pane::Rows {
                    self.settings.pane = screens::settings::Pane::Sections;
                    self.paint();
                } else {
                    self.back();
                }
            }
            _ => {}
        }
    }

    /// Ok in the fan editor. Editing stays in this process; saving and
    /// resetting are the daemon's. Restarting is asked for from the question
    /// the editor puts up after a save, with "later" under the focus, and that
    /// question is the confirmation.
    fn press_cooling(&mut self) {
        use screens::cooling::Press;
        match self.settings.cooling.press() {
            Press::Nothing => {}
            Press::Changed => self.paint(),
            Press::Save(curve) => {
                self.say("Fan eğrisi kaydediliyor…".into());
                self.fan_pending = true;
                spawn_fan(Some(curve));
            }
            Press::Reset => {
                self.say("Kartın kendi fan eğrisine dönülüyor…".into());
                self.fan_pending = true;
                spawn_fan(None);
            }
            Press::Restart => self.run(Action::Restart),
        }
    }

    // -------------------------------------------------------------- the account

    fn act_on_account(&mut self, intent: Intent) {
        match intent {
            Intent::Move(dx, dy) => {
                if self.account.step(dx, dy) {
                    self.paint();
                }
            }
            Intent::Select => match self.account.press() {
                screens::account::Press::SignIn => self.sign_in(),
                screens::account::Press::Changed => self.paint(),
                screens::account::Press::Nothing => {}
            },
            Intent::Dismiss => {
                if self.account.dismiss() {
                    self.paint();
                } else {
                    self.back();
                }
            }
            _ => {}
        }
    }

    /// The one place the password leaves the screen. It is moved out here and
    /// wiped as soon as the answer lands, either way — see `account_answered`.
    fn sign_in(&mut self) {
        let email = self.account.email().trim().to_string();
        let password = self.account.password().to_string();
        self.account.begin("Bağlanıyor…");
        self.paint();
        spawn_media_login(email, password);
    }

    /// The answer to a sign-in or a sign-out.
    ///
    /// `status` is a fresh control-plane status rather than the login's own
    /// reply, so what the screen shows afterwards is what the box will say to
    /// anybody else who asks.
    fn account_answered(&mut self, answer: Result<Value, String>) {
        match answer {
            Ok(status) => {
                self.status = Some(status);
                self.account.signed_in(self.status.as_ref());
                self.recompose_settings();
                // The catalogue is assembled from whatever add-ons the account
                // brings, so it is stale the moment the account changes.
                spawn_home_reload();
            }
            Err(why) => self.account.failed(&why),
        }
        self.paint();
    }

    fn act_on_diagnostics(&mut self, intent: Intent) {
        match intent {
            Intent::Move(dx, dy) => {
                if self.diagnostics.step(dx, dy) {
                    self.paint();
                }
            }
            Intent::Dismiss => self.back(),
            _ => {}
        }
    }

    // ------------------------------------------------------------ the radios

    fn act_on_wifi(&mut self, intent: Intent) {
        use screens::wireless::Press;
        match intent {
            Intent::Move(dx, dy) => {
                if dy != 0 {
                    self.wifi.step(dy);
                } else {
                    self.wifi.sideways(dx);
                }
                self.paint();
            }
            Intent::Select => {
                if self.wifi.busy {
                    return;
                }
                match self.wifi.press() {
                    Press::None => self.paint(),
                    Press::Scan => {
                        self.wifi.busy = true;
                        self.wifi.scanning = true;
                        self.wifi.notice = "Ağlar aranıyor…".into();
                        self.paint();
                        spawn_wifi(WifiCommand::Scan);
                    }
                    Press::Power(on) => {
                        self.wifi.busy = true;
                        self.wifi.notice =
                            if on { "Açılıyor…" } else { "Kapatılıyor…" }.into();
                        self.paint();
                        spawn_wifi(WifiCommand::Power(on));
                    }
                    Press::Join => self.join_wifi(),
                    Press::Forget => {
                        let Some(network) = self.wifi.highlighted().cloned() else {
                            return;
                        };
                        self.wifi.busy = true;
                        self.wifi.notice = format!("{} unutuluyor…", network.ssid);
                        self.paint();
                        spawn_wifi(WifiCommand::Forget(network.ssid));
                    }
                    Press::Close => self.back(),
                }
            }
            Intent::Dismiss => {
                if !self.wifi.back() {
                    self.back();
                } else {
                    self.paint();
                }
            }
            _ => {}
        }
    }

    /// Join the network the password face is for.
    ///
    /// Reached from the button on the panel and from Enter on a real
    /// keyboard, so both do exactly the same thing.
    fn join_wifi(&mut self) {
        let Some(target) = self.wifi.target.clone() else {
            return;
        };
        let secret = self.wifi.password().to_string();
        self.wifi.busy = true;
        self.wifi.notice = format!("{} ağına bağlanılıyor…", target.ssid);
        self.paint();
        spawn_wifi(WifiCommand::Connect {
            ssid: target.ssid,
            psk: (!secret.is_empty()).then_some(secret),
        });
        // The typed secret is not kept here a moment longer than the call
        // that carries it.
        self.wifi.forget_secret();
    }

    fn act_on_bluetooth(&mut self, intent: Intent) {
        use screens::wireless::BtPress;
        match intent {
            Intent::Move(_, dy) if dy != 0 => {
                self.bt.step(dy);
                self.paint();
            }
            Intent::Select => {
                if self.bt.busy {
                    return;
                }
                let Some(address) = self.bt.highlighted().map(|d| d.address.clone()) else {
                    // The two head rows carry no address.
                    match self.bt.press() {
                        BtPress::Scan => {
                            self.bt.busy = true;
                            self.bt.notice = "Aygıtlar aranıyor…".into();
                            self.paint();
                            spawn_bt(BtCommand::Scan);
                        }
                        BtPress::Power(on) => {
                            self.bt.busy = true;
                            self.bt.notice =
                                if on { "Açılıyor…" } else { "Kapatılıyor…" }.into();
                            self.paint();
                            spawn_bt(BtCommand::Power(on));
                        }
                        _ => {}
                    }
                    return;
                };
                let command = match self.bt.press() {
                    BtPress::Pair => {
                        self.bt.notice = "Eşleştiriliyor…".into();
                        BtCommand::Pair(address)
                    }
                    BtPress::Connect => {
                        self.bt.notice = "Bağlanıyor…".into();
                        BtCommand::Connect(address)
                    }
                    BtPress::Disconnect => {
                        self.bt.notice = "Bağlantı kesiliyor…".into();
                        BtCommand::Disconnect(address)
                    }
                    BtPress::Forget => {
                        self.bt.notice = "Eşleşme kaldırılıyor…".into();
                        BtCommand::Forget(address)
                    }
                    _ => return,
                };
                self.bt.busy = true;
                self.paint();
                spawn_bt(command);
            }
            Intent::Dismiss => self.back(),
            _ => {}
        }
    }

    /// An answer from the daemon about the Wi-Fi radio.
    fn wifi_answer(&mut self, answer: Result<serde_json::Value, String>, scanned: bool) {
        self.wifi.busy = false;
        self.wifi.scanning = false;
        match answer {
            Ok(value) => {
                self.wifi.notice.clear();
                if scanned {
                    let networks = value
                        .get("networks")
                        .and_then(serde_json::Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .map(|item| screens::wireless::Network {
                                    ssid: item
                                        .get("ssid")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    signal: item
                                        .get("signal")
                                        .and_then(serde_json::Value::as_i64)
                                        .unwrap_or(-99)
                                        as i32,
                                    secured: item
                                        .get("secured")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(true),
                                    band: item
                                        .get("band")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    remembered: item
                                        .get("remembered")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(false),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    self.wifi.take_networks(networks);
                    // A scan answer carries no link state, so ask for it.
                    spawn_wifi(WifiCommand::Status);
                } else {
                    self.wifi.take_status(&value);
                    self.wifi.face = screens::wireless::Face::List;
                }
            }
            Err(error) => self.wifi.notice = error,
        }
        self.paint();
    }

    fn bt_answer(&mut self, answer: Result<serde_json::Value, String>) {
        self.bt.busy = false;
        match answer {
            Ok(value) => {
                self.bt.notice.clear();
                self.bt.take_status(&value);
            }
            Err(error) => self.bt.notice = error,
        }
        self.paint();
    }

    fn paint_wifi(&mut self, window: &MediaBoxWindow) {
        let wifi = &self.wifi;
        window.set_wifi_known(wifi.known);
        window.set_wifi_present(wifi.present);
        window.set_wifi_powered(wifi.powered);
        window.set_wifi_index(wifi.index as i32);
        window.set_wifi_busy(wifi.busy);
        window.set_wifi_scanning(wifi.scanning);
        window.set_wifi_notice(wifi.notice.clone().into());
        window.set_wifi_address(wifi.address.clone().unwrap_or_default().into());
        window.set_wifi_summary(
            match (wifi.known, &wifi.connected_to, wifi.powered) {
                // Nothing has come back yet, so there is nothing to say. This
                // used to read "Kapalı" for the first moment of every visit.
                (false, _, _) => String::new(),
                (true, Some(ssid), _) => format!("{ssid} · bağlı"),
                (true, None, true) => "Bağlı değil".to_string(),
                (true, None, false) => "Kapalı".to_string(),
            }
            .into(),
        );
        window.set_wifi_face(
            match wifi.face {
                screens::wireless::Face::List => "list",
                screens::wireless::Face::Password => "password",
            }
            .into(),
        );
        window.set_wifi_target(
            wifi.target
                .as_ref()
                .map(|network| network.ssid.clone())
                .unwrap_or_default()
                .into(),
        );
        window.set_wifi_password_mask(wifi.password_mask().into());
        window.set_wifi_show(wifi.show);
        window.set_wifi_focus(
            match wifi.focus {
                screens::wireless::PasswordFocus::Field => "field",
                screens::wireless::PasswordFocus::Show => "show",
                screens::wireless::PasswordFocus::Keys => "keys",
                screens::wireless::PasswordFocus::Join => "join",
            }
            .into(),
        );
        window.set_wifi_key_row(wifi.keys.row() as i32);
        window.set_wifi_key_col(wifi.keys.col() as i32);
        window.set_wifi_keys(key_rows(&wifi.keys));
        let connected = wifi.connected_to.clone();
        window.set_wifi_networks(slint::ModelRc::new(slint::VecModel::from(
            wifi.networks
                .iter()
                .map(|network| WifiRow {
                    ssid: network.ssid.clone().into(),
                    bars: network.bars() as i32,
                    band: network.band.clone().into(),
                    secured: network.secured,
                    remembered: network.remembered,
                    connected: connected.as_deref() == Some(network.ssid.as_str()),
                })
                .collect::<Vec<_>>(),
        )));
    }

    fn paint_bluetooth(&mut self, window: &MediaBoxWindow) {
        let bt = &self.bt;
        window.set_bt_known(bt.known);
        window.set_bt_present(bt.present);
        window.set_bt_powered(bt.powered);
        window.set_bt_index(bt.index as i32);
        window.set_bt_busy(bt.busy);
        window.set_bt_notice(bt.notice.clone().into());
        window.set_bt_press_label(bt.press_label().into());
        window.set_bt_summary(
            match (bt.known, &bt.controller, bt.powered) {
                (false, _, _) => String::new(),
                (true, Some(_), true) => "Açık".to_string(),
                (true, Some(_), false) => "Kapalı".to_string(),
                (true, None, _) => "Denetleyici yok".to_string(),
            }
            .into(),
        );
        window.set_bt_devices(slint::ModelRc::new(slint::VecModel::from(
            bt.devices
                .iter()
                .map(|device| BtRow {
                    name: device.title().into(),
                    address: device.address.clone().into(),
                    kind: device.kind().into(),
                    paired: device.paired,
                    connected: device.connected,
                })
                .collect::<Vec<_>>(),
        )));
    }

    /// The one place a settings decision leaves this process.
    fn run(&mut self, action: Action) {
        match action {
            Action::OpenAccount => {
                self.account.open(self.status.as_ref());
                self.open(Route::Account);
            }
            Action::SignOut => {
                self.say("Hesap bağlantısı kesiliyor…".into());
                spawn_media_logout();
            }
            Action::OpenWifi => {
                self.wifi.open();
                self.open(Route::Wifi);
                // Ask straight away: a screen that opens empty and waits for a
                // press reads as broken.
                self.wifi.busy = true;
                self.wifi.scanning = true;
                spawn_wifi(WifiCommand::Refresh);
            }
            Action::OpenBluetooth => {
                self.bt.open();
                self.open(Route::Bluetooth);
                self.bt.busy = true;
                spawn_bt(BtCommand::Refresh);
            }
            Action::OpenDiagnostics => {
                self.diagnostics.compose(
                    self.status.as_ref(),
                    self.diag.as_ref(),
                    self.display.as_ref(),
                );
                self.open(Route::Diagnostics);
            }
            Action::SetLeds(mode) => {
                // No message along the bottom. A setting whose own row shows
                // the new value has already said it, and this interface's
                // notice line has nothing that clears it — a line about a
                // light would sit there over whatever came next.
                self.clear_notice();
                // Drawn now, on this frame.
                self.leds_pending = Some(mode);
                self.show_leds(mode);
                spawn_leds(mode);
            }
            Action::SetColorMode(mode) => {
                // Same shape as the lights: the row already shows where the
                // press moved it, so there is nothing to say along the bottom.
                self.clear_notice();
                self.color_mode_pending = Some(mode);
                self.show_color_mode(mode);
                spawn_color_mode(mode);
            }
            Action::SetResolution(choice) => {
                self.say("Çözünürlük uygulanıyor…".into());
                spawn_resolution(choice);
            }
            Action::WakeTelevision => {
                self.say("Televizyon uyandırılıyor…".into());
                spawn_kodi(KodiCommand::WakeTelevision);
            }
            Action::StandbyTelevision => {
                self.say("Televizyon beklemeye alınıyor…".into());
                spawn_kodi(KodiCommand::StandbyTelevision);
            }
            Action::RestartPlayer => {
                self.say("Oynatıcı yeniden başlatılıyor…".into());
                spawn_kodi(KodiCommand::RestartPlayer);
            }
            Action::Restart | Action::Shutdown => {
                let power = if action == Action::Restart {
                    PowerAction::Restart
                } else {
                    PowerAction::Shutdown
                };
                self.say(
                    if power == PowerAction::Restart {
                        "Yeniden başlatılıyor…"
                    } else {
                        "Kapatılıyor…"
                    }
                    .into(),
                );
                self.remember();
                self.store.flush();
                spawn_power(power);
            }
        }
    }

    // ------------------------------------------------------------- navigation

    fn open(&mut self, route: Route) {
        self.stack.push(route);
        self.paint();
        self.remember();
    }

    fn back(&mut self) {
        if self.stack.pop() {
            if self.route() != Route::Detail {
                self.detail = None;
            }
            self.paint();
            self.remember();
        }
    }

    // -------------------------------------------------------------- the data

    fn remember(&mut self) {
        let snapshot = session::Snapshot {
            screen: self.route().name().into(),
            home_row: self.home.row,
            home_col: self.home.column(),
            detail_kind: self
                .detail
                .as_ref()
                .map(|d| d.kind.clone())
                .unwrap_or_default(),
            detail_id: self
                .detail
                .as_ref()
                .map(|d| d.id.clone())
                .unwrap_or_default(),
        };
        self.store.put(snapshot);
    }

    /// What the box can run, and how it is doing, on the control plane's word.
    fn machine_read(
        &mut self,
        display: Option<model::DisplayStatus>,
        status: Option<Value>,
        diagnostics: Option<Value>,
        kodi: Option<Value>,
    ) {
        if let Some(display) = display {
            self.home.set_apps(state::app_tiles_from(&display));
            self.display = Some(display);
            if let Some(window) = self.window.upgrade() {
                window.set_apps(slint::ModelRc::from(self.home.app_tiles.clone()));
            }
        }
        if let Some(mut fresh) = status {
            // A poll that left before the viewer pressed Ok cannot know what
            // was pressed. Keep the chosen mode until the daemon answers, or
            // the row would show the old value again for one frame.
            if self.color_mode_pending.is_some() {
                let kept = self
                    .status
                    .as_ref()
                    .and_then(|status| status.get("display_color"))
                    .cloned();
                if let (Some(kept), Some(fresh)) = (kept, fresh.as_object_mut()) {
                    fresh.insert("display_color".into(), kept);
                }
            }
            if self.leds_pending.is_some() {
                let kept = self
                    .status
                    .as_ref()
                    .and_then(|status| status.get("leds"))
                    .cloned();
                if let (Some(kept), Some(fresh)) = (kept, fresh.as_object_mut()) {
                    fresh.insert("leds".into(), kept);
                }
            }
            if self.fan_pending {
                let kept = self
                    .status
                    .as_ref()
                    .and_then(|status| status.get("fan"))
                    .cloned();
                if let (Some(kept), Some(fresh)) = (kept, fresh.as_object_mut()) {
                    fresh.insert("fan".into(), kept);
                }
            }
            self.status = Some(fresh);
        }
        if diagnostics.is_some() {
            self.diag = diagnostics;
        }
        if let Some(kodi) = kodi.as_ref().filter(|_| self.here.is_none()) {
            self.now.take(kodi);
        }

        self.settings.compose(
            self.status.as_ref(),
            self.diag.as_ref(),
            self.display.as_ref(),
        );
        if self.route() == Route::Diagnostics {
            self.diagnostics.compose(
                self.status.as_ref(),
                self.diag.as_ref(),
                self.display.as_ref(),
            );
        }
        // The account screen reads the same status. It takes the new session
        // and nothing else: the poll must not move the remote or throw away
        // half a typed address.
        if self.route() == Route::Account {
            self.account.refresh(self.status.as_ref());
        }

        if let Some(window) = self.window.upgrade() {
            window.set_vitals(vitals::read(self.diag.as_ref()));
        }
        self.paint();
    }

    /// Both answers, or whichever of them arrived. False when there was nothing
    /// in either, which is the loader's signal to ask again.
    fn loaded(
        &mut self,
        home: Option<model::HomeRows>,
        library: Option<model::LibraryListing>,
    ) -> bool {
        let rows = home.unwrap_or(model::HomeRows { rows: Vec::new() });
        let shelves = state::shelves_from(&rows, library.as_ref());

        if shelves.is_empty() {
            return false;
        }

        // Whether the catalogue itself has arrived, as opposed to this board's
        // own library, which is read from a file and is therefore always
        // there.
        //
        // This is the difference between "there is something to draw" and
        // "there is nothing more coming", and conflating the two is what made
        // a cold boot show a television with three local files on it and no
        // catalogue at all. The loader stops asking when this function says
        // the answer is final; the library alone made it say so on the very
        // first attempt, sixteen milliseconds in, while the worker's upstreams
        // were still unreachable -- and nothing asked again for the rest of
        // the run. What put it right was starting an application and coming
        // back, because that restarts this process.
        //
        // So the shelves are drawn either way, and only a row from the
        // catalogue ends the retry.
        let from_catalogue = rows
            .rows
            .iter()
            .any(|row| row.addon_id != model::LIBRARY_ADDON_ID && !row.items.is_empty());

        self.meter.data_arrived();

        // Whatever the last attempt said, there is a catalogue now.
        if let Some(window) = self.window.upgrade() {
            window.set_failed(false);
        }

        self.home.set_shelves(shelves);
        self.library.build(&self.home.shelves);

        self.media.set_shelves(self.home.shelves.clone());

        if let Some(window) = self.window.upgrade() {
            window.set_media_rails(slint::ModelRc::from(self.media.rails.clone()));
            window.set_recent(RailModel {
                title: "Devam Et".into(),
                source: String::new().into(),
                items: slint::ModelRc::from(self.home.recent_tiles()),
            });
        }
        if self.route() == Route::Boot {
            self.stack.reset(Route::Home);
        }
        self.paint();
        self.restore();
        if !from_catalogue {
            eprintln!(
                "mediabox-tv.load only this board's own library so far; still asking for the catalogue"
            );
        }
        from_catalogue
    }

    /// The catalogue has not arrived yet, and the loader is going to ask again.
    ///
    /// Said plainly on the boot screen rather than as a failure: on a cold boot
    /// this is the network coming up, and a television that says "nothing could
    /// be fetched" and then quietly fixes itself has told the viewer a lie
    /// either way.
    fn still_waiting(&mut self, attempt: u32) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        if self.route() != Route::Boot {
            return;
        }
        window.set_failed(false);
        window.set_status(
            if attempt <= 2 {
                "Raflar getiriliyor…".to_string()
            } else {
                format!("Raflar getiriliyor… ({attempt}. deneme)")
            }
            .into(),
        );
    }

    /// Puts the remote back where it was before the display changed hands.
    /// What the appliance comes back to after a restart.
    ///
    /// The home screen, always. It is this product's opening screen — the
    /// launcher, the board's own vital signs, the shelf — and an appliance
    /// that reappears three screens deep in a settings tree, or on a detail
    /// page for a film somebody finished last night, is not an appliance that
    /// has started: it is one that never finished what it was doing.
    ///
    /// This used to put the viewer back where they were, which produced a
    /// specific complaint and one real fault before it. The fault was Now
    /// Playing, restored with no player running, greeting somebody with a dead
    /// end and four transport buttons that did nothing; it was fixed by
    /// excluding that one screen, and the same reasoning applies to all of
    /// them. Nothing about a previous session is true at boot.
    ///
    /// A film that is genuinely still playing is a different matter and does
    /// not come through here: `watch_the_film` adopts any film on the plane
    /// four times a second — including one this interface did not start — and
    /// opens Now Playing itself. So that screen is reached when it is true,
    /// and never because a word was left in a file.
    fn restore(&mut self) {
        // Read and dropped: the file is still written, because the session
        // carries more than a screen name, but the screen name no longer
        // decides anything.
        let _ = self.resume.take();
        self.paint();
    }

    fn fail(&mut self, why: &str) {
        if let Some(window) = self.window.upgrade() {
            window.set_failed(true);
            window.set_status(why.into());
        }
    }

    // ------------------------------------------------------------- the drawing

    fn paint(&mut self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let route = self.route();
        // A film is drawn by the display controller on the window under this
        // one, so while one is playing this surface is transparent and carries
        // the film's own controls and nothing else. No screen answers to
        // "film", which is how the rest of the interface is kept off the panel.
        if self.here.is_some() {
            window.set_screen("film".into());
            let open = self.controls_open();
            window.set_film_controls(open);
            if std::env::var_os("MEDIABOX_TV_TRACE_INPUT").is_some() {
                eprintln!("mediabox-tv.film paint controls={open}");
            }
            self.paint_now_playing(&window);
            self.paint_sheet(&window);
            return;
        }
        window.set_screen(route.name().into());

        self.paint_sheet(&window);

        match route {
            Route::Boot => {}
            Route::Home => self.paint_home(&window),
            Route::Media => self.paint_media(&window),
            Route::Search => self.paint_search(&window),
            Route::Library => self.paint_library(&window),
            Route::Detail => self.paint_detail(&window),
            Route::NowPlaying => self.paint_now_playing(&window),
            Route::Settings => self.paint_settings(&window),
            Route::Account => self.paint_account(&window),
            Route::Diagnostics => self.paint_diagnostics(&window),
            Route::Wifi => self.paint_wifi(&window),
            Route::Bluetooth => self.paint_bluetooth(&window),
        }
    }

    fn paint_sheet(&mut self, window: &MediaBoxWindow) {
        let Some(sheet) = self.sheet.as_ref() else {
            window.set_sheet_open(false);
            return;
        };
        window.set_sheet_open(true);
        window.set_sheet_title(sheet.title().into());

        match sheet {
            Sheet::Power { index } => {
                window.set_sheet_kind("power".into());
                window.set_sheet_index(*index as i32);
                window.set_sheet_rows(slint::ModelRc::new(slint::VecModel::from(
                    screens::power::CHOICES
                        .iter()
                        .map(|choice| SheetRow {
                            label: choice.label().into(),
                            hint: choice.hint().into(),
                            tone: if choice.destructive() {
                                "bad".into()
                            } else {
                                "".into()
                            },
                        })
                        .collect::<Vec<_>>(),
                )));
            }
            Sheet::Confirm { yes, .. } => {
                window.set_sheet_kind("confirm".into());
                window.set_sheet_index(i32::from(*yes));
                window.set_sheet_rows(slint::ModelRc::new(slint::VecModel::from(vec![
                    SheetRow {
                        label: "Vazgeç".into(),
                        hint: "".into(),
                        tone: "".into(),
                    },
                    SheetRow {
                        label: "Evet".into(),
                        hint: "".into(),
                        tone: "bad".into(),
                    },
                ])));
            }
        }
    }

    fn paint_home(&mut self, window: &MediaBoxWindow) {
        self.home.sync_artwork(&mut self.images);
        window.set_focus_row(self.home.row as i32);
        window.set_focus_col(self.home.column() as i32);
        window.set_recent_row(self.home.recent_row().map(|r| r as i32).unwrap_or(-1));
        self.remember();
    }

    fn paint_search(&mut self, window: &MediaBoxWindow) {
        window.set_search_query(self.search.query.clone().into());
        window.set_search_note(self.search.note.clone().into());
        window.set_search_on_keys(self.search.pane == screens::search::Pane::Keys);
        window.set_search_key_row(self.search.key_row as i32);
        window.set_search_key_col(self.search.key_col as i32);
        window.set_search_index(self.search.index as i32);
        window.set_search_columns(screens::search::COLUMNS as i32);

        let keys: Vec<KeyRow> = self
            .search
            .keys()
            .iter()
            .map(|row| KeyRow {
                keys: slint::ModelRc::new(slint::VecModel::from(
                    row.iter()
                        .map(|cap| KeyCap {
                            label: cap.label(false).into(),
                            span: cap.span() as i32,
                        })
                        .collect::<Vec<_>>(),
                )),
            })
            .collect();
        window.set_search_keys(slint::ModelRc::new(slint::VecModel::from(keys)));

        let tiles = posters(&mut self.images, &self.search.results);
        window.set_search_results(slint::ModelRc::new(slint::VecModel::from(tiles)));
    }

    fn paint_library(&mut self, window: &MediaBoxWindow) {
        window.set_library_tabs(strings(
            self.library
                .sections
                .iter()
                .map(|section| section.title.clone()),
        ));
        window.set_library_notes(strings(
            self.library
                .sections
                .iter()
                .map(|section| section.note.clone()),
        ));
        window.set_library_tab(self.library.tab as i32);
        window.set_library_on_tabs(self.library.on_tabs);
        window.set_library_index(self.library.index() as i32);
        window.set_library_columns(screens::library::COLUMNS as i32);

        let items = self.library.items().to_vec();
        let tiles = posters(&mut self.images, &items);
        window.set_library_items(slint::ModelRc::new(slint::VecModel::from(tiles)));

        let focused = self.library.focused().cloned();
        if let Some(url) = focused
            .as_ref()
            .and_then(|item| item.background.clone().or_else(|| item.poster.clone()))
        {
            let key = images::Key::new(&url, state::BACKDROP_WIDTH);
            self.images.want(&key);
            if let Some(art) = self.images.get(&key) {
                window.set_library_art(art);
            }
        }

        match self.library.focused() {
            Some(item) => {
                window.set_library_title(item.title.clone().into());
                window.set_library_facts(hero_facts(item).into());
                window.set_library_summary(item.summary.clone().unwrap_or_default().into());
                window.set_library_genres(strings(item.genres.iter().take(4).cloned()));
            }
            None => {
                window.set_library_title("".into());
                window.set_library_facts("".into());
                window.set_library_summary("".into());
                window.set_library_genres(strings(std::iter::empty()));
            }
        }
    }

    fn paint_detail(&mut self, window: &MediaBoxWindow) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };

        window.set_detail_title(detail.meta.name.clone().into());
        window.set_detail_facts(strings(detail.facts().into_iter()));
        window.set_detail_imdb(detail.has_rating());
        window.set_detail_summary(detail.meta.description.clone().unwrap_or_default().into());
        window.set_detail_genres(strings(detail.genres().into_iter()));
        window.set_detail_cast(strings(detail.meta.cast.iter().take(4).cloned()));
        window.set_detail_directors(strings(detail.meta.director.iter().take(3).cloned()));

        window.set_detail_actions(strings(detail::ACTIONS.iter().map(|a| a.to_string())));
        window.set_detail_marks(strings(detail::MARKS.iter().map(|m| m.0.to_string())));
        window.set_detail_cuts(strings(detail::MARKS.iter().map(|m| m.1.to_string())));
        window.set_detail_enabled(slint::ModelRc::new(slint::VecModel::from(
            detail.enabled().to_vec(),
        )));

        window.set_detail_sources(slint::ModelRc::new(slint::VecModel::from(
            detail.rows_for_display(),
        )));
        window.set_detail_note(detail.note.clone().into());
        window.set_detail_col(detail.action as i32);
        window.set_detail_on_sources(detail.pane == detail::Pane::Sources);
        window.set_detail_on_filter(detail.on_filter);
        window.set_detail_filter_open(detail.filter_open);
        window.set_detail_filter_focus(detail.filter_focus as i32);
        window.set_detail_source_focus(detail.source_focus as i32);
        let mut providers: Vec<String> = vec!["Tümü".into()];
        providers.extend(detail.providers());
        window.set_detail_providers(strings(providers.into_iter()));
        window.set_detail_provider(detail.provider as i32);

        if let Some(url) = detail.meta.logo.clone().filter(|url| !url.is_empty()) {
            let key = images::Key::new(&url, state::POSTER_WIDTH);
            self.images.want(&key);
            if let Some(art) = self.images.get(&key) {
                window.set_detail_logo(art);
            }
        }

        // The poster is almost always a cache hit: the shelf this screen was
        // opened from decoded it at the same width a moment ago.
        if let Some(url) = detail.meta.poster.clone() {
            let key = images::Key::new(&url, state::POSTER_WIDTH);
            self.images.want(&key);
            if let Some(art) = self.images.get(&key) {
                window.set_detail_poster(art);
            }
        }

        if let Some(url) = detail
            .meta
            .background
            .clone()
            .or_else(|| detail.meta.poster.clone())
        {
            if self.detail_backdrop.as_deref() != Some(url.as_str()) {
                self.detail_backdrop = Some(url.clone());
                self.detail_fade = if self.detail_fade > 0.5 { 0.0 } else { 1.0 };
            }
            let key = images::Key::new(&url, state::BACKDROP_WIDTH);
            self.images.want(&key);
            if let Some(art) = self.images.get(&key) {
                if self.detail_fade > 0.5 {
                    window.set_detail_art_b(art);
                } else {
                    window.set_detail_art_a(art);
                }
            }
            window.set_detail_fade(self.detail_fade);
        }

        self.remember();
    }

    fn paint_now_playing(&mut self, window: &MediaBoxWindow) {
        let now = &self.now;
        window.set_np_title(now.title.clone().into());
        window.set_np_subtitle(now.subtitle.clone().into());
        window.set_np_note(now.note.clone().into());
        // The opening message, and only while a film really is opening: from
        // the press until the first frame lands. The note itself is drawn
        // with the controls, and the controls are shut at exactly that
        // moment, so the player overlay is given it separately.
        let waiting = match self.here.as_ref() {
            Some(playing) if self.now.film && !playing.seen => self.now.note.clone(),
            _ => String::new(),
        };
        window.set_np_waiting(waiting.into());
        window.set_np_surface(now.surface.clone().into());
        window.set_np_elapsed(screens::now_playing::timecode(now.elapsed_seconds).into());
        // An unknown length is drawn as unknown. The bar stays empty with it:
        // `progress` is zero without a duration, which is the truth.
        window.set_np_duration(if now.duration_known() {
            screens::now_playing::timecode(now.duration_seconds).into()
        } else {
            slint::SharedString::from("—")
        });
        window.set_np_progress(now.progress());
        window.set_np_focus(now.focus as i32);
        window.set_np_row(match now.row {
            screens::now_playing::Row::Bar => 0,
            screens::now_playing::Row::Controls => 1,
        });
        // The tick's own time, while it is being moved. Empty the rest of the
        // time, which is how the line knows to show the film's.
        window.set_np_scrub(match now.scrub {
            Some(seconds) => screens::now_playing::timecode(seconds).into(),
            None => slint::SharedString::new(),
        });

        let playing = now.playing();
        window.set_np_actions(strings(
            screens::now_playing::CONTROLS
                .iter()
                .map(|control| control.label(playing).to_string()),
        ));
        window.set_np_controls(slint::ModelRc::new(slint::VecModel::from(
            screens::now_playing::CONTROLS
                .iter()
                .map(|control| NpControl {
                    label: control.label(playing).into(),
                    mark: control.mark(playing).into(),
                })
                .collect::<Vec<_>>(),
        )));
        // The film's own row: two layers a mark, and the hairlines between the
        // groups. Drawn for the film screen; the Kodi screen reads the four
        // controls above.
        use screens::now_playing::{FILM_CONTROLS, FILM_RULES, Menu, SPEEDS};
        window.set_np_marks(strings(
            FILM_CONTROLS
                .iter()
                .map(|control| control.fill(playing).to_string()),
        ));
        window.set_np_lines(strings(
            FILM_CONTROLS
                .iter()
                .map(|control| control.line().to_string()),
        ));
        window.set_np_rules(slint::ModelRc::new(slint::VecModel::from(
            FILM_RULES.iter().map(|at| *at as i32).collect::<Vec<_>>(),
        )));
        window.set_np_pill(now.pill.clone().into());

        // The panels, grouped the way the reference groups them: the languages
        // in the first column and that language's own tracks in the second.
        // Four English audio tracks are one row saying "English" and four rows
        // beside it, not four rows all saying the same word.
        let tracks_shown: Vec<MenuRow> = now
            .tracks_of_language()
            .into_iter()
            .filter_map(|index| now.tracks().get(index))
            .map(|track| MenuRow {
                label: track.detail.clone().into(),
                detail: "".into(),
                active: track.selected,
            })
            .collect();

        let languages = |tracks: &[screens::now_playing::Track], lead: Option<&str>| {
            let names = now.languages_of(tracks);
            let mut rows: Vec<MenuRow> = Vec::new();
            if let Some(lead) = lead {
                rows.push(MenuRow {
                    label: lead.into(),
                    detail: "".into(),
                    active: !tracks.iter().any(|track| track.selected),
                });
            }
            rows.extend(names.iter().map(|name| {
                MenuRow {
                    label: name.clone().into(),
                    detail: "".into(),
                    active: tracks
                        .iter()
                        .any(|track| track.selected && track.label == *name),
                }
            }));
            rows
        };

        let (name, rows, side): (&str, Vec<MenuRow>, Option<(String, String)>) = match now.menu {
            Menu::None => ("", Vec::new(), None),
            Menu::Subtitles => (
                "subtitles",
                languages(&now.subtitles, Some("Etkisizleştirildi")),
                Some(("Gecikme".into(), seconds_shown(now.sub_delay))),
            ),
            Menu::Audio => (
                "audio",
                languages(&now.audio, None),
                Some(("Ses gecikmesi".into(), seconds_shown(now.audio_delay))),
            ),
            Menu::Speed => (
                "speed",
                SPEEDS
                    .iter()
                    .map(|value| MenuRow {
                        // The reference's own spelling: a quarter step keeps
                        // its second digit, a half step does not.
                        label: if (value * 100.0).round() as i64 % 10 == 0 {
                            format!("{value:.1}x")
                        } else {
                            format!("{value:.2}x")
                        }
                        .into(),
                        detail: "".into(),
                        active: (value - now.speed).abs() < 0.01,
                    })
                    .collect(),
                None,
            ),
            Menu::Player => (
                "player",
                ["MediaBox", "Kodi"]
                    .iter()
                    .enumerate()
                    .map(|(index, label)| MenuRow {
                        label: (*label).into(),
                        detail: "".into(),
                        active: index == 0,
                    })
                    .collect(),
                None,
            ),
        };
        window.set_np_menu_tracks(slint::ModelRc::new(slint::VecModel::from(tracks_shown)));
        window.set_np_menu_track_focus(now.menu_track_focus as i32);
        window.set_np_menu(name.into());
        window.set_np_menu_title(
            match now.menu {
                Menu::Subtitles => "Altyazılar",
                Menu::Audio => "Ses",
                Menu::Speed => "Oynatma Hızı",
                Menu::Player => "Oynatıcı",
                Menu::None => "",
            }
            .into(),
        );
        window.set_np_menu_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
        window.set_np_menu_focus(now.menu_focus as i32);
        window.set_np_menu_column(now.menu_column as i32);
        let (side_label, side_value) = side.unwrap_or_default();
        window.set_np_menu_side_label(side_label.into());
        window.set_np_menu_side_value(side_value.into());

        window.set_np_technical(slint::ModelRc::new(slint::VecModel::from(
            now.technical
                .iter()
                .map(|(label, value)| TechRow {
                    label: label.clone().into(),
                    value: value.clone().into(),
                    tone: "".into(),
                })
                .collect::<Vec<_>>(),
        )));

        let art = now.backdrop.clone().or_else(|| now.artwork.clone());
        if let Some(url) = art {
            let key = images::Key::new(&url, state::BACKDROP_WIDTH);
            self.images.want(&key);
            if let Some(art) = self.images.get(&key) {
                window.set_np_art(art);
            }
        }
        // Wanted at poster width: it is drawn at about a fifth of the panel
        // and the shelf it was opened from has usually decoded it already.
        if let Some(url) = now.logo.clone() {
            let key = images::Key::new(&url, state::POSTER_WIDTH);
            self.images.want(&key);
            if let Some(logo) = self.images.get(&key) {
                window.set_np_logo(logo);
            }
        }
    }

    fn paint_settings(&mut self, window: &MediaBoxWindow) {
        window.set_settings_sections(strings(
            self.settings.groups.iter().map(|group| group.title.clone()),
        ));
        window.set_settings_section(self.settings.section as i32);
        window.set_settings_on_sections(self.settings.pane == screens::settings::Pane::Sections);
        window.set_settings_row(self.settings.row_index() as i32);
        window.set_settings_rows(slint::ModelRc::new(slint::VecModel::from(
            self.settings
                .rows()
                .iter()
                .map(|row| SettingRow {
                    label: row.label.clone().into(),
                    value: row.value.clone().into(),
                    hint: row.hint.clone().into(),
                    tone: row.tone.clone().into(),
                    selectable: row.selectable(),
                })
                .collect::<Vec<_>>(),
        )));
        self.paint_cooling(window);
    }

    fn paint_cooling(&self, window: &MediaBoxWindow) {
        let cooling = self.settings.is_cooling();
        window.set_settings_cooling(cooling);
        if !cooling {
            return;
        }
        let view = self.settings.cooling.view();
        fn model<T: Clone + 'static>(items: Vec<T>) -> slint::ModelRc<T> {
            slint::ModelRc::new(slint::VecModel::from(items))
        }
        let ticks = |items: &[screens::cooling::Tick]| {
            model(
                items
                    .iter()
                    .map(|tick| CoolTick {
                        at: tick.at as f32,
                        label: tick.label.clone().into(),
                        shown: tick.shown,
                    })
                    .collect(),
            )
        };
        let chips = |items: &[screens::cooling::Chip]| {
            model(
                items
                    .iter()
                    .map(|chip| CoolChip {
                        label: chip.label.clone().into(),
                        enabled: chip.enabled,
                        on: chip.on,
                        focused: chip.focused,
                    })
                    .collect(),
            )
        };
        let line = |items: &[screens::cooling::Segment]| {
            model(
                items
                    .iter()
                    .map(|seg| CoolSeg {
                        x0: seg.x0 as f32,
                        y0: seg.y0 as f32,
                        x1: seg.x1 as f32,
                        y1: seg.y1 as f32,
                    })
                    .collect(),
            )
        };
        window.set_settings_cooling_view(CoolingView {
            available: view.available,
            message: view.message.into(),
            temperature: view.temperature.into(),
            duty: view.duty.into(),
            duty_raw: view.duty_raw.into(),
            curve: view.curve.into(),
            profile: view.profile.into(),
            state: view.state.into(),
            state_tone: view.state_tone.into(),
            chips: chips(&view.chips),
            line: line(&view.line),
            previous_line: line(&view.previous_line),
            legend: view.legend.into(),
            points: model(
                view.points
                    .iter()
                    .map(|p| CoolPoint {
                        x: p.x as f32,
                        y: p.y as f32,
                        selected: p.selected,
                        last: p.last,
                    })
                    .collect(),
            ),
            x_ticks: ticks(&view.x_ticks),
            y_ticks: ticks(&view.y_ticks),
            has_now: view.now_x.is_some() && view.now_y.is_some(),
            now_x: view.now_x.unwrap_or(0.0) as f32,
            now_y: view.now_y.unwrap_or(0.0) as f32,
            now_temperature: view.now_temperature.into(),
            now_duty: view.now_duty.into(),
            trip_x: view.trip_x as f32,
            callout: view.callout.into(),
            callout_editing: view.callout_editing,
            rows: model(
                view.rows
                    .into_iter()
                    .map(|row| CoolRow {
                        number: row.number.into(),
                        temperature: row.temperature.into(),
                        duty: row.duty.into(),
                        raw: row.raw.into(),
                        locked: row.locked,
                        selected: row.selected,
                        can_add: row.can_add,
                        can_remove: row.can_remove,
                        focus_col: row.focus_col,
                        edit_col: row.edit_col,
                    })
                    .collect(),
            ),
            count: view.count.into(),
            actions: chips(&view.actions),
            note: view.note.into(),
            advanced: view.advanced,
            info: model(
                view.info
                    .into_iter()
                    .map(|(label, value)| CoolInfo {
                        label: label.into(),
                        value: value.into(),
                    })
                    .collect(),
            ),
            prompt: view.prompt,
            prompt_focus: view.prompt_focus as i32,
        });
    }

    fn paint_account(&mut self, window: &MediaBoxWindow) {
        use screens::account::{Field, Focus};

        let account = &self.account;
        let (email_before, email_after) = account.split(Field::Email);
        let (secret_before, secret_after) = account.split(Field::Password);
        window.set_account_email(account.email().into());
        window.set_account_email_before(email_before.into());
        window.set_account_email_after(email_after.into());
        window.set_account_secret_before(secret_before.into());
        window.set_account_secret_after(secret_after.into());
        // Bullets and a length. The panel is never handed the password.
        window.set_account_password_mask(account.password_mask().into());
        window.set_account_show(account.showing());
        window.set_account_focus(
            match account.focus {
                Focus::Field(Field::Email) => "email",
                Focus::Field(Field::Password) => "password",
                Focus::Show => "show",
                Focus::Keys => "keys",
                Focus::Submit => "submit",
            }
            .into(),
        );
        window.set_account_editing(
            match account.field {
                Field::Email => "email",
                Field::Password => "password",
            }
            .into(),
        );
        window.set_account_key_row(account.keys.row() as i32);
        window.set_account_key_col(account.keys.col() as i32);
        window.set_account_shifted(account.keys.shifted());
        window.set_account_signed_in(account.session.authenticated);
        window.set_account_who(account.session.email.clone().into());
        window.set_account_addons(account.session.addons as i32);
        window.set_account_busy(account.busy);
        window.set_account_notice(account.notice.clone().into());
        window.set_account_submit_label(
            if account.busy {
                "Bağlanıyor…"
            } else {
                "Giriş yap"
            }
            .into(),
        );
        window.set_account_submit_ready(account.can_submit());

        let shifted = account.keys.shifted();
        let keys: Vec<KeyRow> = account
            .keys
            .rows()
            .iter()
            .map(|row| KeyRow {
                keys: slint::ModelRc::new(slint::VecModel::from(
                    row.iter()
                        .map(|cap| KeyCap {
                            label: cap.label(shifted).into(),
                            span: cap.span() as i32,
                        })
                        .collect::<Vec<_>>(),
                )),
            })
            .collect();
        window.set_account_keys(slint::ModelRc::new(slint::VecModel::from(keys)));
    }

    fn paint_diagnostics(&mut self, window: &MediaBoxWindow) {
        window.set_diag_group(self.diagnostics.group as i32);
        window.set_diag_groups(slint::ModelRc::new(slint::VecModel::from(
            self.diagnostics
                .groups
                .iter()
                .map(|group| DiagGroup {
                    title: group.title.clone().into(),
                    rows: slint::ModelRc::new(slint::VecModel::from(
                        group
                            .rows
                            .iter()
                            .map(|row| TechRow {
                                label: row.label.clone().into(),
                                value: row.value.clone().into(),
                                tone: row.tone.clone().into(),
                            })
                            .collect::<Vec<_>>(),
                    )),
                })
                .collect::<Vec<_>>(),
        )));
    }
}

/// Poster tiles with whatever artwork is already decoded, and a request for
/// what is not.
///
/// The grid screens ask for everything they draw, because a grid is a page and
/// all of it is on the panel. The shelves have their own window instead — a
/// shelf is longer than the television, and decoding the whole catalogue to
/// draw eight of it is how an image cache becomes the resident size.
fn posters(images: &mut images::ImageManager, items: &[state::Item]) -> Vec<PosterItem> {
    items
        .iter()
        .map(|item| {
            let art = match item.poster.as_deref() {
                Some(url) => {
                    let key = images::Key::new(url, state::POSTER_WIDTH);
                    images.want(&key);
                    images.get(&key).unwrap_or_default()
                }
                None => slint::Image::default(),
            };
            PosterItem {
                id: item.id.clone().into(),
                kind: item.kind.clone().into(),
                title: item.title.clone().into(),
                subtitle: item.year.clone().unwrap_or_default().into(),
                art,
                hue: item.hue,
                progress: item.progress,
                local: item.local,
            }
        })
        .collect()
}

/// Year, rating and a genre or two, as one line. The same shape the detail
/// screen's own facts line has, so a title reads the same wherever it is shown.
fn hero_facts(item: &state::Item) -> String {
    let mut facts: Vec<String> = Vec::new();
    if let Some(year) = &item.year {
        facts.push(year.clone());
    }
    if let Some(rating) = &item.rating {
        facts.push(format!("IMDb {rating}"));
    }
    facts.extend(item.genres.iter().take(2).cloned());
    facts.join("  ·  ")
}

fn strings(values: impl Iterator<Item = String>) -> slint::ModelRc<slint::SharedString> {
    let items: Vec<slint::SharedString> = values.map(Into::into).collect();
    slint::ModelRc::new(slint::VecModel::from(items))
}

/// What a detail load came back with. The appliance's own library answers with
/// the record and its sources together; a catalogue title takes two calls.
enum DetailAnswer {
    Library(Box<model::LibraryItemEnvelope>),
    Catalogue {
        meta: Box<model::Meta>,
        listing: Box<model::StreamListing>,
        raw: Vec<serde_json::Value>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Before anything that can spawn a thread — the image workers, the state
    // store, tokio, and the Mali driver's own four. A thread inherits the mask
    // of whoever spawned it, and a SIGTERM delivered to one that has it
    // unblocked kills the process where it stands, without the display ever
    // being released. See session::block_exit_signals.
    session::block_exit_signals();

    // The display device a previous stop left lit, if there is one. Taken
    // before any thread exists; closed once this run's first frame is shown.
    fdstore::adopt();

    let started = Instant::now();
    let trace_input = !std::env::args().any(|a| a == "--quiet-input");

    // Errors from winit, glutin and Slint itself go through `log`. Off unless
    // RUST_LOG says otherwise, so the journal is not filled on an ordinary run.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // Install the RK3588 split render/display platform before Slint creates a
    // component. It renders on the Mali GBM render node and presents the
    // exported dma-buf on the Rockchip KMS card.
    platform::install()?;

    // The window, and nothing else. When the television showed nothing there
    // was no way to tell a broken interface from a broken window system, and
    // guessing at that cost an afternoon. This answers it in one run.
    if std::env::var_os("MEDIABOX_TV_SELFTEST").is_some() {
        let window = MediaBoxWindow::new()?;
        window.set_status("selftest".into());
        window.window().set_rendering_notifier(|state, _| {
            eprintln!("mediabox-tv.selftest rendering-state {state:?}");
        })?;
        eprintln!("mediabox-tv.selftest showing");
        window.run()?;
        return Ok(());
    }

    let window = MediaBoxWindow::new()?;

    let app = Rc::new(RefCell::new(App {
        window: window.as_weak(),
        stack: route::Stack::new(),
        home: state::Home::new(),
        media: screens::media::Media::new(),
        search: screens::search::Search::new(),
        library: screens::library::Library::new(),
        settings: screens::settings::Settings::new(),
        account: screens::account::Account::new(),
        wifi: screens::wireless::Wifi::new(),
        bt: screens::wireless::Bluetooth::new(),
        diagnostics: screens::diagnostics::Diagnostics::new(),
        now: screens::now_playing::NowPlaying::new(),
        detail: None,
        sheet: None,
        images: images::ImageManager::new(cache_dir(), || {
            let _ = slint::invoke_from_event_loop(|| {
                with_app(|app| {
                    if app.images.collect() {
                        app.paint();
                    }
                })
            });
        }),
        meter: metrics::Metrics::new(started),
        dispatcher: input::Dispatcher::new(trace_input),
        store: session::Store::new(state_file()),
        status: None,
        diag: None,
        leds_pending: None,
        color_mode_pending: None,
        fan_pending: false,
        display: None,
        controls_were_open: false,
        handing_over: false,
        here: None,
        detail_backdrop: None,
        notice_until: None,
        detail_fade: 0.0,
        epoch: 0,
        resume: session::read(state_file()),
    }));
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));

    window.set_nav(strings(state::NAV.iter().map(|n| n.label().to_string())));

    // The last position, written the moment the display changed hands. Coming
    // back from a film should not mean starting again at the top of the home
    // screen.
    session::on_shutdown(|| {
        with_app(|app| {
            app.remember();
            app.store.flush();
        });
        // The store writes on its own thread; give it the moment it needs
        // rather than racing the exit below.
        std::thread::sleep(Duration::from_millis(200));
    });

    // A picture of what is on the television, on request. `kill -USR1` from the
    // appliance, and the next frame lands under /var/lib/mediabox-ui/snapshots
    // named after the screen it is of. It is a real scanout frame at the
    // panel's own resolution, which is the only kind of screenshot worth having
    // from a process that holds DRM master.
    session::on_snapshot_request(|| {
        let _ = slint::invoke_from_event_loop(|| {
            with_app(|app| {
                let name = app.route().name().to_string();
                let path = std::path::PathBuf::from(snapshot_dir()).join(format!("{name}.png"));
                platform::snapshot_to(path);
                if let Some(window) = app.window.upgrade() {
                    window.window().request_redraw();
                }
            });
        });
    });

    // Frames are counted from the renderer rather than from a timer, so the
    // number is what was actually presented and not what we hoped for. It also
    // means the idle CPU figure this reports is not inflated by the thing
    // reporting it.
    {
        let handle = window.as_weak();
        let reported = std::cell::Cell::new(false);
        window
            .window()
            .set_rendering_notifier(move |state, _| {
                if !matches!(state, slint::RenderingState::AfterRendering) {
                    return;
                }
                with_app(|app| app.meter.frame());
                if !reported.replace(true) {
                    if let Some(window) = handle.upgrade() {
                        report_surface(&window);
                    }
                }
            })
            .expect("this renderer cannot report when it has drawn");
    }

    // Keys from the appliance's own keyboards, through libinput. The remote
    // does not come this way — the daemon is the authority for it — and the
    // dispatcher drops a press that arrives on both roads.
    window.on_key_pressed(move |text| {
        let Some(first) = text.chars().next() else {
            return;
        };
        // Backspace is a deletion where there is text and Back where there is
        // not; only the screen knows which, so it is not routed as an action.
        if input::is_backspace(text.as_str()) {
            with_app(|app| app.backspace());
            return;
        }
        match input::action_for_key(text.as_str()) {
            Some(action) => with_app(|app| {
                if let Some(action) = app.dispatcher.accept(action, input::Origin::Keyboard) {
                    app.act(action);
                }
            }),
            None => with_app(|app| app.typed(first)),
        }
    });

    // Everything the daemon normalises — the CEC remote, the phone remote, an
    // injected action — arrives on its event stream.
    spawn_bus_listener();
    spawn_loader();
    // The launcher and the machine's vitals. Started beside the catalogue
    // rather than after it: the applications this box can run do not depend on
    // a third-party addon answering, and the home screen should not look empty
    // while one is being waited for.
    spawn_machine_poll();

    let reporter = slint::Timer::default();
    reporter.start(slint::TimerMode::Repeated, Duration::from_secs(5), || {
        with_app(|app| {
            let held = app.images.held_mb();
            if let Some(line) = app.meter.report_due() {
                eprintln!("{line} art_cpu_mb={held}");
            }
        });
    });

    // A film ends by itself as often as it is stopped, and when it does the
    // player exits and the video window goes dark — this process is the one
    // holding it, so it knows without asking anybody.
    let ended = slint::Timer::default();
    ended.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(250),
        || {
            with_app(|app| {
                app.watch_the_film();
                app.expire_notice();
            });
        },
    );

    window.set_status("Raflar getiriliyor…".into());
    window.run()?;
    Ok(())
}

/// The panel, the buffer and the scale, as this process sees them.
fn report_surface(window: &MediaBoxWindow) {
    let w = window.window();
    let physical = w.size();
    let scale = w.scale_factor();
    eprintln!(
        "mediabox-tv.surface buffer={}x{} scale={:.3} logical={:.0}x{:.0} env_scale={}",
        physical.width,
        physical.height,
        scale,
        physical.width as f32 / scale,
        physical.height as f32 / scale,
        std::env::var("SLINT_SCALE_FACTOR").unwrap_or_else(|_| "unset".into()),
    );
}

/// One short-lived runtime per request. The control plane answers in
/// milliseconds for everything local and seconds for anything that fans out to
/// addons; keeping a runtime alive between those would be keeping threads alive
/// for an interface that is usually doing nothing at all.
/// A keyboard grid as Slint wants it. Shared by the account form and the
/// Wi-Fi password face, which draw the same letters.
fn key_rows(grid: &crate::keyboard::Grid) -> slint::ModelRc<KeyRow> {
    let shifted = grid.shifted();
    let rows: Vec<KeyRow> = grid
        .rows()
        .iter()
        .map(|row| KeyRow {
            keys: slint::ModelRc::new(slint::VecModel::from(
                row.iter()
                    .map(|cap| KeyCap {
                        label: cap.label(shifted).into(),
                        span: cap.span() as i32,
                    })
                    .collect::<Vec<_>>(),
            )),
        })
        .collect();
    slint::ModelRc::new(slint::VecModel::from(rows))
}

enum WifiCommand {
    /// Status and a scan, which is what an opening screen wants.
    Refresh,
    Status,
    Scan,
    Connect { ssid: String, psk: Option<String> },
    Forget(String),
    Power(bool),
}

enum BtCommand {
    Refresh,
    Scan,
    Power(bool),
    Pair(String),
    Connect(String),
    Disconnect(String),
    Forget(String),
}

/// Ask the daemon about the Wi-Fi radio.
///
/// The passphrase travels in the request body and is dropped with this future.
/// It is never a process argument here and never printed: the error path below
/// reports the daemon's own sentence, which is written not to contain it.
fn spawn_wifi(command: WifiCommand) {
    detached("mediabox-tv-wifi", async move {
        let client = rpc::Client::new(socket_path());
        // A scan answers with a network list; everything else answers with the
        // radio's state, and the screen folds them in differently.
        let scanned = matches!(command, WifiCommand::Scan | WifiCommand::Refresh);
        let answer = match command {
            WifiCommand::Refresh | WifiCommand::Scan => client.wifi_scan().await,
            WifiCommand::Status => client.wifi_status().await,
            WifiCommand::Connect { ssid, psk } => client.wifi_connect(&ssid, psk.as_deref()).await,
            WifiCommand::Forget(ssid) => client.wifi_forget(&ssid).await,
            WifiCommand::Power(on) => client.wifi_power(on).await,
        };
        let answer = answer.map_err(|error| error.to_string());
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.wifi_answer(answer, scanned));
        });
    });
}

fn spawn_bt(command: BtCommand) {
    detached("mediabox-tv-bluetooth", async move {
        let client = rpc::Client::new(socket_path());
        let answer = match command {
            BtCommand::Refresh => client.bluetooth_status().await,
            BtCommand::Scan => client.bluetooth_scan().await,
            BtCommand::Power(on) => client.bluetooth_power(on).await,
            BtCommand::Pair(address) => client.bluetooth_pair(&address).await,
            BtCommand::Connect(address) => client.bluetooth_connect(&address).await,
            BtCommand::Disconnect(address) => client.bluetooth_disconnect(&address).await,
            BtCommand::Forget(address) => client.bluetooth_remove(&address).await,
        };
        let answer = answer.map_err(|error| error.to_string());
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.bt_answer(answer));
        });
    });
}

fn detached(name: &str, work: impl std::future::Future<Output = ()> + Send + 'static) {
    let name = name.to_string();
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(work);
        })
        .expect("a control-plane thread could not be started");
}

fn spawn_detail_load(epoch: u64, kind: String, id: String) {
    detached("mediabox-tv-detail", async move {
        let client = rpc::Client::new(socket_path());

        let answer = if kind == "library" || id.starts_with("library:") {
            client
                .library_item(&id)
                .await
                .map(|envelope| DetailAnswer::Library(Box::new(envelope)))
                .map_err(|e| e.to_string())
        } else {
            // Together rather than one after the other: the record comes from
            // one addon and the sources from every addon that has any, and the
            // screen is already drawn from the shelf's seed either way.
            let (meta, streams) = tokio::join!(client.meta(&kind, &id), client.streams(&kind, &id));

            match (meta, streams) {
                (Ok(meta), Ok(streams)) => {
                    // The parsed listing is what the rows are laid out from;
                    // the raw array is what goes back when one is chosen.
                    let raw = streams
                        .get("streams")
                        .and_then(|value| value.as_array())
                        .cloned()
                        .unwrap_or_default();
                    match serde_json::from_value::<model::StreamListing>(streams) {
                        Ok(listing) => Ok(DetailAnswer::Catalogue {
                            meta: Box::new(meta.meta),
                            listing: Box::new(listing),
                            raw,
                        }),
                        Err(e) => Err(format!("kaynak listesi çözülemedi: {e}")),
                    }
                }
                (Err(e), _) | (_, Err(e)) => Err(e.to_string()),
            }
        };

        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.detail_loaded(epoch, answer));
        });
    });
}

/// One search, after the typing has stopped.
///
/// The debounce is here rather than on a timer in the interface: the thread is
/// cheap, and a search that is already stale when it is sent costs the media
/// core a fan-out over every addon for an answer nobody will see.
fn spawn_search(generation: u64, query: String) {
    // What the box is currently typing, readable from a thread that has no
    // access to the interface. The screen's own generation counter is the
    // authority; this is a copy of it kept where the debounce can see it.
    static TYPING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    TYPING.store(generation, std::sync::atomic::Ordering::Relaxed);

    detached("mediabox-tv-search", async move {
        tokio::time::sleep(SEARCH_DEBOUNCE).await;
        if TYPING.load(std::sync::atomic::Ordering::Relaxed) != generation {
            // A later keystroke already owns the screen; this query was never
            // what the viewer was asking for.
            return;
        }

        let client = rpc::Client::new(socket_path());
        let answer = client
            .search(&query)
            .await
            .map(|results| {
                let mut seen = std::collections::HashSet::new();
                results
                    .rows
                    .iter()
                    .flat_map(|row| row.items.iter())
                    .filter(|preview| seen.insert(preview.id.clone()))
                    .map(state::Item::from_preview)
                    .take(60)
                    .collect::<Vec<_>>()
            })
            .map_err(|e| e.to_string());

        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.searched(generation, answer));
        });
    });
}

fn spawn_plan(epoch: u64, url: Option<String>, raw: serde_json::Value) {
    detached("mediabox-tv-plan", async move {
        let client = rpc::Client::new(socket_path());
        let plan = match url.as_deref() {
            Some(url) => client.plan_for_url(url).await,
            None => client.plan_for_stream(&raw).await,
        };
        let plan = match plan {
            Ok(plan) => Some(plan),
            Err(e) => {
                eprintln!("mediabox-tv.plan failed: {e}");
                None
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.planned(epoch, plan));
        });
    });
}

/// Asks the control plane to hand the film over to Kodi.
/// A delay, as the reference writes one: a tenth of a second, with a comma.
fn seconds_shown(seconds: f64) -> String {
    format!("{seconds:.1}s").replace('.', ",")
}

fn spawn_handoff() {
    detached("mediabox-tv-handoff", async move {
        let client = rpc::Client::new(socket_path());
        // This process is being stopped while the call is in flight — the
        // control plane stops the unit as part of handing the display over —
        // so a transport error here is as likely to be the handover working as
        // it is to be a failure, and only a refusal the daemon actually
        // answered with is reported.
        match client.handoff_to_kodi().await {
            Ok(_) => eprintln!("mediabox-tv.play handed over to kodi"),
            Err(e) => {
                eprintln!("mediabox-tv.handoff failed: {e}");
                let message = e.to_string();
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|app| app.handoff_failed(message));
                });
            }
        }
    });
}

/// A film this interface started on its own video plane.
struct Playing {
    /// When the player was asked to start. A source takes a second or two to
    /// open and the plane is dark until it has.
    asked: std::time::Instant,
    /// Until when the controls stay up. None is a film with nothing over it,
    /// which is the ordinary state of watching one.
    controls_until: Option<std::time::Instant>,
    /// When the player was last asked where it had got to.
    polled: Option<std::time::Instant>,
    /// Whether a frame has actually reached the plane.
    ///
    /// This is the whole reason this is a struct rather than a flag. The
    /// watcher below reads "no picture" as "the film has ended", and for the
    /// first seconds of every film that is exactly wrong: the interface
    /// believed the film was already over before its first frame arrived, went
    /// back to the home screen, and sent every transport key from then on to
    /// Kodi — so a film played with no way to pause, seek or stop it, over a
    /// home screen showing through its own letterbox.
    seen: bool,
}

impl Playing {
    fn new() -> Self {
        Self {
            asked: std::time::Instant::now(),
            controls_until: None,
            polled: None,
            seen: false,
        }
    }
}

/// How long a film may take to put its first frame on the plane before the
/// interface stops waiting for it. Measured on the appliance: a local file is
/// on screen in under a second, a stream over the network in two to four.
const FIRST_FRAME_GRACE: Duration = Duration::from_secs(25);

/// How long a line along the bottom of the screen stays up.
///
/// Long enough to read a sentence twice from a sofa, short enough that it is
/// gone before the viewer has finished doing the next thing.
const NOTICE_LIFETIME: Duration = Duration::from_secs(6);

/// How long the controls stay up after the last press.
const CONTROLS_LINGER: Duration = Duration::from_secs(5);

/// How often the player is asked where it has got to, while it is up.
const POSITION_INTERVAL: Duration = Duration::from_millis(900);

/// What the remote can do to a film playing on this interface's own video
/// plane. Closed for the same reason `KodiCommand` is.
enum HereCommand {
    PlayPause,
    Stop,
    Seek(i64),
    SeekTo(u64),
    Subtitle(i64),
    Audio(i64),
    SubtitleDelay(f64),
    AudioDelay(f64),
    Speed(f64),
    Scale(mediabox_core::ScaleMode),
}

/// Asks the interface's own player where it has got to.
fn spawn_here_status() {
    detached("mediabox-tv-here-status", async move {
        let client = rpc::Client::new(socket_path());
        let Ok(status) = client.status_here().await else {
            return;
        };
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.film_moved(status));
        });
    });
}

fn spawn_here(command: HereCommand) {
    detached("mediabox-tv-here", async move {
        let client = rpc::Client::new(socket_path());
        let answer = match command {
            HereCommand::PlayPause => client.transport_here(serde_json::json!("play_pause")).await,
            HereCommand::Seek(seconds) => {
                client
                    .transport_here(serde_json::json!({"seek": {"seconds": seconds}}))
                    .await
            }
            HereCommand::SeekTo(seconds) => {
                client
                    .transport_here(serde_json::json!({"seek_to": {"seconds": seconds}}))
                    .await
            }
            HereCommand::Subtitle(id) => {
                client
                    .transport_here(serde_json::json!({"subtitle": {"id": id}}))
                    .await
            }
            HereCommand::Audio(id) => {
                client
                    .transport_here(serde_json::json!({"audio": {"id": id}}))
                    .await
            }
            HereCommand::SubtitleDelay(seconds) => {
                client
                    .transport_here(serde_json::json!({"subtitle_delay": {"seconds": seconds}}))
                    .await
            }
            HereCommand::AudioDelay(seconds) => {
                client
                    .transport_here(serde_json::json!({"audio_delay": {"seconds": seconds}}))
                    .await
            }
            HereCommand::Speed(value) => {
                client
                    .transport_here(serde_json::json!({"speed": {"value": value}}))
                    .await
            }
            HereCommand::Scale(mode) => {
                client
                    .transport_here(serde_json::json!({"scale": {"mode": mode}}))
                    .await
            }
            HereCommand::Stop => client.stop_here().await,
        };
        if let Err(e) = answer {
            eprintln!("mediabox-tv.here failed: {e}");
        }
    });
}

/// Starts the film on this interface's own video plane.
///
/// The name and the length go with it. Neither is the player's to work out —
/// a film opened from the catalogue is played through a session whose address
/// is a hexadecimal identifier with no duration in it — and this is the one
/// moment both are known.
fn spawn_play_here(
    url: Option<String>,
    raw: serde_json::Value,
    title: String,
    runtime: Option<u64>,
) {
    detached("mediabox-tv-play-here", async move {
        let client = rpc::Client::new(socket_path());
        let stream = url.is_none().then_some(&raw);
        match client
            .play_here(url.as_deref(), stream, 0, Some(title.as_str()), runtime)
            .await
        {
            Ok(_) => eprintln!("mediabox-tv.play started here=true"),
            Err(e) => {
                eprintln!("mediabox-tv.play here failed: {e}");
                let message = e.to_string();
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|app| {
                        app.here = None;
                        if let Some(window) = app.window.upgrade() {
                            window.set_detail_note(format!("Oynatılamadı — {message}").into());
                        }
                    });
                });
            }
        }
    });
}

/// The few things this interface asks the control plane to do that are not
/// about the catalogue. Each is one closed variant rather than a command, for
/// the same reason the daemon's own request type is an enum.
enum KodiCommand {
    PlayPause,
    Stop,
    Seek(i64),
    RestartPlayer,
    WakeTelevision,
    StandbyTelevision,
}

fn spawn_kodi(command: KodiCommand) {
    detached("mediabox-tv-control", async move {
        let client = rpc::Client::new(socket_path());
        let answer = match command {
            KodiCommand::PlayPause => client.kodi_play_pause().await,
            KodiCommand::Stop => client.kodi_stop().await,
            KodiCommand::Seek(seconds) => client.kodi_seek(seconds).await,
            KodiCommand::RestartPlayer => client.kodi_restart().await,
            KodiCommand::WakeTelevision => client.cec_wake_tv().await,
            KodiCommand::StandbyTelevision => client.cec_standby_tv().await,
        };
        if let Err(e) = answer {
            eprintln!("mediabox-tv.control failed: {e}");
        }
    });
}

/// Sign in to a Stremio account.
///
/// The password is moved in here, used once, and dropped when this future
/// ends. It is a field in a JSON body over the daemon's socket: never a process
/// argument, never a shell word, and never printed — the error path below
/// reports the daemon's message and nothing of what was typed.
fn spawn_media_login(email: String, password: String) {
    detached("mediabox-tv-account", async move {
        let client = rpc::Client::new(socket_path());
        let answer = match client.media_login(&email, &password).await {
            // The login's own reply is not what the screen shows: a fresh
            // status is, so that what is drawn is what the box would tell
            // anybody else who asked.
            Ok(_) => match client.status().await {
                Ok(status) => Ok(status),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        };
        drop(password);
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.account_answered(answer));
        });
    });
}

fn spawn_media_logout() {
    detached("mediabox-tv-account", async move {
        let client = rpc::Client::new(socket_path());
        let answer = match client.media_logout().await {
            Ok(_) => match client.status().await {
                Ok(status) => Ok(status),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        };
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.account_answered(answer));
        });
    });
}

/// Fetch the catalogue again, because the account behind it changed.
///
/// The loader thread runs until it has something and then stops, which is
/// right for a box that has just been turned on and wrong for one whose
/// add-ons have just been replaced.
fn spawn_home_reload() {
    detached("mediabox-tv-reload", async move {
        let client = rpc::Client::new(socket_path());
        let (home, library) = tokio::join!(client.home(), client.library());
        let home = home.ok();
        let library = library.ok();
        if home.is_none() && library.is_none() {
            return;
        }
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| {
                app.loaded(home, library);
            });
        });
    });
}

/// The board's indicator lights.
///
/// Unlike the fire-and-forget calls above, this one carries its answer back to
/// the event loop. The daemon replies with the light status it actually
/// reached, and that is what the row must show: a press that was refused —
/// a board with no controllable lights, an unwritable sysfs — has to put the
/// row back rather than leave the chosen mode sitting there as though it had
/// worked.
fn spawn_color_mode(mode: Option<(mediabox_core::ColorFormat, u8)>) {
    detached("mediabox-tv-color-mode", async move {
        let client = rpc::Client::new(socket_path());
        let answer = match client.display_color_mode_set(mode).await {
            Ok(status) => Some(status),
            Err(error) => {
                eprintln!("mediabox-tv.color-mode failed: {error}");
                None
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.color_mode_answered(answer));
        });
    });
}

/// Remember a resolution; the daemon then restarts this process on it.
fn spawn_resolution(choice: mediabox_core::ResolutionChoice) {
    detached("mediabox-tv-resolution", async move {
        let client = rpc::Client::new(socket_path());
        if let Err(error) = client.display_resolution_set(choice).await {
            eprintln!("mediabox-tv.resolution failed: {error}");
        }
    });
}

/// Save a fan curve for the next boot, or `None` to go back to the board's
/// own. The answer comes back to the event loop either way: a refusal is the
/// daemon's to explain.
fn spawn_fan(curve: Option<mediabox_core::FanCurve>) {
    detached("mediabox-tv-fan", async move {
        let client = rpc::Client::new(socket_path());
        let reset = curve.is_none();
        let answer = match &curve {
            Some(curve) => client.fan_curve_set(curve).await,
            None => client.fan_curve_reset().await,
        }
        .map_err(|error| {
            eprintln!("mediabox-tv.fan failed: {error}");
            error.to_string()
        });
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.fan_answered(reset, answer));
        });
    });
}

fn spawn_leds(mode: mediabox_core::LedMode) {
    detached("mediabox-tv-leds", async move {
        let client = rpc::Client::new(socket_path());
        let answer = match client.leds_set(mode).await {
            Ok(status) => Some(status),
            Err(error) => {
                eprintln!("mediabox-tv.leds failed: {error}");
                None
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.leds_answered(answer));
        });
    });
}

/// The appliance's own power state, and the only call in this process that can
/// change it. Reached from one place: a confirmation whose focus started on
/// "Vazgeç" and was deliberately moved.
fn spawn_power(action: PowerAction) {
    detached("mediabox-tv-power", async move {
        eprintln!("mediabox-tv.power confirmed action={action:?}");
        let client = rpc::Client::new(socket_path());
        if let Err(e) = client.system_power(action).await {
            eprintln!("mediabox-tv.power failed: {e}");
            let message = e.to_string();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|app| app.say(format!("Yapılamadı — {message}")));
            });
        }
    });
}

/// Hands the television to another application.
fn spawn_launch(id: String, name: String) {
    detached("mediabox-tv-launch", async move {
        let client = rpc::Client::new(socket_path());
        if let Err(e) = client.application_launch(&id).await {
            eprintln!("mediabox-tv.launch {id} failed: {e}");
            let message = format!("{name} açılamadı — {e}");
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|app| app.say(message));
            });
        } else {
            eprintln!("mediabox-tv.launch {id} started");
        }
    });
}

/// The machine's own readings, on a slow timer.
///
/// One thread for all four answers: they are drawn on the same screens and
/// asking for them separately would put four round trips a second apart on the
/// same panel.
fn spawn_machine_poll() {
    detached("mediabox-tv-vitals", async move {
        let client = rpc::Client::new(socket_path());
        loop {
            let (display, status, diagnostics, kodi) = tokio::join!(
                client.applications(),
                client.status(),
                client.diagnostics(),
                client.kodi_status(),
            );
            let display = display.ok();
            let status = status.ok();
            let diagnostics = diagnostics.ok();
            let kodi = kodi.ok();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|app| app.machine_read(display, status, diagnostics, kodi));
            });
            tokio::time::sleep(vitals::EVERY).await;
        }
    });
}

fn spawn_open(url: String) {
    detached("mediabox-tv-open", async move {
        let client = rpc::Client::new(socket_path());
        if let Err(e) = client.browser_open(&url).await {
            eprintln!("mediabox-tv.trailer failed: {e}");
        }
    });
}

/// Asks the control plane for the home surface, until it answers with one.
///
/// The two calls go out together on purpose: the library is this appliance's
/// own and answers in milliseconds, while the catalogues are a fan-out over
/// third-party hosts. Waiting for the second before drawing the first is time
/// the viewer spends looking at a name and a spinner.
///
/// It asks again if nothing came back, and that is not defensive
/// programming — it is the difference between a working television and a broken
/// one after a power cut. Measured on the appliance, one second into a cold
/// boot:
///
///   mediabox-tv.load media_home+library_ms=16
///   mediabox-tv.load home failed: media worker HTTP 502 Bad Gateway:
///     UPSTREAM_FAILED api.strem.io is unreachable (urlopen error [Errno 16])
///
/// The daemon was up; its upstreams were not, because the network was still
/// coming up. The interface asked sixteen milliseconds after it started, took
/// the refusal as the answer, and showed an empty catalogue until somebody
/// restarted the unit — which, on this box, is what starting a film and coming
/// back happens to do. Hence the retry, and hence the unit now waiting for the
/// network as well as for the daemon.
fn spawn_loader() {
    std::thread::Builder::new()
        .name("mediabox-tv-data".into())
        .spawn(|| {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };

            runtime.block_on(async {
                let client = rpc::Client::new(socket_path());

                for attempt in 1u32.. {
                    let at = Instant::now();
                    let (home, library) = tokio::join!(client.home(), client.library());
                    eprintln!(
                        "mediabox-tv.load attempt={attempt} media_home+library_ms={}",
                        at.elapsed().as_millis()
                    );

                    let home = match home {
                        Ok(home) => Some(home),
                        Err(e) => {
                            eprintln!("mediabox-tv.load home failed: {e}");
                            None
                        }
                    };
                    let library = match library {
                        Ok(library) => Some(library),
                        Err(e) => {
                            eprintln!("mediabox-tv.load library failed: {e}");
                            None
                        }
                    };

                    // Whether this answer is worth drawing is the home model's
                    // decision, not this thread's, so it is asked.
                    let (sender, receiver) = tokio::sync::oneshot::channel();
                    let posted = slint::invoke_from_event_loop(move || {
                        with_app(|app| {
                            let landed = app.loaded(home, library);
                            let _ = sender.send(landed);
                        });
                    });
                    if posted.is_err() {
                        return;
                    }
                    if receiver.await.unwrap_or(false) {
                        return;
                    }

                    // One second, two, four, eight, then every fifteen. A
                    // television left on overnight with no network should be
                    // showing its catalogue the moment there is one, without
                    // having asked for it four thousand times.
                    let wait = match attempt {
                        1 => 1,
                        2 => 2,
                        3 => 4,
                        4 => 8,
                        _ => 15,
                    };
                    let _ = slint::invoke_from_event_loop(move || {
                        with_app(|app| app.still_waiting(attempt));
                    });
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                }
            });
        })
        .expect("the loader thread could not be started");
}

/// Reads the daemon's event stream for as long as the process lives,
/// reconnecting when it ends.
fn spawn_bus_listener() {
    std::thread::Builder::new()
        .name("mediabox-tv-bus".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    eprintln!("mediabox-tv.bus no runtime: {e}");
                    return;
                }
            };

            runtime.block_on(async move {
                loop {
                    if let Err(e) = read_events().await {
                        eprintln!("mediabox-tv.bus disconnected: {e}");
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        })
        .expect("the input thread could not be started");
}

async fn read_events() -> Result<(), Box<dyn std::error::Error>> {
    let mut response = reqwest::Client::new()
        .get(events_url())
        .send()
        .await?
        .error_for_status()?;

    // Frames are newline-delimited and small; assembling them here avoids
    // pulling a stream adapter crate in for four lines of work.
    let mut pending = String::new();
    while let Some(chunk) = response.chunk().await? {
        pending.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(at) = pending.find('\n') {
            let line: String = pending.drain(..=at).collect();
            deliver(line.trim_end());
        }
    }

    Ok(())
}

fn deliver(line: &str) {
    // Keepalives arrive as comment frames and carry no payload.
    let Some(payload) = line.strip_prefix("data: ") else {
        return;
    };
    let Ok(event) = serde_json::from_str::<InputEvent>(payload) else {
        return;
    };

    // Only the press edge. Through CEC the press, the hold and the release each
    // arrive as their own event.
    if !event.pressed {
        return;
    }

    let action = event.action;
    let _ = slint::invoke_from_event_loop(move || {
        with_app(|app| {
            if let Some(action) = app.dispatcher.accept(action, input::Origin::Bus) {
                app.act(action);
            }
        });
    });
}
