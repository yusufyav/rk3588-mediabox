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
//! first PCR the receiver's clock is recovered as a frequency, not chased as
//! a phase (`Recovery`). Until 2026-11-04 the pace followed the earliest
//! arrivals of the last two seconds by up to 50 us a chunk: a step per PCR,
//! not per second, so 25 to 50 PCRs a second could move it 1.2 to 2.5 ms a
//! second. The envelope wanders by milliseconds with the USB transfers, and
//! the pace wandered with it: every sound PES timed by it (`pts_on_clock`)
//! was off the one before by the step, and the sound's timestamps ran ahead
//! of its samples (51616, 2026-11-04: the parser's PTS less its frames
//! equal to the follow, to the nanosecond).
//!
//! What the reader of the FIFO stamps is when its `read` returned, not when
//! the tuner received the data: the receiver's USB transfers, its queue and
//! the FIFO lie between. Their delays are late, never early, which is why
//! only the earliest arrivals are taken.

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
    /// The arrivals stepped later by this much (ns) at this chunk
    /// (`STEP_NS`): seen, and kept out of the clock; the stamps go on.
    pub stepped: Option<i64>,
}

/// From this long after the first PCR, the arrivals steer the pace.
const SETTLE_NS: u64 = 10_000_000_000;
/// The arrivals' envelope for the phase: the earliest of this long.
const ENVELOPE_NS: u64 = 2_000_000_000;
/// One point of the frequency fit per this much of arrival time: the
/// earliest arrival in it against the PCR's pace.
const POINT_NS: u64 = 1_000_000_000;
/// The fit is over this many points (a minute), and starts with this many.
const FIT_POINTS: usize = 60;
const FIT_FROM: usize = 20;
/// A point this far above the fit is a lump (late, never early): fitted
/// again without it.
const LUMP_NS: i64 = 2_000_000;
/// The frequency correction, ppb, at most: the broadcaster's 27 MHz may be
/// 30 ppm off (ISO/IEC 13818-1 2.4.2.1: 810 Hz), the board's crystal is
/// taken to be within 70 more.
const MAX_PPB: i64 = 100_000;
/// The phase is brought back over this long (s), by a frequency of at most
/// `MAX_PHASE_PPB` above the fitted one.
const PHASE_TAU_S: i64 = 300;
const MAX_PHASE_PPB: i64 = 20_000;
/// How fast the frequency applied may change, ppb per second of arrival
/// time: a PTS a frame after another is never more than a nanosecond off
/// the frame's own length for it.
const SLEW_PPB_PER_S: i64 = 1_000;
/// A step in the arrivals: every arrival of the last `STEP_WINDOW_NS` later
/// than the settled envelope by more than `STEP_NS`. Within a second the
/// arrivals spread by up to 25 ms around their envelope (51616,
/// 2026-11-04); a stall upstream that never catches up moves all of them,
/// 86 to 313 ms at a time on that service.
const STEP_WINDOW_NS: u64 = 300_000_000;
const STEP_NS: i64 = 40_000_000;
/// A PCR this much further on than the time since the last one is a new
/// time base without its indicator (ISO/IEC 13818-1 allows 100 ms between
/// PCRs; a lost one moves both alike).
const AHEAD_JUMP_NS: i64 = 250_000_000;

/// The receiver's clock against the broadcaster's, from the arrivals.
///
/// The stamps are the PCR's pace from an anchor, `at = at0 + E + C(E)`, `E`
/// the PCR's time since the anchor and `C` the correction: a frequency
/// (ppb) applied from where it was last changed, so the map is one
/// continuous line of slowly changing slope -- no step anywhere, whatever
/// the arrivals do. The frequency is a least-squares fit over a minute of
/// points, each the earliest arrival of one second against the PCR's pace
/// (a lump comes late, never early; points above the fit are dropped), plus
/// a phase term that brings the arrivals' 2 s envelope back to where it was
/// when the clock settled. Both are updated once a second of arrival time:
/// how often PCRs come, or how the stream is cut into reads, changes
/// nothing. Integer throughout: `E` from the PCR's ticks since the anchor,
/// the correction kept in ns * 1e9.
#[derive(Default, Clone)]
struct Recovery {
    /// Where the frequency was last changed: `E` there, and the correction
    /// reached, ns * 1e9.
    seg_e: i128,
    seg_c: i128,
    ppb: i64,
    /// The second being taken (arrival / `POINT_NS`), and its earliest
    /// (E, arrival - at0 - E).
    second: Option<u64>,
    point: Option<(i64, i64)>,
    points: std::collections::VecDeque<(i64, i64)>,
    /// Arrival less the map, the last `ENVELOPE_NS`; and its earliest when
    /// it was first whole.
    envelope: std::collections::VecDeque<(u64, i64)>,
    base: Option<i64>,
    fitted: Option<i64>,
    /// The arrivals' steps (`STEP_NS`), summed: lateness, kept out of the
    /// fit and the phase -- neither a frequency nor this clock's phase.
    jump: i64,
    /// The step seen at the last chunk, if any.
    stepped: Option<i64>,
}

impl Recovery {
    fn correction(&self, e: i64) -> i64 {
        ((self.seg_c + (i128::from(e) - self.seg_e) * i128::from(self.ppb)) / 1_000_000_000) as i64
    }

    /// The least-squares slope (ppb) of the points, once without the ones
    /// more than `LUMP_NS` above the first fit.
    fn fit(points: &std::collections::VecDeque<(i64, i64)>) -> Option<i64> {
        let line = |keep: &dyn Fn(i64, i64) -> bool| -> Option<(f64, f64)> {
            let kept: Vec<(f64, f64)> = points.iter().filter(|&&(x, y)| keep(x, y)).map(|&(x, y)| (x as f64, y as f64)).collect();
            if kept.len() < FIT_FROM {
                return None;
            }
            let n = kept.len() as f64;
            let (mx, my) = (kept.iter().map(|p| p.0).sum::<f64>() / n, kept.iter().map(|p| p.1).sum::<f64>() / n);
            let sxx: f64 = kept.iter().map(|p| (p.0 - mx).powi(2)).sum();
            let sxy: f64 = kept.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
            (sxx > 0.0).then(|| {
                let b = sxy / sxx;
                (b, my - b * mx)
            })
        };
        let (b, a) = line(&|_, _| true)?;
        let (b, _) = line(&|x, y| (y as f64) - (a + b * x as f64) <= LUMP_NS as f64).unwrap_or((b, a));
        Some((b * 1e9).round() as i64)
    }

    /// A chunk with a PCR at `e` (ns since the anchor) arrived at
    /// `arrived`, `at0` the anchor's time. Returns the correction at `e`.
    fn observe(&mut self, at0: i64, e: i64, arrived: u64, trace: &mut crate::clock_probe::StampTrace) -> i64 {
        let pure = arrived as i64 - at0 - e;
        let second = arrived / POINT_NS;
        if self.second != Some(second) {
            if let Some(point) = self.point.take() {
                self.points.push_back(point);
                while self.points.len() > FIT_POINTS {
                    self.points.pop_front();
                }
                self.update(e);
            }
            self.second = Some(second);
        }
        // The fit sees the arrivals without the steps: a frequency is not
        // made of them.
        if self.point.is_none_or(|(_, y)| pure - self.jump < y) {
            self.point = Some((e, pure - self.jump));
        }
        // The map is the PCR's: a step in the arrivals is lateness, said to
        // the presentation (`Presentation`), never a phase of this clock.
        let correction = self.correction(e);
        let mut late = pure - correction - self.jump;
        self.envelope.push_back((arrived, late));
        while self.envelope.front().is_some_and(|&(t, _)| t + ENVELOPE_NS < arrived) {
            self.envelope.pop_front();
        }
        // A step: everything that came in the last `STEP_WINDOW_NS` was that
        // much later. Taken at once, as a phase, where the arrivals now are.
        self.stepped = None;
        if let Some(base) = self.base {
            // Arrivals across the whole window, not one lump after a hold:
            // a held reader's backlog comes at one instant and catches up.
            let mut recent = self.envelope.iter().filter(|&&(t, _)| t + STEP_WINDOW_NS >= arrived).peekable();
            let whole = recent.peek().is_some_and(|&&(t, _)| t + STEP_WINDOW_NS * 9 / 10 <= arrived);
            let least = recent.map(|&(_, l)| l).min().unwrap_or(late);
            if whole && least - base > STEP_NS {
                let step = least - base;
                self.jump += step;
                self.stepped = Some(step);
                late -= step;
                for (_, l) in self.envelope.iter_mut() {
                    *l -= step;
                }
            }
        }
        let earliest = self.envelope.iter().map(|&(_, l)| l).min().unwrap_or(late);
        if self.envelope.front().is_some_and(|&(t, _)| t + ENVELOPE_NS * 9 / 10 <= arrived) {
            self.base.get_or_insert(earliest);
        }
        trace.late = late;
        trace.earliest = earliest;
        trace.base = self.base.unwrap_or(0);
        trace.follow = correction;
        correction
    }

    /// Once a second: the frequency towards the fit plus the phase term,
    /// by at most `SLEW_PPB_PER_S` a second.
    fn update(&mut self, e: i64) {
        self.fitted = Self::fit(&self.points);
        let (Some(fitted), Some(base)) = (self.fitted, self.base) else { return };
        let earliest = self.envelope.iter().map(|&(_, l)| l).min().unwrap_or(base);
        let phase = ((earliest - base) / PHASE_TAU_S).clamp(-MAX_PHASE_PPB, MAX_PHASE_PPB);
        let target = (fitted + phase).clamp(-MAX_PPB, MAX_PPB);
        let ppb = self.ppb + (target - self.ppb).clamp(-SLEW_PPB_PER_S, SLEW_PPB_PER_S);
        // The correction carried to here, and the new slope from here.
        self.seg_c += (i128::from(e) - self.seg_e) * i128::from(self.ppb);
        self.seg_e = i128::from(e);
        self.ppb = ppb;
    }

    /// A new time base: the points and the phase said nothing of it; the
    /// crystals' frequency is kept.
    fn rebase(&mut self) {
        *self = Recovery { ppb: self.ppb, ..Recovery::default() };
    }

    pub fn stepped(&self) -> Option<i64> {
        self.stepped
    }
}

#[derive(Default)]
pub struct Stamp {
    /// The branch these times are for: a new one starts again.
    key: Option<u64>,
    /// The anchor: its time (ns), and the PCR ticks since it -- summed step
    /// by step, so the PCR's wrap every 26.5 hours is a step like any.
    at0: Option<i64>,
    ticks: u128,
    /// When the first PCR arrived: the burst is in `BURST_WITHIN_NS` after.
    started: Option<u64>,
    last: Option<u64>,
    last_pcr: Option<(u64, u64)>,
    clock: Recovery,
    /// The next PCR anchors anew (`reanchor`).
    reanchor: bool,
    /// The last chunk with a PCR, for the clock probe.
    pub trace: Option<crate::clock_probe::StampTrace>,
}

/// Lateness of the stream that the presentation takes up: the stamps stay
/// the PCR's (`Recovery`), the sinks wait longer.
///
/// On 51616 (2026-11-05, an hour of the raw multiplex): the service's PCR
/// and its payload came later together, by 80 to 381 ms at a time (p50
/// 149, p95 376), 2.7 s in the hour and never back, while the USB
/// transfers came every 21 ms and the multiplex's other services on time:
/// a service delivered late at its source, its time base whole. Its
/// pictures came late 0.4 to 4 s before its PCRs did; of the 151 pictures
/// lost in 45 minutes, 136 had reached the decoder 40 to 580 ms later than
/// the minute's before them. Nothing can show that time; the sinks wait it
/// out once, as a pause, and nothing is dropped or resynced.
///
/// What is judged is the pictures' own lateness as they leave tsdemux
/// (now less PTS, running time): each has its time at the sink the
/// pipeline's latency (150 ms) and the offset later, and decoding and the
/// vblank take up to 80 ms of it. Two running more than
/// `PICTURE_ALLOWED_NS` late, the offset grows by the lateness and
/// `LATE_MARGIN_NS` (the pictures then `LATE_MARGIN_NS` ahead of their PTS
/// again). A PCR's lateness is no measure of it: the pictures come ahead of
/// their PCR by 170 ms here, and the sound by 250.
///
/// Only the input moves it: a picture's lateness is measured as it
/// leaves tsdemux, against the offset already taken, so the sinks' own
/// wait (`ts-offset`) is never new lateness. A sink-side watch that took a
/// picture not yet shown for a stop (FAZ 1H, `observe_gap`) fed on its own
/// waits: on 51616 in two hours (2026-11-05) 79 of its 80 raises came with
/// the pictures at tsdemux 0.6 to 4 s early for the offset, 17.7 s in all
/// for about 5 s the service came later, and three rebases. Removed.
pub const PICTURE_ALLOWED_NS: i64 = 70_000_000;
pub const LATE_MARGIN_NS: i64 = 20_000_000;
/// At most this much later than the stamps: the streams' queues hold 6 s
/// (`STREAM_QUEUE_NS`); past it the channel's time is taken anew at its
/// arrivals, once, and what waited is skipped. 51616 came 2.7 s later in
/// an hour: about once in two hours there.
pub const LATE_MAX_NS: u64 = 5_000_000_000;
/// Within this of a wait a picture up to `WAIT_SHOW_LATE_NS` past its time
/// is shown, not thrown away: the wait was taken the moment the stop was
/// seen, and the pictures already on their way when it was are the ones
/// it was for (51616, 2026-11-05: 19 pictures in 45 minutes, 20 to 111 ms
/// late, every one within 0.5 s of a wait).
pub const WAIT_SHOW_NS: u64 = 1_000_000_000;
pub const WAIT_SHOW_LATE_NS: u64 = 150_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Late {
    /// The presentation now waits `offset` (ns) past the stamps, `by` more
    /// than before.
    Raised { offset: u64, by: i64 },
    /// Past `LATE_MAX_NS`: the stamps are to be anchored at the arrivals
    /// again (`Stamp::reanchor`), the presentation back to 0.
    Rebase,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presentation {
    key: Option<u64>,
    offset: u64,
    /// The last picture's lateness past the offset (`observe_picture`), and
    /// the newest PTS yet.
    last_picture: Option<i64>,
    newest: Option<u64>,
    /// A picture's time: the newest PTS's last step forwards (under 100 ms).
    frame: Option<u64>,
    /// A rebase asked for that the reader has not taken (`take_rebase`).
    rebase: bool,
    /// When the last wait was taken (running time).
    waited_at: Option<u64>,
}

impl Presentation {
    /// A picture of branch `key` leaving tsdemux at `now`, its PTS `pts`
    /// (both running time): late twice running, the presentation waits.
    pub fn observe_picture(&mut self, key: u64, now: u64, pts: u64) -> Option<Late> {
        if self.key != Some(key) {
            *self = Presentation { key: Some(key), ..Presentation::default() };
        }
        if let Some(newest) = self.newest
            && pts > newest
            && pts - newest < 100_000_000
        {
            self.frame = Some(pts - newest);
        }
        self.newest = Some(self.newest.map_or(pts, |n| n.max(pts)));
        let late = now as i64 - pts as i64 - self.offset as i64;
        let before = self.last_picture.replace(late);
        if late <= PICTURE_ALLOWED_NS || before.is_none_or(|b| b <= PICTURE_ALLOWED_NS) {
            return None;
        }
        self.waited_at = Some(now);
        self.raise(late)
    }

    /// The reader takes a rebase asked for here (`Late::Rebase` from a
    /// picture, on another thread).
    pub fn take_rebase(&mut self) -> bool {
        std::mem::take(&mut self.rebase)
    }

    fn raise(&mut self, late: i64) -> Option<Late> {
        let offset = self.offset + (late + LATE_MARGIN_NS) as u64;
        if offset > LATE_MAX_NS {
            *self = Presentation { key: self.key, rebase: true, ..Presentation::default() };
            return Some(Late::Rebase);
        }
        let by = offset as i64 - self.offset as i64;
        self.offset = offset;
        // The pictures that made this raise are spent: the next takes two
        // more, late past the new offset (a step is waited for once).
        self.last_picture = None;
        Some(Late::Raised { offset, by })
    }

    #[cfg(test)]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// A picture of branch `key` at `now` (running time) past its time by
    /// `late`: shown, within a wait (`WAIT_SHOW_NS`, `WAIT_SHOW_LATE_NS`).
    pub fn shows_late(&self, key: u64, now: u64, late: u64) -> bool {
        self.key == Some(key) && late <= WAIT_SHOW_LATE_NS && self.waited_at.is_some_and(|at| now.saturating_sub(at) <= WAIT_SHOW_NS)
    }

    /// The offset for branch `key`: none for another.
    pub fn offset_for(&self, key: u64) -> u64 {
        if self.key == Some(key) { self.offset } else { 0 }
    }
}

/// How long after a channel's first PCR its live edge is taken
/// (`LiveEdge`): the receiver's burst from its last access point arrives
/// within a few milliseconds, and the stream's own pace after.
pub const EDGE_WINDOW_NS: u64 = 30_000_000;

/// How far a channel's start lies behind the broadcast.
///
/// The receiver starts a channel from its last access point -- data it had
/// for up to a second, sent at once -- and the pace (`Stamp`) is reckoned
/// from the first of it: the whole channel played that far behind the
/// broadcast, 0.19 to 1.02 s, a different amount at every change
/// (2026-11-01, 32 openings). A receiver's STC follows the newest PCR it
/// got; the newest data stands for now. Each chunk with a PCR says where the
/// pace would be anchored had it been the newest (its arrival less its PCR's
/// distance from the first); the earliest of them, against the first's
/// arrival, is how far behind the start is. The sinks are then put that far
/// ahead (`ts-offset`): what came from the access point is decoded, as the
/// pictures after it need, and passes unshown; the first picture shown is
/// the one the broadcast is at. The stream's own times are not touched:
/// timed into the past, tsdemux timed its pictures 2.5 to 50 ms apart for
/// seconds, and they were caught up by skipping.
#[derive(Default)]
pub struct LiveEdge {
    first: Option<(u64, u64)>,
    earliest: i64,
    done: bool,
}

impl LiveEdge {
    /// A chunk that arrived at `arrived` with `pcr`; how far behind the
    /// start is (ns), once, when the window has passed.
    pub fn observe(&mut self, arrived: u64, pcr: Option<u64>) -> Option<u64> {
        if self.done {
            return None;
        }
        let Some((first_pcr, first_at)) = self.first else {
            if let Some(pcr) = pcr {
                self.first = Some((pcr, arrived));
                self.earliest = arrived as i64;
            }
            return None;
        };
        if let Some(pcr) = pcr {
            let at = arrived as i64 - ticks_to_ns(pcr_ticks(first_pcr, pcr)) as i64;
            self.earliest = self.earliest.min(at);
        }
        if arrived < first_at + EDGE_WINDOW_NS {
            return None;
        }
        self.done = true;
        Some((first_at as i64 - self.earliest).max(0) as u64)
    }
}

impl Stamp {
    /// The time for a chunk of branch `key` that arrived at `arrived` (ns,
    /// running time) carrying `pcr` (its first, 27 MHz) if any;
    /// `indicated`: its packet's discontinuity_indicator, a new time base
    /// from this PCR on (ISO/IEC 13818-1 2.4.3.5).
    pub fn time(&mut self, key: u64, arrived: u64, pcr: Option<u64>, indicated: bool) -> Stamped {
        if self.key != Some(key) {
            *self = Stamp { key: Some(key), ..Stamp::default() };
        }
        let in_burst = self.started.is_none_or(|started| arrived < started + BURST_WITHIN_NS);
        let class = if in_burst { Class::Paced } else { Class::Live };
        let settled = self.started.is_some_and(|started| arrived >= started + SETTLE_NS);
        // A discontinuity: indicated, a step back or over a second on (a
        // wrap is a small step forwards), or -- once settled -- a PCR much
        // further on than the time since the last one.
        let discontinuity = match (self.last_pcr, pcr) {
            (Some(_), Some(_)) if std::mem::take(&mut self.reanchor) => true,
            (Some((last, last_arrived)), Some(pcr)) => {
                let step = pcr_ticks(last, pcr);
                indicated
                    || step > PCR_JUMP_TICKS
                    || (settled && ticks_to_ns(step) as i64 - (arrived as i64 - last_arrived as i64) > AHEAD_JUMP_NS)
            }
            _ => false,
        };
        let at = match (self.at0, pcr) {
            (None, Some(_)) => {
                self.at0 = Some(arrived as i64);
                self.started = Some(arrived);
                arrived
            }
            (None, None) => self.last.map_or(arrived, |last| last.max(arrived)),
            // Between PCRs: where the last one put it.
            (Some(_), None) => self.last.unwrap_or(arrived),
            // A new time base: the PCRs before it say nothing of the ones
            // after. The pace starts again from it, at the time already
            // reached -- paced from the old PCR it ran a day ahead for a
            // jump back.
            (Some(_), Some(_)) if discontinuity => {
                let at = self.last.map_or(arrived, |last| last.max(arrived));
                self.at0 = Some(at as i64);
                self.ticks = 0;
                self.clock.rebase();
                at
            }
            (Some(at0), Some(pcr)) => {
                let (last, _) = self.last_pcr.expect("a PCR before the anchor's next");
                self.ticks += u128::from(pcr_ticks(last, pcr));
                let e = (self.ticks * 1000 / 27) as i64;
                let mut trace = crate::clock_probe::StampTrace { pcr, arrived, paced: at0 + e, settled, ..Default::default() };
                let correction = if settled { self.clock.observe(at0, e, arrived, &mut trace) } else { 0 };
                let at = (at0 + e + correction).max(0) as u64;
                trace.at = at;
                trace.delta = self.clock.ppb;
                if !settled {
                    trace.late = arrived as i64 - (at0 + e);
                }
                self.trace = Some(trace);
                at
            }
        };
        if let Some(pcr) = pcr {
            self.last_pcr = Some((pcr, arrived));
        }
        let at = self.last.map_or(at, |last| last.max(at));
        self.last = Some(at);
        let stepped = if pcr.is_some() && !discontinuity { self.clock.stepped() } else { None };
        Stamped { at, class, discontinuity, stepped }
    }

    /// The presentation is too far behind (`Late::Rebase`): the next PCR
    /// starts a new anchor at its arrival, as a new time base does.
    pub fn reanchor(&mut self) {
        self.reanchor = true;
    }

    /// The frequency applied, ppb, and the fitted one.
    #[cfg(test)]
    pub fn frequency(&self) -> (i64, Option<i64>) {
        (self.clock.ppb, self.clock.fitted)
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

pub const PACKET: usize = 188;

fn pid_of(p: &[u8]) -> u16 {
    (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2])
}

/// A piece of the stream for one buffer: whole packets, starting at a PCR
/// packet of the programme's PCR PID (but for what came before the first).
#[derive(Debug, PartialEq, Eq)]
pub struct Piece {
    pub data: Vec<u8>,
    pub pcr: Option<u64>,
    /// The discontinuity_indicator of the packet carrying `pcr`: on the
    /// PCR_PID, a system time-base discontinuity (ISO/IEC 13818-1 2.4.3.5).
    /// Carried for the clock; `Stamp` does not read it yet.
    pub discontinuity: bool,
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
        let mut piece = Piece { data: Vec::new(), pcr: None, discontinuity: false };
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
                let discontinuity = packet[5] & 0x80 != 0;
                if !piece.data.is_empty() || piece.pcr.is_some() {
                    pieces.push(std::mem::replace(&mut piece, Piece { data: Vec::new(), pcr: Some(pcr), discontinuity }));
                } else {
                    piece.pcr = Some(pcr);
                    piece.discontinuity = discontinuity;
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

    /// A start from the receiver's access point: 680 ms of the broadcast
    /// arriving within 5 ms, then the stream at its own pace: the start is
    /// 680 ms behind; a start with no burst is not behind.
    #[test]
    fn a_start_from_the_access_point_is_measured_behind_the_broadcast() {
        let pcr = |ms: u64| 1_000_000_000 + ms * 27_000;
        let mut edge = LiveEdge::default();
        let mut behind = None;
        for i in 0..=17u64 {
            behind = behind.or(edge.observe(1_000 * MS + i * 300_000, Some(pcr(i * 40))));
        }
        for i in 1..=3u64 {
            behind = behind.or(edge.observe(1_005 * MS + i * 40 * MS, Some(pcr(680 + i * 40))));
        }
        let behind = behind.expect("measured once the window has passed");
        assert!(behind.abs_diff(675 * MS) <= 6 * MS, "{behind}");

        let mut edge = LiveEdge::default();
        let mut behind = None;
        for i in 0..5u64 {
            behind = behind.or(edge.observe(1_000 * MS + i * 40 * MS + 300_000 * (i % 2), Some(pcr(i * 40))));
        }
        assert!(behind.expect("measured") <= MS, "{behind:?}");
    }
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
                stamp.time(1, 100 * MS + k * MS / 5, Some(1_000_000 + k * FIELD_50), false);
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
            stamp.time(1, t0, Some(k * FIELD_50), false);
        }
        let mut last = 0;
        for k in 40..200 {
            let arrived = t0 + (k - 40) * 20 * MS;
            let Stamped { at, class, .. } = stamp.time(1, arrived, Some(k * FIELD_50), false);
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
                let stamped = stamp.time(1, 0, Some(pcr), false);
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
            assert!(!stamp.time(1, 0, Some(100 * 27_000_000), false).discontinuity);
            assert!(
                !stamp
                    .time(1, 20 * MS, Some(100 * 27_000_000 + FIELD_50), false)
                    .discontinuity
            );
            assert!(
                stamp.time(1, 40 * MS, Some(jump_to), false).discontinuity,
                "{jump_to}"
            );
            assert!(
                !stamp
                    .time(1, 60 * MS, Some(jump_to + FIELD_50), false)
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
                at = stamp.time(7, 0, Some(k * step), false).at;
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
        stamp.time(1, 0, Some(0), false);
        stamp.time(1, 0, Some(27_000_000), false);
        let stamped = stamp.time(2, 5 * MS, Some(999), false);
        assert_eq!(
            (stamped.at, stamped.class, stamped.discontinuity),
            (5 * MS, Class::Paced, false)
        );
    }

    #[test]
    fn a_chunk_without_a_pcr_is_never_before_the_one_before() {
        let mut stamp = Stamp::default();
        stamp.time(1, 0, Some(0), false);
        let paced = stamp.time(1, MS, Some(10 * FIELD_50), false).at;
        let at = stamp.time(1, 2 * MS, None, false).at;
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
                let stamped = stamp.time(1, 0, Some(start + k * FIELD_50), false);
                assert_eq!(stamped.at, k * 20 * MS);
                assert!(!stamped.discontinuity);
            }
            let jumped = stamp.time(1, MS, Some(jump_to), false);
            assert!(jumped.discontinuity, "{jump_to}: said");
            assert_eq!(jumped.at, 60 * MS, "{jump_to}: where the burst had got to, not ahead");
            // On at 20 ms a field from the new PCR, across its own wrap too.
            for k in 1..10 {
                let pcr = (jump_to + k * FIELD_50) % PCR_WRAP;
                let stamped = stamp.time(1, 2 * MS, Some(pcr), false);
                assert!(!stamped.discontinuity, "{jump_to} field {k}");
                assert_eq!(stamped.at, 60 * MS + k * 20 * MS, "{jump_to} field {k}");
            }
            assert!(stamp.time(1, 3 * MS, None, false).at <= 3 * MS + BURST_WITHIN_NS);
        }
    }

    /// The burst is in 1.5 s after the first PCR arrived, a discontinuity
    /// in it or not: the window is not started again.
    #[test]
    fn a_discontinuity_does_not_lengthen_the_burst() {
        let mut stamp = Stamp::default();
        stamp.time(1, 0, Some(0), false);
        stamp.time(1, 700 * MS, Some(50 * 27_000_000), false);
        stamp.time(1, 1_500 * MS, Some(50 * 27_000_000 + 40 * FIELD_50), false);
        let live = stamp.time(1, 1_520 * MS, Some(0), false);
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
            let stamped = stamp.time(1, t0 + k * 40 * MS + lateness, Some(k * 2 * FIELD_50), false);
            if let Some(last) = last {
                assert_eq!(stamped.at - last, 40 * MS, "chunk {k}");
            }
            last = Some(stamped.at);
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
            let stamped = stamp.time(1, arrived, Some(k * 32 * 27_000), false);
            if let Some(last) = last {
                assert_eq!(stamped.at - last, 32 * MS, "chunk {k}");
            }
            last = Some(stamped.at);
        }
    }

    /// A broadcast whose 27 MHz runs `ppb` against the board's clock: a PCR
    /// every `every` ns of its own time from `pcr0`, arriving `late(k, t)`
    /// after its time on the board. Each PCR chunk's stamp, its arrival and
    /// the frequency applied after it.
    fn broadcast(stamp: &mut Stamp, pcr0: u64, every: u64, seconds: u64, ppb: i64, late: &dyn Fn(u64, u64) -> u64) -> Vec<(u64, u64, i64)> {
        let t0 = 1_000 * MS;
        let ticks = every * 27 / 1000;
        let mut out = Vec::new();
        for k in 0..seconds * 1_000_000_000 / every {
            let own = k * every;
            let board = (own as i128 + own as i128 * ppb as i128 / 1_000_000_000) as u64;
            let arrived = t0 + board + late(k, board);
            let stamped = stamp.time(1, arrived, Some((pcr0 + k * ticks) % PCR_WRAP), false);
            out.push((stamped.at, arrived, stamp.frequency().0));
        }
        out
    }

    /// USB lumps as measured on 51616 (2026-11-04): -12 to +10 ms around the
    /// envelope within a second, deterministic here.
    fn lumpy(k: u64, _: u64) -> u64 {
        [3, 0, 14, 7, 22, 1, 9, 18, 4, 11, 0, 16][(k % 12) as usize] * MS
    }

    /// The same PCRs at the same arrivals, the stream cut into reads two
    /// ways (chunks without a PCR between them, or not): one map.
    #[test]
    fn the_map_does_not_depend_on_how_the_stream_is_cut() {
        let (mut a, mut b) = (Stamp::default(), Stamp::default());
        for k in 0..6_000u64 {
            let arrived = 1_000 * MS + k * 40 * MS + lumpy(k, 0) + k * 40 * MS * 30 / 1_000_000;
            let pcr = Some(k * 2 * FIELD_50);
            let x = a.time(1, arrived, pcr, false).at;
            if k % 3 == 0 {
                b.time(1, arrived.saturating_sub(5 * MS), None, false);
            }
            let y = b.time(1, arrived, pcr, false).at;
            if k % 5 == 0 {
                b.time(1, arrived + MS, None, false);
            }
            assert_eq!(x, y, "chunk {k}");
        }
        assert_eq!(a.frequency(), b.frequency());
    }

    /// PCRs every 20 ms or every 80 ms: the clock is recovered at one rate.
    #[test]
    fn the_loop_gain_does_not_depend_on_the_pcr_rate() {
        let (mut fast, mut slow) = (Stamp::default(), Stamp::default());
        let a = broadcast(&mut fast, 0, 20 * MS, 240, 40_000, &|_, _| 0);
        let b = broadcast(&mut slow, 0, 80 * MS, 240, 40_000, &|_, _| 0);
        for second in [40u64, 60, 90, 120, 240] {
            let fa = a[(second * 50 - 1) as usize].2;
            let fb = b[(second * 25 / 2 - 1) as usize].2;
            assert!((fa - fb).abs() <= 1_000, "at {second} s: {fa} ppb against {fb} ppb");
        }
        // Still bringing back the phase lost while it was found (300 s).
        assert!((a.last().unwrap().2 - 40_000).abs() <= 5_000, "{:?}", a.last());
    }

    /// The reader held 500 ms by a full source (the push blocked), the
    /// stream then read in one go: the frequency is the one recovered
    /// without the hold, and the stamps do not step.
    #[test]
    fn a_held_push_does_not_move_the_recovered_clock() {
        let held = |k: u64, board: u64| -> u64 {
            let (from, to) = (200 * 1_000 * MS, 200 * 1_000 * MS + 500 * MS);
            if (from..to).contains(&board) { to - board } else { lumpy(k, board) }
        };
        let (mut a, mut b) = (Stamp::default(), Stamp::default());
        let with = broadcast(&mut a, 0, 40 * MS, 400, 29_000, &held);
        let without = broadcast(&mut b, 0, 40 * MS, 400, 29_000, &lumpy);
        for (k, (w, o)) in with.iter().zip(&without).enumerate() {
            assert!((w.2 - o.2).abs() <= 200, "chunk {k}: {} against {}", w.2, o.2);
        }
        for w in with.windows(2) {
            assert!(w[1].0 - w[0].0 <= 40 * MS + 10_000, "no step in the stamps: {:?}", w);
        }
    }

    /// The board's crystal +-60 ppm off, USB lumps, and a burst of 300 ms
    /// every 7 s: the frequency is found to within 3 ppm and the arrivals'
    /// envelope stays where it was for half an hour.
    #[test]
    fn drift_jitter_and_bursts_are_recovered_and_the_edge_holds() {
        for ppb in [60_000i64, -60_000] {
            let mut stamp = Stamp::default();
            let late = |k: u64, board: u64| lumpy(k, board) + (300 * MS).saturating_sub(board % (7_000 * MS));
            let out = broadcast(&mut stamp, 0, 40 * MS, 1_800, ppb, &late);
            let (applied, fitted) = stamp.frequency();
            assert!((applied - ppb).abs() <= 3_000, "{ppb}: applied {applied}");
            assert!(fitted.is_some_and(|f| (f - ppb).abs() <= 3_000), "{ppb}: fitted {fitted:?}");
            // The earliest arrival less its stamp, minute by minute.
            let edges: Vec<i64> = out.chunks(25 * 60).map(|m| m.iter().map(|&(at, arrived, _)| arrived as i64 - at as i64).min().unwrap()).collect();
            let settled = edges[3];
            for (minute, edge) in edges.iter().enumerate().skip(3) {
                assert!((edge - settled).abs() <= 3 * MS as i64, "{ppb}: minute {minute}: {edge} against {settled}");
            }
        }
    }

    /// The frequency changes by no more than 1 ppm a second, and the map
    /// never steps: a PCR 40 ms on is stamped 40 ms on, within 100 ppm.
    #[test]
    fn the_frequency_slews_and_the_map_is_continuous() {
        let mut stamp = Stamp::default();
        let out = broadcast(&mut stamp, 0, 40 * MS, 300, 100_000, &lumpy);
        for w in out.windows(26) {
            assert!((w[25].2 - w[0].2).abs() <= 1_000, "{} -> {}", w[0].2, w[25].2);
        }
        for w in out.windows(2) {
            let step = (w[1].0 - w[0].0) as i64 - 40 * MS as i64;
            assert!((0..=4_000).contains(&step), "{step}");
        }
    }

    /// Across the PCR's wrap, settled: no discontinuity, no step.
    #[test]
    fn a_pcr_wrap_while_settled_is_a_step_like_any() {
        let mut stamp = Stamp::default();
        let pcr0 = PCR_WRAP - 30 * 27_000_000;
        let out = broadcast(&mut stamp, pcr0, 40 * MS, 60, 0, &|_, _| 0);
        for w in out.windows(2) {
            assert_eq!(w[1].0 - w[0].0, 40 * MS);
        }
    }

    /// The broadcaster's own word for a new time base, a small jump on
    /// without it, a jump back and a big one: each a discontinuity, the
    /// stamps going on from where they were, the frequency kept.
    #[test]
    fn time_base_jumps_are_taken_where_the_stamps_are() {
        for (jump, indicated) in [(5 * 27_000_000i64, true), (300 * 27_000, false), (-300 * 27_000, false), (10 * 27_000_000, false)] {
            let mut stamp = Stamp::default();
            let out = broadcast(&mut stamp, 0, 40 * MS, 120, 20_000, &|_, _| 0);
            let (last_at, last_arrived, ppb) = *out.last().unwrap();
            let pcr = ((3_000u64 * 27_000_000 / 25 + 40 * 27_000) as i64 + jump) as u64 % PCR_WRAP;
            let jumped = stamp.time(1, last_arrived + 40 * MS, Some(pcr), indicated);
            assert!(jumped.discontinuity, "{jump} {indicated}");
            assert!(jumped.at >= last_at && jumped.at <= last_arrived + 40 * MS, "{jump}: {} after {last_at}", jumped.at);
            assert_eq!(stamp.frequency().0, ppb, "{jump}: the crystals did not change");
            let next = stamp.time(1, last_arrived + 80 * MS, Some((pcr + 40 * 27_000) % PCR_WRAP), false);
            assert!(!next.discontinuity);
            // 40 ms of the new time base, on the board's frequency.
            assert!((next.at - jumped.at).abs_diff(40 * MS + 40 * MS * ppb as u64 / 1_000_000_000) <= 1, "{jump}");
        }
    }

    /// PCRs lost for 600 ms (the packets were): the PCR and the time moved
    /// alike, no discontinuity, the stamps on the pace.
    #[test]
    fn missing_pcrs_and_their_return_are_no_discontinuity() {
        let mut stamp = Stamp::default();
        let out = broadcast(&mut stamp, 0, 40 * MS, 60, 0, &|_, _| 0);
        let (at, arrived, _) = *out.last().unwrap();
        let back = stamp.time(1, arrived + 640 * MS, Some((1_499 + 16) * 2 * FIELD_50), false);
        assert!(!back.discontinuity);
        assert_eq!(back.at - at, 640 * MS);
    }

    /// Thirty hours, PCRs 900 ms and one tick apart, across the wrap: the
    /// stamp is the PCR's whole time since the anchor, to the nanosecond --
    /// nothing rounded is summed.
    #[test]
    fn thirty_hours_add_up_to_the_nanosecond() {
        let mut stamp = Stamp::default();
        let step: u64 = 900 * 27_000 + 1;
        let pcr0 = PCR_WRAP - 27_000_000;
        let count: u64 = 30 * 3_600 * 10 / 9;
        let mut last = 0;
        for k in 0..=count {
            let ticks = u128::from(k) * u128::from(step);
            let own = (ticks * 1000 / 27) as u64;
            last = stamp.time(1, 1_000 * MS + own, Some(((u128::from(pcr0) + ticks) % u128::from(PCR_WRAP)) as u64), false).at;
        }
        let ticks = u128::from(count) * u128::from(step);
        assert_eq!(last, 1_000 * MS + (ticks * 1000 / 27) as u64);
        assert_eq!(stamp.frequency().0, 0);
    }

    /// The sound on the recovered clock, 1152-sample frames at 48 kHz, three
    /// to a PES, each PES timed by the last PCR's stamp (`pts_on_clock`):
    /// one PES's start is the one before's plus its samples, within one
    /// sample -- while the frequency is being found.
    #[test]
    fn the_sound_on_the_recovered_clock_is_sample_exact() {
        let mut stamp = Stamp::default();
        let mut reference;
        let mut last: Option<u64> = None;
        let pes = 3 * 1152 * 90_000 / 48_000; // 90 kHz ticks
        let mut next_pts = 600 * 90; // the sound 600 ms ahead of its PCR
        for k in 0..25 * 600u64 {
            let own = k * 40 * MS;
            let board = own + own * 50 / 1_000_000;
            let pcr = k * 2 * FIELD_50;
            reference = Some((stamp.time(1, 1_000 * MS + board + lumpy(k, 0), Some(pcr), false).at, pcr));
            while next_pts * 300 <= pcr + 600 * 27_000 {
                let at = crate::engine::pts_on_clock(next_pts, reference.unwrap());
                if let Some(last) = last {
                    let step = at as i64 - last as i64 - 72_000_000;
                    assert!(step.abs() <= 20_833, "PES at {next_pts}: {step} ns off its samples");
                }
                last = Some(at);
                next_pts += pes;
            }
        }
    }

    /// The arrivals step 100 ms later for good (a stall upstream that never
    /// catches up, as 51616 does): one step of the stamps, within 400 ms,
    /// by that much; the frequency is the one without it. A lump of 100 ms
    /// that catches up is no step.
    #[test]
    fn a_lasting_step_is_one_phase_step_and_a_lump_is_none() {
        let mut steps = Vec::new();
        let mut stamp = Stamp::default();
        let mut plain = Stamp::default();
        let from = 200 * 1_000 * MS;
        for k in 0..25 * 400u64 {
            let board = k * 40 * MS;
            let jitter = lumpy(k, board);
            let a = stamp.time(1, 1_000 * MS + board + jitter + if board >= from { 100 * MS } else { 0 }, Some(k * 2 * FIELD_50), false);
            plain.time(1, 1_000 * MS + board + jitter, Some(k * 2 * FIELD_50), false);
            if let Some(step) = a.stepped {
                steps.push((board, step));
            }
        }
        assert_eq!(steps.len(), 1, "{steps:?}");
        let (when, step) = steps[0];
        assert!(when >= from && when <= from + 400 * MS, "{when}");
        assert!((step - 100 * MS as i64).abs() <= 2 * MS as i64, "{step}");
        assert!((stamp.frequency().0 - plain.frequency().0).abs() <= 200, "{:?} {:?}", stamp.frequency(), plain.frequency());

        let mut lump = Stamp::default();
        for k in 0..25 * 400u64 {
            let board = k * 40 * MS;
            // 100 ms of the stream held and then let go at once.
            let held = if (from..from + 100 * MS).contains(&board) { from + 100 * MS - board } else { 0 };
            let a = lump.time(1, 1_000 * MS + board + lumpy(k, board) + held, Some(k * 2 * FIELD_50), false);
            assert_eq!(a.stepped, None, "chunk {k}");
        }
    }

    // ------------------------------------------------- the stream and its sinks

    /// One broadcast as the reader sees it: a PCR every 20 ms with a
    /// picture `LEAD_NS` ahead of its time, four chunks of payload between,
    /// the board's crystal `ppb` off, USB lumps on top. `shape` moves each
    /// chunk's arrival (and may take its PCR away): (k, chunk within the
    /// 20 ms, own time, natural arrival) -> (arrival, keep the PCR). The
    /// picture goes with the PCR's chunk.
    struct Run {
        disconts: usize,
        waits: Vec<i64>,
        rebases: usize,
        lost: usize,
        /// The largest step of the stamps between PCRs off the PCR's own.
        stamp_step_off: i64,
        ppb: i64,
        offset: u64,
    }

    /// A picture's lead over its PCR (51616: 170 ms), decoding and the
    /// vblank, and the sinks' latency.
    const LEAD_NS: u64 = 170 * MS;
    const DECODE_NS: u64 = 30 * MS;
    const SINK_LATENCY_NS: u64 = 150 * MS;

    fn run(seconds: u64, ppb: i64, shape: &dyn Fn(u64, u64, u64, u64) -> (u64, bool)) -> Run {
        let (mut stamp, mut presentation) = (Stamp::default(), Presentation::default());
        let mut offsets: Vec<(u64, u64)> = vec![(0, 0)];
        let (mut disconts, mut waits, mut rebases) = (0, Vec::new(), 0);
        let mut last_pcr_at: Option<(u64, u64)> = None;
        let mut stamp_step_off = 0i64;
        let mut pictures: Vec<(u64, u64)> = Vec::new();
        let mut events: Vec<(u64, u64, u64, bool)> = Vec::new();
        for k in 0..seconds * 50 {
            for j in 0..5u64 {
                let own = k * 20 * MS + j * 4 * MS;
                let board = (own as i128 + own as i128 * ppb as i128 / 1_000_000_000) as u64;
                let natural = 1_000 * MS + board + lumpy(k, board) / 4;
                let (arrived, keep) = shape(k, j, own, natural);
                events.push((arrived, k, j, keep));
            }
        }
        // The reader takes them in the order they came.
        events.sort_by_key(|e| (e.0, e.1, e.2));
        // Each picture's PTS on the stamps' line: where its own time falls.
        let mut line: Option<(u64, u64)> = None;
        for (arrived, k, j, keep) in events {
            let pcr = (j == 0 && keep).then_some(k * FIELD_50);
            if presentation.take_rebase() {
                stamp.reanchor();
            }
            let stamped = stamp.time(1, arrived, pcr, false);
            disconts += usize::from(stamped.discontinuity);
            if let Some(pcr) = pcr {
                if let Some((p0, a0)) = last_pcr_at {
                    let own = ticks_to_ns(pcr - p0) as i64;
                    stamp_step_off = stamp_step_off.max((stamped.at as i64 - a0 as i64 - own).abs());
                }
                last_pcr_at = Some((pcr, stamped.at));
                line = Some((k * 20 * MS, stamped.at));
            }
            if j == 0 {
                let own = k * 20 * MS;
                let pts = line.map_or(stamped.at, |(own0, at0)| at0 + own.saturating_sub(own0)) + LEAD_NS;
                match presentation.observe_picture(1, arrived, pts) {
                    Some(Late::Raised { offset, by }) => {
                        waits.push(by);
                        offsets.push((arrived, offset));
                    }
                    Some(Late::Rebase) => {
                        rebases += 1;
                        offsets.push((arrived, 0));
                    }
                    None => {}
                }
                pictures.push((arrived, pts));
            }
        }
        // Lost: past its time at the sink when decoded, with the offset then.
        let offset_at = |t: u64| offsets.iter().rev().find(|&&(at, _)| at <= t).map_or(0, |&(_, o)| o);
        let lost = pictures.iter().filter(|&&(arrived, pts)| {
            let ready = arrived + DECODE_NS;
            ready > pts + offset_at(ready) + SINK_LATENCY_NS
        }).count();
        Run { disconts, waits, rebases, lost, stamp_step_off, ppb: stamp.frequency().0, offset: presentation.offset() }
    }

    const STEADY: i64 = 30_000;

    /// PCRs missing for 50 to 500 ms while the payload comes as ever, and
    /// back on their line: the clock coasts; nothing is stepped, lost or
    /// waited for.
    #[test]
    fn pcrs_missing_while_the_payload_flows_are_coasted() {
        for gap in [50u64, 100, 250, 500] {
            let r = run(120, STEADY, &|k, _, _, natural| (natural, !(3_000..3_000 + gap / 20).contains(&k)));
            assert_eq!((r.disconts, r.waits.len(), r.rebases, r.lost), (0, 0, 0, 0), "{gap} ms");
            assert!(r.stamp_step_off <= 2 * MS as i64, "{gap} ms: the stamps on the PCR's line ({} ns off)", r.stamp_step_off);
        }
    }

    /// PCRs held up and let go together while the payload came on time:
    /// coasted, no step, nothing late, the crystal kept.
    #[test]
    fn pcrs_bunched_with_their_values_right_are_no_step() {
        let steady = run(120, STEADY, &|_, _, _, natural| (natural, true));
        for hold in [50u64, 100, 250, 500] {
            let release = 1_000 * MS + 60_000 * MS + hold * MS;
            let r = run(120, STEADY, &|k, j, own, natural| {
                let held = j == 0 && (60_000 * MS..60_000 * MS + hold * MS).contains(&own);
                // Let go together, in the order they were sent; their
                // pictures are in the chunks after them, on time.
                (if held { release + k } else { natural }, true)
            });
            assert_eq!((r.disconts, r.rebases), (0, 0), "{hold} ms");
            assert!((r.ppb - steady.ppb).abs() <= 500, "{hold} ms: {} ppb against {}", r.ppb, steady.ppb);
        }
    }

    /// The whole stream held 50 to 500 ms and let go at once: within the
    /// pictures' lead nothing happens; past it one wait, nothing lost, no
    /// step of the clock, the crystal kept.
    #[test]
    fn a_stalled_stream_that_catches_up_is_one_wait_and_no_loss() {
        let steady = run(120, STEADY, &|_, _, _, natural| (natural, true));
        for hold in [50u64, 100, 250, 500] {
            let release = 1_000 * MS + 60_000 * MS + hold * MS;
            let r = run(120, STEADY, &|_, _, own, natural| {
                let held = (60_000 * MS..60_000 * MS + hold * MS).contains(&own);
                (if held { release } else { natural }, true)
            });
            assert_eq!((r.disconts, r.rebases, r.lost), (0, 0, 0), "{hold} ms: {:?}", r.waits);
            // Two pictures running: the second, 20 ms later in its own
            // time, as late less those 20 ms.
            let expected = usize::from(hold * MS > LEAD_NS + PICTURE_ALLOWED_NS as u64 + 20 * MS);
            assert_eq!(r.waits.len(), expected, "{hold} ms: {:?}", r.waits);
            assert!((r.ppb - steady.ppb).abs() <= 500, "{hold} ms: {} ppb against {}", r.ppb, steady.ppb);
        }
    }

    /// The stream comes 100 or 300 ms later from a point on and stays so:
    /// within the lead, nothing; past it, one wait. Nothing lost, the
    /// stamps whole.
    #[test]
    fn a_lasting_latency_step_is_one_wait_and_no_loss() {
        let steady = run(180, STEADY, &|_, _, _, natural| (natural, true));
        for step in [100u64, 300] {
            let r = run(180, STEADY, &|_, _, own, natural| (natural + if own >= 90_000 * MS { step * MS } else { 0 }, true));
            assert_eq!((r.disconts, r.rebases, r.lost), (0, 0, 0), "{step} ms");
            assert_eq!(r.waits.len(), usize::from(step * MS > LEAD_NS + PICTURE_ALLOWED_NS as u64), "{step} ms: {:?}", r.waits);
            assert!(r.stamp_step_off <= 2 * MS as i64, "{step} ms: {}", r.stamp_step_off);
            assert!((r.ppb - steady.ppb).abs() <= 500, "{step} ms: {} ppb against {}", r.ppb, steady.ppb);
        }
    }

    /// 51616's hour (2026-11-05): steps of 80 to 380 ms, 2.0 s in all, with
    /// the crystal's drift, the lumps, and PCRs missing now and then: a wait
    /// for each that the lead does not cover, nothing lost, no DISCONT. Then
    /// 3.5 s more takes the wait past `LATE_MAX_NS`: one rebase, its one
    /// DISCONT.
    #[test]
    fn drift_jitter_sparse_pcrs_and_lasting_steps_together() {
        let hour: [(u64, u64); 9] =
            [(300, 111), (600, 200), (900, 89), (1_400, 284), (1_700, 152), (2_000, 373), (2_300, 127), (2_700, 381), (3_000, 326)];
        let late = |steps: &[(u64, u64)], own: u64| -> u64 { steps.iter().filter(|&&(at, _)| own >= at * 1_000 * MS).map(|&(_, ms)| ms * MS).sum() };
        for ppb in [-60_000i64, 0, 60_000] {
            let r = run(3_400, ppb, &|k, _, own, natural| (natural + late(&hour, own), k % 997 > 10));
            assert_eq!((r.lost, r.disconts, r.rebases), (0, 0, 0), "{ppb}: {:?}", r.waits);
            // A wait leaves the pictures `LATE_MARGIN_NS` ahead, the lead
            // the broadcast had taken up in it: a step after waits as well,
            // and one that stops, then comes, then comes later, twice.
            assert!(!r.waits.is_empty() && r.waits.len() <= hour.len() + 2, "{ppb}: {:?}", r.waits);
            assert!((r.ppb - ppb).abs() <= 3_000, "{ppb}: {} ppb", r.ppb);
            let mut more = hour.to_vec();
            more.push((3_200, 3_500));
            let r = run(3_400, ppb, &|k, _, own, natural| (natural + late(&more, own), k % 997 > 10));
            assert_eq!((r.rebases, r.disconts), (1, 1), "{ppb}: {:?}", r.waits);
            assert!(r.offset <= LATE_MAX_NS);
        }
    }

    /// 51616 at 1040 s (2026-11-05): +124, +121, +108 ms within 1.2 s, past
    /// what the lead covers. Nothing comes past its time.
    #[test]
    fn a_staircase_of_steps_is_waited_ahead() {
        let stairs = [(60_000u64, 224u64), (60_500, 121), (61_200, 108)];
        let r = run(120, STEADY, &|_, _, own, natural| {
            (natural + stairs.iter().filter(|&&(at, _)| own >= at * MS).map(|&(_, ms)| ms * MS).sum::<u64>(), true)
        });
        assert_eq!((r.lost, r.disconts, r.rebases), (0, 0, 0), "{:?}", r.waits);
        assert!(!r.waits.is_empty() && r.waits.len() <= 3, "{:?}", r.waits);
    }

    /// 51616 (2026-11-05): the pictures came 300 ms late a second before
    /// the PCRs did: one wait, from the pictures; the PCRs as late after it
    /// change nothing. One late picture alone is none.
    #[test]
    fn pictures_late_before_the_pcrs_are_one_wait() {
        let r = run(60, STEADY, &|_, j, own, natural| {
            let picture = j == 0;
            let late = if own >= 30_000 * MS && (!picture || own >= 29_000 * MS) { 300 * MS } else { 0 };
            (natural + late, true)
        });
        assert_eq!((r.lost, r.disconts, r.waits.len()), (0, 0, 1), "{:?}", r.waits);
        let mut p = Presentation::default();
        for k in 0..100u64 {
            let now = 1_000 * MS + k * 20 * MS;
            let late = if k == 50 { 400 * MS } else { 0 };
            assert_eq!(p.observe_picture(1, now, now + LEAD_NS - late), None, "picture {k}");
        }
    }

    /// The FAZ 1H fault (51616, 2026-11-05, two hours): the sinks waiting
    /// on their own offset were taken for a stream stopped, 79 of 80 raises
    /// with the pictures early at tsdemux. One step of the source is one
    /// raise; then, the pictures on time at the new lag, thousands of the
    /// health thread's reads and the appsink's (`offset_for`, `shows_late`)
    /// while the sink holds its picture change nothing.
    #[test]
    fn a_sink_waiting_on_its_own_offset_is_no_new_lateness() {
        let mut p = Presentation::default();
        let frame = 40 * MS;
        let step = 300 * MS;
        let mut raises = Vec::new();
        let mut reads = 0u64;
        // 25 pictures a second, leaving tsdemux 170 ms ahead of their time;
        // from picture 250 on, 300 ms later for good. Between pictures the
        // health thread reads every 10 ms, the sink showing nothing.
        for k in 0..2_000u64 {
            let pts = 1_000 * MS + k * frame;
            let now = pts - LEAD_NS + if k >= 250 { step } else { 0 };
            if let Some(late) = p.observe_picture(1, now, pts) {
                raises.push(late);
            }
            for tick in 0..4u64 {
                let before = p.clone();
                let offset = p.offset_for(1);
                let _ = p.shows_late(1, now + tick * 10 * MS, 100 * MS);
                assert_eq!(p, before, "a read moved the presentation");
                assert_eq!(offset, p.offset());
                reads += 1;
            }
        }
        assert_eq!(reads, 8_000);
        assert_eq!(raises.len(), 1, "one step, one raise: {raises:?}");
        // The step less the lead, once, and the margin once.
        let Late::Raised { offset, .. } = raises[0] else { panic!("{raises:?}") };
        assert_eq!(offset as i64, (step - LEAD_NS) as i64 + LATE_MARGIN_NS, "{raises:?}");
    }

    /// The pictures that made a raise are spent: a further step needs two
    /// pictures of its own past the new offset, not one paired with them.
    #[test]
    fn a_picture_of_a_step_waited_for_does_not_count_again() {
        let mut p = Presentation::default();
        for k in 0..10u64 {
            let pts = 1_000 * MS + k * 40 * MS;
            assert_eq!(p.observe_picture(1, pts - LEAD_NS, pts), None);
        }
        // Two pictures 200 ms late: a raise to 220 ms.
        let pts = 1_400 * MS;
        assert_eq!(p.observe_picture(1, pts + 200 * MS, pts), None);
        assert_eq!(p.observe_picture(1, pts + 40 * MS + 200 * MS, pts + 40 * MS), Some(Late::Raised { offset: 220 * MS, by: 220 * MS as i64 }));
        // The source 100 ms later again: its first picture alone is nothing.
        assert_eq!(p.observe_picture(1, pts + 80 * MS + 300 * MS, pts + 80 * MS), None);
        assert_eq!(p.observe_picture(1, pts + 120 * MS + 300 * MS, pts + 120 * MS), Some(Late::Raised { offset: 320 * MS, by: 100 * MS as i64 }));
        // Back on time at that lag: nothing more, however long.
        for k in 4..1_000u64 {
            let pts = 1_400 * MS + k * 40 * MS;
            assert_eq!(p.observe_picture(1, pts + 300 * MS, pts), None, "picture {k}");
        }
        assert_eq!(p.offset(), 320 * MS);
    }

    /// Two steps of the source apart are two raises, each its own size.
    #[test]
    fn two_steps_of_the_source_are_two_raises() {
        let mut p = Presentation::default();
        let mut raises = Vec::new();
        for k in 0..3_000u64 {
            let pts = 1_000 * MS + k * 40 * MS;
            let lag = match k {
                ..500 => 0,
                500..1_500 => 250 * MS,
                _ => 250 * MS + 150 * MS,
            };
            if let Some(late) = p.observe_picture(1, pts - LEAD_NS + lag, pts) {
                raises.push(late);
            }
        }
        assert_eq!(
            raises,
            vec![
                Late::Raised { offset: 100 * MS, by: 100 * MS as i64 },
                Late::Raised { offset: 250 * MS, by: 150 * MS as i64 },
            ]
        );
    }

    /// A picture a little late within a second of a wait is shown; later,
    /// or with no wait near, it is not.
    #[test]
    fn within_a_wait_a_picture_a_little_late_is_shown() {
        let mut p = Presentation::default();
        for k in 0..10u64 {
            let pts = 1_000 * MS + k * 20 * MS;
            p.observe_picture(1, pts - 200 * MS, pts);
        }
        assert!(!p.shows_late(1, 2_000 * MS, 50 * MS), "no wait yet");
        // Two pictures 300 ms late: a wait at 1.5 s.
        p.observe_picture(1, 1_500 * MS, 1_200 * MS);
        assert!(matches!(p.observe_picture(1, 1_520 * MS, 1_220 * MS), Some(Late::Raised { .. })));
        assert!(p.shows_late(1, 1_900 * MS, 100 * MS));
        assert!(!p.shows_late(1, 1_900 * MS, 200 * MS), "too late");
        assert!(!p.shows_late(1, 2_600 * MS, 50 * MS), "the wait long past");
        assert!(!p.shows_late(2, 1_900 * MS, 50 * MS), "another channel");
    }

    /// The same stream cut into reads two ways is waited for alike.
    #[test]
    fn the_presentation_does_not_depend_on_how_the_stream_is_cut() {
        let shape = |_, _, own: u64, natural: u64| (natural + if own >= 50_000 * MS { 300 * MS } else { 0 }, true);
        let a = run(90, STEADY, &shape);
        let b = run(90, STEADY, &|k, j, own, natural| {
            let (arrived, keep) = shape(k, j, own, natural);
            // The payload chunks a little later, as if read with the next.
            (arrived + if j > 0 { 2 * MS } else { 0 }, keep)
        });
        assert_eq!(a.waits.len(), b.waits.len());
        assert!((a.waits[0] - b.waits[0]).abs() <= 2 * MS as i64, "{:?} {:?}", a.waits, b.waits);
    }
}
