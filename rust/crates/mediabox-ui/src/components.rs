//! The pieces every screen is built from.

use crate::focus::FOCUS_ATTR;
use crate::model::MetaPreview;
use leptos::prelude::*;

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

#[component]
pub fn Chip(#[prop(into)] text: String, #[prop(into, optional)] tone: String) -> impl IntoView {
    view! { <span class=format!("chip {tone}")>{text}</span> }
}

/// One poster.
#[component]
pub fn Card(item: MetaPreview, #[prop(into)] on_pick: Callback<MetaPreview>) -> impl IntoView {
    let art_style = item
        .poster
        .as_ref()
        .map(|url| format!("background-image:url('{url}')"))
        .unwrap_or_default();
    let has_art = item.poster.is_some();
    let local = item.is_library();
    let name = item.name.clone();
    let fallback = item.name.clone();
    let sub = item
        .release_info
        .clone()
        .or_else(|| item.genres.first().cloned())
        .unwrap_or_default();
    let picked = item.clone();
    let label = format!("{} aç", item.name);

    view! {
        <button
            class="card"
            data-focus="1"
            tabindex="-1"
            aria-label=label
            on:click=move |_| on_pick.run(picked.clone())
        >
            <div class="card-art" style=art_style>
                {(!has_art).then(|| view! { <span class="fallback">{fallback}</span> })}
                {local.then(|| view! { <span class="badge-local">"YEREL"</span> })}
            </div>
            <span class="card-name">{name}</span>
            <span class="card-sub">{sub}</span>
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
) -> impl IntoView {
    if items.is_empty() {
        return None;
    }
    Some(view! {
        <section class="rail">
            <div class="rail-head">
                <h2 class="rail-title">{title}</h2>
                {(!source.is_empty()).then(|| view! { <span class="rail-source">{source}</span> })}
            </div>
            <div class="rail-track">
                {items
                    .into_iter()
                    .map(|item| view! { <Card item=item on_pick=on_pick /> })
                    .collect_view()}
            </div>
        </section>
    })
}

#[component]
pub fn RailSkeleton() -> impl IntoView {
    view! {
        <section class="rail">
            <div class="rail-head">
                <h2 class="rail-title">"Katalog yükleniyor…"</h2>
            </div>
            <div class="skeleton-rail">
                {(0..7).map(|_| view! { <div class="skeleton-card"></div> }).collect_view()}
            </div>
        </section>
    }
}

#[component]
pub fn Failure(#[prop(into)] title: String, #[prop(into)] detail: String) -> impl IntoView {
    view! {
        <div class="state bad">
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

/// The attribute name, re-exported so screens do not hard-code the string.
pub const FOCUS: &str = FOCUS_ATTR;
