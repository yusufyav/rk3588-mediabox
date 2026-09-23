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
    ColorFormat, ColorMode, ColourCell, OUTPUT_TRIAL_SECONDS, OutputModeOffer, OutputOffer,
    OutputSetting, OutputStatus, Request, ResolutionChoice,
};
use std::collections::BTreeMap;
use mediabox_core::{
    FAN_CURVE_MAX_POINTS, FAN_CURVE_MIN_POINTS, FAN_PWM_MAX, FAN_TEMP_MAX_C, FanBoardFix, FanCurve,
    FanProfile, FanStatus, fan_pwm_allowed, fan_pwm_from_percent, fan_pwm_percent,
};
use serde_json::Value;
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
                            "display" => view! { <Display /> }.into_any(),
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

/// The display: the size, its refresh and the colour mode at it, as the
/// television's own screen shows them -- readings along the top, the three
/// lists in cards, buttons under them.
///
/// Nothing is decided here. Every mode, every colour cell and every reason
/// comes from the daemon's account of the display, which the television's
/// interface computed by the HDMI rules from the kernel's mode list and the
/// EDID. A choice is a draft until "Uygula"; then it is on trial, and the
/// question whether to keep it is asked here and on the television alike.
#[component]
fn Display() -> impl IntoView {
    let toaster = expect_context::<Toaster>();
    let events = expect_context::<crate::app::OutputEvents>();
    let status = RwSignal::new(None::<Result<OutputStatus, String>>);
    // The draft: `None` until the daemon's first answer seeds it, and again
    // after a trial ends.
    let draft = RwSignal::new(None::<(ResolutionChoice, BTreeMap<String, ColorMode>)>);
    let open = RwSignal::new(None::<(u16, u16)>);
    let hovered = RwSignal::new(String::new());
    let edid = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let now = RwSignal::new(0u32);

    let read = move || {
        spawn_local(async move {
            let answer = api::typed::<OutputStatus>(api::output(&Request::OutputStatus))
                .await
                .map_err(|error| error.message);
            if let Ok(found) = &answer {
                if draft.with_untracked(Option::is_none) {
                    let kept = found
                        .trial
                        .as_ref()
                        .map(|trial| trial.setting.clone())
                        .unwrap_or_else(|| found.setting.clone());
                    draft.set(Some((kept.resolution, kept.colours)));
                }
            }
            status.set(Some(answer));
        });
    };
    read();
    // Every display event on the daemon's stream: a trial, a keep, a new mode.
    Effect::new(move |seen: Option<u64>| {
        let count = events.0.get();
        if seen.is_some_and(|seen| seen != count) {
            if status.with_untracked(|s| {
                s.as_ref()
                    .and_then(|s| s.as_ref().ok())
                    .is_some_and(|s| s.trial.is_some())
            }) {
                // The trial this page was showing is over, or another began:
                // the draft follows what the daemon says next.
                draft.set(None);
            }
            read();
        }
        count
    });
    // The countdown, while there is one.
    Effect::new(move |previous: Option<Option<IntervalHandle>>| {
        if let Some(Some(handle)) = previous {
            handle.clear();
        }
        let trial = status.with(|s| {
            s.as_ref()
                .and_then(|s| s.as_ref().ok())
                .and_then(|s| s.trial.as_ref().map(|t| t.seconds_left))
        })?;
        now.set(trial);
        set_interval_with_handle(
            move || now.update(|left| *left = left.saturating_sub(1)),
            std::time::Duration::from_secs(1),
        )
        .ok()
    });

    let call = move |request: Request, done: &'static str| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            let answer = api::typed::<OutputStatus>(api::output(&request)).await;
            busy.set(false);
            match answer {
                Ok(found) => {
                    if !done.is_empty() {
                        toaster.say(done);
                    }
                    status.set(Some(Ok(found)));
                }
                Err(error) => toaster.warn(error.message),
            }
        });
    };

    view! {
        {move || {
            let Some(answer) = status.get() else {
                return view! { <div class="state"><strong>"Okunuyor…"</strong></div> }.into_any();
            };
            let found = match answer {
                Ok(found) => found,
                Err(why) => return missing("Ekran okunamadı", &why).into_any(),
            };
            let Some(offer) = found.offer.clone() else {
                let why = found.error.clone().unwrap_or_else(|| "Bağlı bir ekran yok.".into());
                return view! {
                    <h2>"Ekran"</h2>
                    {missing("Ekran okunamadı", &why)}
                }
                    .into_any();
            };
            let (resolution, colours) = draft.get().unwrap_or_default();
            let chosen = offer.resolve(resolution).cloned();
            let chosen_colour = chosen
                .as_ref()
                .and_then(|mode| colours.get(&mode.label).copied());
            let kept = found
                .trial
                .as_ref()
                .map(|trial| trial.setting.clone())
                .unwrap_or_else(|| found.setting.clone());
            let label = chosen.as_ref().map(|mode| mode.label.clone()).unwrap_or_default();
            let unsaved = resolution != kept.resolution
                || chosen_colour != kept.colours.get(&label).copied();
            let wire = found.wire.clone().unwrap_or_default();
            let wire_mode = offer.mode(&wire.mode).cloned();
            let link = offer.link.clone();
            let ceiling = if link.max_character_rate_khz > 0 {
                link.max_character_rate_khz.min(link.source_max_khz)
            } else {
                link.source_max_khz
            };
            let load = match (&wire_mode, wire.colour) {
                (Some(mode), Some(colour)) => colour.character_rate_khz(mode.pixel_clock_khz),
                _ => 0,
            };
            let (state, state_class) = if found.trial.is_some() {
                ("Onay bekliyor", "draft")
            } else if unsaved {
                ("Uygulanmadı", "draft")
            } else if wire_mode.as_ref().is_some_and(|mode| mode.hdr10()) {
                ("Etkin · HDR hazır", "ok")
            } else {
                ("Etkin · HDR yok", "ok")
            };
            let open_size = open.get().or_else(|| chosen.as_ref().map(|mode| (mode.width, mode.height)));
            let group = offer
                .groups
                .iter()
                .find(|group| Some((group.width, group.height)) == open_size)
                .cloned();
            let set_draft = move |resolution: ResolutionChoice, colours: BTreeMap<String, ColorMode>| {
                draft.set(Some((resolution, colours)));
            };
            let colours_for_sizes = colours.clone();
            let colours_for_rates = colours.clone();
            let colours_for_cells = colours.clone();
            let colours_for_auto = colours.clone();
            let chosen_label = label.clone();
            let chosen_label_auto = label.clone();
            let mut divided = false;

            view! {
                <div class="fan-head">
                    <h2>"Ekran"</h2>
                    <span class="out-sink">
                        {format!(
                            "{} · {} · {} MHz",
                            offer.sink_name.clone().unwrap_or_else(|| "Ekran".into()),
                            offer.connector,
                            link.max_character_rate_khz / 1000
                        )}
                    </span>
                </div>
                <div class="fan-readings">
                    <div class="fan-reading">
                        <span class="k">"Şu an giden"</span>
                        <span class="v">
                            {wire_mode.as_ref().map(|m| format!("{}×{}", m.width, m.height)).unwrap_or_else(|| "—".into())}
                            <small>{wire_mode.as_ref().map(|m| format!("{} Hz", hz_text(m.refresh_mhz))).unwrap_or_default()}</small>
                        </span>
                    </div>
                    <div class="fan-reading">
                        <span class="k">"Renk"</span>
                        <span class="v">
                            {wire.colour.map(format_short).unwrap_or_else(|| "—".into())}
                            <small>
                                {wire.colour.map(|c| format!("{} bit", c.bits)).unwrap_or_default()}
                                {wire.bus_format.clone().map(|bus| format!(" · {bus}")).unwrap_or_default()}
                            </small>
                        </span>
                    </div>
                    <div class="fan-reading">
                        <span class="k">"Hat yükü"</span>
                        <span class="v">
                            {if load > 0 { format!("{}", load / 1000) } else { "—".into() }}
                            <small>{format!("/ {} MHz", ceiling / 1000)}</small>
                        </span>
                        <span class="out-load">
                            <i style=format!(
                                "width:{:.1}%",
                                if ceiling > 0 { (f64::from(load) / f64::from(ceiling) * 100.0).min(100.0) } else { 0.0 }
                            )></i>
                        </span>
                    </div>
                    <span class=format!("fan-state {state_class}")>{state}</span>
                </div>

                <div class="out-main">
                    <section class="fan-card">
                        <div class="fan-card-head">
                            <b>"Çözünürlük"</b>
                            <span>{format!("{} boyut · {} mod", offer.groups.len(), offer.modes().count())}</span>
                        </div>
                        <div class="out-list" role="radiogroup" aria-label="Çözünürlük">
                            <label class="out-row">
                                <input
                                    type="radio"
                                    name="out-size"
                                    data-focus="1"
                                    prop:checked=resolution == ResolutionChoice::Auto
                                    on:change=move |_| {
                                        open.set(None);
                                        set_draft(ResolutionChoice::Auto, colours_for_sizes.clone());
                                    }
                                />
                                <span class="out-t">"Otomatik"<small>{offer.mode(&offer.auto).map(label_of).unwrap_or_default()}</small></span>
                                {(kept.resolution == ResolutionChoice::Auto && found.trial.is_none()).then(|| view! { <span class="out-tag out-now">"şu an"</span> })}
                            </label>
                            {offer
                                .groups
                                .iter()
                                .map(|group| {
                                    let size = (group.width, group.height);
                                    let fastest = group
                                        .modes
                                        .iter()
                                        .filter(|mode| mode.allowed().next().is_some())
                                        .map(|mode| mode.refresh_mhz)
                                        .max();
                                    let heading = (group.computer && !divided).then(|| view! {
                                        <div class="out-divider">"Bilgisayar modları"</div>
                                    });
                                    divided |= group.computer;
                                    let selected = resolution != ResolutionChoice::Auto
                                        && chosen.as_ref().is_some_and(|m| (m.width, m.height) == size);
                                    let showing = open_size == Some(size);
                                    let hdr = group.modes.iter().any(|m| m.hdr10());
                                    let name = group.name.clone();
                                    view! {
                                        {heading}
                                        <button
                                            class="out-row"
                                            class:open=showing
                                            data-focus="1"
                                            tabindex="-1"
                                            on:click=move |_| open.set(Some(size))
                                        >
                                            <span class=if selected { "out-dot on" } else { "out-dot" }></span>
                                            <span class="out-t">
                                                {format!("{}×{}", size.0, size.1)}
                                                <small>{match fastest {
                                                    Some(hz) if name.is_empty() => format!("{} Hz'e kadar", hz_text(hz)),
                                                    Some(hz) => format!("{name} · {} Hz'e kadar", hz_text(hz)),
                                                    None => "sığmıyor".into(),
                                                }}</small>
                                            </span>
                                            {hdr.then(|| view! { <span class="out-tag out-hdr">"HDR"</span> })}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    </section>

                    <section class="fan-card">
                        <div class="fan-card-head">
                            <b>"Yenileme"</b>
                            <span>{open_size.map(|(w, h)| format!("{w}×{h}")).unwrap_or_default()}</span>
                        </div>
                        <div class="out-list" role="radiogroup" aria-label="Yenileme">
                            {group
                                .map(|group| {
                                    group
                                        .modes
                                        .into_iter()
                                        .map(|mode| {
                                            let fits = mode.allowed().next().is_some();
                                            let checked = mode.is(resolution);
                                            let choice = mode.choice();
                                            let on_wire = mode.label == wire.mode;
                                            let colours = colours_for_rates.clone();
                                            let reason = if fits {
                                                format!(
                                                    "{}: {} renk seçeneği. {}",
                                                    label_of(&mode),
                                                    mode.allowed().count(),
                                                    if mode.hdr10() { "HDR10 taşınabilir." } else { "HDR10 taşınamaz: bu modda 10 bit sığmıyor." }
                                                )
                                            } else {
                                                format!("{}: bu bağlantıda hiçbir renk biçimi sığmıyor.", label_of(&mode))
                                            };
                                            view! {
                                                <label class="out-row" class:refused=!fits on:mouseenter=move |_| hovered.set(reason.clone())>
                                                    <input
                                                        type="radio"
                                                        name="out-rate"
                                                        data-focus="1"
                                                        disabled=!fits
                                                        prop:checked=checked
                                                        on:change=move |_| set_draft(choice, colours.clone())
                                                    />
                                                    <span class="out-t">
                                                        {format!("{} Hz{}", hz_text(mode.refresh_mhz), if mode.interlaced { " i" } else { "" })}
                                                        {mode.preferred.then(|| view! { <small>"Ekranın tercihi"</small> })}
                                                    </span>
                                                    {on_wire.then(|| view! { <span class="out-tag out-now">"şu an"</span> })}
                                                    {mode.hdr10().then(|| view! { <span class="out-tag out-hdr">"HDR"</span> })}
                                                </label>
                                            }
                                        })
                                        .collect_view()
                                })}
                        </div>
                    </section>

                    <section class="fan-card">
                        <div class="fan-card-head">
                            <b>"Renk"</b>
                            <span>{chosen.as_ref().map(label_of).unwrap_or_default()}</span>
                        </div>
                        <label class="out-auto">
                            <input
                                type="radio"
                                name="out-colour"
                                data-focus="1"
                                prop:checked=chosen_colour.is_none()
                                on:change=move |_| {
                                    let mut colours = colours_for_auto.clone();
                                    colours.remove(&chosen_label_auto);
                                    set_draft(resolution, colours);
                                }
                            />
                            <span class="out-t">"Otomatik"</span>
                            <small>{format!(
                                "SDR: {} · HDR: {}",
                                chosen.as_ref().and_then(|m| m.auto_sdr).map(colour_text).unwrap_or_else(|| "—".into()),
                                chosen.as_ref().and_then(|m| m.auto_hdr).map(colour_text).unwrap_or_else(|| "yok".into())
                            )}</small>
                        </label>
                        <div class="out-grid">
                            <span></span><span class="out-gh">"8 bit"</span><span class="out-gh">"10 bit"</span>
                            {[
                                (ColorFormat::Rgb, "RGB"),
                                (ColorFormat::Ycbcr444, "YCbCr 4:4:4"),
                                (ColorFormat::Ycbcr422, "YCbCr 4:2:2"),
                                (ColorFormat::Ycbcr420, "YCbCr 4:2:0"),
                            ]
                                .into_iter()
                                .map(|(format, name)| {
                                    let cells: Vec<ColourCell> = chosen
                                        .as_ref()
                                        .map(|m| m.cells.iter().filter(|c| c.mode.format == format).cloned().collect())
                                        .unwrap_or_default();
                                    let cells_view = cells
                                        .into_iter()
                                        .map(|cell| {
                                            let ok = cell.refused.is_none();
                                            let checked = chosen_colour == Some(cell.mode);
                                            let on_wire = wire.mode == chosen_label && wire.colour == Some(cell.mode);
                                            let reason = match cell.refused {
                                                Some(refusal) => format!("{}: seçilemez. {}", colour_text(cell.mode), refusal.text(cell.mode.format)),
                                                None => format!(
                                                    "{}: gereken {} MHz, bu bağlantının {} MHz tavanına sığar.{}",
                                                    colour_text(cell.mode),
                                                    cell.rate_khz / 1000,
                                                    ceiling / 1000,
                                                    if cell.mode.carries_hdr() { " HDR10 taşır." } else { " HDR10 için 10 bit gerekir." }
                                                ),
                                            };
                                            let fill = (f64::from(cell.rate_khz) / f64::from(link.source_max_khz) * 100.0).min(100.0);
                                            let tick = f64::from(ceiling) / f64::from(link.source_max_khz) * 100.0;
                                            let colours = colours_for_cells.clone();
                                            let label = chosen_label.clone();
                                            let span = cell.mode.format == ColorFormat::Ycbcr422;
                                            let title = reason.clone();
                                            view! {
                                                <label
                                                    class="out-cell"
                                                    class:refused=!ok
                                                    class:span=span
                                                    class:chosen=checked
                                                    title=title
                                                    on:mouseenter=move |_| hovered.set(reason.clone())
                                                >
                                                    <input
                                                        type="radio"
                                                        name="out-colour"
                                                        data-focus="1"
                                                        disabled=!ok
                                                        prop:checked=checked
                                                        on:change=move |_| {
                                                            let mut colours = colours.clone();
                                                            colours.insert(label.clone(), cell.mode);
                                                            set_draft(resolution, colours);
                                                        }
                                                    />
                                                    <span class="out-st">
                                                        {match (ok, checked, on_wire) {
                                                            (false, _, _) => "Olmaz",
                                                            (true, true, _) => "Seçili",
                                                            (true, false, true) => "Şu an",
                                                            (true, false, false) => "Seçilebilir",
                                                        }}
                                                        {span.then(|| " · 12 bit kapta")}
                                                    </span>
                                                    {(ok && cell.mode.carries_hdr()).then(|| view! { <span class="out-tag out-hdr">"HDR"</span> })}
                                                    <span class="out-mhz">{format!("{} MHz", cell.rate_khz / 1000)}</span>
                                                    <span class="out-meter">
                                                        <i style=format!("width:{fill:.1}%")></i>
                                                        <b style=format!("left:{tick:.1}%")></b>
                                                    </span>
                                                </label>
                                            }
                                        })
                                        .collect_view();
                                    view! {
                                        <span class="out-gf">{name}</span>
                                        {cells_view}
                                    }
                                })
                                .collect_view()}
                        </div>
                        <p class="fan-note">{move || {
                            let text = hovered.get();
                            if text.is_empty() { "Bir hücrenin üzerine gelince gerekçesi burada yazar.".to_string() } else { text }
                        }}</p>
                    </section>
                </div>

                <div class="actions-row fan-actions">
                    <Action
                        label="Uygula"
                        variant="primary"
                        disabled=Signal::derive(move || !unsaved || busy.get())
                        on_press=Callback::new(move |()| {
                            call(Request::OutputTry { resolution, colour: chosen_colour }, "");
                        })
                    />
                    <Action
                        label="Geri al"
                        disabled=Signal::derive(move || !unsaved)
                        on_press=Callback::new({
                            let kept = kept.clone();
                            move |()| {
                                open.set(None);
                                draft.set(Some((kept.resolution, kept.colours.clone())));
                            }
                        })
                    />
                    <Action
                        label="Otomatiğe dön"
                        on_press=Callback::new(move |()| {
                            open.set(None);
                            draft.set(Some(Default::default()));
                        })
                    />
                    <Action label="EDID bilgileri" on_press=Callback::new(move |()| edid.set(true)) />
                    <span class="fan-foot">
                        {if unsaved {
                            format!("Taslak ekrana gönderilmedi. Uygula'ya basınca {OUTPUT_TRIAL_SECONDS} saniye içinde onay istenir.")
                        } else {
                            "Seçim bu ekran için saklanır; başka bir ekran Otomatik ile açılır.".to_string()
                        }}
                    </span>
                </div>

                {found.trial.clone().map(|trial| {
                    let describe = |setting: &OutputSetting| {
                        offer
                            .resolve(setting.resolution)
                            .map(|mode| format!(
                                "{} · {}",
                                label_of(mode),
                                offer.colour(setting, mode).map(colour_text).unwrap_or_default()
                            ))
                            .unwrap_or_default()
                    };
                    let new = describe(&trial.setting);
                    let old = describe(&trial.previous);
                    view! {
                        <div class="overlay" data-focus-scope="1">
                            <div class="sheet fan-prompt">
                                <h2>"Bu görüntü kalsın mı?"</h2>
                                <p><b>{format!("Yeni: {new}")}</b></p>
                                <p class="panel-note">
                                    {move || format!(
                                        "Yanıt gelmezse {} saniye sonra önceki ayara dönülür: {old}. Süreyi denetim düzlemi tutar; ekran görüntü alamasa ya da arayüz kapansa bile geri dönülür.",
                                        now.get()
                                    )}
                                </p>
                                <div class="actions-row">
                                    <Action
                                        label="Koru"
                                        variant="primary"
                                        autofocus=true
                                        on_press=Callback::new(move |()| call(Request::OutputKeep, "Bu ekran için kaydedildi"))
                                    />
                                    <Action
                                        label="Geri dön"
                                        variant="ghost"
                                        on_press=Callback::new(move |()| call(Request::OutputRevert, "Önceki ayara dönüldü"))
                                    />
                                </div>
                            </div>
                        </div>
                    }
                })}

                {move || edid.get().then(|| {
                    let rows = edid_rows(&offer);
                    view! {
                        <div class="overlay" data-focus-scope="1">
                            <div class="sheet fan-prompt">
                                <h2>"EDID bilgileri"</h2>
                                <div class="fan-kv out-kv">
                                    {rows
                                        .into_iter()
                                        .map(|(k, v)| view! {
                                            <div class="fan-kv-row"><span>{k}</span><span class="fan-kv-value">{v}</span></div>
                                        })
                                        .collect_view()}
                                </div>
                                <div class="actions-row">
                                    <Action label="Kapat" variant="primary" autofocus=true on_press=Callback::new(move |()| edid.set(false)) />
                                </div>
                            </div>
                        </div>
                    }
                })}
            }
                .into_any()
        }}
    }
}

/// `3840×2160 · 59.94 Hz`
fn label_of(mode: &OutputModeOffer) -> String {
    format!(
        "{}×{} · {} Hz{}",
        mode.width,
        mode.height,
        hz_text(mode.refresh_mhz),
        if mode.interlaced { " (geçmeli)" } else { "" }
    )
}

fn hz_text(refresh_mhz: u32) -> String {
    let hz = f64::from(refresh_mhz) / 1000.0;
    if (hz - hz.round()).abs() < 0.005 {
        format!("{}", hz.round() as u32)
    } else {
        format!("{hz:.3}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn format_short(mode: ColorMode) -> String {
    match mode.format {
        ColorFormat::Rgb => "RGB",
        ColorFormat::Ycbcr444 => "4:4:4",
        ColorFormat::Ycbcr422 => "4:2:2",
        ColorFormat::Ycbcr420 => "4:2:0",
    }
    .into()
}

fn colour_text(mode: ColorMode) -> String {
    match mode.format {
        ColorFormat::Rgb => format!("RGB {} bit", mode.bits),
        _ => format!("YCbCr {} {} bit", format_short(mode), mode.bits),
    }
}

fn edid_rows(offer: &OutputOffer) -> Vec<(String, String)> {
    let link = &offer.link;
    let depths = |bits: &[u8]| {
        if bits.is_empty() {
            "yok".to_string()
        } else {
            bits.iter().map(|b| format!("{b} bit")).collect::<Vec<_>>().join(", ")
        }
    };
    let vics = |vics: &[u8]| vics.iter().map(u8::to_string).collect::<Vec<_>>().join(", ");
    vec![
        ("Ekran adı".into(), offer.sink_name.clone().unwrap_or_else(|| "—".into())),
        ("Bağlayıcı".into(), offer.connector.clone()),
        (
            "Bu girişin tavanı".into(),
            if link.max_character_rate_khz > 0 {
                format!("{} MHz · {}", link.max_character_rate_khz / 1000, link.declared_by)
            } else {
                "bildirilmedi".into()
            },
        ),
        ("HDMI".into(), if link.is_hdmi { "evet".into() } else { "hayır (DVI)".into() }),
        (
            "YCbCr".into(),
            match (link.ycbcr444, link.ycbcr422) {
                (true, true) => "4:4:4, 4:2:2".into(),
                (true, false) => "4:4:4".into(),
                (false, true) => "4:2:2".into(),
                (false, false) => "yok".into(),
            },
        ),
        ("Derin renk (RGB)".into(), depths(&link.rgb_deep)),
        (
            "4:2:0 modları (VIC)".into(),
            match (link.y420_only.is_empty(), link.y420_also.is_empty()) {
                (true, true) => "yok".into(),
                (false, _) => format!("{} · yalnız 4:2:0", vics(&link.y420_only)),
                (true, false) => format!("{} · 4:2:0 da olur", vics(&link.y420_also)),
            },
        ),
        ("4:2:0 derin renk".into(), depths(&link.ycbcr420_deep)),
        (
            "HDR aktarımı".into(),
            match (link.st2084, link.hlg) {
                (true, true) => "SMPTE ST 2084 (HDR10), HLG".into(),
                (true, false) => "SMPTE ST 2084 (HDR10)".into(),
                (false, true) => "HLG".into(),
                (false, false) => "yok".into(),
            },
        ),
        ("Kimlik (checksum)".into(), offer.sink.clone()),
        (
            "Kaynak (bu kart)".into(),
            format!("{} MHz · {} bit · RGB, 4:4:4, 4:2:2, 4:2:0", link.source_max_khz / 1000, link.source_max_bits),
        ),
    ]
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
