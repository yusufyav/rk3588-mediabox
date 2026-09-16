//! One shelf, opened out into a grid.
//!
//! A rail shows what fits across the television and hides the rest behind a
//! long horizontal walk: seventy titles is seventy presses of the right arrow,
//! with no way to see where you are in them. The same seventy in a grid are
//! six presses down. So every shelf has a way out of itself, and this is where
//! it leads.
//!
//! Where the titles come from depends on the shelf. A catalogue shelf is asked
//! again, without the row's limit, so the grid holds more than the rail did. A
//! shelf built from the operator's own library is not asked again at all: the
//! library is one list this box already has, and fetching it twice to show the
//! same answer would only add a wait.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::{Nav, Route};
use crate::components::{Failure, Load};
use crate::model::{CatalogItems, LibraryListing, MetaPreview, WatchState};
use crate::{LIBRARY_ADDON_ID, api};

/// The shelves that are not catalogues, named where a catalogue's addon id
/// would be. They are answered from the library rather than from an addon.
pub const CONTINUE_SOURCE: &str = "mediabox.continue";
pub const ACCOUNT_LIBRARY_SOURCE: &str = "stremio.library";

#[component]
pub fn Collection(addon: String, kind: String, catalog: String, title: String) -> impl IntoView {
    let nav = expect_context::<Nav>();
    let items = RwSignal::new(Load::Loading);

    let source = addon.clone();
    let load_kind = kind.clone();
    let load_catalog = catalog.clone();
    spawn_local(async move {
        let loaded = match source.as_str() {
            CONTINUE_SOURCE | ACCOUNT_LIBRARY_SOURCE | LIBRARY_ADDON_ID => {
                api::typed::<LibraryListing>(api::media_library())
                    .await
                    .map(|listing| match source.as_str() {
                        LIBRARY_ADDON_ID => listing.items,
                        CONTINUE_SOURCE => listing
                            .stremio
                            .into_iter()
                            .filter(|item| item.state.as_ref().is_some_and(WatchState::unfinished))
                            .collect(),
                        _ => listing
                            .stremio
                            .into_iter()
                            .filter(|item| !item.state.as_ref().is_some_and(WatchState::unfinished))
                            .collect(),
                    })
            }
            addon_id => api::typed::<CatalogItems>(api::media_catalog(
                &load_kind,
                &load_catalog,
                Some(addon_id),
                // Enough to be worth opening out, and bounded: an addon asked
                // for everything can answer with thousands, and a grid that
                // long is a slower way to find nothing.
                Some(200),
            ))
            .await
            .map(|listing| listing.items),
        };
        items.set(match loaded {
            Ok(found) => Load::Ready(found),
            Err(error) => Load::Failed(error.message),
        });
    });

    let open = Callback::new(move |item: MetaPreview| {
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

    view! {
        <section class="collection">
            <div class="collection-head">
                <h1>{title}</h1>
                {move || {
                    items
                        .with(|state| state.ready().map(Vec::len))
                        .map(|count| view! { <span class="collection-count">{count}" başlık"</span> })
                }}
            </div>
            {move || match items.get() {
                Load::Loading => {
                    view! { <div class="state"><strong>"Yükleniyor…"</strong></div> }.into_any()
                }
                Load::Failed(detail) => {
                    view! { <Failure title="Liste alınamadı" detail=detail /> }.into_any()
                }
                Load::Ready(found) if found.is_empty() => {
                    view! {
                        <div class="state"><strong>"Burada henüz bir şey yok"</strong></div>
                    }
                        .into_any()
                }
                Load::Ready(found) => {
                    view! {
                        <div class="grid">
                            {found
                                .into_iter()
                                .map(|item| {
                                    view! { <crate::components::Card item=item on_pick=open /> }
                                })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}
