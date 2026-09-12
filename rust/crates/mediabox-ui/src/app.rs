//! The shell: which screen is showing, and how a remote reaches it.

use crate::focus::{self, Direction};
use crate::screens;
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
    Search,
    Detail { kind: String, id: String },
    NowPlaying,
    Settings,
}

impl Route {
    fn tab(&self) -> &'static str {
        match self {
            Self::Home | Self::Detail { .. } => "home",
            Self::Search => "search",
            Self::NowPlaying => "now",
            Self::Settings => "settings",
        }
    }
}

/// Navigation, with the back stack a remote's Back button needs.
#[derive(Clone, Copy)]
pub struct Nav {
    route: RwSignal<Route>,
    stack: RwSignal<Vec<Route>>,
    /// Bumped by every accepted input. Anything waiting for "the person has
    /// not acted yet" watches this rather than guessing from a timer.
    pulse: RwSignal<u64>,
}

impl Nav {
    pub fn route(&self) -> Route {
        self.route.get()
    }

    pub fn go(&self, next: Route) {
        let current = self.route.get_untracked();
        if current == next {
            return;
        }
        self.stack.update(|stack| {
            stack.push(current);
            // Deep enough for any real journey, bounded so a long session
            // cannot grow it without limit.
            if stack.len() > 32 {
                stack.remove(0);
            }
        });
        self.route.set(next);
    }

    /// Replace the current screen without adding a step to go back through.
    pub fn swap(&self, next: Route) {
        self.route.set(next);
    }

    pub fn back(&self) {
        let previous = self.stack.try_update(|stack| stack.pop()).flatten();
        self.route.set(previous.unwrap_or(Route::Home));
    }

    pub fn home(&self) {
        self.stack.set(Vec::new());
        self.route.set(Route::Home);
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

/// Subscribe to the daemon's input bus.
///
/// The CEC remote is read by the daemon, not by the browser, so without this
/// the television's own remote would move nothing on screen. Keyboards are not
/// grabbed anywhere, so they arrive as ordinary key events and need no relay.
fn listen_to_remote(nav: Nav) {
    let Ok(source) = EventSource::new("/v1/events") else {
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
            dispatch(action, nav);
        }
    });
    source.set_onmessage(Some(handler.as_ref().unchecked_ref()));
    // The stream lives as long as the page does.
    handler.forget();
}

fn install_keyboard(nav: Nav) {
    let _ = window_event_listener(ev::keydown, move |event| {
        let key = event.key();
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
        pulse: RwSignal::new(0),
    };
    let toaster = Toaster(RwSignal::new(None));
    provide_context(nav);
    provide_context(toaster);

    install_keyboard(nav);
    listen_to_remote(nav);

    // Focus must survive every screen change, and a screen that has just
    // rendered has no focused element until this runs. The screen's preferred
    // element usually arrives with its data a moment later, so the claim is
    // retried briefly — and abandoned the moment the person presses anything,
    // because moving focus out from under someone is worse than starting in
    // the wrong place.
    Effect::new(move |_| {
        let _ = nav.route.get();
        let since = nav.pulse.get_untracked();
        request_animation_frame(move || {
            focus::focus_first();
            let _ = focus::claim_autofocus();
        });
        let attempts = std::cell::Cell::new(0u8);
        if let Ok(handle) = set_interval_with_handle(
            move || {
                attempts.set(attempts.get() + 1);
                let moved = nav.pulse.get_untracked() != since;
                if moved || attempts.get() > 24 || focus::claim_autofocus() {
                    AUTOFOCUS_TIMER.with(|slot| {
                        if let Some(handle) = slot.take() {
                            handle.clear();
                        }
                    });
                }
            },
            std::time::Duration::from_millis(250),
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
            <main class="screen">
                {move || match nav.route() {
                    Route::Home => screens::home::Home().into_any(),
                    Route::Search => screens::search::Search().into_any(),
                    Route::Detail { kind, id } => screens::detail::Detail(
                            screens::detail::DetailProps { kind, id },
                        )
                        .into_any(),
                    Route::NowPlaying => screens::now_playing::NowPlaying().into_any(),
                    Route::Settings => screens::settings::Settings().into_any(),
                }}
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

#[component]
fn TopBar() -> impl IntoView {
    let nav = expect_context::<Nav>();
    let tabs = [
        ("home", "Ana Sayfa", Route::Home),
        ("search", "Ara", Route::Search),
        ("now", "Şimdi Oynatılan", Route::NowPlaying),
        ("settings", "Ayarlar", Route::Settings),
    ];
    view! {
        <header class="topbar">
            <div class="wordmark">"MEDIABOX"</div>
            <nav class="nav">
                {tabs
                    .into_iter()
                    .map(|(key, label, target)| {
                        let current = move || nav.route().tab() == key;
                        view! {
                            <button
                                class="nav-item"
                                data-focus="1"
                                tabindex="-1"
                                aria-current=move || current().then_some("true")
                                on:click=move |_| nav.go(target.clone())
                            >
                                {label}
                            </button>
                        }
                    })
                    .collect_view()}
            </nav>
            <Clock />
        </header>
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
