//! Subtitles for the interface's own player.
//!
//! Three kinds of subtitle meet here and leave as one list:
//!
//! * `embedded` -- the tracks the file carries, which mpv already lists;
//! * `stream_external` -- ones the addon attached to the stream itself;
//! * `addon_external` -- ones a subtitle addon found for the title.
//!
//! The last two are the worker's to find, fetch and time
//! (`media/subtitles/`): it owns the network, and this player's ffmpeg has
//! no TLS. This module decides what is on, tells mpv, and puts the worker's
//! timing into effect with `sub-delay` and `sub-speed`, or by swapping in the
//! corrected file the worker wrote for a different cut.
//!
//! Two rules shape what it may do on its own:
//!
//! * the viewer's choice is final for the film: once somebody has picked a
//!   subtitle, or moved its delay, nothing automatic changes either until
//!   they ask for it again (`SubtitleAutoSync`);
//! * a choice is remembered by what it means -- on or off, which language,
//!   carried or fetched -- and never by a track number, which means nothing
//!   in the next episode.

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
}

/// One external subtitle the worker offered for this film.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub source: String,
    pub language: Option<String>,
    pub label: Option<String>,
    pub hash_match: bool,
}

impl Candidate {
    fn from_worker(value: &Value) -> Option<Self> {
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
        })
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
                    self.with_film(number, |film| film.candidates = candidates);
                }
                Err(error) => eprintln!("mediaboxd-rs: film {number}: no external subtitles: {error}"),
            }
        }

        let (manual, candidates) = match self.with_film(number, |film| (film.manual, film.candidates.clone())) {
            Some(found) => found,
            None => return,
        };
        if manual {
            return;
        }
        self.apply(number, auto_choice(&preferences, &tracks, &candidates)).await;
    }

    /// Put an automatically chosen external subtitle on and time it.
    async fn try_external(self: &Arc<Self>, number: u64, id: String) {
        self.with_film(number, |film| film.tried.push(id.clone()));
        if self.put_on(number, &id).await {
            spawn_timing(Arc::clone(self), number, id, true);
        }
    }

    /// Load (if need be) and select one external candidate.
    async fn put_on(&self, number: u64, id: &str) -> bool {
        let Some(loaded) = self.load(number, id).await else {
            return false;
        };
        let applied = self.with_film(number, |film| film.timing.get(id).cloned()).flatten();
        self.player.select_subtitle(Some(loaded.mpv)).await;
        match applied.filter(|timing| timing.state == "applied") {
            Some(timing) if timing.model.as_deref() != Some("piecewise") => {
                self.player.subtitle_timing(timing.offset, timing.scale).await;
            }
            _ => {
                self.player.subtitle_timing(0.0, 1.0).await;
            }
        }
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
        self.set_timing(number, &id, Timing { state: "analysing".into(), scale: 1.0, ..Timing::default() });
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
                    self.set_timing(number, &id, Timing { state: "failed".into(), scale: 1.0, ..Timing::default() });
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
        let apply = job.get("apply").filter(|value| !value.is_null()).cloned();
        let Some(apply) = apply else {
            self.set_timing(number, id, timing_from(job, "rejected"));
            // A timing nobody could find is most often the wrong subtitle.
            // An automatic choice moves on to the next of its language; one
            // the viewer made stays exactly as it is.
            let next = self.with_film(number, |film| {
                if film.manual || film.selected.as_deref() != Some(id) {
                    return None;
                }
                next_candidate(&film.candidates, &film.tried, id)
            });
            if let Some(Some(next)) = next.filter(|_| automatic) {
                eprintln!("mediaboxd-rs: film {number}: subtitle {id} does not fit, trying {next}");
                self.try_external(number, next).await;
            } else if automatic {
                // Every one tried was rejected: the best-ranked stays on, as
                // it came, rather than none at all.
                let first = self.with_film(number, |film| film.tried.first().cloned()).flatten();
                if let Some(first) = first.filter(|first| first != id) {
                    let manual = self.with_film(number, |film| film.manual).unwrap_or(true);
                    if !manual {
                        self.put_on(number, &first).await;
                    }
                }
            }
            return;
        };
        let timing = timing_from(job, "applied");
        let (on, by_hand) = self
            .with_film(number, |film| (film.selected.as_deref() == Some(id), film.manual_delay))
            .unwrap_or((false, true));
        match apply.get("kind").and_then(Value::as_str) {
            Some("file") => {
                // A different cut: the corrected timeline replaces the track,
                // and the original goes, so the menu still shows one entry.
                let Some(url) = apply.get("url").and_then(Value::as_str) else { return };
                let Some(key) = apply.get("key").and_then(Value::as_str) else { return };
                let candidate = self
                    .with_film(number, |film| film.candidates.iter().find(|c| c.id == id).cloned())
                    .flatten();
                let title = candidate.as_ref().map(title_for).unwrap_or_else(|| "Harici".into());
                let language = candidate.as_ref().and_then(|c| c.language.clone());
                let Some(mpv) = self.player.sub_add(url, &title, language.as_deref()).await else {
                    return;
                };
                let old = self
                    .with_film(number, |film| {
                        film.loaded.insert(
                            id.to_owned(),
                            Loaded { key: key.to_owned(), url: url.to_owned(), mpv },
                        )
                    })
                    .flatten();
                if on {
                    self.player.select_subtitle(Some(mpv)).await;
                    if !by_hand {
                        self.player.subtitle_timing(0.0, 1.0).await;
                    }
                }
                if let Some(old) = old {
                    self.player.sub_remove(old.mpv).await;
                }
            }
            _ => {
                if on && !by_hand {
                    self.player.subtitle_timing(timing.offset, timing.scale).await;
                }
            }
        }
        let state = if on && by_hand { "ready" } else { "applied" };
        self.set_timing(number, id, Timing { state: state.into(), ..timing });
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
        if !known || !self.put_on(number, id).await {
            return false;
        }
        let untimed = self
            .with_film(number, |film| !film.timing.contains_key(id))
            .unwrap_or(false);
        if untimed {
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
            film.candidates.clone()
        }) else {
            return false;
        };
        let tracks = self.player.tracks().await;
        let choice = auto_choice(&self.preferences(), &tracks, &candidates);
        self.apply(number, choice).await;
        true
    }

    async fn apply(self: &Arc<Self>, number: u64, choice: Choice) {
        match choice {
            Choice::Leave => {}
            Choice::Off => {
                self.player.select_subtitle(None).await;
                self.with_film(number, |film| film.selected = Some("off".into()));
            }
            Choice::Embedded(id) => {
                self.player.select_subtitle(Some(id)).await;
                self.player.subtitle_timing(0.0, 1.0).await;
                self.with_film(number, |film| film.selected = Some(format!("emb:{id}")));
            }
            Choice::External(id) => {
                self.try_external(number, id).await;
            }
        }
    }

    /// The viewer moved the delay: automatic timing keeps off it from here.
    pub fn delay_moved(&self) {
        let number = self.player.film();
        self.with_film(number, |film| film.manual_delay = true);
    }

    /// Put the automatic timing back on the subtitle that is on.
    pub async fn auto_sync(self: &Arc<Self>) -> bool {
        let number = self.player.film();
        let Some((selected, timing)) = self
            .with_film(number, |film| {
                film.manual_delay = false;
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
            Some(timing) if timing.state == "applied" || timing.state == "ready" => {
                if timing.model.as_deref() != Some("piecewise") {
                    self.player.subtitle_timing(timing.offset, timing.scale).await;
                } else {
                    self.player.subtitle_timing(0.0, 1.0).await;
                }
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
        Value::Array(unify(tracks, film.map(|film| (&film.candidates, &film.loaded, &film.timing))))
    }

    /// The line on screen, for the interface to draw.
    pub async fn text(&self) -> Value {
        let text = self.player.subtitle_text().await.unwrap_or_default();
        json!({"film": self.player.film(), "text": text})
    }
}

/// `Subtitles::time` on the runtime. Boxed, because a rejected automatic
/// choice moves on to the next candidate from inside it, which starts this
/// again: without the box the future would contain itself.
fn spawn_timing(this: Arc<Subtitles>, number: u64, id: String, automatic: bool) {
    let future: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move { this.time(number, id, automatic).await });
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

/// The next untried candidate in the same language as `current`.
fn next_candidate(candidates: &[Candidate], tried: &[String], current: &str) -> Option<String> {
    let language = candidates.iter().find(|c| c.id == current)?.language.clone();
    candidates
        .iter()
        .filter(|c| c.language == language && !tried.contains(&c.id))
        .take(ATTEMPTS.saturating_sub(tried.len()))
        .map(|c| c.id.clone())
        .next()
}

fn timing_from(job: &Value, state: &str) -> Timing {
    let result = job.get("result").cloned().unwrap_or(Value::Null);
    Timing {
        state: state.into(),
        model: result.get("model").and_then(Value::as_str).map(str::to_owned),
        offset: result.get("offset").and_then(Value::as_f64).unwrap_or(0.0),
        scale: result.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
        confidence: result.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
        reason: result.get("reason").and_then(Value::as_str).map(str::to_owned),
    }
}

fn title_for(candidate: &Candidate) -> String {
    candidate.label.clone().unwrap_or_else(|| {
        match candidate.source.as_str() {
            "stream_external" => "Akış",
            _ => "Harici",
        }
        .into()
    })
}

fn unify(
    tracks: &[Value],
    film: Option<(&Vec<Candidate>, &HashMap<String, Loaded>, &HashMap<String, Timing>)>,
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
        }
    }

    fn on(language: &str, origin: Option<Origin>) -> Preferences {
        Preferences { enabled: Some(true), language: Some(language.into()), origin }
    }

    #[test]
    fn nothing_is_chosen_for_a_viewer_who_never_chose() {
        let tracks = [track(1, "tur", false)];
        assert_eq!(auto_choice(&Preferences::default(), &tracks, &[]), Choice::Leave);
    }

    #[test]
    fn off_stays_off() {
        let preferences = Preferences { enabled: Some(false), language: Some("tr".into()), origin: None };
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
        assert_eq!(next_candidate(&candidates, &["ext:1".into()], "ext:1"), Some("ext:2".into()));
        let tried: Vec<String> = ["ext:1", "ext:2", "ext:3"].iter().map(|s| s.to_string()).collect();
        assert_eq!(next_candidate(&candidates, &tried, "ext:3"), None);
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
            Timing { state: "applied".into(), model: Some("offset".into()), offset: 1.82, scale: 1.0, confidence: 0.97, reason: None },
        );
        let list = unify(&tracks, Some((&candidates, &loaded, &timing)));
        let ids: Vec<&str> = list.iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["emb:1", "emb:2", "ext:a", "ext:b"]);
        assert_eq!(list[1]["source"], "embedded");
        assert_eq!(list[1]["language"], "tr");
        assert_eq!(list[2]["source"], "addon_external");
        assert_eq!(list[2]["mpv_id"], 3);
        assert_eq!(list[2]["selected"], true);
        assert_eq!(list[2]["sync"]["offset"], 1.82);
        // Not loaded yet: listed, with no track number.
        assert!(list[3]["mpv_id"].is_null());
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
    #[derive(Clone)]
    struct Worker {
        apply: Value,
        /// Polls answered "running" before "done".
        polls_before_done: usize,
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
                let answer = if line.starts_with("POST /media/subtitles/prepare") {
                    json!({"video": {"identity": "osh:1:2"}, "candidates": [
                        {"id": "ext:tr1", "source": "addon_external", "language": "tr", "label": "WEB-DL"},
                        {"id": "ext:tr2", "source": "addon_external", "language": "tr"},
                        {"id": "ext:en1", "source": "stream_external", "language": "en"},
                    ]})
                } else if line.starts_with("POST /media/subtitles/load") {
                    let candidate: Value = serde_json::from_str(&body).unwrap();
                    let id = candidate["candidate"].as_str().unwrap().trim_start_matches("ext:");
                    json!({"key": format!("{id:0>32}"), "url": format!("http://127.0.0.1:8790/media/subtitles/file/{id}.srt"), "format": "srt"})
                } else if line.starts_with("POST /media/subtitles/sync") {
                    json!({"job": "j1", "state": "running"})
                } else if line.starts_with("GET /media/subtitles/sync/") {
                    let mut count = polls.lock().unwrap();
                    *count += 1;
                    if *count <= worker.polls_before_done {
                        json!({"job": "j1", "state": "running"})
                    } else {
                        let decision = if worker.apply.is_null() { "reject" } else { "apply" };
                        json!({"job": "j1", "state": "done", "apply": worker.apply,
                               "result": {"model": if worker.apply["kind"] == "file" {"piecewise"} else {"offset"},
                                          "decision": decision, "offset": 1.82, "scale": 1.0, "confidence": 0.97}})
                    }
                } else {
                    json!({})
                };
                let body = answer.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
        Preferences { enabled: Some(true), language: Some("tr".into()), origin: None }
    }

    fn properties() -> Value {
        json!({"kind": "properties", "subDelay": 1.82, "subSpeed": 1.0})
    }

    #[tokio::test]
    async fn an_addon_subtitle_reaches_the_player_and_is_timed() {
        let rig = rig(Worker { apply: properties(), polls_before_done: 0 }, turkish()).await;
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
        assert_eq!(on["source"], "addon_external");
        assert_eq!(on["sync"]["state"], "applied");
        assert_eq!(list[0]["source"], "embedded");
        assert_eq!(list[4]["source"], "stream_external");
    }

    #[tokio::test]
    async fn the_viewers_choice_is_not_overwritten_by_a_late_answer() {
        // The worker takes a few polls to answer; meanwhile the viewer picks
        // the file's English track.
        let rig = rig(Worker { apply: properties(), polls_before_done: 2 }, turkish()).await;
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
        let rig = rig(Worker { apply: properties(), polls_before_done: 1 }, turkish()).await;
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
    async fn a_different_cut_swaps_in_the_corrected_file_and_removes_the_original() {
        let corrected = json!({"kind": "file", "key": "c".repeat(32), "url": "http://127.0.0.1:8790/media/subtitles/file/corrected.srt"});
        let rig = rig(Worker { apply: corrected, polls_before_done: 0 }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let mpv = Arc::clone(&rig.mpv);
        until("the corrected file is on and the original gone", || {
            let mpv = mpv.lock().unwrap();
            let on = mpv.tracks.iter().find(|t| t["type"] == "sub" && t["id"].as_i64() == mpv.sid);
            on.is_some_and(|t| t["external-filename"] == "http://127.0.0.1:8790/media/subtitles/file/corrected.srt")
                && !mpv.tracks.iter().any(|t| t["external-filename"] == "http://127.0.0.1:8790/media/subtitles/file/tr1.srt")
        })
        .await;
        let mpv = rig.mpv.lock().unwrap();
        assert_eq!((mpv.delay, mpv.speed), (0.0, 1.0));
        assert!(mpv.log.iter().any(|c| c[0] == "sub-remove" && c[1] == 3));
    }

    #[tokio::test]
    async fn a_rejected_automatic_choice_moves_to_the_next_of_its_language() {
        let rig = rig(Worker { apply: Value::Null, polls_before_done: 0 }, turkish()).await;
        rig.subtitles.begin(0, "s".repeat(32), watch(), Some(5400));
        let seen = Arc::clone(&rig.seen);
        until("the second Turkish candidate is loaded", || {
            seen.lock().unwrap().iter().any(|l| l.contains("\"candidate\":\"ext:tr2\""))
        })
        .await;
        // Never the English one: the language is the viewer's.
        assert!(!rig.seen.lock().unwrap().iter().any(|l| l.contains("ext:en1") && l.starts_with("POST /media/subtitles/load")));
    }

    #[tokio::test]
    async fn the_preference_set_during_a_film_is_kept_and_put_on() {
        let rig = rig(Worker { apply: properties(), polls_before_done: 0 }, Preferences::default()).await;
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
        until("a Turkish one is on", || mpv.lock().unwrap().sid == Some(3)).await;
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
        let rig = rig(Worker { apply: properties(), polls_before_done: 0 }, Preferences::default()).await;
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
    }
}
