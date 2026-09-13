//! Typed client for the local-only Python media worker.

use reqwest::{Client, StatusCode, Url};
use serde_json::{Value, json};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("gecersiz media worker endpoint'i: {0}")]
    InvalidEndpoint(String),
    #[error("media worker baglanti hatasi: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("media worker HTTP {status}: {body}")]
    Http { status: StatusCode, body: String },
    #[error("media worker gecersiz yanit verdi: {0}")]
    InvalidResponse(String),
}

pub struct MediaClient {
    endpoint: Url,
    client: Client,
}

impl MediaClient {
    pub fn new(endpoint: &str, timeout: Duration) -> Result<Self, MediaError> {
        let endpoint =
            Url::parse(endpoint).map_err(|error| MediaError::InvalidEndpoint(error.to_string()))?;
        let loopback = endpoint
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
        if endpoint.scheme() != "http"
            || !loopback
            || !matches!(endpoint.path(), "" | "/")
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(MediaError::InvalidEndpoint(
                "yalnizca path/query icermeyen loopback http origin kabul edilir".into(),
            ));
        }
        Ok(Self {
            endpoint,
            client: Client::builder().timeout(timeout).build()?,
        })
    }

    pub async fn status(&self) -> Result<Value, MediaError> {
        self.get(&["media", "status"], &[]).await
    }

    pub async fn capabilities(&self) -> Result<Value, MediaError> {
        self.get(&["media", "capabilities"], &[]).await
    }

    pub async fn home(&self) -> Result<Value, MediaError> {
        self.get(&["media", "home"], &[]).await
    }

    pub async fn catalog(
        &self,
        media_type: &str,
        id: &str,
        addon_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value, MediaError> {
        let limit = limit.map(|value| value.to_string());
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(addon) = addon_id {
            query.push(("addon", addon));
        }
        if let Some(limit) = limit.as_deref() {
            query.push(("limit", limit));
        }
        self.get(&["media", "catalog", media_type, id], &query).await
    }

    pub async fn meta(&self, media_type: &str, id: &str) -> Result<Value, MediaError> {
        self.get(&["media", "meta", media_type, id], &[]).await
    }

    pub async fn subtitles(
        &self,
        media_type: &str,
        id: &str,
        video_id: Option<&str>,
    ) -> Result<Value, MediaError> {
        let query: Vec<(&str, &str)> = video_id.map(|v| vec![("videoId", v)]).unwrap_or_default();
        self.get(&["media", "subtitles", media_type, id], &query)
            .await
    }

    pub async fn library(&self) -> Result<Value, MediaError> {
        self.get(&["media", "library"], &[]).await
    }

    pub async fn library_item(&self, id: &str) -> Result<Value, MediaError> {
        self.get(&["media", "library", id], &[]).await
    }

    pub async fn resolve(&self, stream: Value) -> Result<Value, MediaError> {
        self.post(&["media", "resolve"], json!({"stream": stream}))
            .await
    }

    pub async fn stream_plan(&self, stream: Value) -> Result<Value, MediaError> {
        self.post(&["media", "plan"], json!({"stream": stream})).await
    }

    pub async fn session_start_stream(
        &self,
        stream: Value,
        start_seconds: u64,
    ) -> Result<Value, MediaError> {
        self.post(
            &["media", "session"],
            json!({"stream": stream, "startSeconds": start_seconds}),
        )
        .await
    }

    pub async fn session_start_at(
        &self,
        url: &str,
        start_seconds: u64,
    ) -> Result<Value, MediaError> {
        self.post(
            &["media", "session"],
            json!({"url": url, "startSeconds": start_seconds}),
        )
        .await
    }

    pub async fn search(&self, query: &str) -> Result<Value, MediaError> {
        self.get(&["media", "search"], &[("q", query)]).await
    }

    pub async fn inspect(&self, url: &str) -> Result<Value, MediaError> {
        self.post(&["media", "inspect"], json!({"url": url})).await
    }

    pub async fn streams(&self, media_type: &str, id: &str) -> Result<Value, MediaError> {
        self.get(&["media", "streams", media_type, id], &[]).await
    }

    pub async fn policy(&self, url: &str) -> Result<Value, MediaError> {
        self.post(&["media", "plan"], json!({"url": url})).await
    }

    pub async fn sessions(&self) -> Result<Value, MediaError> {
        self.get(&["media", "session"], &[]).await
    }

    pub async fn session_start(&self, url: &str) -> Result<Value, MediaError> {
        self.post(&["media", "session"], json!({"url": url})).await
    }

    /// The live bytes of one session, still as a streaming response so the
    /// relay above can copy them without buffering a whole film in memory.
    pub async fn session_stream(&self, id: &str) -> Result<reqwest::Response, MediaError> {
        Ok(self.client.get(self.url(&["media", "session", id])?).send().await?)
    }

    pub async fn session_stop(&self, id: &str) -> Result<Value, MediaError> {
        self.delete(&["media", "session", id]).await
    }

    /// Sign in to a Stremio account, so the box uses that account's addon
    /// collection instead of the default one.
    ///
    /// The credentials are forwarded to the media core over loopback and are
    /// never stored here: what is kept, by the core, is the auth key the login
    /// returns.
    pub async fn login(&self, email: &str, password: &str) -> Result<Value, MediaError> {
        self.post(&["media", "login"], json!({"email": email, "password": password}))
            .await
    }

    pub async fn logout(&self) -> Result<Value, MediaError> {
        self.post(&["media", "logout"], json!({})).await
    }

    async fn get(&self, path: &[&str], query: &[(&str, &str)]) -> Result<Value, MediaError> {
        let mut url = self.url(path)?;
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        let response = self.client.get(url).send().await?;
        Self::decode(response).await
    }

    async fn post(&self, path: &[&str], body: Value) -> Result<Value, MediaError> {
        let response = self.client.post(self.url(path)?).json(&body).send().await?;
        Self::decode(response).await
    }

    async fn delete(&self, path: &[&str]) -> Result<Value, MediaError> {
        let response = self.client.delete(self.url(path)?).send().await?;
        Self::decode(response).await
    }

    fn url(&self, path: &[&str]) -> Result<Url, MediaError> {
        let mut url = self.endpoint.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                MediaError::InvalidEndpoint("origin path segmentlerine donusturulemiyor".into())
            })?;
            segments.clear();
            segments.extend(path);
        }
        Ok(url)
    }

    async fn decode(response: reqwest::Response) -> Result<Value, MediaError> {
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            return Err(MediaError::Http {
                status,
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        serde_json::from_slice(&bytes)
            .map_err(|error| MediaError::InvalidResponse(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn endpoint_must_be_a_loopback_origin() {
        assert!(MediaClient::new("http://127.0.0.1:8790", Duration::from_secs(1)).is_ok());
        assert!(MediaClient::new("http://192.0.2.1:8790", Duration::from_secs(1)).is_err());
        assert!(MediaClient::new("http://127.0.0.1:8790/media", Duration::from_secs(1)).is_err());
    }

    #[tokio::test]
    async fn search_is_encoded_and_sent_only_to_the_worker() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 2048];
            let size = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("GET /media/search?q=Dune+Part+Two HTTP/1.1"));
            let body = br#"{"query":"Dune Part Two","rows":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(body).await.unwrap();
        });
        let client =
            MediaClient::new(&format!("http://{address}"), Duration::from_secs(1)).unwrap();
        let response = client.search("Dune Part Two").await.unwrap();
        assert_eq!(response["query"], "Dune Part Two");
        server.await.unwrap();
    }
}
