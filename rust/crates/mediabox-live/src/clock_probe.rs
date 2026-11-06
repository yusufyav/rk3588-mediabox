//! The clock chain, measured: diagnosis only, off unless
//! `MEDIABOX_LIVE_CLOCK_PROBE=<file>` names where its lines go. Off, every
//! call is one load of a `OnceLock` and nothing else.
//!
//! Nothing is written per packet: each call adds to the current second's
//! record in memory, and `flush` (the health thread, once a second) writes
//! that second as one JSON line. Each series keeps its first sample of the
//! second -- enough for a regression over a 30 or 60 s window -- and its
//! extremes and sums.
//!
//! Clocks, by name in the lines:
//! - `raw`: CLOCK_MONOTONIC_RAW, the crystal not slewed by NTP.
//! - `mono`: CLOCK_MONOTONIC, the pipeline's clock (GstSystemClock).
//! - `arrived`: running time (mono - base_time) the read was taken at.
//! - `pcr`: 27 MHz, as carried; `paced`/`at`: `Stamp`'s running times.
//! - `pes`: 90 kHz PTS; `pes_at`: `pts_on_clock` of it.
//! - layers: buffer PTS (running time) against the samples or frames
//!   counted since the epoch began.

use std::sync::{Mutex, OnceLock};

static PROBE: OnceLock<Option<Probe>> = OnceLock::new();

pub fn get() -> Option<&'static Probe> {
    PROBE
        .get_or_init(|| {
            let path = std::env::var("MEDIABOX_LIVE_CLOCK_PROBE").ok().filter(|p| !p.is_empty())?;
            Some(Probe { path, sec: Mutex::new(Second::default()), seq: Default::default(), layers: Mutex::new(Default::default()) })
        })
        .as_ref()
}

static TRACE: OnceLock<Option<mediabox_core::bintrace::Trace>> = OnceLock::new();

/// The player's path record by record (`mediabox_core::bintrace`):
/// `MEDIABOX_LIVE_TRACE=<file>`, off by default. Kinds:
///
/// - 11 a FIFO read: n bytes, a ns `read` waited.
/// - 12 a TS chunk pushed: n bytes, a ns the push held, b arrival and c
///   stamp (running time).
/// - 13 the clock stepped: a the step (ns, as i64), c the stamp.
/// - 14 a sound PES start: a PTS (90 kHz), c its running time.
/// - 15..18 a buffer at the parser, decoder, convert, sink: id generation,
///   n PCM frames (0 coded), a PTS, b duration, c bytes << 8 | 1 if DISCONT.
/// - 19 a PCR: a PCR (27 MHz), b arrival, c stamp.
/// - 20 the presentation waited: a its offset, b by how much.
/// - 21 a picture left tsdemux: a PTS, b when (running time).
pub fn trace() -> Option<&'static mediabox_core::bintrace::Trace> {
    TRACE.get_or_init(|| mediabox_core::bintrace::Trace::from_env("MEDIABOX_LIVE_TRACE", 512 << 20)).as_ref()
}

pub fn clock_ns(id: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid clock id and a timespec to fill.
    unsafe { libc::clock_gettime(id, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

pub fn mono() -> u64 {
    clock_ns(libc::CLOCK_MONOTONIC)
}

pub fn raw() -> u64 {
    clock_ns(libc::CLOCK_MONOTONIC_RAW)
}

/// `Stamp`'s state after a chunk that carried a PCR.
#[derive(Debug, Clone, Copy, Default)]
pub struct StampTrace {
    pub pcr: u64,
    pub arrived: u64,
    pub paced: i64,
    pub late: i64,
    pub earliest: i64,
    pub base: i64,
    pub follow: i64,
    pub delta: i64,
    pub at: u64,
    pub settled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Parser,
    Decoder,
    Convert,
    Sink,
}

const LAYERS: [&str; 4] = ["parser", "decoder", "convert", "sink"];

/// One layer's buffers against the time their samples last: an epoch starts
/// at a DISCONT, a new generation or rate, or a step over 50 ms.
#[derive(Default, Clone, Copy)]
struct Track {
    generation: u64,
    rate: u32,
    /// The epoch's first PTS, and the samples (or ns, without a rate) since.
    anchor: Option<u64>,
    count: u64,
    next: Option<u64>,
}

#[derive(Default, Clone, Copy)]
struct LayerSecond {
    buffers: u64,
    gaps: u64,
    overlaps: u64,
    gap_ns: i64,
    overlap_ns: i64,
    epochs: u64,
    /// First of the second: PTS, ns of samples since the epoch's anchor, the
    /// anchor, and PTS - (anchor + samples).
    first: Option<(u64, u64, u64, i64)>,
    residual_last: Option<i64>,
}

#[derive(Default)]
struct Second {
    reads: u64,
    bytes: u64,
    read_wait_sum: u64,
    read_wait_max: u64,
    /// First read of the second: seq, mono after, raw after.
    read_first: Option<(u64, u64, u64)>,
    pushes: u64,
    push_sum: u64,
    push_max: u64,
    stamp_first: Option<StampTrace>,
    stamp_last: Option<StampTrace>,
    late_min: Option<i64>,
    late_max: Option<i64>,
    follow_delta_sum: i64,
    pcr_pieces: u64,
    /// arrived - at, extremes.
    edge_min: Option<i64>,
    edge_max: Option<i64>,
    pes_first: Option<(u64, u64)>,
    pes: u64,
    video_first: Option<(u64, u64)>,
    video: u64,
    layer: [LayerSecond; 4],
}

pub struct Probe {
    path: String,
    sec: Mutex<Second>,
    seq: std::sync::atomic::AtomicU64,
    layers: Mutex<[Track; 4]>,
}

fn min(a: Option<i64>, v: i64) -> Option<i64> {
    Some(a.map_or(v, |a| a.min(v)))
}

fn max(a: Option<i64>, v: i64) -> Option<i64> {
    Some(a.map_or(v, |a| a.max(v)))
}

impl Probe {
    fn with(&self, f: impl FnOnce(&mut Second)) {
        f(&mut self.sec.lock().unwrap_or_else(|p| p.into_inner()));
    }

    /// A read of the FIFO: its bytes and how long `read` waited.
    pub fn read(&self, bytes: usize, wait_ns: u64, mono_after: u64, raw_after: u64) {
        self.with(|s| {
            let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            s.reads += 1;
            s.bytes += bytes as u64;
            s.read_wait_sum += wait_ns;
            s.read_wait_max = s.read_wait_max.max(wait_ns);
            s.read_first.get_or_insert((seq, mono_after, raw_after));
        });
    }

    /// A `push_buffer` of the transport stream and how long it held.
    pub fn push(&self, held_ns: u64) {
        self.with(|s| {
            s.pushes += 1;
            s.push_sum += held_ns;
            s.push_max = s.push_max.max(held_ns);
        });
    }

    pub fn chunk(&self, arrived: u64, at: u64) {
        let edge = arrived as i64 - at as i64;
        self.with(|s| {
            s.edge_min = min(s.edge_min, edge);
            s.edge_max = max(s.edge_max, edge);
        });
    }

    pub fn stamp(&self, t: StampTrace) {
        self.with(|s| {
            s.pcr_pieces += 1;
            s.stamp_first.get_or_insert(t);
            s.stamp_last = Some(t);
            if t.settled {
                s.late_min = min(s.late_min, t.late);
                s.late_max = max(s.late_max, t.late);
            }
            s.follow_delta_sum += t.delta;
        });
    }

    /// A PES start: its PTS (90 kHz) and where `pts_on_clock` put it.
    pub fn pes(&self, pts: u64, at: u64) {
        self.with(|s| {
            s.pes += 1;
            s.pes_first.get_or_insert((pts, at));
        });
    }

    /// A picture: its PTS and the running time it was handed on at.
    pub fn video(&self, pts: u64, now: u64) {
        self.with(|s| {
            s.video += 1;
            s.video_first.get_or_insert((pts, now));
        });
    }

    /// A buffer at `layer`: PCM with `frames` at `rate`, or coded with its
    /// `duration`.
    pub fn layer(&self, layer: Layer, generation: u64, pts: Option<u64>, duration: Option<u64>, pcm: Option<(u64, u32)>, discont: bool) {
        let i = layer as usize;
        let Some(pts) = pts else { return };
        let mut tracks = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        let t = &mut tracks[i];
        let rate = pcm.map_or(0, |(_, r)| r);
        let length = |count: u64, rate: u32| if rate > 0 { (u128::from(count) * 1_000_000_000 / u128::from(rate)) as u64 } else { count };
        let tolerance = if rate > 0 { 1_000_000_000 / u64::from(rate) } else { 25_000 };
        let mut outcome = None;
        let restart = t.generation != generation || t.rate != rate || discont || t.next.is_none_or(|n| n.abs_diff(pts) > 50_000_000);
        if restart {
            *t = Track { generation, rate, anchor: Some(pts), count: 0, next: None };
        } else if let Some(next) = t.next {
            let step = pts as i64 - next as i64;
            if step.unsigned_abs() > tolerance {
                outcome = Some(step);
            }
        }
        let anchor = t.anchor.unwrap_or(pts);
        let since = length(t.count, rate);
        let residual = pts as i64 - (anchor + since) as i64;
        t.count += pcm.map_or(duration.unwrap_or(0), |(frames, _)| frames);
        t.next = Some(pts + pcm.map_or(duration.unwrap_or(0), |(frames, rate)| length(frames, rate)));
        drop(tracks);
        self.with(|s| {
            let l = &mut s.layer[i];
            l.buffers += 1;
            if restart {
                l.epochs += 1;
            }
            match outcome {
                Some(step) if step > 0 => {
                    l.gaps += 1;
                    l.gap_ns += step;
                }
                Some(step) => {
                    l.overlaps += 1;
                    l.overlap_ns += step;
                }
                None => {}
            }
            l.first.get_or_insert((pts, since, anchor, residual));
            l.residual_last = Some(residual);
        });
    }

    /// The second so far as one line, and a new one begun. `extra` is what
    /// the health thread samples itself (queue levels and the like).
    pub fn flush(&self, extra: serde_json::Value) {
        let s = std::mem::take(&mut *self.sec.lock().unwrap_or_else(|p| p.into_inner()));
        let stamp = |t: Option<StampTrace>| {
            t.map(|t| {
                serde_json::json!({
                    "pcr": t.pcr, "arrived": t.arrived, "paced": t.paced, "late": t.late,
                    "earliest": t.earliest, "base": t.base, "follow": t.follow, "at": t.at, "settled": t.settled,
                })
            })
        };
        let layers: serde_json::Map<String, serde_json::Value> = LAYERS
            .iter()
            .zip(s.layer.iter())
            .map(|(name, l)| {
                (
                    (*name).to_string(),
                    serde_json::json!({
                        "n": l.buffers, "gaps": l.gaps, "overlaps": l.overlaps, "gap_ns": l.gap_ns,
                        "overlap_ns": l.overlap_ns, "epochs": l.epochs,
                        "first": l.first.map(|(pts, since, anchor, r)| [pts as i64, since as i64, anchor as i64, r]),
                        "residual": l.residual_last,
                    }),
                )
            })
            .collect();
        let line = serde_json::json!({
            "mono": mono(), "raw": raw(),
            "reads": s.reads, "bytes": s.bytes, "read_wait_sum": s.read_wait_sum, "read_wait_max": s.read_wait_max,
            "read_first": s.read_first,
            "pushes": s.pushes, "push_sum": s.push_sum, "push_max": s.push_max,
            "pcr_pieces": s.pcr_pieces, "stamp": stamp(s.stamp_first), "stamp_last": stamp(s.stamp_last),
            "late_min": s.late_min, "late_max": s.late_max, "follow_delta": s.follow_delta_sum,
            "edge_min": s.edge_min, "edge_max": s.edge_max,
            "pes": s.pes, "pes_first": s.pes_first,
            "video": s.video, "video_first": s.video_first,
            "layers": layers,
            "extra": extra,
        });
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{line}");
        }
    }
}
