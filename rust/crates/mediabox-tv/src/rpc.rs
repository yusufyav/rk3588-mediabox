//! Talking to the control plane.
//!
//! mediaboxd-rs is the only thing this interface talks to. Not the media
//! worker, not the streaming server, not Kodi: those are all loopback services
//! the daemon owns, and going round it would mean reimplementing decisions it
//! already makes.
//!
//! The transport is the daemon's own socket rather than its HTTP listener: one
//! line of JSON in, one line of JSON out, over /run/mediabox/mediaboxd.sock.
//! That is what mediaboxctl uses, and the request vocabulary is a closed enum
//! in mediabox-core, so there is no way to ask for something that is not a
//! command.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub const DEFAULT_SOCKET: &str = "/run/mediabox/mediaboxd.sock";

/// The daemon refuses a request over 64 KiB, so a reply larger than that is a
/// bug on one side or the other rather than something to keep reading.
const MAX_LINE: u64 = 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Malformed(serde_json::Error),
    /// The daemon answered, and the answer was no.
    Refused {
        code: String,
        message: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "denetim düzlemine ulaşılamadı: {e}"),
            Error::Malformed(e) => write!(f, "yanıt çözülemedi: {e}"),
            Error::Refused { message, .. } => write!(f, "{message}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Malformed(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Self {
            socket: socket.as_ref().to_path_buf(),
        }
    }

    /// One request, one connection. The daemon closes after answering anything
    /// but input_monitor, and holding a socket open across a Kodi handover only
    /// means discovering it is dead at the worst moment.
    pub async fn call(&self, request: Value) -> Result<Value> {
        let stream = UnixStream::connect(&self.socket).await?;
        let (read_half, mut write_half) = stream.into_split();

        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        write_half.write_all(&line).await?;
        write_half.flush().await?;

        let mut reader = BufReader::new(read_half).take(MAX_LINE);
        let mut answer = String::new();
        reader.read_line(&mut answer).await?;

        let envelope: Envelope = serde_json::from_str(answer.trim_end())?;
        if envelope.ok {
            Ok(envelope.result.unwrap_or(Value::Null))
        } else {
            let error = envelope.error.unwrap_or(WireError {
                code: "UNKNOWN".into(),
                message: "denetim düzlemi bir neden bildirmedi".into(),
            });
            Err(Error::Refused {
                code: error.code,
                message: error.message,
            })
        }
    }

    pub async fn typed<T: DeserializeOwned>(&self, request: Value) -> Result<T> {
        let value = self.call(request).await?;
        Ok(serde_json::from_value(value)?)
    }

    pub async fn home(&self) -> Result<crate::model::HomeRows> {
        self.typed(json!({"command": "media_home"})).await
    }

    pub async fn library(&self) -> Result<crate::model::LibraryListing> {
        self.typed(json!({"command": "media_library"})).await
    }

    pub async fn meta(&self, kind: &str, id: &str) -> Result<crate::model::MetaEnvelope> {
        self.typed(json!({"command": "media_meta", "media_type": kind, "id": id}))
            .await
    }

    pub async fn library_item(&self, id: &str) -> Result<crate::model::LibraryItemEnvelope> {
        self.typed(json!({"command": "media_library_item", "id": id}))
            .await
    }

    /// Kept as a raw value as well as a parsed one: the exact stream descriptor
    /// an addon sent is what has to be handed back when the viewer picks it,
    /// and a round trip through our own struct would drop the fields we do not
    /// model.
    pub async fn streams(&self, kind: &str, id: &str) -> Result<Value> {
        self.call(json!({"command": "media_streams", "media_type": kind, "id": id}))
            .await
    }

    pub async fn search(&self, query: &str) -> Result<crate::model::SearchResults> {
        self.typed(json!({"command": "media_search", "query": query}))
            .await
    }

    pub async fn plan_for_stream(&self, stream: &Value) -> Result<crate::model::Plan> {
        self.typed(json!({"command": "media_stream_plan", "stream": stream}))
            .await
    }

    pub async fn plan_for_url(&self, url: &str) -> Result<crate::model::Plan> {
        self.typed(json!({"command": "media_policy", "url": url}))
            .await
    }

    pub async fn play_here(
        &self,
        url: Option<&str>,
        stream: Option<&Value>,
        start_seconds: u64,
        title: Option<&str>,
        duration_seconds: Option<u64>,
    ) -> Result<Value> {
        let mut request = json!({"command": "media_play_here", "start_seconds": start_seconds});
        // What the catalogue knows and the player cannot: see the request's own
        // documentation. Sent at the start rather than asked for later, because
        // by the time anything is playing the answer is already needed.
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            request["title"] = json!(title);
        }
        if let Some(seconds) = duration_seconds.filter(|seconds| *seconds > 0) {
            request["duration_seconds"] = json!(seconds);
        }
        if let Some(url) = url {
            request["url"] = json!(url);
        } else if let Some(stream) = stream {
            request["stream"] = stream.clone();
        }
        self.call(request).await
    }

    /// The interface's own player, while it is running. Stop first, because
    /// it is the one a remote reaches by pressing Back.
    /// Hands a film that is playing here over to Kodi, at the second it had
    /// reached. The daemon reads the position off the player, stops it
    /// cleanly, and only then starts Kodi — and it stops this process as part
    /// of handing the display over, so nothing after this call is guaranteed
    /// to run.
    pub async fn handoff_to_kodi(&self) -> Result<Value> {
        self.call(json!({"command": "media_handoff_to_kodi"})).await
    }

    pub async fn stop_here(&self) -> Result<Value> {
        self.call(json!({"command": "media_stop_here"})).await
    }

    pub async fn status_here(&self) -> Result<Value> {
        self.call(json!({"command": "media_status_here"})).await
    }

    pub async fn transport_here(&self, action: Value) -> Result<Value> {
        self.call(json!({"command": "media_transport_here", "action": action}))
            .await
    }

    /// Opens an address in the box's browser application. The control plane
    /// whitelists the scheme and starts the unit; this interface does not get
    /// to run a browser, and does not want to.
    pub async fn browser_open(&self, url: &str) -> Result<Value> {
        self.call(json!({"command": "browser_open", "url": url}))
            .await
    }

    pub async fn kodi_status(&self) -> Result<Value> {
        self.call(json!({"command": "kodi_status"})).await
    }

    pub async fn kodi_play_pause(&self) -> Result<Value> {
        self.call(json!({"command": "kodi_play_pause"})).await
    }

    pub async fn kodi_stop(&self) -> Result<Value> {
        self.call(json!({"command": "kodi_stop"})).await
    }

    pub async fn kodi_seek(&self, seconds: i64) -> Result<Value> {
        self.call(json!({"command": "kodi_seek", "seconds": seconds}))
            .await
    }

    pub async fn kodi_restart(&self) -> Result<Value> {
        self.call(json!({"command": "kodi_restart"})).await
    }

    /// CEC, which the daemon owns. This interface never opens /dev/cec0: one
    /// process may hold that adapter and it is not this one.
    pub async fn cec_wake_tv(&self) -> Result<Value> {
        self.call(json!({"command": "cec_wake_tv"})).await
    }

    pub async fn cec_standby_tv(&self) -> Result<Value> {
        self.call(json!({"command": "cec_standby_tv"})).await
    }

    /// The board's indicator lights, which the daemon owns for the same reason
    /// it owns the CEC adapter: `/sys/class/leds` is root's, and this process
    /// runs under a unit that mounts /sys read-only. The daemon also remembers
    /// the choice, so this is a request and not a write.
    /// Remember a colour mode choice, or `None` for the measured default.
    /// The daemon owns it because the state directory is the daemon's and this
    /// process's unit mounts /sys read-only.
    pub async fn display_color_mode_set(
        &self,
        mode: Option<(mediabox_core::ColorFormat, u8)>,
    ) -> Result<Value> {
        let choice = match mode {
            None => json!({"kind": "auto"}),
            Some((format, bits)) => json!({"kind": "fixed", "format": format, "bits": bits}),
        };
        self.call(json!({"command": "display_color_mode_set", "choice": choice}))
            .await
    }

    /// Save a fan curve for the next boot. The daemon validates it and writes
    /// the overlay; the kernel's pwm-fan is what drives the fan either way.
    pub async fn fan_curve_set(&self, curve: &mediabox_core::FanCurve) -> Result<Value> {
        if curve.profile == mediabox_core::FanProfile::Custom {
            self.call(json!({
                "command": "fan_curve_set",
                "profile": curve.profile,
                "points": curve.points,
            }))
            .await
        } else {
            self.call(json!({"command": "fan_curve_set", "profile": curve.profile}))
                .await
        }
    }

    pub async fn fan_curve_reset(&self) -> Result<Value> {
        self.call(json!({"command": "fan_curve_reset"})).await
    }

    pub async fn leds_set(&self, mode: mediabox_core::LedMode) -> Result<Value> {
        self.call(json!({"command": "leds_set", "mode": mode}))
            .await
    }

    /// Restart or shut the appliance down. Called from exactly one place — a
    /// confirmation the viewer moved the focus onto and pressed — and the
    /// request body is a two-valued enum on both sides of the socket.
    pub async fn system_power(&self, action: mediabox_core::PowerAction) -> Result<Value> {
        self.call(json!({"command": "system_power", "action": action}))
            .await
    }

    pub async fn status(&self) -> Result<Value> {
        self.call(json!({"command": "status"})).await
    }

    // ------------------------------------------------------------ the radios
    //
    // All of these are the daemon's: rfkill, netplan and bluetoothctl are
    // root's, and this process runs under a unit that mounts /sys read-only.
    // The passphrase travels the same way a Stremio password does — a field in
    // a JSON body over a socket owned by root, never a process argument and
    // never printed.

    pub async fn wifi_status(&self) -> Result<Value> {
        self.call(json!({"command": "wifi_status"})).await
    }

    pub async fn wifi_scan(&self) -> Result<Value> {
        self.call(json!({"command": "wifi_scan"})).await
    }

    pub async fn wifi_connect(&self, ssid: &str, psk: Option<&str>) -> Result<Value> {
        let mut body = json!({"command": "wifi_connect", "ssid": ssid});
        if let Some(psk) = psk {
            body["psk"] = json!(psk);
        }
        self.call(body).await
    }

    pub async fn wifi_forget(&self, ssid: &str) -> Result<Value> {
        self.call(json!({"command": "wifi_forget", "ssid": ssid}))
            .await
    }

    pub async fn wifi_power(&self, on: bool) -> Result<Value> {
        self.call(json!({"command": "wifi_power", "on": on})).await
    }

    pub async fn bluetooth_status(&self) -> Result<Value> {
        self.call(json!({"command": "bluetooth_status"})).await
    }

    pub async fn bluetooth_scan(&self) -> Result<Value> {
        self.call(json!({"command": "bluetooth_scan"})).await
    }

    pub async fn bluetooth_power(&self, on: bool) -> Result<Value> {
        self.call(json!({"command": "bluetooth_power", "on": on}))
            .await
    }

    pub async fn bluetooth_pair(&self, address: &str) -> Result<Value> {
        self.call(json!({"command": "bluetooth_pair", "address": address}))
            .await
    }

    pub async fn bluetooth_connect(&self, address: &str) -> Result<Value> {
        self.call(json!({"command": "bluetooth_connect", "address": address}))
            .await
    }

    pub async fn bluetooth_disconnect(&self, address: &str) -> Result<Value> {
        self.call(json!({"command": "bluetooth_disconnect", "address": address}))
            .await
    }

    pub async fn bluetooth_remove(&self, address: &str) -> Result<Value> {
        self.call(json!({"command": "bluetooth_remove", "address": address}))
            .await
    }

    /// What this box can run, and which of them owns the television.
    pub async fn applications(&self) -> Result<crate::model::DisplayStatus> {
        self.typed(json!({"command": "applications"})).await
    }

    /// Put one application on the television. This interface is itself one of
    /// them, so choosing another ends this process: the display changes hands.
    pub async fn application_launch(&self, id: &str) -> Result<Value> {
        self.call(json!({"command": "application_launch", "id": id}))
            .await
    }

    pub async fn diagnostics(&self) -> Result<Value> {
        self.call(json!({"command": "diagnostics"})).await
    }

    /// Sign in to a Stremio account.
    ///
    /// The password crosses this socket once and is not kept on either side:
    /// the media core exchanges it for an auth key, stores the key, and the
    /// interface forgets the password as soon as this returns. It is a field in
    /// a JSON body and never a process argument — nothing here builds a command
    /// line, so there is no `ps` output and no shell history to leak into.
    pub async fn media_login(&self, email: &str, password: &str) -> Result<Value> {
        self.call(json!({
            "command": "media_login",
            "email": email,
            "password": password,
        }))
        .await
    }

    pub async fn media_logout(&self) -> Result<Value> {
        self.call(json!({"command": "media_logout"})).await
    }
}

#[derive(serde::Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(serde::Deserialize)]
struct WireError {
    code: String,
    message: String,
}
