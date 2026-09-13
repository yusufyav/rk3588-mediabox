//! The shell: which screen is showing, and how a remote reaches it.

use crate::focus::{self, Direction};
use crate::screens;
use crate::model::MetaPreview;
use crate::{api, model};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{EventSource, MessageEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    /// The catalogue, which is an application of the box rather than its home
    /// screen. It used to be printed onto the home screen itself.
    Media,
    Browser,
    Search,
    Detail { kind: String, id: String },
    /// One shelf opened out into a grid. `addon` names where the titles come
    /// from: an addon's id for a catalogue, or one of the library sentinels.
    Collection {
        addon: String,
        kind: String,
        catalog: String,
        title: String,
    },
    NowPlaying,
    Settings,
}

impl Route {
    fn tab(&self) -> &'static str {
        match self {
            Self::Home
            | Self::Media
            | Self::Browser
            | Self::Detail { .. }
            | Self::Collection { .. } => "home",
            Self::Search => "search",
            Self::NowPlaying => "now",
            Self::Settings => "settings",
        }
    }
}

/// Navigation, with the back stack a remote's Back button needs.
///
/// Each step of the stack carries the name of the element the remote was on
/// when that screen was left. Coming back without it lands on whatever the
/// screen autofocuses — the hero's first button — which on a television reads
/// as having lost your place, because you have.
#[derive(Clone, Copy)]
pub struct Nav {
    route: RwSignal<Route>,
    stack: RwSignal<Vec<(Route, Option<String>)>>,
    /// Set by Back: the element the next render should put the remote on.
    restore: RwSignal<Option<String>>,
    /// Bumped by every accepted input. Anything waiting for "the person has
    /// not acted yet" watches this rather than guessing from a timer.
    pulse: RwSignal<u64>,
    /// What the shelf already knew about the title being opened.
    ///
    /// The detail screen used to start from nothing and ask the addon for a
    /// record it mostly had in its hand, so opening a film showed a black
    /// screen — for as long as the slowest addon took — where the artwork and
    /// the title should have been. Handed over, the page is drawn on the first
    /// frame and the full record fills in the rest when it lands.
    seed: RwSignal<Option<MetaPreview>>,
}

impl Nav {
    pub fn route(&self) -> Route {
        self.route.get()
    }

    /// Hand the next screen what this one already knows about the title.
    pub fn seed(&self, item: MetaPreview) {
        self.seed.set(Some(item));
    }

    /// The handed-over preview, if it is about the title being asked for.
    pub fn seeded(&self, id: &str) -> Option<MetaPreview> {
        self.seed
            .get_untracked()
            .filter(|item| item.id == id)
    }

    pub fn go(&self, next: Route) {
        let current = self.route.get_untracked();
        if current == next {
            return;
        }
        let was_on = focus::current_key();
        self.stack.update(|stack| {
            stack.push((current, was_on));
            // Deep enough for any real journey, bounded so a long session
            // cannot grow it without limit.
            if stack.len() > 32 {
                stack.remove(0);
            }
        });
        self.restore.set(None);
        self.route.set(next);
    }

    /// Replace the current screen without adding a step to go back through.
    pub fn swap(&self, next: Route) {
        self.route.set(next);
    }

    pub fn back(&self) {
        let previous = self.stack.try_update(|stack| stack.pop()).flatten();
        match previous {
            Some((route, was_on)) => {
                self.restore.set(was_on);
                self.route.set(route);
            }
            None => {
                self.restore.set(None);
                self.route.set(Route::Home);
            }
        }
    }

    pub fn home(&self) {
        self.stack.set(Vec::new());
        self.restore.set(None);
        self.route.set(Route::Home);
    }
}

/// The artwork the room takes its colour from.
///
/// Held in context and written from an effect rather than from a component's
/// body. Setting a signal while rendering is what broke navigation here: the
/// write landed in the middle of building the screen, and coming back from a
/// title produced a Home with no rails on it at all.
#[derive(Clone, Copy)]
pub struct Ambient(pub RwSignal<Option<String>>);

impl Ambient {
    pub fn show(&self, url: Option<String>) {
        if self.0.get_untracked() != url {
            self.0.set(url);
        }
    }
}

/// A short message about something that just happened, good or bad.
#[derive(Clone, Copy)]
pub struct Toaster(RwSignal<Option<(String, bool)>>);

impl Toaster {
    pub fn say(&self, message: impl Into<String>) {
        self.0.set(Some((message.into(), false)));
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.0.set(Some((message.into(), true)));
    }

    pub fn clear(&self) {
        self.0.set(None);
    }
}

// ------------------------------------------------------- continue watching

/// What was last played, kept on the device that played it.
///
/// The appliance has no Stremio account, so there is no server-side "continue
/// watching" to read. Rather than invent one — or worse, fill the row with
/// something that is not real — the row is built from titles this browser has
/// actually started, and it is simply absent until there are some.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub release_info: Option<String>,
}

const RECENT_KEY: &str = "mediabox.continue";
const RECENT_MAX: usize = 12;

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

pub fn recents() -> Vec<Recent> {
    // Storage can be unavailable or hold something from an older build; either
    // way the row is empty rather than the screen being broken.
    storage()
        .and_then(|store| store.get_item(RECENT_KEY).ok().flatten())
        .and_then(|raw| serde_json::from_str::<Vec<Recent>>(&raw).ok())
        .unwrap_or_default()
}

pub fn remember(entry: Recent) {
    let mut found = recents();
    found.retain(|item| item.id != entry.id);
    found.insert(0, entry);
    found.truncate(RECENT_MAX);
    if let Some(store) = storage()
        && let Ok(raw) = serde_json::to_string(&found)
    {
        let _ = store.set_item(RECENT_KEY, &raw);
    }
}

impl From<Recent> for model::MetaPreview {
    fn from(entry: Recent) -> Self {
        Self {
            id: entry.id,
            kind: entry.kind,
            name: entry.name,
            poster: entry.poster,
            background: entry.background,
            logo: None,
            description: None,
            release_info: entry.release_info,
            imdb_rating: None,
            genres: Vec::new(),
            addon_id: None,
            // What this box remembers is that the title was opened, not how
            // far it was watched; the account is what knows that.
            state: None,
        }
    }
}

// ------------------------------------------------------------------- input

fn transport(request: serde_json::Value) {
    spawn_local(async move {
        let _ = api::control(request).await;
    });
}

/// One normalized action, from whichever source produced it.
fn dispatch(action: &str, nav: Nav) -> bool {
    let handled = dispatch_inner(action, nav);
    if handled {
        nav.pulse.update(|value| *value = value.wrapping_add(1));
    }
    handled
}

fn dispatch_inner(action: &str, nav: Nav) -> bool {
    match action {
        "up" => focus::step(Direction::Up),
        "down" => focus::step(Direction::Down),
        "left" => focus::step(Direction::Left),
        "right" => focus::step(Direction::Right),
        "ok" => {
            focus::activate();
            true
        }
        "back" => {
            nav.back();
            true
        }
        "home" => {
            nav.home();
            true
        }
        "play" | "pause" | "play_pause" => {
            transport(api::kodi_play_pause());
            true
        }
        "stop" => {
            transport(api::kodi_stop());
            true
        }
        "seek_forward" => {
            transport(api::kodi_seek(30));
            true
        }
        "seek_backward" => {
            transport(api::kodi_seek(-10));
            true
        }
        _ => false,
    }
}

/// Subscribe to the daemon's input bus — the television's own remote.
///
/// The daemon reads CEC and normalises it; this is how those presses reach the
/// screen. Only the kiosk asks for them, with `?tv=1`: the stream reaches every
/// client, and while it carried navigation to all of them a laptop or a phone
/// on the LAN moved in step with whoever was holding the remote in the living
/// room.
///
/// A press can also arrive as an ordinary key event, because the kernel's CEC
/// driver registers an input device of its own. Both paths land on the same
/// rate limit, so a press that comes twice still moves one step.
fn listen_to_remote(nav: Nav) {
    let url = if focus::television() {
        "/v1/events?tv=1"
    } else {
        "/v1/events"
    };
    let Ok(source) = EventSource::new(url) else {
        return;
    };
    let handler = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let Some(text) = event.data().as_string() else {
            return;
        };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        // Only the press edge moves anything; a release would double every step.
        if payload.get("pressed").and_then(|v| v.as_bool()) != Some(true) {
            return;
        }
        if let Some(action) = payload.get("action").and_then(|v| v.as_str()) {
            if matches!(action, "up" | "down" | "left" | "right") && !may_step() {
                return;
            }
            dispatch(action, nav);
        }
    });
    source.set_onmessage(Some(handler.as_ref().unchecked_ref()));
    // The stream lives as long as the page does.
    handler.forget();
}

/// Take the keyboard back whenever the television comes back to this page.
///
/// The display changes hands on this appliance — Kodi or the browser takes DRM
/// master and hands it back — and when it comes back the page is still running
/// but no longer has keyboard focus, so `keydown` never reaches the window and
/// the remote is dead: no focus ring, arrows do nothing, Back does nothing. A
/// reload fixed it, which is what proved it was focus and not a panic. This
/// asks for the keyboard again on every event that marks a return, and puts
/// the remote back on something it can see.
fn install_focus_guard() {
    let reclaim = || {
        if let Some(window) = web_sys::window() {
            let _ = window.focus();
        }
        focus::ensure_focus();
    };
    let _ = window_event_listener(ev::focus, move |_| reclaim());
    let _ = window_event_listener(ev::pageshow, move |_| reclaim());
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        let handler = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || {
            // `hidden` rather than the VisibilityState enum: the feature that
            // carries the enum is not enabled for this build, and the boolean
            // is the same answer.
            if web_sys::window()
                .and_then(|window| window.document())
                .is_some_and(|document| !document.hidden())
            {
                reclaim();
            }
        });
        let _ = document.add_event_listener_with_callback(
            "visibilitychange",
            handler.as_ref().unchecked_ref(),
        );
        handler.forget();
    }
}

/// The shortest gap between two moves the remote is allowed to cause.
///
/// A television remote does not send one event per press. Through CEC the
/// press, the hold and the release each arrive, and a held direction repeats as
/// fast as the bus will carry it — so one deliberate press of Right walked the
/// focus three or four tiles along and the screen looked like it was ignoring
/// the remote rather than outrunning it. Holding a direction still repeats,
/// just at a rate a person can follow.
const STEP_INTERVAL_MS: f64 = 110.0;

thread_local! {
    static LAST_STEP: std::cell::Cell<f64> = const { std::cell::Cell::new(f64::MIN) };
}

/// True when enough time has passed since the last move for this one to count.
fn may_step() -> bool {
    // js_sys rather than web_sys: the Performance binding is behind a feature
    // this build does not enable, and Date.now() is precise enough for a gap
    // measured in tens of milliseconds.
    let now = js_sys::Date::now();
    LAST_STEP.with(|last| {
        if now - last.get() < STEP_INTERVAL_MS {
            return false;
        }
        last.set(now);
        true
    })
}

fn install_keyboard(nav: Nav) {
    let _ = window_event_listener(ev::keydown, move |event| {
        let key = event.key();
        // Directions are rate limited; everything else is one press, one
        // answer, and a second Enter must never be thrown away.
        if matches!(
            key.as_str(),
            "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight"
        ) && !may_step()
        {
            event.prevent_default();
            return;
        }
        let editing = focus::editing();
        let handled = match key.as_str() {
            "ArrowUp" => dispatch("up", nav),
            "ArrowDown" => dispatch("down", nav),
            // In a text field the horizontal arrows belong to the caret.
            "ArrowLeft" if !editing => dispatch("left", nav),
            "ArrowRight" if !editing => dispatch("right", nav),
            "Enter" => {
                if editing {
                    false
                } else {
                    dispatch("ok", nav)
                }
            }
            "Escape" => dispatch("back", nav),
            "Backspace" if !editing => dispatch("back", nav),
            "Home" | "GoHome" | "BrowserHome" => dispatch("home", nav),
            "MediaPlayPause" | "MediaPlay" | "MediaPause" => dispatch("play_pause", nav),
            "MediaStop" => dispatch("stop", nav),
            "MediaTrackNext" | "MediaFastForward" => dispatch("seek_forward", nav),
            "MediaTrackPrevious" | "MediaRewind" => dispatch("seek_backward", nav),
            _ => false,
        };
        if handled {
            event.prevent_default();
        }
    });
}

thread_local! {
    /// At most one autofocus retry runs; a new screen cancels the old one's.
    static AUTOFOCUS_TIMER: std::cell::RefCell<Option<IntervalHandle>> =
        const { std::cell::RefCell::new(None) };
}

// -------------------------------------------------------------------- shell

#[component]
pub fn App() -> impl IntoView {
    let nav = Nav {
        route: RwSignal::new(Route::Home),
        stack: RwSignal::new(Vec::new()),
        restore: RwSignal::new(None),
        pulse: RwSignal::new(0),
        seed: RwSignal::new(None),
    };
    let toaster = Toaster(RwSignal::new(None));
    let ambient = Ambient(RwSignal::new(None));
    provide_context(nav);
    provide_context(toaster);
    provide_context(ambient);

    install_keyboard(nav);
    install_focus_guard();
    listen_to_remote(nav);

    // Focus must survive every screen change, and a screen that has just
    // rendered has no focused element until this runs. Coming back from a
    // title is the case that matters most: the poster it was opened from does
    // not exist yet at the moment the screen reappears, because its rail is
    // still being rebuilt, so the claim is retried briefly — and abandoned the
    // moment the person presses anything, because moving focus out from under
    // someone is worse than starting in the wrong place.
    Effect::new(move |_| {
        let _ = nav.route.get();
        let since = nav.pulse.get_untracked();
        let wanted = nav.restore.get_untracked();
        focus::reset_motion();

        // "Landed" has to mean the element we were actually asked for. Coming
        // back from a title, the poster it was opened from does not exist for
        // the first few hundred milliseconds — its rail is still being
        // fetched — and the hero's button does. Treating that button as a
        // satisfactory landing is what used to cancel the retry and drop the
        // remote at the top of the page on every Back.
        let settle = {
            let wanted = wanted.clone();
            move || match wanted.as_deref() {
                Some(key) => {
                    if focus::focus_key(key) {
                        return true;
                    }
                    // Somewhere sane meanwhile, but keep looking.
                    let _ = focus::claim_autofocus() || focus::settle_into_screen();
                    false
                }
                None => focus::claim_autofocus() || focus::settle_into_screen(),
            }
        };

        {
            let settle = settle.clone();
            request_animation_frame(move || {
                focus::ensure_focus();
                if settle() {
                    nav.restore.set(None);
                }
            });
        }

        let attempts = std::cell::Cell::new(0u8);
        if let Ok(handle) = set_interval_with_handle(
            move || {
                attempts.set(attempts.get() + 1);
                let moved = nav.pulse.get_untracked() != since;
                let landed = settle();
                if landed {
                    nav.restore.set(None);
                }
                // Measure while the screen is still settling. The table only
                // rebuilds when what is on the screen has changed, so this is
                // a cache hit on most ticks and one rebuild on the tick after
                // each shelf arrives — paid here, in the seconds before anyone
                // touches the remote, instead of on the first press.
                focus::warm();
                // Long enough to outlast the slowest thing a screen waits for.
                // A title's sources are fetched from every installed addon and
                // on this appliance that can take several seconds; the screen
                // has no focusable element until they land, so giving up at
                // three seconds left the remote parked on the navigation bar
                // with the title's own buttons sitting unreachable underneath.
                if moved || landed || attempts.get() > 90 {
                    AUTOFOCUS_TIMER.with(|slot| {
                        if let Some(handle) = slot.take() {
                            handle.clear();
                        }
                    });
                }
            },
            std::time::Duration::from_millis(120),
        ) {
            AUTOFOCUS_TIMER.with(|slot| {
                if let Some(previous) = slot.replace(Some(handle)) {
                    previous.clear();
                }
            });
        }
    });

    // A dropped node — an overlay closing, a list reloading — must not leave
    // the remote with nothing selected.
    let _ = window_event_listener(ev::focusout, move |_| {
        request_animation_frame(focus::ensure_focus);
    });

    let toast = toaster.0;
    Effect::new(move |_| {
        if toast.get().is_some() {
            set_timeout(
                move || toast.set(None),
                std::time::Duration::from_millis(5200),
            );
        }
    });

    view! {
        <div class="app">
            <TopBar />
            // A fixed window with the screen's own column inside it. Nothing
            // here scrolls: the column is moved by a transform, which the
            // compositor animates without repainting a single poster.
            <main class="viewport">
                <div class="column" data-scroller="1">
                    {move || match nav.route() {
                        Route::Home => screens::home::Home().into_any(),
                        Route::Media => screens::media::Media().into_any(),
                        Route::Browser => screens::browser::Browser().into_any(),
                        Route::Search => screens::search::Search().into_any(),
                        Route::Detail { kind, id } => screens::detail::Detail(
                                screens::detail::DetailProps { kind, id },
                            )
                            .into_any(),
                        Route::Collection { addon, kind, catalog, title } => {
                            screens::collection::Collection(
                                    screens::collection::CollectionProps {
                                        addon,
                                        kind,
                                        catalog,
                                        title,
                                    },
                                )
                                .into_any()
                        }
                        Route::NowPlaying => screens::now_playing::NowPlaying().into_any(),
                        Route::Settings => screens::settings::Settings().into_any(),
                    }}
                </div>
            </main>
            {move || {
                toast
                    .get()
                    .map(|(message, bad)| {
                        view! {
                            <div class=if bad { "toast bad" } else { "toast" }>{message}</div>
                        }
                    })
            }}
        </div>
    }
}

/// The four places the product goes, across the top of the screen.
///
/// A bar rather than a rail down the side: the side rail floated over whatever
/// was underneath it, so its labels landed on the hero's button on one screen
/// and through the settings list on another. The bar owns a row nothing else
/// occupies — the screen starts below it — and each entry carries its word
/// beside its icon, which is what makes navigation readable from a sofa
/// without having to learn the icons first.
#[component]
fn TopBar() -> impl IntoView {
    let nav = expect_context::<Nav>();
    let items = [
        ("home", "Ana Sayfa", Route::Home, Icon::Home),
        ("search", "Ara", Route::Search, Icon::Search),
        ("now", "Şimdi Oynatılan", Route::NowPlaying, Icon::Play),
        ("settings", "Ayarlar", Route::Settings, Icon::Gear),
    ];
    view! {
        <nav class="topbar">
            <div class="brand">
                <span class="brand-dot"></span>
                <span>"MediaBox"</span>
            </div>
            <div class="nav-items">
                {items
                    .into_iter()
                    .map(|(key, label, target, icon)| {
                        let current = move || nav.route().tab() == key;
                        view! {
                            <button
                                class="nav-item"
                                data-focus="1"
                                data-focus-key=format!("nav:{key}")
                                tabindex="-1"
                                aria-current=move || current().then_some("true")
                                on:click=move |_| nav.go(target.clone())
                            >
                                <Glyph icon=icon />
                                <span>{label}</span>
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
            <Clock />
        </nav>
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Home,
    Search,
    Play,
    Gear,
}

/// Line art rather than glyphs from a font: an appliance cannot depend on a
/// particular icon font being installed, and a stroked path stays crisp at
/// whatever size the root font lands on.
#[component]
fn Glyph(icon: Icon) -> impl IntoView {
    let path = match icon {
        Icon::Home => "M4 11.2 12 4.5l8 6.7M6.4 9.6V19h11.2V9.6",
        Icon::Search => "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14ZM16.2 16.2 21 21",
        Icon::Play => "M8 5.5v13l11-6.5-11-6.5Z",
        Icon::Gear => "M12 9.4a2.6 2.6 0 1 0 0 5.2 2.6 2.6 0 0 0 0-5.2ZM12 3.4l1.4 2.2 2.6-.5.6 2.6 2.2 1.3L19.6 12l1.2 2.4-2.2 1.3-.6 2.6-2.6-.5-1.4 2.2-1.4-2.2-2.6.5-.6-2.6-2.2-1.3L8.4 12 7.2 9.6l2.2-1.3.6-2.6 2.6.5L12 3.4Z",
    };
    view! {
        <svg class="glyph" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d=path />
        </svg>
    }
}

#[component]
fn Clock() -> impl IntoView {
    let now = RwSignal::new(clock_text());
    set_interval(
        move || now.set(clock_text()),
        std::time::Duration::from_secs(20),
    );
    view! { <span class="clock">{move || now.get()}</span> }
}

fn clock_text() -> String {
    let date = js_sys::Date::new_0();
    format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
}
