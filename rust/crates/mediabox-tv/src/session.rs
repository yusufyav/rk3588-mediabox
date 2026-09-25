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

        Self {
            tx,
            last: Snapshot::default(),
        }
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
    let Ok(text) = serde_json::to_vec(snapshot) else {
        return;
    };
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

/// Asks for a picture of what is on the television.
///
/// SIGUSR1, because the appliance has no other way to say "now" to a process
/// that owns the display: a screenshot tool would need DRM master, and there is
/// only one of those. The handler writes the next drawn frame to a file, which
/// is what the acceptance evidence is made of.
pub fn on_snapshot_request(then: impl Fn() + Send + 'static) {
    // SAFETY: one signal, this process's own, and the set is initialised by
    // libc's own calls before it is read.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());

        std::thread::Builder::new()
            .name("mediabox-tv-snap".into())
            .spawn(move || {
                loop {
                    let mut signal: libc::c_int = 0;
                    if libc::sigwait(&set, &mut signal) != 0 {
                        return;
                    }
                    then();
                }
            })
            .expect("the snapshot thread could not be started");
    }
}

/// The two signals the appliance is stopped with.
///
/// SAFETY: the set is zeroed and initialised through libc's own calls before
/// anything reads it.
unsafe fn exit_signals() -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        // Blocked here as well as in on_snapshot_request, and for the same
        // reason the other two are: a thread inherits the mask of whoever
        // spawned it, and SIGUSR1's default action is to kill the process.
        libc::sigaddset(&mut set, libc::SIGUSR1);
        set
    }
}

/// Blocks the shutdown signals on this thread, and must be the first thing
/// `main` does.
///
/// A thread inherits the mask of the thread that spawned it, and a
/// process-directed signal is delivered to any one thread that has it
/// unblocked. Blocking only at the point [`on_shutdown`] is called is therefore
/// too late: by then the image workers, the state store, tokio and the Mali
/// driver's own threads all exist with the signal unblocked, and SIGTERM lands
/// on one of them and takes the default action.
///
/// Measured on the appliance, with the mask taken late: every worker thread in
/// /proc/<pid>/task/*/status carried SigBlk 0000000000000000 against the main
/// thread's 0000000000004002, and the process exited 143 — killed — rather than
/// running the handler below. The television's display was then released only
/// because the kernel closes the DRM device with the process, which is the
/// thing the platform's own release path exists to not depend on.
pub fn block_exit_signals() {
    // SAFETY: only the two signals above are touched, and only this thread's
    // mask is changed.
    unsafe {
        let set = exit_signals();
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Has somebody asked this process to stop, before there is anybody to hear it?
///
/// [`block_exit_signals`] makes SIGTERM and SIGINT wait in the pending set
/// until [`on_shutdown`]'s thread calls `sigwait`. That thread does not exist
/// until the interface is running, and the interface does not start running
/// until there is a television to run on -- so between those two points the
/// process is deaf to `systemctl stop`. Code that waits in that window asks
/// this, and leaves.
pub fn exit_was_asked() -> bool {
    // SAFETY: the set is initialised by `sigpending` before it is read, and
    // nothing here changes the process's disposition or mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        if libc::sigpending(&mut set) != 0 {
            return false;
        }
        libc::sigismember(&set, libc::SIGTERM) == 1 || libc::sigismember(&set, libc::SIGINT) == 1
    }
}

/// Waits for one of the shutdown signals on a thread of its own, so the last
/// position is written by ordinary code rather than from a signal handler,
/// where almost nothing is safe to call.
///
/// [`block_exit_signals`] must already have run; it is called again here so the
/// invariant holds for a caller that uses this alone.
pub fn on_shutdown(then: impl Fn() + Send + 'static) {
    block_exit_signals();

    // SAFETY: the set is initialised before use and only these signals are
    // touched.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);

        std::thread::Builder::new()
            .name("mediabox-tv-exit".into())
            .spawn(move || {
                let mut signal: libc::c_int = 0;
                if libc::sigwait(&set, &mut signal) == 0 {
                    eprintln!("mediabox-tv.exit signal={signal}");
                }
                then();
                // Ask the platform loop to unwind normally. The split-KMS
                // platform disables its CRTC, removes imported framebuffers,
                // closes PRIME handles and drops DRM master before the unit
                // becomes inactive, so Kodi never races a process that is
                // still releasing the display.
                let _ = slint::quit_event_loop();
            })
            .expect("the shutdown thread could not be started");
    }
}
