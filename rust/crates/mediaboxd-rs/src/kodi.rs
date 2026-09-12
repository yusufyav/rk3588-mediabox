use mediabox_core::{KodiStatus, PlaybackState};
use reqwest::Url;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;

/// How long `Player.Open` may take. Kodi holds the call open until the stream
/// is playing, and a remote source has to be fetched and probed first.
const OPEN_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Error)]
pub enum KodiError {
    #[error("Kodi kullanılamıyor: {0}")]
    Unavailable(String),
    #[error("Kodi JSON-RPC hatası: {0}")]
    Rpc(String),
    #[error("Kodi yanıtı geçersiz: {0}")]
    InvalidResponse(String),
    #[error("geçersiz istek: {0}")]
    InvalidRequest(String),
}

pub struct KodiClient {
    endpoint: Url,
    http: reqwest::Client,
    next_id: AtomicU64,
}

impl KodiClient {
    pub fn new(endpoint: &str, timeout: Duration) -> Result<Self, KodiError> {
        let endpoint =
            Url::parse(endpoint).map_err(|e| KodiError::InvalidRequest(e.to_string()))?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(KodiError::InvalidRequest(
                "Kodi endpoint http(s) olmalı".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| KodiError::Unavailable(e.to_string()))?;
        Ok(Self {
            endpoint,
            http,
            next_id: AtomicU64::new(1),
        })
    }

    pub async fn call(&self, method: &str, params: Option<Value>) -> Result<Value, KodiError> {
        self.call_within(method, params, None).await
    }

    /// One JSON-RPC call, optionally allowed longer than the client default.
    ///
    /// Status polling must fail fast so a wedged player cannot stall a screen.
    /// `Player.Open` is the opposite: Kodi does not answer it until the stream
    /// is actually open, and opening a film over the internet routinely takes
    /// longer than a status poll may. Timing that out would report a failure
    /// for playback that is in fact starting, so the two get different budgets
    /// rather than one compromise between them.
    async fn call_within(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<Value, KodiError> {
        // Method names are internal constants, never copied from the local API.
        let mut body = json!({"jsonrpc":"2.0", "id":self.next_id.fetch_add(1, Ordering::Relaxed), "method":method});
        if let Some(params) = params {
            body["params"] = params;
        }
        let mut request = self.http.post(self.endpoint.clone()).json(&body);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let response = request
            .send()
            .await
            .map_err(|e| KodiError::Unavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(KodiError::Unavailable(format!(
                "HTTP {}",
                response.status()
            )));
        }
        let document: Value = response
            .json()
            .await
            .map_err(|e| KodiError::InvalidResponse(e.to_string()))?;
        if let Some(error) = document.get("error") {
            return Err(KodiError::Rpc(error.to_string()));
        }
        document
            .get("result")
            .cloned()
            .ok_or_else(|| KodiError::InvalidResponse("result alanı yok".into()))
    }

    pub async fn ping(&self) -> Result<bool, KodiError> {
        Ok(self.call("JSONRPC.Ping", None).await? == "pong")
    }

    pub async fn active_players(&self) -> Result<Vec<Value>, KodiError> {
        let value = self.call("Player.GetActivePlayers", None).await?;
        value
            .as_array()
            .cloned()
            .ok_or_else(|| KodiError::InvalidResponse("active players dizi değil".into()))
    }

    async fn player_id(&self) -> Result<i64, KodiError> {
        let players = self.active_players().await?;
        let player = players
            .iter()
            .find(|p| p.get("type") == Some(&Value::String("video".into())))
            .or_else(|| players.first())
            .ok_or_else(|| KodiError::InvalidRequest("aktif Kodi oynatıcısı yok".into()))?;
        player
            .get("playerid")
            .and_then(Value::as_i64)
            .ok_or_else(|| KodiError::InvalidResponse("playerid geçersiz".into()))
    }

    pub async fn status(&self) -> KodiStatus {
        let running = kodi_running(Path::new("/proc"));
        let mut status = KodiStatus {
            running,
            jsonrpc_reachable: false,
            state: PlaybackState::Offline,
            active_player_id: None,
            item: None,
            speed: None,
            time: None,
            total_time: None,
            error: None,
        };
        let result: Result<(), KodiError> = async {
            status.jsonrpc_reachable = self.ping().await?;
            let players = self.active_players().await?;
            if players.is_empty() {
                status.state = PlaybackState::Idle;
                return Ok(());
            }
            let id = players
                .iter()
                .find(|p| p.get("type") == Some(&Value::String("video".into())))
                .unwrap_or(&players[0])
                .get("playerid")
                .and_then(Value::as_i64)
                .ok_or_else(|| KodiError::InvalidResponse("playerid geçersiz".into()))?;
            status.active_player_id = Some(id);
            let properties = self
                .call(
                    "Player.GetProperties",
                    Some(json!({"playerid":id,"properties":["speed","time","totaltime"]})),
                )
                .await?;
            let item = self
                .call(
                    "Player.GetItem",
                    Some(json!({"playerid":id,"properties":["title","file"]})),
                )
                .await?;
            status.speed = properties.get("speed").and_then(Value::as_i64);
            status.time = properties.get("time").cloned();
            status.total_time = properties.get("totaltime").cloned();
            status.item = item.get("item").cloned();
            status.state = if status.speed == Some(0) {
                PlaybackState::Paused
            } else {
                PlaybackState::Playing
            };
            Ok(())
        }
        .await;
        if let Err(error) = result {
            status.error = Some(error.to_string());
        }
        status
    }

    pub async fn play_pause(&self) -> Result<Value, KodiError> {
        let id = self.player_id().await?;
        self.call("Player.PlayPause", Some(json!({"playerid":id})))
            .await
    }

    pub async fn set_playing(&self, playing: bool) -> Result<Value, KodiError> {
        let id = self.player_id().await?;
        self.call(
            "Player.PlayPause",
            Some(json!({"playerid":id,"play":playing})),
        )
        .await
    }

    pub async fn stop(&self) -> Result<Value, KodiError> {
        let id = self.player_id().await?;
        self.call("Player.Stop", Some(json!({"playerid":id}))).await
    }

    pub async fn seek_relative(&self, delta_seconds: i64) -> Result<Value, KodiError> {
        if !(-86_400..=86_400).contains(&delta_seconds) {
            return Err(KodiError::InvalidRequest(
                "seek -86400..86400 aralığında olmalı".into(),
            ));
        }
        let id = self.player_id().await?;
        let properties = self
            .call(
                "Player.GetProperties",
                Some(json!({"playerid":id,"properties":["time"]})),
            )
            .await?;
        let current = kodi_time_seconds(properties.get("time"))
            .ok_or_else(|| KodiError::InvalidResponse("Kodi zamanı geçersiz".into()))?;
        let target = (current + delta_seconds).max(0) as u64;
        // `value` is a union, and an absolute position has to name itself as
        // one: a bare time object is rejected as matching no member of it.
        self.call(
            "Player.Seek",
            Some(json!({"playerid":id,"value":{"time":seconds_to_kodi_time(target)}})),
        )
        .await
    }

    pub async fn open(&self, url: &str, resume_seconds: u64) -> Result<Value, KodiError> {
        if resume_seconds > 604_800 {
            return Err(KodiError::InvalidRequest("resume_seconds çok büyük".into()));
        }
        let parsed = Url::parse(url).map_err(|e| KodiError::InvalidRequest(e.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https" | "file") {
            return Err(KodiError::InvalidRequest(
                "URL şeması http, https veya file olmalı".into(),
            ));
        }
        if url.bytes().any(|byte| matches!(byte, 0 | b'\r' | b'\n')) {
            return Err(KodiError::InvalidRequest(
                "URL kontrol karakteri içeriyor".into(),
            ));
        }
        self.call_within(
            "Player.Open",
            Some(
                json!({"item":{"file":url},"options":{"resume":seconds_to_kodi_time(resume_seconds)}}),
            ),
            Some(OPEN_TIMEOUT),
        )
        .await
    }
}

fn seconds_to_kodi_time(seconds: u64) -> Value {
    json!({"hours":seconds/3600,"minutes":(seconds%3600)/60,"seconds":seconds%60,"milliseconds":0})
}

fn kodi_time_seconds(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    Some(
        value.get("hours").and_then(Value::as_i64).unwrap_or(0) * 3600
            + value.get("minutes").and_then(Value::as_i64).unwrap_or(0) * 60
            + value.get("seconds").and_then(Value::as_i64).unwrap_or(0),
    )
}

pub fn kodi_running(proc_root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
            && std::fs::read_to_string(entry.path().join("comm"))
                .ok()
                .is_some_and(|s| matches!(s.trim(), "kodi-gbm" | "kodi.bin"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn an_absolute_seek_names_its_union_member() {
        let value = json!({"value": {"time": seconds_to_kodi_time(3661)}});
        let time = &value["value"]["time"];
        assert_eq!(time["hours"], 1);
        assert_eq!(time["minutes"], 1);
        assert_eq!(time["seconds"], 1);
    }

    #[tokio::test]
    async fn kodi_mock_client_sends_json_rpc() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let n = stream.read(&mut request).await.unwrap();
            let text = String::from_utf8_lossy(&request[..n]);
            assert!(text.contains("JSONRPC.Ping"));
            let body = r#"{"jsonrpc":"2.0","id":1,"result":"pong"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let client =
            KodiClient::new(&format!("http://{addr}/jsonrpc"), Duration::from_secs(1)).unwrap();
        assert!(client.ping().await.unwrap());
    }
}
