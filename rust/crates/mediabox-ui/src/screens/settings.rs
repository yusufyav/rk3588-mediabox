//! Settings, and the diagnostics that belong behind them.
//!
//! Diagnostics live here and nowhere else. A media appliance whose first
//! screen is a health dashboard has confused the operator with the viewer, so
//! everything numeric is one deliberate step away from the content.
//!
//! Nothing on these panels is a guess: capability values come from the profile
//! the media core is actually deciding with, and vitals come from the kernel
//! through the control plane. Where a subsystem is genuinely absent, the panel
//! says it is absent rather than drawing an empty table.

use crate::api;
use crate::app::Toaster;
use crate::components::{Action, Failure, Field, Keyboard, Load, Row, human_size, seconds_to_clock};
use crate::model::SystemStatus;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

const TABS: [(&str, &str); 9] = [
    ("account", "Hesap"),
    ("playback", "Oynatma"),
    ("network", "Ağ"),
    ("bluetooth", "Bluetooth"),
    ("cec", "HDMI / CEC"),
    ("display", "Ekran"),
    ("audio", "Ses"),
    ("system", "Sistem"),
    ("diagnostics", "Tanılama"),
];

/// The Stremio account, and what it is for.
///
/// Without one the box uses Stremio's default addon collection, which supplies
/// artwork and descriptions and almost no streams: the catalogue looks full
/// and nearly every title answers "no source". Signing in brings the account's
/// own addons, which are where streams actually come from. The password is
/// typed on the remote's keyboard, sent once over loopback to the media core,
/// and is not stored — what the core keeps is the auth key the login returns.
#[component]
fn Account() -> impl IntoView {
    let toaster = expect_context::<Toaster>();
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    // Which field the keys are building. Two fields and one keyboard, because
    // a remote has one focus and a second keyboard would just be more to walk
    // past.
    let editing = RwSignal::new(0u8);
    let busy = RwSignal::new(false);
    let session = RwSignal::new(None::<Value>);

    let refresh = move || {
        spawn_local(async move {
            if let Ok(status) = api::control(api::status()).await {
                session.set(status.pointer("/media/provider").cloned());
            }
        });
    };
    refresh();

    let signed_in = move || {
        session.with(|found| {
            found
                .as_ref()
                .and_then(|provider| provider.get("authenticated"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
    };
    let who = move || {
        session.with(|found| {
            found
                .as_ref()
                .and_then(|provider| provider.get("email"))
                .and_then(Value::as_str)
                .unwrap_or("—")
                .to_string()
        })
    };
    let addons = move || {
        session.with(|found| {
            found
                .as_ref()
                .and_then(|provider| provider.get("addonCount"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        })
    };

    let sign_in = Callback::new(move |()| {
        let (address, secret) = (email.get_untracked(), password.get_untracked());
        if address.trim().is_empty() || secret.is_empty() || busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::control(api::media_login(address.trim(), &secret)).await {
                Ok(_) => {
                    // Whatever happens next, the password does not stay in the
                    // page.
                    password.set(String::new());
                    toaster.say("Stremio hesabı bağlandı".to_string());
                    refresh();
                }
                Err(error) => toaster.warn(format!("Giriş başarısız: {}", error.message)),
            }
            busy.set(false);
        });
    });

    let sign_out = Callback::new(move |()| {
        busy.set(true);
        spawn_local(async move {
            match api::control(api::media_logout()).await {
                Ok(_) => {
                    toaster.say("Hesap bağlantısı kesildi".to_string());
                    refresh();
                }
                Err(error) => toaster.warn(format!("Çıkış başarısız: {}", error.message)),
            }
            busy.set(false);
        });
    });

    view! {
        <div class="panel">
            <h2>"Stremio hesabı"</h2>
            <p class="panel-note">
                "Hesap bağlı değilken cihaz Stremio'nun varsayılan eklenti koleksiyonunu \
                 kullanır: afiş ve özet gelir, akış çoğu başlıkta gelmez. Kendi hesabınızı \
                 bağladığınızda sizin eklentileriniz devreye girer."
            </p>
            // `Row` takes plain strings, so the whole block is what re-runs
            // when the session changes rather than each value on its own.
            {move || {
                view! {
                    <div class="rows">
                        <Row
                            label="Durum"
                            value=if signed_in() { "Bağlı" } else { "Bağlı değil" }
                            tone=if signed_in() { "ok" } else { "warn" }
                        />
                        <Row label="Hesap" value=who() />
                        <Row label="Eklenti sayısı" value=addons().to_string() />
                    </div>
                }
            }}

            {move || {
                if signed_in() {
                    view! {
                        <div class="actions-row">
                            <Action
                                label="Çıkış yap"
                                variant="ghost"
                                disabled=Signal::derive(move || busy.get())
                                on_press=sign_out
                            />
                        </div>
                    }
                        .into_any()
                } else {
                    view! {
                        <div class="keyboard">
                            <div class="fields" data-row="1">
                                <Field
                                    label="E-posta"
                                    value=email
                                    key="field:email"
                                    autofocus=true
                                    placeholder="ornek@eposta.com"
                                    on_focus=Callback::new(move |()| editing.set(0))
                                    on_enter=sign_in
                                />
                                <Field
                                    label="Parola"
                                    value=password
                                    key="field:password"
                                    secret=true
                                    on_focus=Callback::new(move |()| editing.set(1))
                                    on_enter=sign_in
                                />
                            </div>
                            {move || {
                                if editing.get() == 0 {
                                    view! { <Keyboard value=email /> }.into_any()
                                } else {
                                    view! { <Keyboard value=password /> }.into_any()
                                }
                            }}
                            <div class="keys wide" data-row="1">
                                <button
                                    class="key wide"
                                    data-focus="1"
                                    data-focus-key="key:back"
                                    tabindex="-1"
                                    on:click=move |_| {
                                        if editing.get() == 0 {
                                            email.update(|value| { value.pop(); });
                                        } else {
                                            password.update(|value| { value.pop(); });
                                        }
                                    }
                                >
                                    "Sil"
                                </button>
                                <button
                                    class="key wide"
                                    data-focus="1"
                                    data-focus-key="key:clear"
                                    tabindex="-1"
                                    on:click=move |_| {
                                        if editing.get() == 0 {
                                            email.set(String::new());
                                        } else {
                                            password.set(String::new());
                                        }
                                    }
                                >
                                    "Temizle"
                                </button>
                                <Action
                                    label=move || {
                                        if busy.get() {
                                            "Bağlanıyor…".to_string()
                                        } else {
                                            "Giriş yap".to_string()
                                        }
                                    }
                                    variant="primary"
                                    disabled=Signal::derive(move || {
                                        busy.get() || email.get().trim().is_empty()
                                            || password.get().is_empty()
                                    })
                                    on_press=sign_in
                                />
                            </div>
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

#[component]
pub fn Settings() -> impl IntoView {
    let tab = RwSignal::new("playback".to_string());
    let status = RwSignal::new(Load::<SystemStatus>::Loading);
    let caps = RwSignal::new(None::<Value>);
    let vitals = RwSignal::new(None::<Value>);

    /// Write only on a real change: an identical value would still invalidate
    /// every reader and rebuild the panel for nothing.
    fn put<T: PartialEq + Send + Sync + 'static>(signal: RwSignal<T>, value: T) {
        if signal.with_untracked(|current| current != &value) {
            signal.set(value);
        }
    }

    let read_status = move || {
        spawn_local(async move {
            match api::typed::<SystemStatus>(api::status()).await {
                Ok(found) => put(status, Load::Ready(found)),
                Err(error) => put(status, Load::Failed(error.message)),
            }
        });
    };
    read_status();
    spawn_local(async move {
        if let Ok(found) = api::control(api::media_capabilities()).await {
            put(caps, Some(found));
        }
    });

    // Only the diagnostics panel shows anything that moves, and it is the only
    // panel with no controls on it. Polling while another panel is open would
    // rebuild that panel around whatever button the remote was sitting on, so
    // the interval exists exactly where it is both needed and harmless.
    Effect::new(move |previous: Option<Option<IntervalHandle>>| {
        if let Some(Some(handle)) = previous {
            handle.clear();
        }
        if tab.get() != "diagnostics" {
            return None;
        }
        let poll = move || {
            read_status();
            spawn_local(async move {
                if let Ok(found) = api::control(api::diagnostics()).await {
                    put(vitals, Some(found));
                }
            });
        };
        poll();
        set_interval_with_handle(poll, std::time::Duration::from_secs(5)).ok()
    });

    view! {
        <div class="settings">
            <nav class="settings-nav">
                {TABS
                    .into_iter()
                    .map(|(key, label)| {
                        let current = move || tab.get() == key;
                        view! {
                            <button
                                class="settings-tab"
                                data-focus="1"
                                tabindex="-1"
                                aria-current=move || current().then_some("true")
                                on:click=move |_| tab.set(key.to_string())
                            >
                                {label}
                            </button>
                        }
                    })
                    .collect_view()}
            </nav>
            <div class="panel">
                {move || match status.get() {
                    Load::Loading => {
                        view! { <div class="state"><strong>"Okunuyor…"</strong></div> }.into_any()
                    }
                    Load::Failed(detail) => {
                        view! { <Failure title="Durum alınamadı" detail=detail /> }.into_any()
                    }
                    Load::Ready(system) => {
                        let caps = caps.get();
                        let vitals = vitals.get();
                        match tab.get().as_str() {
                            "playback" => view! { <Playback caps=caps /> }.into_any(),
                            "network" => {
                                view! { <Network system=system vitals=vitals /> }.into_any()
                            }
                            "bluetooth" => view! { <Bluetooth system=system /> }.into_any(),
                            "cec" => view! { <Cec system=system /> }.into_any(),
                            "display" => view! { <Display vitals=vitals /> }.into_any(),
                            "audio" => view! { <Audio caps=caps /> }.into_any(),
                            "account" => view! { <Account /> }.into_any(),
                            "system" => view! { <SystemPanel system=system /> }.into_any(),
                            _ => {
                                view! { <Diagnostics system=system vitals=vitals /> }.into_any()
                            }
                        }
                    }
                }}
            </div>
        </div>
    }
}

// ------------------------------------------------------------------ helpers

fn text(value: &Option<Value>, pointer: &str) -> String {
    value
        .as_ref()
        .and_then(|root| root.pointer(pointer))
        .map(|found| match found {
            Value::String(text) => text.clone(),
            Value::Array(items) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
                .join(", "),
            other => other.to_string(),
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "—".to_string())
}

fn yes_no(value: bool) -> (&'static str, &'static str) {
    if value {
        ("Evet", "ok")
    } else {
        ("Hayır", "bad")
    }
}

fn missing(title: &str, detail: &str) -> impl IntoView {
    view! {
        <div class="state">
            <strong>{title.to_string()}</strong>
            <p>{detail.to_string()}</p>
        </div>
    }
}

// ------------------------------------------------------------------- panels

#[component]
fn Playback(caps: Option<Value>) -> impl IntoView {
    if caps.is_none() {
        return missing(
            "Yetenek profili okunamadı",
            "Medya çekirdeği yanıt vermedi; oynatma kararları gösterilemiyor.",
        )
        .into_any();
    }
    // The profile answers with a JSON boolean, and printing it raw put the
    // word "false" on the television in red. A setting says whether it is on.
    let dv_pipeline = match text(&caps, "/video/dolbyVisionPipeline").as_str() {
        "true" => "Var".to_string(),
        "false" => "Yok".to_string(),
        other => other.to_string(),
    };
    let max = format!(
        "{}×{} @ {} fps",
        text(&caps, "/video/maxWidth"),
        text(&caps, "/video/maxHeight"),
        text(&caps, "/video/maxFps")
    );
    view! {
        <h2>"Oynatma"</h2>
        <p class="panel-note">
            "Bu kart için kabul edilmiş yetenek profili. Her kaynak kararı bu değerlere göre \
             verilir; arayüz bunları yorumlamaz, yalnızca gösterir."
        </p>
        <div class="rows">
            <Row label="Profil" value=text(&caps, "/name") />
            <Row label="Donanım video kodekleri" value=text(&caps, "/video/codecs") />
            <Row label="Azami çözünürlük" value=max />
            <Row label="Bit derinliği" value=text(&caps, "/video/bitDepths") />
            <Row label="HDR biçimleri" value=text(&caps, "/video/hdrFormats") />
            <Row label="Dolby Vision işlem hattı" value=dv_pipeline tone="bad" />
            <Row label="Doğrudan açılan kapsayıcılar" value=text(&caps, "/container/direct") />
            <Row label="Yeniden paketleme hedefi" value=text(&caps, "/container/remuxTarget") />
            <Row label="Yazılımsal video dönüştürme" value="Kapalı" tone="ok" />
        </div>
        <p class="panel-note">
            "Kartta donanım video kodlayıcı yok. 4K yazılımsal video dönüştürme kalıcı olarak \
             kapalıdır; gerekirse yalnız ses dönüştürülür, video her zaman kopyalanır."
        </p>
    }
    .into_any()
}

#[component]
fn Network(system: SystemStatus, vitals: Option<Value>) -> impl IntoView {
    let origin = web_sys::window()
        .and_then(|window| window.location().host().ok())
        .unwrap_or_default();
    let torrent = system
        .media
        .pointer("/torrentNetwork/status")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_string();
    let torrent_tone = if torrent == "TORRENT_NETWORK_BLOCKED" { "bad" } else { "ok" };
    let server_ok = system
        .media
        .pointer("/provider/streamingServer/reachable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (server_text, server_tone) = yes_no(server_ok);
    let server_version = text(&Some(system.media.clone()), "/provider/streamingServer/version");
    let interfaces = vitals
        .as_ref()
        .and_then(|root| root.pointer("/network/interfaces"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    view! {
        <h2>"Ağ"</h2>
        <div class="rows">
            <Row label="Bu arayüzün adresi" value=origin />
            <Row label="Varsayılan arayüz" value=text(&vitals, "/network/default/interface") />
            <Row label="Ağ geçidi" value=text(&vitals, "/network/default/gateway") />
            <Row label="Akış sunucusu" value=server_text tone=server_tone />
            <Row label="Akış sunucusu sürümü" value=server_version />
            <Row label="Torrent ağı" value=torrent.clone() tone=torrent_tone />
            {interfaces
                .into_iter()
                .map(|interface| {
                    let name = interface
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string();
                    let state = interface
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string();
                    let tone = if state == "up" { "ok" } else { "" };
                    view! { <Row label=name value=state tone=tone /> }
                })
                .collect_view()}
        </div>
        {(torrent == "TORRENT_NETWORK_BLOCKED")
            .then(|| {
                view! {
                    <p class="panel-note">
                        "Bu ağda eş bağlantıları engelleniyor. Torrent kaynaklarının üst verisi \
                         çözümlenebilir fakat parça aktarımı gerçekleşmez; bu kaynaklar kaynak \
                         listesinde açıkça işaretlenir."
                    </p>
                }
            })}
    }
}

#[component]
fn Bluetooth(system: SystemStatus) -> impl IntoView {
    let devices: Vec<Value> = system
        .input_devices
        .iter()
        .filter(|device| {
            device.get("source").and_then(Value::as_str) == Some("bluetooth_hid")
        })
        .cloned()
        .collect();
    view! {
        <h2>"Bluetooth"</h2>
        <p class="panel-note">
            "MediaBox Bluetooth giriş aygıtlarını çekirdeğin HID katmanından görür. \
             Eşleştirme cihazın kendi Bluetooth yığınıyla yapılır; bu panel yalnız \
             MediaBox'ın gerçekten gördüğü aygıtları listeler."
        </p>
        {if devices.is_empty() {
            view! {
                <div class="state">
                    <strong>"Bağlı Bluetooth giriş aygıtı yok"</strong>
                    <p>"Şu anda HID olarak görünen bir Bluetooth kumanda veya klavye yok."</p>
                </div>
            }
                .into_any()
        } else {
            view! {
                <div class="rows">
                    {devices
                        .into_iter()
                        .map(|device| {
                            let name = device
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_string();
                            let class = device
                                .get("class")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_string();
                            view! { <Row label=name value=class /> }
                        })
                        .collect_view()}
                </div>
            }
                .into_any()
        }}
    }
}

#[component]
fn Cec(system: SystemStatus) -> impl IntoView {
    let toaster = expect_context::<Toaster>();
    let cec = system.cec.clone();
    let (available, tone) = yes_no(cec.available);
    let logical = cec
        .logical_addresses
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let cec_available = cec.available;
    let known = cec.known_devices.len().to_string();
    let send = move |request: serde_json::Value, ok: &'static str| {
        spawn_local(async move {
            match api::control(request).await {
                Ok(_) => toaster.say(ok),
                Err(error) => toaster.warn(format!("CEC komutu başarısız — {}", error.message)),
            }
        });
    };
    view! {
        <h2>"HDMI / CEC"</h2>
        <div class="rows">
            <Row label="CEC kullanılabilir" value=available tone=tone />
            <Row label="Adaptör" value=cec.adapter.clone().unwrap_or_else(|| "—".into()) />
            <Row label="Sürücü" value=cec.driver.clone().unwrap_or_else(|| "—".into()) />
            <Row
                label="Fiziksel adres"
                value=cec.physical_address.clone().unwrap_or_else(|| "—".into())
            />
            <Row label="Mantıksal adresler" value=logical />
            <Row label="Bilinen cihaz" value=known />
        </div>
        {cec
            .error
            .clone()
            .map(|error| view! { <p class="panel-note">{error}</p> })}
        <div class="actions-row">
            <Action
                label="Televizyonu Uyandır"
                variant="primary"
                disabled=Signal::derive(move || !cec_available)
                on_press=Callback::new(move |()| {
                    send(api::cec_wake_tv(), "Uyandırma komutu gönderildi.")
                })
            />
            <Action
                label="Televizyonu Beklemeye Al"
                disabled=Signal::derive(move || !cec_available)
                on_press=Callback::new(move |()| {
                    send(api::cec_standby_tv(), "Bekleme komutu gönderildi.")
                })
            />
        </div>
    }
}

#[component]
fn Display(vitals: Option<Value>) -> impl IntoView {
    let connectors = vitals
        .as_ref()
        .and_then(|root| root.get("drm"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    view! {
        <h2>"Ekran"</h2>
        <p class="panel-note">
            "Bağlayıcı durumu doğrudan DRM/KMS'ten okunur. Kodi ve MediaBox arayüzü aynı \
             ekranı paylaşır; aynı anda yalnız biri DRM master olabilir."
        </p>
        {if connectors.is_empty() {
            missing("DRM bilgisi okunamadı", "Çekirdek bağlayıcı listesi görülemedi.").into_any()
        } else {
            view! {
                <div class="rows">
                    {connectors
                        .into_iter()
                        .map(|connector| {
                            let name = connector
                                .get("connector")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_string();
                            let status = connector
                                .get("status")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_string();
                            let mode = connector
                                .get("mode")
                                .and_then(Value::as_str)
                                .unwrap_or("—")
                                .to_string();
                            let tone = if status == "connected" { "ok" } else { "" };
                            view! {
                                <Row label=name value=format!("{status} · {mode}") tone=tone />
                            }
                        })
                        .collect_view()}
                </div>
            }
                .into_any()
        }}
    }
}

#[component]
fn Audio(caps: Option<Value>) -> impl IntoView {
    if caps.is_none() {
        return missing(
            "Ses profili okunamadı",
            "Medya çekirdeği yanıt vermedi.",
        )
        .into_any();
    }
    view! {
        <h2>"Ses"</h2>
        <div class="rows">
            <Row label="Bit-perfect iletim" value=text(&caps, "/audio/passthroughCodecs") />
            <Row label="Çözülen kodekler" value=text(&caps, "/audio/decodeCodecs") />
            <Row label="Azami PCM kanalı" value=text(&caps, "/audio/maxPcmChannels") />
            <Row label="Azami iletim kanalı" value=text(&caps, "/audio/maxPassthroughChannels") />
            <Row label="Dönüştürme hedefi" value=text(&caps, "/audio/transcodeTarget") />
            <Row label="Dönüştürme kanal sınırı" value=text(&caps, "/audio/transcodeMaxChannels") />
        </div>
        <p class="panel-note">
            "Alıcının çözemediği bir ses akışı AC-3 5.1'e dönüştürülür. Bu durumda Atmos veya \
             kayıpsız katman korunmaz ve kaynak ekranında bu açıkça belirtilir. Video hiçbir \
             durumda yeniden kodlanmaz."
        </p>
    }
    .into_any()
}

#[component]
fn SystemPanel(system: SystemStatus) -> impl IntoView {
    let surface = system
        .surface
        .as_ref()
        .map(|surface| match surface.active.as_str() {
            "kodi" => "Kodi".to_string(),
            "ui" => "MediaBox arayüzü".to_string(),
            _ => "Boşta".to_string(),
        })
        .unwrap_or_else(|| "—".to_string());
    view! {
        <h2>"Sistem"</h2>
        <div class="rows">
            <Row label="Cihaz" value=system.hostname.clone() />
            <Row label="Çekirdek" value=system.kernel.clone() />
            <Row label="Mimari" value=system.architecture.clone() />
            <Row label="Denetim düzlemi" value=format!("mediaboxd-rs {}", system.version) />
            <Row label="Çalışma süresi" value=seconds_to_clock(system.uptime_seconds) />
            <Row label="Ekranı kullanan" value=surface />
            <Row label="Giriş kipi" value=system.input_mode.clone() />
            <Row label="Giriş aygıtı" value=system.input_devices.len().to_string() />
        </div>
        <h2>"Servisler"</h2>
        <div class="rows">
            {system
                .services
                .iter()
                .map(|service| {
                    let (value, tone) = yes_no(service.healthy);
                    let value = service
                        .detail
                        .clone()
                        .filter(|_| !service.healthy)
                        .unwrap_or_else(|| value.to_string());
                    view! { <Row label=service.name.clone() value=value tone=tone /> }
                })
                .collect_view()}
        </div>
    }
}

#[component]
fn Diagnostics(system: SystemStatus, vitals: Option<Value>) -> impl IntoView {
    let memory_used = vitals
        .as_ref()
        .and_then(|root| root.pointer("/memory/usedBytes"))
        .and_then(Value::as_f64);
    let memory_total = vitals
        .as_ref()
        .and_then(|root| root.pointer("/memory/totalBytes"))
        .and_then(Value::as_f64);
    let storage = vitals
        .as_ref()
        .and_then(|root| root.get("storage"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let temperatures = vitals
        .as_ref()
        .and_then(|root| root.get("temperatures"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ffmpeg = vitals
        .as_ref()
        .and_then(|root| root.get("ffmpeg"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let sessions = system
        .media
        .get("sessions")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cpu_count = vitals
        .as_ref()
        .and_then(|root| root.pointer("/cpu/count"))
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
        .max(1.0);
    let load_one = vitals
        .as_ref()
        .and_then(|root| root.pointer("/cpu/load/one"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);

    let memory_text = match (memory_used, memory_total) {
        (Some(used), Some(total)) => format!("{} / {}", human_size(used), human_size(total)),
        _ => "—".to_string(),
    };
    let memory_fraction = match (memory_used, memory_total) {
        (Some(used), Some(total)) if total > 0.0 => used / total,
        _ => 0.0,
    };
    let connected_drm = vitals
        .as_ref()
        .and_then(|root| root.get("drm"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|c| c.get("status").and_then(Value::as_str) == Some("connected"))
                .filter_map(|c| c.get("connector").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "—".into());
    let profile_name = system
        .media
        .get("capabilityProfile")
        .and_then(Value::as_str)
        .unwrap_or("—")
        .to_string();
    let load_meter = load_one / cpu_count;
    let load_text = format!("{load_one:.2}");
    let cpu_label = format!("İşlemci yükü ({} çekirdek)", cpu_count as u64);
    let ffmpeg_tone = if ffmpeg > sessions as usize { "warn" } else { "ok" };
    let (kodi_text, kodi_tone) = yes_no(system.kodi.jsonrpc_reachable);
    let (cec_text, cec_tone) = yes_no(system.cec.available);
    let media_ok = system.media.get("available").and_then(Value::as_bool) != Some(false);
    let (media_text, media_tone) = yes_no(media_ok);
    // A running encoder with no session behind it is the leak worth naming.
    let orphaned = ffmpeg > sessions as usize;

    view! {
        <h2>"Tanılama"</h2>
        <div class="rows">
            <Row label=cpu_label value=load_text meter=load_meter />
            <Row label="Bellek" value=memory_text meter=memory_fraction />
            {storage
                .into_iter()
                .map(|volume| {
                    let path = volume
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string();
                    let used = volume.get("usedBytes").and_then(Value::as_f64).unwrap_or(0.0);
                    let total = volume.get("totalBytes").and_then(Value::as_f64).unwrap_or(0.0);
                    let fraction = if total > 0.0 { used / total } else { 0.0 };
                    view! {
                        <Row
                            label=format!("Depolama {path}")
                            value=format!("{} / {}", human_size(used), human_size(total))
                            meter=fraction
                        />
                    }
                })
                .collect_view()}
            {temperatures
                .into_iter()
                .map(|zone| {
                    let name = zone.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
                    let celsius = zone.get("celsius").and_then(Value::as_f64).unwrap_or(0.0);
                    let tone = if celsius >= 85.0 {
                        "bad"
                    } else if celsius >= 70.0 {
                        "warn"
                    } else {
                        "ok"
                    };
                    view! {
                        <Row label=format!("Sıcaklık · {name}") value=format!("{celsius:.1} °C") tone=tone />
                    }
                })
                .collect_view()}
        </div>

        <h2>"Alt sistemler"</h2>
        <div class="rows">
            <Row label="Çekirdek" value=system.kernel.clone() />
            <Row label="Varsayılan ağ arayüzü" value=text(&vitals, "/network/default/interface") />
            <Row label="DRM bağlayıcı" value=connected_drm />
            <Row label="Kodi JSON-RPC" value=kodi_text tone=kodi_tone />
            <Row label="CEC" value=cec_text tone=cec_tone />
            <Row label="Medya çalışanı" value=media_text tone=media_tone />
            <Row label="Medya oturumu" value=sessions.to_string() />
            <Row label="Çalışan ffmpeg süreci" value=ffmpeg.to_string() tone=ffmpeg_tone />
            <Row label="Yetenek profili" value=profile_name />
        </div>
        {orphaned
            .then(|| {
                view! {
                    <p class="panel-note">
                        "Kayıtlı oturumdan daha fazla ffmpeg süreci var. Bu, sahipsiz kalmış \
                         bir kodlayıcıya işaret eder."
                    </p>
                }
            })}
        {system
            .kodi
            .error
            .clone()
            .map(|error| view! { <p class="panel-note">{format!("Kodi: {error}")}</p> })}
    }
}
