use crate::kodi::KodiClient;
use crate::lifecycle::KodiLifecycle;
use crate::media::MediaClient;
use mediabox_cec::Adapter;
use mediabox_core::{CecStatus, InputSource, Request, Response, ServiceHealth, SystemStatus};
use mediabox_input::{InputManager, KodiRoute, RouteDecision};
use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener, UnixStream};

const MAX_REQUEST_BYTES: usize = 64 * 1024;

pub struct CecRuntime {
    pub adapter: Option<Arc<Adapter>>,
    pub unavailable: CecStatus,
}

impl CecRuntime {
    pub fn status(&self) -> CecStatus {
        self.adapter
            .as_ref()
            .map_or_else(|| self.unavailable.clone(), |adapter| adapter.status())
    }
}

pub struct AppState {
    pub kodi: Arc<KodiClient>,
    pub lifecycle: KodiLifecycle,
    pub cec: CecRuntime,
    pub input: InputManager,
    pub media: Arc<MediaClient>,
}

impl AppState {
    pub async fn handle(&self, request: Request) -> Response {
        match request {
            Request::Status => Response::success(self.status().await),
            Request::System => Response::success(system_snapshot()),
            Request::KodiStatus => Response::success(self.kodi.status().await),
            Request::KodiPlayPause => result(self.kodi.play_pause().await),
            Request::KodiStop => result(self.kodi.stop().await),
            Request::KodiSeek { seconds } => result(self.kodi.seek_relative(seconds).await),
            Request::KodiOpen {
                url,
                resume_seconds,
            } => result(self.kodi.open(&url, resume_seconds).await),
            Request::KodiRestart => match self.lifecycle.restart().await {
                Ok(()) => Response::success(json!({"restarted":true})),
                Err(error) => Response::failure("KODI_LIFECYCLE_ERROR", error.to_string()),
            },
            Request::CecStatus => Response::success(self.cec.status()),
            Request::CecDevices => {
                let Some(adapter) = self.cec.adapter.clone() else {
                    return Response::failure("CEC_UNAVAILABLE", cec_error(&self.cec.status()));
                };
                match tokio::task::spawn_blocking(move || adapter.discover_devices()).await {
                    Ok(Ok(devices)) => Response::success(devices),
                    Ok(Err(error)) => Response::failure("CEC_ERROR", error.to_string()),
                    Err(error) => Response::failure("INTERNAL_ERROR", error.to_string()),
                }
            }
            Request::CecActiveSource => {
                cec_action(&self.cec, |adapter| adapter.active_source()).await
            }
            Request::CecWakeTv => cec_action(&self.cec, |adapter| adapter.wake_tv()).await,
            Request::CecStandbyTv => cec_action(&self.cec, |adapter| adapter.standby_tv()).await,
            Request::MediaStatus => media_result(self.media.status().await),
            Request::MediaSearch { query } => media_result(self.media.search(&query).await),
            Request::MediaInspect { url } => media_result(self.media.inspect(&url).await),
            Request::MediaStreams { media_type, id } => {
                media_result(self.media.streams(&media_type, &id).await)
            }
            Request::MediaPolicy { url } => media_result(self.media.policy(&url).await),
            Request::MediaSessions => media_result(self.media.sessions().await),
            Request::MediaSessionStart { url } => {
                media_result(self.media.session_start(&url).await)
            }
            Request::MediaSessionStop { id } => media_result(self.media.session_stop(&id).await),
            Request::InputInject { action } => {
                let decision = self.input.publish(action, InputSource::Api, true, None);
                if let Err(error) = apply_kodi_route(&self.kodi, decision).await {
                    return Response::failure("INPUT_ROUTE_ERROR", error);
                }
                Response::success(
                    json!({"accepted":true,"action":action,"route":format!("{decision:?}")}),
                )
            }
            Request::InputMonitor => {
                Response::failure("PROTOCOL_ERROR", "input.monitor akış komutudur")
            }
        }
    }

    async fn status(&self) -> SystemStatus {
        let kodi = self.kodi.status().await;
        let cec = self.cec.status();
        let system = system_snapshot();
        let media = match self.media.status().await {
            Ok(value) => value,
            Err(error) => {
                json!({"available":false,"error":{"code":"MEDIA_WORKER_UNAVAILABLE","message":error.to_string()}})
            }
        };
        let input_devices = mediabox_input::enumerate()
            .into_iter()
            .filter_map(|item| serde_json::to_value(item).ok())
            .collect();
        let services = vec![
            ServiceHealth {
                name: "mediaboxd-rs".into(),
                healthy: true,
                detail: None,
            },
            ServiceHealth {
                name: "kodi".into(),
                healthy: kodi.jsonrpc_reachable,
                detail: kodi.error.clone(),
            },
            ServiceHealth {
                name: "cec".into(),
                healthy: cec.available,
                detail: cec.error.clone(),
            },
            ServiceHealth {
                name: "media-worker".into(),
                healthy: media.get("available").and_then(Value::as_bool) != Some(false),
                detail: media
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
        ];
        SystemStatus {
            version: env!("CARGO_PKG_VERSION").into(),
            hostname: string_field(&system, "hostname"),
            kernel: string_field(&system, "kernel"),
            architecture: std::env::consts::ARCH.into(),
            uptime_seconds: system
                .get("uptime_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            input_mode: self.input.mode(),
            input_devices,
            services,
            kodi,
            cec,
            media,
        }
    }
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn cec_error(status: &CecStatus) -> String {
    status
        .error
        .clone()
        .unwrap_or_else(|| "CEC kullanılamıyor".into())
}

async fn cec_action<F>(runtime: &CecRuntime, action: F) -> Response
where
    F: FnOnce(&Adapter) -> Result<(), mediabox_cec::CecError> + Send + 'static,
{
    let Some(adapter) = runtime.adapter.clone() else {
        return Response::failure("CEC_UNAVAILABLE", cec_error(&runtime.status()));
    };
    match tokio::task::spawn_blocking(move || action(&adapter)).await {
        Ok(Ok(())) => Response::success(json!({"transmitted":true})),
        Ok(Err(error)) => Response::failure("CEC_ERROR", error.to_string()),
        Err(error) => Response::failure("INTERNAL_ERROR", error.to_string()),
    }
}

fn result(value: Result<Value, crate::kodi::KodiError>) -> Response {
    match value {
        Ok(value) => Response::success(value),
        Err(error) => Response::failure("KODI_ERROR", error.to_string()),
    }
}

fn media_result(value: Result<Value, crate::media::MediaError>) -> Response {
    match value {
        Ok(value) => Response::success(value),
        Err(error) => Response::failure("MEDIA_WORKER_ERROR", error.to_string()),
    }
}

pub async fn apply_kodi_route(kodi: &KodiClient, decision: RouteDecision) -> Result<(), String> {
    let RouteDecision::Kodi(route) = decision else {
        return Ok(());
    };
    let response = match route {
        KodiRoute::PlayPause => kodi.play_pause().await,
        KodiRoute::Play => kodi.set_playing(true).await,
        KodiRoute::Pause => kodi.set_playing(false).await,
        KodiRoute::Stop => kodi.stop().await,
        KodiRoute::Seek(seconds) => kodi.seek_relative(seconds).await,
        // Kodi Application.SetVolume supports increment/decrement and mute is a separate call.
        KodiRoute::VolumeUp => {
            kodi.call("Application.SetVolume", Some(json!({"volume":"increment"})))
                .await
        }
        KodiRoute::VolumeDown => {
            kodi.call("Application.SetVolume", Some(json!({"volume":"decrement"})))
                .await
        }
        KodiRoute::Mute => {
            kodi.call("Application.SetMute", Some(json!({"mute":"toggle"})))
                .await
        }
    };
    response.map(|_| ()).map_err(|error| error.to_string())
}

pub async fn serve_unix(listener: UnixListener, state: Arc<AppState>) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let _ = handle_unix(stream, state).await;
        });
    }
}

async fn handle_unix(stream: UnixStream, state: Arc<AppState>) -> io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut line = String::new();
    let count = read.read_line(&mut line).await?;
    if count == 0 {
        return Ok(());
    }
    if count > MAX_REQUEST_BYTES || !line.ends_with('\n') {
        return write_response(
            &mut write,
            &Response::failure("INVALID_REQUEST", "istek çok büyük veya satır sonu yok"),
        )
        .await;
    }
    let request: Request = match serde_json::from_str(&line) {
        Ok(value) => value,
        Err(error) => {
            return write_response(
                &mut write,
                &Response::failure("INVALID_REQUEST", error.to_string()),
            )
            .await;
        }
    };
    if request == Request::InputMonitor {
        let mut events = state.input.subscribe();
        write_response(&mut write, &Response::success(json!({"monitoring":true}))).await?;
        loop {
            match events.recv().await {
                Ok(event) => {
                    write
                        .write_all(
                            format!("{}\n", serde_json::to_string(&event).expect("event JSON"))
                                .as_bytes(),
                        )
                        .await?
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    write
                        .write_all(
                            format!("{}\n", json!({"warning":"lagged","events":count})).as_bytes(),
                        )
                        .await?
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
    write_response(&mut write, &state.handle(request).await).await
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    response: &Response,
) -> io::Result<()> {
    write
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(response).expect("response JSON")
            )
            .as_bytes(),
        )
        .await
}

pub async fn serve_http(listener: TcpListener, state: Arc<AppState>) -> io::Result<()> {
    loop {
        let (mut stream, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let state = state.clone();
        tokio::spawn(async move {
            let mut data = Vec::with_capacity(4096);
            let mut chunk = [0u8; 4096];
            loop {
                let Ok(n) = stream.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                data.extend_from_slice(&chunk[..n]);
                if data.len() > MAX_REQUEST_BYTES {
                    let _ = http_response(
                        &mut stream,
                        413,
                        &Response::failure("INVALID_REQUEST", "istek çok büyük"),
                    )
                    .await;
                    return;
                }
                if let Some(header_end) = find_header_end(&data) {
                    let headers = String::from_utf8_lossy(&data[..header_end]);
                    let first = headers.lines().next().unwrap_or_default();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if first != "POST /v1/control HTTP/1.1" {
                        let _ = http_response(
                            &mut stream,
                            404,
                            &Response::failure("NOT_FOUND", "yalnız POST /v1/control desteklenir"),
                        )
                        .await;
                        return;
                    }
                    if data.len() < header_end + 4 + length {
                        continue;
                    }
                    let body = &data[header_end + 4..header_end + 4 + length];
                    let response = match serde_json::from_slice::<Request>(body) {
                        Ok(Request::InputMonitor) => {
                            Response::failure("INVALID_REQUEST", "HTTP streaming desteklenmiyor")
                        }
                        Ok(request) => state.handle(request).await,
                        Err(error) => Response::failure("INVALID_REQUEST", error.to_string()),
                    };
                    let status = if response.ok { 200 } else { 400 };
                    let _ = http_response(&mut stream, status, &response).await;
                    return;
                }
            }
        });
    }
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    response: &Response,
) -> io::Result<()> {
    let body = serde_json::to_vec(response).expect("response JSON");
    let reason = if status == 200 {
        "OK"
    } else if status == 404 {
        "Not Found"
    } else if status == 413 {
        "Payload Too Large"
    } else {
        "Bad Request"
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await
}

pub fn system_snapshot() -> Value {
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_else(|_| "unknown".into())
        .trim()
        .to_string();
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_else(|_| "unknown".into())
        .trim()
        .to_string();
    let uptime_seconds = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    json!({"hostname":hostname,"kernel":kernel,"architecture":std::env::consts::ARCH,"uptime_seconds":uptime_seconds})
}

pub fn socket_is_live(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::KodiLifecycle;
    use mediabox_core::{CecStatus, InputMode};
    use std::time::Duration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn unix_socket_request_returns_structured_response() {
        let dir = tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let state = Arc::new(AppState {
            kodi: Arc::new(
                KodiClient::new("http://127.0.0.1:9/jsonrpc", Duration::from_millis(20)).unwrap(),
            ),
            lifecycle: KodiLifecycle::new("kodi.service").unwrap(),
            cec: CecRuntime {
                adapter: None,
                unavailable: CecStatus {
                    error: Some("testte kapalı".into()),
                    ..Default::default()
                },
            },
            input: InputManager::new(InputMode::Ui),
            media: Arc::new(
                MediaClient::new("http://127.0.0.1:9", Duration::from_millis(20)).unwrap(),
            ),
        });
        let task = tokio::spawn(serve_unix(listener, state));
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream
            .write_all(b"{\"command\":\"system\"}\n")
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        let response: Response = serde_json::from_str(&line).unwrap();
        assert!(response.ok);
        task.abort();
    }

    #[tokio::test]
    async fn arbitrary_command_is_rejected_at_wire_boundary() {
        let parsed =
            serde_json::from_str::<Request>(r#"{"command":"exec","command_line":"reboot"}"#);
        assert!(parsed.is_err());
    }
}
