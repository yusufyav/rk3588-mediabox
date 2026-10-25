//! A frame trace in vo_mediabox's own format (MEDIABOX_VO_TRACE), so the
//! player's side joins the interface's (MEDIABOX_TV_FRAME_TRACE) by frame id
//! on one clock: `<CLOCK_MONOTONIC µs> <event> <id> <value>`.
//!
//!   S id pts_us   sent to the interface, when the pipeline clock said it was due
//!   R id 0        given back by the interface
//!   D 0 pts_us    not sent: every buffer was out
//!   G gen 0       a channel is opened (its branch's generation)
//!   P gen pts_us  the first frame of that channel reached the sink, due
//!   M 0 pts_us    not sent: not after the frame shown before it
//!   X 0 pts_us    not sent: not a dma-buf the plane takes (a software decoder)

use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Trace(Option<Arc<Mutex<std::io::BufWriter<std::fs::File>>>>);

pub fn monotonic_us() -> u64 {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid clock id and a timespec on the stack.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as u64 * 1_000_000 + now.tv_nsec as u64 / 1000
}

impl Trace {
    pub fn from_env() -> Self {
        let Some(path) = std::env::var_os("MEDIABOX_VO_TRACE").filter(|path| !path.is_empty()) else {
            return Self(None);
        };
        match std::fs::File::create(&path) {
            Ok(file) => Self(Some(Arc::new(Mutex::new(std::io::BufWriter::new(file))))),
            Err(error) => {
                eprintln!("mediabox-live: {} açılamadı: {error}", path.to_string_lossy());
                Self(None)
            }
        }
    }

    pub fn event(&self, kind: char, id: u64, value: u64) {
        let Some(out) = &self.0 else { return };
        let mut out = out.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writeln!(out, "{} {kind} {id} {value}", monotonic_us());
        if kind == 'S' && id.is_multiple_of(64) {
            let _ = out.flush();
        }
    }

    pub fn flush(&self) {
        if let Some(out) = &self.0 {
            let _ = out.lock().unwrap_or_else(|p| p.into_inner()).flush();
        }
    }
}
