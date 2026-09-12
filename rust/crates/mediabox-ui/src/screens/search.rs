//! Search across every catalogue the media core can reach.

use crate::app::{Nav, Route};
use crate::components::{Card, Failure, Load};
use crate::model::{MetaPreview, SearchResults};
use crate::{api, focus};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

#[component]
pub fn Search() -> impl IntoView {
    let nav = expect_context::<Nav>();
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(None::<Load<Vec<MetaPreview>>>);
    // Every keystroke would be a fan-out across every searchable addon, so a
    // search is only sent when the person says they are done typing.
    let submitted = RwSignal::new(String::new());

    let run = move || {
        let text = query.get_untracked().trim().to_string();
        if text.is_empty() || text == submitted.get_untracked() {
            return;
        }
        submitted.set(text.clone());
        results.set(Some(Load::Loading));
        spawn_local(async move {
            match api::typed::<SearchResults>(api::media_search(&text)).await {
                Ok(found) => {
                    let mut seen = std::collections::HashSet::new();
                    let items: Vec<MetaPreview> = found
                        .rows
                        .into_iter()
                        .flat_map(|row| row.items)
                        // The same title comes back from several catalogues;
                        // showing it three times is noise, not a result.
                        .filter(|item| seen.insert(item.id.clone()))
                        .collect();
                    results.set(Some(Load::Ready(items)));
                }
                Err(error) => results.set(Some(Load::Failed(error.message))),
            }
        });
    };

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

    view! {
        <div class="search">
            <div class="search-field" data-focus="1" tabindex="-1" data-autofocus="1"
                on:focus=move |event| {
                    // The wrapper is what the remote lands on; the caret has to
                    // follow it in, or typing would go nowhere.
                    if let Some(target) = event.target()
                        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                        .and_then(|t| t.query_selector("input").ok().flatten())
                        .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
                    {
                        let _ = target.focus();
                    }
                }
            >
                <span aria-hidden="true">"⌕"</span>
                <input
                    type="search"
                    placeholder="Film veya dizi ara"
                    autocomplete="off"
                    prop:value=move || query.get()
                    on:input=move |event| query.set(event_target_value(&event))
                    on:keydown=move |event| {
                        match event.key().as_str() {
                            "Enter" => {
                                event.prevent_default();
                                run();
                            }
                            "ArrowDown" | "Escape" => {
                                // Leave the field so the arrows belong to the
                                // results again.
                                event.prevent_default();
                                if let Some(element) = event
                                    .target()
                                    .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                                {
                                    let _ = element.blur();
                                }
                                focus::focus_first();
                                focus::step(focus::Direction::Down);
                            }
                            _ => {}
                        }
                    }
                />
            </div>
            {move || match results.get() {
                None => {
                    view! {
                        <div class="state">
                            <strong>"Ne izlemek istersiniz?"</strong>
                            <p>
                                "Yazın ve Enter'a basın. Arama kurulu tüm kataloglarda çalışır."
                            </p>
                        </div>
                    }
                        .into_any()
                }
                Some(Load::Loading) => {
                    view! {
                        <div class="state">
                            <strong>"Aranıyor…"</strong>
                        </div>
                    }
                        .into_any()
                }
                Some(Load::Failed(detail)) => {
                    view! { <Failure title="Arama başarısız" detail=detail /> }.into_any()
                }
                Some(Load::Ready(items)) if items.is_empty() => {
                    view! {
                        <div class="state">
                            <strong>"Sonuç yok"</strong>
                            <p>{format!("\"{}\" için eşleşme bulunamadı.", submitted.get())}</p>
                        </div>
                    }
                        .into_any()
                }
                Some(Load::Ready(items)) => {
                    view! {
                        <div class="results-grid">
                            {items
                                .into_iter()
                                .map(|item| view! { <Card item=item on_pick=open /> })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}
