//! Subtitles for the interface's own player.
//!
//! Three kinds of subtitle meet here and leave as one list:
//!
//! * `embedded` -- the tracks the file carries, which mpv already lists;
//! * `stream_external` -- ones the addon attached to the stream itself;
//! * `addon_external` -- ones a subtitle addon found for the title;
//! * `provider_external` -- ones a provider the worker asks itself found
//!   (OpenSubtitles.com).
//!
//! A fetched one says which provider found it (`provider`, `provider_name`)
//! and the menu shows that name. The last three are the worker's to find,
//! fetch and time
//! (`media/subtitles/`): it owns the network, and this player's ffmpeg has
//! no TLS. This module decides what is on, tells mpv, and puts the worker's
//! timing into effect with `sub-delay`.
//!
//! The worker first asks whether an external subtitle belongs to this film's
//! timeline at all (`Eligibility`); only one that does is timed, and only by
//! a constant offset. A subtitle for another timebase or another cut is left
//! as it came, never converted.
//!
//! Three rules shape what it may do on its own:
//!
//! * the viewer's choice is final for the film: once somebody has picked a
//!   subtitle, or moved its delay, nothing automatic changes either until
//!   they ask for it again (`SubtitleAutoSync`);
//! * "Otomatik eşitleme" off means no external subtitle is analysed or timed
//!   unless the viewer asks for it in so many words;
//! * a choice is remembered by what it means -- on or off, which language,
//!   carried or fetched -- and never by a track number, which means nothing
//!   in the next episode.
//!
//! And one about what is never done on its own: a subtitle shown not to fit
//! the film (`known_bad`) is never put on automatically, not even as the
//! last resort when every candidate was refused -- the film's own text
//! track in the language is, or nothing. Chosen by hand it stays on,
//! untimed. With "AutoSync uyumsuz altyazıları göster" off (the default)
//! no subtitle AutoSync refused -- `INCONCLUSIVE` included -- is in the menu
//! or on the screen; on, they are both.
//!
//! With "Otomatik eşitleme" on, every fetched subtitle in the preferred
//! language is checked, not only the first that fits (`survey`).

use crate::media::MediaClient;
use crate::player::PlayerManager;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where the viewer's subtitle preference is kept, under the daemon's
/// `StateDirectory`.
pub const PREFERENCES_FILE: &str = "/var/lib/mediabox/subtitles.json";

/// Candidates of one language tried in turn when the automatic choice's
/// timing is rejected -- a subtitle for another release, or another film.
const ATTEMPTS: usize = 3;

/// How often a running sync is asked how it is going. Each question is a
/// line in the worker's journal; a sync takes tens of seconds.
#[cfg(not(test))]
const POLL: Duration = Duration::from_secs(5);
#[cfg(test)]
const POLL: Duration = Duration::from_millis(300);

/// A torrent's evidence arrives as the film plays; asked again this often.
const WAITING: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Embedded,
    External,
}

/// The worker's pre-filter answer for one external subtitle. The same
/// strings as `media/subtitles/eligibility.py`'s `Eligibility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Eligibility {
    /// This film's timeline up to one offset: AutoSync may time it.
    AcceptTimelineCompatible,
    /// Only part of the film (CD1, a trailer's lines).
    RejectPartial,
    /// Another timebase (25/24, 23.976/24 ...): refused, not converted.
    RejectTimebaseMismatch,
    /// Another cut or release: a step, lines outside the film, broken timings.
    RejectWrongRelease,
    /// The same file as a candidate already tried.
    RejectDuplicate,
    /// Nothing could be told reliably: treated as a refusal.
    Inconclusive,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Preferences {
    /// None until the viewer has chosen anything: the player's own default
    /// is left alone rather than guessed over.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Canonical language code (`tr`, `en`, `pt-br`).
    #[serde(default)]
    pub language: Option<String>,
    /// Whether the last choice was a carried or a fetched subtitle; it only
    /// breaks a tie between the two in the same language.
    #[serde(default)]
    pub origin: Option<Origin>,
    /// "Otomatik eşitleme". Absent in files written before it existed, and
    /// then on: timing external subtitles is what those films had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_sync_enabled: Option<bool>,
    /// "AutoSync uyumsuz altyazıları göster". Absent in older files, and then
    /// off: the menu carries only what may fit the film.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_incompatible_subtitles: Option<bool>,
}

impl Preferences {
    pub fn auto_sync(&self) -> bool {
        self.auto_sync_enabled.unwrap_or(true)
    }

    pub fn show_incompatible(&self) -> bool {
        self.show_incompatible_subtitles.unwrap_or(false)
    }
}

/// One external subtitle the worker offered for this film.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub source: String,
    pub language: Option<String>,
    pub label: Option<String>,
    pub hash_match: bool,
    /// Who found it (`opensubtitles_com`, `opensubtitles_v3`), and the name
    /// the menu shows for it. None for one the stream carried.
    pub provider: Option<String>,
    pub provider_name: Option<String>,
    /// What the provider's own metadata already said against it, before
    /// anything was downloaded: a refusal, or nothing.
    pub metadata: Option<(Eligibility, Option<String>)>,
    /// Why it cannot be had now ("quota-exhausted"), if it cannot.
    pub unavailable: Option<String>,
}

impl Candidate {
    fn from_worker(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let metadata = value.get("eligibility").and_then(|verdict| {
            let result = serde_json::from_value(verdict.get("result")?.clone()).ok()?;
            Some((result, verdict.get("reason").and_then(Value::as_str).map(str::to_owned)))
        });
        Some(Self {
            id: value.get("id")?.as_str()?.to_owned(),
            source: value
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("addon_external")
                .to_owned(),
            language: value.get("language").and_then(Value::as_str).map(str::to_owned),
            label: value.get("label").and_then(Value::as_str).map(str::to_owned),
            hash_match: value.get("hashMatch").and_then(Value::as_bool).unwrap_or(false),
            provider: text("provider"),
            provider_name: text("providerName"),
            metadata: metadata.filter(|(result, _)| *result != Eligibility::AcceptTimelineCompatible),
            unavailable: text("unavailable"),
        })
    }

    /// The timing its metadata already settles: refused, from the metadata.
    fn metadata_timing(&self) -> Option<Timing> {
        let (eligibility, reason) = self.metadata.clone()?;
        Some(Timing {
            state: "rejected".into(),
            scale: 1.0,
            reason,
            eligibility: Some(eligibility),
            reference: Some("metadata".into()),
            ..Timing::default()
        })
    }
}

/// Where the worker's analysis of one candidate has got to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Timing {
    /// analysing | waiting | applied | ready | rejected | failed
    pub state: String,
    pub model: Option<String>,
    pub offset: f64,
    pub scale: f64,
    pub confidence: f64,
    pub reason: Option<String>,
    /// The pre-filter's answer, once there is one.
    pub eligibility: Option<Eligibility>,
    /// What the timeline was compared with: embedded-picture, embedded-text,
    /// audio, none.
    pub reference: Option<String>,
    /// The canonical timebase equivalent the timeline follows ("1", "25/24").
    pub ratio: Option<String>,
    /// The pre-filter's own estimate of the offset (seconds).
    pub estimated_offset: Option<f64>,
    /// Answered from what was kept, not worked out now.
    pub cached: bool,
}

impl Timing {
    fn as_json(&self) -> Value {
        json!({
            "state": self.state,
            "model": self.model,
            "offset": self.offset,
            "scale": self.scale,
            "confidence": self.confidence,
            "reason": self.reason,
            "eligibility": self.eligibility,
            "reference": self.reference,
            "ratio": self.ratio,
            "estimated_offset": self.estimated_offset,
            "cached": self.cached,
        })
    }

    fn analysing() -> Self {
        Timing { state: "analysing".into(), scale: 1.0, ..Timing::default() }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Loaded {
    key: String,
    url: String,
    mpv: i64,
}

/// What is known about the subtitles of the film playing now.
#[derive(Debug, Default)]
struct Film {
    number: u64,
    session: String,
    candidates: Vec<Candidate>,
    loaded: HashMap<String, Loaded>,
    timing: HashMap<String, Timing>,
    /// The viewer has chosen during this film.
    manual: bool,
    /// The viewer has moved the delay of the subtitle that is on.
    manual_delay: bool,
    /// The viewer asked for the automatic timing in so many words: it is put
    /// on when it arrives, "Otomatik eşitleme" or not.
    requested: bool,
    /// The unified id of what is on, as this module last set it.
    selected: Option<String>,
    /// Automatic candidates already tried, in order.
    tried: Vec<String>,
}

/// What the automatic choice comes to.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// Leave the player's own default: the viewer has never said.
    Leave,
    Off,
    Embedded(i64),
    External(String),
}

pub struct Subtitles {
    media: Arc<MediaClient>,
    player: Arc<PlayerManager>,
    preferences_file: PathBuf,
    preferences: Mutex<Preferences>,
    film: Mutex<Option<Film>>,
}

impl Subtitles {
    pub fn new(media: Arc<MediaClient>, player: Arc<PlayerManager>, preferences_file: impl Into<PathBuf>) -> Arc<Self> {
        let preferences_file = preferences_file.into();
        let preferences = std::fs::read(&preferences_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Arc::new(Self {
            media,
            player,
            preferences_file,
            preferences: Mutex::new(preferences),
            film: Mutex::new(None),
        })
    }

    pub fn preferences(&self) -> Preferences {
        self.preferences.lock().unwrap().clone()
    }

    fn remember(&self, change: impl FnOnce(&mut Preferences)) {
        let snapshot = {
            let mut preferences = self.preferences.lock().unwrap();
            change(&mut preferences);
            preferences.clone()
        };
        let path = self.preferences_file.clone();
        // Written off the runtime: /var/lib is a disk and this is a remote
        // press away from being asked again.
        std::thread::spawn(move || {
            if let Ok(bytes) = serde_json::to_vec_pretty(&snapshot) {
                let temporary = path.with_extension("json.part");
                if std::fs::write(&temporary, bytes).is_ok() {
                    let _ = std::fs::rename(&temporary, &path);
                }
            }
        });
    }

    fn with_film<T>(&self, number: u64, f: impl FnOnce(&mut Film) -> T) -> Option<T> {
        let mut film = self.film.lock().unwrap();
        film.as_mut().filter(|film| film.number == number).map(f)
    }

    // ------------------------------------------------------------ a new film

    /// A film has started here: find its subtitles and put the preferred one
    /// on. Runs in the background; the film does not wait for any of it.
    pub fn begin(
        self: &Arc<Self>,
        number: u64,
        session: String,
        watch: Option<mediabox_core::WatchRef>,
        duration: Option<u64>,
    ) {
        *self.film.lock().unwrap() = Some(Film {
            number,
            session: session.clone(),
            ..Film::default()
        });
        let this = Arc::clone(self);
        tokio::spawn(async move { this.settle(number, session, watch, duration).await });
    }

    async fn settle(
        self: Arc<Self>,
        number: u64,
        session: String,
        watch: Option<mediabox_core::WatchRef>,
        duration: Option<u64>,
    ) {
        // The file's own tracks are known once mpv has opened it.
        let mut tracks = Vec::new();
        for _ in 0..150 {
            if self.player.film() != number {
                return;
            }
            tracks = self.player.tracks().await;
            if !tracks.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let preferences = self.preferences();
        if let Some(watch) = watch.as_ref() {
            let mut languages: Vec<String> = preferences.language.iter().cloned().collect();
            for fallback in ["tr", "en"] {
                if !languages.iter().any(|code| code == fallback) {
                    languages.push(fallback.into());
                }
            }
            let body = json!({
                "sessionId": session,
                "type": watch.kind,
                "id": watch.id,
                "videoId": watch.video_id,
                // Only for a provider's last resort: a film with no IMDb or
                // TMDb identity is searched by name.
                "title": watch.name,
                "duration": duration,
                "languages": languages,
            });
            match self.media.subtitles_prepare(body).await {
                Ok(answer) => {
                    let candidates: Vec<Candidate> = answer
                        .get("candidates")
                        .and_then(Value::as_array)
                        .map(|list| list.iter().filter_map(Candidate::from_worker).collect())
                        .unwrap_or_default();
                    eprintln!(
                        "mediaboxd-rs: film {number}: {} external subtitles, video {}",
                        candidates.len(),
                        answer.pointer("/video/identity").and_then(Value::as_str).unwrap_or("?")
                    );
                    self.with_film(number, |film| {
                        for candidate in &candidates {
                            if let Some(timing) = candidate.metadata_timing() {
                                film.timing.insert(candidate.id.clone(), timing);
                            }
                        }
                        film.candidates = candidates;
                    });
                }
                Err(error) => eprintln!("mediaboxd-rs: film {number}: no external subtitles: {error}"),
            }
        }

        let (manual, candidates) = match self.with_film(number, |film| (film.manual, usable(film))) {
            Some(found) => found,
            None => return,
        };
        if manual {
            return;
        }
        let choice = auto_choice(&preferences, &tracks, &candidates);
        // An external choice surveys the rest once it has settled.
        let external = matches!(choice, Choice::External(_));
        self.apply(number, choice).await;
        if !external {
            spawn_survey(Arc::clone(&self), number);
        }
    }

    /// Put an automatically chosen external subtitle on and, with
    /// "Otomatik eşitleme" on, have it checked and timed. One that cannot be
    /// had (the provider's quota, the network) is passed over for the next
    /// of its language, as a refused one is.
    async fn try_external(self: &Arc<Self>, number: u64, id: String) {
        let mut id = id;
        loop {
            self.with_film(number, |film| film.tried.push(id.clone()));
            if self.put_on(number, &id).await {
                if self.preferences().auto_sync() {
                    spawn_timing(Arc::clone(self), number, id, true);
                }
                return;
            }
            let next = self
                .with_film(number, |film| {
                    if film.manual {
                        return None;
                    }
                    next_candidate(&film.candidates, &film.tried, &film.timing, &id)
                })
                .flatten();
            match next {
                Some(next) => id = next,
                None => {
                    self.fall_back(number).await;
                    return;
                }
            }
        }
    }

    /// Load (if need be) and select one external candidate.
    async fn put_on(&self, number: u64, id: &str) -> bool {
        let Some(loaded) = self.load(number, id).await else {
            return false;
        };
        let applied = self.with_film(number, |film| film.timing.get(id).cloned()).flatten();
        self.player.select_subtitle(Some(loaded.mpv)).await;
        let delay = applied.filter(|timing| timing.state == "applied").map_or(0.0, |timing| timing.offset);
        self.player.subtitle_timing(delay, 1.0).await;
        self.with_film(number, |film| {
            film.selected = Some(id.to_owned());
            film.manual_delay = false;
        });
        true
    }

    async fn load(&self, number: u64, id: &str) -> Option<Loaded> {
        if let Some(loaded) = self.with_film(number, |film| film.loaded.get(id).cloned()).flatten() {
            return Some(loaded);
        }
        let (session, candidate) = self.with_film(number, |film| {
            (film.session.clone(), film.candidates.iter().find(|c| c.id == id).cloned())
        })?;
        let candidate = candidate?;
        let answer = match self.media.subtitles_load(&session, id).await {
            Ok(answer) => answer,
            Err(error) => {
                eprintln!("mediaboxd-rs: film {number}: subtitle {id} not loaded: {error}");
                // The provider's quota is used up: said beside it, not retried.
                if error.to_string().contains("SUBTITLE_QUOTA_EXHAUSTED") {
                    self.with_film(number, |film| {
                        if let Some(candidate) = film.candidates.iter_mut().find(|c| c.id == id) {
                            candidate.unavailable = Some("quota-exhausted".into());
                        }
                    });
                }
                return None;
            }
        };
        let url = answer.get("url").and_then(Value::as_str)?.to_owned();
        let key = answer.get("key").and_then(Value::as_str)?.to_owned();
        let title = title_for(&candidate);
        let mpv = self
            .player
            .sub_add(&url, &title, candidate.language.as_deref())
            .await?;
        let loaded = Loaded { key, url, mpv };
        self.with_film(number, |film| {
            film.loaded.insert(id.to_owned(), loaded.clone());
        })?;
        Some(loaded)
    }

    // ---------------------------------------------------------- the timing

    /// Ask the worker where this subtitle's lines go, and put the answer into
    /// effect if it is still the one on and its delay is still nobody's.
    async fn time(self: Arc<Self>, number: u64, id: String, automatic: bool) {
        let Some(key) = self
            .with_film(number, |film| film.loaded.get(&id).map(|l| l.key.clone()))
            .flatten()
        else {
            return;
        };
        let Some(session) = self.with_film(number, |film| film.session.clone()) else {
            return;
        };
        if automatic {
            // The same file as one already tried, under another addon's id:
            // it was answered once and the answer would be the same.
            let twin = self
                .with_film(number, |film| {
                    film.tried
                        .iter()
                        .filter(|tried| tried.as_str() != id)
                        .any(|tried| film.loaded.get(tried).is_some_and(|l| l.key == key))
                })
                .unwrap_or(false);
            if twin {
                self.set_timing(
                    number,
                    &id,
                    Timing {
                        state: "rejected".into(),
                        scale: 1.0,
                        reason: Some("duplicate".into()),
                        eligibility: Some(Eligibility::RejectDuplicate),
                        ..Timing::default()
                    },
                );
                self.move_on(number, &id, automatic).await;
                return;
            }
        }
        self.set_timing(number, &id, Timing::analysing());
        loop {
            if self.player.film() != number {
                return;
            }
            let duration = self
                .player
                .property("duration")
                .await
                .and_then(|value| value.as_f64())
                .filter(|seconds| *seconds > 0.0);
            let position = self.player.position().await;
            let body = json!({
                "sessionId": session,
                "key": key,
                "duration": duration,
                "position": position,
            });
            let mut job = match self.media.subtitles_sync(body).await {
                Ok(job) => job,
                Err(error) => {
                    eprintln!("mediaboxd-rs: film {number}: subtitle sync not started: {error}");
                    self.set_timing(number, &id, Timing { state: "failed".into(), ..Timing::analysing() });
                    return;
                }
            };
            let job_id = job.get("job").and_then(Value::as_str).unwrap_or_default().to_owned();
            while job.get("state").and_then(Value::as_str) == Some("running") {
                tokio::time::sleep(POLL).await;
                if self.player.film() != number {
                    return;
                }
                match self.media.subtitles_job(&job_id).await {
                    Ok(next) => job = next,
                    Err(_) => return,
                }
            }
            match job.get("state").and_then(Value::as_str) {
                Some("waiting") => {
                    self.set_timing(number, &id, timing_from(&job, "waiting"));
                    tokio::time::sleep(WAITING).await;
                    continue;
                }
                Some("done") => {
                    self.settled(number, &id, &job, automatic).await;
                    return;
                }
                _ => {
                    self.set_timing(number, &id, timing_from(&job, "failed"));
                    return;
                }
            }
        }
    }

    fn set_timing(&self, number: u64, id: &str, timing: Timing) {
        self.with_film(number, |film| {
            film.timing.insert(id.to_owned(), timing);
        });
    }

    async fn settled(self: &Arc<Self>, number: u64, id: &str, job: &Value, automatic: bool) {
        // Only ever a delay: the worker answers nothing else for a subtitle
        // it accepted, and nothing at all for one it did not.
        let delay = job
            .get("apply")
            .filter(|apply| apply.get("kind").and_then(Value::as_str) == Some("properties"))
            .and_then(|apply| apply.get("subDelay"))
            .and_then(Value::as_f64);
        let Some(delay) = delay else {
            self.set_timing(number, id, timing_from(job, "rejected"));
            self.move_on(number, id, automatic).await;
            self.withdraw(number, id).await;
            return;
        };
        let timing = Timing { offset: delay, scale: 1.0, ..timing_from(job, "applied") };
        let (on, by_hand, requested) = self
            .with_film(number, |film| (film.selected.as_deref() == Some(id), film.manual_delay, film.requested))
            .unwrap_or((false, true, false));
        // Turned off while it was being worked out: kept, and put on only
        // when asked for.
        let allowed = self.preferences().auto_sync() || requested;
        if on && !by_hand && allowed {
            self.player.subtitle_timing(timing.offset, 1.0).await;
        }
        let state = if on && !by_hand && allowed { "applied" } else { "ready" };
        self.set_timing(number, id, Timing { state: state.into(), ..timing });
        if automatic {
            spawn_survey(Arc::clone(self), number);
        }
    }

    /// With "AutoSync uyumsuz altyazıları göster" off, a subtitle AutoSync
    /// refused is not left on the screen, whoever put it on.
    async fn withdraw(&self, number: u64, id: &str) {
        if self.preferences().show_incompatible() {
            return;
        }
        let on = self
            .with_film(number, |film| film.selected.as_deref() == Some(id) && incompatible(film.timing.get(id)))
            .unwrap_or(false);
        if on {
            eprintln!("mediaboxd-rs: film {number}: subtitle {id} is incompatible, taken off");
            self.player.select_subtitle(None).await;
            self.with_film(number, |film| film.selected = Some("off".into()));
        }
    }

    /// With "Otomatik eşitleme" on, every fetched subtitle in the preferred
    /// language is checked, not only the first that fits: the menu then says
    /// of each whether it fits. One at a time, once the automatic choice has
    /// settled; none of them is put on.
    async fn survey(self: Arc<Self>, number: u64) {
        let mut skipped: Vec<String> = Vec::new();
        loop {
            let preferences = self.preferences();
            if !preferences.auto_sync() || preferences.enabled != Some(true) || self.player.film() != number {
                return;
            }
            let Some(language) = preferences.language else {
                return;
            };
            let next = self.with_film(number, |film| {
                let busy = film.timing.values().any(|t| t.state == "analysing" || t.state == "waiting");
                let next = film
                    .candidates
                    .iter()
                    .find(|c| {
                        c.language.as_deref() == Some(language.as_str())
                            && c.unavailable.is_none()
                            && !film.timing.contains_key(&c.id)
                            && !film.tried.contains(&c.id)
                            && !skipped.contains(&c.id)
                    })
                    .map(|c| c.id.clone());
                (busy, next)
            });
            match next {
                None | Some((_, None)) => return,
                Some((true, Some(_))) => tokio::time::sleep(POLL).await,
                Some((false, Some(id))) => {
                    eprintln!("mediaboxd-rs: film {number}: checking subtitle {id} as well");
                    if self.load(number, &id).await.is_some() {
                        Arc::clone(&self).time(number, id, false).await;
                    } else {
                        skipped.push(id);
                    }
                }
            }
        }
    }

    /// A timing nobody could find, or a subtitle that does not fit this film:
    /// an automatic choice moves on to the next of its language; one the
    /// viewer made stays exactly as it is, untimed.
    async fn move_on(self: &Arc<Self>, number: u64, id: &str, automatic: bool) {
        if !automatic {
            return;
        }
        let Some(Some(next)) = self.with_film(number, |film| {
            if film.manual || film.selected.as_deref() != Some(id) {
                return None;
            }
            Some(next_candidate(&film.candidates, &film.tried, &film.timing, id))
        }) else {
            return;
        };
        match next {
            Some(next) => {
                eprintln!("mediaboxd-rs: film {number}: subtitle {id} does not fit, trying {next}");
                self.try_external(number, next).await;
            }
            None => self.fall_back(number).await,
        }
    }

    /// Every automatic candidate was tried and none was accepted. A subtitle
    /// shown not to fit is never what is left on: the film's own text track
    /// in the language, else -- only with "AutoSync uyumsuz altyazıları
    /// göster" on -- one whose fit could not be told (untimed, as it came),
    /// else nothing.
    async fn fall_back(self: &Arc<Self>, number: u64) {
        // The automatic choice is over: the rest of the language is checked.
        spawn_survey(Arc::clone(self), number);
        let show = self.preferences().show_incompatible();
        let Some((manual, undecided)) = self.with_film(number, |film| {
            let undecided = film
                .tried
                .iter()
                .find(|tried| film.loaded.contains_key(tried.as_str()) && !known_bad(film.timing.get(tried.as_str())))
                .filter(|_| show)
                .cloned();
            (film.manual, undecided)
        }) else {
            return;
        };
        if manual {
            return;
        }
        let tracks = self.player.tracks().await;
        let preferences = self.preferences();
        let choice = match (fallback_choice(&preferences, &tracks), undecided) {
            (Choice::Embedded(track), _) => Choice::Embedded(track),
            (_, Some(undecided)) => {
                let on = self.with_film(number, |film| film.selected.as_deref() == Some(undecided.as_str()));
                if on != Some(true) {
                    self.put_on(number, &undecided).await;
                }
                return;
            }
            (choice, None) => choice,
        };
        eprintln!("mediaboxd-rs: film {number}: no fetched subtitle fits; {choice:?}");
        if !self.with_film(number, |film| film.manual).unwrap_or(true) {
            self.apply_own(number, choice).await;
        }
    }

    // ------------------------------------------------------- the viewer

    /// The viewer chose `id` from the menu.
    pub async fn choose(self: &Arc<Self>, id: &str) -> bool {
        let number = self.player.film();
        if self
            .with_film(number, |film| {
                film.manual = true;
                film.manual_delay = false;
            })
            .is_none()
        {
            // A film nobody began (the web interface's): still the viewer's.
            *self.film.lock().unwrap() = Some(Film { number, manual: true, ..Film::default() });
        }
        if id == "off" {
            let done = self.player.select_subtitle(None).await;
            self.with_film(number, |film| film.selected = Some("off".into()));
            return done;
        }
        if let Some(track) = id.strip_prefix("emb:").and_then(|n| n.parse::<i64>().ok()) {
            let done = self.player.select_subtitle(Some(track)).await;
            self.player.subtitle_timing(0.0, 1.0).await;
            self.with_film(number, |film| film.selected = Some(id.to_owned()));
            return done;
        }
        let known = self
            .with_film(number, |film| film.candidates.iter().any(|c| c.id == id))
            .unwrap_or(false);
        let hidden = !self.preferences().show_incompatible()
            && self.with_film(number, |film| incompatible(film.timing.get(id))).unwrap_or(false);
        if !known || hidden || !self.put_on(number, id).await {
            return false;
        }
        // The viewer's pick is final: it stays on whatever the answer is.
        // Checked and timed only with "Otomatik eşitleme" on -- even when
        // the film has an embedded track in the preferred language, which
        // then serves as the timing reference and nothing more.
        let untimed = self
            .with_film(number, |film| !film.timing.contains_key(id))
            .unwrap_or(false);
        if untimed && self.preferences().auto_sync() {
            spawn_timing(Arc::clone(self), number, id.to_owned(), false);
        }
        true
    }

    /// The viewer picked a track by the player's number (the older request).
    pub async fn chose_track(&self, track: i64) {
        let number = self.player.film();
        self.with_film(number, |film| {
            film.manual = true;
            film.manual_delay = false;
            let found = film.loaded.iter().find(|(_, l)| l.mpv == track).map(|(id, _)| id.clone());
            film.selected = Some(if track < 0 {
                "off".into()
            } else {
                found.unwrap_or_else(|| format!("emb:{track}"))
            });
        });
    }

    /// The settings panel's "Tercih edilen altyazı dili" was changed: kept
    /// for every film from now on, and put into effect on this one.
    ///
    /// The preference is only ever written here. A subtitle picked from the
    /// panel during a film is that film's business and leaves it alone.
    pub async fn set_preference(self: &Arc<Self>, language: Option<String>) -> bool {
        let language = canonical(language.as_deref());
        self.remember(|p| {
            p.enabled = Some(language.is_some());
            if language.is_some() {
                p.language = language.clone();
            }
        });
        let number = self.player.film();
        let Some(candidates) = self.with_film(number, |film| {
            // Asked for in so many words: the automatic choice is the
            // viewer's own again for this film.
            film.manual = false;
            film.tried.clear();
            usable(film)
        }) else {
            return false;
        };
        let tracks = self.player.tracks().await;
        let choice = auto_choice(&self.preferences(), &tracks, &candidates);
        let external = matches!(choice, Choice::External(_));
        self.apply(number, choice).await;
        if !external {
            spawn_survey(Arc::clone(self), number);
        }
        true
    }

    async fn apply(self: &Arc<Self>, number: u64, choice: Choice) {
        match choice {
            Choice::External(id) => self.try_external(number, id).await,
            other => self.apply_own(number, other).await,
        }
    }

    /// `apply` for everything but a fetched subtitle.
    async fn apply_own(&self, number: u64, choice: Choice) {
        match choice {
            Choice::Leave | Choice::External(_) => {}
            Choice::Off => {
                self.player.select_subtitle(None).await;
                self.with_film(number, |film| film.selected = Some("off".into()));
            }
            Choice::Embedded(id) => {
                self.player.select_subtitle(Some(id)).await;
                self.player.subtitle_timing(0.0, 1.0).await;
                self.with_film(number, |film| film.selected = Some(format!("emb:{id}")));
            }
        }
    }

    /// "Otomatik eşitleme" was turned on or off in the settings panel: kept
    /// for every film from now on. Turned on during a film, the external
    /// subtitle that is on is checked and timed now.
    pub async fn set_auto_sync(self: &Arc<Self>, enabled: bool) -> bool {
        self.remember(|p| p.auto_sync_enabled = Some(enabled));
        if !enabled {
            return true;
        }
        let number = self.player.film();
        let waiting = self
            .with_film(number, |film| {
                let selected = film.selected.clone().filter(|id| id.starts_with("ext:"))?;
                (!film.timing.contains_key(&selected)).then(|| (selected, !film.manual))
            })
            .flatten();
        let chained = matches!(waiting, Some((_, true)));
        if let Some((id, automatic)) = waiting {
            spawn_timing(Arc::clone(self), number, id, automatic);
        }
        // An automatic one surveys the rest once it has settled.
        if !chained {
            spawn_survey(Arc::clone(self), number);
        }
        true
    }

    /// "AutoSync uyumsuz altyazıları göster" was turned on or off: kept for
    /// every film, and the menu reads it on its next look. Turned off during
    /// a film, an incompatible subtitle that is on is taken off.
    pub async fn set_show_incompatible(&self, enabled: bool) {
        self.remember(|p| p.show_incompatible_subtitles = Some(enabled));
        if enabled {
            return;
        }
        let number = self.player.film();
        if let Some(Some(id)) = self.with_film(number, |film| film.selected.clone()) {
            self.withdraw(number, &id).await;
        }
    }

    /// The viewer moved the delay: automatic timing keeps off it from here.
    pub fn delay_moved(&self) {
        let number = self.player.film();
        self.with_film(number, |film| film.manual_delay = true);
    }

    /// Put the automatic timing back on the subtitle that is on: the
    /// viewer's own request, honoured whether "Otomatik eşitleme" is on or
    /// not. A subtitle the worker refused stays as it is.
    pub async fn auto_sync(self: &Arc<Self>) -> bool {
        let number = self.player.film();
        let Some((selected, timing)) = self
            .with_film(number, |film| {
                film.manual_delay = false;
                film.requested = true;
                let selected = film.selected.clone()?;
                Some((selected.clone(), film.timing.get(&selected).cloned()))
            })
            .flatten()
        else {
            return false;
        };
        if !selected.starts_with("ext:") {
            return false;
        }
        match timing {
            // Shown not to fit: stays on as the viewer's choice, untimed.
            Some(timing) if known_bad(Some(&timing)) => false,
            Some(timing) if timing.state == "applied" || timing.state == "ready" => {
                self.player.subtitle_timing(timing.offset, 1.0).await;
                self.set_timing(number, &selected, Timing { state: "applied".into(), ..timing });
                true
            }
            Some(timing) if timing.state == "analysing" || timing.state == "waiting" => true,
            _ => {
                spawn_timing(Arc::clone(self), number, selected, false);
                true
            }
        }
    }

    // ------------------------------------------------------------ reading

    /// The one list the menu shows: the file's tracks and the external
    /// candidates, loaded or not, each with where it came from.
    pub fn unified(&self, tracks: &[Value]) -> Value {
        let number = self.player.film();
        let film = self.film.lock().unwrap();
        let film = film.as_ref().filter(|film| film.number == number);
        let show = self.preferences().show_incompatible();
        Value::Array(unify(
            tracks,
            film.map(|film| (&film.candidates, &film.loaded, &film.timing)),
            show,
        ))
    }

    /// The line on screen, for the interface to draw.
    pub async fn text(&self) -> Value {
        // One question of mpv ten times a second: the styled line, and the
        // plain one made from it for whoever only wants the words.
        let ass = self.player.subtitle_ass().await.unwrap_or_default();
        json!({"film": self.player.film(), "text": plain_caption(&ass), "ass": ass})
    }
}

/// A line as mpv's `sub-text/ass` gives it, without its styling: override
/// blocks removed, `\N` a line break, `\h` a space.
pub fn plain_caption(ass: &str) -> String {
    let mut out = String::with_capacity(ass.len());
    let mut depth = 0usize;
    for ch in ass.replace("\\N", "\n").replace("\\n", " ").replace("\\h", " ").chars() {
        match ch {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// `Subtitles::time` on the runtime. Boxed, because a rejected automatic
/// choice moves on to the next candidate from inside it, which starts this
/// again: without the box the future would contain itself.
fn spawn_timing(this: Arc<Subtitles>, number: u64, id: String, automatic: bool) {
    let future: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move { this.time(number, id, automatic).await });
    tokio::spawn(future);
}

/// `Subtitles::survey` on the runtime, boxed for the same reason.
fn spawn_survey(this: Arc<Subtitles>, number: u64) {
    let future: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move { this.survey(number).await });
    tokio::spawn(future);
}

// ------------------------------------------------------------ pure parts

/// What to put on when a film starts, from what the viewer chose before.
pub fn auto_choice(preferences: &Preferences, tracks: &[Value], candidates: &[Candidate]) -> Choice {
    match preferences.enabled {
        None => return Choice::Leave,
        Some(false) => return Choice::Off,
        Some(true) => {}
    }
    let Some(language) = preferences.language.as_deref() else {
        return Choice::Leave;
    };
    // The file's own in that language: a full one before a forced-only one,
    // the default before the rest. A picture subtitle (PGS, VobSub) only
    // when nothing else in the language exists at all: this player's line is
    // drawn by the interface from text, and a picture subtitle has none.
    let of_language = |track: &&Value| {
        track.get("type").and_then(Value::as_str) == Some("sub")
            && !track.get("external").and_then(Value::as_bool).unwrap_or(false)
            && canonical(track.get("lang").and_then(Value::as_str)).as_deref() == Some(language)
    };
    let best = |text: bool| {
        tracks
            .iter()
            .filter(of_language)
            .filter(|track| is_picture(track) != text)
            .min_by_key(|track| {
                (
                    track.get("forced").and_then(Value::as_bool).unwrap_or(false),
                    !track.get("default").and_then(Value::as_bool).unwrap_or(false),
                    track.get("id").and_then(Value::as_i64).unwrap_or(i64::MAX),
                )
            })
            .and_then(|track| track.get("id").and_then(Value::as_i64))
    };
    let embedded = best(true);
    let picture = best(false);
    // Candidates arrive ranked; the first of the language is the best.
    let external = candidates
        .iter()
        .find(|candidate| candidate.language.as_deref() == Some(language))
        .map(|candidate| candidate.id.clone());
    // The original text track before a fetched one, always. (An older
    // build kept the origin of the last pick in the preferences file; it is
    // read and ignored.)
    let pick = embedded.map(Choice::Embedded).or(external.map(Choice::External));
    // Nothing in the language at all: subtitles stay off rather than on in a
    // language nobody asked for.
    pick.or(picture.map(Choice::Embedded)).unwrap_or(Choice::Off)
}

/// A subtitle that is pictures, not text.
fn is_picture(track: &Value) -> bool {
    matches!(
        track.get("codec").and_then(Value::as_str),
        Some("hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub")
    )
}

/// The next untried candidate in the same language as `current`, passing
/// over the ones already known not to fit or not to be had.
fn next_candidate(
    candidates: &[Candidate],
    tried: &[String],
    timing: &HashMap<String, Timing>,
    current: &str,
) -> Option<String> {
    let language = candidates.iter().find(|c| c.id == current)?.language.clone();
    candidates
        .iter()
        .filter(|c| c.language == language && !tried.contains(&c.id))
        .filter(|c| c.unavailable.is_none() && !known_bad(timing.get(&c.id)))
        .take(ATTEMPTS.saturating_sub(tried.len()))
        .map(|c| c.id.clone())
        .next()
}

/// Whether this subtitle was shown not to fit the film: part of it (a CD1, a
/// trailer's lines), another timebase, another cut, or a file already tried.
/// Never put on automatically; left out of the menu unless asked for.
/// INCONCLUSIVE is not among them: it says only that nothing could be told.
fn known_bad(timing: Option<&Timing>) -> bool {
    timing.is_some_and(|timing| {
        matches!(
            timing.eligibility,
            Some(
                Eligibility::RejectPartial
                    | Eligibility::RejectTimebaseMismatch
                    | Eligibility::RejectWrongRelease
                    | Eligibility::RejectDuplicate
            )
        ) || timing.reason.as_deref() == Some("does-not-cover-film")
    })
}

/// Whether AutoSync refused this subtitle, for whatever reason --
/// `INCONCLUSIVE` and the metadata's refusals included. What "AutoSync
/// uyumsuz altyazıları göster" off keeps out of the menu and off the screen.
fn incompatible(timing: Option<&Timing>) -> bool {
    timing.is_some_and(|timing| timing.state == "rejected")
}

/// The candidates the automatic choice may take, in their order.
fn usable(film: &Film) -> Vec<Candidate> {
    film.candidates
        .iter()
        .filter(|c| c.unavailable.is_none() && !known_bad(film.timing.get(&c.id)))
        .cloned()
        .collect()
}

/// What is left when no fetched subtitle fits: the film's own text track in
/// the language, or nothing. A picture track is not it -- this interface
/// draws the line from text, and a picture subtitle has none.
fn fallback_choice(preferences: &Preferences, tracks: &[Value]) -> Choice {
    match auto_choice(preferences, tracks, &[]) {
        Choice::Embedded(id)
            if !tracks.iter().any(|t| t.get("id").and_then(Value::as_i64) == Some(id) && t.get("type").and_then(Value::as_str) == Some("sub") && is_picture(t)) =>
        {
            Choice::Embedded(id)
        }
        _ => Choice::Off,
    }
}

fn timing_from(job: &Value, state: &str) -> Timing {
    let result = job.get("result").cloned().unwrap_or(Value::Null);
    let verdict = result.get("eligibility").cloned().unwrap_or(Value::Null);
    Timing {
        state: state.into(),
        model: result.get("model").and_then(Value::as_str).map(str::to_owned),
        offset: result.get("offset").and_then(Value::as_f64).unwrap_or(0.0),
        scale: result.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
        confidence: result.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
        reason: result.get("reason").and_then(Value::as_str).map(str::to_owned),
        eligibility: verdict.get("result").cloned().and_then(|v| serde_json::from_value(v).ok()),
        reference: verdict.get("reference").and_then(Value::as_str).map(str::to_owned),
        ratio: verdict.get("ratioLabel").and_then(Value::as_str).map(str::to_owned),
        estimated_offset: verdict.get("offset").and_then(Value::as_f64),
        cached: job.get("cache").and_then(Value::as_str) == Some("hit"),
    }
}

fn title_for(candidate: &Candidate) -> String {
    let origin = candidate.provider_name.clone().unwrap_or_else(|| {
        match candidate.source.as_str() {
            "stream_external" => "Akış",
            _ => "Harici",
        }
        .into()
    });
    match candidate.label.as_deref() {
        Some(label) => format!("{origin} · {label}"),
        None => origin,
    }
}

fn unify(
    tracks: &[Value],
    film: Option<(&Vec<Candidate>, &HashMap<String, Loaded>, &HashMap<String, Timing>)>,
    show_incompatible: bool,
) -> Vec<Value> {
    let mut out = Vec::new();
    let by_mpv: HashMap<i64, &String> = film
        .map(|(_, loaded, _)| loaded.iter().map(|(id, l)| (l.mpv, id)).collect())
        .unwrap_or_default();
    for track in tracks {
        if track.get("type").and_then(Value::as_str) != Some("sub") {
            continue;
        }
        let Some(mpv) = track.get("id").and_then(Value::as_i64) else { continue };
        // A track this module added is listed as its candidate, below.
        if by_mpv.contains_key(&mpv) {
            continue;
        }
        let external = track.get("external").and_then(Value::as_bool).unwrap_or(false);
        out.push(json!({
            "id": format!("emb:{mpv}"),
            "mpv_id": mpv,
            "source": if external { "local" } else { "embedded" },
            "language": canonical(track.get("lang").and_then(Value::as_str)),
            "title": track.get("title").cloned().unwrap_or(Value::Null),
            "codec": track.get("codec").cloned().unwrap_or(Value::Null),
            "selected": track.get("selected").and_then(Value::as_bool).unwrap_or(false),
            "forced": track.get("forced").and_then(Value::as_bool).unwrap_or(false),
            "default": track.get("default").and_then(Value::as_bool).unwrap_or(false),
            "sync": Value::Null,
        }));
    }
    let Some((candidates, loaded, timing)) = film else {
        return out;
    };
    for candidate in candidates {
        let track = loaded.get(&candidate.id).and_then(|l| {
            tracks.iter().find(|track| {
                track.get("type").and_then(Value::as_str) == Some("sub")
                    && track.get("id").and_then(Value::as_i64) == Some(l.mpv)
            })
        });
        // Refused by AutoSync: out of the menu unless asked for.
        if !show_incompatible && incompatible(timing.get(&candidate.id)) {
            continue;
        }
        out.push(json!({
            "id": candidate.id,
            "mpv_id": track.and_then(|t| t.get("id")).cloned().unwrap_or(Value::Null),
            "source": candidate.source,
            "language": candidate.language,
            "title": candidate.label,
            "codec": track.and_then(|t| t.get("codec")).cloned().unwrap_or(Value::Null),
            "selected": track.and_then(|t| t.get("selected")).and_then(Value::as_bool).unwrap_or(false),
            "forced": false,
            "default": false,
            "hash_match": candidate.hash_match,
            "provider": candidate.provider,
            "provider_name": candidate.provider_name,
            "unavailable": candidate.unavailable,
            "sync": timing.get(&candidate.id).map(Timing::as_json).unwrap_or(Value::Null),
        }));
    }
    out
}

/// Language codes as muxers and addons write them, made comparable. The
/// same table as the worker's `media/subtitles/languages.py`, for the codes
/// a file is likely to carry.
pub fn canonical(code: Option<&str>) -> Option<String> {
    let value = code?.trim().to_ascii_lowercase().replace('_', "-");
    if value.is_empty() || matches!(value.as_str(), "und" | "unk" | "mul" | "zxx" | "mis") {
        return None;
    }
    let mapped = match value.as_str() {
        "tur" => "tr",
        "eng" => "en",
        "ger" | "deu" => "de",
        "fre" | "fra" => "fr",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "pob" | "ptb" => "pt-br",
        "rus" => "ru",
        "ara" => "ar",
        "jpn" => "ja",
        "kor" => "ko",
        "chi" | "zho" => "zh",
        "dut" | "nld" => "nl",
        "pol" => "pl",
        "swe" => "sv",
        "nor" | "nob" => "no",
        "dan" => "da",
        "fin" => "fi",
        "gre" | "ell" => "el",
        "heb" => "he",
        "hun" => "hu",
        "cze" | "ces" => "cs",
        "rum" | "ron" => "ro",
        "bul" => "bg",
        "hrv" => "hr",
        "srp" => "sr",
        "ukr" => "uk",
        "per" | "fas" => "fa",
        other => other,
    };
    Some(mapped.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64, lang: &str, external: bool) -> Value {
        json!({"id": id, "type": "sub", "lang": lang, "external": external, "selected": false, "codec": "subrip"})
    }

    fn candidate(id: &str, language: &str) -> Candidate {
        Candidate {
            id: id.into(),
            source: "addon_external".into(),
            language: Some(language.into()),
            label: None,
            hash_match: false,
            provider: None,
            provider_name: None,
            metadata: None,
            unavailable: None,
        }
    }

    fn refused(eligibility: Eligibility) -> Timing {
        Timing { state: "rejected".into(), scale: 1.0, eligibility: Some(eligibility), ..Timing::default() }
    }

    fn on(language: &str, origin: Option<Origin>) -> Preferences {
        Preferences { enabled: Some(true), language: Some(language.into()), origin, ..Preferences::default() }
    }

    #[test]
    fn nothing_is_chosen_for_a_viewer_who_never_chose() {
        let tracks = [track(1, "tur", false)];
        assert_eq!(auto_choice(&Preferences::default(), &tracks, &[]), Choice::Leave);
    }

    #[test]
    fn off_stays_off() {
        let preferences = Preferences { enabled: Some(false), language: Some("tr".into()), ..Preferences::default() };
        assert_eq!(auto_choice(&preferences, &[track(1, "tur", false)], &[]), Choice::Off);
    }

    #[test]
    fn the_language_is_matched_not_the_track_number() {
        // Last episode's Turkish was track 3; in this one it is track 2.
        let tracks = [track(1, "eng", false), track(2, "tur", false)];
        assert_eq!(auto_choice(&on("tr", None), &tracks, &[]), Choice::Embedded(2));
    }

    #[test]
    fn the_original_before_a_fetched_one() {
        let tracks = [track(4, "tur", false)];
        let candidates = [candidate("ext:a", "tr")];
        assert_eq!(auto_choice(&on("tr", None), &tracks, &candidates), Choice::Embedded(4));
        assert_eq!(auto_choice(&on("tr", Some(Origin::Embedded)), &tracks, &candidates), Choice::Embedded(4));
        // A stale "external" from an older build does not put a fetched
        // subtitle over the film's own.
        assert_eq!(auto_choice(&on("tr", Some(Origin::External)), &tracks, &candidates), Choice::Embedded(4));
    }

    #[test]
    fn a_fetched_one_when_the_file_has_none_in_the_language() {
        let tracks = [track(1, "eng", false)];
        let candidates = [candidate("ext:e", "en"), candidate("ext:t", "tr"), candidate("ext:t2", "tr")];
        assert_eq!(auto_choice(&on("tr", None), &tracks, &candidates), Choice::External("ext:t".into()));
    }

    #[test]
    fn a_picture_subtitle_only_when_nothing_else_is_in_the_language() {
        let mut picture = track(1, "tur", false);
        picture["codec"] = json!("hdmv_pgs_subtitle");
        let candidates = [candidate("ext:t", "tr")];
        assert_eq!(auto_choice(&on("tr", None), &[picture.clone()], &candidates), Choice::External("ext:t".into()));
        assert_eq!(auto_choice(&on("tr", None), &[picture], &[]), Choice::Embedded(1));
        // Nothing in the language: off, not some other language.
        assert_eq!(auto_choice(&on("tr", None), &[track(2, "eng", false)], &[]), Choice::Off);
    }

    #[test]
    fn a_full_track_before_a_forced_one() {
        let mut forced = track(1, "tur", false);
        forced["forced"] = json!(true);
        let tracks = [forced, track(2, "tur", false)];
        assert_eq!(auto_choice(&on("tr", None), &tracks, &[]), Choice::Embedded(2));
    }

    #[test]
    fn the_next_candidate_is_of_the_same_language_and_attempts_are_bounded() {
        let candidates = [
            candidate("ext:1", "tr"),
            candidate("ext:e", "en"),
            candidate("ext:2", "tr"),
            candidate("ext:3", "tr"),
            candidate("ext:4", "tr"),
        ];
        let none = HashMap::new();
        assert_eq!(next_candidate(&candidates, &["ext:1".into()], &none, "ext:1"), Some("ext:2".into()));
        let tried: Vec<String> = ["ext:1", "ext:2", "ext:3"].iter().map(|s| s.to_string()).collect();
        assert_eq!(next_candidate(&candidates, &tried, &none, "ext:3"), None);
        // One its metadata already refused, or one that cannot be had, is
        // passed over without being downloaded.
        let mut timing = HashMap::new();
        timing.insert("ext:2".to_string(), refused(Eligibility::RejectTimebaseMismatch));
        let mut candidates = candidates.to_vec();
        candidates[3].unavailable = Some("quota-exhausted".into());
        assert_eq!(next_candidate(&candidates, &["ext:1".into()], &timing, "ext:1"), Some("ext:4".into()));
    }

    #[test]
    fn what_is_shown_not_to_fit_is_known_bad_and_inconclusive_is_not() {
        for bad in [
            Eligibility::RejectPartial,
            Eligibility::RejectTimebaseMismatch,
            Eligibility::RejectWrongRelease,
            Eligibility::RejectDuplicate,
        ] {
            assert!(known_bad(Some(&refused(bad))), "{bad:?}");
        }
        assert!(!known_bad(Some(&refused(Eligibility::Inconclusive))));
        assert!(!known_bad(Some(&refused(Eligibility::AcceptTimelineCompatible))));
        assert!(!known_bad(None));
    }

    #[test]
    fn with_nothing_fetched_fitting_the_films_own_text_or_nothing() {
        let turkish = on("tr", None);
        assert_eq!(fallback_choice(&turkish, &[track(1, "eng", false), track(4, "tur", false)]), Choice::Embedded(4));
        let mut picture = track(2, "tur", false);
        picture["codec"] = json!("hdmv_pgs_subtitle");
        assert_eq!(fallback_choice(&turkish, &[picture]), Choice::Off);
        assert_eq!(fallback_choice(&turkish, &[track(1, "eng", false)]), Choice::Off);
    }

    #[test]
    fn the_menu_leaves_out_what_autosync_refused_unless_asked() {
        let candidates: Vec<Candidate> = ["ext:p", "ext:t", "ext:w", "ext:d", "ext:i", "ext:ok", "ext:new"]
            .iter()
            .map(|id| candidate(id, "tr"))
            .collect();
        let mut timing = HashMap::new();
        timing.insert("ext:p".to_string(), refused(Eligibility::RejectPartial));
        timing.insert("ext:t".to_string(), refused(Eligibility::RejectTimebaseMismatch));
        timing.insert("ext:w".to_string(), refused(Eligibility::RejectWrongRelease));
        timing.insert("ext:d".to_string(), refused(Eligibility::RejectDuplicate));
        timing.insert("ext:i".to_string(), refused(Eligibility::Inconclusive));
        timing.insert(
            "ext:ok".to_string(),
            Timing { state: "applied".into(), eligibility: Some(Eligibility::AcceptTimelineCompatible), ..Timing::default() },
        );
        let loaded = HashMap::new();
        let ids = |list: Vec<Value>| list.iter().map(|t| t["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        // INCONCLUSIVE is refused too; one not checked yet is not refused.
        let hidden = unify(&[], Some((&candidates, &loaded, &timing)), false);
        assert_eq!(ids(hidden), ["ext:ok", "ext:new"]);
        let all = unify(&[], Some((&candidates, &loaded, &timing)), true);
        assert_eq!(all.len(), 7);
        assert_eq!(all[1]["sync"]["eligibility"], "REJECT_TIMEBASE_MISMATCH");
        assert_eq!(all[4]["sync"]["eligibility"], "INCONCLUSIVE");
    }

    #[test]
    fn a_fetched_one_carries_its_providers_name_and_metadata() {
        let found = Candidate::from_worker(&json!({
            "id": "ext:c", "source": "provider_external", "language": "tr", "label": "WEB-DL",
            "provider": "opensubtitles_com", "providerName": "OpenSubtitles.com",
            "eligibility": {"result": "REJECT_PARTIAL", "reason": "metadata-multi-part", "stage": "metadata"},
        }))
        .unwrap();
        assert_eq!(found.provider.as_deref(), Some("opensubtitles_com"));
        assert_eq!(title_for(&found), "OpenSubtitles.com · WEB-DL");
        let timing = found.metadata_timing().unwrap();
        assert_eq!((timing.state.as_str(), timing.eligibility), ("rejected", Some(Eligibility::RejectPartial)));
        assert_eq!(timing.reference.as_deref(), Some("metadata"));
        let v3 = Candidate::from_worker(&json!({"id": "ext:v", "source": "addon_external", "language": "tr",
            "provider": "opensubtitles_v3", "providerName": "OpenSubtitles v3"}))
        .unwrap();
        assert!(v3.metadata.is_none() && v3.metadata_timing().is_none());
        assert_eq!(title_for(&v3), "OpenSubtitles v3");
        let carried = Candidate::from_worker(&json!({"id": "ext:s", "source": "stream_external"})).unwrap();
        assert_eq!(title_for(&carried), "Akış");
    }

    #[test]
    fn embedded_and_external_share_one_list_and_keep_their_origin() {
        let mut loaded = HashMap::new();
        loaded.insert(
            "ext:a".to_string(),
            Loaded { key: "k".into(), url: "http://127.0.0.1:8790/media/subtitles/file/k.srt".into(), mpv: 3 },
        );
        let mut added = track(3, "tur", true);
        added["selected"] = json!(true);
        let tracks = [track(1, "eng", false), track(2, "tur", false), added];
        let candidates = vec![candidate("ext:a", "tr"), candidate("ext:b", "en")];
        let mut timing = HashMap::new();
        timing.insert(
            "ext:a".to_string(),
            Timing {
                state: "applied".into(),
                model: Some("offset".into()),
                offset: 1.82,
                scale: 1.0,
                confidence: 0.97,
                eligibility: Some(Eligibility::AcceptTimelineCompatible),
                ..Timing::default()
            },
        );
        let list = unify(&tracks, Some((&candidates, &loaded, &timing)), false);
        let ids: Vec<&str> = list.iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["emb:1", "emb:2", "ext:a", "ext:b"]);
        assert_eq!(list[1]["source"], "embedded");
        assert_eq!(list[1]["language"], "tr");
        assert_eq!(list[2]["source"], "addon_external");
        assert_eq!(list[2]["mpv_id"], 3);
        assert_eq!(list[2]["selected"], true);
        assert_eq!(list[2]["sync"]["offset"], 1.82);
        assert_eq!(list[2]["sync"]["eligibility"], "ACCEPT_TIMELINE_COMPATIBLE");
        // Not loaded yet: listed, with no track number.
        assert!(list[3]["mpv_id"].is_null());
    }

    #[test]
    fn a_preferences_file_from_before_auto_sync_keeps_it_on() {
        let old: Preferences = serde_json::from_str(r#"{"enabled": true, "language": "tr", "origin": "embedded"}"#).unwrap();
        assert!(old.auto_sync());
        assert_eq!(old.language.as_deref(), Some("tr"));
        let off = Preferences { auto_sync_enabled: Some(false), ..old.clone() };
        let written = serde_json::to_value(&off).unwrap();
        assert_eq!(written["auto_sync_enabled"], false);
        assert!(!serde_json::from_value::<Preferences>(written).unwrap().auto_sync());
        // A file that never said is written as it was.
        assert!(serde_json::to_value(&old).unwrap().get("auto_sync_enabled").is_none());
        // Nor did it say anything of the incompatible ones: left out.
        assert!(!old.show_incompatible());
        assert!(serde_json::to_value(&old).unwrap().get("show_incompatible_subtitles").is_none());
        let shown = Preferences { show_incompatible_subtitles: Some(true), ..old.clone() };
        let written = serde_json::to_value(&shown).unwrap();
        assert_eq!(written["show_incompatible_subtitles"], true);
        let read: Preferences = serde_json::from_value(written).unwrap();
        assert!(read.show_incompatible() && read.auto_sync());
    }

    #[test]
    fn the_pre_filters_answers_are_the_workers_strings() {
        for (name, value) in [
            ("ACCEPT_TIMELINE_COMPATIBLE", Eligibility::AcceptTimelineCompatible),
            ("REJECT_PARTIAL", Eligibility::RejectPartial),
            ("REJECT_TIMEBASE_MISMATCH", Eligibility::RejectTimebaseMismatch),
            ("REJECT_WRONG_RELEASE", Eligibility::RejectWrongRelease),
            ("REJECT_DUPLICATE", Eligibility::RejectDuplicate),
            ("INCONCLUSIVE", Eligibility::Inconclusive),
        ] {
            assert_eq!(serde_json::to_value(value).unwrap(), name);
            assert_eq!(serde_json::from_value::<Eligibility>(json!(name)).unwrap(), value);
        }
        let job = json!({"cache": "hit", "result": {"model": "rejected", "reason": "canonical-timebase",
            "eligibility": {"result": "REJECT_TIMEBASE_MISMATCH", "reference": "embedded-picture", "ratioLabel": "25/24", "offset": 0.4}}});
        let timing = timing_from(&job, "rejected");
        assert_eq!(timing.eligibility, Some(Eligibility::RejectTimebaseMismatch));
        assert_eq!(timing.ratio.as_deref(), Some("25/24"));
        assert_eq!(timing.reference.as_deref(), Some("embedded-picture"));
        assert!(timing.cached);
    }

    #[test]
    fn a_styled_line_read_plain() {
        // What mpv v0.41 answered for an SRT with <i> and <b> in it.
        let ass = "{\\i1}Dış ses: Merhaba.{\\i0}\\Nİkinci satır {\\b1}kalın{\\b0} ve {\\i1}italik{\\i0}.";
        assert_eq!(plain_caption(ass), "Dış ses: Merhaba.\nİkinci satır kalın ve italik.");
        assert_eq!(plain_caption(""), "");
        assert_eq!(plain_caption("a\\hb\\nc"), "a b c");
    }

    #[test]
    fn language_codes_compare() {
        assert_eq!(canonical(Some("tur")).as_deref(), Some("tr"));
        assert_eq!(canonical(Some("TR")).as_deref(), Some("tr"));
        assert_eq!(canonical(Some("pob")).as_deref(), Some("pt-br"));
        assert_eq!(canonical(Some("und")), None);
        assert_eq!(canonical(None), None);
    }
}

/// The manager against a stand-in mpv (a unix socket speaking its JSON IPC)
/// and a stand-in worker (loopback HTTP), end to end through the real
/// `PlayerManager` and `MediaClient`.
#[cfg(test)]
mod flows {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, UnixListener};

    #[derive(Default)]
    struct Mpv {
        tracks: Vec<Value>,
        sid: Option<i64>,
        delay: f64,
        speed: f64,
        log: Vec<Value>,
    }

    impl Mpv {
        fn answer(&mut self, command: &[Value]) -> Value {
            self.log.push(Value::Array(command.to_vec()));
            let name = command.first().and_then(Value::as_str).unwrap_or_default();
            let ok = |data: Value| json!({"error": "success", "data": data});
            match name {
                "get_property" => match command[1].as_str().unwrap() {
                    "track-list" => {
                        let sid = self.sid;
                        let tracks: Vec<Value> = self
                            .tracks
                            .iter()
                            .map(|t| {
                                let mut t = t.clone();
                                if t["type"] == "sub" {
                                    t["selected"] = json!(t["id"].as_i64() == sid);
                                }
                                t
                            })
                            .collect();
                        ok(json!(tracks))
                    }
                    "duration" => ok(json!(5400.0)),
                    "time-pos" => ok(json!(120.0)),
                    "sub-text" => ok(json!(if self.sid.is_some() { "Merhaba" } else { "" })),
                    "sub-text/ass" => ok(json!(if self.sid.is_some() { "{\\i1}Merhaba{\\i0}\\Nnasılsın" } else { "" })),
                    _ => ok(Value::Null),
                },
                "set_property" => {
                    match command[1].as_str().unwrap() {
                        "sid" => self.sid = command[2].as_i64(),
                        "sub-delay" => self.delay = command[2].as_f64().unwrap(),
                        "sub-speed" => self.speed = command[2].as_f64().unwrap(),
                        _ => {}
                    }
                    ok(Value::Null)
                }
                "sub-add" => {
                    let id = self.tracks.iter().filter_map(|t| t["id"].as_i64()).max().unwrap_or(0) + 1;
                    self.tracks.push(json!({
                        "id": id, "type": "sub", "external": true,
                        "external-filename": command[1], "title": command[3],
                        "lang": command.get(4).cloned().unwrap_or(Value::Null), "codec": "subrip",
                    }));
                    ok(Value::Null)
                }
                "sub-remove" => {
                    let id = command[1].as_i64();
                    self.tracks.retain(|t| !(t["type"] == "sub" && t["id"].as_i64() == id));
                    if self.sid == id {
                        self.sid = None;
                    }
                    ok(Value::Null)
                }
                _ => ok(Value::Null),
            }
        }
    }

    async fn serve_mpv(path: PathBuf, mpv: Arc<StdMutex<Mpv>>) {
        let listener = UnixListener::bind(&path).unwrap();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mpv = Arc::clone(&mpv);
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut lines = BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let command = request["command"].as_array().cloned().unwrap_or_default();
                    let answer = mpv.lock().unwrap().answer(&command);
                    let _ = writer.write_all(format!("{answer}\n").as_bytes()).await;
                }
            });
        }
    }

    /// How the stand-in worker answers a sync.
    #[derive(Clone, Default)]
    struct Worker {
        apply: Value,
        /// Polls answered "running" before "done".
        polls_before_done: usize,
        /// The candidate whose subtitle ends long before the film.
        misfit: Option<&'static str>,
        /// The pre-filter's answer for a refused one.
        refusal: Option<&'static str>,
        /// Candidates that are the same file as the first Turkish one.
        twins: bool,
        /// The first Turkish one comes with its provider's metadata already
        /// refusing it (another timebase).
        metadata_refused: bool,
        /// A candidate whose provider's download quota is used up.
        quota: Option<&'static str>,
    }

    async fn serve_worker(listener: TcpListener, worker: Worker, seen: Arc<StdMutex<Vec<String>>>) {
        let polls = Arc::new(StdMutex::new(0usize));
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let worker = worker.clone();
            let seen = Arc::clone(&seen);
            let polls = Arc::clone(&polls);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 65536];
                let mut used = 0;
                let (head, body) = loop {
                    let n = stream.read(&mut buffer[used..]).await.unwrap();
                    used += n;
                    let text = String::from_utf8_lossy(&buffer[..used]).to_string();
                    if let Some(at) = text.find("\r\n\r\n") {
                        let length = text[..at]
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                            .unwrap_or(0);
                        if used >= at + 4 + length || n == 0 {
                            break (text[..at].to_string(), text[at + 4..].to_string());
                        }
                    }
                };
                let line = head.lines().next().unwrap().to_string();
                seen.lock().unwrap().push(format!("{line} {body}"));
                let mut status = "200 OK";
                let answer = if line.starts_with("POST /media/subtitles/prepare") {
                    let refused = worker.metadata_refused.then(|| {
                        json!({"result": "REJECT_TIMEBASE_MISMATCH", "reason": "metadata-fps", "stage": "metadata"})
                    });
                    json!({"video": {"identity": "osh:1:2"}, "candidates": [
                        {"id": "ext:tr1", "source": "provider_external", "language": "tr", "label": "WEB-DL",
                         "provider": "opensubtitles_com", "providerName": "OpenSubtitles.com", "eligibility": refused},
                        {"id": "ext:tr2", "source": "addon_external", "language": "tr",
                         "provider": "opensubtitles_v3", "providerName": "OpenSubtitles v3"},
                        {"id": "ext:en1", "source": "stream_external", "language": "en"},
                    ]})
                } else if line.starts_with("POST /media/subtitles/load") {
                    let candidate: Value = serde_json::from_str(&body).unwrap();
                    let id = candidate["candidate"].as_str().unwrap().trim_start_matches("ext:");
                    if worker.quota == Some(id) {
                        status = "429 Too Many Requests";
                    }
                    let same = if worker.twins && id == "tr2" { "tr1" } else { id };
                    if status == "200 OK" {
                        json!({"key": format!("{same:0>32}"), "url": format!("http://127.0.0.1:8790/media/subtitles/file/{id}.srt"), "format": "srt"})
                    } else {
                        json!({"error": {"code": "SUBTITLE_QUOTA_EXHAUSTED", "message": "quota"}})
                    }
                } else if line.starts_with("POST /media/subtitles/sync") {
                    let request: Value = serde_json::from_str(&body).unwrap();
                    json!({"job": request["key"], "state": "running"})
                } else if line.starts_with("GET /media/subtitles/sync/") {
                    let path = line.split_whitespace().nth(1).unwrap().split('?').next().unwrap();
                    let job = path.rsplit('/').next().unwrap().to_owned();
                    let (reason, eligibility) = match worker.misfit {
                        Some(misfit) if job.ends_with(misfit) => ("does-not-cover-film", "REJECT_PARTIAL"),
                        _ => ("canonical-timebase", worker.refusal.unwrap_or("REJECT_TIMEBASE_MISMATCH")),
                    };
                    let mut count = polls.lock().unwrap();
                    *count += 1;
                    if *count <= worker.polls_before_done {
                        json!({"job": job, "state": "running"})
                    } else {
                        let decision = if worker.apply.is_null() { "reject" } else { "apply" };
                        let verdict = if worker.apply.is_null() { eligibility } else { "ACCEPT_TIMELINE_COMPATIBLE" };
                        json!({"job": job, "state": "done", "apply": worker.apply,
                               "result": {"model": if worker.apply["kind"] == "file" {"piecewise"} else {"offset"},
                                          "decision": decision, "offset": 1.82, "scale": 1.0, "confidence": 0.97,
                                          "reason": if worker.apply.is_null() { Some(reason) } else { None },
                                          "eligibility": {"result": verdict, "reference": "embedded-picture", "ratioLabel": "1"}}})
                    }
                } else {
                    json!({})
                };
                let body = answer.to_string();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    }

    struct Rig {
        subtitles: Arc<Subtitles>,
        mpv: Arc<StdMutex<Mpv>>,
        seen: Arc<StdMutex<Vec<String>>>,
        _dir: tempfile::TempDir,
    }

    async fn rig(worker: Worker, preferences: Preferences) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("player.sock");
        let mpv = Arc::new(StdMutex::new(Mpv {
            tracks: vec![
                json!({"id": 1, "type": "video"}),
                json!({"id": 1, "type": "sub", "lang": "eng", "external": false, "codec": "subrip"}),
                json!({"id": 2, "type": "sub", "lang": "ger", "external": false, "codec": "hdmv_pgs_subtitle"}),
            ],
            speed: 1.0,
            ..Mpv::default()
        }));
        tokio::spawn(serve_mpv(socket.clone(), Arc::clone(&mpv)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        tokio::spawn(serve_worker(listener, worker, Arc::clone(&seen)));
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let media = Arc::new(MediaClient::new(&format!("http://{address}"), Duration::from_secs(5)).unwrap());
        let player = Arc::new(PlayerManager::new("/bin/true", &socket));
        let file = dir.path().join("subtitles.json");
        std::fs::write(&file, serde_json::to_vec(&preferences).unwrap()).unwrap();
        let subtitles = Subtitles::new(media, player, file);
        Rig { subtitles, mpv, seen, _dir: dir }
    }

    fn watch() -> Option<mediabox_core::WatchRef> {
        Some(mediabox_core::WatchRef {
            kind: "series".into(),
            id: "tt1".into(),
            video_id: Some("tt1:1:2".into()),
            name: "Dizi".into(),
            poster: None,
        })
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..100 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never happened: {what}");
    }

    fn turkish() -> Preferences {
        Preferences { enabled: Some(true), language: Some("tr".into()), ..Preferences::default() }
    }

    fn syncs(seen: &StdMutex<Vec<String>>) -> usize {
        seen.lock().unwrap().iter().filter(|l| l.starts_with("POST /media/subtitles/sync")).count()
    }

    fn properties() -> Value {
        json!({"kind": "properties", "subDelay": 1.82, "subSpeed": 1.0})
    }

    #[tokio::test]
    async fn an_addon_subtitle_reaches_the_player_and_is_timed() {
        let rig = rig(Worker { apply: properties(), ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the Turkish subtitle is on", || {
            let mpv = mpv.lock().unwrap();
            mpv.sid == Some(3) && mpv.tracks.iter().any(|t| t["external-filename"] == "http://127.0.0.1:8790/media/subtitles/file/tr1.srt")
        })
        .await;
        let mpv2 = Arc::clone(&rig.mpv);
        until("the timing is in effect", || (mpv2.lock().unwrap().delay - 1.82).abs() < 1e-9).await;
        // The addons were asked about the episode, through the worker.
        let seen = rig.seen.lock().unwrap().clone();
        assert!(seen.iter().any(|l| l.starts_with("POST /media/subtitles/prepare") && l.contains("tt1:1:2")), "{seen:?}");
        // Carried and fetched share one list.
        let tracks = rig.subtitles.player.tracks().await;
        let list = rig.subtitles.unified(&tracks);
        let ids: Vec<&str> = list.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["emb:1", "emb:2", "ext:tr1", "ext:tr2", "ext:en1"]);
        let on = &list[2];
        assert_eq!(on["selected"], true);
        assert_eq!(on["source"], "provider_external");
        assert_eq!(on["provider_name"], "OpenSubtitles.com");
        assert_eq!(list[3]["provider_name"], "OpenSubtitles v3");
        assert!(list[4]["provider_name"].is_null(), "the stream's own is nobody's");
        assert_eq!(on["sync"]["state"], "applied");
        assert_eq!(list[0]["source"], "embedded");
        assert_eq!(list[4]["source"], "stream_external");
    }

    #[tokio::test]
    async fn the_viewers_choice_is_not_overwritten_by_a_late_answer() {
        // The worker takes a few polls to answer; meanwhile the viewer picks
        // the file's English track.
        let rig = rig(Worker { apply: properties(), polls_before_done: 2, ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the automatic choice is on", || mpv.lock().unwrap().sid == Some(3)).await;
        assert!(rig.subtitles.choose("emb:1").await);
        tokio::time::sleep(Duration::from_millis(3500)).await;
        let mpv = rig.mpv.lock().unwrap();
        assert_eq!(mpv.sid, Some(1), "the viewer's track stays on");
        assert_eq!(mpv.delay, 0.0, "the other subtitle's timing is not put on it");
        drop(mpv);
        // The film's choice is the film's: the preference is the setting's.
        assert_eq!(rig.subtitles.preferences().language.as_deref(), Some("tr"));
    }

    #[tokio::test]
    async fn a_delay_set_by_hand_is_kept_until_auto_sync_is_asked_for() {
        let rig = rig(Worker { apply: properties(), polls_before_done: 1, ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the automatic choice is on", || mpv.lock().unwrap().sid == Some(3)).await;
        rig.subtitles.delay_moved();
        rig.mpv.lock().unwrap().delay = 0.5;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(rig.mpv.lock().unwrap().delay, 0.5);
        let tracks = rig.subtitles.player.tracks().await;
        assert_eq!(rig.subtitles.unified(&tracks)[2]["sync"]["state"], "ready");
        assert!(rig.subtitles.auto_sync().await);
        assert!((rig.mpv.lock().unwrap().delay - 1.82).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_corrected_file_is_never_swapped_in() {
        // An older worker's answer for a different cut: a remapped file. The
        // rule now is to refuse such a subtitle, not to rescue it.
        let corrected = json!({"kind": "file", "key": "c".repeat(32), "url": "http://127.0.0.1:8790/media/subtitles/file/corrected.srt"});
        let rig = rig(Worker { apply: corrected, ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let seen = Arc::clone(&rig.seen);
        until("the first answer is in", || syncs(&seen) >= 1).await;
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let mpv = rig.mpv.lock().unwrap();
        assert!(!mpv.tracks.iter().any(|t| t["external-filename"] == "http://127.0.0.1:8790/media/subtitles/file/corrected.srt"));
        assert!(!mpv.log.iter().any(|c| c[0] == "sub-remove"));
        assert_eq!((mpv.delay, mpv.speed), (0.0, 1.0));
    }

    #[tokio::test]
    async fn a_rejected_automatic_choice_moves_to_the_next_of_its_language() {
        let rig = rig(Worker { apply: Value::Null, ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let seen = Arc::clone(&rig.seen);
        until("the second Turkish candidate is loaded", || {
            seen.lock().unwrap().iter().any(|l| l.contains("\"candidate\":\"ext:tr2\""))
        })
        .await;
        // Never the English one: the language is the viewer's.
        assert!(!rig.seen.lock().unwrap().iter().any(|l| l.contains("ext:en1") && l.starts_with("POST /media/subtitles/load")));
    }

    /// The unified list, once `done` says it has settled.
    async fn settled(subtitles: &Arc<Subtitles>, done: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..100 {
            let tracks = subtitles.player.tracks().await;
            let list = subtitles.unified(&tracks);
            if done(&list) {
                return list;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never settled");
    }

    fn entry<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
        list.as_array().unwrap().iter().find(|t| t["id"] == id)
    }

    #[tokio::test]
    async fn every_automatic_choice_refused_leaves_none_of_them_on() {
        // The first Turkish one is a trailer's, the second another
        // timebase: both shown not to fit. Neither is left on as a last
        // resort, and the film has no Turkish text of its own: off.
        let rig = rig(Worker { apply: Value::Null, misfit: Some("tr1"), ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let seen = Arc::clone(&rig.seen);
        until("both were answered", || syncs(&seen) >= 2).await;
        let mpv = Arc::clone(&rig.mpv);
        until("subtitles are off", || mpv.lock().unwrap().sid.is_none()).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(rig.mpv.lock().unwrap().sid, None);
        // And the menu, with the setting off, does not offer them.
        let tracks = rig.subtitles.player.tracks().await;
        let list = rig.subtitles.unified(&tracks);
        assert!(entry(&list, "ext:tr1").is_none() && entry(&list, "ext:tr2").is_none(), "{list:?}");
        assert!(entry(&list, "ext:en1").is_some());
        // Asked for, they are there, each with why.
        rig.subtitles.set_show_incompatible(true).await;
        let list = rig.subtitles.unified(&rig.subtitles.player.tracks().await);
        assert_eq!(entry(&list, "ext:tr1").unwrap()["sync"]["eligibility"], "REJECT_PARTIAL");
        assert_eq!(entry(&list, "ext:tr2").unwrap()["sync"]["eligibility"], "REJECT_TIMEBASE_MISMATCH");
    }

    fn shown() -> Preferences {
        Preferences { show_incompatible_subtitles: Some(true), ..turkish() }
    }

    fn language_of(mpv: &Mpv, sid: Option<i64>) -> Option<String> {
        mpv.tracks
            .iter()
            .find(|t| t["type"] == "sub" && t["id"].as_i64() == sid)
            .and_then(|t| t["lang"].as_str().map(str::to_owned))
    }

    #[tokio::test]
    async fn an_inconclusive_one_is_never_shown_while_incompatible_ones_are_hidden() {
        // The default: "AutoSync uyumsuz altyazıları göster" off. Both
        // Turkish ones are INCONCLUSIVE; neither is left on, neither is in
        // the menu, and the film has no Turkish text of its own: off.
        let rig = rig(Worker { apply: Value::Null, refusal: Some("INCONCLUSIVE"), ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let seen = Arc::clone(&rig.seen);
        until("both were answered", || syncs(&seen) >= 2).await;
        let mpv = Arc::clone(&rig.mpv);
        until("subtitles are off", || mpv.lock().unwrap().sid.is_none()).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(rig.mpv.lock().unwrap().sid, None);
        let list = rig.subtitles.unified(&rig.subtitles.player.tracks().await);
        assert!(entry(&list, "ext:tr1").is_none() && entry(&list, "ext:tr2").is_none(), "{list:?}");
        // Picked anyway (an old menu), it is not put on.
        assert!(!rig.subtitles.choose("ext:tr1").await);
        assert_eq!(rig.mpv.lock().unwrap().sid, None);
    }

    #[tokio::test]
    async fn when_nothing_could_be_told_the_first_stays_on_untimed() {
        // With incompatible ones shown, INCONCLUSIVE is not "does not fit":
        // the best-ranked stays on, as it came, and nothing is timed.
        let rig = rig(Worker { apply: Value::Null, refusal: Some("INCONCLUSIVE"), ..Worker::default() }, shown()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let subtitles = Arc::clone(&rig.subtitles);
        let list = settled(&subtitles, |list| {
            entry(list, "ext:tr2").is_some_and(|e| e["sync"]["state"] == "rejected")
        })
        .await;
        assert_eq!(entry(&list, "ext:tr1").unwrap()["sync"]["eligibility"], "INCONCLUSIVE");
        let mpv = Arc::clone(&rig.mpv);
        until("the first is back on", || mpv.lock().unwrap().sid == Some(3)).await;
        assert_eq!(rig.mpv.lock().unwrap().delay, 0.0);
        // Turned off now, it comes off the screen and out of the menu.
        rig.subtitles.set_show_incompatible(false).await;
        assert_eq!(rig.mpv.lock().unwrap().sid, None);
        let list = rig.subtitles.unified(&rig.subtitles.player.tracks().await);
        assert!(entry(&list, "ext:tr1").is_none() && entry(&list, "ext:tr2").is_none(), "{list:?}");
    }

    #[tokio::test]
    async fn a_subtitle_picked_by_hand_and_refused_is_taken_off() {
        let rig = rig(Worker { apply: Value::Null, refusal: Some("INCONCLUSIVE"), ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let seen = Arc::clone(&rig.seen);
        until("the automatic choice is answered", || syncs(&seen) >= 2).await;
        assert!(rig.subtitles.choose("ext:en1").await);
        {
            let mpv = rig.mpv.lock().unwrap();
            assert_eq!(language_of(&mpv, mpv.sid).as_deref(), Some("en"));
        }
        let mpv = Arc::clone(&rig.mpv);
        until("the refused pick is off", || mpv.lock().unwrap().sid.is_none()).await;
        let list = rig.subtitles.unified(&rig.subtitles.player.tracks().await);
        assert!(entry(&list, "ext:en1").is_none(), "{list:?}");
    }

    #[tokio::test]
    async fn every_subtitle_of_the_language_is_checked_not_only_the_first_that_fits() {
        let rig = rig(Worker { apply: properties(), ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let subtitles = Arc::clone(&rig.subtitles);
        let list = settled(&subtitles, |list| {
            entry(list, "ext:tr2").is_some_and(|e| e["sync"]["state"] == "ready")
        })
        .await;
        assert_eq!(entry(&list, "ext:tr1").unwrap()["sync"]["state"], "applied");
        assert_eq!(entry(&list, "ext:tr2").unwrap()["sync"]["eligibility"], "ACCEPT_TIMELINE_COMPATIBLE");
        // The first stays the one on; the other language is not touched.
        assert_eq!(rig.mpv.lock().unwrap().sid, Some(3));
        assert_eq!(syncs(&rig.seen), 2);
        assert!(!rig.seen.lock().unwrap().iter().any(|l| l.contains("\"candidate\":\"ext:en1\"")));
    }

    #[tokio::test]
    async fn one_its_metadata_refused_is_never_downloaded_automatically() {
        let rig = rig(Worker { apply: properties(), metadata_refused: true, ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the second Turkish one is timed", || (mpv.lock().unwrap().delay - 1.82).abs() < 1e-9).await;
        let seen = rig.seen.lock().unwrap().clone();
        assert!(!seen.iter().any(|l| l.contains("\"candidate\":\"ext:tr1\"")), "{seen:?}");
        let list = rig.subtitles.unified(&rig.subtitles.player.tracks().await);
        // Out of the menu, and the one on is named by its provider.
        assert!(entry(&list, "ext:tr1").is_none());
        let on = entry(&list, "ext:tr2").unwrap();
        assert_eq!((on["provider"].as_str(), on["provider_name"].as_str()), (Some("opensubtitles_v3"), Some("OpenSubtitles v3")));
        assert_eq!(on["selected"], true);
    }

    #[tokio::test]
    async fn chosen_by_hand_one_its_metadata_refused_stays_on_untimed() {
        // With incompatible ones shown; hidden, it cannot be put on at all.
        let rig = rig(Worker { apply: properties(), metadata_refused: true, ..Worker::default() }, shown()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the automatic choice is timed", || (mpv.lock().unwrap().delay - 1.82).abs() < 1e-9).await;
        let before = syncs(&rig.seen);
        assert!(rig.subtitles.choose("ext:tr1").await);
        // Downloaded now, because it was asked for; put on; never timed.
        assert!(rig.seen.lock().unwrap().iter().any(|l| l.contains("\"candidate\":\"ext:tr1\"")));
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(!rig.subtitles.auto_sync().await);
        assert_eq!(syncs(&rig.seen), before);
        assert_eq!(rig.mpv.lock().unwrap().delay, 0.0);
        let list = rig.subtitles.unified(&rig.subtitles.player.tracks().await);
        assert_eq!(entry(&list, "ext:tr1").unwrap()["selected"], true, "the one on is listed");
    }

    #[tokio::test]
    async fn a_used_up_quota_passes_on_to_the_next() {
        let rig = rig(Worker { apply: properties(), quota: Some("tr1"), ..Worker::default() }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the second Turkish one is timed", || (mpv.lock().unwrap().delay - 1.82).abs() < 1e-9).await;
        let list = rig.subtitles.unified(&rig.subtitles.player.tracks().await);
        assert_eq!(entry(&list, "ext:tr1").unwrap()["unavailable"], "quota-exhausted");
        assert_eq!(entry(&list, "ext:tr2").unwrap()["selected"], true);
    }

    #[tokio::test]
    async fn the_preference_set_during_a_film_is_kept_and_put_on() {
        let rig = rig(Worker { apply: properties(), ..Worker::default() }, Preferences::default()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let seen = Arc::clone(&rig.seen);
        until("the candidates are known", || {
            seen.lock().unwrap().iter().any(|l| l.starts_with("POST /media/subtitles/prepare"))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        // The file's English track.
        assert!(rig.subtitles.set_preference(Some("eng".into())).await);
        assert_eq!(rig.mpv.lock().unwrap().sid, Some(1));
        assert_eq!(rig.subtitles.preferences().language.as_deref(), Some("en"));
        // Turkish: only fetched ones exist, so one of those.
        assert!(rig.subtitles.set_preference(Some("tr".into())).await);
        let mpv = Arc::clone(&rig.mpv);
        until("a Turkish one is on", || {
            let mpv = mpv.lock().unwrap();
            language_of(&mpv, mpv.sid).as_deref() == Some("tr")
        })
        .await;
        // Off.
        assert!(rig.subtitles.set_preference(None).await);
        assert_eq!(rig.mpv.lock().unwrap().sid, None);
        assert_eq!(rig.subtitles.preferences().enabled, Some(false));
        // Kept on disk, for the next film.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let kept: Preferences = serde_json::from_slice(&std::fs::read(&rig.subtitles.preferences_file).unwrap()).unwrap();
        assert_eq!(kept.enabled, Some(false));
        assert_eq!(kept.language.as_deref(), Some("tr"));
    }

    #[tokio::test]
    async fn choosing_off_and_an_unloaded_external() {
        let rig = rig(Worker { apply: properties(), ..Worker::default() }, Preferences::default()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Nobody chose before: nothing was put on.
        assert_eq!(rig.mpv.lock().unwrap().sid, None);
        assert!(rig.subtitles.choose("ext:en1").await);
        let on = rig.mpv.lock().unwrap().sid;
        assert!(on.is_some());
        assert!(rig.subtitles.choose("off").await);
        assert_eq!(rig.mpv.lock().unwrap().sid, None);
        assert_eq!(rig.subtitles.preferences().enabled, None);
        let text = rig.subtitles.text().await;
        assert_eq!(text["text"], "");
        assert_eq!(text["ass"], "");
    }

    #[tokio::test]
    async fn h_an_external_chosen_over_the_preferred_embedded_one_is_checked_and_kept() {
        // The film has its own Turkish track, which is what comes on; the
        // viewer picks a fetched Turkish one instead. It is checked (the
        // embedded track is only the worker's timing reference), found to be
        // another timebase, and stays on as the viewer's choice, untimed.
        // Shown, the refused one can be picked at all.
        let rig = rig(Worker { apply: Value::Null, ..Worker::default() }, shown()).await;
        rig.mpv.lock().unwrap().tracks.push(json!({"id": 9, "type": "sub", "lang": "tur", "external": false, "codec": "subrip"}));
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the embedded Turkish track is on", || mpv.lock().unwrap().sid == Some(9)).await;
        // The fetched ones are checked beside it, and none is put on.
        let seen = Arc::clone(&rig.seen);
        until("both fetched Turkish ones are analysed", || syncs(&seen) == 2).await;
        assert_eq!(rig.mpv.lock().unwrap().sid, Some(9));
        assert!(rig.subtitles.choose("ext:tr2").await);
        let chosen = rig.mpv.lock().unwrap().sid;
        assert!(chosen.is_some() && chosen != Some(9));
        let subtitles = Arc::clone(&rig.subtitles);
        let mut refused = Value::Null;
        for _ in 0..100 {
            let tracks = subtitles.player.tracks().await;
            let list = subtitles.unified(&tracks);
            if let Some(entry) = list.as_array().unwrap().iter().find(|t| t["id"] == "ext:tr2") {
                if entry["sync"]["state"] == "rejected" {
                    refused = entry.clone();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(refused["sync"]["eligibility"], "REJECT_TIMEBASE_MISMATCH", "{refused:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mpv = rig.mpv.lock().unwrap();
        assert_eq!(mpv.sid, chosen, "the viewer's choice stays, embedded or not");
        assert_eq!(mpv.delay, 0.0);
        drop(mpv);
        // Already answered, it is not analysed again, and nothing else is
        // tried in its place.
        assert_eq!(syncs(&rig.seen), 2);
    }

    #[tokio::test]
    async fn i_with_auto_sync_off_nothing_is_analysed_or_timed() {
        let off = Preferences { auto_sync_enabled: Some(false), ..turkish() };
        let rig = rig(Worker { apply: properties(), ..Worker::default() }, off).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the Turkish subtitle is on", || mpv.lock().unwrap().sid == Some(3)).await;
        assert!(rig.subtitles.choose("ext:tr2").await);
        tokio::time::sleep(Duration::from_millis(1000)).await;
        assert_eq!(syncs(&rig.seen), 0);
        assert_eq!(rig.mpv.lock().unwrap().delay, 0.0);
        // Asked for in so many words (Ok on the delay), it is done.
        assert!(rig.subtitles.auto_sync().await);
        let seen = Arc::clone(&rig.seen);
        until("the requested analysis ran", || syncs(&seen) == 1).await;
        let mpv = Arc::clone(&rig.mpv);
        until("the requested timing is on", || (mpv.lock().unwrap().delay - 1.82).abs() < 1e-9).await;
    }

    #[tokio::test]
    async fn turning_auto_sync_on_during_a_film_times_the_subtitle_that_is_on() {
        let off = Preferences { auto_sync_enabled: Some(false), ..turkish() };
        let rig = rig(Worker { apply: properties(), ..Worker::default() }, off).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the Turkish subtitle is on", || mpv.lock().unwrap().sid == Some(3)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(syncs(&rig.seen), 0);
        assert!(rig.subtitles.set_auto_sync(true).await);
        let mpv = Arc::clone(&rig.mpv);
        until("the timing is on", || (mpv.lock().unwrap().delay - 1.82).abs() < 1e-9).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let kept: Preferences = serde_json::from_slice(&std::fs::read(&rig.subtitles.preferences_file).unwrap()).unwrap();
        assert_eq!(kept.auto_sync_enabled, Some(true));
        assert_eq!(kept.language.as_deref(), Some("tr"), "the language is its own setting");
    }

    #[tokio::test]
    async fn the_same_file_under_another_id_is_not_analysed_again() {
        // ext:tr2 is the file ext:tr1 was: the first is refused, the second
        // is skipped as a duplicate rather than sent to the worker again.
        // Shown in the menu for the test to read: it is left out by default.
        let shown = Preferences { show_incompatible_subtitles: Some(true), ..turkish() };
        let rig = rig(Worker { apply: Value::Null, twins: true, ..Worker::default() }, shown).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let subtitles = Arc::clone(&rig.subtitles);
        let mut duplicate = Value::Null;
        for _ in 0..100 {
            let tracks = subtitles.player.tracks().await;
            let list = subtitles.unified(&tracks);
            if let Some(entry) = list.as_array().unwrap().iter().find(|t| t["id"] == "ext:tr2") {
                if !entry["sync"].is_null() && entry["sync"]["state"] == "rejected" {
                    duplicate = entry.clone();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(duplicate["sync"]["eligibility"], "REJECT_DUPLICATE", "{duplicate:?}");
        assert_eq!(syncs(&rig.seen), 1);
    }
}
