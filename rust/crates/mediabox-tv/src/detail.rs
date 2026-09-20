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

/// What the marks are drawn from. SVG path data in a 24x24 box, in two layers:
/// the shape, and what is cut out of it in the button's own colour.
///
/// One mark, because there is one button. The reference's record carries the
/// trailer and nothing else: a film is played by choosing where it comes from,
/// in the column on the right, and Back is a key on the remote rather than a
/// word taking up the row.
pub const MARKS: [(&str, &str); 1] = [(
    // The trailer.
    "M3 5h18v14H3z",
    "M5 7h2v2H5zM5 11h2v2H5zM5 15h2v2H5zM17 7h2v2h-2zM17 11h2v2h-2zM17 15h2v2h-2zM10 9l5 3-5 3z",
)];

pub const ACTIONS: [&str; 1] = ["Fragman"];

pub const ACTION_TRAILER: usize = 0;

/// Which half of the screen the remote is in.
///
/// The sources are a column of this screen rather than a sheet behind a button.
/// The reference does it that way and it is right: choosing where a film comes
/// from is most of what this screen is for, and putting forty releases behind
/// "Kaynak Seç" made the page look like a record with nothing to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Record,
    Sources,
}

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

    /// Which half of the screen the remote is in.
    pub pane: Pane,
    /// Which button on the action row the remote is on.
    pub action: usize,
    /// Which source the play button and the technical panel are about, as an
    /// index into `sources`.
    pub selected: Option<usize>,
    /// Where the remote is in the source column, as a position in the *shown*
    /// list — which is the filtered one.
    pub source_focus: usize,
    /// 0 is "Tümü"; 1.. are the providers in `providers()`.
    pub provider: usize,
    /// True while the remote is on the provider filter rather than on a source.
    pub on_filter: bool,
    /// True while the filter's list is down over the column.
    ///
    /// The reference's filter is a dropdown: it opens onto the providers, one
    /// is picked, and it closes. The first version here walked them with Left
    /// and Right on a closed row, which is not the same control and did not
    /// look like one.
    pub filter_open: bool,
    /// Where the remote is inside the open list. 0 is "Tümü".
    pub filter_focus: usize,

    pub loading: bool,
    pub note: String,
}

impl Detail {
    /// Opened from a shelf, so everything the shelf knew is already here.
    pub fn seeded(item: &Item) -> Self {
        Self {
            kind: if item.local {
                "library".into()
            } else {
                item.kind.clone()
            },
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
            pane: Pane::Record,
            action: ACTION_TRAILER,
            selected: None,
            source_focus: 0,
            provider: 0,
            on_filter: false,
            filter_open: false,
            filter_focus: 0,
            loading: true,
            note: "Kaynaklar aranıyor…".into(),
        }
    }

    /// Who is offering the sources, in the order they first appear.
    ///
    /// The reference's own filter: "Tümü", then one entry per addon. A title
    /// with forty releases usually has them from two or three places, and being
    /// able to say "only this one" is the difference between a list and a
    /// choice.
    pub fn providers(&self) -> Vec<String> {
        let mut providers: Vec<String> = Vec::new();
        for source in &self.sources {
            let name = Self::provider_of(source);
            if !name.is_empty() && !providers.contains(&name) {
                providers.push(name);
            }
        }
        providers
    }

    /// Which addon produced a source.
    ///
    /// The addon says so itself, in the field the protocol has for it, and
    /// that is the only reading that matches the reference: Stremio groups a
    /// title's releases by the addon they came from, not by what the release
    /// happens to be called.
    ///
    /// This used to be parsed out of the first line of the stream's `name`
    /// instead, with the bracketed marks stripped off the front. That works
    /// for an addon whose first line is exactly its own name and for no other
    /// kind. Addons that put the resolution on the same line -- "Torrentio
    /// 4k" -- produced one "provider" per resolution, so the filter offered
    /// the same addon three times and each entry hid the rest of its own
    /// releases. Addons that put a debrid tag outside the brackets produced
    /// another. The parsed name is still what the row is labelled with, which
    /// is what it is good for.
    fn provider_of(source: &Source) -> String {
        if let Some(name) = source
            .parsed
            .addon_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return name.to_string();
        }
        // An addon that named neither itself nor its id is one this list can
        // only tell apart by what it wrote, so fall back to that.
        if let Some(id) = source
            .parsed
            .addon_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            return id.to_string();
        }
        source.parsed.facts().provider
    }

    /// The sources the column is showing, as indices into `sources`.
    pub fn shown(&self) -> Vec<usize> {
        let providers = self.providers();
        let wanted = self
            .provider
            .checked_sub(1)
            .and_then(|i| providers.get(i).cloned());
        self.sources
            .iter()
            .enumerate()
            .filter(|(_, source)| match &wanted {
                Some(name) => Self::provider_of(source) == *name,
                None => true,
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The source the remote is on, as an index into `sources`.
    pub fn focused_source(&self) -> Option<usize> {
        self.shown().get(self.source_focus).copied()
    }

    /// Moves the remote. Returns whether anything changed.
    ///
    /// Left and right cross between the record and the source column, and only
    /// there: inside the record they walk the action row, and inside the column
    /// they change the provider filter when the remote is on it.
    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        match self.pane {
            Pane::Record => self.step_record(dx, dy),
            Pane::Sources => self.step_sources(dx, dy),
        }
    }

    fn step_record(&mut self, dx: i32, dy: i32) -> bool {
        if dx > 0 {
            let enabled = self.enabled();
            let last = enabled.iter().rposition(|ok| *ok).unwrap_or(0);
            if self.action >= last {
                if self.sources.is_empty() {
                    return false;
                }
                self.pane = Pane::Sources;
                self.on_filter = false;
                return true;
            }
        }
        if dy != 0 {
            return false;
        }
        if dx == 0 {
            return false;
        }
        let mut next = self.action as i32 + dx;
        // A button that cannot be pressed — no trailer, no playable source —
        // is stepped over rather than landed on.
        let enabled = self.enabled();
        while next >= 0 && (next as usize) < ACTIONS.len() && !enabled[next as usize] {
            next += dx.signum();
        }
        if next < 0 || next as usize >= ACTIONS.len() || next as usize == self.action {
            return false;
        }
        self.action = next as usize;
        true
    }

    fn step_sources(&mut self, dx: i32, dy: i32) -> bool {
        if self.filter_open {
            // The list is down: Up and Down walk it and nothing else moves.
            // Left and Right belong to the screen behind it and would take the
            // remote somewhere it cannot see.
            if dy < 0 && self.filter_focus > 0 {
                self.filter_focus -= 1;
                return true;
            }
            if dy > 0 && self.filter_focus + 1 < self.providers().len() + 1 {
                self.filter_focus += 1;
                return true;
            }
            return false;
        }
        if dx < 0 {
            // Left out of the column is the record — but only when there is
            // something there to land on. A title with no trailer has an empty
            // action row, and moving the remote onto it would strand it.
            if !self.enabled().iter().any(|ok| *ok) {
                return false;
            }
            self.pane = Pane::Record;
            self.settle();
            return true;
        }
        if dx > 0 {
            return false;
        }
        if dy < 0 {
            if self.on_filter {
                return false;
            }
            if self.source_focus == 0 {
                // Up off the top of the list is the filter above it.
                self.on_filter = true;
                return true;
            }
            self.source_focus -= 1;
            return true;
        }
        if dy > 0 {
            if self.on_filter {
                self.on_filter = false;
                return true;
            }
            let shown = self.shown().len();
            if self.source_focus + 1 >= shown {
                return false;
            }
            self.source_focus += 1;
            return true;
        }
        false
    }

    /// Ok on the closed filter: the list comes down on the provider it is
    /// already showing.
    pub fn open_filter(&mut self) -> bool {
        if self.filter_open || self.providers().is_empty() {
            return false;
        }
        self.filter_open = true;
        self.filter_focus = self.provider;
        true
    }

    /// Ok inside the list: this is the provider now, and the list goes up.
    pub fn choose_provider(&mut self) -> bool {
        if !self.filter_open {
            return false;
        }
        self.filter_open = false;
        if self.filter_focus == self.provider {
            return true;
        }
        self.provider = self.filter_focus;
        self.source_focus = 0;
        true
    }

    /// Back inside the list: it goes up and nothing has changed.
    pub fn close_filter(&mut self) -> bool {
        if !self.filter_open {
            return false;
        }
        self.filter_open = false;
        true
    }

    /// Puts the remote on the first button that can actually be pressed. Called
    /// when the sources land, because until then only Back is live.
    pub fn settle(&mut self) {
        let enabled = self.enabled();
        if enabled.get(self.action).copied().unwrap_or(false) {
            return;
        }
        self.action = enabled.iter().position(|ok| *ok).unwrap_or(ACTION_TRAILER);
    }

    /// Ok in the source column: this is the one it plays from now.
    pub fn choose_source(&mut self) -> Option<usize> {
        let index = self.focused_source()?;
        self.selected = Some(index);
        Some(index)
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
        let background = meta
            .background
            .clone()
            .or_else(|| self.meta.background.clone());
        self.meta = Meta {
            poster,
            background,
            ..meta
        };
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
            // A count is not something a viewer has to be told: the column is
            // right there and the reference says nothing above it.
            _ => String::new(),
        };
        // The first that can actually play, so the buttons mean something
        // before the viewer has chosen anything.
        self.selected = self
            .sources
            .iter()
            .position(|source| source.parsed.playable);
        self.provider = 0;
        self.on_filter = false;
        self.filter_open = false;
        self.filter_focus = 0;
        self.source_focus = self
            .selected
            .and_then(|index| self.shown().iter().position(|shown| *shown == index))
            .unwrap_or(0);
        self.settle();
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
        self.take_streams(
            StreamListing {
                streams: envelope.streams,
                playable: count,
            },
            raw,
        );
    }

    pub fn fail(&mut self, why: &str) {
        self.loading = false;
        self.sources.clear();
        self.selected = None;
        self.note = why.to_string();
        self.settle();
    }

    // ------------------------------------------------------------- rendering

    /// How long, when, and what it scored — in the reference's order, and as
    /// separate words because the last of them wears IMDb's badge rather than
    /// their name.
    pub fn facts(&self) -> Vec<String> {
        let mut facts: Vec<String> = Vec::new();
        if let Some(runtime) = non_empty(&self.meta.runtime) {
            facts.push(runtime);
        }
        if let Some(year) = non_empty(&self.meta.release_info) {
            facts.push(year);
        }
        if let Some(rating) = non_empty(&self.meta.imdb_rating) {
            facts.push(rating);
        }
        facts
    }

    /// Whether the last of the facts is a rating, and so whether the badge is
    /// drawn after it.
    pub fn has_rating(&self) -> bool {
        non_empty(&self.meta.imdb_rating).is_some()
    }

    /// The genres, in the language the rest of the screen is in.
    ///
    /// The catalogue answers in English whatever the interface asks in, and a
    /// Turkish page that says "Comedy" is a page that has not been translated.
    /// The list is Cinemeta's own and it is closed: anything outside it is left
    /// as the catalogue wrote it rather than guessed at.
    pub fn genres(&self) -> Vec<String> {
        self.meta
            .genres
            .iter()
            .take(4)
            .map(|genre| genre_in_turkish(genre))
            .collect()
    }

    /// Which actions can be pressed.
    ///
    /// A title with no trailer has none, and the record then has nothing to
    /// focus — which is why Left out of the source column checks this before
    /// it moves the remote anywhere.
    pub fn enabled(&self) -> [bool; 1] {
        [non_empty(&self.meta.trailer).is_some()]
    }

    /// How long the film is, in seconds, if the catalogue said.
    ///
    /// The catalogue writes it for a person to read rather than for a machine,
    /// and not always the same way, so every shape seen from these addons is
    /// handled and anything else is refused rather than guessed at. Nothing
    /// here is allowed to produce a number from a string it did not
    /// understand: a wrong runtime is a progress bar that lies.
    pub fn runtime_seconds(&self) -> Option<u64> {
        runtime_seconds(self.meta.runtime.as_deref()?)
    }

    /// The technical rows, as pairs, for whatever screen wants them next — the
    /// now-playing screen carries them over when a film starts.
    pub fn technical_pairs(&self) -> Vec<(String, String)> {
        self.technical()
            .into_iter()
            .map(|row| (row.label.to_string(), row.value.to_string()))
            .collect()
    }

    /// The rows the column draws: the filtered list, in order.
    ///
    /// Both strings are the addon's own, untouched. The reference draws the
    /// stream's name down the left of the row and its title down the right,
    /// with the line breaks and the pictograms the addon put there, and that is
    /// what a viewer choosing between two releases is reading.
    pub fn rows_for_display(&self) -> Vec<SourceRow> {
        self.shown()
            .into_iter()
            .filter_map(|index| self.sources.get(index))
            .map(|source| {
                let (title, meta) = source.parsed.file_lines();
                SourceRow {
                    name: source.parsed.addon_label().into(),
                    title: title.into(),
                    meta: meta.into(),
                    playable: source.parsed.playable,
                }
            })
            .collect()
    }

    /// The technical panel: what the media core found and decided, never what a
    /// file name claimed.
    pub fn technical(&self) -> Vec<TechRow> {
        let Some(plan) = &self.plan else {
            return Vec::new();
        };
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

        // Only what the core had a *blocking* reason to say. The full list of
        // notes is the media core's own report and belongs on the diagnostics
        // screen; a title's page carrying six lines of "hevc Main 10 is hardware
        // decodable" is a page about the appliance rather than about the film.
        let reasons = plan
            .playback
            .reasons
            .iter()
            .chain(plan.playback.video.iter().flat_map(|v| v.reasons.iter()))
            .chain(plan.playback.audio.iter().flat_map(|a| a.reasons.iter()))
            .chain(plan.media.warnings.iter());
        for reason in reasons {
            if reason.message.is_empty() || severity_tone(&reason.severity).is_empty() {
                continue;
            }
            rows.push(tech(
                "Not",
                &reason.message,
                severity_tone(&reason.severity),
            ));
        }

        // Four is what the column has room for and about as much as anybody
        // reads from a sofa.
        rows.truncate(4);
        rows
    }
}

fn tech(label: &str, value: &str, tone: &str) -> TechRow {
    TechRow {
        label: label.into(),
        value: value.into(),
        tone: tone.into(),
    }
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn severity_tone(severity: &str) -> &'static str {
    match severity {
        "blocking" | "error" => "bad",
        "warning" => "warn",
        _ => "",
    }
}

/// Dolby Vision is never softened here. A profile whose base layer is not
/// backwards compatible is not "HDR with a caveat" on this box — it is a
/// picture that will come out wrong, and the panel should say so.
fn hdr_chip(video: &crate::model::VideoTrack) -> (String, &'static str) {
    if let Some(dv) = &video.dolby_vision {
        let profile = dv
            .profile
            .map(|p| format!("Dolby Vision {p}"))
            .unwrap_or("Dolby Vision".into());
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
            let object = audio
                .track
                .as_ref()
                .and_then(|t| t.object_audio)
                .unwrap_or(false);
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

/// How long a film is, from what the catalogue wrote for a reader.
///
/// "99 min", "1h 39min", "1 h 39 m", "99" — hours and minutes, spelt several
/// ways by several addons. Anything this does not recognise returns None and
/// the interface draws an unknown length as unknown: a guessed runtime is a
/// progress bar that is wrong and looks right, which is how a ninety-nine
/// minute film came up as three minutes and forty-five seconds.
fn runtime_seconds(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    if text.is_empty() {
        return None;
    }

    let mut hours = 0u64;
    let mut minutes = 0u64;
    let mut understood = false;
    let mut bare: Option<u64> = None;

    let add = |unit: &str, value: u64, hours: &mut u64, minutes: &mut u64| -> bool {
        match unit {
            "h" | "hr" | "hrs" | "hour" | "hours" | "sa" | "saat" => *hours += value,
            "m" | "min" | "mins" | "minute" | "minutes" | "dk" | "dakika" => *minutes += value,
            _ => return false,
        }
        true
    };

    for part in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        // "39min" and "1h" arrive glued together as often as not.
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        let unit: String = part.chars().skip(digits.len()).collect();

        if !digits.is_empty() {
            let value = digits.parse::<u64>().ok()?;
            if unit.is_empty() {
                // A number on its own: its unit may be the next word.
                if let Some(previous) = bare.replace(value) {
                    minutes += previous;
                    understood = true;
                }
            } else if add(&unit, value, &mut hours, &mut minutes) {
                understood = true;
            } else {
                return None;
            }
            continue;
        }

        let Some(value) = bare.take() else {
            return None;
        };
        if !add(&unit, value, &mut hours, &mut minutes) {
            return None;
        }
        understood = true;
    }

    // A trailing number with nothing after it is minutes, which is how every
    // one of these addons writes a film's length.
    if let Some(value) = bare {
        minutes += value;
        understood = true;
    }

    let total = hours * 3600 + minutes * 60;
    (understood && total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The progress bar is only as honest as this function.
    #[test]
    fn a_runtime_written_for_a_reader_becomes_seconds() {
        assert_eq!(runtime_seconds("99 min"), Some(99 * 60));
        assert_eq!(runtime_seconds("99"), Some(99 * 60));
        assert_eq!(runtime_seconds("105 minutes"), Some(105 * 60));
        assert_eq!(runtime_seconds("1h 39min"), Some(3600 + 39 * 60));
        assert_eq!(runtime_seconds("1 h 39 m"), Some(3600 + 39 * 60));
        assert_eq!(runtime_seconds("2h"), Some(7200));
        assert_eq!(runtime_seconds("118 dk"), Some(118 * 60));
    }

    /// Anything unrecognised is refused rather than guessed at.
    #[test]
    fn an_unrecognised_runtime_is_not_guessed_at() {
        assert_eq!(runtime_seconds(""), None);
        assert_eq!(runtime_seconds("   "), None);
        assert_eq!(runtime_seconds("bilinmiyor"), None);
        assert_eq!(runtime_seconds("0 min"), None);
        assert_eq!(runtime_seconds("S01E04"), None);
    }
}

/// Cinemeta's genre list, in Turkish. Closed on purpose: a genre outside it is
/// drawn as the catalogue wrote it rather than mistranslated.
fn genre_in_turkish(genre: &str) -> String {
    let turkish = match genre.trim() {
        "Action" => "Aksiyon",
        "Adventure" => "Macera",
        "Animation" => "Animasyon",
        "Biography" => "Biyografi",
        "Comedy" => "Komedi",
        "Crime" => "Suç",
        "Documentary" => "Belgesel",
        "Drama" => "Dram",
        "Family" => "Aile",
        "Fantasy" => "Fantastik",
        "Film-Noir" => "Kara Film",
        "Game-Show" => "Yarışma",
        "History" => "Tarih",
        "Horror" => "Korku",
        "Music" => "Müzik",
        "Musical" => "Müzikal",
        "Mystery" => "Gizem",
        "News" => "Haber",
        "Reality-TV" => "Realite",
        "Romance" => "Romantik",
        "Sci-Fi" => "Bilim Kurgu",
        "Short" => "Kısa Film",
        "Sport" => "Spor",
        "Talk-Show" => "Talk Show",
        "Thriller" => "Gerilim",
        "War" => "Savaş",
        "Western" => "Western",
        other => return other.to_string(),
    };
    turkish.to_string()
}
