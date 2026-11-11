//! One timeline for live television, across its processes.
//!
//! The control plane, the receiver, the player and the interface each write
//! their own file, all on CLOCK_MONOTONIC in nanoseconds -- the clock of the
//! kernel's vblank timestamps and of the player's due times -- so that
//! `tools/live-trace.py analyze` can lay a channel request, the tune, the
//! access point, every frame's PTS, its commit and the vblank it was scanned
//! out at on one line.
//!
//! On while the directory exists (`/run/mediabox-trace`, or
//! `MEDIABOX_TRACE_DIR`), looked at every quarter second by a thread of its
//! own in each process:
//!
//! ```text
//! mkdir /run/mediabox-trace                         # on, within 250 ms
//! mv /run/mediabox-trace /run/mediabox-trace.done   # off: every file
//! sleep 1                                           # flushed and closed
//! ```
//!
//! Turned off, a process flushes and closes its file (it is in the renamed
//! directory, complete); turned on again, it starts a new one. A directory
//! put back in place of the old one under the same name is a new one (its
//! inode is compared), so nothing is written into a directory taken away.
//! Each file is `<component>-<pid>-<n>.trace`:
//!
//! ```text
//! #mbtrace 2 <component> run=<boot id>:<pid>:<start ns> pid=<pid> start=<start ns>
//! <CLOCK_MONOTONIC ns> <event> key=value ...
//! ```
//!
//! `run` names this process for the whole boot, and is what makes an id
//! local to a process (a frame's) unique: the pair (run, id).

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const VERSION: u32 = 2;
pub const DIR: &str = "/run/mediabox-trace";
/// How often the directory is looked at, and the file flushed.
pub const TICK: Duration = Duration::from_millis(250);

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: i32, out: *mut Timespec) -> i32;
}

const CLOCK_MONOTONIC: i32 = 1;

/// CLOCK_MONOTONIC, nanoseconds.
pub fn now_ns() -> u64 {
    let mut now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid clock id and a timespec on the stack (64-bit Linux:
    // time_t and long are both 64 bits).
    unsafe { clock_gettime(CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

static ON: AtomicBool = AtomicBool::new(false);
static OUT: Mutex<Option<Writer>> = Mutex::new(None);
static STARTED: Mutex<Option<Process>> = Mutex::new(None);

struct Process {
    component: &'static str,
    run: String,
    pid: u32,
    start: u64,
    files: u32,
}

struct Writer {
    out: std::io::BufWriter<std::fs::File>,
    /// The directory it was opened in: (device, inode).
    dir: (u64, u64),
}

fn dir() -> std::path::PathBuf {
    std::env::var_os("MEDIABOX_TRACE_DIR")
        .filter(|d| !d.is_empty())
        .map_or_else(|| DIR.into(), Into::into)
}

/// Name this process on the timeline and start looking for the directory.
/// Called once, first thing; later calls do nothing.
pub fn init(component: &'static str) {
    {
        let mut started = STARTED.lock().unwrap_or_else(|p| p.into_inner());
        if started.is_some() {
            return;
        }
        let pid = std::process::id();
        let start = now_ns();
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        let boot: String = boot.trim().chars().filter(|c| *c != '-').take(12).collect();
        *started = Some(Process {
            component,
            run: format!("{boot}:{pid}:{start}"),
            pid,
            start,
            files: 0,
        });
    }
    tick();
    let _ = std::thread::Builder::new()
        .name("timeline".into())
        .spawn(|| {
            loop {
                std::thread::sleep(TICK);
                tick();
            }
        });
}

/// This process on the timeline (`run=` of its files), once `init` has run.
pub fn run() -> Option<String> {
    STARTED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|p| p.run.clone())
}

/// Look at the directory: open a file in it, close the one in a directory
/// gone, flush what is written.
pub fn tick() {
    let mut out = OUT.lock().unwrap_or_else(|p| p.into_inner());
    let here = std::fs::metadata(dir())
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .and_then(|m| dir_id(&m));
    match (out.as_mut(), here) {
        (Some(writer), Some(here)) if writer.dir == here => {
            let _ = writer.out.flush();
        }
        (Some(_), _) => {
            ON.store(false, Ordering::SeqCst);
            if let Some(mut writer) = out.take() {
                let _ = writer.out.flush();
            }
            // A new directory under the name: a new file, at once.
            if let Some(here) = here {
                *out = open(here);
            }
        }
        (None, Some(here)) => *out = open(here),
        (None, None) => {}
    }
    ON.store(out.is_some(), Ordering::SeqCst);
}

/// The directory's identity, its device and inode: a new directory under
/// the name is another one. Unix has it; elsewhere (the web interface's
/// wasm32) there is none, and the timeline stays off.
#[cfg(unix)]
fn dir_id(m: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn dir_id(_: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

fn open(dir_id: (u64, u64)) -> Option<Writer> {
    let mut started = STARTED.lock().unwrap_or_else(|p| p.into_inner());
    let process = started.as_mut()?;
    process.files += 1;
    let name = format!(
        "{}-{}-{}.trace",
        process.component, process.pid, process.files
    );
    let file = std::fs::File::create(dir().join(name)).ok()?;
    let mut out = std::io::BufWriter::new(file);
    let _ = writeln!(
        out,
        "#mbtrace {VERSION} {} run={} pid={} start={}",
        process.component, process.run, process.pid, process.start
    );
    Some(Writer { out, dir: dir_id })
}

/// Whether events are written now: one atomic read.
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// One event, `fields` as `key=value` pairs separated by spaces.
pub fn write(event: &str, fields: std::fmt::Arguments<'_>) {
    let at = now_ns();
    let mut out = OUT.lock().unwrap_or_else(|p| p.into_inner());
    let Some(writer) = out.as_mut() else { return };
    let _ = writeln!(writer.out, "{at} {event} {fields}");
}

/// What is written so far, to the file: before the process ends.
pub fn flush() {
    if let Some(writer) = OUT.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        let _ = writer.out.flush();
    }
}

/// `timeline!("event", "key={} key={}", a, b)`: nothing is formatted unless
/// the timeline is on.
#[macro_export]
macro_rules! timeline {
    ($event:expr) => {
        if $crate::timeline::enabled() {
            $crate::timeline::write($event, format_args!(""));
        }
    };
    ($event:expr, $($fields:tt)+) => {
        if $crate::timeline::enabled() {
            $crate::timeline::write($event, format_args!($($fields)+));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_is_monotonic_nanoseconds() {
        let a = now_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = now_ns();
        assert!(b - a >= 2_000_000 && b - a < 1_000_000_000, "{a} {b}");
    }

    fn lines(path: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// The commands in the module's documentation, as they are: mkdir turns
    /// it on, mv turns it off with the file complete, and a directory made
    /// again gets a new file. One test: the timeline is the process's.
    #[test]
    fn mkdir_turns_it_on_and_mv_turns_it_off_with_the_file_complete() {
        let base = std::env::temp_dir().join(format!("mbtrace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let on = base.join("trace");
        // SAFETY: the only test of this crate that reads the variable.
        unsafe { std::env::set_var("MEDIABOX_TRACE_DIR", &on) };
        init("test");
        let pid = std::process::id();
        let wait = || std::thread::sleep(TICK * 3);

        assert!(!enabled(), "off with no directory");
        crate::timeline!("lost", "n=0");

        std::fs::create_dir(&on).unwrap();
        wait();
        assert!(enabled(), "on after mkdir");
        crate::timeline!("kept", "n=1");
        crate::timeline!("kept", "n=2");

        let done = base.join("trace.done");
        std::fs::rename(&on, &done).unwrap();
        wait();
        assert!(!enabled(), "off after mv");
        crate::timeline!("lost", "n=3");
        let first = lines(&done.join(format!("test-{pid}-1.trace")));
        assert!(
            first[0].starts_with("#mbtrace 2 test run="),
            "{first:?}"
        );
        assert!(first[0].contains(&format!(":{pid}:")), "{first:?}");
        let events: Vec<&str> = first[1..]
            .iter()
            .map(|l| l.split_whitespace().nth(2).unwrap())
            .collect();
        assert_eq!(events, ["n=1", "n=2"], "complete, and nothing after off");

        // The same name made again: a new directory, a new file.
        std::fs::create_dir(&on).unwrap();
        wait();
        assert!(enabled());
        crate::timeline!("kept", "n=4");
        std::fs::rename(&on, base.join("trace.again")).unwrap();
        wait();
        let second = lines(&base.join("trace.again").join(format!("test-{pid}-2.trace")));
        assert_eq!(second.len(), 2, "{second:?}");
        assert!(second[1].ends_with("kept n=4"));
        assert_eq!(
            lines(&done.join(format!("test-{pid}-1.trace"))).len(),
            3,
            "the first file untouched"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
