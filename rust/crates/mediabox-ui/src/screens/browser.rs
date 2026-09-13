//! The web, as an application of this box.
//!
//! The browser itself is a separate unit with no address bar, because a
//! television remote cannot use one: four arrows and an OK button. So the
//! address is chosen here, in an interface built for a remote, and handed to
//! the control plane — which leaves it where the browser reads it at start and
//! then gives the browser the display. Before this screen existed the browser
//! could only ever open the one address compiled into its launcher, which made
//! it a kiosk for a single site rather than a browser.

use crate::api;
use crate::app::Toaster;
use crate::components::{Action, Field, Keyboard};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// Somewhere worth starting, and reachable with four arrows.
const SITES: [(&str, &str, &str); 7] = [
    // Stremio's own interface, pointed at the streaming server this box
    // already runs. Nothing is installed for this: the server is the part that
    // matters and it is local, so the catalogue, the addons and the account are
    // the ones the operator already has.
    (
        "Stremio",
        "https://web.stremio.com/#/?streamingServer=http%3A%2F%2F127.0.0.1%3A11470",
        "Kendi arayüzü, yerel sunucu",
    ),
    ("YouTube", "https://www.youtube.com/tv", "Televizyon arayüzü"),
    ("Twitch", "https://www.twitch.tv", "Canlı yayın"),
    ("Vikipedi", "https://tr.wikipedia.org", "Ansiklopedi"),
    ("Google", "https://www.google.com", "Arama"),
    ("X", "https://x.com", "Akış"),
    ("Hava Durumu", "https://www.mgm.gov.tr", "Meteoroloji"),
];

#[component]
pub fn Browser() -> impl IntoView {
    let toaster = expect_context::<Toaster>();
    let address = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let open = move |url: String| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        toaster.say("Tarayıcı açılıyor…".to_string());
        spawn_local(async move {
            if let Err(error) = api::control(api::browser_open(&url)).await {
                toaster.warn(format!("Tarayıcı açılamadı: {}", error.message));
            }
            busy.set(false);
        });
    };

    // What a person types is a hostname, not a URL. The scheme is the
    // product's business, and typing "https://" with a remote is eleven
    // presses of nothing.
    let go = move |()| {
        let typed = address.get_untracked();
        let typed = typed.trim().to_string();
        if typed.is_empty() {
            return;
        }
        let url = if typed.starts_with("http://") || typed.starts_with("https://") {
            typed
        } else {
            format!("https://{typed}")
        };
        open(url);
    };

    view! {
        <div class="browser">
            <section class="apps" data-row="1">
                <div class="rail-head">
                    <h2 class="rail-title">"Siteler"</h2>
                </div>
                // The same tile as the home screen's applications, because to
                // the person holding the remote that is what these are: one
                // press, one place. A separate shape for them would have been a
                // second visual language for the same idea.
                <div class="app-grid">
                    {SITES
                        .into_iter()
                        .enumerate()
                        .map(|(position, (name, url, note))| {
                            let go_here = move |_| open(url.to_string());
                            let tint = name
                                .bytes()
                                .fold(7u32, |acc, byte| {
                                    acc.wrapping_mul(31).wrapping_add(byte as u32)
                                }) % 360;
                            view! {
                                <button
                                    class="app-tile"
                                    data-focus="1"
                                    data-autofocus=(position == 0).then_some("1")
                                    data-focus-key=format!("site:{name}")
                                    tabindex="-1"
                                    on:click=go_here
                                >
                                    <span class="app-icon" style=format!("--tint:{tint}")>
                                        <span class="site-initial">
                                            {name.chars().next().unwrap_or('?').to_string()}
                                        </span>
                                    </span>
                                    <span class="app-name">{name}</span>
                                    <span class="app-note">{note}</span>
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </section>

            <section class="keyboard">
                // A real field: the remote can build it with the keys below,
                // and a keyboard or a phone can type and paste straight into
                // it. It used to be a readout only the on-screen keys could
                // write to, which made an attached keyboard useless and paste
                // impossible.
                <Field
                    label="Adres"
                    value=address
                    key="field:address"
                    placeholder="site.com"
                    on_enter=Callback::new(go)
                />

                <Keyboard value=address />

                <div class="keys wide" data-row="1">
                    <button
                        class="key wide"
                        data-focus="1"
                        data-focus-key="key:www"
                        tabindex="-1"
                        on:click=move |_| address.update(|value| value.push_str("www."))
                    >
                        "www."
                    </button>
                    <button
                        class="key wide"
                        data-focus="1"
                        data-focus-key="key:com"
                        tabindex="-1"
                        on:click=move |_| address.update(|value| value.push_str(".com"))
                    >
                        ".com"
                    </button>
                    <button
                        class="key wide"
                        data-focus="1"
                        data-focus-key="key:back"
                        tabindex="-1"
                        on:click=move |_| {
                            address
                                .update(|value| {
                                    value.pop();
                                })
                        }
                    >
                        "Sil"
                    </button>
                    <button
                        class="key wide"
                        data-focus="1"
                        data-focus-key="key:clear"
                        tabindex="-1"
                        on:click=move |_| address.set(String::new())
                    >
                        "Temizle"
                    </button>
                    <Action
                        label=move || if busy.get() { "Açılıyor…".to_string() } else { "Git".to_string() }
                        variant="primary"
                        disabled=Signal::derive(move || busy.get() || address.get().trim().is_empty())
                        on_press=Callback::new(go)
                    />
                </div>
                <p class="panel-note">
                    "Tarayıcı ekranı devralır. Kumandanın geri tuşu arayüze döndürür."
                </p>
            </section>
        </div>
    }
}
