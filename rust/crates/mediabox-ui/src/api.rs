//! The control plane, as the UI sees it.
//!
//! Every screen goes through `POST /v1/control` on the daemon that served the
//! page. There is no second base URL and no direct call to the media worker or
//! to Kodi: the same origin that delivered this bundle is the only authority
//! the UI knows about, which is what keeps a phone on the LAN and the
//! television running exactly the same code.

use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, Response};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    fn local(message: impl Into<String>) -> Self {
        Self {
            code: "UI_ERROR".into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Debug, Deserialize)]
struct WireError {
    code: String,
    message: String,
}

fn js_message(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(error, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| "ağ hatası".into())
}

/// Send one typed command and return its result payload.
pub async fn control(request: Value) -> Result<Value, ApiError> {
    let window = web_sys::window().ok_or_else(|| ApiError::local("tarayıcı penceresi yok"))?;
    let options = RequestInit::new();
    options.set_method("POST");
    options.set_body(&JsValue::from_str(&request.to_string()));
    let headers = Headers::new().map_err(|e| ApiError::local(js_message(&e)))?;
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| ApiError::local(js_message(&e)))?;
    options.set_headers(&headers);

    let request = Request::new_with_str_and_init("/v1/control", &options)
        .map_err(|e| ApiError::local(js_message(&e)))?;
    let response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| {
            ApiError::local(format!("denetim düzlemine ulaşılamadı: {}", js_message(&e)))
        })?;
    let response: Response = response
        .dyn_into()
        .map_err(|_| ApiError::local("beklenmeyen yanıt türü"))?;
    let text = JsFuture::from(
        response
            .text()
            .map_err(|e| ApiError::local(js_message(&e)))?,
    )
    .await
    .map_err(|e| ApiError::local(js_message(&e)))?;
    let text = text
        .as_string()
        .ok_or_else(|| ApiError::local("yanıt gövdesi metin değil"))?;

    let envelope: Envelope = serde_json::from_str(&text)
        .map_err(|error| ApiError::local(format!("yanıt çözümlenemedi: {error}")))?;
    if envelope.ok {
        return Ok(envelope.result.unwrap_or(Value::Null));
    }
    Err(envelope.error.map_or_else(
        || ApiError::local("denetim düzlemi tanımsız bir hata döndürdü"),
        |error| ApiError {
            code: error.code,
            message: error.message,
        },
    ))
}

/// Send a command and deserialize its result into `T`.
pub async fn typed<T: serde::de::DeserializeOwned>(request: Value) -> Result<T, ApiError> {
    let value = control(request).await?;
    serde_json::from_value(value)
        .map_err(|error| ApiError::local(format!("yanıt beklenen biçimde değil: {error}")))
}

// --------------------------------------------------------------- commands

pub fn status() -> Value {
    json!({"command": "status"})
}

pub fn diagnostics() -> Value {
    json!({"command": "diagnostics"})
}

/// What the television can be sent, measured from its EDID.
pub fn display_color_modes() -> Value {
    json!({"command": "display_color_modes"})
}

/// Fix the colour mode, or `None` to go back to the measured default.
pub fn display_color_mode_set(format: Option<(&str, u8)>) -> Value {
    let choice = match format {
        None => json!({"kind": "auto"}),
        Some((format, bits)) => json!({"kind": "fixed", "format": format, "bits": bits}),
    };
    json!({"command": "display_color_mode_set", "choice": choice})
}

/// The fan as the kernel runs it, and the curve chosen for the next boot.
pub fn fan_status() -> Value {
    json!({"command": "fan_status"})
}

/// Save a curve for the next boot. A preset is named; only a custom curve
/// carries its points. The daemon checks it again whatever this side did.
pub fn fan_curve_set(curve: &mediabox_core::FanCurve) -> Value {
    if curve.profile == mediabox_core::FanProfile::Custom {
        json!({"command": "fan_curve_set", "profile": curve.profile, "points": curve.points})
    } else {
        json!({"command": "fan_curve_set", "profile": curve.profile})
    }
}

/// Back to the board's own curve from the next boot on.
pub fn fan_curve_reset() -> Value {
    json!({"command": "fan_curve_reset"})
}

/// Restart the appliance. The same closed request the television's power
/// sheet sends; here it is reached only from the row a person pressed after
/// saving a curve that needs it.
pub fn system_restart() -> Value {
    json!({"command": "system_power", "action": "restart"})
}

pub fn media_home() -> Value {
    json!({"command": "media_home"})
}

/// One catalogue in full, rather than the handful a shelf shows.
pub fn media_catalog(kind: &str, id: &str, addon: Option<&str>, limit: Option<u32>) -> Value {
    let mut request = json!({"command": "media_catalog", "media_type": kind, "id": id});
    if let Some(addon) = addon {
        request["addon_id"] = json!(addon);
    }
    if let Some(limit) = limit {
        request["limit"] = json!(limit);
    }
    request
}

pub fn media_library() -> Value {
    json!({"command": "media_library"})
}

pub fn media_library_item(id: &str) -> Value {
    json!({"command": "media_library_item", "id": id})
}

/// Sign in to a Stremio account. The password crosses loopback once and is not
/// stored by anything on the path; what the media core keeps is the auth key.
pub fn media_login(email: &str, password: &str) -> Value {
    json!({"command": "media_login", "email": email, "password": password})
}

pub fn media_logout() -> Value {
    json!({"command": "media_logout"})
}

pub fn media_search(query: &str) -> Value {
    json!({"command": "media_search", "query": query})
}

pub fn media_meta(media_type: &str, id: &str) -> Value {
    json!({"command": "media_meta", "media_type": media_type, "id": id})
}

pub fn media_streams(media_type: &str, id: &str) -> Value {
    json!({"command": "media_streams", "media_type": media_type, "id": id})
}

pub fn media_stream_plan(stream: &Value) -> Value {
    json!({"command": "media_stream_plan", "stream": stream})
}

pub fn media_policy(url: &str) -> Value {
    json!({"command": "media_policy", "url": url})
}

pub fn media_capabilities() -> Value {
    json!({"command": "media_capabilities"})
}

pub fn media_sessions() -> Value {
    json!({"command": "media_sessions"})
}

pub fn media_session_start(url: &str) -> Value {
    json!({"command": "media_session_start", "url": url})
}

pub fn media_session_stop(id: &str) -> Value {
    json!({"command": "media_session_stop", "id": id})
}

pub fn play_on_kodi(url: Option<&str>, stream: Option<&Value>, start_seconds: u64) -> Value {
    let mut request = json!({"command": "media_play_on_kodi", "start_seconds": start_seconds});
    if let Some(url) = url {
        request["url"] = json!(url);
    }
    if let Some(stream) = stream {
        request["stream"] = stream.clone();
    }
    request
}

pub fn kodi_status() -> Value {
    json!({"command": "kodi_status"})
}

pub fn kodi_play_pause() -> Value {
    json!({"command": "kodi_play_pause"})
}

pub fn kodi_stop() -> Value {
    json!({"command": "kodi_stop"})
}

pub fn kodi_seek(seconds: i64) -> Value {
    json!({"command": "kodi_seek", "seconds": seconds})
}

pub fn cec_status() -> Value {
    json!({"command": "cec_status"})
}

pub fn cec_devices() -> Value {
    json!({"command": "cec_devices"})
}

pub fn cec_wake_tv() -> Value {
    json!({"command": "cec_wake_tv"})
}

pub fn cec_standby_tv() -> Value {
    json!({"command": "cec_standby_tv"})
}

/// What this box can run, and what it is running.
pub fn applications() -> Value {
    json!({"command": "applications"})
}

/// Put one application on the television. This interface is itself one of
/// them, so choosing another ends this page: the display changes hands.
pub fn application_launch(id: &str) -> Value {
    json!({"command": "application_launch", "id": id})
}

/// Put one web address in front of the television's browser and give it the
/// display. The browser has no address bar, so this is the only way in.
/// Play it here, in the interface's own player.
pub fn play_here(url: Option<&str>, stream: Option<&Value>, start_seconds: u64) -> Value {
    let mut request = json!({"command": "media_play_here", "start_seconds": start_seconds});
    if let Some(url) = url {
        request["url"] = json!(url);
    }
    if let Some(stream) = stream {
        request["stream"] = stream.clone();
    }
    request
}

/// Send what is playing here to Kodi, at the second it had reached.
pub fn handoff_to_kodi() -> Value {
    json!({"command": "media_handoff_to_kodi"})
}

pub fn browser_open(url: &str) -> Value {
    json!({"command": "browser_open", "url": url})
}

pub fn surface_switch(target: &str) -> Value {
    json!({"command": "surface_switch", "target": target})
}
