//! Where the viewer was, kept across the display changing hands.
//!
//! Starting a film hands the television to Kodi, and the control plane does
//! that by stopping this unit. Coming back should not mean starting again at
//! the top of the home screen — the viewer was three shelves down, on a title
//! they had just been reading about.
//!
//! The position is therefore written as it changes rather than on the way out.
//! A shutdown handler alone would not do: `PAMName=login` puts this process
//! outside the unit's own cgroup, the reaper escalates to SIGKILL when it has
//! waited long enough, and a signal that may never be delivered is not
//! somewhere to keep the only copy of anything. SIGTERM here is a last flush,
//! not the mechanism.
//!
//! Writes are debounced and atomic: a temporary file, fsync, rename. A half
//! written snapshot read at startup would be worse than none.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Long enough that walking along a shelf is one write rather than twenty,
/// short enough that what is lost to a SIGKILL is a keypress or two.
const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub screen: String,
    #[serde(default)]
    pub home_row: usize,
    #[serde(default)]
    pub home_col: usize,
    #[serde(default)]
    pub detail_kind: String,
    #[serde(default)]
    pub detail_id: String,
}

pub struct Store {
    tx: Sender<Message>,
    last: Snapshot,
}

enum Message {
    Put(Snapshot),
    Flush,
}

impl Store {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel::<Message>();

        std::thread::Builder::new()
            .name("mediabox-tv-state".into())
            .spawn(move || {
                let mut pending: Option<Snapshot> = None;
                loop {
                    // With something waiting, sleep out the debounce and write
                    // whatever the position ended up being. With nothing
                    // waiting, block: an idle interface does no work here.
                    let message = if pending.is_some() {
                        match rx.recv_timeout(DEBOUNCE) {
                            Ok(message) => Some(message),
                            Err(RecvTimeoutError::Timeout) => None,
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    } else {
                        match rx.recv() {
                            Ok(message) => Some(message),
                            Err(_) => break,
                        }
                    };

                    match message {
                        Some(Message::Put(snapshot)) => pending = Some(snapshot),
                        Some(Message::Flush) | None => {
                            if let Some(snapshot) = pending.take() {
                                write_atomically(&path, &snapshot);
                            }
                        }
                    }
                }

                if let Some(snapshot) = pending.take() {
                    write_atomically(&path, &snapshot);
                }
            })
            .expect("the state thread could not be started");

        Self { tx, last: Snapshot::default() }
    }

    /// Records a position. Identical positions cost nothing, which matters
    /// because this is called on every repaint.
    pub fn put(&mut self, snapshot: Snapshot) {
        if snapshot == self.last {
            return;
        }
        self.last = snapshot.clone();
        let _ = self.tx.send(Message::Put(snapshot));
    }

    pub fn flush(&self) {
        let _ = self.tx.send(Message::Flush);
    }
}

pub fn read(path: impl AsRef<Path>) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_atomically(path: &PathBuf, snapshot: &Snapshot) {
    let Ok(text) = serde_json::to_vec(snapshot) else { return };
    let temporary = path.with_extension("tmp");

    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&text)?;
        file.sync_all()
    })();

    if written.is_ok() {
        let _ = std::fs::rename(&temporary, path);
    } else {
        let _ = std::fs::remove_file(&temporary);
    }
}

/// Blocks the shutdown signals on every thread and waits for one on this thread
/// alone, so the last position is written by ordinary code rather than from a
/// signal handler, where almost nothing is safe to call.
pub fn on_shutdown(then: impl Fn() + Send + 'static) {
    // SAFETY: the set is initialised before use and only these two signals are
    // touched. Blocking them here, before any other thread exists, is what
    // makes them arrive at the sigwait below rather than killing the process.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());

        std::thread::Builder::new()
            .name("mediabox-tv-exit".into())
            .spawn(move || {
                let mut signal: libc::c_int = 0;
                if libc::sigwait(&set, &mut signal) == 0 {
                    eprintln!("mediabox-tv.exit signal={signal}");
                }
                then();
                // The interface is not asked to unwind: whatever takes the
                // display next is already being started, and the reaper is
                // waiting for this process to be gone.
                std::process::exit(0);
            })
            .expect("the shutdown thread could not be started");
    }
}
