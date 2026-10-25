//! The control socket, in the JSON IPC the control plane speaks to its
//! players (mpv's, which films are played with).
//!
//! What the control plane asks of live television
//! (rust/crates/mediaboxd-rs/src/player.rs and playback.rs): `loadfile` for a
//! channel opened in the running player, `get_property` for where it is and
//! whether it shows the channel, `observe_property eof-reached`, `aid`,
//! `volume`/`mute`, `audio-delay`, `quit`. Only what is done is answered
//! "success": a property this player does not have is "property
//! unavailable", a command it does not carry out an error, and a channel that
//! cannot be opened answers with why.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{Sender, SyncSender};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// What a client asked that the engine must do on its own thread.
pub enum Command {
    Load(String),
    /// The channel cannot be played: the player ends, saying why.
    Fail(String),
    Pause(bool),
    Aid(Option<usize>),
    Volume { gain: Option<f64>, muted: Option<bool> },
    AudioDelay(f64),
    Quit,
}

/// Answers the engine gives without a round trip: read from shared state by
/// the closure the caller hands in.
pub type Query = Arc<dyn Fn(&str) -> Option<Value> + Send + Sync>;

#[derive(Default)]
pub struct Clients {
    writers: Mutex<Vec<(u64, std::sync::mpsc::Sender<String>)>>,
    observers: Mutex<Vec<(u64, Value, String)>>,
}

impl Clients {
    /// An event to every client, as mpv sends one.
    pub fn event(&self, event: Value) {
        let line = format!("{event}\n");
        self.writers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(_, writer)| writer.send(line.clone()).is_ok());
    }

    /// A watched property changed: told to whoever observes it.
    pub fn changed(&self, name: &str, data: Value) {
        let observers = self.observers.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let writers = self.writers.lock().unwrap_or_else(|p| p.into_inner()).clone();
        for (client, id, watched) in observers {
            if watched != name {
                continue;
            }
            if let Some((_, writer)) = writers.iter().find(|(owner, _)| *owner == client) {
                let _ = writer.send(format!("{}\n", json!({"event": "property-change", "id": id, "name": name, "data": data})));
            }
        }
    }
}

/// Commands to the engine's thread, each with where to say it is done.
pub type Commands = Sender<(Command, Option<SyncSender<Result<(), String>>>)>;

pub fn serve(path: &str, clients: Arc<Clients>, query: Query, commands: Commands) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|e| format!("{path}: {e}"))?;
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for (client, stream) in (1u64..).zip(listener.incoming().flatten()) {
                let (clients, query, commands) = (Arc::clone(&clients), Arc::clone(&query), commands.clone());
                let _ = std::thread::Builder::new()
                    .name(format!("ipc-{client}"))
                    .spawn(move || serve_client(client, stream, &clients, &query, &commands));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn serve_client(client: u64, stream: UnixStream, clients: &Clients, query: &Query, commands: &Commands) {
    let Ok(mut writer) = stream.try_clone() else { return };
    let (out, lines) = std::sync::mpsc::channel::<String>();
    clients.writers.lock().unwrap_or_else(|p| p.into_inner()).push((client, out.clone()));
    let pump = std::thread::spawn(move || {
        for line in lines {
            if writer.write_all(line.as_bytes()).is_err() {
                return;
            }
        }
    });
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let request_id = message.get("request_id").cloned();
        let command = message.get("command").and_then(Value::as_array).cloned().unwrap_or_default();
        let (error, data) = handle(client, &command, clients, query, commands);
        let mut answer = json!({"error": error});
        if let Some(data) = data {
            answer["data"] = data;
        }
        if let Some(id) = request_id {
            answer["request_id"] = id;
        }
        if out.send(format!("{answer}\n")).is_err() {
            break;
        }
    }
    clients.writers.lock().unwrap_or_else(|p| p.into_inner()).retain(|(owner, _)| *owner != client);
    clients.observers.lock().unwrap_or_else(|p| p.into_inner()).retain(|(owner, _, _)| *owner != client);
    drop(out);
    let _ = pump.join();
}

const FAILED: &str = "error running command";

/// Send a command to the engine and wait for it to be done, so an answer of
/// "success" means it happened; a failure is answered with its reason.
fn run(commands: &Commands, command: Command) -> String {
    let (done, wait) = std::sync::mpsc::sync_channel(1);
    if commands.send((command, Some(done))).is_err() {
        return FAILED.into();
    }
    match wait.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => "success".into(),
        Ok(Err(why)) => why,
        Err(_) => FAILED.into(),
    }
}

fn handle(client: u64, command: &[Value], clients: &Clients, query: &Query, commands: &Commands) -> (String, Option<Value>) {
    let word = |at: usize| command.get(at).and_then(Value::as_str).unwrap_or_default();
    match word(0) {
        "get_property" | "get_property_string" => match query(word(1)) {
            Some(value) => ("success".into(), Some(value)),
            None => ("property unavailable".into(), None),
        },
        "observe_property" | "observe_property_string" => {
            let id = command.get(1).cloned().unwrap_or(Value::Null);
            let name = word(2).to_string();
            clients.observers.lock().unwrap_or_else(|p| p.into_inner()).push((client, id.clone(), name.clone()));
            // mpv sends the current value at once.
            clients.changed(&name, query(&name).unwrap_or(Value::Null));
            ("success".into(), None)
        }
        "unobserve_property" => {
            let id = command.get(1).cloned().unwrap_or(Value::Null);
            clients
                .observers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .retain(|(owner, watched, _)| !(*owner == client && *watched == id));
            ("success".into(), None)
        }
        "loadfile" => match word(1) {
            "" => ("invalid parameter".into(), None),
            url => (run(commands, Command::Load(url.trim_start_matches("file://").to_string())), None),
        },
        "set_property" | "set_property_string" => {
            let value = command.get(2).cloned().unwrap_or(Value::Null);
            let number = || value.as_f64().or_else(|| value.as_str().and_then(|text| text.parse().ok()));
            let flag = || value.as_bool().or_else(|| value.as_str().map(|text| text == "yes"));
            match word(1) {
                "pause" => match flag() {
                    Some(paused) => (run(commands, Command::Pause(paused)), None),
                    None => ("invalid parameter".into(), None),
                },
                // mpv numbers tracks from 1; "no" is none, "auto" the first.
                "aid" => {
                    let aid = match &value {
                        Value::String(text) if text == "no" => None,
                        Value::Bool(false) => None,
                        Value::String(text) if text == "auto" => Some(0),
                        _ => match number() {
                            Some(id) if id >= 1.0 => Some(id as usize - 1),
                            _ => return ("invalid parameter".into(), None),
                        },
                    };
                    (run(commands, Command::Aid(aid)), None)
                }
                "volume" => match number() {
                    // mpv's volume curve is cubic.
                    Some(volume) => (run(commands, Command::Volume { gain: Some((volume / 100.0).max(0.0).powi(3)), muted: None }), None),
                    None => ("invalid parameter".into(), None),
                },
                "mute" => match flag() {
                    Some(muted) => (run(commands, Command::Volume { gain: None, muted: Some(muted) }), None),
                    None => ("invalid parameter".into(), None),
                },
                "audio-delay" => match number() {
                    Some(seconds) => (run(commands, Command::AudioDelay(seconds)), None),
                    None => ("invalid parameter".into(), None),
                },
                _ => ("property unavailable".into(), None),
            }
        }
        "cycle" if word(1) == "pause" => {
            let paused = query("pause").and_then(|value| value.as_bool()).unwrap_or(false);
            (run(commands, Command::Pause(!paused)), None)
        }
        "quit" => {
            let _ = commands.send((Command::Quit, None));
            ("success".into(), None)
        }
        _ => ("invalid parameter".into(), None),
    }
}
