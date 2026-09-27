//! How a film ends, decided once, in one place.
//!
//! A film on this box is played by the interface's own player (mpv) or by
//! Kodi, and ends in one of three ways -- the ways Kodi's own player reports
//! it (`OnPlayBackStopped`, `OnPlayBackEnded`, `OnPlayBackError`):
//!
//! * **stopped**: somebody asked -- the viewer, a new film, a handover;
//! * **ended**: the player reached the end of the film;
//! * **failed**: it could not go on.
//!
//! This module is the only place that decides which. The interface draws the
//! answer and sends commands; it does not infer an ending from a dark video
//! plane, which is how a failed seek was once taken for the end of a film
//! (2026-09-27: the source's session had been reaped under the player, the
//! seek got a 404, mpv reached "end of file" 88 ms later and the interface
//! told the account the film was closed).
//!
//! What a player says is not taken on its word. mpv's own manual says its
//! `eof` "can include incomplete files or broken network connections", and
//! Kodi's VideoPlayer sets its error flag only when a file fails to *open*: a
//! read that fails half way through is reported as the end. So "the player
//! reached the end" counts as **ended** only when the position is at the
//! film's length; short of it, it is **failed**. The tolerance is a few
//! seconds of the player's own length, not a share of the film: the
//! account's "watched" threshold is a different question, answered by the
//! media core with stremio-core's rules.
//!
//! The same record writes the account: where the film got to while it plays,
//! and how it ended. `closed` is the Stremio client's "the player was
//! closed" -- sent when the film was stopped or ended, never when it failed,
//! so a failure is kept as progress to come back to and not as a film the
//! viewer finished with.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use mediabox_core::WatchRef;
use serde::Serialize;
use serde_json::{Value, json};

/// stremio-core's `PUSH_TO_LIBRARY_EVERY`.
pub const TELL_THE_ACCOUNT_EVERY: Duration = Duration::from_secs(90);
/// A position this far from where the clock says it should be is a jump the
/// viewer made, not playback.
const JUMP_SECONDS: f64 = 15.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerKind {
    Here,
    Kodi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Stopped,
    Ended,
    Failed,
}

/// Why the control plane is ending a film itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// The viewer stopped it.
    Stop,
    /// Another film is starting.
    Replace,
    /// It carries on in Kodi.
    Handover,
}

/// What a player said, or did, when it stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// mpv's `eof-reached`: it is holding the last frame (`--keep-open`).
    EndOfFile,
    /// mpv's `end-file` event, with its `reason` and `file_error`.
    EndFile { reason: String, error: Option<String> },
    /// Kodi's `Player.OnStop`, with its `data.end`.
    KodiStop { end: bool },
    /// The application was closed while it played: Kodi quit.
    Closed,
    /// The player went away without a word: its process or socket is gone.
    Gone,
}

/// Where the film had got to when it stopped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reached {
    pub position: f64,
    pub duration: Option<f64>,
    /// Whether the duration is the player's own reading of the file. The
    /// catalogue's runtime is a round number of minutes, and is given a
    /// wider margin.
    pub exact: bool,
}

impl Reached {
    fn at_the_end(&self) -> Option<bool> {
        let duration = self.duration.filter(|seconds| *seconds > 0.0)?;
        let margin = if self.exact {
            (duration * 0.005).max(15.0)
        } else {
            (duration * 0.03).max(180.0)
        };
        Some(self.position >= duration - margin)
    }
}

/// How a film ended, and what to tell the viewer when it did not end well.
pub fn verdict(intent: Option<Intent>, signal: &Signal, reached: Reached) -> (Outcome, Option<String>) {
    if intent.is_some() {
        return (Outcome::Stopped, None);
    }
    let reached_the_end = || match reached.at_the_end() {
        Some(false) => (
            Outcome::Failed,
            Some(format!(
                "Oynatma {} konumunda kesildi: kaynak okunamadı.",
                clock(reached.position)
            )),
        ),
        // A film with no length to hold the position against: the player's
        // word is all there is.
        _ => (Outcome::Ended, None),
    };
    match signal {
        Signal::EndOfFile | Signal::KodiStop { end: true } => reached_the_end(),
        Signal::EndFile { reason, error } => match reason.as_str() {
            "eof" => reached_the_end(),
            "error" => (
                Outcome::Failed,
                Some(match error.as_deref().filter(|text| !text.is_empty()) {
                    Some(text) => format!("Oynatıcı hata verdi: {text}."),
                    None => "Oynatıcı hata verdi.".to_string(),
                }),
            ),
            // quit, stop, redirect: something asked it to.
            _ => (Outcome::Stopped, None),
        },
        Signal::KodiStop { end: false } | Signal::Closed => (Outcome::Stopped, None),
        Signal::Gone => (
            Outcome::Failed,
            Some("Oynatıcı beklenmedik biçimde kapandı.".to_string()),
        ),
    }
}

fn clock(seconds: f64) -> String {
    let whole = seconds.max(0.0) as u64;
    format!("{}:{:02}:{:02}", whole / 3600, whole / 60 % 60, whole % 60)
}

/// How a film ended: kept after it has, for whoever draws it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finished {
    pub film: u64,
    pub player: PlayerKind,
    pub outcome: Outcome,
    pub position: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// What the supervisor needs done outside itself.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Write this progress change to the account.
    Account(Value),
    /// Let the worker's session go: its player is done with it.
    Release(String),
}

/// A film starting.
#[derive(Debug, Clone)]
pub struct Start {
    pub player: PlayerKind,
    pub watch: Option<WatchRef>,
    /// The worker session it reads, when it reads one.
    pub session: Option<String>,
    /// The catalogue's length, when it knows one.
    pub duration: Option<u64>,
    pub position: u64,
}

struct Film {
    number: u64,
    player: PlayerKind,
    watch: Option<WatchRef>,
    session: Option<String>,
    given: Option<f64>,
    /// The player's own reading of the length.
    measured: Option<f64>,
    position: f64,
    intent: Option<Intent>,
    /// When the position was last read, what it was, and whether it was paused.
    seen: Option<(Instant, f64, bool)>,
    told: Option<Instant>,
    jumped: bool,
}

impl Film {
    fn reached(&self) -> Reached {
        Reached {
            position: self.position,
            duration: self.measured.or(self.given),
            exact: self.measured.is_some(),
        }
    }

    fn change(&self, closed: bool) -> Option<Value> {
        let watch = self.watch.as_ref()?;
        let duration = self.measured.or(self.given).filter(|seconds| *seconds > 0.0)?;
        Some(json!({
            "type": watch.kind,
            "id": watch.id,
            "videoId": watch.video_id,
            "timeMs": (self.position.max(0.0) * 1000.0) as u64,
            "durationMs": (duration * 1000.0) as u64,
            "seek": self.jumped,
            "closed": closed,
            "name": watch.name,
            "poster": watch.poster,
        }))
    }
}

#[derive(Default)]
struct Inner {
    next: u64,
    film: Option<Film>,
    finished: Option<Finished>,
}

pub struct Supervisor {
    inner: Mutex<Inner>,
    effects: Box<dyn Fn(Effect) + Send + Sync>,
}

impl Supervisor {
    pub fn new(effects: impl Fn(Effect) + Send + Sync + 'static) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            effects: Box::new(effects),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn run(&self, effects: Vec<Effect>) {
        for effect in effects {
            (self.effects)(effect);
        }
    }

    /// A film has started. Whatever was playing is over: replaced.
    pub fn begin(&self, start: Start) -> u64 {
        let mut effects = Vec::new();
        let number = {
            let mut inner = self.lock();
            if let Some(mut previous) = inner.film.take() {
                previous.intent.get_or_insert(Intent::Replace);
                inner.finished = Some(close(previous, &Signal::Gone, &mut effects));
            }
            inner.next += 1;
            let number = inner.next;
            inner.film = Some(Film {
                number,
                player: start.player,
                watch: start.watch,
                session: start.session,
                given: start.duration.filter(|seconds| *seconds > 0).map(|seconds| seconds as f64),
                measured: None,
                position: start.position as f64,
                intent: None,
                seen: None,
                told: None,
                jumped: false,
            });
            number
        };
        self.run(effects);
        number
    }

    /// The film playing on `player` now, if one is.
    pub fn current(&self, player: PlayerKind) -> Option<u64> {
        self.lock()
            .film
            .as_ref()
            .filter(|film| film.player == player)
            .map(|film| film.number)
    }

    /// What the film belongs to, for a handover to carry on with.
    pub fn watch(&self, film: u64) -> Option<WatchRef> {
        self.lock()
            .film
            .as_ref()
            .filter(|playing| playing.number == film)
            .and_then(|playing| playing.watch.clone())
    }

    /// The control plane is about to end this film itself.
    pub fn intend(&self, film: u64, intent: Intent) {
        if let Some(playing) = self.lock().film.as_mut().filter(|playing| playing.number == film) {
            playing.intent = Some(intent);
        }
    }

    /// Where the film has got to, as its player says. Writes the account
    /// every so often and after a jump, as every Stremio client does.
    pub fn observe(&self, film: u64, position: f64, duration: Option<f64>, paused: bool, now: Instant) {
        if !position.is_finite() || position < 0.0 {
            return;
        }
        let mut effects = Vec::new();
        {
            let mut inner = self.lock();
            let Some(playing) = inner.film.as_mut().filter(|playing| playing.number == film) else {
                return;
            };
            if let Some(seconds) = duration.filter(|seconds| seconds.is_finite() && *seconds > 0.0) {
                playing.measured = Some(seconds);
            }
            if let (Some((at, was, was_paused)), Some(_)) = (playing.seen, playing.told) {
                let moved = if was_paused { 0.0 } else { now.duration_since(at).as_secs_f64() };
                if (position - (was + moved)).abs() > JUMP_SECONDS {
                    playing.jumped = true;
                }
            }
            playing.position = position;
            playing.seen = Some((now, position, paused));
            let due = playing.jumped
                || playing
                    .told
                    .is_none_or(|told| now.duration_since(told) >= TELL_THE_ACCOUNT_EVERY);
            if due && let Some(change) = playing.change(false) {
                playing.told = Some(now);
                playing.jumped = false;
                effects.push(Effect::Account(change));
            }
        }
        self.run(effects);
    }

    /// The film has stopped, and this is what its player said. Decided once:
    /// a second word about the same film changes nothing.
    pub fn finish(&self, film: u64, signal: Signal) -> Option<Finished> {
        let mut effects = Vec::new();
        let finished = {
            let mut inner = self.lock();
            if inner.film.as_ref().is_none_or(|playing| playing.number != film) {
                return None;
            }
            let playing = inner.film.take()?;
            let finished = close(playing, &signal, &mut effects);
            inner.finished = Some(finished.clone());
            finished
        };
        eprintln!(
            "mediaboxd-rs: film {} on {:?}: {:?} at {:.0}s after {:?}{}",
            finished.film,
            finished.player,
            finished.outcome,
            finished.position,
            signal,
            finished.message.as_deref().map(|text| format!(" -- {text}")).unwrap_or_default()
        );
        self.run(effects);
        Some(finished)
    }

    /// What the interface draws: the film playing now, and how the last one
    /// ended.
    pub fn status(&self) -> Value {
        let inner = self.lock();
        json!({
            "film": inner.film.as_ref().map(|playing| playing.number),
            "finished": inner.finished,
        })
    }
}

fn close(playing: Film, signal: &Signal, effects: &mut Vec<Effect>) -> Finished {
    let reached = playing.reached();
    let (outcome, message) = verdict(playing.intent, signal, reached);
    // Stopped or ended is the player closed; a handover is not -- the film
    // goes on in Kodi -- and a failure is not either: it is where the viewer
    // comes back to.
    let closed = match outcome {
        Outcome::Failed => false,
        Outcome::Stopped => playing.intent != Some(Intent::Handover),
        Outcome::Ended => true,
    };
    if let Some(change) = playing.change(closed) {
        effects.push(Effect::Account(change));
    }
    if let Some(session) = playing.session.clone() {
        effects.push(Effect::Release(session));
    }
    Finished {
        film: playing.number,
        player: playing.player,
        outcome,
        position: reached.position,
        duration: reached.duration,
        message,
    }
}

// ---------------------------------------------------------------- followers

/// Follow a film on the interface's own player until it stops, over mpv's
/// IPC socket: its `end-file` event, its `eof-reached` under `--keep-open`,
/// and the socket closing when the process goes. `here` is the player's own
/// number for the film, which moves when it is stopped or replaced.
pub async fn follow_mpv(
    supervisor: std::sync::Arc<Supervisor>,
    player: std::sync::Arc<crate::player::PlayerManager>,
    film: u64,
    here: u64,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut stream = None;
    for _ in 0..150 {
        if player.film() != here {
            break;
        }
        if let Ok(connected) = tokio::net::UnixStream::connect(player.socket()).await {
            stream = Some(connected);
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let Some(stream) = stream else {
        let signal = if player.film() == here {
            Signal::EndFile {
                reason: "error".into(),
                error: Some("oynatıcı açılmadı".into()),
            }
        } else {
            Signal::Gone
        };
        supervisor.finish(film, signal);
        return;
    };
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let ask = |command: Value| format!("{command}\n");
    let polls = [
        ask(json!({"command": ["get_property", "time-pos"], "request_id": 101})),
        ask(json!({"command": ["get_property", "duration"], "request_id": 102})),
        ask(json!({"command": ["get_property", "pause"], "request_id": 103})),
    ]
    .concat();
    let observe = ask(json!({"command": ["observe_property", 1, "eof-reached"]}));
    let signal = if writer.write_all(observe.as_bytes()).await.is_err() {
        Signal::Gone
    } else {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let (mut position, mut duration, mut eof) = (None, None, false);
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Ok(Some(line)) = line else { break Signal::Gone };
                    let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
                    match message.get("event").and_then(Value::as_str) {
                        Some("end-file") => break Signal::EndFile {
                            reason: message.get("reason").and_then(Value::as_str).unwrap_or("unknown").to_string(),
                            error: message.get("file_error").and_then(Value::as_str).map(str::to_owned),
                        },
                        Some("property-change")
                            if message.get("name").and_then(Value::as_str) == Some("eof-reached")
                                && message.get("data").and_then(Value::as_bool) == Some(true) =>
                        {
                            // Where it stopped is asked now, not a second ago.
                            eof = true;
                            if writer.write_all(polls.as_bytes()).await.is_err() {
                                break Signal::Gone;
                            }
                        }
                        _ => {}
                    }
                    let number = |message: &Value| message.get("data").and_then(Value::as_f64).filter(|value| value.is_finite());
                    match message.get("request_id").and_then(Value::as_u64) {
                        Some(101) => position = number(&message),
                        Some(102) => duration = number(&message),
                        Some(103) => {
                            let paused = message.get("data").and_then(Value::as_bool).unwrap_or(false);
                            if let Some(seconds) = position {
                                supervisor.observe(film, seconds, duration, paused, Instant::now());
                            }
                            if eof {
                                break Signal::EndOfFile;
                            }
                        }
                        _ => {}
                    }
                }
                _ = tick.tick() => {
                    if writer.write_all(polls.as_bytes()).await.is_err() {
                        break Signal::Gone;
                    }
                }
            }
        }
    };
    // Held on its last frame at an end, early or not: the film is over, and
    // so is the player.
    if supervisor.finish(film, signal).is_some() && player.film() == here {
        player.stop().await;
    }
}

/// Whether Kodi's own schema says `Player.OnStop` carries `data.end` -- read
/// from `JSONRPC.Introspect` at run time rather than assumed from a version.
pub fn stop_carries_end(introspection: &Value) -> bool {
    let result = introspection.get("result").unwrap_or(introspection);
    result
        .pointer("/notifications/Player.OnStop/params")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|param| param.get("name").and_then(Value::as_str) == Some("data"))
        .any(|data| data.pointer("/properties/end/type").and_then(Value::as_str) == Some("boolean"))
}

/// Kodi's TCP JSON-RPC sends objects back to back with nothing between
/// them. Take every complete one off the front of `buffer`.
pub fn split_json(buffer: &mut Vec<u8>) -> Vec<Value> {
    let mut values = Vec::new();
    let mut consumed = 0;
    let mut stream = serde_json::Deserializer::from_slice(buffer).into_iter::<Value>();
    loop {
        match stream.next() {
            Some(Ok(value)) => {
                values.push(value);
                consumed = stream.byte_offset();
            }
            Some(Err(error)) if error.is_eof() => break,
            Some(Err(_)) => {
                // Not JSON at all: nothing after it can be trusted to line up.
                consumed = buffer.len();
                break;
            }
            None => break,
        }
    }
    buffer.drain(..consumed);
    values
}

/// Follow a film in Kodi until it stops, over Kodi's JSON-RPC notifications
/// (`services.esenabled`, TCP, loopback only), reading its position over
/// HTTP while it plays.
pub async fn follow_kodi(
    supervisor: std::sync::Arc<Supervisor>,
    kodi: std::sync::Arc<crate::kodi::KodiClient>,
    events: std::net::SocketAddr,
    film: u64,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = None;
    for _ in 0..60 {
        if supervisor.current(PlayerKind::Kodi) != Some(film) {
            return;
        }
        if let Ok(connected) = tokio::net::TcpStream::connect(events).await {
            stream = Some(connected);
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let Some(mut stream) = stream else {
        eprintln!("mediaboxd-rs: film {film}: no Kodi notifications on {events}; its ending will not be known");
        supervisor.finish(film, Signal::Closed);
        return;
    };
    let introspect = json!({
        "jsonrpc": "2.0", "id": 1, "method": "JSONRPC.Introspect",
        "params": {"getdescriptions": false, "filter": {"id": "Player.OnStop", "type": "notification"}},
    });
    let _ = stream.write_all(introspect.to_string().as_bytes()).await;

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let signal = loop {
        tokio::select! {
            read = stream.read(&mut chunk) => {
                let Ok(read) = read else { break Signal::Closed };
                if read == 0 {
                    break Signal::Closed;
                }
                buffer.extend_from_slice(&chunk[..read]);
                let mut stopped = None;
                for message in split_json(&mut buffer) {
                    if message.get("id").and_then(Value::as_u64) == Some(1) {
                        if !stop_carries_end(&message) {
                            eprintln!("mediaboxd-rs: Kodi's Player.OnStop does not declare data.end; a stop is taken as a stop");
                        }
                        continue;
                    }
                    if message.get("method").and_then(Value::as_str) == Some("Player.OnStop") {
                        let end = message.pointer("/params/data/end").and_then(Value::as_bool).unwrap_or(false);
                        stopped = Some(Signal::KodiStop { end });
                    }
                }
                if let Some(signal) = stopped {
                    break signal;
                }
            }
            _ = tick.tick() => {
                if supervisor.current(PlayerKind::Kodi) != Some(film) {
                    return;
                }
                if let Some((position, duration, paused)) = kodi.progress().await {
                    supervisor.observe(film, position, Some(duration), paused, Instant::now());
                }
            }
        }
    };
    supervisor.finish(film, signal);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    fn exact(position: f64, duration: f64) -> Reached {
        Reached { position, duration: Some(duration), exact: true }
    }

    #[test]
    fn an_end_of_file_at_the_films_length_is_the_end() {
        let (outcome, message) = verdict(None, &Signal::EndOfFile, exact(6_696.0, 6_700.0));
        assert_eq!(outcome, Outcome::Ended);
        assert!(message.is_none());
    }

    #[test]
    fn an_end_of_file_far_short_of_the_length_is_a_failure() {
        // The fault itself: the seek's range got a 404 and mpv read the end
        // of the file forty minutes into a two-hour film.
        let (outcome, message) = verdict(None, &Signal::EndOfFile, exact(2_400.0, 6_700.0));
        assert_eq!(outcome, Outcome::Failed);
        assert!(message.unwrap().contains("0:40:00"));
        let early_end_file = Signal::EndFile { reason: "eof".into(), error: None };
        assert_eq!(verdict(None, &early_end_file, exact(2_400.0, 6_700.0)).0, Outcome::Failed);
    }

    #[test]
    fn a_player_error_is_a_failure_with_its_reason() {
        let signal = Signal::EndFile { reason: "error".into(), error: Some("loading failed".into()) };
        let (outcome, message) = verdict(None, &signal, exact(10.0, 6_700.0));
        assert_eq!(outcome, Outcome::Failed);
        assert!(message.unwrap().contains("loading failed"));
    }

    #[test]
    fn a_player_that_vanishes_has_failed_unless_it_was_asked_to_go() {
        assert_eq!(verdict(None, &Signal::Gone, exact(10.0, 100.0)).0, Outcome::Failed);
        for intent in [Intent::Stop, Intent::Replace, Intent::Handover] {
            assert_eq!(verdict(Some(intent), &Signal::Gone, exact(10.0, 100.0)).0, Outcome::Stopped);
            // Whatever the player says on its way out.
            assert_eq!(verdict(Some(intent), &Signal::EndOfFile, exact(10.0, 100.0)).0, Outcome::Stopped);
        }
    }

    #[test]
    fn kodi_says_whether_it_reached_the_end_and_is_held_to_it() {
        assert_eq!(verdict(None, &Signal::KodiStop { end: false }, exact(100.0, 6_700.0)).0, Outcome::Stopped);
        assert_eq!(verdict(None, &Signal::KodiStop { end: true }, exact(6_690.0, 6_700.0)).0, Outcome::Ended);
        // Kodi calls a read that failed half way "the end" too.
        assert_eq!(verdict(None, &Signal::KodiStop { end: true }, exact(2_400.0, 6_700.0)).0, Outcome::Failed);
        assert_eq!(verdict(None, &Signal::Closed, exact(2_400.0, 6_700.0)).0, Outcome::Stopped);
    }

    #[test]
    fn a_catalogue_runtime_is_given_a_wider_margin_than_the_files_own() {
        let signal = Signal::EndOfFile;
        // A 120-minute runtime for a film that is 117 minutes long.
        let catalogue = Reached { position: 7_020.0, duration: Some(7_200.0), exact: false };
        assert_eq!(verdict(None, &signal, catalogue).0, Outcome::Ended);
        assert_eq!(verdict(None, &signal, Reached { exact: true, ..catalogue }).0, Outcome::Failed);
        let unknown = Reached { position: 30.0, duration: None, exact: false };
        assert_eq!(verdict(None, &signal, unknown).0, Outcome::Ended);
    }

    fn watch() -> WatchRef {
        WatchRef {
            kind: "movie".into(),
            id: "tt0000001".into(),
            video_id: None,
            name: "Film".into(),
            poster: None,
        }
    }

    fn rig() -> (Arc<Supervisor>, Arc<StdMutex<Vec<Effect>>>) {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let supervisor = Supervisor::new(move |effect| sink.lock().unwrap().push(effect));
        (Arc::new(supervisor), seen)
    }

    fn start(player: PlayerKind) -> Start {
        Start {
            player,
            watch: Some(watch()),
            session: Some("a306b91d".into()),
            duration: Some(6_700),
            position: 0,
        }
    }

    fn closings(effects: &[Effect]) -> Vec<(u64, bool)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Account(change) => Some((
                    change["timeMs"].as_u64().unwrap() / 1000,
                    change["closed"].as_bool().unwrap(),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_failure_is_written_as_progress_and_never_as_closed() {
        let (supervisor, effects) = rig();
        let film = supervisor.begin(start(PlayerKind::Here));
        let now = Instant::now();
        supervisor.observe(film, 2_400.0, Some(6_700.0), false, now);
        let finished = supervisor.finish(film, Signal::EndOfFile).unwrap();
        assert_eq!(finished.outcome, Outcome::Failed);
        let effects = effects.lock().unwrap();
        assert_eq!(closings(&effects).last(), Some(&(2_400, false)));
        assert!(effects.contains(&Effect::Release("a306b91d".into())));
    }

    #[test]
    fn a_stop_and_an_end_close_the_film_for_the_account() {
        let (supervisor, effects) = rig();
        let film = supervisor.begin(start(PlayerKind::Here));
        supervisor.observe(film, 600.0, Some(6_700.0), false, Instant::now());
        supervisor.intend(film, Intent::Stop);
        assert_eq!(supervisor.finish(film, Signal::Gone).unwrap().outcome, Outcome::Stopped);
        let film = supervisor.begin(start(PlayerKind::Here));
        supervisor.observe(film, 6_695.0, Some(6_700.0), false, Instant::now());
        assert_eq!(supervisor.finish(film, Signal::EndOfFile).unwrap().outcome, Outcome::Ended);
        let closed: Vec<_> = closings(&effects.lock().unwrap()).into_iter().filter(|(_, closed)| *closed).collect();
        assert_eq!(closed, vec![(600, true), (6_695, true)]);
    }

    #[test]
    fn a_handover_is_progress_and_the_film_goes_on_in_kodi() {
        let (supervisor, effects) = rig();
        let here = supervisor.begin(start(PlayerKind::Here));
        supervisor.observe(here, 1_200.0, Some(6_700.0), false, Instant::now());
        supervisor.intend(here, Intent::Handover);
        let watch = supervisor.watch(here);
        supervisor.finish(here, Signal::Gone);
        let kodi = supervisor.begin(Start { player: PlayerKind::Kodi, watch, session: None, duration: Some(6_700), position: 1_200 });
        assert_eq!(supervisor.current(PlayerKind::Kodi), Some(kodi));
        supervisor.observe(kodi, 6_698.0, Some(6_700.0), false, Instant::now());
        assert_eq!(supervisor.finish(kodi, Signal::KodiStop { end: true }).unwrap().outcome, Outcome::Ended);
        let changes = closings(&effects.lock().unwrap());
        assert!(changes.contains(&(1_200, false)));
        assert_eq!(changes.last(), Some(&(6_698, true)));
    }

    #[test]
    fn a_film_is_decided_once() {
        let (supervisor, _) = rig();
        let film = supervisor.begin(start(PlayerKind::Here));
        supervisor.intend(film, Intent::Stop);
        assert!(supervisor.finish(film, Signal::Gone).is_some());
        // mpv's own end-file arrives after the stop: nothing changes.
        let late = Signal::EndFile { reason: "error".into(), error: None };
        assert!(supervisor.finish(film, late).is_none());
        assert_eq!(supervisor.status()["finished"]["outcome"], "stopped");
        assert_eq!(supervisor.status()["film"], Value::Null);
    }

    #[test]
    fn a_new_film_replaces_the_one_playing() {
        let (supervisor, _) = rig();
        let first = supervisor.begin(start(PlayerKind::Here));
        let second = supervisor.begin(start(PlayerKind::Here));
        assert_ne!(first, second);
        assert_eq!(supervisor.status()["finished"]["film"], first);
        assert_eq!(supervisor.status()["finished"]["outcome"], "stopped");
        assert!(supervisor.finish(first, Signal::Gone).is_none());
    }

    #[test]
    fn the_account_hears_every_ninety_seconds_and_after_a_jump() {
        let (supervisor, effects) = rig();
        let film = supervisor.begin(start(PlayerKind::Here));
        let t0 = Instant::now();
        supervisor.observe(film, 0.0, Some(6_700.0), false, t0);
        supervisor.observe(film, 30.0, None, false, t0 + Duration::from_secs(30));
        assert_eq!(closings(&effects.lock().unwrap()).len(), 1);
        supervisor.observe(film, 91.0, None, false, t0 + Duration::from_secs(91));
        assert_eq!(closings(&effects.lock().unwrap()).len(), 2);
        // Left arrow held: eighty seconds back in a second.
        supervisor.observe(film, 10.0, None, false, t0 + Duration::from_secs(92));
        let effects = effects.lock().unwrap();
        let Effect::Account(last) = effects.last().unwrap() else { panic!() };
        assert_eq!(last["seek"], true);
        assert_eq!(last["timeMs"], 10_000);
    }

    #[test]
    fn nothing_is_written_for_a_film_the_account_does_not_know() {
        let (supervisor, effects) = rig();
        let film = supervisor.begin(Start { watch: None, ..start(PlayerKind::Here) });
        supervisor.observe(film, 10.0, Some(100.0), false, Instant::now());
        supervisor.finish(film, Signal::EndOfFile);
        assert_eq!(*effects.lock().unwrap(), vec![Effect::Release("a306b91d".into())]);
    }

    /// Kodi's own schema entry, as `xbmc/interfaces/json-rpc/schema/
    /// notifications.json` declares it and `JSONRPC.Introspect` returns it.
    const KODI_ON_STOP: &str = r#"{"id":1,"jsonrpc":"2.0","result":{"notifications":{"Player.OnStop":{
        "type":"notification","params":[{"name":"sender","type":"string","required":true},
        {"name":"data","type":"object","required":true,"properties":{
        "item":{"$ref":"Notifications.Item"},"end":{"type":"boolean","required":true}}}],"returns":null}}}}"#;

    #[test]
    fn kodis_schema_is_read_for_the_end_flag() {
        let schema: Value = serde_json::from_str(KODI_ON_STOP).unwrap();
        assert!(stop_carries_end(&schema));
        let mut without = schema.clone();
        without["result"]["notifications"]["Player.OnStop"]["params"][1]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("end");
        assert!(!stop_carries_end(&without));
    }

    #[test]
    fn kodis_notifications_arrive_back_to_back_and_in_pieces() {
        let mut buffer = br#"{"jsonrpc":"2.0","method":"Player.OnPause","params":{}}{"jsonrpc":"2.0","method":"Player.OnStop","params":{"data":{"end":tr"#.to_vec();
        let first = split_json(&mut buffer);
        assert_eq!(first.len(), 1);
        buffer.extend_from_slice(br#"ue,"item":{}},"sender":"xbmc"}}"#);
        let second = split_json(&mut buffer);
        assert_eq!(second[0].pointer("/params/data/end"), Some(&Value::Bool(true)));
        assert!(buffer.is_empty());
    }
}
