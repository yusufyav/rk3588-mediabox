//! The pieces every screen is built from.

use crate::focus::{FOCUS_ATTR, KEY_ATTR, ROW_ATTR, TRACK_ATTR};
use crate::model::{LibraryIds, MetaPreview, WatchState};
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// Where an asynchronous read has got to.
///
/// The UI never renders an empty shell as if it were an answer: a screen is
/// either waiting, showing real data, or saying plainly what went wrong.
#[derive(Clone, Debug, PartialEq)]
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }
}

pub fn seconds_to_clock(total: u64) -> String {
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub fn human_size(bytes: f64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn human_bitrate(bits_per_second: f64) -> String {
    if bits_per_second >= 1_000_000.0 {
        format!("{:.1} Mb/s", bits_per_second / 1_000_000.0)
    } else {
        format!("{:.0} kb/s", bits_per_second / 1000.0)
    }
}

/// A resolution label people actually use, rather than raw pixels.
pub fn resolution_label(width: u32, height: u32) -> String {
    let name = match (width, height) {
        (w, _) if w >= 3800 => Some("4K"),
        (_, h) if h >= 1000 => Some("1080p"),
        (_, h) if h >= 700 => Some("720p"),
        (_, h) if h >= 540 => Some("576p"),
        _ => None,
    };
    match name {
        Some(name) => format!("{name} · {width}×{height}"),
        None => format!("{width}×{height}"),
    }
}

/// The facts worth saying about a title, in the order a person reads them.
///
/// Joined with a separator rather than boxed into chips: a row of bordered
/// pills is a dashboard's way of showing a year, and it competes with the
/// artwork it is sitting on.
pub fn meta_line(item: &MetaPreview) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(year) = item.release_info.as_ref().filter(|y| !y.is_empty()) {
        parts.push(year.clone());
    }
    if let Some(rating) = item.imdb_rating.as_ref().filter(|r| !r.is_empty()) {
        parts.push(format!("IMDb {rating}"));
    }
    parts.extend(item.genres.iter().take(2).cloned());
    parts.join("  ·  ")
}

#[component]
pub fn Chip(#[prop(into)] text: String, #[prop(into, optional)] tone: String) -> impl IntoView {
    view! { <span class=format!("chip {tone}")>{text}</span> }
}

/// One poster.
///
/// The artwork is an `<img>` rather than a CSS background for a reason that is
/// visible on the television: a background image is fetched the moment the
/// element exists, so a home screen of six rails asked for a hundred pictures
/// at once and the browser — six connections to a host — spent its first
/// seconds downloading posters nobody could see while the backdrop behind the
/// hero, the one picture that fills the screen, waited its turn and was painted
/// half-finished. `loading="lazy"` lets the browser fetch what is on screen
/// first, and `decoding="async"` keeps the decode off the frame that is being
/// drawn.
#[component]
pub fn Card(
    item: MetaPreview,
    #[prop(into)] on_pick: Callback<MetaPreview>,
    /// True for the posters that are on screen the moment the shelf appears.
    /// Those are fetched straight away; everything past the edge of the picture
    /// stays lazy, so a home screen still does not ask for a hundred pictures
    /// at once.
    #[prop(optional)]
    eager: bool,
) -> impl IntoView {
    let poster = item.poster.clone();
    let has_art = poster.is_some();
    let local = item.is_library();
    let name = item.name.clone();
    let fallback = item.name.clone();
    let alt = item.name.clone();
    let key = format!("card:{}:{}", item.kind, item.id);
    // A stable colour per title, so a shelf of artless entries reads as a set
    // of covers rather than as a row of failures. Any spread over the circle
    // will do; this one just has to give the same answer every time.
    let hue = item.name.bytes().fold(17u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as u32)
    }) % 360;
    let picked = item.clone();
    let label = format!("{} aç", item.name);
    // How far in this title was left, when the account knows. Zero is not a
    // position: a title never started should look untouched, not like one
    // abandoned in its first second.
    let progress = item
        .state
        .as_ref()
        .map(WatchState::progress)
        .filter(|fraction| *fraction > 0.0);
    // Whether this title is already in the operator's library. Worth saying on
    // a catalogue shelf, where the same film appears again among strangers;
    // on the library's own shelf it would be saying the obvious, so the rails
    // built from the library do not provide the set.
    let owned = use_context::<LibraryIds>();
    let item_id = item.id.clone();

    view! {
        <button
            class="card"
            data-focus="1"
            data-focus-key=key
            tabindex="-1"
            aria-label=label
            on:click=move |_| on_pick.run(picked.clone())
        >
            <span class="card-art" style=format!("--tile-hue:{hue}")>
                // The title is always present underneath the artwork, not just
                // when a poster is missing. A catalogue is third-party data and
                // some of its poster URLs simply 404; without something behind
                // the picture the shelf shows a browser's broken-image icon and
                // the alt text, which is the most obviously unfinished thing a
                // television can display. If the fetch fails the image takes
                // itself out of the way and the title is what was already there.
                <span class="card-fallback">{fallback}</span>
                {poster
                    .map(|url| {
                        view! {
                            <img
                                class="card-img"
                                src=url
                                alt=alt
                                loading=if eager { "eager" } else { "lazy" }
                                decoding="async"
                                draggable="false"
                                on:error=|event| {
                                    if let Some(image) = event
                                        .target()
                                        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                                    {
                                        let _ = image.class_list().add_1("is-broken");
                                    }
                                }
                            />
                        }
                    })}
                {local.then(|| view! { <span class="badge-local">"YEREL"</span> })}
                {owned
                    .map(|ids| {
                        let id = item_id.clone();
                        view! {
                            <Show when=move || ids.has(&id)>
                                <span class="badge-owned" aria-label="Kitaplığında">
                                    <svg viewBox="0 0 24 24" aria-hidden="true">
                                        <path d="M5 13l4 4L19 7" />
                                    </svg>
                                </span>
                            </Show>
                        }
                    })}
                {progress
                    .map(|fraction| {
                        view! {
                            <span class="card-progress" aria-hidden="true">
                                <i style=format!("width:{:.1}%", fraction * 100.0)></i>
                            </span>
                        }
                    })}
            </span>
            // Out of flow on purpose: the name appears under the poster the
            // remote is on and nowhere else, so a shelf reads as artwork rather
            // than as a table of contents, and showing it moves nothing.
            //
            // Not under a tile that has no artwork, though: that tile already
            // has the title printed across it, and saying it twice is the sort
            // of thing that makes an interface look unfinished.
            {has_art.then(|| view! { <span class="card-name">{name}</span> })}
        </button>
    }
}

/// One horizontal row of posters.
#[component]
pub fn Rail(
    #[prop(into)] title: String,
    #[prop(into, optional)] source: String,
    items: Vec<MetaPreview>,
    #[prop(into)] on_pick: Callback<MetaPreview>,
    /// Where this shelf opens out into a grid, when it has somewhere to go.
    ///
    /// A shelf is a window onto a longer list, and walking a hundred titles
    /// sideways with an arrow key is not browsing. The button lives in the
    /// head, at the end of the line the title starts, which is where the eye
    /// already goes when a row runs off the screen.
    #[prop(optional)]
    on_all: Option<Callback<()>>,
) -> impl IntoView {
    if items.is_empty() {
        return None;
    }
    Some(view! {
        <section class="rail" data-row="1">
            <div class="rail-head">
                <h2 class="rail-title">{title}</h2>
                {(!source.is_empty()).then(|| view! { <span class="rail-source">{source}</span> })}
                {on_all
                    .map(|open| {
                        view! {
                            <button
                                class="rail-all"
                                data-focus="1"
                                tabindex="-1"
                                on:click=move |_| open.run(())
                            >
                                "Tümünü gör"
                                <svg viewBox="0 0 24 24" aria-hidden="true">
                                    <path d="M9 6l6 6-6 6" />
                                </svg>
                            </button>
                        }
                    })}
            </div>
            // Two elements, and the split matters. The outer one clips and
            // never moves; the inner one holds the posters and is what the
            // transform is written to. Moving the element that does the
            // clipping drags its window along with it, so the right-hand side
            // of the shelf was being cut off by the very thing that was
            // supposed to be revealing it — the screen went black from
            // wherever the rail had travelled to. The compositor animates the
            // strip on its own thread, so walking a rail repaints nothing.
            <div class="rail-track">
                <div class="rail-strip" data-track="1">
                    {items
                        .into_iter()
                        .enumerate()
                        .map(|(position, item)| {
                            view! { <Card item=item on_pick=on_pick eager=position < 8 /> }
                        })
                        .collect_view()}
                </div>
            </div>
        </section>
    })
}

#[component]
pub fn RailSkeleton() -> impl IntoView {
    view! {
        <section class="rail" data-row="1">
            <div class="rail-head">
                <h2 class="rail-title">"Katalog yükleniyor"</h2>
            </div>
            <div class="rail-track">
                <div class="rail-strip">
                    {(0..7).map(|_| view! { <div class="skeleton-card"></div> }).collect_view()}
                </div>
            </div>
        </section>
    }
}

#[component]
pub fn Failure(#[prop(into)] title: String, #[prop(into)] detail: String) -> impl IntoView {
    view! {
        <div class="state bad" data-row="1">
            <strong>{title}</strong>
            <p>{detail}</p>
        </div>
    }
}

/// A labelled value, the unit settings and diagnostics are made of.
#[component]
pub fn Row(
    #[prop(into)] label: String,
    #[prop(into)] value: String,
    #[prop(into, optional)] tone: String,
    #[prop(optional)] meter: Option<f64>,
) -> impl IntoView {
    view! {
        <div class="row">
            <span class="row-label">{label}</span>
            <span class=format!("row-value {tone}")>{value}</span>
            {meter
                .map(|fraction| {
                    let width = format!("width:{:.1}%", (fraction.clamp(0.0, 1.0)) * 100.0);
                    view! { <span class="meter"><i style=width></i></span> }
                })}
        </div>
    }
}

/// A button the remote can reach.
#[component]
pub fn Action(
    #[prop(into)] label: Signal<String>,
    #[prop(into)] on_press: Callback<()>,
    #[prop(into, optional)] variant: String,
    #[prop(into, optional)] disabled: Signal<bool>,
    #[prop(optional)] autofocus: bool,
) -> impl IntoView {
    view! {
        <button
            class=format!("btn {variant}")
            // A disabled control is not a place the remote may land, so it
            // leaves the focus graph entirely rather than becoming a dead stop.
            data-focus=move || (!disabled.get()).then_some("1")
            data-autofocus=autofocus.then_some("1")
            tabindex="-1"
            disabled=move || disabled.get()
            on:click=move |_| on_press.run(())
        >
            {move || label.get()}
        </button>
    }
}

/// A text field the remote, a keyboard and a phone can all use.
///
/// A real `<input>`, deliberately. The first version of the sign-in and address
/// screens drew the value into a `<button>` and let only the on-screen keys
/// write to it — which meant a USB keyboard plugged into the appliance typed
/// nothing, and paste was impossible anywhere, including from a phone on the
/// LAN where the browser's own keyboard and clipboard are right there. The
/// on-screen keyboard writes into the same signal, so the two are alternatives
/// rather than a choice the screen makes for you.
#[component]
pub fn Field(
    #[prop(into)] label: String,
    value: RwSignal<String>,
    #[prop(into)] key: String,
    /// A password: shown as dots, and never in the clear on a television that
    /// a room full of people is looking at.
    #[prop(optional)]
    secret: bool,
    #[prop(optional)] autofocus: bool,
    #[prop(into, optional)] placeholder: String,
    /// Called when the field has focus and Enter is pressed, so a keyboard can
    /// finish the job without walking to the button.
    #[prop(optional)]
    on_enter: Option<Callback<()>>,
    /// Told when this field takes focus, so an on-screen keyboard beside it
    /// knows which field it is building.
    #[prop(optional)]
    on_focus: Option<Callback<()>>,
) -> impl IntoView {
    view! {
        <div
            class="field"
            data-focus="1"
            data-focus-key=key
            data-autofocus=autofocus.then_some("1")
            tabindex="-1"
            on:focus=move |event| {
                if let Some(callback) = on_focus {
                    callback.run(());
                }
                // The wrapper is what the remote lands on; the caret has to
                // follow it in, or typing would go nowhere.
                if let Some(target) = event
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .and_then(|t| t.query_selector("input").ok().flatten())
                    .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                {
                    let _ = target.focus();
                }
            }
        >
            <span class="field-label">{label}</span>
            <input
                class="field-input"
                type=if secret { "password" } else { "text" }
                placeholder=placeholder
                autocomplete="off"
                autocapitalize="off"
                spellcheck="false"
                prop:value=move || value.get()
                on:input=move |event| value.set(event_target_value(&event))
                on:keydown=move |event| {
                    if event.key() == "Enter"
                        && let Some(callback) = on_enter
                    {
                        event.prevent_default();
                        callback.run(());
                    }
                }
            />
        </div>
    }
}

/// A keyboard for a remote control.
///
/// Not an `<input>` with a caret: a caret on a television is a promise of a
/// keyboard that is not there. Every key is a place the four arrows can land,
/// and what they build is written into the signal the caller owns — which is
/// how the same component serves a web address and an account sign-in without
/// either of them knowing about the other.
#[component]
pub fn Keyboard(value: RwSignal<String>) -> impl IntoView {
    const ROWS: [&str; 4] = ["1234567890", "qwertyuıopğü", "asdfghjklşi", "zxcvbnmöç.@-_"];
    let press = move |key: char| {
        value.update(|text| {
            if text.chars().count() < 200 {
                text.push(key);
            }
        });
    };
    view! {
        {ROWS
            .into_iter()
            .map(|row| {
                view! {
                    <div class="keys" data-row="1">
                        {row
                            .chars()
                            .map(|key| {
                                view! {
                                    <button
                                        class="key"
                                        data-focus="1"
                                        data-focus-key=format!("key:{key}")
                                        tabindex="-1"
                                        on:click=move |_| press(key)
                                    >
                                        {key.to_string()}
                                    </button>
                                }
                            })
                            .collect_view()}
                    </div>
                }
            })
            .collect_view()}
    }
}

/// The attribute names, re-exported so screens do not hard-code the strings.
pub const FOCUS: &str = FOCUS_ATTR;
pub const FOCUS_KEY: &str = KEY_ATTR;
pub const ROW: &str = ROW_ATTR;
pub const TRACK: &str = TRACK_ATTR;
