//! The media application: artwork first, real catalogues.
//!
//! What is on this screen is what the media core answers with — Stremio
//! catalogues through the headless worker, and the appliance's own library
//! ahead of them. There is no placeholder row: a catalogue that returns
//! nothing is not drawn at all, because an empty shelf that looks like content
//! is worse than no shelf.

use crate::app::{Nav, Recent, Route, recents};

/// How many titles a shelf carries before the rest is left to the grid.
///
/// A catalogue answers with forty-five and more, and a shelf holding all of
/// them is a layer eleven thousand pixels wide that has to be laid out and
/// painted the first time it is revealed — measured at 139 ms of rendering in
/// one frame, which is a visible lurch. Nobody walks sideways through
/// forty-five posters anyway; past the twentieth the grid is the better way in,
/// and every shelf now has a door to it.
const RAIL_LIMIT: usize = 20;
use crate::components::{Action, Failure, Load, Rail, RailSkeleton, meta_line};
use crate::model::{HomeRows, LibraryIds, LibraryListing, MetaPreview, WatchState};
use crate::screens::collection::{ACCOUNT_LIBRARY_SOURCE, CONTINUE_SOURCE};
use crate::{LIBRARY_ADDON_ID, api};
use leptos::context::Provider;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;

#[component]
pub fn Media() -> impl IntoView {
    let nav = expect_context::<Nav>();
    // The catalogue this device saw last, drawn before anything is asked for.
    //
    // Assembling the home screen means the media core calling out to every
    // installed addon, and on this appliance that measured 2.1 s warm and 17.0 s
    // cold. For those seconds the television had a hero and nothing under it,
    // which is what "half the screen arrives later" is. The shelves that were
    // there last time are shown immediately and replaced the moment the real
    // answer lands — and only if it differs, so a screen that has not changed
    // is never rebuilt underneath the remote.
    let rows = RwSignal::new(match cached_home() {
        Some(home) => Load::Ready(home),
        None => Load::Loading,
    });
    let library = RwSignal::new(cached_library());
    // The account's own library, which is a different thing from the shelf
    // above it: those titles live on this box, these follow the operator.
    let mine = RwSignal::new(cached_mine());
    let recent = RwSignal::new(recents());
    spawn_local(async move {
        match api::typed::<HomeRows>(api::media_home()).await {
            Ok(home) => {
                remember_home(&home);
                if rows.with_untracked(|state| state.ready() != Some(&home)) {
                    rows.set(Load::Ready(home));
                }
            }
            // A failed refresh must not take away shelves that are on screen
            // and still perfectly good.
            Err(error) => {
                if rows.with_untracked(|state| state.ready().is_none()) {
                    rows.set(Load::Failed(error.message));
                }
            }
        }
    });
    // Why a library can fail to arrive is worth saying out loud. It used to be
    // swallowed here, and the screen simply had one shelf fewer than it should
    // — indistinguishable from an account with nothing in it.
    let library_error = RwSignal::new(None::<String>);
    // Shared with every poster below, so a title the operator already keeps is
    // marked wherever a catalogue happens to list it again.
    let owned = LibraryIds(RwSignal::new(std::collections::HashSet::new()));
    spawn_local(async move {
        match api::typed::<LibraryListing>(api::media_library()).await {
            Ok(listing) => {
                library_error.set(None);
                remember_library(&listing.items);
                remember_mine(&listing.stremio);
                if library.with_untracked(|seen| seen != &listing.items) {
                    library.set(listing.items);
                }
                owned
                    .0
                    .set(listing.stremio.iter().map(|item| item.id.clone()).collect());
                if mine.with_untracked(|seen| seen != &listing.stremio) {
                    mine.set(listing.stremio);
                }
            }
            Err(error) => library_error.set(Some(error.message)),
        }
    });

    let open = Callback::new(move |item: MetaPreview| {
        // A library title may share an IMDb id with a catalogue title. What
        // separates them is provenance: the library entry names the operator's
        // own source, which is the one certain to play, so it must not be
        // resolved through the addons instead.
        nav.seed(item.clone());
        nav.go(Route::Detail {
            kind: if item.is_library() {
                "library".to_string()
            } else {
                item.kind.clone()
            },
            id: item.id.clone(),
        });
    });

    let hero = move || {
        rows.with(|state| {
            state
                .ready()
                .and_then(|home| {
                    home.rows
                        .iter()
                        .filter(|row| row.addon_id != LIBRARY_ADDON_ID)
                        .flat_map(|row| row.items.iter())
                        .find(|item| item.background.is_some())
                        .cloned()
                })
                .or_else(|| library.get().first().cloned())
        })
    };

    view! {
        // The first screenful is one picture with the title of the evening
        // standing on it.
        <section class="stage stage--media">
            {move || hero().map(|item| view! { <Backdrop item=item /> })}
            {move || hero().map(|item| view! { <Hero item=item on_open=open /> })}
        </section>
        {move || {
            // What to carry on with. The account knows this across every device
            // the operator uses, so it is asked first; the titles this box
            // itself remembers are the answer only until the account replies,
            // and when there is no account at all.
            let carried: Vec<MetaPreview> = mine
                .get()
                .into_iter()
                .filter(|item| item.state.as_ref().is_some_and(WatchState::unfinished))
                .collect();
            let entries: Vec<MetaPreview> = if carried.is_empty() {
                recent.get().into_iter().map(Recent::into).collect()
            } else {
                carried
            }
            .into_iter()
            .take(RAIL_LIMIT)
            .collect();
            let all = Callback::new(move |()| {
                nav.go(Route::Collection {
                    addon: CONTINUE_SOURCE.to_string(),
                    kind: "movie".to_string(),
                    catalog: String::new(),
                    title: "Devam Et".to_string(),
                })
            });
            view! { <Rail title="Devam Et" items=entries on_pick=open on_all=all /> }
        }}
        {move || {
            // Whatever is being carried on with is on the shelf above; a
            // library that repeats it is the same eight posters twice.
            let items: Vec<MetaPreview> = mine
                .get()
                .into_iter()
                .filter(|item| !item.state.as_ref().is_some_and(WatchState::unfinished))
                .take(RAIL_LIMIT)
                .collect();
            let all = Callback::new(move |()| {
                nav.go(Route::Collection {
                    addon: ACCOUNT_LIBRARY_SOURCE.to_string(),
                    kind: "movie".to_string(),
                    catalog: String::new(),
                    title: "Kitaplığım".to_string(),
                })
            });
            view! {
                <Rail
                    title="Kitaplığım"
                    source="Stremio hesabın"
                    items=items
                    on_pick=open
                    on_all=all
                />
            }
        }}
        {move || {
            let items = library.get();
            let all = Callback::new(move |()| {
                nav.go(Route::Collection {
                    addon: LIBRARY_ADDON_ID.to_string(),
                    kind: "movie".to_string(),
                    catalog: String::new(),
                    title: "Kitaplık".to_string(),
                })
            });
            view! {
                <Rail
                    title="Kitaplık"
                    source="Bu cihazda"
                    items=items
                    on_pick=open
                    on_all=all
                />
            }
        }}
        {move || {
            library_error
                .get()
                .map(|detail| {
                    view! {
                        <Failure
                            title="Kitaplık alınamadı"
                            detail=format!("Hesabınızın kitaplığı okunamadı. {detail}")
                        />
                    }
                })
        }}
        {move || match rows.get() {
            Load::Loading => view! { <RailSkeleton /> }.into_any(),
            Load::Failed(detail) => {
                view! {
                    <Failure
                        title="Katalog alınamadı"
                        detail=format!(
                            "Medya çekirdeğinden katalog okunamadı. {detail}",
                        )
                    />
                }
                    .into_any()
            }
            Load::Ready(home) => {
                let rails: Vec<_> = home
                    .rows
                    .into_iter()
                    .filter(|row| row.addon_id != LIBRARY_ADDON_ID && !row.items.is_empty())
                    .map(|row| {
                        let title = rail_title(&row.name, &row.kind);
                        let items: Vec<MetaPreview> =
                            row.items.into_iter().take(RAIL_LIMIT).collect();
                        let heading = title.clone();
                        let addon = row.addon_id.clone();
                        let kind = row.kind.clone();
                        let catalog = row.catalog_id.clone();
                        let all = Callback::new(move |()| {
                            nav.go(Route::Collection {
                                addon: addon.clone(),
                                kind: kind.clone(),
                                catalog: catalog.clone(),
                                title: heading.clone(),
                            })
                        });
                        view! {
                            <Rail
                                title=title
                                source=row.addon_name.clone()
                                items=items
                                on_pick=open
                                on_all=all
                            />
                        }
                    })
                    .collect();
                if rails.is_empty() {
                    view! {
                        <Failure
                            title="Katalog boş döndü"
                            detail="Eklentiler yanıt verdi fakat gösterilecek başlık göndermedi."
                        />
                    }
                        .into_any()
                } else {
                    // The badge belongs here and only here: on a catalogue
                    // shelf it says "you already keep this one". On the
                    // library's own shelves it would be on every poster,
                    // saying nothing.
                    view! { <Provider value=owned>{rails}</Provider> }.into_any()
                }
            }
        }}
    }
}

// ------------------------------------------------------------ last seen

const HOME_KEY: &str = "mediabox.home";
const LIBRARY_KEY: &str = "mediabox.library";

fn store() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Read something kept from the last visit, or nothing at all.
///
/// Storage can be unavailable, and what is in it can be from an older build
/// whose shape no longer parses. Either way the screen simply starts empty
/// rather than breaking, which is why every failure here is the same answer.
fn kept<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    let raw = store()?.get_item(key).ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

fn keep<T: serde::Serialize>(key: &str, value: &T) {
    if let Some(store) = store()
        && let Ok(raw) = serde_json::to_string(value)
    {
        let _ = store.set_item(key, &raw);
    }
}

fn cached_home() -> Option<HomeRows> {
    kept::<HomeRows>(HOME_KEY).filter(|home| !home.rows.is_empty())
}

fn remember_home(home: &HomeRows) {
    keep(HOME_KEY, home);
}

fn cached_library() -> Vec<MetaPreview> {
    kept::<Vec<MetaPreview>>(LIBRARY_KEY).unwrap_or_default()
}

fn cached_mine() -> Vec<MetaPreview> {
    kept("mediabox.mine").unwrap_or_default()
}

fn remember_mine(items: &[MetaPreview]) {
    keep("mediabox.mine", &items.to_vec());
}

fn remember_library(items: &[MetaPreview]) {
    keep(LIBRARY_KEY, &items);
}

/// Catalogue names arrive in English from the addon; the two that name the
/// whole shelf are the ones worth saying in the product's own language.
fn rail_title(name: &str, kind: &str) -> String {
    let kind_label = match kind {
        "movie" => "Filmler",
        "series" => "Diziler",
        _ => return name.to_string(),
    };
    match name {
        "Popular" => format!("Popüler {kind_label}"),
        "Featured" => format!("Öne Çıkan {kind_label}"),
        "New" | "Latest" => format!("Yeni {kind_label}"),
        other => format!("{kind_label} · {other}"),
    }
}

/// The artwork behind the first screenful.
#[component]
fn Backdrop(item: MetaPreview) -> impl IntoView {
    let art = item.background.clone().or_else(|| item.poster.clone());
    let alt = format!("{} arka planı", item.name);
    view! {
        // The one picture that fills the screen, and the only one on it fetched
        // eagerly. Every poster below is lazy and most of them start below the
        // fold, so the browser's handful of connections per host go here first
        // instead of to a hundred thumbnails — which is what used to leave the
        // backdrop painted half finished, in bands, while the rails underneath
        // it were already full.
        //
        // Shown only once it is whole. A backdrop is a large progressive JPEG
        // and Chromium paints each pass as it arrives, so the television showed
        // the picture assembling itself in bands — which is exactly what
        // somebody sitting in front of it reads as the screen tearing.
        {art
            .map(|url| {
                view! {
                    <img
                        class="hero-img"
                        src=url
                        alt=alt
                        decoding="async"
                        draggable="false"
                        on:load=|event| {
                            if let Some(target) = event.target()
                                && let Ok(image) = target.dyn_into::<web_sys::Element>()
                            {
                                let _ = image.class_list().add_1("is-ready");
                            }
                        }
                    />
                }
            })}
        <div class="hero-scrim"></div>
    }
}

#[component]
fn Hero(item: MetaPreview, #[prop(into)] on_open: Callback<MetaPreview>) -> impl IntoView {
    let logo = item.logo.clone();
    let name = item.name.clone();
    let title_for_alt = item.name.clone();
    let description = item.description.clone();
    let facts = meta_line(&item);
    let picked = item.clone();

    view! {
        <div class="hero" data-row="1">
            <div class="hero-body">
                {match logo {
                    Some(url) => {
                        view! {
                            <img class="hero-logo" src=url alt=title_for_alt decoding="async" />
                        }
                            .into_any()
                    }
                    None => view! { <h1 class="hero-title">{name}</h1> }.into_any(),
                }}
                {(!facts.is_empty()).then(|| view! { <p class="hero-meta">{facts}</p> })}
                {description.map(|text| view! { <p class="hero-desc">{text}</p> })}
                <div class="hero-actions">
                    <Action
                        label="Aç"
                        variant="primary"
                        on_press=Callback::new(move |()| on_open.run(picked.clone()))
                    />
                </div>
            </div>
        </div>
    }
}
