//! A transport hole made on purpose, for a measurement: the receiver's
//! output loses packets as it would lose them to the signal, and the
//! player's recovery is watched on the timeline. Only while the timeline is
//! on, and only once per request: a file in the trace directory asks for it
//! and is taken (renamed `inject.taken`) when the hole is made.
//!
//! ```text
//! echo "hole_ms=480 pids=all" > /run/mediabox-trace/inject       # every PID
//! echo "hole_ms=480 pids=256" > /run/mediabox-trace/inject       # one PID
//! echo "hole_ms=480 pids=256 anchor=256" > ...                   # cut by 256
//! ```
//!
//! The hole starts inside a PES: the anchor PID's (the first of `pids`, or
//! `anchor`; for `all` the first PID with a PES start) second packet after a
//! PES start is the first one lost. It lasts `hole_ms` of the stream's own
//! clock (the PCRs going by, lost or not). After it each PID comes back
//! with its next packet starting a PES or a section -- what a receiver
//! finds after a loss that ends -- and a PID with no payload at once.
//! Every step is on the timeline (`inject-*`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mediabox_core::ts_trace::PACKET;

const LOOK_EVERY: Duration = Duration::from_millis(250);
const PCR_HZ: u64 = 90_000;
const PCR_WRAP: u64 = 1 << 33;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub hole_ms: u64,
    /// None: every PID.
    pub pids: Option<BTreeSet<u16>>,
    pub anchor: Option<u16>,
}

impl Spec {
    pub fn parse(text: &str) -> Option<Self> {
        let mut spec = Spec { hole_ms: 0, pids: None, anchor: None };
        for part in text.split_whitespace() {
            let (key, value) = part.split_once('=')?;
            match key {
                "hole_ms" => spec.hole_ms = value.parse().ok()?,
                "pids" if value == "all" => spec.pids = None,
                "pids" => spec.pids = Some(value.split(',').map(|v| v.parse().ok()).collect::<Option<_>>()?),
                "anchor" => spec.anchor = Some(value.parse().ok()?),
                _ => return None,
            }
        }
        (spec.hole_ms > 0).then_some(spec)
    }

    fn takes(&self, pid: u16) -> bool {
        self.pids.as_ref().is_none_or(|pids| pids.contains(&pid))
    }
}

#[derive(Debug)]
enum Phase {
    /// The anchor's PES start is waited for, then one packet of it.
    Cutting { anchor: Option<u16>, after_start: Option<u32> },
    /// From this PCR on, for the hole's length.
    Hole { from: Option<u64> },
    /// The PIDs not back yet, each with its next start.
    Ending { waiting: BTreeSet<u16> },
}

#[derive(Debug)]
pub struct Hole {
    spec: Spec,
    phase: Phase,
    lost: BTreeMap<u16, u64>,
    seen: BTreeSet<u16>,
}

impl Hole {
    pub fn new(spec: Spec) -> Self {
        let anchor = spec.anchor.or_else(|| spec.pids.as_ref().and_then(|p| p.iter().next().copied()));
        Self { spec, phase: Phase::Cutting { anchor, after_start: None }, lost: BTreeMap::new(), seen: BTreeSet::new() }
    }

    pub fn done(&self) -> bool {
        matches!(&self.phase, Phase::Ending { waiting } if waiting.is_empty())
    }

    /// `chunk` with what the hole takes out of it; `say` is told each step.
    pub fn apply(&mut self, chunk: &[u8], say: &mut dyn FnMut(&str, String)) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        let mut at = 0;
        while at < chunk.len() {
            // Whole packets only; bytes out of step go through as they are.
            if chunk[at] != 0x47 || at + PACKET > chunk.len() {
                out.push(chunk[at]);
                at += 1;
                continue;
            }
            let p = &chunk[at..at + PACKET];
            at += PACKET;
            if self.keep(p, say) {
                out.extend_from_slice(p);
            } else {
                *self.lost.entry(pid_of(p)).or_default() += 1;
            }
        }
        out
    }

    fn keep(&mut self, p: &[u8], say: &mut dyn FnMut(&str, String)) -> bool {
        let pid = pid_of(p);
        let start = p[1] & 0x40 != 0;
        let payload = (p[3] >> 4) & 0x1 != 0;
        let pcr = pcr_of(p);
        self.seen.insert(pid);
        match &mut self.phase {
            Phase::Cutting { anchor, after_start } => {
                if anchor.is_none() && start && payload && is_pes(p) {
                    *anchor = Some(pid);
                }
                if *anchor != Some(pid) || !payload {
                    return true;
                }
                match after_start {
                    None if start => *after_start = Some(0),
                    Some(n) if !start => {
                        *n += 1;
                        if *n >= 1 {
                            say("inject-start", format!("pid={pid} hole_ms={} pids={:?}", self.spec.hole_ms, self.spec.pids));
                            self.phase = Phase::Hole { from: pcr };
                            return !self.spec.takes(pid);
                        }
                    }
                    Some(_) => *after_start = Some(0),
                    None => {}
                }
                true
            }
            Phase::Hole { from } => {
                if from.is_none() {
                    *from = pcr;
                }
                if let (Some(begun), Some(now)) = (*from, pcr)
                    && (now + PCR_WRAP - begun) % PCR_WRAP >= self.spec.hole_ms * PCR_HZ / 1000
                {
                    let waiting: BTreeSet<u16> = self.seen.iter().copied().filter(|&p| self.spec.takes(p)).collect();
                    say("inject-hole-end", format!("pcr={now} lost={:?}", self.lost));
                    self.phase = Phase::Ending { waiting };
                    return self.keep(p, say);
                }
                !self.spec.takes(pid)
            }
            Phase::Ending { waiting } => {
                if !waiting.contains(&pid) {
                    return true;
                }
                if start || !payload {
                    waiting.remove(&pid);
                    say(
                        "inject-back",
                        format!("pid={pid} start={} pes_pts={} lost={}", u8::from(start), pes_pts(p).map_or(-1, |v| v as i64), self.lost.get(&pid).copied().unwrap_or(0)),
                    );
                    if waiting.is_empty() {
                        say("inject-done", format!("lost={:?}", self.lost));
                    }
                    return true;
                }
                false
            }
        }
    }
}

/// The receiver's output's hole, asked for by a file in the trace directory.
#[derive(Debug, Default)]
pub struct Injector {
    looked: Option<Instant>,
    hole: Option<Hole>,
}

impl Injector {
    pub fn apply(&mut self, chunk: Vec<u8>) -> Vec<u8> {
        if !mediabox_core::timeline::enabled() && self.hole.is_none() {
            return chunk;
        }
        if self.hole.is_none() && self.looked.is_none_or(|at| at.elapsed() >= LOOK_EVERY) {
            self.looked = Some(Instant::now());
            self.hole = asked(&trace_dir()).map(Hole::new);
        }
        let Some(hole) = self.hole.as_mut() else { return chunk };
        let out = hole.apply(&chunk, &mut |event, fields| mediabox_core::timeline::write(event, format_args!("{fields}")));
        if hole.done() {
            self.hole = None;
        }
        out
    }
}

fn trace_dir() -> PathBuf {
    std::env::var_os("MEDIABOX_TRACE_DIR").map_or_else(|| PathBuf::from(mediabox_core::timeline::DIR), PathBuf::from)
}

/// The spec in `<dir>/inject`, taken: renamed so that it is made once.
fn asked(dir: &Path) -> Option<Spec> {
    let file = dir.join("inject");
    let text = std::fs::read_to_string(&file).ok()?;
    let _ = std::fs::rename(&file, dir.join("inject.taken"));
    let spec = Spec::parse(&text);
    mediabox_core::timeline!("inject-asked", "spec={:?} ok={}", text.trim(), u8::from(spec.is_some()));
    spec
}

fn pid_of(p: &[u8]) -> u16 {
    (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2])
}

fn pcr_of(p: &[u8]) -> Option<u64> {
    let adaptation = (p[3] >> 4) & 0x2 != 0;
    if !adaptation || p[4] < 7 || p[5] & 0x10 == 0 {
        return None;
    }
    Some((u64::from(p[6]) << 25) | (u64::from(p[7]) << 17) | (u64::from(p[8]) << 9) | (u64::from(p[9]) << 1) | (u64::from(p[10]) >> 7))
}

fn body(p: &[u8]) -> &[u8] {
    let control = (p[3] >> 4) & 0x3;
    let from = if control & 0x2 != 0 { 5 + usize::from(p[4]) } else { 4 };
    p.get(from..).unwrap_or_default()
}

fn is_pes(p: &[u8]) -> bool {
    body(p).starts_with(&[0, 0, 1])
}

fn pes_pts(p: &[u8]) -> Option<u64> {
    let b = body(p);
    if b.len() < 14 || b[..3] != [0, 0, 1] || b[7] & 0x80 == 0 {
        return None;
    }
    let b = &b[9..14];
    Some((u64::from(b[0] >> 1) & 0x7) << 30 | u64::from(b[1]) << 22 | (u64::from(b[2]) >> 1) << 15 | u64::from(b[3]) << 7 | u64::from(b[4]) >> 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packet of `pid`; with `pcr` (90 kHz) an adaptation field carries it.
    fn packet(pid: u16, start: bool, pcr: Option<u64>) -> Vec<u8> {
        let mut p = vec![0x47, (u8::from(start) << 6) | (pid >> 8) as u8, pid as u8, 0x10];
        if let Some(base) = pcr {
            p[3] = 0x30;
            p.extend_from_slice(&[7, 0x10, (base >> 25) as u8, (base >> 17) as u8, (base >> 9) as u8, (base >> 1) as u8, ((base & 1) as u8) << 7, 0]);
        }
        if start {
            p.extend_from_slice(&[0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1]);
        }
        p.resize(PACKET, 0xFF);
        p
    }

    /// 40 ms pictures on 256 (PCR on each start), sound on 257 each 24 ms:
    /// 10 packets a picture, 2 a sound frame.
    fn stream(seconds: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut t = 0u64; // 90 kHz
        while t < seconds * PCR_HZ {
            if t % 3600 == 0 {
                out.push(packet(256, true, Some(t)));
                for _ in 0..9 {
                    out.push(packet(256, false, None));
                }
            }
            if t % 2160 == 0 {
                out.push(packet(257, true, None));
                out.push(packet(257, false, None));
            }
            t += 360;
        }
        out
    }

    fn run(spec: &str) -> (Vec<Vec<u8>>, Vec<(String, String)>) {
        let mut hole = Hole::new(Spec::parse(spec).expect("a spec"));
        let mut said = Vec::new();
        let mut kept = Vec::new();
        for p in stream(3) {
            let out = hole.apply(&p, &mut |e, f| said.push((e.to_string(), f)));
            if !out.is_empty() {
                kept.push(out);
            }
        }
        assert!(hole.done(), "{said:?}");
        (kept, said)
    }

    #[test]
    fn the_hole_starts_inside_a_pes_and_every_pid_comes_back_at_a_start() {
        let all = stream(3);
        let (kept, said) = run("hole_ms=480 pids=all");
        let lost = all.len() - kept.len();
        // 480 ms of 3 s: about 16 % of the packets, and some more for each
        // PID's wait for its start.
        assert!((lost as f64 / all.len() as f64 - 0.16).abs() < 0.04, "{lost} of {}", all.len());
        let events: Vec<&str> = said.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(events, ["inject-start", "inject-hole-end", "inject-back", "inject-back", "inject-done"], "{said:?}");
        // The first packet lost is the second of a picture's PES; the first
        // kept of each PID after the hole starts a PES.
        let first_lost = all.iter().zip(&kept).position(|(a, b)| a != b).expect("a hole");
        assert_eq!(pid_of(&all[first_lost]), 256);
        assert_eq!(all[first_lost][1] & 0x40, 0);
        assert!(all[first_lost - 1][1] & 0x40 != 0);
        let mut back = BTreeSet::new();
        for p in &kept[first_lost..] {
            if back.insert(pid_of(p)) {
                assert!(p[1] & 0x40 != 0, "pid {} came back without a start", pid_of(p));
            }
        }
    }

    #[test]
    fn one_pid_is_taken_and_the_others_left_alone() {
        let all = stream(3);
        let (kept, said) = run("hole_ms=480 pids=257");
        let lost: usize = all.iter().filter(|p| pid_of(p) == 257).count() - kept.iter().filter(|p| pid_of(p) == 257).count();
        assert_eq!(kept.iter().filter(|p| pid_of(p) == 256).count(), all.iter().filter(|p| pid_of(p) == 256).count());
        // 480 ms of sound frames, 24 ms each, two packets each, and the
        // frame cut at its start.
        assert!((38..=44).contains(&lost), "{lost} {said:?}");
    }

    #[test]
    fn a_spec_is_read_or_refused() {
        assert_eq!(Spec::parse("hole_ms=480 pids=all"), Some(Spec { hole_ms: 480, pids: None, anchor: None }));
        assert_eq!(Spec::parse("hole_ms=480 pids=256,257 anchor=257").and_then(|s| s.anchor), Some(257));
        assert_eq!(Spec::parse("hole_ms=0 pids=all"), None);
        assert_eq!(Spec::parse("hole=480"), None);
    }
}
