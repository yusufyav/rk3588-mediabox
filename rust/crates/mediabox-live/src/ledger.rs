//! Where every decoded picture of a channel ended, counted where it ended.
//!
//! A channel's pictures leave the decoder (`decoded`) and each has one end
//! here: thrown away before the queue behind the decoder (a stale channel's,
//! a bogus first PTS, a catch-up), or into the queue, out of it, into the
//! sink and out of it to one of the sink's verdicts -- a stale channel's, out
//! of order, pre-roll at a shut gate, not a dma-buf, late, full, the plane
//! gone, or sent to the plane (`submit_attempted`). What the interface did
//! with each one sent is its own record (mediabox-tv's `commit`, `flip` and
//! `unseen`), joined to these by the frame's id.
//!
//! Each count is taken at a pad of its own; what went into the queue or the
//! sink and never came out was there when the channel was taken down
//! (`terminal_drain_after_stop`), and can be no more than what the queue and
//! the sink hold. A channel's ledger is closed once its elements are down and
//! the sink flushed; it closes only if every picture has its end and those
//! bounds hold (`Ledger::verdict`), and says so either way.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{Value, json};

/// The queue behind the decoder holds 3 (`stream`), and one more on its way out.
pub const QUEUE_HOLDS: u64 = 4;
/// The sink: one waiting for its time, one in its own queue (max-buffers 1).
pub const SINK_HOLDS: u64 = 2;

/// One end of a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Decoded,
    /// Left the decoder after its channel was left.
    StaleAtDecoder,
    BogusPts,
    CatchUp,
    QueueIn,
    QueueOut,
    SinkIn,
    /// Reached the sink after its channel was left.
    StaleAtSink,
    Order,
    Preroll,
    NotDmabuf,
    Late,
    FaultSkipped,
    PlaneFull,
    /// The plane was closed: the player is ending.
    PlaneClosed,
    /// The interface went away.
    PlaneGone,
    Submitted,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ledger {
    pub decoded: u64,
    pub stale_at_decoder: u64,
    pub bogus_pts: u64,
    pub catch_up: u64,
    pub queue_in: u64,
    pub queue_out: u64,
    pub sink_in: u64,
    pub stale_at_sink: u64,
    pub order: u64,
    pub preroll: u64,
    pub not_dmabuf: u64,
    pub late: u64,
    pub fault_skipped: u64,
    pub plane_full: u64,
    pub plane_closed: u64,
    pub plane_gone: u64,
    pub submitted: u64,
}

impl Ledger {
    fn count(&mut self, end: End) {
        let field = match end {
            End::Decoded => &mut self.decoded,
            End::StaleAtDecoder => &mut self.stale_at_decoder,
            End::BogusPts => &mut self.bogus_pts,
            End::CatchUp => &mut self.catch_up,
            End::QueueIn => &mut self.queue_in,
            End::QueueOut => &mut self.queue_out,
            End::SinkIn => &mut self.sink_in,
            End::StaleAtSink => &mut self.stale_at_sink,
            End::Order => &mut self.order,
            End::Preroll => &mut self.preroll,
            End::NotDmabuf => &mut self.not_dmabuf,
            End::Late => &mut self.late,
            End::FaultSkipped => &mut self.fault_skipped,
            End::PlaneFull => &mut self.plane_full,
            End::PlaneClosed => &mut self.plane_closed,
            End::PlaneGone => &mut self.plane_gone,
            End::Submitted => &mut self.submitted,
        };
        *field += 1;
    }

    /// The sink's verdicts, one per picture it gave out.
    fn sink_out(&self) -> u64 {
        self.stale_at_sink + self.order + self.preroll + self.not_dmabuf + self.late + self.fault_skipped + self.plane_full + self.plane_closed + self.plane_gone + self.submitted
    }

    /// Pictures in the queue, between it and the sink, and in the sink when
    /// the channel was taken down; negative if more came out than went in.
    fn held(&self) -> (i128, i128, i128) {
        let (qi, qo, si, so) = (self.queue_in as i128, self.queue_out as i128, self.sink_in as i128, self.sink_out() as i128);
        (qi - qo, qo - si, si - so)
    }

    /// The pictures left in the player when it was taken down: what the
    /// queue, the way to the sink and the sink held, and what the plane, closed
    /// for the player's end, was not sent.
    pub fn terminal_drain_after_stop(&self) -> u64 {
        let (queue, between, sink) = self.held();
        (queue.max(0) + between.max(0) + sink.max(0)) as u64 + self.plane_closed
    }

    /// The ends thrown away before the plane, apart from the terminal ones.
    pub fn cancelled_before_submit(&self) -> u64 {
        self.stale_at_decoder + self.stale_at_sink
    }

    /// Closed: every picture decoded has one end, each count matches the
    /// next one's, and what was left behind is within what can be held. With
    /// the channel still on (`stopped` false), nothing is left behind but
    /// what is on its way.
    pub fn verdict(&self, stopped: bool) -> Result<(), String> {
        let before_queue = self.stale_at_decoder + self.bogus_pts + self.catch_up;
        if self.decoded != before_queue + self.queue_in {
            return Err(format!("decoded {} != thrown away before the queue {before_queue} + into the queue {}", self.decoded, self.queue_in));
        }
        let (queue, between, sink) = self.held();
        if queue < 0 || between < 0 || sink < 0 {
            return Err(format!("more out than in: queue {queue}, between {between}, sink {sink}"));
        }
        if queue as u64 > QUEUE_HOLDS || between > 1 || sink as u64 > SINK_HOLDS {
            return Err(format!("more left behind than can be held: queue {queue}/{QUEUE_HOLDS}, between {between}/1, sink {sink}/{SINK_HOLDS}"));
        }
        if !stopped && self.plane_closed > 0 {
            return Err(format!("{} sent to a closed plane while the channel was on", self.plane_closed));
        }
        let ends = self.preroll
            + self.submitted
            + self.not_dmabuf
            + self.late
            + self.cancelled_before_submit()
            + self.terminal_drain_after_stop()
            + self.order
            + self.bogus_pts
            + self.catch_up
            + self.fault_skipped
            + self.plane_full
            + self.plane_gone;
        if ends != self.decoded {
            return Err(format!("decoded {} != ends {ends}", self.decoded));
        }
        Ok(())
    }

    pub fn json(&self, generation: u64, stopped: bool) -> Value {
        let verdict = self.verdict(stopped);
        json!({
            "generation": generation,
            "stopped": stopped,
            "decoded": self.decoded,
            "preroll_discarded": self.preroll,
            "submit_attempted": self.submitted,
            "rejected_not_dmabuf": self.not_dmabuf,
            "rejected_late": self.late,
            "cancelled_before_submit": self.cancelled_before_submit(),
            "terminal_drain_after_stop": self.terminal_drain_after_stop(),
            "rejected_order": self.order,
            "rejected_bogus_pts": self.bogus_pts,
            "rejected_catch_up": self.catch_up,
            "fault_skipped": self.fault_skipped,
            "rejected_plane_full": self.plane_full,
            "rejected_plane_gone": self.plane_gone,
            "detail": {
                "stale_at_decoder": self.stale_at_decoder,
                "stale_at_sink": self.stale_at_sink,
                "queue_in": self.queue_in,
                "queue_out": self.queue_out,
                "sink_in": self.sink_in,
                "plane_closed": self.plane_closed,
            },
            "closed": verdict.is_ok(),
            "error": verdict.err(),
        })
    }

    /// The timeline's line: the same, flat.
    pub fn line(&self, generation: u64, stopped: bool) -> String {
        let verdict = self.verdict(stopped);
        format!(
            "gen={generation} stopped={} closed={} decoded={} preroll_discarded={} submit_attempted={} rejected_not_dmabuf={} rejected_late={} cancelled_before_submit={} terminal_drain_after_stop={} rejected_order={} rejected_bogus_pts={} rejected_catch_up={} fault_skipped={} rejected_plane_full={} rejected_plane_gone={} queue_in={} queue_out={} sink_in={} plane_closed={} error={}",
            u8::from(stopped),
            u8::from(verdict.is_ok()),
            self.decoded,
            self.preroll,
            self.submitted,
            self.not_dmabuf,
            self.late,
            self.cancelled_before_submit(),
            self.terminal_drain_after_stop(),
            self.order,
            self.bogus_pts,
            self.catch_up,
            self.fault_skipped,
            self.plane_full,
            self.plane_gone,
            self.queue_in,
            self.queue_out,
            self.sink_in,
            self.plane_closed,
            verdict.err().unwrap_or_default().replace(' ', "_")
        )
    }
}

/// Every channel's ledger: open while it plays, closed once it is down.
#[derive(Default)]
pub struct Ledgers {
    open: Mutex<BTreeMap<u64, Ledger>>,
    closed: Mutex<Vec<(u64, Ledger)>>,
}

/// The closed ones kept for the health file.
const KEPT: usize = 8;

impl Ledgers {
    pub fn count(&self, generation: u64, end: End) {
        self.open.lock().unwrap_or_else(|p| p.into_inner()).entry(generation).or_default().count(end);
    }

    /// Channel `generation` is down: its ledger closed, on the timeline and
    /// in the journal if it does not close.
    pub fn close(&self, generation: u64) {
        let Some(ledger) = self.open.lock().unwrap_or_else(|p| p.into_inner()).remove(&generation) else { return };
        mediabox_core::timeline!("ledger", "{}", ledger.line(generation, true));
        if let Err(why) = ledger.verdict(true) {
            eprintln!("mediabox-live: kanal {generation} kare defteri kapanmadı: {why}");
        }
        let mut closed = self.closed.lock().unwrap_or_else(|p| p.into_inner());
        closed.push((generation, ledger));
        let excess = closed.len().saturating_sub(KEPT);
        closed.drain(..excess);
    }

    /// Every one still open: the player's end.
    pub fn close_all(&self) {
        let open: Vec<u64> = self.open.lock().unwrap_or_else(|p| p.into_inner()).keys().copied().collect();
        for generation in open {
            self.close(generation);
        }
    }

    pub fn json(&self) -> Value {
        let closed = self.closed.lock().unwrap_or_else(|p| p.into_inner());
        let open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        json!({
            "closed": closed.iter().map(|(generation, ledger)| ledger.json(*generation, true)).collect::<Vec<_>>(),
            "open": open.iter().map(|(generation, ledger)| ledger.json(*generation, false)).collect::<Vec<_>>(),
        })
    }

    #[cfg(test)]
    pub fn closed(&self) -> Vec<(u64, Ledger)> {
        self.closed.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ends: &[(End, u64)]) -> Ledger {
        let mut ledger = Ledger::default();
        for (end, n) in ends {
            for _ in 0..*n {
                ledger.count(*end);
            }
        }
        ledger
    }

    /// 100 decoded: 2 stale at the decoder, 1 bogus, 97 into the queue; 3
    /// still in it at the stop, 94 out, all into the sink, 1 waiting there:
    /// 93 given out -- 10 pre-roll, 80 sent, 2 late, 1 stale.
    fn stopped_channel() -> Ledger {
        run(&[
            (End::Decoded, 100),
            (End::StaleAtDecoder, 2),
            (End::BogusPts, 1),
            (End::QueueIn, 97),
            (End::QueueOut, 94),
            (End::SinkIn, 94),
            (End::Preroll, 10),
            (End::Submitted, 80),
            (End::Late, 2),
            (End::StaleAtSink, 1),
        ])
    }

    #[test]
    fn every_picture_has_one_end_and_what_was_left_is_the_terminal_drain() {
        let ledger = stopped_channel();
        assert_eq!(ledger.verdict(true), Ok(()));
        assert_eq!(ledger.terminal_drain_after_stop(), 4);
        assert_eq!(ledger.cancelled_before_submit(), 3);
        let json = ledger.json(7, true);
        assert_eq!(json["closed"], true);
        let sum: u64 = ["preroll_discarded", "submit_attempted", "rejected_not_dmabuf", "rejected_late", "cancelled_before_submit", "terminal_drain_after_stop", "rejected_order", "rejected_bogus_pts", "rejected_catch_up", "fault_skipped", "rejected_plane_full", "rejected_plane_gone"]
            .iter()
            .map(|k| json[k].as_u64().unwrap())
            .sum();
        assert_eq!(sum, json["decoded"].as_u64().unwrap());
    }

    #[test]
    fn a_picture_lost_without_an_end_does_not_close() {
        // Lost inside the sink beyond what it can hold.
        let mut ledger = stopped_channel();
        ledger.sink_in += 3;
        ledger.queue_out += 3;
        ledger.queue_in += 3;
        ledger.decoded += 3;
        assert!(ledger.verdict(true).unwrap_err().contains("more left behind"), "{:?}", ledger.verdict(true));
        // Decoded and never into the queue, nor thrown away.
        let mut ledger = stopped_channel();
        ledger.decoded += 1;
        assert!(ledger.verdict(true).unwrap_err().starts_with("decoded 101"));
        // More out than in: a count taken twice.
        let mut ledger = stopped_channel();
        ledger.submitted += 2;
        assert!(ledger.verdict(true).unwrap_err().contains("more out than in"));
    }

    #[test]
    fn a_closed_plane_while_the_channel_is_on_does_not_close() {
        let mut ledger = stopped_channel();
        ledger.plane_closed += 1;
        ledger.submitted -= 1;
        assert_eq!(ledger.verdict(true), Ok(()));
        assert!(ledger.verdict(false).is_err());
    }

    #[test]
    fn ledgers_close_once_and_keep_the_last() {
        let ledgers = Ledgers::default();
        for generation in 1..=10 {
            ledgers.count(generation, End::Decoded);
            ledgers.count(generation, End::QueueIn);
        }
        for generation in 1..=9 {
            ledgers.close(generation);
            ledgers.close(generation);
        }
        assert_eq!(ledgers.closed().len(), KEPT);
        assert_eq!(ledgers.json()["open"].as_array().unwrap().len(), 1);
        ledgers.close_all();
        let closed = ledgers.closed();
        assert_eq!(closed.last().unwrap().0, 10);
        assert!(closed.iter().all(|(_, ledger)| ledger.verdict(true).is_ok()));
    }
}
