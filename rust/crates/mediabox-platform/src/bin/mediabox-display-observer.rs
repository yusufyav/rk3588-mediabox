//! `mediabox-display-observer` -- the display, watched from outside whoever
//! owns it, in shadow mode. See `mediabox_platform::observer`.
//!
//!   mediabox-display-observer                 watch, publish, compare
//!   mediabox-display-observer --once          one look, printed, then exit
//!
//! Options: `--runtime <dir>` (default /run/mediabox-display-observer),
//! `--no-drm-query` (never read the kernel's mode list, even under AM-2),
//! `--no-compare` (do not ask the control plane what it believes).

use std::path::PathBuf;

use mediabox_platform::drm_query::{Access, System};
use mediabox_platform::observer::{self, Observer, Reason, Uevents, Wake};
use mediabox_platform::{Platform, Roots};

const TRANSITION_LOCK: &str = "/run/mediabox/display-transition.lock";
const DAEMON_SOCKET: &str = "/run/mediabox/mediaboxd.sock";

extern "C" fn on_signal(_: libc::c_int) {
    observer::request_stop();
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let value = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    if flag("--help") || flag("-h") {
        eprintln!(
            "kullanım: mediabox-display-observer [--once] [--runtime <dizin>] [--no-drm-query] [--no-compare]"
        );
        return std::process::ExitCode::SUCCESS;
    }
    let runtime = value("--runtime")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/mediabox-display-observer"));
    let mut observer = Observer::new(Roots::from_env(), runtime);
    if !flag("--no-compare") {
        observer.daemon_socket = Some(PathBuf::from(DAEMON_SOCKET));
    }
    if !flag("--no-drm-query") {
        observer.drm = Some(Box::new(|platform: &Platform| -> Box<dyn Access> {
            Box::new(System {
                transition_lock: PathBuf::from(TRANSITION_LOCK),
                clients: platform
                    .debugfs
                    .dir()
                    .map(|dir| dir.join("clients"))
                    .unwrap_or_default(),
                primary: platform
                    .kms
                    .as_ref()
                    .map(|node| node.device.clone())
                    .unwrap_or_default(),
            })
        }));
    }

    if flag("--once") {
        let snapshot = observer.reconcile(Reason::Startup);
        println!("{}", serde_json::to_string_pretty(&snapshot).unwrap_or_default());
        return std::process::ExitCode::SUCCESS;
    }

    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
    eprintln!(
        "mediabox-display-observer: shadow mode; publishing {}, looking every {:?} and on DRM uevents",
        observer.snapshot_path().display(),
        observer::PERIOD
    );
    // The uevent socket is a hint and nothing more: without it the period
    // alone keeps the snapshot right.
    let events = match Uevents::open() {
        Ok(events) => Some(events),
        Err(error) => {
            eprintln!("mediabox-display-observer: no uevent socket ({error}); periodic only");
            None
        }
    };
    observer::run(&mut observer, |period| match &events {
        Some(events) => events.wait(period),
        None => {
            let deadline = std::time::Instant::now() + period;
            while std::time::Instant::now() < deadline {
                if observer::stop_requested() {
                    return Wake::Stop;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Wake::Timeout
        }
    });
    eprintln!("mediabox-display-observer: stopped");
    std::process::ExitCode::SUCCESS
}
