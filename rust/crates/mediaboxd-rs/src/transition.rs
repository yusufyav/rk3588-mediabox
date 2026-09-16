//! One display transition at a time, visible from outside the daemon.
//!
//! Two things need to know that the display is changing hands, and only one of
//! them is in this process.
//!
//! Inside, `SurfaceManager` and `ApplicationManager` both stop the incumbent
//! and start a successor. They used to carry a private `Mutex` each, which
//! serialised each manager against itself and against nothing else: a surface
//! switch and an application launch could run at the same time, stop each
//! other's target and leave the television on neither.
//!
//! Outside, `mediabox-display-guard` runs as kodi.service's `ExecStopPost` and
//! has to answer one question before it does anything: did somebody ask for
//! this, or did Kodi just end? When the control plane stopped Kodi it is
//! already starting the successor, and a second request from in there is the
//! deadlock `b34a48a` fixed. When Kodi ended by itself nobody is coming, and
//! the interface has to be put back at once.
//!
//! An advisory `flock` on a file under `/run` answers both. It is held for
//! exactly as long as a transition runs, any process can test it without
//! blocking, and it cannot be left behind:
//!
//!   * the kernel drops it when the last descriptor closes, so a daemon that
//!     is killed mid-transition releases it as it dies rather than leaving the
//!     appliance believing a switch is forever in progress;
//!   * `/run` is a tmpfs, so a power cut leaves no file at all.
//!
//! Nothing here names a board. The lock says "a transition is running", which
//! is true of every appliance this daemon runs on.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};

/// Where the lock lives. Under `/run`, which is why it cannot outlive a boot.
pub const LOCK_PATH: &str = "/run/mediabox/display-transition.lock";

/// The one gate every change of display ownership passes through.
#[derive(Clone)]
pub struct DisplayTransition {
    path: Arc<PathBuf>,
    gate: Arc<Mutex<()>>,
}

impl DisplayTransition {
    pub fn new() -> Self {
        Self::at(Path::new(LOCK_PATH))
    }

    /// The same thing somewhere else, so a test does not need `/run`.
    pub fn at(path: &Path) -> Self {
        Self {
            path: Arc::new(path.to_path_buf()),
            gate: Arc::new(Mutex::new(())),
        }
    }

    /// Claim the display for the duration of one handover.
    ///
    /// The in-process mutex is what actually serialises the two managers; the
    /// file lock is how anything else finds out. Holding the mutex first means
    /// the file lock is never contended from within this process, so asking
    /// for it without blocking is not a gamble.
    pub async fn begin(&self) -> Transition<'_> {
        let gate = self.gate.lock().await;
        Transition {
            _file: self.claim(),
            _gate: gate,
        }
    }

    /// Whether a transition is running, asked without waiting for one.
    ///
    /// Test-only in this process — the guard script asks the same question of
    /// the same file with `flock -n` — but it is what the tests assert on.
    pub fn in_progress(&self) -> bool {
        held(&self.path)
    }

    fn claim(&self) -> Option<std::fs::File> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.as_path())
            .ok()?;
        // SAFETY: a live descriptor from the File above, which outlives the
        // call. LOCK_NB cannot block the runtime, and the mutex we already
        // hold means the only way this fails is a broken /run -- in which case
        // the transition still runs, correctly, just unobservably.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        locked.then_some(file)
    }
}

impl Default for DisplayTransition {
    fn default() -> Self {
        Self::new()
    }
}

/// A transition in progress. Dropping it ends the transition: the descriptor
/// closes, which is what releases the lock, and the mutex is given back.
pub struct Transition<'a> {
    // Dropped in declaration order: the lock goes before the mutex, so nobody
    // can see "no transition" while the next one is still waiting to start.
    _file: Option<std::fs::File>,
    _gate: MutexGuard<'a, ()>,
}

/// Is somebody holding the lock on this path?
fn held(path: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
    else {
        return false;
    };
    // SAFETY: a live descriptor from the File above.
    let got_it = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if got_it {
        // SAFETY: same descriptor, still open.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
    !got_it
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("mb-transition-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        directory.join(name)
    }

    #[tokio::test]
    async fn a_transition_is_visible_while_it_runs_and_gone_after() {
        let path = scratch("visible.lock");
        let transition = DisplayTransition::at(&path);

        assert!(
            !transition.in_progress(),
            "nothing is switching before anything asked"
        );
        {
            let _running = transition.begin().await;
            assert!(
                transition.in_progress(),
                "the guard has to be able to see a handover it did not start"
            );
        }
        assert!(
            !transition.in_progress(),
            "a finished transition must not look like a running one"
        );
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn two_handovers_cannot_run_at_once() {
        let path = scratch("serial.lock");
        let transition = DisplayTransition::at(&path);
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let first = transition.begin().await;
        order.lock().await.push("first-in");

        let second = {
            let transition = transition.clone();
            let order = Arc::clone(&order);
            tokio::spawn(async move {
                let _running = transition.begin().await;
                order.lock().await.push("second-in");
            })
        };

        // The second handover cannot have started: the first still holds it.
        tokio::task::yield_now().await;
        assert_eq!(order.lock().await.as_slice(), ["first-in"]);

        order.lock().await.push("first-out");
        drop(first);
        second.await.unwrap();

        assert_eq!(
            order.lock().await.as_slice(),
            ["first-in", "first-out", "second-in"],
            "a surface switch and an application launch must not interleave"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_dead_daemon_does_not_leave_a_transition_behind() {
        // The lock lives on a descriptor, so the proof is that closing it --
        // which is all that happens when a process dies -- clears the state.
        let path = scratch("dead.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(held(&path), "a held lock reads as a transition");
        drop(file);
        assert!(
            !held(&path),
            "the appliance cannot be left believing a switch is forever in progress"
        );
        std::fs::remove_file(&path).ok();
    }
}
