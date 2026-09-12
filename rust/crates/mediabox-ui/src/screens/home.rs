//! Home: artwork first, real catalogues, nothing about the appliance itself.
//!
//! What is on this screen is what the media core answers with — Stremio
//! catalogues through the headless worker, and the appliance's own library
//! ahead of them. There is no placeholder row: a catalogue that returns
//! nothing is not drawn at all, because an empty shelf that looks like content
//! is worse than no shelf.

use crate::app::{Nav, Recent, Route, recents};
use crate::components::{Action, Failure, Load, Rail, RailSkeleton};
use crate::model::{HomeRows, LibraryListing, MetaPreview};
use crate::{api, LIBRARY_ADDON_ID};
use leptos::prelude::*;
use leptos::task::spawn_local;

#[component]
pub fn Home() -> impl IntoView {
    let nav = expect_context::<Nav>();
    let rows = RwSignal::new(Load::Loading);
    let library = RwSignal::new(Vec::<MetaPreview>::new());
    let recent = RwSignal::new(recents());

    spawn_local(async move {
        match api::typed::<HomeRows>(api::media_home()).await {
            Ok(home) => rows.set(Load::Ready(home)),
            Err(error) => rows.set(Load::Failed(error.message)),
        }
    });
    spawn_local(async move {
        if let Ok(listing) = api::typed::<LibraryListing>(api::media_library()).await {
            library.set(listing.items);
        }
    });

    let open = Callback::new(move |item: MetaPreview| {
        // A library title may share an IMDb id with a catalogue title. What
        // separates them is provenance: the library entry names the operator's
        // own source, which is the one certain to play, so it must not be
        // resolved through the addons instead.
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
        {move || hero().map(|item| view! { <Hero item=item on_open=open /> })}
        {move || {
            let entries: Vec<MetaPreview> = recent
                .get()
                .into_iter()
                .map(Recent::into)
                .collect();
            view! { <Rail title="Devam Et" items=entries on_pick=open /> }
        }}
        {move || {
            let items = library.get();
            view! { <Rail title="Kitaplık" source="Bu cihazda" items=items on_pick=open /> }
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
                        view! {
                            <Rail
                                title=rail_title(&row.name, &row.kind)
                                source=row.addon_name.clone()
                                items=row.items
                                on_pick=open
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
                    rails.into_any()
                }
            }
        }}
    }
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

#[component]
fn Hero(item: MetaPreview, #[prop(into)] on_open: Callback<MetaPreview>) -> impl IntoView {
    let nav = expect_context::<Nav>();
    let art = item
        .background
        .clone()
        .or_else(|| item.poster.clone())
        .map(|url| format!("background-image:url('{url}')"))
        .unwrap_or_default();
    let logo = item.logo.clone();
    let name = item.name.clone();
    let title_for_alt = item.name.clone();
    let description = item.description.clone();
    let mut facts: Vec<String> = Vec::new();
    if let Some(year) = item.release_info.clone() {
        facts.push(year);
    }
    if let Some(rating) = item.imdb_rating.clone() {
        facts.push(format!("IMDb {rating}"));
    }
    facts.extend(item.genres.iter().take(3).cloned());
    let picked = item.clone();

    view! {
        <section class="hero">
            <div class="hero-art" style=art></div>
            <div class="hero-body">
                {match logo {
                    Some(url) => {
                        view! { <img class="hero-logo" src=url alt=title_for_alt /> }.into_any()
                    }
                    None => view! { <h1 class="hero-title">{name}</h1> }.into_any(),
                }}
                <div class="hero-line">
                    {facts
                        .into_iter()
                        .map(|fact| view! { <span class="chip">{fact}</span> })
                        .collect_view()}
                </div>
                {description
                    .map(|text| view! { <p class="hero-desc">{text}</p> })}
                <div class="hero-actions">
                    <Action
                        label="Aç"
                        variant="primary"
                        autofocus=true
                        on_press=Callback::new(move |()| on_open.run(picked.clone()))
                    />
                    <Action
                        label="Ara"
                        variant="ghost"
                        on_press=Callback::new(move |()| nav.go(Route::Search))
                    />
                </div>
            </div>
        </section>
    }
}
