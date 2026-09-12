//! The shapes the media core answers with.
//!
//! Deliberately permissive: an addon is third-party data and a field it omits
//! must degrade the screen, never break it. Anything technical the UI shows —
//! codec, bit depth, HDR class, the audio decision — is read from the media
//! core's own inspection and policy output and is never inferred from a title
//! string, because a filename claiming "HDR" is not evidence of anything.

use serde::Deserialize;

fn default_movie() -> String {
    "movie".to_string()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MetaPreview {
    pub id: String,
    #[serde(rename = "type", default = "default_movie")]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "releaseInfo", default)]
    pub release_info: Option<String>,
    #[serde(rename = "imdbRating", default)]
    pub imdb_rating: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(rename = "addonId", default)]
    pub addon_id: Option<String>,
}

impl MetaPreview {
    pub fn is_library(&self) -> bool {
        self.addon_id.as_deref() == Some(crate::LIBRARY_ADDON_ID)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CatalogRow {
    #[serde(rename = "addonId", default)]
    pub addon_id: String,
    #[serde(rename = "addonName", default)]
    pub addon_name: String,
    #[serde(rename = "catalogId", default)]
    pub catalog_id: String,
    #[serde(rename = "type", default = "default_movie")]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub items: Vec<MetaPreview>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HomeRows {
    #[serde(default)]
    pub rows: Vec<CatalogRow>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LibraryListing {
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub items: Vec<MetaPreview>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SearchResults {
    #[serde(default)]
    pub rows: Vec<CatalogRow>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Meta {
    pub id: String,
    #[serde(rename = "type", default = "default_movie")]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "releaseInfo", default)]
    pub release_info: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(rename = "imdbRating", default)]
    pub imdb_rating: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub cast: Vec<String>,
    #[serde(default)]
    pub director: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MetaEnvelope {
    pub meta: Meta,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LibraryItemEnvelope {
    pub meta: Meta,
    #[serde(default)]
    pub streams: Vec<Stream>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Stream {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub identity: String,
    #[serde(rename = "addonId", default)]
    pub addon_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(rename = "infoHash", default)]
    pub info_hash: Option<String>,
    #[serde(rename = "externalUrl", default)]
    pub external_url: Option<String>,
    #[serde(default)]
    pub playable: bool,
}

impl Stream {
    pub fn label(&self) -> String {
        self.name
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("Kaynak")
            .replace('\n', " ")
    }

    pub fn detail(&self) -> Option<String> {
        self.title
            .as_deref()
            .map(|value| value.replace('\n', " · "))
            .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamListing {
    #[serde(default)]
    pub streams: Vec<Stream>,
    #[serde(default)]
    pub playable: u32,
}

// ------------------------------------------------------- inspection + policy

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Reason {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Container {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(rename = "durationSeconds", default)]
    pub duration_seconds: Option<f64>,
    #[serde(rename = "bitRate", default)]
    pub bit_rate: Option<f64>,
    #[serde(rename = "sizeBytes", default)]
    pub size_bytes: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VideoTrack {
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub fps: Option<f64>,
    #[serde(rename = "bitDepth", default)]
    pub bit_depth: Option<u32>,
    #[serde(default)]
    pub chroma: Option<String>,
    #[serde(rename = "colorPrimaries", default)]
    pub color_primaries: Option<String>,
    #[serde(rename = "colorTransfer", default)]
    pub color_transfer: Option<String>,
    #[serde(default)]
    pub hdr: Option<String>,
    #[serde(rename = "dolbyVision", default)]
    pub dolby_vision: Option<DolbyVision>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DolbyVision {
    #[serde(default)]
    pub profile: Option<u32>,
    #[serde(default)]
    pub level: Option<u32>,
    #[serde(rename = "blSignalCompatibilityId", default)]
    pub bl_signal_compatibility_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AudioTrack {
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub channels: Option<u32>,
    #[serde(rename = "channelLayout", default)]
    pub channel_layout: Option<String>,
    #[serde(rename = "sampleRate", default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(rename = "objectAudio", default)]
    pub object_audio: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MediaInfo {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub container: Option<Container>,
    #[serde(default)]
    pub video: Vec<VideoTrack>,
    #[serde(default)]
    pub audio: Vec<AudioTrack>,
    #[serde(default)]
    pub warnings: Vec<Reason>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VideoDecision {
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub hdr: Option<String>,
    #[serde(default)]
    pub track: Option<VideoTrack>,
    #[serde(default)]
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AudioDecision {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub track: Option<AudioTrack>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PlaybackDecision {
    #[serde(default)]
    pub mode: String,
    #[serde(rename = "needsSession", default)]
    pub needs_session: bool,
    #[serde(rename = "videoCopied", default)]
    pub video_copied: bool,
    #[serde(default)]
    pub video: Option<VideoDecision>,
    #[serde(default)]
    pub audio: Option<AudioDecision>,
    #[serde(default)]
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PreviewDecision {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Plan {
    pub media: MediaInfo,
    pub playback: PlaybackDecision,
    pub preview: PreviewDecision,
}

impl Plan {
    pub fn video(&self) -> Option<&VideoTrack> {
        self.playback
            .video
            .as_ref()
            .and_then(|decision| decision.track.as_ref())
            .or_else(|| self.media.video.first())
    }

    pub fn audio(&self) -> Option<&AudioTrack> {
        self.playback
            .audio
            .as_ref()
            .and_then(|decision| decision.track.as_ref())
            .or_else(|| self.media.audio.first())
    }

    /// The one sentence that matters: what will actually happen on the TV.
    pub fn verdict(&self) -> (&'static str, &'static str) {
        match self.playback.mode.as_str() {
            "Direct" => ("Doğrudan oynatma", "good"),
            "DirectWithAudioTranscode" => ("Video kopyalanır, ses AC-3'e çevrilir", "warn"),
            "Remux" => ("Kapsayıcı yeniden paketlenir, video kopyalanır", "warn"),
            "FallbackSourcePreferred" => ("Başka bir kaynak tercih edilmeli", "warn"),
            "Unsupported" => ("Bu cihazda oynatılamaz", "bad"),
            _ => ("Karar bilinmiyor", ""),
        }
    }

    pub fn preview_available(&self) -> bool {
        matches!(self.preview.mode.as_str(), "BrowserDirect" | "BrowserRemux")
    }
}

// ------------------------------------------------------------------ status

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ServiceHealth {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub healthy: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct KodiStatus {
    #[serde(default)]
    pub running: bool,
    #[serde(rename = "jsonrpc_reachable", default)]
    pub jsonrpc_reachable: bool,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub item: Option<serde_json::Value>,
    #[serde(default)]
    pub speed: Option<i64>,
    #[serde(default)]
    pub time: Option<KodiTime>,
    #[serde(rename = "total_time", default)]
    pub total_time: Option<KodiTime>,
    #[serde(default)]
    pub error: Option<String>,
}

impl KodiStatus {
    pub fn title(&self) -> Option<String> {
        let item = self.item.as_ref()?;
        item.get("title")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                item.get("label")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct KodiTime {
    #[serde(default)]
    pub hours: u64,
    #[serde(default)]
    pub minutes: u64,
    #[serde(default)]
    pub seconds: u64,
}

impl KodiTime {
    pub fn total_seconds(self) -> u64 {
        self.hours * 3600 + self.minutes * 60 + self.seconds
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CecStatus {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(rename = "physical_address", default)]
    pub physical_address: Option<String>,
    #[serde(rename = "logical_addresses", default)]
    pub logical_addresses: Vec<u8>,
    #[serde(rename = "known_devices", default)]
    pub known_devices: Vec<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SurfaceStatus {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub kodi_active: bool,
    #[serde(default)]
    pub ui_active: bool,
    #[serde(default)]
    pub ui_installed: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SystemStatus {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub kernel: String,
    #[serde(default)]
    pub architecture: String,
    #[serde(rename = "uptime_seconds", default)]
    pub uptime_seconds: u64,
    #[serde(rename = "input_mode", default)]
    pub input_mode: String,
    #[serde(rename = "input_devices", default)]
    pub input_devices: Vec<serde_json::Value>,
    #[serde(default)]
    pub services: Vec<ServiceHealth>,
    pub kodi: KodiStatus,
    pub cec: CecStatus,
    #[serde(default)]
    pub media: serde_json::Value,
    #[serde(default)]
    pub surface: Option<SurfaceStatus>,
}
