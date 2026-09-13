//! The shapes the media core answers with.
//!
//! Deliberately permissive: an addon is third-party data and a field it omits
//! must degrade the screen, never break it. Anything technical the UI shows —
//! codec, bit depth, HDR class, the audio decision — is read from the media
//! core's own inspection and policy output and is never inferred from a title
//! string, because a filename claiming "HDR" is not evidence of anything.

use serde::{Deserialize, Serialize};

fn default_movie() -> String {
    "movie".to_string()
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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
    /// How far this title was watched, when it comes from the account's own
    /// library. Titles from a catalogue have none.
    #[serde(default)]
    pub state: Option<WatchState>,
}

/// A position in a title, as the account records it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct WatchState {
    #[serde(rename = "timeOffset", default)]
    pub time_offset: Option<u64>,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(rename = "lastWatched", default)]
    pub last_watched: Option<String>,
}

impl WatchState {
    /// Whether this is a title to carry on with, rather than one finished or
    /// never started.
    ///
    /// The tail of a film is credits, and a title left there is done rather
    /// than interrupted; ninety-five percent is where Stremio itself draws
    /// that line. A position without a duration is meaningless, because there
    /// is nothing to be a fraction of.
    pub fn unfinished(&self) -> bool {
        match (self.time_offset, self.duration) {
            (Some(offset), Some(duration)) if duration > 0 && offset > 0 => {
                (offset as f64) < (duration as f64) * 0.95
            }
            _ => false,
        }
    }

    /// How far in, as a fraction, for a progress bar.
    pub fn progress(&self) -> f64 {
        match (self.time_offset, self.duration) {
            (Some(offset), Some(duration)) if duration > 0 => {
                ((offset as f64) / (duration as f64)).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }
}

/// The ids the operator's own library holds, shared with whatever draws a
/// poster so a title already owned can say so wherever it turns up.
///
/// A signal rather than a set, because the library arrives after the first
/// catalogue does: the shelves are on screen before the answer, and a badge
/// that only appeared on the next redraw would be missing from exactly the
/// screen the remote is looking at.
#[derive(Clone, Copy)]
pub struct LibraryIds(pub leptos::prelude::RwSignal<std::collections::HashSet<String>>);

impl LibraryIds {
    pub fn has(&self, id: &str) -> bool {
        use leptos::prelude::*;
        self.0.with(|ids| ids.contains(id))
    }
}

impl MetaPreview {
    pub fn is_library(&self) -> bool {
        self.addon_id.as_deref() == Some(crate::LIBRARY_ADDON_ID)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct HomeRows {
    #[serde(default)]
    pub rows: Vec<CatalogRow>,
}

/// One catalogue's items, as the media core answers for a catalogue asked
/// directly rather than as part of the home surface.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CatalogItems {
    #[serde(default)]
    pub items: Vec<MetaPreview>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LibraryListing {
    #[serde(default)]
    pub configured: bool,
    /// This appliance's own titles, certain to play.
    #[serde(default)]
    pub items: Vec<MetaPreview>,
    /// The account's library, which follows the operator between devices.
    #[serde(default)]
    pub stremio: Vec<MetaPreview>,
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
    /// The title's trailer, as a YouTube id.
    #[serde(default)]
    pub trailer: Option<String>,
}

impl MetaPreview {
    /// The same title, in the shape the detail screen draws.
    ///
    /// A shelf already holds everything the top of a title's page shows — the
    /// name, the artwork, the year, the genres — and throwing it away to ask
    /// the addon for it again is what left the screen black for as long as the
    /// request took. What a preview does not carry (runtime, cast, directors)
    /// arrives with the full record and fills in underneath.
    pub fn as_meta(&self) -> Meta {
        Meta {
            id: self.id.clone(),
            kind: self.kind.clone(),
            name: self.name.clone(),
            poster: self.poster.clone(),
            background: self.background.clone(),
            logo: self.logo.clone(),
            description: self.description.clone(),
            release_info: self.release_info.clone(),
            runtime: None,
            imdb_rating: self.imdb_rating.clone(),
            genres: self.genres.clone(),
            cast: Vec::new(),
            director: Vec::new(),
            trailer: None,
        }
    }
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
    /// The addon's display name, which is what a tab can be labelled with; the
    /// id is a reverse-domain string.
    #[serde(rename = "addonName", default)]
    pub addon_name: Option<String>,
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

    /// The first line of the addon's own label — who is offering this, as they
    /// write it: "[RD+] Torrentio".
    ///
    /// Addons put the provider on the first line and what the file is on the
    /// ones after, and joining them into one string is what turned a column of
    /// distinct sources into a column of near-identical sentences.
    pub fn lead(&self) -> String {
        self.name
            .as_deref()
            .and_then(|value| value.lines().next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Kaynak")
            .to_string()
    }

    /// What the addon said about the file itself — "4k DV | HDR" — from the
    /// lines after the first.
    pub fn tag(&self) -> Option<String> {
        let rest: Vec<&str> = self
            .name
            .as_deref()?
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        (!rest.is_empty()).then(|| rest.join(" · "))
    }

    pub fn detail(&self) -> Option<String> {
        self.title
            .as_deref()
            .map(|value| value.replace('\n', " · "))
            .filter(|value| !value.is_empty())
    }
}

/// What an addon said about a source, taken apart.
///
/// Every addon writes the same handful of facts as one wall of text with
/// pictograms standing in for the field names — "👤 290 💾 21.31 GB ⚙️
/// Torrent9" — and putting that wall on the row verbatim is what made the
/// panel unreadable from a sofa: the release name was cut off mid-word to
/// leave room for an emoji, and the provider's own label wrapped onto three
/// lines so every row was a different height. The facts are all there. They
/// just have to be separated before they can be laid out.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SourceFacts {
    /// The debrid service already holds the file. Torrentio writes this as a
    /// "+" inside the bracketed mark it puts in front of its name, and it is
    /// the single most useful thing on the row: a cached source starts at
    /// once, an uncached one has to be fetched first.
    pub cached: bool,
    pub provider: String,
    pub quality: Option<String>,
    pub flags: Vec<String>,
    pub release: Option<String>,
    pub size: Option<String>,
    pub seeders: Option<String>,
    pub tracker: Option<String>,
    pub languages: Vec<String>,
}

const SEEDERS_MARK: char = '\u{1f464}';
const SIZE_MARK: char = '\u{1f4be}';
const TRACKER_MARK: char = '\u{2699}';
const MARKS: [char; 3] = [SEEDERS_MARK, SIZE_MARK, TRACKER_MARK];

/// The text between one pictogram and the next.
fn marker_field(line: &str, marker: char) -> Option<String> {
    let at = line.find(marker)? + marker.len_utf8();
    let rest = &line[at..];
    let end = rest.find(MARKS).unwrap_or(rest.len());
    // The variation selector after a pictogram belongs to the pictogram, not
    // to the value that follows it.
    let value = rest[..end].trim_matches(|c: char| c.is_whitespace() || c == '\u{fe0f}');
    (!value.is_empty()).then(|| value.to_string())
}

/// Bracketed marks come first and the provider's name is what is left.
fn split_marks(line: &str) -> (Vec<String>, String) {
    let mut marks = Vec::new();
    let mut rest = line.trim();
    while let Some(open) = rest.strip_prefix('[') {
        let Some(end) = open.find(']') else { break };
        let mark = open[..end].trim();
        if !mark.is_empty() {
            marks.push(mark.to_string());
        }
        rest = open[end + 1..].trim_start();
    }
    (marks, rest.to_string())
}

/// A regional indicator pair is a flag, and a flag is the shortest a language
/// can be written.
fn is_flag(text: &str) -> bool {
    text.chars().any(|c| ('\u{1f1e6}'..='\u{1f1ff}').contains(&c))
}

fn quality_of(word: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    for (needle, shown) in [
        ("2160", "4K"),
        ("4k", "4K"),
        ("1440", "2K"),
        ("1080", "1080p"),
        ("720", "720p"),
        ("480", "480p"),
        ("360", "360p"),
    ] {
        if lower.contains(needle) {
            return Some(shown.to_string());
        }
    }
    None
}

impl Stream {
    /// Read the addon's two text fields as the record they actually are.
    pub fn facts(&self) -> SourceFacts {
        let mut facts = SourceFacts::default();

        let name = self.name.as_deref().unwrap_or_default();
        let mut lines = name.lines().map(str::trim).filter(|line| !line.is_empty());
        if let Some(head) = lines.next() {
            let (marks, provider) = split_marks(head);
            facts.cached = marks.iter().any(|mark| mark.ends_with('+'));
            facts.provider = provider;
        }
        // "4k DV | HDR10+ | HDR" — the resolution is one word among the rest,
        // and which word it is varies by addon, so it is found rather than
        // assumed to be first.
        for word in lines
            .flat_map(|line| line.split('|'))
            .flat_map(str::split_whitespace)
        {
            match (&facts.quality, quality_of(word)) {
                (None, Some(shown)) => facts.quality = Some(shown),
                _ => facts.flags.push(word.to_string()),
            }
        }

        let title = self.title.as_deref().unwrap_or_default();
        for (index, line) in title
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .enumerate()
        {
            if index == 0 {
                facts.release = Some(line.to_string());
            } else if line.contains(MARKS) {
                if facts.seeders.is_none() {
                    facts.seeders = marker_field(line, SEEDERS_MARK);
                }
                if facts.size.is_none() {
                    facts.size = marker_field(line, SIZE_MARK);
                }
                if facts.tracker.is_none() {
                    facts.tracker = marker_field(line, TRACKER_MARK);
                }
            } else {
                facts.languages.extend(
                    line.split('/')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(str::to_string),
                );
            }
        }

        facts
    }
}

impl SourceFacts {
    /// The one line that names the file. Falls back to the provider, because a
    /// row with no title at all is a row that cannot be told from its
    /// neighbours.
    pub fn headline(&self, fallback: &str) -> String {
        self.release
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                if self.provider.is_empty() {
                    fallback.to_string()
                } else {
                    self.provider.clone()
                }
            })
    }

    /// The second line: size, how many are sharing it, where it came from, and
    /// what it is spoken in — in that order, because that is the order the
    /// question is asked in.
    pub fn chips(&self) -> Vec<String> {
        let mut chips = Vec::new();
        if let Some(size) = &self.size {
            chips.push(size.clone());
        }
        if let Some(seeders) = &self.seeders {
            chips.push(format!("{seeders} eş"));
        }
        if let Some(tracker) = &self.tracker {
            chips.push(tracker.clone());
        }
        // One language, and a flag before a word: "Multi Audio" takes the
        // width of three chips to say less than "🇫🇷" does. The word is kept
        // only when the addon gave no flag at all.
        let flag = self.languages.iter().find(|text| is_flag(text));
        if let Some(text) = flag.or_else(|| self.languages.first()) {
            chips.push(text.clone());
        }
        // The provider is not here on purpose — the panel is filtered by
        // provider and the filter says which, so repeating it on every row
        // costs the width the facts need. The caller puts it back, last, when
        // more than one is in the list.
        chips
    }

    /// The small line under the resolution badge — "DV · HDR".
    pub fn flag_line(&self) -> Option<String> {
        (!self.flags.is_empty()).then(|| {
            self.flags
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" · ")
        })
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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reason {
    pub code: String,
    pub severity: String,
    pub message: String,
}

/// A reason arrives either as a full record or as a bare sentence.
///
/// The media core's policy output carries `{code, severity, message}`, but
/// parts of the inspection path answer with a plain string — "the source has
/// no audio track" is one. Insisting on the record made the whole plan fail to
/// parse, and the television showed a serde message where the verdict should
/// have been. A sentence with no code is still a reason worth showing; what it
/// is not is an error.
impl<'de> Deserialize<'de> for Reason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Said(String),
            Told {
                #[serde(default)]
                code: String,
                #[serde(default)]
                severity: String,
                #[serde(default)]
                message: String,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Said(message) => Self {
                message,
                ..Self::default()
            },
            Wire::Told {
                code,
                severity,
                message,
            } => Self {
                code,
                severity,
                message,
            },
        })
    }
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
    /// Whether an object-audio substream is actually signalled. The worker
    /// sends a tri-state — `null` means "this probe could not tell", which is
    /// not the same as "no Atmos" — and reading it as a string made every plan
    /// for an Atmos title fail to parse, so the whole technical panel said
    /// "kaynak incelenemedi" for exactly the sources that needed it most.
    #[serde(rename = "objectAudio", default)]
    pub object_audio: Option<bool>,
    #[serde(rename = "objectAudioSource", default)]
    pub object_audio_source: Option<String>,
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

/// One thing this box can be used for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AppEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub owns_display: bool,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub active: bool,
}

/// What the television is showing, and what else it could show.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DisplayStatus {
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub applications: Vec<AppEntry>,
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
