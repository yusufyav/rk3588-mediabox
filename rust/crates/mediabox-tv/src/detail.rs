//! One title, its sources, and what this appliance will do with them.
//!
//! The first frame is never blank: the shelf already had a poster, a name and
//! often a backdrop, so the screen is drawn from that the moment it opens and
//! the record fills in behind it. A detail screen that waits for the network
//! before showing anything is a detail screen that reads as a delay.

use serde_json::Value;

use crate::model::{LibraryItemEnvelope, Meta, Plan, Stream, StreamListing};
use crate::state::Item;
use crate::{SourceRow, TechRow};

/// What the four marks are drawn from. SVG path data in a 24x24 box, in two
/// layers: the shape, and what is cut out of it in the button's own colour.
pub const MARKS: [(&str, &str); 4] = [
    // Play on the television.
    (
        "M1 4h22v13H1zM8 19h8v2H8z",
        "M10 8l6 3.5-6 3.5z",
    ),
    // Play here, in the interface's own player.
    (
        "M12 2a10 10 0 100 20 10 10 0 000-20z",
        "M10 8l6 4-6 4z",
    ),
    // The trailer.
    (
        "M3 5h18v14H3z",
        "M5 7h2v2H5zM5 11h2v2H5zM5 15h2v2H5zM17 7h2v2h-2zM17 11h2v2h-2zM17 15h2v2h-2zM10 9l5 3-5 3z",
    ),
    // Back.
    ("M11 4l-8 8 8 8v-5h9v-6h-9z", ""),
];

pub const ACTIONS: [&str; 4] = ["Kodi'de Oynat", "Bu Cihazda", "Fragman", "Geri"];

pub const ACTION_KODI: usize = 0;
pub const ACTION_HERE: usize = 1;
pub const ACTION_TRAILER: usize = 2;
pub const ACTION_BACK: usize = 3;

/// A source, kept both ways round.
///
/// The parsed form is what the row is laid out from; the raw value is what goes
/// back to the control plane when the viewer picks it. A round trip through our
/// own struct would drop every field we do not model, and an addon's stream
/// descriptor is the addon's, not ours.
pub struct Source {
    pub raw: Value,
    pub parsed: Stream,
}

pub struct Detail {
    pub kind: String,
    pub id: String,
    pub meta: Meta,
    pub sources: Vec<Source>,
    pub plan: Option<Plan>,

    /// Row 0 is the action strip; 1.. are the sources.
    pub row: usize,
    pub action: usize,
    /// Which source the technical panel and the play buttons are about.
    pub selected: Option<usize>,

    pub loading: bool,
    pub note: String,
}

impl Detail {
    /// Opened from a shelf, so everything the shelf knew is already here.
    pub fn seeded(item: &Item) -> Self {
        Self {
            kind: if item.local { "library".into() } else { item.kind.clone() },
            id: item.id.clone(),
            meta: Meta {
                id: item.id.clone(),
                kind: item.kind.clone(),
                name: item.title.clone(),
                poster: item.poster.clone(),
                background: item.background.clone(),
                logo: None,
                description: item.summary.clone(),
                release_info: item.year.clone(),
                runtime: None,
                imdb_rating: item.rating.clone(),
                genres: item.genres.clone(),
                cast: Vec::new(),
                director: Vec::new(),
                trailer: None,
            },
            sources: Vec::new(),
            plan: None,
            row: 0,
            action: ACTION_KODI,
            selected: None,
            loading: true,
            note: "Kaynaklar aranıyor…".into(),
        }
    }

    pub fn rows(&self) -> usize {
        1 + self.sources.len()
    }

    /// Moves the remote. Returns whether anything changed.
    pub fn step(&mut self, dx: isize, dy: isize) -> bool {
        let mut moved = false;

        if dy != 0 {
            let next = (self.row as isize + dy).clamp(0, self.rows() as isize - 1);
            if next != self.row as isize {
                self.row = next as usize;
                moved = true;
            }
        }

        // Left and right only mean anything on the action strip; the sources
        // are a column. Left from a source goes back to the actions, which is
        // the one thing a viewer tries when a list has taken the remote.
        if dx != 0 {
            if self.row == 0 {
                let next = (self.action as isize + dx).clamp(0, ACTIONS.len() as isize - 1);
                if next != self.action as isize {
                    self.action = next as usize;
                    moved = true;
                }
            } else if dx < 0 {
                self.row = 0;
                moved = true;
            }
        }

        moved
    }

    pub fn selected_source(&self) -> Option<&Source> {
        self.sources.get(self.selected?)
    }

    /// The record the media core answered with, over the shelf's seed.
    pub fn take_meta(&mut self, meta: Meta) {
        // The seed's artwork is already decoded and on the panel. Letting a
        // record with empty artwork fields overwrite it is how a detail screen
        // blinks to black a second after it opens.
        let poster = meta.poster.clone().or_else(|| self.meta.poster.clone());
        let background = meta.background.clone().or_else(|| self.meta.background.clone());
        self.meta = Meta { poster, background, ..meta };
    }

    pub fn take_streams(&mut self, listing: StreamListing, raw: Vec<Value>) {
        self.sources = listing
            .streams
            .into_iter()
            .zip(raw)
            .map(|(parsed, raw)| Source { raw, parsed })
            .collect();
        self.loading = false;
        self.note = match self.sources.len() {
            0 => "Bu başlık için kaynak yok".into(),
            n => format!("{n} kaynak"),
        };
        // The first that can actually play, so the buttons mean something
        // before the viewer has chosen anything.
        self.selected = self.sources.iter().position(|source| source.parsed.playable);
    }

    /// The appliance's own library answers with the record and the sources in
    /// one call.
    pub fn take_library(&mut self, envelope: LibraryItemEnvelope) {
        self.take_meta(envelope.meta);
        let raw: Vec<Value> = envelope
            .streams
            .iter()
            .map(|stream| {
                // A library source is a URL the operator wrote down. The
                // control plane takes it as a url rather than as an addon
                // descriptor, so the raw form only has to carry that.
                serde_json::json!({
                    "url": stream.url,
                    "addonId": crate::model::LIBRARY_ADDON_ID,
                })
            })
            .collect();
        let count = envelope.streams.len() as u32;
        self.take_streams(StreamListing { streams: envelope.streams, playable: count }, raw);
    }

    pub fn fail(&mut self, why: &str) {
        self.loading = false;
        self.sources.clear();
        self.note = why.to_string();
    }

    // ------------------------------------------------------------- rendering

    pub fn facts_line(&self) -> String {
        let mut facts: Vec<String> = Vec::new();
        if let Some(year) = non_empty(&self.meta.release_info) {
            facts.push(year);
        }
        if let Some(runtime) = non_empty(&self.meta.runtime) {
            facts.push(runtime);
        }
        if let Some(rating) = non_empty(&self.meta.imdb_rating) {
            facts.push(format!("IMDb {rating}"));
        }
        facts.join("  ·  ")
    }

    /// Cast and directors, in one strip. Five names is what fits and is also
    /// about as many as anybody reads.
    pub fn people(&self) -> Vec<String> {
        let mut people: Vec<String> = self.meta.director.iter().take(2).cloned().collect();
        people.extend(self.meta.cast.iter().take(5).cloned());
        people
    }

    /// Which actions can be pressed. Back is always one of them: a title with
    /// no sources would otherwise leave the screen with nothing to focus.
    pub fn enabled(&self) -> [bool; 4] {
        let playable = self
            .selected_source()
            .map(|source| source.parsed.playable)
            .unwrap_or(false);
        [
            playable,
            playable,
            non_empty(&self.meta.trailer).is_some(),
            true,
        ]
    }

    pub fn rows_for_display(&self) -> Vec<SourceRow> {
        // The provider is on every row only when there is more than one to tell
        // apart. Otherwise it is the same word repeated down the panel, taking
        // the width the release name needs.
        let providers: std::collections::HashSet<String> = self
            .sources
            .iter()
            .map(|source| source.parsed.facts().provider)
            .collect();
        let many = providers.len() > 1;

        self.sources
            .iter()
            .map(|source| {
                let facts = source.parsed.facts();
                let mut chips: Vec<slint::SharedString> =
                    facts.chips().into_iter().map(Into::into).collect();
                if many && !facts.provider.is_empty() {
                    chips.push(facts.provider.clone().into());
                }

                let (kind, tone) = kind_chip(&source.parsed.kind);

                SourceRow {
                    identity: source.parsed.identity.clone().into(),
                    quality: facts.quality.clone().unwrap_or_default().into(),
                    flags: facts.flag_line().unwrap_or_default().into(),
                    release: facts.headline(&source.parsed.label()).into(),
                    chips: slint::ModelRc::new(slint::VecModel::from(chips)),
                    kind: kind.into(),
                    tone: tone.into(),
                    cached: facts.cached,
                    playable: source.parsed.playable,
                    provider: facts.provider.into(),
                }
            })
            .collect()
    }

    /// The technical panel: what the media core found and decided, never what a
    /// file name claimed.
    pub fn technical(&self) -> Vec<TechRow> {
        let Some(plan) = &self.plan else { return Vec::new() };
        let mut rows = Vec::new();

        if let Some(video) = plan.video() {
            if let (Some(w), Some(h)) = (video.width, video.height) {
                rows.push(tech("Görüntü", &format!("{w}×{h}"), ""));
            }
            let mut codec = video.codec.clone().unwrap_or_default();
            if let Some(profile) = non_empty(&video.profile) {
                codec = format!("{codec} · {profile}");
            }
            if let Some(depth) = video.bit_depth {
                codec = format!("{codec} · {depth} bit");
            }
            if !codec.is_empty() {
                rows.push(tech("Kodek", &codec, ""));
            }
            if let Some(fps) = video.fps {
                rows.push(tech("Kare", &format!("{fps:.3} fps"), ""));
            }

            let (hdr, tone) = hdr_chip(video);
            if !hdr.is_empty() {
                rows.push(tech("HDR", &hdr, tone));
            }
        }

        if let Some(container) = &plan.media.container {
            if let Some(format) = non_empty(&container.format) {
                rows.push(tech("Kapsayıcı", &format, ""));
            }
            if let Some(size) = container.size_bytes {
                rows.push(tech("Boyut", &gigabytes(size), ""));
            }
        }

        if let Some(audio) = plan.audio() {
            let mut line = audio.codec.clone().unwrap_or_default();
            if let Some(layout) = non_empty(&audio.channel_layout) {
                line = format!("{line} · {layout}");
            }
            if !line.is_empty() {
                rows.push(tech("Ses", &line, ""));
            }
        }

        if let Some(note) = audio_note(plan) {
            rows.push(tech("Ses kararı", &note, ""));
        }

        // Everything the core had a reason to say, at the bottom, in its own
        // words.
        let reasons = plan
            .playback
            .reasons
            .iter()
            .chain(plan.playback.video.iter().flat_map(|v| v.reasons.iter()))
            .chain(plan.playback.audio.iter().flat_map(|a| a.reasons.iter()))
            .chain(plan.media.warnings.iter());
        for reason in reasons {
            if reason.message.is_empty() {
                continue;
            }
            rows.push(tech("Not", &reason.message, severity_tone(&reason.severity)));
        }

        rows
    }
}

fn tech(label: &str, value: &str, tone: &str) -> TechRow {
    TechRow { label: label.into(), value: value.into(), tone: tone.into() }
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn severity_tone(severity: &str) -> &'static str {
    match severity {
        "blocking" | "error" => "bad",
        "warning" => "warn",
        _ => "",
    }
}

fn kind_chip(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "url" => ("Doğrudan HTTP", "good"),
        "torrent" => ("Torrent", "warn"),
        "external" => ("Harici servis", "bad"),
        "youtube" => ("YouTube", ""),
        _ => ("", ""),
    }
}

/// Dolby Vision is never softened here. A profile whose base layer is not
/// backwards compatible is not "HDR with a caveat" on this box — it is a
/// picture that will come out wrong, and the panel should say so.
fn hdr_chip(video: &crate::model::VideoTrack) -> (String, &'static str) {
    if let Some(dv) = &video.dolby_vision {
        let profile = dv.profile.map(|p| format!("Dolby Vision {p}")).unwrap_or("Dolby Vision".into());
        return match dv.bl_signal_compatibility_id {
            Some(id) if id != 0 => (format!("{profile} · DV katmanı yok sayılır"), "warn"),
            _ => (format!("{profile} · desteklenmiyor"), "bad"),
        };
    }
    match non_empty(&video.hdr) {
        Some(hdr) => (hdr, "good"),
        None => (String::new(), ""),
    }
}

fn audio_note(plan: &Plan) -> Option<String> {
    let audio = plan.playback.audio.as_ref()?;
    Some(match audio.action.as_str() {
        "Passthrough" => "Olduğu gibi aktarılır".into(),
        "DecodeToPCM" => "PCM'e çözülür".into(),
        "TranscodeToAC3" => {
            let object = audio.track.as_ref().and_then(|t| t.object_audio).unwrap_or(false);
            if object {
                "AC-3'e çevrilir — nesne tabanlı ses katmanı kaybolur".into()
            } else {
                "AC-3'e çevrilir".into()
            }
        }
        "Unsupported" => "Bu ses bu cihazda çalınamaz".into(),
        other if other.is_empty() => return None,
        other => other.to_string(),
    })
}

fn gigabytes(bytes: f64) -> String {
    let gb = bytes / 1_000_000_000.0;
    if gb >= 1.0 {
        format!("{gb:.2} GB")
    } else {
        format!("{:.0} MB", bytes / 1_000_000.0)
    }
}
