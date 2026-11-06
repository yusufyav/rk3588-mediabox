//! The receiver's path measured record by record (`mediabox_core::bintrace`,
//! format 2): `MEDIABOX_DVB_TRACE=<path>`, diagnosis only, off by default;
//! files `<path>.NNNN` of `MEDIABOX_DVB_TRACE_MB` (default 256) MB, at most
//! `MEDIABOX_DVB_TRACE_FILES` (default 16) of them, recording stopping
//! (counted) past that or with less than 1 GiB free. `finish` at the end of
//! the process closes it. Kinds:
//!
//! - 1 a USB transfer completed: id status, n seq, a bytes, b queued, c
//!   stream bytes so far.
//! - 2 a chunk fed to the selector: n packets, a when its transfer completed.
//! - 3 a PID of the service played, in that chunk: id PID, n packets, a
//!   payload unit starts, b the transfer's completion, c the packet's index
//!   in the stream.
//! - 4 a PCR, any PID: id PID, n flags (1 discontinuity_indicator), a the
//!   PCR (27 MHz), b the transfer's completion, c the packet's index.
//! - 5 a continuity error, any PID but the null one: id PID, n expected << 8
//!   | got, a the packet's index, b the transfer's completion.
//! - 6 queued for the reader: n bytes, b bytes queued then.
//! - 7 written to the reader: n bytes.
//! - 8 a reader opened the FIFO.
//! - 9 another service: id its id.
//! - 10 a PES start with a PTS on a PID of the service played: id PID, a the
//!   PTS (90 kHz), b the transfer's completion, c the packet's index.
//! - 11 a USB transfer submitted: id slot, n the seq it gets if accepted,
//!   b the libusb code (0 accepted).
//! - 12 a completion callback: id status, n seq, a entered (RAW ns), b left
//!   (RAW ns).
//! - 13 a chunk's packets: n packets, a sync byte errors, b TEI, c null
//!   packets, id continuity errors in it (all PIDs, capped at 65535).
//! - 20 a demodulator sample (`frontend`): id status (0 ok, 1 error, 2 over
//!   budget), n lock | FEC lock << 8 | acquisition retries << 16 | which of
//!   those and SNR, AGC were read << 24 (bits 24..28; 0xFF for one not read),
//!   a started (RAW ns; ended is the record's raw), b SNR dB x 100 (i16) |
//!   RF AGC << 16, c `frontend::UNSUPPORTED` << 48 | the sample's number.
//! - 21 the stream's own demodulator query: id 0 lock check, 1 signal
//!   status, 2 the no-transport check; n lock | FEC lock << 8 (0xFFFF
//!   failed), a started (RAW ns), b ns waited for another thread's
//!   transaction (`frontend::transaction`); ended is the record's raw.
//! - 30 a gap (`gaps`): id class (1 service, 2 multiplex), n its number, a
//!   start, b end (CLOCK_MONOTONIC ns), c its length (ns).
//! - 32 a multiplex gap seen while still open: n 0, a its start.
//! - 40 demodulator sampling started: a the interval (ns).
//! - 41 a stream (a tune's) started: id the service. Continuity and gaps
//!   start over here.

use std::sync::OnceLock;
use std::time::Duration;

use mediabox_core::bintrace::{Config, Summary, Trace};

static TRACE: OnceLock<Option<Trace>> = OnceLock::new();

fn env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub fn get() -> Option<&'static Trace> {
    TRACE
        .get_or_init(|| {
            let path = std::env::var("MEDIABOX_DVB_TRACE").ok().filter(|p| !p.is_empty())?;
            let config = Config {
                max_files: env_u64("MEDIABOX_DVB_TRACE_FILES", 16) as u32,
                ..Config::files(env_u64("MEDIABOX_DVB_TRACE_MB", 256) << 20)
            };
            Trace::with(std::path::Path::new(&path), config)
        })
        .as_ref()
}

/// Close the trace if there is one: its counters, the writer done or not.
pub fn finish() -> Option<Summary> {
    let trace = TRACE.get()?.as_ref()?;
    Some(trace.finish(Duration::from_secs(3)))
}
