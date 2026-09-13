//! What is on the television right now.
//!
//! Every value here is read back from Kodi through the control plane rather
//! than remembered from whatever this browser asked for. If somebody stops
//! playback with the physical remote, this screen says so within a second,
//! because it is reporting the player's state and not its own intent.
//!
//! The polling is why this screen is built the way it is. A once-a-second read
//! that re-rendered the page would destroy and rebuild the transport buttons
//! every tick — the focused one included — and the remote would be thrown off
//! its place once a second. So the structure depends only on `Phase`, which
//! changes when playback actually starts or stops, and everything that ticks
//! is bound to its own signal and updates text in place.

use crate::api;
use crate::app::{Nav, Route, Toaster};
use crate::components::{Action, seconds_to_clock};
use crate::model::SystemStatus;
use leptos::prelude::*;
use leptos::task::spawn_local;

const POLL: std::time::Duration = std::time::Duration::from_millis(1200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Unknown,
    Offline,
    Idle,
    Playing,
}

/// Set a signal only when its value actually changed.
///
/// Leptos notifies on every `set`, so a poll that writes the same value would
/// still invalidate everything reading it.
fn put<T: PartialEq + Send + Sync + 'static>(signal: RwSignal<T>, value: T) {
    if signal.with_untracked(|current| current != &value) {
        signal.set(value);
    }
}

#[component]
pub fn NowPlaying() -> impl IntoView {
    let nav = expect_context::<Nav>();
    let toaster = expect_context::<Toaster>();

    let phase = RwSignal::new(Phase::Unknown);
    let title = RwSignal::new(String::new());
    let elapsed = RwSignal::new(0u64);
    let total = RwSignal::new(0u64);
    let paused = RwSignal::new(false);
    let file = RwSignal::new(None::<String>);
    let detail = RwSignal::new(String::new());
    let on_ui = RwSignal::new(false);

    let refresh = move || {
        spawn_local(async move {
            let Ok(system) = api::typed::<SystemStatus>(api::status()).await else {
                return;
            };
            let kodi = system.kodi;
            put(
                on_ui,
                system.surface.as_ref().is_some_and(|s| s.active == "ui"),
            );
            put(
                phase,
                if !kodi.jsonrpc_reachable {
                    Phase::Offline
                } else if kodi.state == "playing" || kodi.state == "paused" {
                    Phase::Playing
                } else {
                    Phase::Idle
                },
            );
            put(
                detail,
                kodi.error
                    .clone()
                    .unwrap_or_else(|| "Kodi şu anda çalışmıyor.".into()),
            );
            put(title, kodi.title().unwrap_or_else(|| "Oynatılıyor".into()));
            put(elapsed, kodi.time.unwrap_or_default().total_seconds());
            put(total, kodi.total_time.unwrap_or_default().total_seconds());
            put(paused, kodi.speed == Some(0));
            put(
                file,
                kodi.item
                    .as_ref()
                    .and_then(|item| item.get("file"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
            );
        });
    };
    refresh();
    let handle = set_interval_with_handle(refresh, POLL).ok();
    on_cleanup(move || {
        if let Some(handle) = handle {
            handle.clear();
        }
    });

    let send = move |request: serde_json::Value, failure: &'static str| {
        spawn_local(async move {
            match api::control(request).await {
                Ok(_) => refresh(),
                Err(error) => toaster.warn(format!("{failure} — {}", error.message)),
            }
        });
    };

    let fraction = Signal::derive(move || {
        let (elapsed, total) = (elapsed.get() as f64, total.get() as f64);
        if total > 0.0 {
            (elapsed / total).clamp(0.0, 1.0)
        } else {
            0.0
        }
    });

    view! {
        <div class="now">
            {move || match phase.get() {
                Phase::Unknown => {
                    view! { <div class="state"><strong>"Durum okunuyor…"</strong></div> }.into_any()
                }
                Phase::Offline | Phase::Idle => {
                    let offline = phase.get() == Phase::Offline;
                    view! {
                        <div class="state">
                            <strong>
                                {if offline {
                                    "Televizyonda oynatma yok"
                                } else {
                                    "Kodi hazır, oynatma yok"
                                }}
                            </strong>
                            <p>
                                {move || {
                                    // Not the control plane's own words. What
                                    // stood here was the daemon's error string,
                                    // JSON-RPC URL and all, which is a thing to
                                    // put in diagnostics and not on a
                                    // television: "Kodi kullanılamıyor: error
                                    // sending request for url
                                    // (http://127.0.0.1:8080/jsonrpc)" told the
                                    // person in front of it nothing except that
                                    // something was unfinished. The reason is
                                    // still read, and still shown — under
                                    // Ayarlar › Tanılama, where a reason
                                    // belongs.
                                    if offline {
                                        "Şu anda oynatılan bir şey yok.".to_string()
                                    } else {
                                        "Bir başlık seçip \"Kodi'de Oynat\" deyin.".to_string()
                                    }
                                }}
                            </p>
                            <p>
                                {move || {
                                    if on_ui.get() {
                                        "Ekranı şu anda MediaBox arayüzü kullanıyor."
                                    } else {
                                        "Bir başlık seçtiğinizde Kodi otomatik başlatılır."
                                    }
                                }}
                            </p>
                            <div class="transport">
                                <Action
                                    label="Ana Sayfa"
                                    variant="primary"
                                    autofocus=true
                                    on_press=Callback::new(move |()| nav.go(Route::Home))
                                />
                                <Action
                                    label="Televizyonda Arayüzü Göster"
                                    disabled=Signal::derive(move || on_ui.get())
                                    on_press=Callback::new(move |()| {
                                        send(api::surface_switch("ui"), "Ekran devralınamadı")
                                    })
                                />
                            </div>
                        </div>
                    }
                        .into_any()
                }
                Phase::Playing => {
                    view! {
                        <h1 class="now-title">{move || title.get()}</h1>
                        <div class="now-sub">
                            {move || {
                                if paused.get() { "Duraklatıldı" } else { "Oynatılıyor" }
                            }}
                        </div>
                        <div class="progress">
                            <div class="progress-track">
                                <i
                                    class="progress-fill"
                                    style=move || {
                                        format!("width:{:.2}%", fraction.get() * 100.0)
                                    }
                                ></i>
                            </div>
                            <div class="progress-times">
                                <span>{move || seconds_to_clock(elapsed.get())}</span>
                                <span>{move || seconds_to_clock(total.get())}</span>
                            </div>
                        </div>
                        <div class="transport">
                            <Action
                                label="10 sn geri"
                                on_press=Callback::new(move |()| {
                                    send(api::kodi_seek(-10), "Geri sarılamadı")
                                })
                            />
                            <Action
                                label=Signal::derive(move || {
                                    if paused.get() { "Devam Et" } else { "Duraklat" }.to_string()
                                })
                                variant="primary"
                                autofocus=true
                                on_press=Callback::new(move |()| {
                                    send(api::kodi_play_pause(), "Duraklatılamadı")
                                })
                            />
                            <Action
                                label="30 sn ileri"
                                on_press=Callback::new(move |()| {
                                    send(api::kodi_seek(30), "İleri sarılamadı")
                                })
                            />
                            <Action
                                label="Durdur"
                                on_press=Callback::new(move |()| {
                                    send(api::kodi_stop(), "Durdurulamadı")
                                })
                            />
                            <Action
                                label="Televizyonda Arayüze Dön"
                                on_press=Callback::new(move |()| {
                                    send(api::surface_switch("ui"), "Ekran devralınamadı")
                                })
                            />
                        </div>
                        <div class="rows">
                            <div class="row">
                                <span class="row-label">"Oynatma yolu"</span>
                                <span class="row-value">{move || playback_path(file.get()).0}</span>
                            </div>
                            <div class="row">
                                <span class="row-label">"Açıklama"</span>
                                <span class="row-value">{move || playback_path(file.get()).1}</span>
                            </div>
                            <div class="row">
                                <span class="row-label">"Kaynak"</span>
                                <span class="row-value">
                                    {move || file.get().unwrap_or_else(|| "—".into())}
                                </span>
                            </div>
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

/// How this particular playback is being carried.
///
/// A session URL served by the media core means the audio is being converted
/// or the container repacked; a source URL means Kodi opened the file itself
/// and nothing is in the path.
fn playback_path(file: Option<String>) -> (&'static str, &'static str) {
    match file.as_deref() {
        Some(url) if url.contains("/media/session/") => (
            "Medya çekirdeği üzerinden",
            "Video kopyalanıyor; ses dönüştürülüyor veya kapsayıcı yeniden paketleniyor.",
        ),
        Some(_) => (
            "Doğrudan oynatma",
            "Kodi kaynağı doğrudan açtı; medya çekirdeği yolda değil.",
        ),
        None => ("Bilinmiyor", "—"),
    }
}
