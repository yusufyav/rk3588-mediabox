//! When each chunk of the receiver's stream is put on the pipeline's clock:
//! pure, so the timing can be tested without GStreamer.
//!
//! When a channel opens, the receiver sends its service from the last access
//! point on at once: half a second or more of the broadcast arrives in one
//! burst. Stamped with the time it arrived, all of it would be one instant
//! against PCRs a second apart, and tsdemux's estimate of the broadcaster's
//! clock (mpegtspacketizer's skew) swings for the first seconds: pictures
//! timed 9-60 ms off their place, dropped or doubled -- 100-660 ms of
//! picture lost at every opening (measured 2026-10-30, eleven openings;
//! with tsdemux's ignore-pcr none lost, and the sound off the picture).
//!
//! So the clock is a tuner's STC, driven by the PCR (AOSP, Multimedia
//! tunneling: on live broadcast the A/V sync clock is "a program reference
//! clock (PCR) or system time clock (STC) driven by a tuner"): every chunk is
//! timed by its PCR from the first one's arrival, at the broadcast's own
//! pace -- the burst, and everything after it.
//!
//! Not on arrival plus the lead the burst left, as before: after a retune
//! there is no burst, and the receiver's USB transfers deliver the stream in
//! uneven lumps. Stamped on arrival, those lumps reached tsdemux's estimate
//! of the broadcaster's clock, and the decoder's input was timed 7 to 20 ms
//! apart for 20 ms pictures for the first second (NTV HD after a retune,
//! 2026-10-31): 9-68 ms steps on the panel's side, the picture running
//! ahead of its sound by 108 ms.
//!
//! Nor is a chunk's arrival a broadcast time in the first seconds after a
//! retune: the receiver then hands the stream over at a third of its rate
//! for two seconds and the rest at once (NTV HD, 2026-10-31: chunks 32 ms
//! of PCR apart arriving 100 ms apart, then two seconds of them in 10 ms).
//! Taken as late and timed from their arrival, they moved the pace by up to
//! 0.9 s at a time, and the pictures behind froze for 120-760 ms each time.
//!
//! The arrivals still steer, slowly, once the opening is long over: the
//! board's crystal and the broadcaster's 27 MHz part by tens of ppm, and over
//! hours the stream would build up or run dry. From ten seconds after the
//! first PCR, the earliest arrivals of the last two seconds against the
//! PCR's pace -- a lump comes late, never early -- are followed at no more
//! than 50 us a chunk.

/// 27 MHz PCR: a 33-bit base times 300 plus a 9-bit extension below 300.
pub const PCR_WRAP: u64 = (1 << 33) * 300;

/// How long after the first PCR the burst is taken to be in.
pub const BURST_WITHIN_NS: u64 = 1_500_000_000;

/// Two PCRs of one programme further apart than this, forwards or back,
/// are a discontinuity: DVB repeats the PCR every 40 ms at most (ETSI TS
/// 101 154), MPEG-2 every 100 ms (ISO/IEC 13818-1).
pub const PCR_JUMP_TICKS: u64 = 27_000_000;

/// `to - from` on the PCR's circle.
pub fn pcr_ticks(from: u64, to: u64) -> u64 {
    (to % PCR_WRAP + PCR_WRAP - from % PCR_WRAP) % PCR_WRAP
}

pub fn ticks_to_ns(ticks: u64) -> u64 {
    // 27 MHz -> ns, exact for the whole PCR range in u128.
    (u128::from(ticks) * 1000 / 27) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Part of the opening burst, timed by its PCR.
    Paced,
    /// After the burst: arrival plus the lead the burst left.
    Live,
}

/// One chunk's time, and what the timeline is told of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamped {
    pub at: u64,
    pub class: Class,
    /// Its PCR is a jump from the last one (`PCR_JUMP_TICKS`): the times
    /// of this opening are not to be trusted as a measurement.
    pub discontinuity: bool,
}

/// From this long after the first PCR, the arrivals steer the pace.
const SETTLE_NS: u64 = 10_000_000_000;
/// The arrivals' envelope is the earliest of this long.
const ENVELOPE_NS: u64 = 2_000_000_000;
/// How far the pace follows the arrivals' envelope in one chunk, at most.
const SLEW_NS: i64 = 50_000;

#[derive(Default)]
pub struct Stamp {
    /// The branch these times are for: a new one starts again.
    key: Option<u64>,
    /// The PCR the pace is reckoned from and the time it stands for, ns: the
    /// first one and its arrival -- or, after a discontinuity, the one there
    /// and the time already reached.
    first: Option<(u64, i64)>,
    /// When the first PCR arrived: the burst is in `BURST_WITHIN_NS` after.
    started: Option<u64>,
    last: Option<u64>,
    last_pcr: Option<u64>,
    /// After the burst: arrival minus pace, per chunk, the last
    /// `ENVELOPE_NS`; the earliest of them when the envelope was first
    /// whole; and how far the pace has followed it since, ns.
    envelope: std::collections::VecDeque<(u64, i64)>,
    base: Option<i64>,
    follow: i64,
}

impl Stamp {
    /// The time for a chunk of branch `key` that arrived at `arrived` (ns,
    /// running time) carrying `pcr` (its first, 27 MHz) if any.
    pub fn time(&mut self, key: u64, arrived: u64, pcr: Option<u64>) -> Stamped {
        if self.key != Some(key) {
            *self = Stamp { key: Some(key), ..Stamp::default() };
        }
        // Forwards on the circle: a wrap is a small step, any step back a
        // whole day's.
        let discontinuity = matches!((self.last_pcr, pcr), (Some(last), Some(pcr)) if pcr_ticks(last, pcr) > PCR_JUMP_TICKS);
        if pcr.is_some() {
            self.last_pcr = pcr;
        }
        let in_burst = self.started.is_none_or(|started| arrived < started + BURST_WITHIN_NS);
        let class = if in_burst { Class::Paced } else { Class::Live };
        // A discontinuity (a splice, a playout switch): the PCRs before it
        // say nothing of the ones after. The pace starts again from it, at
        // the time already reached -- paced from the old PCR it ran a day
        // ahead for a jump back.
        if discontinuity && let Some(pcr) = pcr {
            let at = self.last.map_or(arrived, |last| last.max(arrived)) as i64;
            self.first = Some((pcr, at - self.follow));
        }
        let at = match (self.first, pcr) {
            (None, Some(pcr)) => {
                self.first = Some((pcr, arrived as i64));
                self.started = Some(arrived);
                arrived
            }
            (None, None) => self.last.map_or(arrived, |last| last.max(arrived)),
            // Between PCRs: where the last one put it.
            (Some(_), None) => self.last.unwrap_or(arrived),
            (Some((first, at)), Some(pcr)) => {
                let paced = at + ticks_to_ns(pcr_ticks(first, pcr)) as i64;
                let settled = self.started.is_some_and(|started| arrived >= started + SETTLE_NS);
                if !settled {
                    paced.max(0) as u64
                } else {
                    let late = arrived as i64 - paced;
                    self.envelope.push_back((arrived, late));
                    while self.envelope.front().is_some_and(|&(t, _)| t + ENVELOPE_NS < arrived) {
                        self.envelope.pop_front();
                    }
                    let earliest = self.envelope.iter().map(|&(_, late)| late).min().unwrap_or(late);
                    let whole = self.envelope.front().is_some_and(|&(t, _)| t + ENVELOPE_NS * 9 / 10 <= arrived);
                    if whole {
                        let base = *self.base.get_or_insert(earliest);
                        self.follow += (earliest - base - self.follow).clamp(-SLEW_NS, SLEW_NS);
                    }
                    (paced + self.follow).max(0) as u64
                }
            }
        };
        let at = self.last.map_or(at, |last| last.max(at));
        self.last = Some(at);
        Stamped { at, class, discontinuity }
    }
}

fn pcr_of(p: &[u8]) -> Option<u64> {
    let pcr = p[0] == 0x47 && p[3] & 0x20 != 0 && p[4] >= 7 && p[5] & 0x10 != 0;
    pcr.then(|| {
        let base = (u64::from(p[6]) << 25)
            | (u64::from(p[7]) << 17)
            | (u64::from(p[8]) << 9)
            | (u64::from(p[9]) << 1)
            | (u64::from(p[10]) >> 7);
        base * 300 + ((u64::from(p[10] & 1) << 8) | u64::from(p[11]))
    })
}

const PACKET: usize = 188;

fn pid_of(p: &[u8]) -> u16 {
    (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2])
}

/// A piece of the stream for one buffer: whole packets, starting at a PCR
/// packet of the programme's PCR PID (but for what came before the first).
#[derive(Debug, PartialEq, Eq)]
pub struct Piece {
    pub data: Vec<u8>,
    pub pcr: Option<u64>,
}

/// The receiver's stream as read from its FIFO, cut where the stamps
/// belong: each PCR at the start of a buffer of its own.
///
/// tsdemux gives every PCR in a buffer that buffer's time. A read of the
/// FIFO is 40 to 65 KB, two or three PCRs 30 ms apart, so stamped once per
/// read they all had one time: tsdemux's estimate of the broadcaster's clock
/// came out 2.6 % fast (TRT HABER HD after a retune, 2026-10-31: 542 pictures
/// timed over 11.1 s), the pictures ran ahead of the grid they are shown on
/// and were caught up every two seconds -- a picture held 60-140 ms each
/// time. Cut at each PCR, every PCR has its own time on the PCR's pace and
/// tsdemux finds the clocks agreeing.
///
/// The reads are not whole packets either (61 552 bytes is 327.4 packets):
/// the part packet is kept for the next read, so the packets are found where
/// they start, not by looking for 0x47 in the middle of a payload -- found
/// that way, a payload made two PCRs 34 hours apart in one read (STAR TV,
/// 2026-10-31). The stream's sync is looked for over three packets, and only
/// PCRs of one PID count: the programme's.
#[derive(Default)]
pub struct Packetizer {
    carry: Vec<u8>,
    pcr_pid: Option<u16>,
}

impl Packetizer {
    /// A new programme on the same stream: its PCR PID is found again.
    pub fn new_programme(&mut self) {
        self.pcr_pid = None;
    }

    pub fn cut(&mut self, read: &[u8]) -> Vec<Piece> {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(read);
        let mut pieces = Vec::new();
        let mut piece = Piece { data: Vec::new(), pcr: None };
        let mut at = 0;
        loop {
            // In sync: a packet starts here, and the two after it where they
            // should (or the data ends first).
            let synced = |k: usize| (0..3).all(|n| data.get(k + n * PACKET).is_none_or(|&b| b == 0x47));
            if at < data.len() && (data[at] != 0x47 || !synced(at)) {
                let Some(next) = (at..data.len()).find(|&k| data[k] == 0x47 && synced(k)) else {
                    // No sync in what is here: kept, up to two packets' worth,
                    // for the next read to find it with.
                    let keep = data.len().saturating_sub(2 * PACKET).max(at);
                    piece.data.extend_from_slice(&data[at..keep]);
                    at = keep;
                    break;
                };
                piece.data.extend_from_slice(&data[at..next]);
                at = next;
            }
            if at + PACKET > data.len() {
                break;
            }
            let packet = &data[at..at + PACKET];
            if let Some(pcr) = pcr_of(packet)
                && *self.pcr_pid.get_or_insert(pid_of(packet)) == pid_of(packet)
            {
                if !piece.data.is_empty() || piece.pcr.is_some() {
                    pieces.push(std::mem::replace(&mut piece, Piece { data: Vec::new(), pcr: Some(pcr) }));
                } else {
                    piece.pcr = Some(pcr);
                }
            }
            piece.data.extend_from_slice(packet);
            at += PACKET;
        }
        if !piece.data.is_empty() {
            pieces.push(piece);
        }
        self.carry = data[at..].to_vec();
        pieces
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;
    /// One 20 ms field of 50 Hz, in PCR ticks.
    const FIELD_50: u64 = 540_000;
    /// One 1001/60000 s frame of 59.94 Hz, in PCR ticks.
    const FRAME_5994: u64 = 450_450;

    fn ts_packet_with_pcr(pcr: u64) -> [u8; 188] {
        let mut p = [0xFFu8; 188];
        let (base, ext) = (pcr / 300, pcr % 300);
        p[..6].copy_from_slice(&[0x47, 0x01, 0x00, 0x30, 7, 0x10]);
        p[6] = (base >> 25) as u8;
        p[7] = (base >> 17) as u8;
        p[8] = (base >> 9) as u8;
        p[9] = (base >> 1) as u8;
        p[10] = (((base & 1) << 7) as u8) | 0x7E | ((ext >> 8) as u8);
        p[11] = ext as u8;
        p
    }

    fn plain(pid: u16, fill: u8) -> [u8; 188] {
        let mut p = [fill; 188];
        p[..4].copy_from_slice(&[0x47, (pid >> 8) as u8, pid as u8, 0x10]);
        p
    }

    /// A stream of PCR packets (PID 0x100) every fifth packet, 0x47 bytes
    /// 188 apart in the payloads between, a second PCR PID among them.
    fn stream() -> (Vec<u8>, Vec<u64>) {
        let mut data = Vec::new();
        let mut pcrs = Vec::new();
        for k in 0..400u64 {
            if k % 5 == 0 {
                data.extend_from_slice(&ts_packet_with_pcr(1_000 + k * 27_000));
                pcrs.push(1_000 + k * 27_000);
            } else if k % 7 == 0 {
                // Another programme's PCR: not this one's clock.
                let mut other = ts_packet_with_pcr(999_999_999);
                other[1] = 0x02;
                data.extend_from_slice(&other);
            } else {
                data.extend_from_slice(&plain(0x101, 0x47));
            }
        }
        (data, pcrs)
    }

    #[test]
    fn reads_of_any_size_are_cut_at_each_pcr_and_lose_nothing() {
        let (data, pcrs) = stream();
        for sizes in [vec![61_552usize], vec![65_424], vec![1_000, 7, 4_096], vec![187, 189, 376]] {
            let mut packetizer = Packetizer::default();
            // Joined mid-packet, as the first read of a FIFO can be.
            let start = 100;
            let mut pieces = Vec::new();
            let (mut at, mut n) = (start, 0);
            while at < data.len() {
                let size = sizes[n % sizes.len()].min(data.len() - at);
                pieces.extend(packetizer.cut(&data[at..at + size]));
                at += size;
                n += 1;
            }
            let joined: Vec<u8> = pieces.iter().flat_map(|piece| piece.data.iter().copied()).collect();
            assert_eq!(joined.len() + packetizer.carry.len(), data.len() - start, "{sizes:?}: nothing lost");
            assert_eq!(&joined[..], &data[start..start + joined.len()], "{sizes:?}");
            let seen: Vec<u64> = pieces.iter().filter_map(|piece| piece.pcr).collect();
            assert_eq!(seen, pcrs[1..], "{sizes:?}: every PCR of the one PID, in order, none from a payload");
            for piece in pieces.iter().filter(|piece| piece.pcr.is_some()) {
                assert_eq!(pcr_of(&piece.data[..188]), piece.pcr, "{sizes:?}: a piece starts at its PCR");
            }
        }
    }

    #[test]
    fn the_opening_burst_is_timed_at_the_broadcasts_pace() {
        let mut stamp = Stamp::default();
        // One second of the broadcast, all arriving within 10 ms.
        for k in 0..50 {
            let Stamped { at, class, .. } =
                stamp.time(1, 100 * MS + k * MS / 5, Some(1_000_000 + k * FIELD_50));
            assert_eq!(at, 100 * MS + k * 20 * MS, "field {k}");
            assert_eq!(class, Class::Paced);
        }
    }

    #[test]
    fn after_the_burst_arrival_plus_the_lead_left_and_no_jump() {
        let mut stamp = Stamp::default();
        let t0 = 1_000 * MS;
        // 0.8 s of burst at once, then the live edge at 20 ms per field.
        for k in 0..40 {
            stamp.time(1, t0, Some(k * FIELD_50));
        }
        let mut last = 0;
        for k in 40..200 {
            let arrived = t0 + (k - 40) * 20 * MS;
            let Stamped { at, class, .. } = stamp.time(1, arrived, Some(k * FIELD_50));
            assert!(at >= last, "never back");
            if k > 40 {
                assert_eq!(at - last, 20 * MS, "no jump at field {k} ({class:?})");
            }
            last = at;
        }
        // The burst's 0.8 s is a constant lead: the channel plays that far
        // behind the live edge for good.
        assert_eq!(last - (t0 + 159 * 20 * MS), 800 * MS);
    }

    #[test]
    fn pcr_ticks_on_the_circle() {
        assert_eq!(pcr_ticks(PCR_WRAP - 1, 0), 1, "the last tick to the first");
        assert_eq!(pcr_ticks(PCR_WRAP - 1, PCR_WRAP - 1), 0);
        assert_eq!(
            pcr_ticks(0, PCR_WRAP - 1),
            PCR_WRAP - 1,
            "a step back is a whole day forwards"
        );
        assert_eq!(
            pcr_ticks(PCR_WRAP - FIELD_50, 0),
            FIELD_50,
            "landing exactly on the wrap"
        );
        assert_eq!(pcr_ticks(5, 5 + FIELD_50), FIELD_50, "just after it");
    }

    /// Opened just before the wrap, exactly on it and just after it: the
    /// burst goes on at one field a field, and no wrap is a discontinuity.
    #[test]
    fn a_pcr_wrap_before_at_and_after_the_opening_keeps_the_pace() {
        for first in [
            PCR_WRAP - 5 * FIELD_50 - 7, // wraps inside the burst, between two PCRs
            PCR_WRAP - 5 * FIELD_50,     // the 6th PCR is exactly 0
            PCR_WRAP - 1,                // the next is just after the wrap
            0,                           // opened exactly on it
            3,                           // just after it
        ] {
            let mut stamp = Stamp::default();
            for k in 0..20 {
                let pcr = (first + k * FIELD_50) % PCR_WRAP;
                let stamped = stamp.time(1, 0, Some(pcr));
                assert_eq!(
                    stamped.at,
                    ticks_to_ns(k * FIELD_50),
                    "first {first}, field {k}, pcr {pcr}"
                );
                assert!(
                    !stamped.discontinuity,
                    "first {first}, field {k}: a wrap is no discontinuity"
                );
            }
        }
    }

    #[test]
    fn a_pcr_jump_either_way_is_flagged_and_the_pace_is_not() {
        for jump_to in [90 * 27_000_000u64, 100 * 27_000_000 + 2 * 27_000_000] {
            let mut stamp = Stamp::default();
            assert!(!stamp.time(1, 0, Some(100 * 27_000_000)).discontinuity);
            assert!(
                !stamp
                    .time(1, 20 * MS, Some(100 * 27_000_000 + FIELD_50))
                    .discontinuity
            );
            assert!(
                stamp.time(1, 40 * MS, Some(jump_to)).discontinuity,
                "{jump_to}"
            );
            assert!(
                !stamp
                    .time(1, 60 * MS, Some(jump_to + FIELD_50))
                    .discontinuity,
                "after the jump"
            );
        }
    }

    #[test]
    fn fifty_and_fifty_nine_point_nine_four_do_not_drift() {
        for step in [FIELD_50, FRAME_5994] {
            let mut stamp = Stamp::default();
            let mut at = 0;
            for k in 0..74 {
                at = stamp.time(7, 0, Some(k * step)).at;
            }
            let exact = 73u128 * u128::from(step) * 1000 / 27;
            assert!(
                (i128::from(at) - exact as i128).abs() <= 1,
                "{step}: {at} vs {exact}"
            );
        }
    }

    #[test]
    fn a_new_branch_starts_its_own_clock() {
        let mut stamp = Stamp::default();
        stamp.time(1, 0, Some(0));
        stamp.time(1, 0, Some(27_000_000));
        let stamped = stamp.time(2, 5 * MS, Some(999));
        assert_eq!(
            (stamped.at, stamped.class, stamped.discontinuity),
            (5 * MS, Class::Paced, false)
        );
    }

    #[test]
    fn a_chunk_without_a_pcr_is_never_before_the_one_before() {
        let mut stamp = Stamp::default();
        stamp.time(1, 0, Some(0));
        let paced = stamp.time(1, MS, Some(10 * FIELD_50)).at;
        let at = stamp.time(1, 2 * MS, None).at;
        assert_eq!(at, paced);
    }

    /// A PCR discontinuity inside the opening burst, back or forwards: the
    /// stamps stay where they were and go on at the broadcast's pace from
    /// the new PCR -- never a day ahead -- and the discontinuity is said.
    #[test]
    fn a_pcr_discontinuity_in_the_burst_does_not_run_away() {
        for jump_to in [90 * 27_000_000u64, 3_000 * 27_000_000, PCR_WRAP - 5 * FIELD_50] {
            let mut stamp = Stamp::default();
            let start = 100 * 27_000_000;
            // Four fields of burst, arriving at once.
            for k in 0..4 {
                let stamped = stamp.time(1, 0, Some(start + k * FIELD_50));
                assert_eq!(stamped.at, k * 20 * MS);
                assert!(!stamped.discontinuity);
            }
            let jumped = stamp.time(1, MS, Some(jump_to));
            assert!(jumped.discontinuity, "{jump_to}: said");
            assert_eq!(jumped.at, 60 * MS, "{jump_to}: where the burst had got to, not ahead");
            // On at 20 ms a field from the new PCR, across its own wrap too.
            for k in 1..10 {
                let pcr = (jump_to + k * FIELD_50) % PCR_WRAP;
                let stamped = stamp.time(1, 2 * MS, Some(pcr));
                assert!(!stamped.discontinuity, "{jump_to} field {k}");
                assert_eq!(stamped.at, 60 * MS + k * 20 * MS, "{jump_to} field {k}");
            }
            assert!(stamp.time(1, 3 * MS, None).at <= 3 * MS + BURST_WITHIN_NS);
        }
    }

    /// The burst is in 1.5 s after the first PCR arrived, a discontinuity
    /// in it or not: the window is not started again.
    #[test]
    fn a_discontinuity_does_not_lengthen_the_burst() {
        let mut stamp = Stamp::default();
        stamp.time(1, 0, Some(0));
        stamp.time(1, 700 * MS, Some(50 * 27_000_000));
        stamp.time(1, 1_500 * MS, Some(50 * 27_000_000 + 40 * FIELD_50));
        let live = stamp.time(1, 1_520 * MS, Some(0));
        assert_eq!(live.class, Class::Live);
        assert!(live.discontinuity, "said, though it times nothing now");
    }

    /// After a retune there is no burst: the stream comes as the receiver's
    /// USB transfers deliver it, in lumps up to 60 ms late. The stamps are
    /// the PCR's pace, every one: none of the lumps reaches tsdemux.
    #[test]
    fn uneven_arrivals_are_timed_by_the_pcr() {
        let mut stamp = Stamp::default();
        let t0 = 5_000 * MS;
        let mut last = None;
        for k in 0..2_000u64 {
            // Two PCRs (40 ms) a chunk; lumps of three late by 0-60 ms.
            let lateness = [0, 30, 60, 10, 45, 0, 20][(k % 7) as usize] * MS;
            let stamped = stamp.time(1, t0 + k * 40 * MS + lateness, Some(k * 2 * FIELD_50));
            if let Some(last) = last {
                assert_eq!(stamped.at - last, 40 * MS, "chunk {k}");
            }
            last = Some(stamped.at);
        }
    }

    /// The board's clock against the broadcaster's: the arrivals come 100
    /// ppm earlier than the PCR's pace says, for ten minutes. The stamps
    /// follow the earliest arrivals, never by more than 50 us a chunk, and
    /// stay within a few ms of them.
    #[test]
    fn a_clock_drift_is_followed_slowly() {
        let mut stamp = Stamp::default();
        let t0 = 5_000 * MS;
        let mut last = 0;
        for k in 0..15_000u64 {
            let paced = k * 40 * MS;
            let arrived = t0 + paced - paced / 10_000 + [0, 25, 5][(k % 3) as usize] * MS;
            let stamped = stamp.time(1, arrived, Some(k * 2 * FIELD_50));
            if k > 0 {
                let step = stamped.at - last;
                assert!(step.abs_diff(40 * MS) <= 50_000, "chunk {k}: {step}");
            }
            if k > 1_000 {
                let earliest = t0 as i64 + paced as i64 - (paced / 10_000) as i64;
                assert!((stamped.at as i64 - earliest).abs() <= 3 * MS as i64, "chunk {k}");
            }
            last = stamped.at;
        }
    }

    /// After a retune, as measured (NTV HD, 2026-10-31): chunks of 32 ms of
    /// PCR arriving 100 ms apart for two seconds, then the rest in a lump.
    /// The stamps are the PCR's pace throughout: no jump anywhere.
    #[test]
    fn a_slow_start_and_a_lump_are_timed_by_the_pcr() {
        let mut stamp = Stamp::default();
        let mut last = None;
        for k in 0..400u64 {
            let arrived = if k < 20 { 1_000 * MS + k * 100 * MS } else { 3_000 * MS + (k - 20) * 32 * MS / 4 };
            let stamped = stamp.time(1, arrived, Some(k * 32 * 27_000));
            if let Some(last) = last {
                assert_eq!(stamped.at - last, 32 * MS, "chunk {k}");
            }
            last = Some(stamped.at);
        }
    }
}
