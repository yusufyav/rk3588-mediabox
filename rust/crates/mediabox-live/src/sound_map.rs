//! tsdemux's sound PES against the reader's, for the health report's
//! `sound_map_ms` -- a measurement, not the sound: tsdemux's sound pad is
//! drained and the sound played is the reader's (`route_sound`).
//!
//! The two meet on the input thread's order: the input thread hands a
//! piece of the stream to tsdemux (`push_piece`) and then the reader's PES
//! starts in it to the sound (`push_sound`, which records them here).
//! tsdemux runs on its own thread and may give a PES whole -- its
//! PES_packet_length known -- before the reader's record of the same PES is
//! made. Matched to the nearest record then, it was matched to the PES
//! before: a whole MPEG frame off (+23.995 ms in FAZ 2G and 2H), where the
//! two clocks agree to microseconds.
//!
//! tsdemux's buffer carries its running time only, not the raw PTS, so the
//! match is by time within `MATCH_NS` -- far under any sound frame (21 ms
//! for AAC, 24 for MP2, 32 for AC-3) -- never by nearness: a PES of tsdemux
//! with no record that close waits (`pending`) for the reader's, and one
//! that finds none in `PENDING_NS` is counted unmatched, its difference
//! never reported. A channel's change drops what waited for its records.

use std::collections::VecDeque;

/// Within this, tsdemux's PES and the reader's are the same PES.
pub const MATCH_NS: i64 = 2_000_000;
/// A PES of tsdemux waits this long for the reader's record of it.
pub const PENDING_NS: u64 = 1_000_000_000;
const KEEP: usize = 64;

#[derive(Debug, Default)]
pub struct SoundMatch {
    generation: u64,
    /// The reader's PES starts, running time.
    starts: VecDeque<u64>,
    /// tsdemux's PES with no record yet: its running time, since when.
    pending: VecDeque<(u64, u64)>,
    pub matched: u64,
    pub unmatched: u64,
    /// The last unmatched: tsdemux's time and the records then, for the
    /// evidence.
    pub last_unmatched: Option<(u64, Vec<u64>)>,
}

fn find(starts: &VecDeque<u64>, theirs: u64) -> Option<i64> {
    starts.iter().map(|&ours| theirs as i64 - ours as i64).find(|d| d.abs() <= MATCH_NS)
}

impl SoundMatch {
    fn channel(&mut self, generation: u64) {
        if generation != self.generation {
            self.generation = generation;
            self.starts.clear();
            self.pending.clear();
        }
    }

    /// The reader's record of a PES start; the differences of what waited
    /// for it.
    pub fn reader(&mut self, generation: u64, at: u64, now: u64) -> Vec<i64> {
        self.channel(generation);
        self.starts.push_back(at);
        while self.starts.len() > KEEP {
            self.starts.pop_front();
        }
        let mut out = Vec::new();
        self.pending.retain(|&(theirs, _)| {
            let d = theirs as i64 - at as i64;
            if d.abs() <= MATCH_NS {
                out.push(d);
                false
            } else {
                true
            }
        });
        self.matched += out.len() as u64;
        self.expire(now);
        out
    }

    /// tsdemux's PES: its difference if the reader has recorded it, else
    /// it waits.
    pub fn demux(&mut self, generation: u64, theirs: u64, now: u64) -> Option<i64> {
        self.channel(generation);
        self.expire(now);
        match find(&self.starts, theirs) {
            Some(d) => {
                self.matched += 1;
                Some(d)
            }
            None => {
                self.pending.push_back((theirs, now));
                while self.pending.len() > KEEP {
                    self.pending.pop_front();
                    self.unmatched += 1;
                }
                None
            }
        }
    }

    /// What waited past `PENDING_NS` is unmatched.
    pub fn expire(&mut self, now: u64) {
        while let Some(&(theirs, since)) = self.pending.front() {
            if now.saturating_sub(since) < PENDING_NS {
                break;
            }
            self.pending.pop_front();
            self.unmatched += 1;
            self.last_unmatched = Some((theirs, self.starts.iter().rev().take(4).copied().collect()));
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 24_000_000;
    const T: u64 = 50_000_000_000;

    /// PES k on the reader's running time; tsdemux's 5 us off it, as the
    /// two mappings were measured to differ.
    fn ours(k: u64) -> u64 {
        T + k * FRAME
    }
    fn theirs(k: u64) -> u64 {
        ours(k) + 5_000
    }

    #[test]
    fn the_reader_first_then_tsdemux_matches_its_own_pes() {
        let mut m = SoundMatch::default();
        for k in 0..10 {
            assert!(m.reader(1, ours(k), k * FRAME).is_empty());
        }
        assert_eq!(m.demux(1, theirs(9), 9 * FRAME), Some(5_000));
        assert_eq!((m.matched, m.unmatched, m.pending()), (1, 0, 0));
    }

    #[test]
    fn tsdemux_before_the_readers_record_waits_for_it_not_the_pes_before() {
        let mut m = SoundMatch::default();
        for k in 0..10 {
            m.reader(1, ours(k), k * FRAME);
        }
        // PES 10 whole at tsdemux before the reader recorded it: the record
        // of PES 9, 24 ms before, is not it.
        assert_eq!(m.demux(1, theirs(10), 10 * FRAME), None);
        assert_eq!(m.pending(), 1);
        // The reader's record comes 200 us later: the match is exact.
        assert_eq!(m.reader(1, ours(10), 10 * FRAME + 200_000), vec![5_000]);
        assert_eq!((m.matched, m.unmatched, m.pending()), (1, 0, 0));
    }

    #[test]
    fn a_pes_the_reader_never_records_is_unmatched_not_a_frame_off() {
        let mut m = SoundMatch::default();
        for k in 0..10 {
            m.reader(1, ours(k), k * FRAME);
        }
        assert_eq!(m.demux(1, theirs(10), 10 * FRAME), None);
        m.expire(10 * FRAME + PENDING_NS);
        assert_eq!((m.matched, m.unmatched, m.pending()), (0, 1, 0));
        assert_eq!(m.last_unmatched.as_ref().map(|(t, _)| *t), Some(theirs(10)));
    }

    #[test]
    fn another_channel_drops_what_waited_for_the_last_ones_records() {
        let mut m = SoundMatch::default();
        for k in 0..10 {
            m.reader(1, ours(k), k * FRAME);
        }
        assert_eq!(m.demux(1, theirs(10), 10 * FRAME), None);
        // Channel 2 starts its times again: its record at PES 10's time of
        // channel 1 is not that PES's.
        assert!(m.reader(2, ours(10), 10 * FRAME + 100_000).is_empty());
        assert_eq!(m.pending(), 0);
        assert_eq!(m.demux(2, theirs(10), 10 * FRAME + 200_000), Some(5_000));
    }

    #[test]
    fn a_time_base_jump_leaves_tsdemux_unmatched_not_mapped_to_a_neighbour() {
        // The reader followed a PCR discontinuity, tsdemux not yet: its PES
        // land 3.5 frames off any record.
        let mut m = SoundMatch::default();
        for k in 0..10 {
            m.reader(1, ours(k), k * FRAME);
        }
        let off = 3 * FRAME + FRAME / 2;
        assert_eq!(m.demux(1, ours(5) + off, 10 * FRAME), None);
        m.expire(10 * FRAME + PENDING_NS);
        assert_eq!((m.matched, m.unmatched), (0, 1));
        // A PTS that wrapped (2^33 ticks, 95 443 s) is as far from every
        // record.
        assert_eq!(m.demux(1, ours(5) + 95_443_717_688_888, 10 * FRAME + PENDING_NS), None);
    }
}
