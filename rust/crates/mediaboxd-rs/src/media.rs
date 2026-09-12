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
