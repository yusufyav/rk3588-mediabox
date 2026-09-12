//! The appliance's web surface: the product UI, and the transport the UI talks
//! to the control plane over.
//!
//! Two things make this a gateway rather than just another listener. The media
//! worker stays bound to loopback and is never exposed, so anything a browser
//! needs from it — including session bytes for a preview — is relayed through
//! here by the daemon that already owns that client. And the LAN listener
//! accepts only private-network peers, so making the UI reachable from a phone
//! does not make the control plane reachable from the internet.
//!
//! The HTTP is hand-written for the same reason the rest of the crate is: this
//! serves a handful of fixed routes to one appliance, and a framework would be
//! a larger dependency surface than the thing it implements.

use crate::daemon::AppState;
use mediabox_core::{Request, Response};
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// Who a listener answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerPolicy {
    /// The appliance itself: Kodi, mediaboxctl, the TV-local browser.
    LoopbackOnly,
    /// The home network as well, for a phone or a laptop.
    LoopbackAndPrivate,
}

impl PeerPolicy {
    fn admits(self, address: IpAddr) -> bool {
        if address.is_loopback() {
            return true;
        }
        if self == Self::LoopbackOnly {
            return false;
        }
        is_private(address)
    }
}

/// RFC1918 / RFC4193 / link-local, plus the v6-mapped forms of them that a
/// dual-stack listener reports for an ordinary IPv4 client.
fn is_private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return mapped.is_private() || mapped.is_link_local();
            }
            let octets = v6.octets();
            // fc00::/7 unique local, fe80::/10 link local.
            (octets[0] & 0xfe) == 0xfc || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80)
        }
    }
}

pub struct WebConfig {
    /// Directory the compiled product UI was installed into. When absent the
    /// daemon still answers the API and simply has no pages to serve.
    pub ui_root: Option<PathBuf>,
    pub policy: PeerPolicy,
}

pub async fn serve(
    listener: TcpListener,
    state: Arc<AppState>,
    config: Arc<WebConfig>,
) -> io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        if !config.policy.admits(peer.ip()) {
            continue;
        }
        let state = state.clone();
        let config = config.clone();
        tokio::spawn(async move {
            let _ = handle(stream, state, config).await;
        });
    }
}

struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body_start: usize,
    content_length: usize,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn parse_head(data: &[u8]) -> Option<Result<Head, &'static str>> {
    let end = data.windows(4).position(|window| window == b"\r\n\r\n")?;
    if end > MAX_HEADER_BYTES {
        return Some(Err("başlıklar çok büyük"));
    }
    let text = String::from_utf8_lossy(&data[..end]);
    let mut lines = text.lines();
    let Some(request_line) = lines.next() else {
        return Some(Err("istek satırı yok"));
    };
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Some(Err("istek satırı hatalı"));
    };
    if !version.starts_with("HTTP/1.") {
        return Some(Err("yalnız HTTP/1.x desteklenir"));
    }
    // A query string is only ever cache-busting here; the path is the route.
    let path = target.split('?').next().unwrap_or(target);
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    Some(Ok(Head {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        body_start: end + 4,
        content_length,
    }))
}

async fn handle(
    mut stream: TcpStream,
    state: Arc<AppState>,
    config: Arc<WebConfig>,
) -> io::Result<()> {
    let mut data = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let head = loop {
        let read = tokio::time::timeout(Duration::from_secs(15), stream.read(&mut chunk)).await;
        let Ok(Ok(count)) = read else { return Ok(()) };
        if count == 0 {
            return Ok(());
        }
        data.extend_from_slice(&chunk[..count]);
        if data.len() > MAX_REQUEST_BYTES {
            return send_json(&mut stream, 413, &Response::failure("INVALID_REQUEST", "istek çok büyük")).await;
        }
        match parse_head(&data) {
            None => continue,
            Some(Err(message)) => {
                return send_json(
                    &mut stream,
                    400,
                    &Response::failure("INVALID_REQUEST", message),
                )
                .await;
            }
            Some(Ok(head)) => break head,
        }
    };

    while data.len() < head.body_start + head.content_length {
        if head.content_length > MAX_REQUEST_BYTES {
            return send_json(&mut stream, 413, &Response::failure("INVALID_REQUEST", "gövde çok büyük")).await;
        }
        let read = tokio::time::timeout(Duration::from_secs(15), stream.read(&mut chunk)).await;
        let Ok(Ok(count)) = read else { return Ok(()) };
        if count == 0 {
            return Ok(());
        }
        data.extend_from_slice(&chunk[..count]);
    }
    let body = data[head.body_start..head.body_start + head.content_length].to_vec();

    route(&mut stream, head, body, state, config).await
}

async fn route(
    stream: &mut TcpStream,
    head: Head,
    body: Vec<u8>,
    state: Arc<AppState>,
    config: Arc<WebConfig>,
) -> io::Result<()> {
    match (head.method.as_str(), head.path.as_str()) {
        ("POST", "/v1/control") => {
            let response = match serde_json::from_slice::<Request>(&body) {
                Ok(Request::InputMonitor) => Response::failure(
                    "INVALID_REQUEST",
                    "input.monitor yerine GET /v1/events kullanın",
                ),
                Ok(request) => state.handle(request).await,
                Err(error) => Response::failure("INVALID_REQUEST", error.to_string()),
            };
            let status = if response.ok { 200 } else { 400 };
            send_json(stream, status, &response).await
        }
        ("GET", "/v1/events") => serve_events(stream, state).await,
        ("GET", path) if path.starts_with("/v1/media/session/") => {
            let id = &path["/v1/media/session/".len()..];
            proxy_session(stream, state, id).await
        }
        ("GET", _) | ("HEAD", _) => serve_static(stream, &head, config).await,
        _ => {
            send_json(
                stream,
                405,
                &Response::failure("METHOD_NOT_ALLOWED", "bu yol bu yöntemi kabul etmiyor"),
            )
            .await
        }
    }
}

// ------------------------------------------------------------------ SSE

/// Input events, as they happen.
///
/// The CEC remote is read by the daemon, not by the browser, so without this
/// the product UI would be navigable by keyboard only. Each normalized action
/// is forwarded verbatim; the UI decides what "up" means on the screen it is
/// showing.
async fn serve_events(stream: &mut TcpStream, state: Arc<AppState>) -> io::Result<()> {
    let mut events = state.input.subscribe();
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
              X-Accel-Buffering: no\r\nConnection: close\r\n\r\n: ready\n\n",
        )
        .await?;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(20), events.recv()).await;
        let payload = match next {
            // A comment frame keeps a browser from deciding the stream died.
            Err(_) => ": keepalive\n\n".to_string(),
            Ok(Ok(event)) => format!(
                "data: {}\n\n",
                serde_json::to_string(&event).expect("event JSON")
            ),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(count))) => {
                format!("data: {{\"warning\":\"lagged\",\"events\":{count}}}\n\n")
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return Ok(()),
        };
        stream.write_all(payload.as_bytes()).await?;
    }
}

// ------------------------------------------------- media session relay

/// Relay the bytes of one media session to the browser.
///
/// The media worker is loopback-only by design. A preview playing in a phone's
/// browser still needs those bytes, so the daemon — which already holds the
/// only client the worker accepts — copies them through. Nothing else about
/// the worker becomes reachable.
async fn proxy_session(stream: &mut TcpStream, state: Arc<AppState>, id: &str) -> io::Result<()> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return send_json(
            stream,
            400,
            &Response::failure("INVALID_REQUEST", "oturum kimliği geçersiz"),
        )
        .await;
    }
    let upstream = match state.media.session_stream(id).await {
        Ok(response) => response,
        Err(error) => {
            return send_json(
                stream,
                502,
                &Response::failure("MEDIA_WORKER_ERROR", error.to_string()),
            )
            .await;
        }
    };
    let status = upstream.status().as_u16();
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\n\
         Accept-Ranges: none\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await?;
    let mut upstream = upstream;
    while let Ok(Some(chunk)) = upstream.chunk().await {
        if stream.write_all(&chunk).await.is_err() {
            // The viewer navigated away. Dropping `upstream` closes the read
            // side, which is what tells the worker to stop its encoder.
            return Ok(());
        }
    }
    Ok(())
}

// --------------------------------------------------------------- static

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Map a request path to a file inside the UI root.
///
/// Only a flat, known-safe character set is accepted and the result is checked
/// to still be under the root after resolution, so no request path can walk out
/// of the installed UI.
fn resolve_static(root: &Path, request_path: &str) -> Option<PathBuf> {
    let trimmed = request_path.trim_start_matches('/');
    let relative = if trimmed.is_empty() { "index.html" } else { trimmed };
    if relative.split('/').any(|segment| {
        segment.is_empty()
            || segment == "."
            || segment == ".."
            || !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    }) {
        return None;
    }
    let candidate = root.join(relative);
    let resolved = candidate.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    resolved.starts_with(&root).then_some(resolved)
}

async fn serve_static(
    stream: &mut TcpStream,
    head: &Head,
    config: Arc<WebConfig>,
) -> io::Result<()> {
    let Some(root) = config.ui_root.as_deref() else {
        return send_json(
            stream,
            404,
            &Response::failure("UI_NOT_INSTALLED", "bu cihazda kurulu bir arayüz yok"),
        )
        .await;
    };
    // The UI keeps its own screen state; every non-file path is the same page.
    let path = resolve_static(root, &head.path)
        .or_else(|| resolve_static(root, "/index.html"));
    let Some(path) = path else {
        return send_json(stream, 404, &Response::failure("NOT_FOUND", "dosya yok")).await;
    };
    let Ok(metadata) = tokio::fs::metadata(&path).await else {
        return send_json(stream, 404, &Response::failure("NOT_FOUND", "dosya yok")).await;
    };
    if !metadata.is_file() {
        return send_json(stream, 404, &Response::failure("NOT_FOUND", "dosya yok")).await;
    }
    let etag = format!(
        "\"{:x}-{:x}\"",
        metadata.len(),
        metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|delta| delta.as_secs())
            .unwrap_or(0)
    );
    if head.header("if-none-match") == Some(etag.as_str()) {
        let response = format!(
            "HTTP/1.1 304 Not Modified\r\nETag: {etag}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
        );
        return stream.write_all(response.as_bytes()).await;
    }
    let body = tokio::fs::read(&path).await.unwrap_or_default();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nETag: {etag}\r\n\
         Cache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        content_type_for(&path),
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    if head.method != "HEAD" {
        stream.write_all(&body).await?;
    }
    Ok(())
}

async fn send_json(stream: &mut TcpStream, status: u16, response: &Response) -> io::Result<()> {
    let body = serde_json::to_vec(response).expect("response JSON");
    let reason = match status {
        200 => "OK",
        304 => "Not Modified",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn loopback_listener_refuses_the_network() {
        let policy = PeerPolicy::LoopbackOnly;
        assert!(policy.admits(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!policy.admits(IpAddr::V4(Ipv4Addr::new(10, 27, 27, 9))));
    }

    #[test]
    fn lan_listener_admits_private_and_refuses_public() {
        let policy = PeerPolicy::LoopbackAndPrivate;
        assert!(policy.admits(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(policy.admits(IpAddr::V4(Ipv4Addr::new(10, 27, 27, 9))));
        assert!(policy.admits(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4))));
        // A v4 client on a dual-stack listener arrives v6-mapped.
        assert!(policy.admits(IpAddr::V6(Ipv4Addr::new(10, 0, 0, 5).to_ipv6_mapped())));
        assert!(!policy.admits(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!policy.admits(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1))));
    }

    #[test]
    fn static_paths_cannot_escape_the_ui_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"<!doctype html>").unwrap();
        let root = dir.path();
        assert!(resolve_static(root, "/").is_some());
        assert!(resolve_static(root, "/index.html").is_some());
        assert!(resolve_static(root, "/../etc/passwd").is_none());
        assert!(resolve_static(root, "/nested/../../escape").is_none());
        assert!(resolve_static(root, "/a%2fb").is_none());
    }

    #[test]
    fn head_parsing_extracts_target_and_length() {
        let raw = b"POST /v1/control HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello";
        let head = parse_head(raw).unwrap().unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/v1/control");
        assert_eq!(head.content_length, 5);
        assert_eq!(&raw[head.body_start..], b"hello");
    }

    #[test]
    fn a_query_string_does_not_become_part_of_the_route() {
        let raw = b"GET /v1/events?since=4 HTTP/1.1\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert_eq!(head.path, "/v1/events");
    }
}
