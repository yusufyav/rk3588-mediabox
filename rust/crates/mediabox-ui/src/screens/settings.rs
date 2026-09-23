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
use crate::components::{
    Action, Failure, Field, Keyboard, Load, Row, human_size, seconds_to_clock,
};
use crate::model::SystemStatus;
use leptos::prelude::*;
use leptos::task::spawn_local;
use mediabox_core::{
    FAN_CURVE_MAX_POINTS, FAN_CURVE_MIN_POINTS, FAN_PWM_MAX, FAN_TEMP_MAX_C, FanBoardFix, FanCurve,
    FanProfile, FanStatus, fan_pwm_allowed, fan_pwm_from_percent, fan_pwm_percent,
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;

const TABS: [(&str, &str); 10] = [
    ("account", "Hesap"),
    ("playback", "Oynatma"),
    ("network", "Ağ"),
    ("bluetooth", "Bluetooth"),
    ("cec", "HDMI / CEC"),
    ("display", "Ekran"),
    ("audio", "Ses"),
    ("cooling", "Soğutma"),
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
    let fan = RwSignal::new(None::<FanStatus>);

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
    //
    // The cooling panel is the one exception, and it is shaped for it: the
    // temperature and the duty are live, and they are the only things the
    // poll writes. They are drawn as text that updates in place, never as
    // part of the block the buttons live in, so the remote stays where it is.
    Effect::new(move |previous: Option<Option<IntervalHandle>>| {
        if let Some(Some(handle)) = previous {
            handle.clear();
        }
        if tab.get() == "cooling" {
            let poll = move || {
                spawn_local(async move {
                    if let Ok(found) = api::typed::<FanStatus>(api::fan_status()).await {
                        put(fan, Some(found));
                    }
                });
            };
            poll();
            return set_interval_with_handle(poll, std::time::Duration::from_secs(5)).ok();
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
                            "cooling" => view! { <Cooling fan=fan /> }.into_any(),
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
    let torrent_tone = if torrent == "TORRENT_NETWORK_BLOCKED" {
        "bad"
    } else {
        "ok"
    };
    let server_ok = system
        .media
        .pointer("/provider/streamingServer/reachable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (server_text, server_tone) = yes_no(server_ok);
    let server_version = text(
        &Some(system.media.clone()),
        "/provider/streamingServer/version",
    );
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
        .filter(|device| device.get("source").and_then(Value::as_str) == Some("bluetooth_hid"))
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
        <ColorModes />
    }
}

/// The colour mode the television is sent, and the list it may be chosen from.
///
/// The list is not a fixed menu. It is computed from the sink's own EDID and
/// the mode in question, because those two together decide what the link can
/// actually carry -- and they are not the same on two sockets of one set. A
/// Sony KD-65XE9005 reports HDMI 1 as a 300 MHz port and HDMI 3 as a 600 MHz
/// one: at 4K60 the first of those can be sent nothing but YCbCr420 8-bit,
/// which is why an "HDR" badge over that picture would be a lie, and the panel
/// says so on the row rather than offering the choice.
///
/// `Otomatik` is the answer nobody has to think about: the deepest colour the
/// link can carry when there is HDR to carry, the fullest chroma otherwise. A
/// fixed choice is here because a sink occasionally behaves better on something
/// other than the measured best, and the person watching is the one who can see
/// that.
#[component]
fn ColorModes() -> impl IntoView {
    let toaster = expect_context::<Toaster>();
    let state = RwSignal::new(None::<Value>);
    let busy = RwSignal::new(false);

    let refresh = move || {
        spawn_local(async move {
            match api::control(api::display_color_modes()).await {
                Ok(value) => state.set(Some(value)),
                Err(error) => state.set(Some(json!({
                    "available": false,
                    "error": error.to_string(),
                }))),
            }
        });
    };
    refresh();

    let choose = move |format: Option<(&'static str, u8)>| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            let result = api::control(api::display_color_mode_set(format)).await;
            busy.set(false);
            match result {
                Ok(value) => {
                    state.set(Some(value));
                    toaster.say("Renk modu kaydedildi");
                }
                Err(error) => toaster.warn(format!("Renk modu ayarlanamadı: {error}")),
            }
        });
    };

    view! {
        <h3>"Renk modu"</h3>
        <p class="panel-note">
            "Liste televizyonun EDID'inden ölçülür: her mod için bağlantının taşıyabildiği \
             biçimler. Aynı televizyonun iki girişi aynı cevabı vermez — bir giriş 4K60'ta \
             yalnız 8 bit taşıyorsa orada HDR sunulmaz, çünkü 8 bitlik HDR bant yapar."
        </p>
        {move || {
            let Some(value) = state.get() else {
                return view! { <div class="state"><strong>"Okunuyor…"</strong></div> }
                    .into_any();
            };
            if !value.get("available").and_then(Value::as_bool).unwrap_or(false) {
                let why = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Bağlı bir ekran yok.")
                    .to_string();
                return missing("Renk modu okunamadı", &why).into_any();
            }
            let current = value
                .pointer("/choice/kind")
                .and_then(Value::as_str)
                .unwrap_or("auto")
                .to_string();
            let current_format = value
                .pointer("/choice/format")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let current_bits = value
                .pointer("/choice/bits")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u8;
            let connector = value
                .get("connector")
                .and_then(Value::as_str)
                .unwrap_or("—")
                .to_string();
            let sink = value
                .get("sink")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let ceiling = value
                .get("max_character_rate_khz")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let declared = value
                .get("rate_is_declared")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let timings = value
                .get("timings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            // Every format/depth pair that any listed timing can carry. A
            // person chooses one setting, not one per mode, so the choices are
            // the union -- and each row then says where it applies.
            let mut choices: Vec<(String, String, u8)> = Vec::new();
            for timing in &timings {
                for option in timing.get("allowed").and_then(Value::as_array).into_iter().flatten() {
                    let label = option.get("label").and_then(Value::as_str).unwrap_or("").to_string();
                    let format = option
                        .pointer("/mode/format")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let bits = option.pointer("/mode/bits").and_then(Value::as_u64).unwrap_or(0) as u8;
                    if !label.is_empty() && !choices.iter().any(|(_, f, b)| *f == format && *b == bits) {
                        choices.push((label, format, bits));
                    }
                }
            }

            view! {
                <div class="rows">
                    <Row
                        label="Bağlayıcı".to_string()
                        value=if sink.is_empty() { connector.clone() } else { format!("{connector} · {sink}") }
                    />
                    <Row
                        label="Bağlantı tavanı".to_string()
                        value=format!(
                            "{} MHz{}",
                            ceiling / 1000,
                            if declared { "" } else { " (bildirilmedi, taban varsayıldı)" },
                        )
                        tone=if declared { "ok" } else { "" }
                    />
                    <Row label="Seçili".to_string() value=if current == "auto" {
                        "Otomatik".to_string()
                    } else {
                        format!("{current_format} {current_bits}bit")
                    } />
                </div>

                <h3>"Modlara göre"</h3>
                <div class="rows">
                    {timings
                        .iter()
                        .map(|timing| {
                            let label = timing.get("label").and_then(Value::as_str).unwrap_or("?").to_string();
                            let hdr = timing.get("hdr10_fits").and_then(Value::as_bool).unwrap_or(false);
                            let allowed: Vec<String> = timing
                                .get("allowed")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(|option| {
                                    option.get("label").and_then(Value::as_str).map(str::to_string)
                                })
                                .collect();
                            view! {
                                <Row
                                    label=label
                                    value=format!(
                                        "{}{}",
                                        allowed.join(", "),
                                        if hdr { "  · HDR10" } else { "  · HDR yok" },
                                    )
                                    tone=if hdr { "ok" } else { "" }
                                />
                            }
                        })
                        .collect_view()}
                </div>

                <h3>"Seçim"</h3>
                <div class="actions">
                    <Action
                        label="Otomatik".to_string()
                        variant=if current == "auto" { "primary".to_string() } else { String::new() }
                        disabled=Signal::derive(move || busy.get())
                        on_press=Callback::new(move |()| choose(None))
                    />
                    {choices
                        .into_iter()
                        .map(|(label, format, bits)| {
                            let selected = current == "fixed"
                                && current_format == format
                                && current_bits == bits;
                            // Leaked so the callback can hold a 'static name;
                            // there are at most a dozen and they live as long
                            // as the screen does.
                            let format: &'static str = Box::leak(format.into_boxed_str());
                            view! {
                                <Action
                                    label=label
                                    variant=if selected { "primary".to_string() } else { String::new() }
                                    disabled=Signal::derive(move || busy.get())
                                    on_press=Callback::new(move |()| choose(Some((format, bits))))
                                />
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
        return missing("Ses profili okunamadı", "Medya çekirdeği yanıt vermedi.").into_any();
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

/// The fan curve editor: what the SoC and the fan are doing, the curve as a
/// graph, and a table of its points -- the same screen the television draws,
/// laid out for a browser.
///
/// Three curves are kept apart: the **active** one in the running device tree,
/// the **saved** one for the next boot, and the **draft** on this page, which
/// nothing else has seen until "Kaydet". Every change goes through
/// [`FanCurve`]'s own editing methods, so the draft is always a curve the
/// control plane would accept -- and the control plane checks it again anyway.
///
/// The cells are real inputs: a keyboard, a phone's number pad and paste all
/// work, the arrows step a value, and Escape puts it back. A value that would
/// break a rule is refused where it is typed, with the range it has to be in.
///
/// The graph joins the points with straight lines, which is what the kernel is
/// given -- the line, one trip per whole degree -- and the board's own curve,
/// before one of these is saved, is drawn as the steps it is. A point's values
/// come up over it when the mouse is on it, or while one of its cells is being
/// typed into; nothing else is written inside the plot. The current
/// temperature and duty are labelled on their axes, and a tick under either
/// label is left out.
///
/// A duty, never a speed: neither board has a tachometer wire.
#[component]
fn Cooling(fan: RwSignal<Option<FanStatus>>) -> impl IntoView {
    let toaster = expect_context::<Toaster>();
    let draft = RwSignal::new(None::<FanCurve>);
    let base = RwSignal::new(None::<FanCurve>);
    let selected = RwSignal::new(0usize);
    let busy = RwSignal::new(false);
    let prompt = RwSignal::new(false);
    let advanced = RwSignal::new(false);
    // A cell of the selected row has the caret.
    let typing = RwSignal::new(false);
    // The point under the mouse, if any.
    let hovered = RwSignal::new(None::<usize>);
    let note = RwSignal::new(String::new());

    // The draft starts once, from the saved curve or else the running one.
    // Later polls do not move it under the person editing it.
    Effect::new(move |_| {
        if draft.with_untracked(Option::is_some) {
            return;
        }
        if let Some(status) = fan.get() {
            let start = status.configured.clone().or(status.curve.clone());
            base.set(start.clone());
            draft.set(start);
        }
    });

    let available = Memo::new(move |_| fan.with(|s| s.as_ref().map(|s| s.available)));
    let count = Memo::new(move |_| draft.with(|d| d.as_ref().map_or(0, |c| c.points.len())));
    let unsaved = Memo::new(move |_| draft.with(Option::is_some) && draft.get() != base.get());
    // The saved curve is not what is running, with no restart to explain it:
    // a boot that did not load it. Not drawn -- the daemon's warning says it
    // under the table -- but the badge does not call the curve running.
    let stale = Memo::new(move |_| {
        fan.with(|s| {
            s.as_ref().is_some_and(|s| {
                !s.pending_reboot
                    && s.configured.as_ref().is_some_and(|configured| {
                        !s.curve
                            .as_ref()
                            .is_some_and(|running| running.same_kernel(configured))
                    })
            })
        })
    });
    // A change, drawn only where it is and only until it is applied: an
    // edit against the saved curve, or a saved curve not yet running against
    // the running one. The curve before it, cut to where the fan would differ.
    let previous = Memo::new(move |_| {
        let before = if unsaved.get() {
            base.get()
        } else if fan.with(|s| s.as_ref().is_some_and(|s| s.pending_reboot)) {
            fan.with(|s| s.as_ref().and_then(|s| s.curve.clone()))
        } else {
            None
        };
        let (Some(before), Some(after)) = (before, draft.get()) else {
            return Vec::new();
        };
        after
            .changed_ranges(&before, 80)
            .into_iter()
            .map(|(from, to)| before.polyline_within(80.0, f64::from(from), f64::from(to)))
            .collect::<Vec<_>>()
    });
    let differs = Memo::new(move |_| previous.with(|p| !p.is_empty()));
    let valid =
        Memo::new(move |_| draft.with(|d| d.as_ref().is_some_and(|c| c.validate().is_ok())));
    let read = move |show: fn(&FanStatus) -> String| {
        move || fan.with(|status| status.as_ref().map(show).unwrap_or_else(|| "—".into()))
    };

    // ------------------------------------------------------------ editing

    let limit = move |curve: &FanCurve, index: usize, duty: bool| {
        if duty {
            let (low, high) = curve.pwm_bounds(index);
            format!(
                "PWM %{}–%{} arasında olmalı (komşu noktaların arası).",
                fan_percent(low),
                fan_percent(high)
            )
        } else {
            let (low, high) = curve.temperature_bounds(index);
            let why = if index + 1 == curve.points.len() && high == FAN_TEMP_MAX_C {
                " (kısma 75 °C'de başlar)"
            } else {
                " (komşu noktaların arası)"
            };
            format!("Sıcaklık {low}–{high} °C arasında olmalı{why}.")
        }
    };
    // A typed value: set, or refused with the reason. Returns what the cell
    // should show afterwards.
    let commit = move |index: usize, duty: bool, text: String| -> String {
        let mut shown = String::new();
        draft.update(|d| {
            let Some(curve) = d else {
                return;
            };
            let value = text.trim().parse::<u32>().ok();
            let accepted = match (duty, value) {
                (false, Some(celsius)) => curve.set_temperature(index, celsius),
                (true, Some(percent)) => {
                    fan_pwm_from_percent(percent).is_some_and(|pwm| curve.set_pwm(index, pwm))
                }
                _ => false,
            };
            if accepted {
                note.set(String::new());
            } else if duty
                && value
                    .and_then(fan_pwm_from_percent)
                    .is_some_and(|pwm| !fan_pwm_allowed(pwm))
            {
                note.set("%1–19 fanın durabildiği aralık: 0 ya da en az %20 yazın.".into());
            } else {
                note.set(limit(curve, index, duty));
            }
            shown = cell_text(curve, index, duty);
        });
        shown
    };
    let step = move |index: usize, duty: bool, up: bool| -> String {
        let mut shown = String::new();
        draft.update(|d| {
            let Some(curve) = d else {
                return;
            };
            let moved = if duty {
                curve.step_pwm(index, up)
            } else {
                curve.step_temperature(index, up)
            };
            note.set(if moved {
                String::new()
            } else {
                limit(curve, index, duty)
            });
            shown = cell_text(curve, index, duty);
        });
        shown
    };
    // After the table changes shape, onto a cell by its key.
    let focus_cell = move |key: String| {
        request_animation_frame(move || {
            if let Some(cell) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| {
                    d.query_selector(&format!("[data-focus-key=\"{key}\"]"))
                        .ok()
                        .flatten()
                })
                .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
            {
                let _ = cell.focus();
            }
        });
    };

    // ------------------------------------------------------------ the daemon

    let save = Callback::new(move |()| {
        let Some(curve) = draft.get_untracked() else {
            return;
        };
        if busy.get_untracked() {
            return;
        }
        if let Err(error) = curve.validate() {
            toaster.warn(format!("Eğri kaydedilmedi: {error}"));
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::typed::<FanStatus>(api::fan_curve_set(&curve)).await {
                Ok(status) => {
                    prompt.set(status.pending_reboot);
                    if let Some(saved) = status.configured.clone() {
                        base.set(Some(saved.clone()));
                        draft.set(Some(saved));
                    }
                    fan.set(Some(status));
                    toaster.say("Fan eğrisi kaydedildi");
                }
                Err(error) => toaster.warn(format!("Fan eğrisi kaydedilemedi: {}", error.message)),
            }
            busy.set(false);
        });
    });

    let reset = Callback::new(move |()| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::typed::<FanStatus>(api::fan_curve_reset()).await {
                Ok(status) => {
                    prompt.set(status.pending_reboot);
                    fan.set(Some(status));
                    base.set(Some(FanCurve::board()));
                    draft.set(Some(FanCurve::board()));
                    selected.set(0);
                    toaster.say("Kartın kendi fan eğrisine dönülecek");
                }
                Err(error) => toaster.warn(format!("Varsayılana dönülemedi: {}", error.message)),
            }
            busy.set(false);
        });
    });

    let restart = Callback::new(move |()| {
        busy.set(true);
        spawn_local(async move {
            match api::control(api::system_restart()).await {
                Ok(_) => toaster.say("Yeniden başlatılıyor…"),
                Err(error) => {
                    toaster.warn(format!("Yeniden başlatılamadı: {}", error.message));
                    busy.set(false);
                }
            }
        });
    });

    // ------------------------------------------------------------ the graph
    //
    // One box, one scale: x from 0 to 80 °C, y from off to full duty. The SVG
    // keeps this aspect wherever it is drawn, so nothing in it can be scaled
    // apart from the rest.
    const W: f64 = 1000.0;
    const H: f64 = 540.0;
    const L: f64 = 86.0;
    const R: f64 = 24.0;
    const T: f64 = 22.0;
    const B: f64 = 64.0;
    const MAX_C: f64 = 80.0;
    let gx = |c: f64| L + (c / MAX_C).clamp(0.0, 1.0) * (W - L - R);
    let gf = |f: f64| T + (1.0 - f) * (H - T - B);
    let gy = move |pwm: u32| gf(f64::from(pwm) / f64::from(FAN_PWM_MAX));
    let path = move |corners: &[(f64, f64)]| {
        let mut path = String::new();
        for &(celsius, pwm) in corners {
            path.push_str(&format!(
                "{} {:.1} {:.1} ",
                if path.is_empty() { "M" } else { "L" },
                gx(celsius),
                gf(pwm / f64::from(FAN_PWM_MAX))
            ));
        }
        path
    };
    let line = move |curve: &FanCurve| path(&curve.polyline(MAX_C));

    let graph = move || {
        let Some(curve) = draft.get() else {
            return view! { <svg class="fan-graph" viewBox=format!("0 0 {W} {H}")></svg> }
                .into_any();
        };
        let status = fan.get();
        let now = status.as_ref().and_then(|s| s.temperature_c);
        let now_pwm = status.as_ref().and_then(|s| s.pwm);
        let edge = line(&curve);
        let fill = format!(
            "{edge} L {:.1} {:.1} L {:.1} {:.1} Z",
            gx(MAX_C),
            gy(0),
            gx(0.0),
            gy(0)
        );
        let trip = gx(f64::from(FAN_TEMP_MAX_C));
        let chosen = selected.get().min(curve.points.len().saturating_sub(1));
        let last = curve.points.len().saturating_sub(1);

        let x_ticks = (0..=8)
            .map(|i| {
                let c = f64::from(i * 10);
                let x = gx(c);
                let shown = now.is_none_or(|now| (c - now).abs() >= 7.0);
                view! {
                    <line class="grid" x1=x y1=T x2=x y2=H - B />
                    {shown.then(|| view! {
                        <text class="tick" class:end=i == 8 x=x y=H - B + 32.0 text-anchor="middle">
                            {format!("{}°", i * 10)}
                        </text>
                    })}
                }
            })
            .collect_view();
        let now_frac = now_pwm.map(|p| f64::from(p) / f64::from(FAN_PWM_MAX));
        let y_ticks = (0..=4)
            .map(|i| {
                let f = f64::from(i) * 0.25;
                let y = gf(f);
                let shown = now_frac.is_none_or(|now| (f - now).abs() >= 0.07);
                view! {
                    <line class="grid" class:base=i == 0 x1=L y1=y x2=W - R y2=y />
                    {shown.then(|| view! {
                        <text class="tick" x=L - 14.0 y=y + 7.0 text-anchor="end">
                            {format!("%{}", i * 25)}
                        </text>
                    })}
                }
            })
            .collect_view();
        let now_marks = now.zip(now_pwm).map(|(c, p)| {
            let (x, y) = (gx(c), gy(p));
            let chip_x = (x - 48.0).clamp(0.0, W - 96.0);
            view! {
                <line class="g-now" x1=x y1=T x2=x y2=H - B />
                <line class="g-now duty" x1=L y1=y x2=W - R y2=y />
                <circle class="now-dot" cx=x cy=y r=7.0 />
                <rect class="g-chip" x=chip_x y=H - B + 10.0 width=96.0 height=36.0 rx=18.0 />
                <text class="chip-text" x=chip_x + 48.0 y=H - B + 34.0 text-anchor="middle">
                    {format!("{c:.1}°")}
                </text>
                <rect class="g-chip duty" x=L - 82.0 y=y - 18.0 width=72.0 height=36.0 rx=18.0 />
                <text class="chip-text" x=L - 46.0 y=y + 6.0 text-anchor="middle">
                    {format!("%{}", fan_percent(p))}
                </text>
            }
        });
        let points = curve
            .points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (x, y) = (gx(f64::from(p.temperature_c)), gy(p.pwm));
                let lit = move || hovered.get() == Some(i);
                view! {
                    <circle class="point" class:selected=i == chosen class:hovered=lit cx=x cy=y
                        r=if i == chosen { 13.0 } else { 9.0 } />
                    {(i == last).then(|| view! {
                        <rect class="point-lock" x=x - 3.5 y=y - 3.5 width=7.0 height=7.0 />
                    })}
                    // A wider, invisible target, so a point is easy to land on.
                    <circle class="point-hit" cx=x cy=y r=24.0
                        on:mouseenter=move |_| hovered.set(Some(i))
                        on:mouseleave=move |_| hovered.set(None)
                        on:click=move |_| {
                            selected.set(i);
                            focus_cell(format!("fan:{i}:t"));
                        }
                    />
                }
            })
            .collect_view();
        // The values of the point under the mouse, or of the one being typed
        // into, over it. Only one at a time, and only while it is being looked
        // at, so it never stands in front of the rest of the curve for long.
        let callout = move || {
            let shown = hovered
                .get()
                .or_else(|| typing.get().then(|| selected.get()))?;
            let p = draft.with(|d| d.as_ref().and_then(|c| c.points.get(shown).copied()))?;
            let (x, y) = (gx(f64::from(p.temperature_c)), gy(p.pwm));
            let (cw, ch) = (178.0, 44.0);
            let bx = (x - cw / 2.0).clamp(L + 2.0, W - R - cw - 2.0);
            let below = y - ch - 20.0 < T;
            let by = if below { y + 20.0 } else { y - ch - 20.0 };
            let tip = if below {
                format!("M {:.1} {:.1} l -8 0 l 8 -9 l 8 9 Z", x, by)
            } else {
                format!("M {:.1} {:.1} l -8 0 l 8 9 l 8 -9 Z", x, by + ch)
            };
            Some(view! {
                <g class="callout" class:editing=move || typing.get() && hovered.get().is_none()>
                    <rect x=bx y=by width=cw height=ch rx=8.0 />
                    <path d=tip />
                    <text x=bx + cw / 2.0 y=by + 29.0 text-anchor="middle">
                        {format!("{} °C  %{}", p.temperature_c, fan_percent(p.pwm))}
                    </text>
                </g>
            })
        };
        view! {
            <svg class="fan-graph" viewBox=format!("0 0 {W} {H}") role="img"
                aria-label="Fan eğrisi: SoC sıcaklığına göre PWM">
                <rect class="trip-band" x=trip y=T width=W - R - trip height=H - T - B />
                <line class="trip" x1=trip y1=T x2=trip y2=H - B />
                <text class="trip-label" text-anchor="middle"
                    transform=format!("translate({:.1} {:.1}) rotate(-90)", (trip + W - R) / 2.0 + 6.0, (H - B + T) / 2.0)>
                    "KISMA ≥ 75 °C"
                </text>
                {x_ticks}
                {y_ticks}
                {previous
                    .get()
                    .into_iter()
                    .map(|corners| view! { <path class="running" d=path(&corners) /> })
                    .collect_view()}
                <path class="fill" d=fill />
                <path class="curve" d=edge />
                {now_marks}
                {points}
                {callout}
            </svg>
        }
        .into_any()
    };

    // ------------------------------------------------------------ the table

    let point_row = move |index: usize| {
        let point = move || draft.with(|d| d.as_ref().and_then(|c| c.points.get(index).copied()));
        let last = move || index + 1 == count.get();
        let chosen = move || selected.get() == index;
        let can_add = move || {
            !busy.get()
                && draft.with(|d| {
                    d.clone()
                        .is_some_and(|mut c| c.insert_after(index).is_some())
                })
        };
        let can_remove = move || !busy.get() && count.get() > FAN_CURVE_MIN_POINTS && !last();
        let cell = move |duty: bool| {
            let key = format!("fan:{index}:{}", if duty { "p" } else { "t" });
            let locked = move || duty && last();
            view! {
                <label class="fan-cell" class:duty=duty class:locked=locked>
                    {duty.then(|| view! { <span class="fan-unit">"%"</span> })}
                    <input
                        class="fan-input"
                        type="text"
                        inputmode="numeric"
                        maxlength="3"
                        autocomplete="off"
                        aria-label=if duty { "PWM yüzdesi" } else { "Sıcaklık" }
                        data-focus=move || (!locked() && !busy.get()).then_some("1")
                        data-focus-key=key
                        disabled=move || locked() || busy.get()
                        prop:value=move || {
                            draft.with(|d| d.as_ref().map(|c| cell_text(c, index, duty)).unwrap_or_default())
                        }
                        on:focus=move |_| {
                            selected.set(index);
                            typing.set(true);
                        }
                        on:blur=move |_| typing.set(false)
                        on:change=move |event| {
                            let input = event_target::<web_sys::HtmlInputElement>(&event);
                            input.set_value(&commit(index, duty, input.value()));
                        }
                        on:keydown=move |event| {
                            let input = event_target::<web_sys::HtmlInputElement>(&event);
                            match event.key().as_str() {
                                // Up and down step the value here, rather than
                                // moving the focus off the cell being edited.
                                "ArrowUp" | "ArrowDown" => {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    input.set_value(&step(index, duty, event.key() == "ArrowUp"));
                                }
                                "Enter" => {
                                    event.prevent_default();
                                    input.set_value(&commit(index, duty, input.value()));
                                    input.select();
                                }
                                // Put it back, and stay on the page.
                                "Escape" => {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    let shown = draft.with_untracked(|d| {
                                        d.as_ref().map(|c| cell_text(c, index, duty)).unwrap_or_default()
                                    });
                                    input.set_value(&shown);
                                    note.set(String::new());
                                }
                                _ => {}
                            }
                        }
                    />
                    {(!duty).then(|| view! { <span class="fan-unit">"°C"</span> })}
                    {duty.then(|| view! {
                        <span class="fan-raw">{move || match point() {
                            _ if last() => "sabit".to_string(),
                            Some(p) if p.pwm == 0 => "kapalı".to_string(),
                            Some(p) => p.pwm.to_string(),
                            None => String::new(),
                        }}</span>
                    })}
                </label>
            }
        };
        view! {
            <div class="fan-row" class:selected=chosen on:click=move |_| selected.set(index)>
                <span class="fan-num">{index + 1}</span>
                {cell(false)}
                {cell(true)}
                <button
                    class="fan-rowbtn"
                    class:shown=chosen
                    title="Bu noktayla sonraki arasına nokta ekle"
                    data-focus=move || (chosen() && can_add()).then_some("1")
                    data-focus-key=format!("fan:{index}:add")
                    tabindex="-1"
                    disabled=move || !can_add()
                    on:click=move |event| {
                        event.stop_propagation();
                        let mut at = None;
                        draft.update(|d| {
                            if let Some(curve) = d {
                                at = curve.insert_after(index);
                            }
                        });
                        if let Some(at) = at {
                            selected.set(at);
                            note.set(format!("Nokta #{} eklendi.", at + 1));
                            focus_cell(format!("fan:{at}:t"));
                        }
                    }
                >
                    "+"
                </button>
                <button
                    class="fan-rowbtn"
                    class:shown=chosen
                    title="Bu noktayı sil"
                    data-focus=move || (chosen() && can_remove()).then_some("1")
                    data-focus-key=format!("fan:{index}:remove")
                    tabindex="-1"
                    disabled=move || !can_remove()
                    on:click=move |event| {
                        event.stop_propagation();
                        let mut removed = false;
                        draft.update(|d| {
                            if let Some(curve) = d {
                                removed = curve.remove(index);
                            }
                        });
                        if removed {
                            note.set(format!("Nokta #{} silindi.", index + 1));
                            focus_cell(format!("fan:{index}:t"));
                        }
                    }
                >
                    "−"
                </button>
            </div>
        }
    };

    let state = move || {
        if unsaved.get() {
            ("Kaydedilmedi", "draft")
        } else if fan.with(|s| s.as_ref().is_some_and(|s| s.pending_reboot)) {
            ("Yeniden başlatma bekliyor", "pending")
        } else if stale.get() {
            ("Kayıtlı", "draft")
        } else {
            ("Eğri etkin", "ok")
        }
    };
    let profile = move || draft.with(|d| d.as_ref().map(|c| c.profile));

    let table = move || {
        view! {
            <div class="fan-card fan-table">
                <div class="fan-card-head">
                    <b>"Noktalar"</b>
                    <span>{move || format!("{} / {FAN_CURVE_MAX_POINTS}", count.get())}</span>
                </div>
                <div class="fan-cols"><span>"#"</span><span>"Sıcaklık"</span><span>"PWM"</span></div>
                <div class="fan-rows" data-row="1">
                    <For each=move || 0..count.get() key=|index| *index children=point_row />
                </div>
                <p class="fan-note">{move || {
                    let said = note.get();
                    if !said.is_empty() {
                        said
                    } else if typing.get() {
                        "Rakam yazın ya da ↑↓ ile adım adım değiştirin · Enter onayla · Esc vazgeç"
                            .into()
                    } else if let Some(warning) = fan.with(|s| s.as_ref().and_then(|s| s.warning.clone())) {
                        warning
                    } else {
                        "Son nokta güvenlik için %100'de sabit. PWM 1–19 % fanın durabildiği \
                         aralık olduğu için atlanır."
                            .into()
                    }
                }}</p>
            </div>
        }
    };

    let info = move || {
        let rows = fan.with(|s| {
            let Some(s) = s else {
                return Vec::new();
            };
            vec![
                ("Denetleyen", "Çekirdek · pwm-fan".to_string(), ""),
                ("Kart", s.board.clone().unwrap_or_else(|| "—".into()), ""),
                (
                    "Ham PWM",
                    s.pwm.map_or_else(|| "—".into(), |p| format!("{p} / 255")),
                    "",
                ),
                (
                    "Kart düzeltmesi (50 Hz)",
                    match s.board_fix {
                        FanBoardFix::Active => "Etkin",
                        FanBoardFix::Missing => "Gerekli ama etkin değil",
                        FanBoardFix::NotNeeded => "Bu kartta gerekmiyor",
                    }
                    .to_string(),
                    match s.board_fix {
                        FanBoardFix::Active => "ok",
                        FanBoardFix::Missing => "warn",
                        FanBoardFix::NotNeeded => "",
                    },
                ),
                (
                    "PWM frekansı",
                    match s.pwm_frequency_hz {
                        Some(hz) if hz >= 1000.0 => format!("{:.0} kHz", hz / 1000.0),
                        Some(hz) => format!("{hz:.0} Hz"),
                        None => "—".into(),
                    },
                    "",
                ),
                (
                    "PWM periyodu",
                    s.pwm_period_ns
                        .map_or_else(|| "—".into(), |ns| format!("{ns} ns")),
                    "",
                ),
                (
                    "Dönüş hızı (RPM)",
                    match (s.rpm_available, s.rpm) {
                        (true, Some(rpm)) => format!("{rpm} RPM"),
                        (true, None) => "Okunamadı".into(),
                        (false, _) => "Ölçülemiyor".into(),
                    },
                    if s.rpm_available { "" } else { "dim" },
                ),
                ("Kısma sınırı", "75 °C".into(), ""),
                (
                    "Çekirdek eşikleri",
                    s.curve.as_ref().map_or_else(
                        || "—".into(),
                        |c| format!("{} eşik · 1 °C aralıkla", c.kernel_steps().len()),
                    ),
                    "",
                ),
            ]
        });
        view! {
            <div class="fan-card fan-advanced">
                <h3>"Gelişmiş bilgiler"</h3>
                <div class="fan-kv">
                    {rows
                        .into_iter()
                        .map(|(label, value, tone)| view! {
                            <div class="fan-kv-row">
                                <span>{label}</span>
                                <span class=format!("fan-kv-value {tone}")>{value}</span>
                            </div>
                        })
                        .collect_view()}
                </div>
                <p class="panel-note">
                    "PWM fana giden güç oranıdır, dönüş hızı değildir: bu kartta devir ölçen hat \
                     yok. 50 Hz düzeltmesi bu kartın donanımı için gerekir ve değiştirilemez; bir \
                     ayar değil, kartın bir özelliği. Eğri çekirdeğe 1 °C aralıklı eşikler \
                     olarak yazılır; çizim ile fanın gerçek davranışı arasındaki fark bir \
                     dereceden azdır. Kaydedilen eğri yeniden başlatınca yüklenir."
                </p>
            </div>
        }
    };

    view! {
        <div class="fan-head">
            <h2>"Soğutma"</h2>
            {move || (available.get() == Some(true)).then(|| view! {
                <div class="fan-seg" data-row="1">
                    {FanProfile::PRESETS
                        .into_iter()
                        .map(|preset| view! {
                            <button
                                class="fan-seg-opt"
                                class:on=move || profile() == Some(preset)
                                data-focus=move || (!busy.get()).then_some("1")
                                tabindex="-1"
                                disabled=move || busy.get()
                                on:click=move |_| {
                                    draft.set(FanCurve::preset(preset));
                                    selected.set(0);
                                    note.set(format!("{} yüklendi. Kaydedince uygulanır.", preset.label()));
                                }
                            >
                                {preset.label()}
                            </button>
                        })
                        .collect_view()}
                    <span class="fan-seg-opt label" class:on=move || profile() == Some(FanProfile::Custom)>
                        {FanProfile::Custom.label()}
                    </span>
                </div>
            })}
        </div>
        {move || match available.get() {
            None => view! { <div class="state"><strong>"Okunuyor…"</strong></div> }.into_any(),
            Some(false) => {
                let why = fan.with_untracked(|s| {
                    s.as_ref().and_then(|s| s.error.clone()).unwrap_or_default()
                });
                missing("Denetlenebilir fan yok", &why).into_any()
            }
            Some(true) => view! {
                <div class="fan-readings">
                    <div class="fan-reading">
                        <span class="k">"SoC sıcaklığı"</span>
                        <span class="v">{read(|s| {
                            s.temperature_c.map_or_else(|| "—".into(), |c| format!("{c:.1} °C"))
                        })}</span>
                    </div>
                    <div class="fan-reading">
                        <span class="k">"Fan gücü (PWM)"</span>
                        <span class="v">
                            {read(|s| s.pwm.map_or_else(|| "—".into(), |p| format!("%{}", fan_percent(p))))}
                            <small>{read(|s| s.pwm.map_or_else(String::new, |p| format!("{p}/255")))}</small>
                        </span>
                    </div>
                    <div class="fan-reading">
                        <span class="k">"Eğri"</span>
                        <span class="v">
                            {move || format!("{} nokta", count.get())}
                            <small>{move || profile().map_or("—", FanProfile::label)}</small>
                        </span>
                    </div>
                    <span class=move || format!("fan-state {}", state().1)>{move || state().0}</span>
                </div>

                {move || if advanced.get() {
                    info().into_any()
                } else {
                    view! {
                        <div class="fan-main">
                            <div class="fan-card fan-plot">
                                <div class="fan-card-head">
                                    <b>"Fan eğrisi"</b>
                                    <span class="fan-legend">
                                        <i class="lg-curve"></i>
                                        {move || {
                                            if unsaved.get() {
                                                "Taslak"
                                            } else if differs.get() {
                                                "Kayıtlı"
                                            } else {
                                                "Eğri"
                                            }
                                        }}
                                        {move || differs.get().then(|| view! {
                                            <i class="lg-running"></i>"Önceki"
                                        })}
                                        <i class="lg-now"></i>"Şu an"
                                        <i class="lg-band"></i>"Kısma"
                                    </span>
                                </div>
                                {graph}
                            </div>
                            {table}
                        </div>
                    }
                    .into_any()
                }}

                <div class="actions-row fan-actions" data-row="1">
                    <Action
                        label="Kaydet"
                        variant="primary"
                        disabled=Signal::derive(move || {
                            busy.get() || !unsaved.get() || !valid.get()
                        })
                        on_press=save
                    />
                    <Action
                        label="Geri al"
                        variant="ghost"
                        disabled=Signal::derive(move || busy.get() || !unsaved.get())
                        on_press=Callback::new(move |()| {
                            draft.set(base.get_untracked());
                            note.set("Değişiklikler geri alındı.".into());
                        })
                    />
                    <Action
                        label="Varsayılana dön"
                        variant="ghost"
                        disabled=Signal::derive(move || busy.get())
                        on_press=reset
                    />
                    <Action
                        label=Signal::derive(move || {
                            if advanced.get() { "Eğriye dön" } else { "Gelişmiş bilgiler" }.to_string()
                        })
                        variant="ghost"
                        on_press=Callback::new(move |()| advanced.update(|open| *open = !*open))
                    />
                    <span class="fan-foot">"Kaydedilen eğri yeniden başlatınca etkin olur."</span>
                </div>

                {move || prompt.get().then(|| view! {
                    <div class="overlay" data-focus-scope="1">
                        <div class="sheet fan-prompt">
                            <h2>"Fan eğrisi kaydedildi"</h2>
                            <p class="panel-note">
                                "Yeni eğri yeniden başlatıldıktan sonra etkin olacaktır."
                            </p>
                            <div class="actions-row">
                                <Action
                                    label="Şimdi yeniden başlat"
                                    variant="primary"
                                    disabled=Signal::derive(move || busy.get())
                                    on_press=restart
                                />
                                // "Later" takes the focus: a stray press must
                                // not restart the appliance.
                                <Action
                                    label="Daha sonra"
                                    variant="ghost"
                                    autofocus=true
                                    on_press=Callback::new(move |()| {
                                        prompt.set(false);
                                        note.set("Yeni eğri bir sonraki açılışta etkin olacak.".into());
                                    })
                                />
                            </div>
                        </div>
                    </div>
                })}
            }
                .into_any(),
        }}
    }
}

/// A duty as the whole percent the page prints.
fn fan_percent(pwm: u32) -> u32 {
    fan_pwm_percent(pwm).round() as u32
}

/// What a cell of the point table shows: the temperature in °C, or the duty
/// in whole percent.
fn cell_text(curve: &FanCurve, index: usize, duty: bool) -> String {
    curve.points.get(index).map_or_else(String::new, |p| {
        if duty {
            fan_percent(p.pwm).to_string()
        } else {
            p.temperature_c.to_string()
        }
    })
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
    let ffmpeg_tone = if ffmpeg > sessions as usize {
        "warn"
    } else {
        "ok"
    };
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
