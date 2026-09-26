//! One title, its sources, and what this appliance will do with them.
//!
//! The first frame is never blank: the shelf already had a poster, a name and
//! often a backdrop, so the screen is drawn from that the moment it opens and
//! the record fills in behind it. A detail screen that waits for the network
//! before showing anything is a detail screen that reads as a delay.

use serde_json::Value;

use crate::model::{LibraryItemEnvelope, Meta, Plan, Stream, StreamListing, TitleState, Video};
use crate::state::Item;
use crate::{EpisodeRow, SourceRow, TechRow};

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
    /// A series' seasons and episodes, in the column the sources use. The
    /// reference puts them there and swaps the column over to the sources when
    /// an episode is chosen.
    Episodes,
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

    /// How far the title has been watched, once the account has said.
    pub watch: Option<TitleState>,
    /// The season the viewer chose. Until they choose one, `season()` picks it
    /// by the reference's rule.
    chosen_season: Option<i64>,
    /// Where the remote is in the episodes of the season shown.
    pub episode_focus: usize,
    /// True while the remote is on the season bar above the episodes.
    pub on_seasons: bool,
    /// The episode the source column is about, for a series.
    pub episode: Option<String>,
    /// Whether the viewer has moved in the episode list yet. Until they have,
    /// the focus follows the account's last-played episode as it arrives.
    episodes_touched: bool,
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
                videos: Vec::new(),
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
            loading: item.kind != "series",
            note: if item.kind == "series" {
                String::new()
            } else {
                "Kaynaklar aranıyor…".into()
            },
            watch: None,
            chosen_season: None,
            episode_focus: 0,
            on_seasons: false,
            episode: None,
            episodes_touched: false,
        }
    }

    /// A series is browsed by episode; its own id has no sources.
    pub fn is_series(&self) -> bool {
        self.kind == "series"
    }

    /// Who is offering the sources, in the order they first appear.
    ///
    /// The reference's own filter: "Tümü", then one entry per addon. A title
    /// with forty releases usually has them from two or three places, and being
    /// able to say "only this one" is the difference between a list and a
    /// choice.
    pub fn providers(&self) -> Vec<String> {
        // Each addon once, and in the order the person put their addons in.
        //
        // The order is asked for rather than inferred from where an addon's
        // first source happens to land in the list. It is the same order
        // today -- the media core merges a title's sources addon by addon, in
        // collection order, on purpose -- but a filter that infers it is one
        // that reorders itself silently the first time a source is
        // deduplicated away or the list is sorted by anything else, and a
        // list that reorders itself between two openings of the same title is
        // a list nobody can learn. An addon that did not say where it sits
        // goes after the ones that did, keeping the order it appeared in.
        let mut seen: Vec<(u32, usize, String)> = Vec::new();
        for (appearance, source) in self.sources.iter().enumerate() {
            let name = Self::provider_of(source);
            if name.is_empty() || seen.iter().any(|(_, _, held)| *held == name) {
                continue;
            }
            let order = source.parsed.addon_order.unwrap_or(u32::MAX);
            seen.push((order, appearance, name));
        }
        seen.sort_by_key(|(order, appearance, _)| (*order, *appearance));
        seen.into_iter().map(|(_, _, name)| name).collect()
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
    /// kind. An addon that puts the resolution on that line as well produced
    /// one "provider" per resolution, so the filter offered the same addon
    /// several times and each entry hid the rest of its own releases; one
    /// that puts a debrid tag outside the brackets produced another; and one
    /// whose first line names the service it links out to was filed under
    /// that service rather than under itself. The parsed name is still what
    /// the row is labelled with, which is what it is good for.
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
            Pane::Episodes => self.step_episodes(dx, dy),
            Pane::Sources => self.step_sources(dx, dy),
        }
    }

    fn step_record(&mut self, dx: i32, dy: i32) -> bool {
        if dx > 0 {
            let enabled = self.enabled();
            let last = enabled.iter().rposition(|ok| *ok).unwrap_or(0);
            if self.action >= last || !enabled.iter().any(|ok| *ok) {
                if self.is_series() {
                    if self.meta.videos.is_empty() {
                        return false;
                    }
                    self.pane = Pane::Episodes;
                    return true;
                }
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
        self.follow_last_played();
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

    // -------------------------------------------------------------- episodes
    //
    // The rules are stremio-web's `VideosList`, read rather than guessed:
    //
    // - the seasons are the season numbers the episodes carry, in order, with
    //   season 0 -- the specials -- last. A season the addon did not list is
    //   not there, and neither is an episode it did not list;
    // - the season shown is the one the viewer chose; before they choose, the
    //   season of the episode the account says was played last; failing that
    //   the first season that is not the specials; failing that the first;
    // - a season's episodes are in episode order.

    /// The seasons, specials last.
    pub fn seasons(&self) -> Vec<i64> {
        let mut seasons: Vec<i64> = self.meta.videos.iter().filter_map(|v| v.season).collect();
        seasons.sort_by_key(|season| if *season == 0 { i64::MAX } else { *season });
        seasons.dedup();
        seasons
    }

    /// The season on the panel.
    pub fn season(&self) -> Option<i64> {
        let seasons = self.seasons();
        if let Some(chosen) = self.chosen_season.filter(|s| seasons.contains(s)) {
            return Some(chosen);
        }
        let last_played = self
            .watch
            .as_ref()
            .and_then(|watch| watch.video_id.as_deref())
            .and_then(|id| self.meta.videos.iter().find(|v| v.id == id))
            .and_then(|video| video.season)
            .filter(|season| *season != 0 && seasons.contains(season));
        last_played
            .or_else(|| seasons.iter().copied().find(|season| *season != 0))
            .or_else(|| seasons.first().copied())
    }

    /// The episodes of the season on the panel, in episode order.
    pub fn episodes(&self) -> Vec<&Video> {
        let season = self.season();
        let mut episodes: Vec<&Video> = self
            .meta
            .videos
            .iter()
            .filter(|video| season.is_none() || video.season == season)
            .collect();
        episodes.sort_by_key(|video| video.episode.unwrap_or(0));
        episodes
    }

    pub fn focused_episode(&self) -> Option<&Video> {
        self.episodes().get(self.episode_focus).copied()
    }

    /// The account's word on this title, once it arrives. Until the viewer has
    /// moved, the remote goes to the episode that was played last.
    pub fn take_watch(&mut self, watch: TitleState) {
        self.watch = Some(watch);
        self.follow_last_played();
    }

    fn follow_last_played(&mut self) {
        if self.episodes_touched {
            return;
        }
        let last = self.watch.as_ref().and_then(|watch| watch.video_id.clone());
        self.episode_focus = last
            .and_then(|id| self.episodes().iter().position(|video| video.id == id))
            .unwrap_or(0);
    }

    fn step_episodes(&mut self, dx: i32, dy: i32) -> bool {
        self.episodes_touched = true;
        if self.on_seasons {
            if dx != 0 {
                let seasons = self.seasons();
                let Some(current) = self.season() else {
                    return false;
                };
                let at = seasons.iter().position(|s| *s == current).unwrap_or(0) as i32;
                let next = at + dx;
                if next < 0 {
                    return self.leave_for_record();
                }
                let Some(season) = seasons.get(next as usize).copied() else {
                    return false;
                };
                self.chosen_season = Some(season);
                self.episode_focus = 0;
                return true;
            }
            if dy > 0 && !self.episodes().is_empty() {
                self.on_seasons = false;
                return true;
            }
            return false;
        }
        if dx < 0 {
            return self.leave_for_record();
        }
        if dy < 0 {
            if self.episode_focus == 0 {
                self.on_seasons = true;
                return true;
            }
            self.episode_focus -= 1;
            return true;
        }
        if dy > 0 && self.episode_focus + 1 < self.episodes().len() {
            self.episode_focus += 1;
            return true;
        }
        false
    }

    fn leave_for_record(&mut self) -> bool {
        if !self.enabled().iter().any(|ok| *ok) {
            return false;
        }
        self.pane = Pane::Record;
        self.settle();
        true
    }

    /// Ok on an episode: the column turns over to its sources, which are asked
    /// for by the episode's own id. Returns that id.
    pub fn choose_episode(&mut self) -> Option<String> {
        let id = self.focused_episode()?.id.clone();
        self.episode = Some(id.clone());
        self.sources.clear();
        self.selected = None;
        self.plan = None;
        self.loading = true;
        self.note = "Kaynaklar aranıyor…".into();
        self.pane = Pane::Sources;
        self.on_filter = false;
        self.filter_open = false;
        self.provider = 0;
        self.source_focus = 0;
        Some(id)
    }

    /// Back out of an episode's sources: the column turns back over to the
    /// episodes, with the remote on the one that was opened.
    pub fn back_to_episodes(&mut self) -> bool {
        if !self.is_series() || self.pane != Pane::Sources {
            return false;
        }
        self.pane = Pane::Episodes;
        self.on_seasons = false;
        self.filter_open = false;
        true
    }

    /// The episode as the header over its sources names it: "S2E3 · Title".
    pub fn episode_heading(&self) -> String {
        let Some(id) = self.episode.as_deref() else {
            return String::new();
        };
        let Some(video) = self.meta.videos.iter().find(|video| video.id == id) else {
            return id.to_string();
        };
        let code = match (video.season, video.episode) {
            (Some(season), Some(episode)) => format!("S{season}E{episode}"),
            _ => String::new(),
        };
        match non_empty(&video.title) {
            Some(title) if !code.is_empty() => format!("{code} · {title}"),
            Some(title) => title,
            None => code,
        }
    }

    /// The season bar's words: "1. Sezon", and "Özel" for season 0.
    pub fn season_labels(&self) -> Vec<String> {
        self.seasons()
            .into_iter()
            .map(|season| {
                if season == 0 {
                    "Özel".to_string()
                } else {
                    format!("{season}. Sezon")
                }
            })
            .collect()
    }

    pub fn season_index(&self) -> usize {
        let current = self.season();
        self.seasons()
            .iter()
            .position(|season| Some(*season) == current)
            .unwrap_or(0)
    }

    /// The episode rows, as the reference draws them: "3. Title", when it was
    /// released (or that it has not been yet), whether it was watched, and how
    /// far into it the account got, for the episode that was played last.
    /// Artwork is filled in by the painter.
    pub fn episode_rows(&self, today: &str) -> Vec<EpisodeRow> {
        let watch = self.watch.as_ref();
        self.episodes()
            .into_iter()
            .map(|video| {
                let number = video.episode.map(|n| format!("{n}. ")).unwrap_or_default();
                let title = non_empty(&video.title).unwrap_or_else(|| video.id.clone());
                let date = video.released.as_deref().and_then(date_prefix);
                let upcoming = date.as_deref().is_some_and(|date| date > today);
                let watched = watch.is_some_and(|w| w.watched.iter().any(|id| *id == video.id));
                let progress = watch
                    .filter(|w| w.video_id.as_deref() == Some(video.id.as_str()))
                    .and_then(|w| match (w.time_offset, w.duration) {
                        (Some(at), Some(of)) if of > 0 && at > 0 => {
                            Some((at as f32 / of as f32).clamp(0.0, 1.0))
                        }
                        _ => None,
                    })
                    .unwrap_or(0.0);
                EpisodeRow {
                    title: format!("{number}{title}").into(),
                    date: date
                        .as_deref()
                        .map(turkish_date)
                        .unwrap_or_else(|| "Tarih belli değil".into())
                        .into(),
                    watched,
                    upcoming,
                    progress,
                    art: slint::Image::default(),
                }
            })
            .collect()
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

/// The date part of an ISO 8601 timestamp, "2009-03-22".
fn date_prefix(released: &str) -> Option<String> {
    let date = released.get(..10)?;
    let bytes = date.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    shaped.then(|| date.to_string())
}

/// "22 Mart 2009".
fn turkish_date(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül",
        "Ekim", "Kasım", "Aralık",
    ];
    let year = &date[0..4];
    let month: usize = date[5..7].parse().unwrap_or(0);
    let day: u32 = date[8..10].parse().unwrap_or(0);
    match MONTHS.get(month.wrapping_sub(1)) {
        Some(name) => format!("{day} {name} {year}"),
        None => date.to_string(),
    }
}

/// Today, as the date part of an ISO 8601 timestamp, in UTC -- which is what
/// the addons write their release dates in.
pub fn today() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Howard Hinnant's days-to-civil.
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
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
mod filter_tests {
    use super::*;
    use serde_json::json;

    fn source(addon: &str, order: Option<u32>, name: &str) -> Source {
        let mut raw = json!({"addonName": addon, "name": name, "playable": true});
        if let Some(order) = order {
            raw["addonOrder"] = json!(order);
        }
        Source {
            parsed: serde_json::from_value(raw.clone()).expect("a stream"),
            raw,
        }
    }

    fn with(sources: Vec<Source>) -> Detail {
        let mut detail = Detail::seeded(&crate::state::Item::stub("x"));
        detail.sources = sources;
        detail
    }

    #[test]
    fn the_filter_groups_by_the_addon_and_not_by_what_the_release_is_called() {
        // One addon that writes the resolution into the same line it names
        // itself on. Parsed from the text this was three providers, each
        // hiding the other two thirds of its own releases.
        let detail = with(vec![
            source("an addon", Some(0), "an addon | cached 2160p"),
            source("an addon", Some(0), "an addon | cached 1080p"),
            source("an addon", Some(0), "an addon | cached 720p"),
        ]);
        assert_eq!(detail.providers(), vec!["an addon".to_string()]);
        assert_eq!(detail.shown().len(), 3);
    }

    #[test]
    fn a_link_out_is_named_after_its_addon_not_after_the_service_it_points_at() {
        // An addon whose first line is the name of the service it links out
        // to, which is not the name of the addon.
        let detail = with(vec![source("an addon", Some(0), "some streaming service")]);
        assert_eq!(detail.providers(), vec!["an addon".to_string()]);
    }

    #[test]
    fn the_groups_come_in_the_order_the_addons_are_installed_in() {
        // Deliberately not the order they appear in the list.
        let detail = with(vec![
            source("Third", Some(2), "a"),
            source("First", Some(0), "b"),
            source("Second", Some(1), "c"),
        ]);
        assert_eq!(
            detail.providers(),
            vec![
                "First".to_string(),
                "Second".to_string(),
                "Third".to_string()
            ]
        );
    }

    #[test]
    fn an_addon_that_did_not_say_where_it_sits_goes_last() {
        let detail = with(vec![
            source("Quiet", None, "a"),
            source("Placed", Some(3), "b"),
        ]);
        assert_eq!(
            detail.providers(),
            vec!["Placed".to_string(), "Quiet".to_string()]
        );
    }

    #[test]
    fn choosing_a_group_shows_that_addon_and_only_that_addon() {
        let mut detail = with(vec![
            source("A", Some(0), "one"),
            source("B", Some(1), "two"),
            source("A", Some(0), "three"),
        ]);
        detail.provider = 1; // "Tümü" is 0, so this is the first addon.
        assert_eq!(detail.shown(), vec![0, 2]);
    }
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

#[cfg(test)]
mod series_tests {
    use super::*;

    fn video(id: &str, season: i64, episode: i64, released: &str) -> Video {
        Video {
            id: id.into(),
            title: Some(format!("Episode {id}")),
            season: Some(season),
            episode: Some(episode),
            released: Some(released.into()),
            overview: None,
            thumbnail: None,
        }
    }

    fn series(videos: Vec<Video>) -> Detail {
        let mut item = crate::state::Item::stub("tt0903747");
        item.kind = "series".into();
        let mut detail = Detail::seeded(&item);
        let mut meta = detail.meta.clone();
        meta.videos = videos;
        detail.take_meta(meta);
        detail
    }

    fn breaking_bad() -> Detail {
        series(vec![
            // Deliberately out of order, with the specials first and a gap
            // where season 3 would be.
            video("tt:2:2", 2, 2, "2009-03-15T00:00:00.000Z"),
            video("tt:0:1", 0, 1, "2009-02-17T00:00:00.000Z"),
            video("tt:1:1", 1, 1, "2008-01-20T00:00:00.000Z"),
            video("tt:2:1", 2, 1, "2009-03-08T00:00:00.000Z"),
            video("tt:4:1", 4, 1, "2011-07-17T00:00:00.000Z"),
            video("tt:2:3", 2, 3, "2009-03-22T00:00:00.000Z"),
        ])
    }

    #[test]
    fn seasons_come_from_the_episodes_with_the_specials_last_and_no_gaps_filled() {
        assert_eq!(breaking_bad().seasons(), vec![1, 2, 4, 0]);
        assert_eq!(breaking_bad().season_labels(), vec!["1. Sezon", "2. Sezon", "4. Sezon", "Özel"]);
    }

    #[test]
    fn a_new_series_opens_on_its_first_real_season() {
        assert_eq!(breaking_bad().season(), Some(1));
        let specials_only = series(vec![video("tt:0:1", 0, 1, "2009-01-01")]);
        assert_eq!(specials_only.season(), Some(0));
    }

    #[test]
    fn a_series_being_watched_opens_on_the_last_played_episode() {
        let mut detail = breaking_bad();
        detail.take_watch(TitleState {
            video_id: Some("tt:2:3".into()),
            ..TitleState::default()
        });
        assert_eq!(detail.season(), Some(2));
        assert_eq!(detail.focused_episode().unwrap().id, "tt:2:3");
    }

    #[test]
    fn a_seasons_episodes_are_in_episode_order() {
        let mut detail = breaking_bad();
        detail.chosen_season = Some(2);
        let ids: Vec<&str> = detail.episodes().iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, vec!["tt:2:1", "tt:2:2", "tt:2:3"]);
    }

    #[test]
    fn the_season_bar_walks_the_seasons_there_are() {
        let mut detail = breaking_bad();
        detail.pane = Pane::Episodes;
        detail.on_seasons = true;
        assert!(detail.step(1, 0));
        assert_eq!(detail.season(), Some(2));
        assert!(detail.step(1, 0));
        assert_eq!(detail.season(), Some(4), "season 3 does not exist and is not stepped onto");
        assert!(detail.step(1, 0));
        assert_eq!(detail.season(), Some(0));
        assert!(!detail.step(1, 0));
    }

    /// The fault this is for: a series was asked for sources by its own id.
    #[test]
    fn an_episode_is_asked_for_by_its_own_id() {
        let mut detail = breaking_bad();
        detail.chosen_season = Some(2);
        detail.pane = Pane::Episodes;
        detail.step(0, 1);
        detail.step(0, 1);
        assert_eq!(detail.choose_episode().as_deref(), Some("tt:2:3"));
        assert_eq!(detail.pane, Pane::Sources);
        assert_eq!(detail.episode_heading(), "S2E3 · Episode tt:2:3");
    }

    #[test]
    fn back_from_an_episodes_sources_lands_on_that_episode() {
        let mut detail = breaking_bad();
        detail.chosen_season = Some(2);
        detail.pane = Pane::Episodes;
        detail.step(0, 1);
        let opened = detail.choose_episode().unwrap();
        assert!(detail.back_to_episodes());
        assert_eq!(detail.pane, Pane::Episodes);
        assert_eq!(detail.focused_episode().unwrap().id, opened);
    }

    #[test]
    fn a_film_has_no_episodes_and_is_asked_for_sources_straight_away() {
        let detail = Detail::seeded(&crate::state::Item::stub("tt1"));
        assert!(!detail.is_series());
        assert!(detail.loading);
    }

    #[test]
    fn rows_say_watched_progress_and_upcoming() {
        let mut detail = breaking_bad();
        detail.chosen_season = Some(2);
        detail.take_watch(TitleState {
            video_id: Some("tt:2:2".into()),
            time_offset: Some(600_000),
            duration: Some(2_400_000),
            watched: vec!["tt:2:1".into()],
            ..TitleState::default()
        });
        let rows = detail.episode_rows("2009-03-16");
        assert!(rows[0].watched && !rows[1].watched);
        assert!((rows[1].progress - 0.25).abs() < 1e-6);
        assert!(!rows[1].upcoming && rows[2].upcoming, "the 22nd is after the 16th");
        assert_eq!(rows[2].title.as_str(), "3. Episode tt:2:3");
        assert_eq!(rows[2].date.as_str(), "22 Mart 2009");
    }

    #[test]
    fn today_is_an_iso_date() {
        let today = today();
        assert_eq!(date_prefix(&today).as_deref(), Some(today.as_str()));
    }
}
