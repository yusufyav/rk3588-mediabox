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
//! The lock is taken exclusively, and only a transition takes it that way.
//! `mediabox-display-observer` takes it *shared*, for the few milliseconds it
//! has the display device open to read a mode list: that keeps a transition
//! from starting while the node is open, and it is not a transition -- the
//! guard asks with a shared lock of its own, gets it, and puts the interface
//! back after a Kodi that ended by itself even while the observer is reading.
//! A transition that finds the observer there waits those milliseconds out.
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
    /// Told that the display is about to change hands, before it does: a
    /// display setting on trial belongs to the owner it was sent to.
    on_begin: Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>,
}

impl DisplayTransition {
    /// The gate at [`LOCK_PATH`], with the file made now: the observer
    /// opens it read-only under a read-only `/run` and cannot make it, and
    /// until it exists it cannot tell "no transition" from "no gate" and
    /// does not read the display's mode list.
    pub fn new() -> Self {
        let gate = Self::at(Path::new(LOCK_PATH));
        let _ = gate.path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(gate.path.as_path());
        gate
    }

    /// The same thing somewhere else, so a test does not need `/run`.
    pub fn at(path: &Path) -> Self {
        Self {
            path: Arc::new(path.to_path_buf()),
            gate: Arc::new(Mutex::new(())),
            on_begin: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Who to tell when a transition begins. Once; a second call is ignored.
    pub fn on_begin(&self, hook: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_begin.set(Box::new(hook));
    }

    /// Claim the display for the duration of one handover.
    ///
    /// The in-process mutex is what actually serialises the two managers; the
    /// file lock is how anything else finds out. Holding the mutex first means
    /// the file lock is never contended from within this process -- only by
    /// the observer's shared hold, which lasts milliseconds and is waited for.
    pub async fn begin(&self) -> Transition<'_> {
        let gate = self.gate.lock().await;
        if let Some(hook) = self.on_begin.get() {
            hook();
        }
        Transition {
            _file: self.claim().await,
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

    async fn claim(&self) -> Option<std::fs::File> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.as_path())
            .ok()?;
        // Without blocking the runtime: the mutex we already hold means the
        // only holders left are the observer's shared read, gone in
        // milliseconds, and `mediabox-display-changed`, which is a recovery
        // in its own right and is waited for as long as it reasonably takes.
        // Past that the transition still runs, correctly, just unobservably.
        for _ in 0..3000 {
            // SAFETY: a live descriptor from the File above.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Some(file);
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EWOULDBLOCK) {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        eprintln!("mediaboxd-rs: display transition lock still held after 30 s; going on without it");
        None
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

/// Is a transition holding the lock on this path? Asked the way the guard
/// asks it (`flock -n -s`): a shared hold -- the observer reading a mode
/// list -- is not one.
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
    let got_it = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0;
    if got_it {
        // SAFETY: same descriptor, still open.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
    !got_it
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether the lock is let go, allowing for the other tests of this
    /// binary: one that forks a command at the moment this one closes its
    /// descriptor hands the child a copy, which holds the lock until the
    /// child's `exec` closes it (`O_CLOEXEC`). Microseconds; never forever.
    fn released(path: &Path) -> bool {
        (0..400).any(|_| {
            if held(path) {
                std::thread::sleep(std::time::Duration::from_millis(5));
                false
            } else {
                true
            }
        })
    }

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
            released(&path),
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

    #[tokio::test]
    async fn the_observers_shared_hold_is_not_a_transition_and_is_waited_out() {
        let path = scratch("shared.lock");
        let transition = DisplayTransition::at(&path);
        let observer = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(observer.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) },
            0
        );
        // Kodi ends by itself while the observer is reading: the guard's
        // question must not be answered "a handover is running".
        assert!(!transition.in_progress());
        // A real handover waits the few milliseconds out and then holds it.
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(observer);
        });
        let running = transition.begin().await;
        assert!(transition.in_progress(), "the handover holds it once the read is done");
        release.await.unwrap();
        drop(running);
        assert!(released(&path));
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_handover_is_announced_before_it_runs() {
        let path = scratch("announce.lock");
        let transition = DisplayTransition::at(&path);
        let told = Arc::new(std::sync::atomic::AtomicU32::new(0));
        transition.on_begin({
            let told = Arc::clone(&told);
            move || {
                told.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        drop(transition.clone().begin().await);
        drop(transition.begin().await);
        assert_eq!(told.load(std::sync::atomic::Ordering::SeqCst), 2);
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
            released(&path),
            "the appliance cannot be left believing a switch is forever in progress"
        );
        std::fs::remove_file(&path).ok();
    }
}
