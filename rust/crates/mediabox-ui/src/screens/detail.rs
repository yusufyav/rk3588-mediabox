//! One title: what it is, how it can be played, and what that will cost.
//!
//! The source list is the heart of the product. Every technical claim on this
//! screen comes from the media core's own inspection and policy output — the
//! probe that opened the file and the decision made against this board's
//! accepted capability profile. Nothing is inferred from a release name: a
//! source called "2160p HDR DV" that probes as 8-bit SDR is drawn as 8-bit SDR.
//!
//! Where the appliance cannot do something, it says so rather than degrading
//! quietly. A Dolby Vision source is never presented as safe HDR10, a torrent
//! whose pieces never arrive is shown as unreachable rather than played into
//! silence, and preview is offered only when the browser can genuinely open
//! the source.

use crate::app::{Nav, Recent, Route, Toaster, remember};
use std::collections::HashSet;
use wasm_bindgen::JsCast;
use crate::components::{
    Action, Chip, Failure, Load, human_bitrate, human_size, resolution_label, seconds_to_clock,
};
use crate::model::{
    LibraryItemEnvelope, Meta, MetaEnvelope, Plan, Reason, Stream, StreamListing, VideoTrack,
};
use crate::{api, LIBRARY_ADDON_ID};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

/// A stream as the UI holds it: the parsed view for drawing, and the exact
/// descriptor the media core handed over, which is what must be sent back.
#[derive(Clone, Debug, PartialEq)]
struct Source {
    parsed: Stream,
    raw: Value,
}

#[component]
pub fn Detail(kind: String, id: String) -> impl IntoView {
    let nav = expect_context::<Nav>();
    let toaster = expect_context::<Toaster>();

    // Drawn on the first frame from what the shelf already knew, so the page
    // is never a black screen waiting on a request. The full record replaces
    // it when it arrives — same title, same artwork, more of it.
    let meta = RwSignal::new(match nav.seeded(&id) {
        Some(item) => Load::Ready(item.as_meta()),
        None => Load::Loading,
    });
    let sources = RwSignal::new(Load::<Vec<Source>>::Loading);
    let selected = RwSignal::new(None::<Source>);
    let plan = RwSignal::new(None::<Load<Plan>>);
    let preview_url = RwSignal::new(None::<Preview>);
    // Which addon's sources are shown. Held here rather than inside the list,
    // because the control that changes it stands outside the part that scrolls.
    let tab = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let is_library = kind == "library" || id.starts_with("library:");
    let load_kind = kind.clone();
    let load_id = id.clone();

    spawn_local(async move {
        // A library title is answered in one call, meta and sources together;
        // an addon title needs the two separate reads.
        if is_library {
            match api::typed::<LibraryItemEnvelope>(api::media_library_item(&load_id)).await {
                Ok(found) => {
                    meta.set(Load::Ready(found.meta));
                    sources.set(Load::Ready(to_sources(found.streams)));
                }
                Err(error) => {
                    meta.set(Load::Failed(error.message.clone()));
                    sources.set(Load::Failed(error.message));
                }
            }
            return;
        }
        match api::typed::<MetaEnvelope>(api::media_meta(&load_kind, &load_id)).await {
            Ok(found) => meta.set(Load::Ready(found.meta)),
            Err(error) => meta.set(Load::Failed(error.message)),
        }
        match api::control(api::media_streams(&load_kind, &load_id)).await {
            Ok(value) => {
                let listing: StreamListing =
                    serde_json::from_value(value.clone()).unwrap_or(StreamListing {
                        streams: Vec::new(),
                        playable: 0,
                    });
                let raw = value
                    .get("streams")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let found: Vec<Source> = listing
                    .streams
                    .into_iter()
                    .zip(raw)
                    .map(|(parsed, raw)| Source { parsed, raw })
                    .collect();
                sources.set(Load::Ready(found));
            }
            Err(error) => sources.set(Load::Failed(error.message)),
        }
    });

    let analyze = move |source: Source| {
        selected.set(Some(source.clone()));
        if !source.parsed.playable {
            plan.set(None);
            return;
        }
        plan.set(Some(Load::Loading));
        spawn_local(async move {
            let request = match source.parsed.url.as_deref() {
                Some(url) => api::media_policy(url),
                None => api::media_stream_plan(&source.raw),
            };
            match api::typed::<Plan>(request).await {
                Ok(found) => plan.set(Some(Load::Ready(found))),
                Err(error) => plan.set(Some(Load::Failed(error.message))),
            }
        });
    };

    // The first source that can be played is the one worth analysing without
    // being asked; everything else stays a deliberate choice.
    Effect::new(move |_| {
        if selected.get_untracked().is_some() {
            return;
        }
        if let Load::Ready(found) = sources.get()
            && let Some(first) = found.iter().find(|source| source.parsed.playable).cloned()
        {
            analyze(first);
        }
    });

    let remember_played = move || {
        if let Load::Ready(found) = meta.get_untracked() {
            remember(Recent {
                id: found.id.clone(),
                kind: found.kind.clone(),
                name: found.name.clone(),
                poster: found.poster.clone(),
                background: found.background.clone(),
                release_info: found.release_info.clone(),
            });
        }
    };

    // Playing is what this screen is for, and it happens here: the film opens
    // in the interface's own player, in front of the page it was chosen from.
    // Handing it to Kodi is a separate, deliberate act — the equivalent of
    // "send to an external player" — not the only way to watch something.
    let play_source = move |source: Source| {
        busy.set(true);
        toaster.say("Açılıyor…");
        spawn_local(async move {
            let request = match source.parsed.url.as_deref() {
                Some(url) => api::play_here(Some(url), None, 0),
                None => api::play_here(None, Some(&source.raw), 0),
            };
            match api::control(request).await {
                Ok(_) => {
                    remember_played();
                    nav.go(Route::NowPlaying);
                }
                Err(error) => toaster.warn(format!("Oynatılamadı — {}", error.message)),
            }
            busy.set(false);
        });
    };

    // Choosing a source is choosing to watch it.
    //
    // It used to only select: the remote then had to travel back up the page
    // to a Play button and press again, for a decision already made. Pressing
    // OK on a source now starts it, which is what pressing OK on a source
    // means everywhere else.
    let pick = Callback::new(move |source: Source| {
        analyze(source.clone());
        play_source(source);
    });

    let play = Callback::new(move |()| match selected.get_untracked() {
        Some(source) => play_source(source),
        None => toaster.warn("Önce bir kaynak seçin."),
    });

    let play_on_kodi = Callback::new(move |()| {
        let Some(source) = selected.get_untracked() else {
            toaster.warn("Önce bir kaynak seçin.");
            return;
        };
        busy.set(true);
        toaster.say("Kodi'ye aktarılıyor…");
        spawn_local(async move {
            let request = match source.parsed.url.as_deref() {
                Some(url) => api::play_on_kodi(Some(url), None, 0),
                None => api::play_on_kodi(None, Some(&source.raw), 0),
            };
            match api::control(request).await {
                Ok(_) => {
                    remember_played();
                    toaster.say("Kodi'de oynatılıyor.");
                    nav.go(Route::NowPlaying);
                }
                Err(error) => toaster.warn(format!("Aktarılamadı — {}", error.message)),
            }
            busy.set(false);
        });
    });

    let preview = Callback::new(move |()| {
        // The trailer first: it belongs to the title rather than to whichever
        // source happens to be selected, so it works before a source is even
        // chosen and on every title that has one.
        if let Load::Ready(found) = meta.get_untracked()
            && let Some(id) = found.trailer.clone()
        {
            preview_url.set(Some(Preview::Trailer(id)));
            return;
        }
        let previewable = selected
            .get_untracked()
            .zip(match plan.get_untracked() {
                Some(Load::Ready(found)) => Some(found),
                _ => None,
            })
            .and_then(|(source, found)| previewable_url(&found, &source));
        match previewable {
            Some(url) => {
                remember_played();
                preview_url.set(Some(Preview::Source(url)));
            }
            None => toaster.warn("Bu başlığın fragmanı yok ve kaynağı tarayıcıda açılamıyor."),
        }
    });

    view! {
        <div class="detail detail--split">
            <div class="detail-main">
            {move || match meta.get() {
                Load::Loading => {
                    view! { <div class="state"><strong>"Yükleniyor…"</strong></div> }.into_any()
                }
                Load::Failed(detail) => {
                    view! { <Failure title="Başlık açılamadı" detail=detail /> }.into_any()
                }
                Load::Ready(found) => {
                    view! {
                        <Head
                            meta=found
                            plan=plan
                            selected=selected
                            busy=busy
                            on_play=play
                            on_play_on_kodi=play_on_kodi
                            on_preview=preview
                        />
                    }
                        .into_any()
                }
            }}
            </div>
            // The sources, beside the title rather than under it.
            //
            // Twenty-six of them stacked full-width is a wall: each row is a
            // sentence the width of the television, and the differences between
            // them — the resolution, the size, who is seeding — sit at the end
            // of it where nothing lines up. In a column of their own the rows
            // are short, the same field is always in the same place, and a
            // dozen are on screen instead of four.
            <aside class="detail-side">
                // A scroller of its own, which is what keeps the picture still.
                //
                // With one scroller for the whole screen, moving down the
                // sources drags the poster, the synopsis and the backdrop up
                // and off the television: by the tenth source the title being
                // chosen for is no longer on screen. The panel scrolls inside
                // itself instead, so what stays is everything the choice is
                // about. The focus engine picks the nearest scroller to
                // whatever the remote is on, so declaring one here is all it
                // takes.
                // The filter stands still while the list moves under it.
                // Scrolled away with the rows it would be unreachable from the
                // tenth source onwards, which is exactly where wanting to
                // narrow the list begins.
                <SourceFilter sources=sources tab=tab />
                // The window the list is seen through, and it starts *below*
                // the filter. Clipping at the panel's own edge instead let the
                // list travel up over the filter and bury it, and it also made
                // the focus engine measure the travel against a window taller
                // than the one the list is actually in.
                <div class="detail-side-window">
                <div class="detail-side-inner" data-scroller="1">
                    <SourceList
                        sources=sources
                        tab=tab
                        selected=selected
                        on_pick=pick
                    />
                    {move || {
                        plan.get()
                            .map(|state| view! { <Technical state=state /> })
                    }}
                </div>
                </div>
            </aside>
            {move || {
                preview_url
                    .get()
                    .map(|url| {
                        view! {
                            <PreviewSheet
                                preview=url
                                on_close=Callback::new(move |()| preview_url.set(None))
                            />
                        }
                    })
            }}
        </div>
    }
}

fn to_sources(streams: Vec<Stream>) -> Vec<Source> {
    streams
        .into_iter()
        .map(|parsed| {
            let raw = serde_json::to_value(&parsed.url).unwrap_or(Value::Null);
            Source {
                raw: serde_json::json!({"url": raw, "addonId": LIBRARY_ADDON_ID}),
                parsed,
            }
        })
        .collect()
}

/// Whether the browser can genuinely open this source, and at what URL.
///
/// Only `BrowserDirect` qualifies, and only when the resolved URL is one a
/// browser can actually fetch. A loopback URL is reachable from the television
/// but not from a phone, and a preview that plays on one device and silently
/// fails on another is worse than an honest refusal.
fn previewable_url(plan: &Plan, source: &Source) -> Option<String> {
    if plan.preview.mode != "BrowserDirect" {
        return None;
    }
    let url = source
        .parsed
        .url
        .clone()
        .or_else(|| plan.media.source.clone())?;
    let reachable = url.starts_with("http://") || url.starts_with("https://");
    let loopback = url.contains("//127.0.0.1") || url.contains("//localhost");
    (reachable && !loopback).then_some(url)
}

/// Who offered this source, as a person would name them.
///
/// The addon's own name when it sent one. Failing that the id, which is a
/// reverse-domain string and ugly, but is at least stable and distinct — and a
/// tab labelled with it is still better than every source in one heap.
fn provider_name(stream: &Stream) -> String {
    stream
        .addon_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| stream.addon_id.clone())
        .unwrap_or_else(|| "Bilinmeyen".to_string())
}

/// The provider filter, above the part of the panel that scrolls.
///
/// A control that shows the choice it is on and opens into the rest of them,
/// rather than a row of every option at once. With three addons installed the
/// row was already wider than the panel; with a dozen it is a strip that has
/// to be walked through to reach the sources under it. Closed, this is one
/// line; open, it is a list with the current choice marked.
#[component]
fn SourceFilter(
    sources: RwSignal<Load<Vec<Source>>>,
    tab: RwSignal<Option<String>>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    // Addons in the order they first answered, with how many each offered.
    // Ordered by arrival rather than by count, so the list does not reshuffle
    // itself between one title and the next.
    let providers = move || {
        sources.with(|state| {
            let mut order: Vec<(String, usize)> = Vec::new();
            for source in state.ready().map(Vec::as_slice).unwrap_or_default() {
                let name = provider_name(&source.parsed);
                match order.iter_mut().find(|(seen, _)| seen == &name) {
                    Some((_, count)) => *count += 1,
                    None => order.push((name, 1)),
                }
            }
            order
        })
    };
    let total = move || providers().iter().map(|(_, count)| count).sum::<usize>();
    let current = move || match tab.get() {
        Some(name) => {
            let count = providers()
                .into_iter()
                .find(|(seen, _)| seen == &name)
                .map(|(_, count)| count)
                .unwrap_or(0);
            (name, count)
        }
        None => ("Tümü".to_string(), total()),
    };

    view! {
        {move || {
            // One provider is not a choice, and a control offering it is
            // furniture.
            (providers().len() > 1)
                .then(|| {
                    view! {
                        <div
                            class="source-select"
                            class:is-open=move || open.get()
                            on:keydown=move |event| {
                                // Back closes the list it opened. Without this
                                // the key went past it to the screen, and
                                // changing your mind about the filter left the
                                // film.
                                if open.get_untracked()
                                    && matches!(event.key().as_str(), "Escape" | "Backspace")
                                {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    open.set(false);
                                }
                            }
                        >
                            <button
                                class="source-select-head"
                                data-focus="1"
                                tabindex="-1"
                                aria-expanded=move || open.get().to_string()
                                on:click=move |_| open.update(|state| *state = !*state)
                            >
                                <span class="source-select-name">{move || current().0}</span>
                                <span class="source-select-count">{move || current().1}</span>
                                <svg class="source-select-mark" viewBox="0 0 24 24" aria-hidden="true">
                                    <path d="M7 10l5 5 5-5" />
                                </svg>
                            </button>
                            <Show when=move || open.get()>
                                // A scope, because the open list is drawn over
                                // the sources: without it the arrow keys are
                                // scored on geometry alone, and the row lying
                                // directly under the second option is nearer
                                // than the option itself — one press down and
                                // the remote had left the list it opened.
                                <div class="source-select-list" data-focus-scope="1">
                                    <ProviderOption
                                        tab=tab
                                        open=open
                                        value=None
                                        label="Tümü"
                                        count=Signal::derive(total)
                                    />
                                    {move || {
                                        providers()
                                            .into_iter()
                                            .map(|(name, count)| {
                                                view! {
                                                    <ProviderOption
                                                        tab=tab
                                                        open=open
                                                        value=Some(name.clone())
                                                        label=name
                                                        count=Signal::derive(move || count)
                                                    />
                                                }
                                            })
                                            .collect_view()
                                    }}
                                </div>
                            </Show>
                        </div>
                    }
                })
        }}
    }
}

/// One line of the open filter. The green dot marks the one in force, the way
/// a chosen item is marked anywhere a list stands in for a single answer.
#[component]
fn ProviderOption(
    tab: RwSignal<Option<String>>,
    open: RwSignal<bool>,
    value: Option<String>,
    #[prop(into)] label: String,
    #[prop(into)] count: Signal<usize>,
) -> impl IntoView {
    let chosen = value.clone();
    let mine = value.clone();
    let active = move || tab.get() == mine;
    view! {
        <button
            class="source-option"
            data-focus="1"
            tabindex="-1"
            aria-selected=move || active().to_string()
            on:click=move |_| {
                tab.set(chosen.clone());
                open.set(false);
                // The list the remote was standing in has just gone. Put it
                // back on the control that opened it; left to itself the
                // engine re-homes focus on whatever is nearest, which was the
                // play button in the other column.
                if let Some(head) = web_sys::window()
                    .and_then(|window| window.document())
                    .and_then(|document| {
                        document.query_selector(".source-select-head").ok().flatten()
                    })
                    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    crate::focus::focus(&head);
                }
            }
        >
            <span class="source-option-name">{label}</span>
            <span class="source-option-count">{move || count.get()}</span>
            <span class="source-option-mark" aria-hidden="true"></span>
        </button>
    }
}

#[component]
fn Head(
    meta: Meta,
    plan: RwSignal<Option<Load<Plan>>>,
    selected: RwSignal<Option<Source>>,
    busy: RwSignal<bool>,
    #[prop(into)] on_play: Callback<()>,
    #[prop(into)] on_play_on_kodi: Callback<()>,
    #[prop(into)] on_preview: Callback<()>,
) -> impl IntoView {
    let nav = expect_context::<Nav>();
    let art = meta
        .background
        .clone()
        .or_else(|| meta.poster.clone())
        .map(|url| format!("background-image:url('{url}')"))
        .unwrap_or_default();
    let logo = meta.logo.clone();
    let title_alt = meta.name.clone();
    let title_text = meta.name.clone();
    let mut facts: Vec<String> = Vec::new();
    if let Some(year) = meta.release_info.clone() {
        facts.push(year);
    }
    if let Some(runtime) = meta.runtime.clone() {
        facts.push(runtime);
    }
    if let Some(rating) = meta.imdb_rating.clone() {
        facts.push(format!("IMDb {rating}"));
    }
    let genres = meta.genres.clone();
    let cast: Vec<String> = meta.cast.iter().take(5).cloned().collect();
    let directors = meta.director.clone();
    let description = meta.description.clone();

    let can_play = move || selected.get().is_some_and(|source| source.parsed.playable);
    // A trailer belongs to the title, so the button is live as soon as the page
    // is — no source has to be chosen first.
    let has_trailer = meta.trailer.is_some();
    let preview_ready = move || {
        has_trailer
            || match (plan.get(), selected.get()) {
                (Some(Load::Ready(found)), Some(source)) => {
                    previewable_url(&found, &source).is_some()
                }
                _ => false,
            }
    };

    view! {
        <div class="detail-art" style=art></div>
        <div class="detail-grid">
            <div class="detail-body">
                // The film's own title art when the catalogue has it. It is the
                // title as the film itself writes it, and it is what tells you
                // where you are without reading anything.
                {match logo {
                    Some(url) => {
                        view! { <img class="detail-logo" src=url alt=title_alt /> }.into_any()
                    }
                    None => view! { <h1 class="detail-title">{title_text}</h1> }.into_any(),
                }}
                <p class="detail-facts">
                    {facts
                        .into_iter()
                        .map(|fact| view! { <span>{fact}</span> })
                        .collect_view()}
                </p>
                <MetaGroup label="TÜRÜ" items=genres />
                <MetaGroup label="OYUNCULAR" items=cast />
                <MetaGroup label="YÖNETMENLER" items=directors />
                {description
                    .map(|text| {
                        view! {
                            <div class="meta-group">
                                <span class="meta-label">"ÖZET"</span>
                                <p class="detail-desc">{text}</p>
                            </div>
                        }
                    })}
                // The actions, as a strip of marks rather than a row of pills.
                // Three buttons the width of a sentence each is the loudest
                // thing on the screen, and none of them is what the screen is
                // about: the film is, and the choice of source is.
                <div class="detail-actions" data-row="1">
                    <IconAction
                        label=Signal::derive(move || {
                            if busy.get() { "Açılıyor…".to_string() } else { "Oynat".to_string() }
                        })
                        glyph="M8 5l11 7-11 7z"
                        primary=true
                        autofocus=true
                        disabled=Signal::derive(move || busy.get() || !can_play())
                        on_press=on_play
                    />
                    // Watching here is the default; Kodi is a place to send it.
                    <IconAction
                        label=Signal::derive(|| "Kodi'ye Aktar".to_string())
                        glyph="M4 5h16v10H4zm7 12h2v2h3v2H8v-2h3z"
                        disabled=Signal::derive(move || busy.get() || !can_play())
                        on_press=on_play_on_kodi
                    />
                    <IconAction
                        label=Signal::derive(move || {
                            if has_trailer { "Fragman" } else { "Ön İzle" }.to_string()
                        })
                        glyph="M12 5c-5 0-9 4.5-9 7s4 7 9 7 9-4.5 9-7-4-7-9-7zm0 10a3 3 0 110-6 3 3 0 010 6z"
                        disabled=Signal::derive(move || !preview_ready())
                        on_press=on_preview
                    />
                    // Always reachable, and the reason is not politeness. A
                    // title whose addons return nothing has a disabled play
                    // button, a disabled preview and no source rows, so the
                    // screen had no focusable element at all: the remote was
                    // left sitting on the navigation bar above a screen it
                    // could not enter. Every screen owes the remote somewhere
                    // to stand.
                    <IconAction
                        label=Signal::derive(|| "Geri".to_string())
                        glyph="M15 6l-6 6 6 6"
                        disabled=Signal::derive(|| false)
                        on_press=Callback::new(move |()| nav.back())
                    />
                </div>
            </div>
        </div>
    }
}

/// One labelled group of chips — the shape the metadata takes when it is a set
/// of names rather than a sentence.
#[component]
fn MetaGroup(#[prop(into)] label: String, items: Vec<String>) -> impl IntoView {
    (!items.is_empty()).then(|| {
        view! {
            <div class="meta-group">
                <span class="meta-label">{label}</span>
                <div class="chips">
                    {items.into_iter().map(|text| view! { <Chip text=text /> }).collect_view()}
                </div>
            </div>
        }
    })
}

/// An action drawn as a mark with its name under it.
#[component]
fn IconAction(
    #[prop(into)] label: Signal<String>,
    #[prop(into)] glyph: String,
    #[prop(optional)] primary: bool,
    #[prop(optional)] autofocus: bool,
    #[prop(into)] disabled: Signal<bool>,
    #[prop(into)] on_press: Callback<()>,
) -> impl IntoView {
    let name = label;
    view! {
        <button
            class="icon-action"
            class:is-primary=primary
            data-focus="1"
            data-autofocus=autofocus.then_some("1")
            tabindex="-1"
            disabled=move || disabled.get()
            aria-label=move || name.get()
            on:click=move |_| {
                if !disabled.get_untracked() {
                    on_press.run(());
                }
            }
        >
            <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d=glyph />
            </svg>
            <span class="icon-action-name">{move || name.get()}</span>
        </button>
    }
}

#[component]
fn SourceList(
    sources: RwSignal<Load<Vec<Source>>>,
    tab: RwSignal<Option<String>>,
    selected: RwSignal<Option<Source>>,
    #[prop(into)] on_pick: Callback<Source>,
) -> impl IntoView {
    view! {
        <div class="sources">
            {move || match sources.get() {
                Load::Loading => {
                    view! { <div class="state"><strong>"Kaynaklar aranıyor…"</strong></div> }
                        .into_any()
                }
                Load::Failed(detail) => {
                    view! { <Failure title="Kaynaklar alınamadı" detail=detail /> }.into_any()
                }
                Load::Ready(found) if found.is_empty() => {
                    view! {
                        <div class="state">
                            <strong>"Bu başlık için kaynak yok"</strong>
                            <p>"Kurulu eklentilerin hiçbiri bu başlık için akış döndürmedi."</p>
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(found) => {
                    let chosen = tab.get();
                    let found: Vec<Source> = found
                        .into_iter()
                        .filter(|source| {
                            chosen
                                .as_ref()
                                .is_none_or(|name| &provider_name(&source.parsed) == name)
                        })
                        .collect();
                    // Naming the addon on every row is worth the width only
                    // when there is more than one to tell apart; with a single
                    // provider installed it is the same word fifty times.
                    // The addon's *own* name for itself, not the id the filter
                    // groups by: two installs of Torrentio are two entries in
                    // the filter but one word on the row, and printing that
                    // word fifty times buys nothing.
                    let providers: HashSet<String> =
                        found.iter().map(|source| source.parsed.facts().provider).collect();
                    let name_provider = providers.len() > 1;
                    view! {
                        <div class="source-list">
                            {found
                                .into_iter()
                                .map(|source| {
                                    let picked = source.clone();
                                    let identity = source.parsed.identity.clone();
                                    let facts = source.parsed.facts();
                                    let headline = facts.headline("Kaynak");
                                    let quality = facts.quality.clone();
                                    let flag_line = facts.flag_line();
                                    let cached = facts.cached;
                                    let provider = name_provider
                                        .then(|| facts.provider.clone())
                                        .filter(|name| !name.is_empty());
                                    let chips = facts.chips();
                                    let is_selected = move || {
                                        selected
                                            .get()
                                            .is_some_and(|current| {
                                                current.parsed.identity == identity
                                            })
                                    };
                                    view! {
                                        <button
                                            class="source"
                                            data-focus="1"
                                            tabindex="-1"
                                            aria-selected=move || is_selected().to_string()
                                            on:click=move |_| on_pick.run(picked.clone())
                                        >
                                            // The resolution is what the eye
                                            // goes to first and it is the same
                                            // shape on every row, so it gets a
                                            // column of its own rather than a
                                            // place in a sentence.
                                            <span class="source-badge">
                                                {quality
                                                    .map(|text| {
                                                        view! {
                                                            <span class="source-quality">{text}</span>
                                                        }
                                                    })}
                                                {flag_line
                                                    .map(|text| {
                                                        view! {
                                                            <span class="source-flags">{text}</span>
                                                        }
                                                    })}
                                            </span>
                                            <span class="source-body">
                                                <span class="source-release">{headline}</span>
                                                <span class="source-chips">
                                                    {cached
                                                        .then(|| {
                                                            view! {
                                                                <span class="chip chip-cached">
                                                                    "Hazır"
                                                                </span>
                                                            }
                                                        })}
                                                    {(!source.parsed.playable)
                                                        .then(|| {
                                                            view! {
                                                                <span class="chip bad">
                                                                    "Oynatılamaz"
                                                                </span>
                                                            }
                                                        })}
                                                    <KindChip stream=source.parsed.clone() />
                                                    {chips
                                                        .into_iter()
                                                        .map(|text| {
                                                            view! { <span class="chip">{text}</span> }
                                                        })
                                                        .collect_view()}
                                                    // Last, because it is the
                                                    // least of them: the filter
                                                    // above already says which
                                                    // addons are in the list,
                                                    // so when the line runs out
                                                    // of room this is the one
                                                    // that should go.
                                                    {provider
                                                        .map(|name| {
                                                            view! {
                                                                <span class="chip chip-provider">
                                                                    {name}
                                                                </span>
                                                            }
                                                        })}
                                                </span>
                                            </span>
                                            // The one the remote is on is the
                                            // one that would play; saying so
                                            // with the shape of the action is
                                            // quicker to read than a highlight.
                                            <span class="source-go" aria-hidden="true">
                                                <svg viewBox="0 0 24 24">
                                                    <path d="M8 5l11 7-11 7z" />
                                                </svg>
                                            </span>
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

#[component]
fn KindChip(stream: Stream) -> impl IntoView {
    // "url" is what a debrid link is, and it is what almost every source in a
    // popular title's list is: a chip saying so on fifty-five consecutive rows
    // is fifty-five pieces of furniture. The chip is for the ones that differ —
    // a torrent that has to be fetched, a subscription this box cannot open, a
    // file already on the appliance.
    let (text, tone) = match stream.kind.as_str() {
        "http" => ("Doğrudan HTTP", "good"),
        "torrent" => ("Torrent", "warn"),
        "external" => ("Harici servis", "bad"),
        "youtube" => ("YouTube", ""),
        _ => return None,
    };
    Some(view! { <Chip text=text.to_string() tone=tone.to_string() /> })
}

#[component]
fn Technical(state: Load<Plan>) -> impl IntoView {
    match state {
        Load::Loading => view! {
            <div class="sources">
                <div class="state"><strong>"Kaynak inceleniyor…"</strong>
                    <p>"Dosya başlıkları okunuyor ve bu karta göre karar veriliyor."</p>
                </div>
            </div>
        }
        .into_any(),
        Load::Failed(detail) => view! {
            <div class="sources">
                <div class="state bad">
                    <strong>"Kaynak incelenemedi"</strong>
                    <p>{detail}</p>
                    <p>
                        "Kaynak çözümlendi fakat veri okunamadı. Torrent kaynaklarında bu, \
                         eşlerden hiç parça gelmediği anlamına gelir."
                    </p>
                </div>
            </div>
        }
        .into_any(),
        Load::Ready(plan) => view! { <TechnicalPlan plan=plan /> }.into_any(),
    }
}

#[component]
fn TechnicalPlan(plan: Plan) -> impl IntoView {
    let (verdict, verdict_tone) = plan.verdict();
    let video = plan.video().cloned();
    let audio = plan.audio().cloned();
    let container = plan.media.container.clone();

    let mut chips: Vec<(String, String)> = vec![(verdict.to_string(), verdict_tone.to_string())];
    if let Some(track) = video.as_ref() {
        if let (Some(width), Some(height)) = (track.width, track.height) {
            chips.push((resolution_label(width, height), String::new()));
        }
        if let Some(codec) = track.codec.clone() {
            let label = match track.profile.clone() {
                Some(profile) => format!("{} {profile}", codec.to_uppercase()),
                None => codec.to_uppercase(),
            };
            chips.push((label, String::new()));
        }
        if let Some(depth) = track.bit_depth {
            chips.push((format!("{depth}-bit"), String::new()));
        }
        if let Some(fps) = track.fps {
            chips.push((format!("{fps:.3} fps"), String::new()));
        }
        chips.push(hdr_chip(track));
    }
    if let Some(format) = container.as_ref().and_then(|c| c.format.clone()) {
        chips.push((container_label(&format), String::new()));
    }
    if let Some(track) = audio.as_ref() {
        let codec = track.codec.clone().unwrap_or_else(|| "ses".into()).to_uppercase();
        let layout = track
            .channel_layout
            .clone()
            .or_else(|| track.channels.map(|count| format!("{count} kanal")))
            .unwrap_or_default();
        chips.push((format!("{codec} {layout}").trim().to_string(), String::new()));
    }

    let audio_note = audio_note(&plan);
    let reasons: Vec<Reason> = plan
        .playback
        .reasons
        .iter()
        .chain(plan.playback.video.iter().flat_map(|d| d.reasons.iter()))
        .chain(plan.playback.audio.iter().flat_map(|d| d.reasons.iter()))
        .chain(plan.media.warnings.iter())
        .cloned()
        .collect();
    let preview_reasons = plan.preview.reasons.clone();
    let duration = container.as_ref().and_then(|c| c.duration_seconds);
    let size = container.as_ref().and_then(|c| c.size_bytes);
    let bitrate = container.as_ref().and_then(|c| c.bit_rate);

    view! {
        <div class="sources">
            <h2>"Seçili kaynak"</h2>
            <div class="chips">
                {chips
                    .into_iter()
                    .map(|(text, tone)| view! { <Chip text=text tone=tone /> })
                    .collect_view()}
            </div>
            {audio_note
                .map(|(text, tone)| {
                    view! { <p class=format!("source-note {tone}")>{text}</p> }
                })}
            <div class="rows">
                {duration
                    .map(|value| {
                        view! {
                            <div class="row">
                                <span class="row-label">"Süre"</span>
                                <span class="row-value">
                                    {seconds_to_clock(value as u64)}
                                </span>
                            </div>
                        }
                    })}
                {size
                    .map(|value| {
                        view! {
                            <div class="row">
                                <span class="row-label">"Boyut"</span>
                                <span class="row-value">{human_size(value)}</span>
                            </div>
                        }
                    })}
                {bitrate
                    .map(|value| {
                        view! {
                            <div class="row">
                                <span class="row-label">"Bit hızı"</span>
                                <span class="row-value">{human_bitrate(value)}</span>
                            </div>
                        }
                    })}
                {video
                    .as_ref()
                    .and_then(|track| track.color_transfer.clone())
                    .map(|value| {
                        view! {
                            <div class="row">
                                <span class="row-label">"Transfer / birincil renkler"</span>
                                <span class="row-value">
                                    {format!(
                                        "{value} / {}",
                                        video
                                            .as_ref()
                                            .and_then(|t| t.color_primaries.clone())
                                            .unwrap_or_else(|| "?".into()),
                                    )}
                                </span>
                            </div>
                        }
                    })}
            </div>
            <ul class="reasons">
                {reasons
                    .into_iter()
                    .map(|reason| {
                        let tone = severity_class(&reason.severity);
                        view! { <li class=tone>{reason.message}</li> }
                    })
                    .collect_view()}
                {preview_reasons
                    .into_iter()
                    .map(|reason| {
                        let tone = severity_class(&reason.severity);
                        view! {
                            <li class=tone>{format!("Ön izleme: {}", reason.message)}</li>
                        }
                    })
                    .collect_view()}
            </ul>
        </div>
    }
}

fn severity_class(severity: &str) -> &'static str {
    match severity {
        "blocking" | "error" => "bad",
        "warning" => "warn",
        _ => "",
    }
}

fn container_label(format: &str) -> String {
    match format {
        "mov,mp4,m4a,3gp,3g2,mj2" => "MP4".to_string(),
        "matroska,webm" | "matroska" => "MKV".to_string(),
        "mpegts" | "mpegts,hls" => "MPEG-TS".to_string(),
        other => other.to_uppercase(),
    }
}

/// The HDR badge.
///
/// Dolby Vision is the case that must never be softened. This board has no
/// Dolby Vision pipeline, and a profile 5 stream carries no HDR10 base layer
/// at all — drawing it as "HDR10" would promise a picture the television will
/// never receive, so it is labelled for what it is.
fn hdr_chip(track: &VideoTrack) -> (String, String) {
    if let Some(dv) = track.dolby_vision.as_ref() {
        let profile = dv.profile.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
        let cross_compatible = dv.bl_signal_compatibility_id.is_some_and(|id| id != 0);
        return if cross_compatible {
            (
                format!("Dolby Vision Profil {profile} — DV katmanı yok sayılır"),
                "warn".into(),
            )
        } else {
            (
                format!("Dolby Vision Profil {profile} — desteklenmiyor"),
                "bad".into(),
            )
        };
    }
    match track.hdr.as_deref() {
        Some("HDR10") => ("HDR10".into(), "hot".into()),
        Some("HDR10Plus") | Some("HDR10+") => ("HDR10+".into(), "hot".into()),
        Some("HLG") => ("HLG".into(), "hot".into()),
        Some("SDR") => ("SDR".into(), String::new()),
        Some(other) => (other.to_string(), String::new()),
        None => ("HDR bilgisi yok".into(), String::new()),
    }
}

/// What happens to the audio, in one sentence.
fn audio_note(plan: &Plan) -> Option<(String, &'static str)> {
    let decision = plan.playback.audio.as_ref()?;
    let track = decision.track.as_ref();
    // The flag says there is an object layer; the profile is what to call it.
    let object = track
        .filter(|t| t.object_audio.unwrap_or(false))
        .and_then(|t| t.profile.clone());
    match decision.action.as_str() {
        "Passthrough" => Some(("Ses bit-perfect olarak alıcıya iletilir.".into(), "")),
        "DecodeToPCM" => Some((
            "Ses oynatıcıda PCM'e çözülür; kayıpsız bir aktarım yapılmaz.".into(),
            "",
        )),
        "TranscodeToAC3" => {
            let lost = object
                .map(|format| {
                    format!(
                        "{format} nesne tabanlı ses AC-3 5.1'e indirgenir ve nesne katmanı kaybolur."
                    )
                })
                .unwrap_or_else(|| {
                    "Ses AC-3 5.1'e dönüştürülür; özgün kayıpsız akış korunmaz.".into()
                });
            Some((format!("{lost} Video kopyalanır, yeniden kodlanmaz."), "warn"))
        }
        "Unsupported" => Some(("Bu ses akışı bu cihazda oynatılamaz.".into(), "bad")),
        _ => None,
    }
}

/// What "Ön İzle" opens.
///
/// It used to mean one thing — the chosen source, played in the browser — and
/// on this appliance that is almost never possible: the sources worth watching
/// are 4K HEVC, ten-bit, HDR, and no browser will decode them. The button was
/// therefore disabled on nearly every title, which is not a preview feature.
///
/// The trailer is what a person actually wants from a preview, it is already
/// in the addon's own metadata, and it plays anywhere. The source preview is
/// kept for the rare title where it works.
#[derive(Clone, PartialEq)]
enum Preview {
    /// A YouTube id, from the title's metadata.
    Trailer(String),
    /// The chosen source itself, when the browser can genuinely open it.
    Source(String),
}

impl Preview {
    fn heading(&self) -> &'static str {
        match self {
            Self::Trailer(_) => "Fragman",
            Self::Source(_) => "Ön İzleme",
        }
    }

    fn note(&self) -> &'static str {
        match self {
            Self::Trailer(_) => {
                "Filmin fragmanı. Filmin tamamı için kapatıp \"Kodi'de Oynat\" seçeneğini \
                 kullanın."
            }
            Self::Source(_) => {
                "Bu, tarayıcıda doğrudan açılan kaynağın kendisidir. Televizyon oynatması \
                 için kapatıp \"Kodi'de Oynat\" seçeneğini kullanın."
            }
        }
    }
}

#[component]
fn PreviewSheet(preview: Preview, #[prop(into)] on_close: Callback<()>) -> impl IntoView {
    view! {
        <div class="overlay" data-focus-scope="1">
            <div class="sheet">
                <h2>{preview.heading()}</h2>
                <p class="panel-note">{preview.note()}</p>
                {match preview {
                    Preview::Trailer(id) => {
                        view! {
                            <iframe
                                class="preview-frame"
                                src=format!(
                                    "https://www.youtube.com/embed/{id}\
                                     ?autoplay=1&rel=0&modestbranding=1&playsinline=1",
                                )
                                allow="autoplay; encrypted-media; picture-in-picture"
                                allowfullscreen
                                referrerpolicy="strict-origin-when-cross-origin"
                            ></iframe>
                        }
                            .into_any()
                    }
                    Preview::Source(url) => {
                        view! { <video src=url controls autoplay playsinline></video> }.into_any()
                    }
                }}
                <div class="actions-row">
                    <Action
                        label="Kapat"
                        variant="primary"
                        autofocus=true
                        on_press=Callback::new(move |()| on_close.run(()))
                    />
                </div>
            </div>
        </div>
    }
}
