//! Two kinds of stop, told apart for the trace (`trace`, kind 30):
//!
//! - `Mux`: no TS packet delivered for longer than `MUX_GAP_NS`, on the
//!   host's clock (the transfer completions). A measured rhythm, not a
//!   standard: at 58 Mbit/s a 150 kB transfer completes every 21 ms
//!   (P2Hh, 2 h: p99 21.4 ms, one 462 ms stop), so 100 ms is about five
//!   transfers missing.
//! - `Service`: the multiplex flowing, consecutive PCRs of the program more
//!   than `SERVICE_PCR_GAP_NS` apart in the multiplex's own time (packets
//!   between them at the rate measured): ETSI TS 101 154 V2.9.1 §4.1.5.3,
//!   at most 100 ms between PCRs of a program. Packet positions, not
//!   completions: a stop of the whole multiplex moves no position, and
//!   a PCR interval that a `Mux` gap falls in is that gap's, not the
//!   service's.

pub const MUX_GAP_NS: u64 = 100_000_000;
pub const SERVICE_PCR_GAP_NS: u64 = 100_000_000;
/// Before this much of the stream the rate is not known well enough.
const RATE_AFTER_NS: u64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Class {
    Service = 1,
    Mux = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub class: Class,
    pub id: u32,
    /// CLOCK_MONOTONIC ns: the last delivery before, the first after.
    pub start_ns: u64,
    pub end_ns: u64,
    /// Service: the PCR interval in the multiplex's time; Mux: the stop.
    pub length_ns: u64,
}

#[derive(Debug, Default)]
pub struct GapWatch {
    /// (completion ns, packets so far) of the first and the last chunk.
    first: Option<(u64, u64)>,
    last: Option<(u64, u64)>,
    /// The last PCR: (packet index, completion ns).
    last_pcr: Option<(u64, u64)>,
    /// The end of the latest Mux gap.
    mux_end: Option<u64>,
    /// A Mux gap seen open (`open`) and not yet closed.
    open_since: Option<u64>,
    next_id: u32,
}

impl GapWatch {
    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    /// A chunk of `packets` completed at `at`; a Mux gap if the one before
    /// was too long ago.
    pub fn chunk(&mut self, at: u64, packets: u64) -> Option<Gap> {
        if packets == 0 {
            return None;
        }
        let total = self.last.map_or(0, |(_, t)| t) + packets;
        let gap = match self.last {
            Some((before, _)) if at.saturating_sub(before) > MUX_GAP_NS => {
                self.mux_end = Some(at);
                self.open_since = None;
                let id = self.id();
                Some(Gap { class: Class::Mux, id, start_ns: before, end_ns: at, length_ns: at - before })
            }
            _ => None,
        };
        self.first.get_or_insert((at, total));
        self.last = Some((at, total));
        gap
    }

    /// Nothing delivered by `now`: the start of a Mux gap still open, once.
    pub fn open(&mut self, now: u64) -> Option<u64> {
        let (before, _) = self.last?;
        if now.saturating_sub(before) > MUX_GAP_NS && self.open_since.is_none() {
            self.open_since = Some(before);
            return Some(before);
        }
        None
    }

    /// Packets per ns, from the stream so far.
    fn rate(&self) -> Option<f64> {
        let ((t0, p0), (t1, p1)) = (self.first?, self.last?);
        (t1.saturating_sub(t0) >= RATE_AFTER_NS && p1 > p0).then(|| (p1 - p0) as f64 / (t1 - t0) as f64)
    }

    /// A PCR of the program, the `index`th packet of the stream, in a chunk
    /// completed at `at`; a Service gap if it came too long after the last
    /// one in the multiplex's time and no Mux gap fell between them.
    pub fn pcr(&mut self, index: u64, at: u64) -> Option<Gap> {
        let before = self.last_pcr.replace((index, at));
        let (index0, at0) = before?;
        let rate = self.rate()?;
        let length = (index.saturating_sub(index0) as f64 / rate) as u64;
        if length <= SERVICE_PCR_GAP_NS || self.mux_end.is_some_and(|end| end > at0) {
            return None;
        }
        let id = self.id();
        Some(Gap { class: Class::Service, id, start_ns: at0, end_ns: at, length_ns: length })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;
    /// 816 packets a 21 ms transfer: 38 857 packets a second.
    const PER: u64 = 816;

    /// Two seconds of the multiplex flowing, a PCR every 22 ms of it.
    fn flowing(w: &mut GapWatch) -> (u64, u64) {
        let (mut t, mut index) = (0, 0);
        for _ in 0..100 {
            t += 21 * MS;
            index += PER;
            assert_eq!(w.chunk(t, PER), None);
            assert_eq!(w.pcr(index, t), None);
        }
        (t, index)
    }

    #[test]
    fn the_service_stopping_while_the_multiplex_flows_is_a_service_gap() {
        let mut w = GapWatch::default();
        let (mut t, mut index) = flowing(&mut w);
        // Seven transfers without the program's PCR: 147 ms of packets.
        for _ in 0..7 {
            t += 21 * MS;
            index += PER;
            assert_eq!(w.chunk(t, PER), None);
        }
        let gap = w.pcr(index, t).expect("a service gap");
        assert_eq!(gap.class, Class::Service);
        assert!((140 * MS..160 * MS).contains(&gap.length_ns), "{gap:?}");
        assert_eq!(w.open(t + 10 * MS), None, "the multiplex never stopped");
    }

    #[test]
    fn the_whole_multiplex_stopping_is_a_mux_gap_not_a_service_gap() {
        let mut w = GapWatch::default();
        let (mut t, mut index) = flowing(&mut w);
        // 462 ms with nothing delivered; seen open while it lasts.
        assert_eq!(w.open(t + 50 * MS), None);
        assert_eq!(w.open(t + 150 * MS), Some(t));
        assert_eq!(w.open(t + 300 * MS), None, "once");
        let before = t;
        t += 462 * MS;
        index += PER;
        let gap = w.chunk(t, PER).expect("a mux gap");
        assert_eq!((gap.class, gap.start_ns, gap.end_ns), (Class::Mux, before, t));
        // The PCR after it: the program's position moved 21 ms.
        assert_eq!(w.pcr(index, t), None);
    }

    #[test]
    fn a_pcr_interval_a_mux_gap_falls_in_is_the_mux_gap_s() {
        let mut w = GapWatch::default();
        let (mut t, mut index) = flowing(&mut w);
        // The multiplex stops, and comes back with packets lost besides:
        // the program's position too moves more than 100 ms.
        t += 300 * MS;
        index += 10 * PER;
        assert_eq!(w.chunk(t, 10 * PER).map(|g| g.class), Some(Class::Mux));
        assert_eq!(w.pcr(index, t), None);
    }

    #[test]
    fn no_service_gap_before_the_rate_is_known() {
        let mut w = GapWatch::default();
        assert_eq!(w.chunk(21 * MS, PER), None);
        assert_eq!(w.pcr(PER, 21 * MS), None);
        assert_eq!(w.chunk(42 * MS, 20 * PER), None);
        assert_eq!(w.pcr(21 * PER, 42 * MS), None);
    }
}
