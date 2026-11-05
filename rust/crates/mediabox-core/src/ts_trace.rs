//! What a transport stream carried, on the timeline: each PES start's PID
//! and PTS, each PAT's transport_stream_id, each PMT's programme, each PCR,
//! and the packets a PID had before its first PES start (a PES cut at its
//! head). Nothing is added to the stream: a packet is told by what is in
//! it, wherever it is looked at -- the receiver's output, the player's
//! input -- so that one stream epoch's packets reaching the next one are
//! seen as such: an event of one side is the other side's when their
//! (kind, PID, value) are the same.
//!
//! Only while the timeline is on (`crate::timeline`); off, a caller pays
//! one atomic load per chunk.
//!
//! ```text
//! <ns> <tag>-pes <label> pid=<pid> v=<PTS, 90 kHz> x=<stream_id>
//! <ns> <tag>-pat <label> pid=0 v=<transport_stream_id> x=<version>
//! <ns> <tag>-pmt <label> pid=<pid> v=<program_number> x=<version>
//! <ns> <tag>-pcr <label> pid=<pid> v=<PCR base, 90 kHz> x=0
//! <ns> <tag>-head <label> pid=<pid> v=<packets before the first PES start> x=0
//! ```

use std::collections::{BTreeMap, BTreeSet};

pub const PACKET: usize = 188;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pes,
    Pat,
    Pmt,
    Pcr,
    Head,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Pes => "pes",
            Kind::Pat => "pat",
            Kind::Pmt => "pmt",
            Kind::Pcr => "pcr",
            Kind::Head => "head",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub kind: Kind,
    pub pid: u16,
    pub value: u64,
    pub extra: u16,
}

/// One stream's reader: the PMT PIDs its latest PAT named, and which PIDs
/// have had a PES start.
#[derive(Debug, Default)]
pub struct Content {
    pmt_pids: BTreeSet<u16>,
    started: BTreeSet<u16>,
    head: BTreeMap<u16, u64>,
}

impl Content {
    /// Another epoch: nothing of the one before is assumed.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Each whole packet of `data` (from its first sync byte with another
    /// one a packet on), what it says.
    pub fn scan(&mut self, data: &[u8], mut each: impl FnMut(Seen)) {
        let Some(mut at) = (0..data.len()).find(|&i| {
            data[i] == 0x47 && (i + PACKET >= data.len() || data[i + PACKET] == 0x47)
        }) else {
            return;
        };
        while at + PACKET <= data.len() {
            let p = &data[at..at + PACKET];
            at += PACKET;
            if p[0] != 0x47 || p[1] & 0x80 != 0 {
                continue;
            }
            let pid = (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]);
            if pid == 0x1FFF {
                continue;
            }
            let start = p[1] & 0x40 != 0;
            let control = (p[3] >> 4) & 0x3;
            let mut payload = 4;
            if control & 0x2 != 0 {
                let length = usize::from(p[4]);
                if length >= 7 && p[5] & 0x10 != 0 {
                    let base = (u64::from(p[6]) << 25)
                        | (u64::from(p[7]) << 17)
                        | (u64::from(p[8]) << 9)
                        | (u64::from(p[9]) << 1)
                        | (u64::from(p[10]) >> 7);
                    each(Seen { kind: Kind::Pcr, pid, value: base, extra: 0 });
                }
                payload = 5 + length;
            }
            if control & 0x1 == 0 || payload >= PACKET {
                continue;
            }
            let body = &p[payload..];
            if !start {
                if !self.started.contains(&pid) && pid != 0 && !self.pmt_pids.contains(&pid) {
                    *self.head.entry(pid).or_default() += 1;
                }
                continue;
            }
            if self.started.insert(pid)
                && let Some(n) = self.head.remove(&pid)
            {
                each(Seen { kind: Kind::Head, pid, value: n, extra: 0 });
            }
            if pid == 0 || self.pmt_pids.contains(&pid) {
                let pointer = usize::from(body[0]);
                let Some(section) = body.get(1 + pointer..) else { continue };
                if section.len() < 8 {
                    continue;
                }
                let id = u64::from(u16::from_be_bytes([section[3], section[4]]));
                let version = u16::from((section[5] >> 1) & 0x1F);
                match (pid, section[0]) {
                    (0, 0x00) => {
                        let length = usize::from(u16::from_be_bytes([section[1], section[2]]) & 0x0FFF);
                        let end = (3 + length).saturating_sub(4).min(section.len());
                        self.pmt_pids = section
                            .get(8..end)
                            .unwrap_or_default()
                            .chunks_exact(4)
                            .filter(|e| u16::from_be_bytes([e[0], e[1]]) != 0)
                            .map(|e| u16::from_be_bytes([e[2], e[3]]) & 0x1FFF)
                            .collect();
                        each(Seen { kind: Kind::Pat, pid, value: id, extra: version });
                    }
                    (_, 0x02) => each(Seen { kind: Kind::Pmt, pid, value: id, extra: version }),
                    _ => {}
                }
                continue;
            }
            // A PES with its optional header and a PTS (2.4.3.7).
            if body.len() >= 14 && body[..3] == [0, 0, 1] {
                let stream_id = body[3];
                let plain = matches!(stream_id, 0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF);
                if !plain && body[7] & 0x80 != 0 {
                    let b = &body[9..14];
                    let pts = (u64::from(b[0] >> 1) & 0x7) << 30
                        | u64::from(b[1]) << 22
                        | (u64::from(b[2]) >> 1) << 15
                        | u64::from(b[3]) << 7
                        | u64::from(b[4]) >> 1;
                    each(Seen { kind: Kind::Pes, pid, value: pts, extra: u16::from(stream_id) });
                }
            }
        }
    }

    /// `scan` onto the timeline as `<tag>-<kind> <label> ...`, if it is on.
    pub fn trace(&mut self, tag: &str, label: std::fmt::Arguments<'_>, data: &[u8]) {
        if !crate::timeline::enabled() {
            return;
        }
        let label = label.to_string();
        self.scan(data, |s| {
            crate::timeline::write(
                &format!("{tag}-{}", s.kind.name()),
                format_args!("{label} pid={} v={} x={}", s.pid, s.value, s.extra),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pid: u16, start: bool, cc: u8, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x47, (u8::from(start) << 6) | (pid >> 8) as u8, pid as u8, 0x10 | (cc & 0xF)];
        p.extend_from_slice(payload);
        p.resize(PACKET, 0xFF);
        p
    }

    fn pes(pts: u64) -> Vec<u8> {
        let b = [
            0x21 | (((pts >> 30) & 0x7) as u8) << 1,
            (pts >> 22) as u8,
            (((pts >> 15) & 0x7F) as u8) << 1 | 1,
            (pts >> 7) as u8,
            ((pts & 0x7F) as u8) << 1 | 1,
        ];
        let mut v = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5];
        v.extend_from_slice(&b);
        v
    }

    fn pat(tsid: u16, program: u16, pmt: u16) -> Vec<u8> {
        let mut v = vec![0, 0x00, 0xB0, 13];
        v.extend_from_slice(&tsid.to_be_bytes());
        v.extend_from_slice(&[0xC3, 0, 0]);
        v.extend_from_slice(&program.to_be_bytes());
        v.extend_from_slice(&(0xE000 | pmt).to_be_bytes());
        v.extend_from_slice(&[0, 0, 0, 0]);
        v
    }

    #[test]
    fn a_stream_is_told_by_its_tables_its_pes_and_a_cut_head() {
        let mut data = Vec::new();
        data.extend(packet(0x100, false, 0, &[0xAA; 10]));
        data.extend(packet(0x100, false, 1, &[0xAA; 10]));
        data.extend(packet(0, true, 0, &pat(43105, 10601, 0x1000)));
        let mut pmt = vec![0, 0x02, 0xB0, 13];
        pmt.extend_from_slice(&10601u16.to_be_bytes());
        pmt.extend_from_slice(&[0xC5, 0, 0, 0xE1, 0, 0xF0, 0, 0, 0, 0, 0]);
        data.extend(packet(0x1000, true, 0, &pmt));
        data.extend(packet(0x100, true, 2, &pes(0x1_2345_6789)));
        let mut seen = Vec::new();
        Content::default().scan(&data[5..], |s| seen.push(s)); // not aligned at first
        let mut seen_aligned = Vec::new();
        Content::default().scan(&data, |s| seen_aligned.push(s));
        assert_eq!(
            seen_aligned,
            vec![
                Seen { kind: Kind::Pat, pid: 0, value: 43105, extra: 1 },
                Seen { kind: Kind::Pmt, pid: 0x1000, value: 10601, extra: 2 },
                Seen { kind: Kind::Head, pid: 0x100, value: 2, extra: 0 },
                Seen { kind: Kind::Pes, pid: 0x100, value: 0x1_2345_6789, extra: 0xE0 },
            ]
        );
        // From the next sync byte on: one packet of the cut head fewer.
        assert_eq!(seen[2], Seen { kind: Kind::Head, pid: 0x100, value: 1, extra: 0 });
        assert_eq!(seen[3], seen_aligned[3]);
    }
}
