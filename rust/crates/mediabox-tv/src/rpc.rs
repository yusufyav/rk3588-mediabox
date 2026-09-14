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
    Refused { code: String, message: String },
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
        Self { socket: socket.as_ref().to_path_buf() }
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
            Err(Error::Refused { code: error.code, message: error.message })
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
        self.typed(json!({"command": "media_meta", "media_type": kind, "id": id})).await
    }

    pub async fn library_item(&self, id: &str) -> Result<crate::model::LibraryItemEnvelope> {
        self.typed(json!({"command": "media_library_item", "id": id})).await
    }

    /// Kept as a raw value as well as a parsed one: the exact stream descriptor
    /// an addon sent is what has to be handed back when the viewer picks it,
    /// and a round trip through our own struct would drop the fields we do not
    /// model.
    pub async fn streams(&self, kind: &str, id: &str) -> Result<Value> {
        self.call(json!({"command": "media_streams", "media_type": kind, "id": id})).await
    }

    pub async fn search(&self, query: &str) -> Result<crate::model::SearchResults> {
        self.typed(json!({"command": "media_search", "query": query})).await
    }

    pub async fn plan_for_stream(&self, stream: &Value) -> Result<crate::model::Plan> {
        self.typed(json!({"command": "media_stream_plan", "stream": stream})).await
    }

    pub async fn plan_for_url(&self, url: &str) -> Result<crate::model::Plan> {
        self.typed(json!({"command": "media_policy", "url": url})).await
    }

    /// Hands the television to Kodi and starts the film. The daemon stops this
    /// process as part of doing it, so nothing after this call is guaranteed to
    /// run.
    pub async fn play_on_kodi(
        &self,
        url: Option<&str>,
        stream: Option<&Value>,
        start_seconds: u64,
    ) -> Result<Value> {
        let mut request = json!({"command": "media_play_on_kodi", "start_seconds": start_seconds});
        if let Some(url) = url {
            request["url"] = json!(url);
        } else if let Some(stream) = stream {
            request["stream"] = stream.clone();
        }
        self.call(request).await
    }

    pub async fn play_here(
        &self,
        url: Option<&str>,
        stream: Option<&Value>,
        start_seconds: u64,
    ) -> Result<Value> {
        let mut request = json!({"command": "media_play_here", "start_seconds": start_seconds});
        if let Some(url) = url {
            request["url"] = json!(url);
        } else if let Some(stream) = stream {
            request["stream"] = stream.clone();
        }
        self.call(request).await
    }

    /// Opens an address in the box's browser application. The control plane
    /// whitelists the scheme and starts the unit; this interface does not get
    /// to run a browser, and does not want to.
    pub async fn browser_open(&self, url: &str) -> Result<Value> {
        self.call(json!({"command": "browser_open", "url": url})).await
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
        self.call(json!({"command": "kodi_seek", "seconds": seconds})).await
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

    /// Restart or shut the appliance down. Called from exactly one place — a
    /// confirmation the viewer moved the focus onto and pressed — and the
    /// request body is a two-valued enum on both sides of the socket.
    pub async fn system_power(&self, action: mediabox_core::PowerAction) -> Result<Value> {
        self.call(json!({"command": "system_power", "action": action})).await
    }

    pub async fn status(&self) -> Result<Value> {
        self.call(json!({"command": "status"})).await
    }

    /// What this box can run, and which of them owns the television.
    pub async fn applications(&self) -> Result<crate::model::DisplayStatus> {
        self.typed(json!({"command": "applications"})).await
    }

    /// Put one application on the television. This interface is itself one of
    /// them, so choosing another ends this process: the display changes hands.
    pub async fn application_launch(&self, id: &str) -> Result<Value> {
        self.call(json!({"command": "application_launch", "id": id})).await
    }

    pub async fn diagnostics(&self) -> Result<Value> {
        self.call(json!({"command": "diagnostics"})).await
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
