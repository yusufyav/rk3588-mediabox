//! A live channel's opening: when its picture, and with it its sound, start.
//!
//! The control plane numbers every channel it opens (its epoch) and hands
//! the number over with the channel (`MEDIABOX_LIVE_EPOCH`, `loadfile ...
//! mediabox-epoch=N`). Such a channel is decoded at once but nothing of it
//! goes to the panel until the control plane says the display shows the
//! stream's rate (`mediabox-gate open N`): the stream's SPS is read by the
//! receiver, the mode matched to it and committed, and the sound device
//! opened again if the commit stopped it. A channel whose display already
//! fits is let go before its first picture is decoded, and shows it as it
//! comes. Before this a channel remembered at 50 Hz that came at 30 Hz was
//! shown at once and its mode changed a second later, under its pictures:
//! 3-5 of them sent and never on the panel, every time (FAZ 2E, 2026-11-07,
//! `unseen why=same-vblank` while the interface committed the mode).
//!
//! Pictures that come while it waits are its pre-roll: never sent, never
//! counted as dropped, and not kept either -- they pass at their time and
//! are given back, so nothing piles up and nothing is shown late. The first
//! picture after the gate opens is the channel's first (V0): it is sent at
//! its own due time, as every picture after it, and the sound starts at its
//! PTS (`Programme::sound`, which cuts the frame that takes it in). Opened
//! before any picture came, the channel starts at its first key frame, as it
//! always did.
//!
//! The control plane answers within its own limits; this side does not wait
//! for ever either: a gate held [`HOLD_MAX`] after its first picture opens by
//! itself, and says so (`outcome=player-timeout`).

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The longest a channel's pictures are held back. The control plane's own
/// limits come first: 2 s for the mode's commit, 3 s for the sound device.
pub const HOLD_MAX: Duration = Duration::from_secs(6);

/// What becomes of a decoded picture at the sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// Sent to the panel; `first`: the channel's first since the gate opened
    /// on pre-roll, its PTS the sound's start.
    Show { first: bool },
    /// The gate is shut: not sent.
    Preroll,
}

#[derive(Default)]
struct State {
    open: bool,
    outcome: Option<String>,
    /// Pictures that came while it was shut.
    preroll: u64,
    first_held: Option<Instant>,
    /// The parser's first key frame.
    key_pts: Option<u64>,
    /// Opened on pre-roll: the next picture is the first.
    first_pending: bool,
    /// Where the picture starts, for the sound; `None` until known.
    picture: Option<u64>,
    /// What the held pictures said of their rate: their caps' and the step
    /// between the first two PTS (interlaced streams come out of MPP at the
    /// field rate whatever the caps say).
    caps_fps: Option<f64>,
    interlace: Option<String>,
    first_pts: Option<u64>,
    step_ns: Option<u64>,
    opened_after_ms: Option<u64>,
}

pub struct Gate {
    pub generation: u64,
    /// The control plane's number for the channel; `None`: not gated.
    pub epoch: Option<u64>,
    armed_at: Instant,
    state: Mutex<State>,
    changed: Condvar,
}

impl Gate {
    /// A channel's gate: shut when the control plane numbered it, open
    /// otherwise -- and open when the same channel opens again after it was
    /// let go once (`open_already`: a branch put up again after an error).
    pub fn new(generation: u64, epoch: Option<u64>, open_already: bool) -> Self {
        let open = epoch.is_none() || open_already;
        Self {
            generation,
            epoch,
            armed_at: Instant::now(),
            state: Mutex::new(State {
                open,
                outcome: open.then(|| if epoch.is_none() { "ungated" } else { "reopened" }.to_string()),
                ..State::default()
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The parser's first key frame: where the picture starts, if the gate
    /// is open and no picture came while it was shut.
    pub fn key_frame(&self, pts: u64) {
        let mut state = self.lock();
        state.key_pts.get_or_insert(pts);
        if state.open && !state.first_pending && state.picture.is_none() {
            state.picture = Some(pts);
            self.changed.notify_all();
        }
    }

    /// A decoded picture reaching the sink.
    pub fn frame(&self, pts: u64, caps_fps: Option<f64>, interlace: &str) -> Frame {
        let mut state = self.lock();
        if !state.open {
            state.preroll += 1;
            state.first_held.get_or_insert_with(Instant::now);
            if state.caps_fps.is_none() {
                state.caps_fps = caps_fps;
                state.interlace = Some(interlace.to_string());
            }
            match state.first_pts {
                None => state.first_pts = Some(pts),
                Some(first) if state.step_ns.is_none() && pts > first => state.step_ns = Some(pts - first),
                _ => {}
            }
            return Frame::Preroll;
        }
        if state.first_pending {
            state.first_pending = false;
            state.picture = Some(pts);
            self.changed.notify_all();
            return Frame::Show { first: true };
        }
        Frame::Show { first: false }
    }

    /// The control plane lets channel `epoch` go: `Ok(true)` opened now,
    /// `Ok(false)` it was open; an error for another channel's number.
    pub fn open(&self, epoch: u64, outcome: &str) -> Result<bool, String> {
        if self.epoch != Some(epoch) {
            return Err(format!("stale: epoch {epoch}, channel {:?}", self.epoch));
        }
        Ok(self.open_now(outcome))
    }

    fn open_now(&self, outcome: &str) -> bool {
        let mut state = self.lock();
        if state.open {
            return false;
        }
        state.open = true;
        state.outcome = Some(outcome.to_string());
        state.opened_after_ms = Some(self.armed_at.elapsed().as_millis() as u64);
        if state.preroll > 0 {
            state.first_pending = true;
        } else if let Some(key) = state.key_pts {
            state.picture.get_or_insert(key);
        }
        self.changed.notify_all();
        true
    }

    /// Held past [`HOLD_MAX`] since its first picture: opened here.
    pub fn expire(&self, now: Instant) -> bool {
        let due = {
            let state = self.lock();
            !state.open && state.first_held.is_some_and(|at| now.saturating_duration_since(at) >= HOLD_MAX)
        };
        due && self.open_now("player-timeout")
    }

    pub fn is_open(&self) -> bool {
        self.lock().open
    }

    /// Where the picture starts, and whether pictures are being held for a
    /// gate that is shut: the sound waits for that without the limit it
    /// keeps for a picture that does not come (`Programme::sound`).
    pub fn picture(&self) -> (Option<u64>, bool) {
        let state = self.lock();
        (state.picture, !state.open && state.preroll > 0)
    }

    /// Wake whoever waits on the picture (`picture_known`).
    pub fn notify(&self) {
        let _state = self.lock();
        self.changed.notify_all();
    }

    /// Wait at most `timeout` for the picture's start to change.
    pub fn wait(&self, timeout: Duration) {
        let state = self.lock();
        let _ = self.changed.wait_timeout(state, timeout);
    }

    /// What the control socket reads (`mediabox-gate`).
    pub fn snapshot(&self) -> Value {
        let state = self.lock();
        json!({
            "generation": self.generation,
            "epoch": self.epoch,
            "open": state.open,
            "outcome": state.outcome,
            "preroll": state.preroll,
            "caps_fps": state.caps_fps,
            "interlace": state.interlace,
            "rate": state.step_ns.filter(|step| *step > 0).map(|step| 1e9 / step as f64),
            "picture": state.picture,
            "opened_after_ms": state.opened_after_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ungated_channel_starts_at_its_key_frame() {
        let gate = Gate::new(1, None, false);
        gate.key_frame(1000);
        assert_eq!(gate.picture(), (Some(1000), false));
        assert_eq!(gate.frame(1000, Some(25.0), "Progressive"), Frame::Show { first: false });
    }

    #[test]
    fn a_gate_opened_before_its_first_picture_costs_nothing() {
        let gate = Gate::new(1, Some(7), false);
        gate.key_frame(1000);
        assert_eq!(gate.picture(), (None, false), "the sound waits while it is shut");
        assert_eq!(gate.open(7, "fast"), Ok(true));
        assert_eq!(gate.picture(), (Some(1000), false));
        assert_eq!(gate.frame(1000, Some(50.0), "Progressive"), Frame::Show { first: false });
    }

    #[test]
    fn pictures_while_shut_are_preroll_and_the_next_one_is_the_first() {
        let gate = Gate::new(1, Some(7), false);
        gate.key_frame(1000);
        assert_eq!(gate.frame(1000, Some(30.0), "Progressive"), Frame::Preroll);
        assert_eq!(gate.frame(33_334_333, Some(30.0), "Progressive"), Frame::Preroll);
        assert_eq!(gate.picture(), (None, true), "held: the sound waits for it without a limit");
        let snapshot = gate.snapshot();
        assert_eq!(snapshot["preroll"], 2);
        assert!((snapshot["rate"].as_f64().unwrap() - 30.0).abs() < 0.01);
        assert_eq!(gate.open(7, "matched"), Ok(true));
        assert_eq!(gate.picture(), (None, false), "not the key frame: it was never shown");
        assert_eq!(gate.frame(300_000_000, Some(30.0), "Progressive"), Frame::Show { first: true });
        assert_eq!(gate.picture(), (Some(300_000_000), false));
        assert_eq!(gate.frame(333_333_333, Some(30.0), "Progressive"), Frame::Show { first: false });
    }

    #[test]
    fn another_channels_number_does_not_open_it() {
        let gate = Gate::new(2, Some(8), false);
        assert!(gate.open(7, "matched").is_err());
        assert!(!gate.is_open());
        assert_eq!(gate.open(8, "matched"), Ok(true));
        assert_eq!(gate.open(8, "matched"), Ok(false));
    }

    #[test]
    fn a_gate_nobody_opens_opens_itself_after_its_limit_and_says_so() {
        let gate = Gate::new(1, Some(7), false);
        let start = Instant::now();
        assert!(!gate.expire(start + HOLD_MAX), "nothing held: nothing to time");
        assert_eq!(gate.frame(1000, None, "Progressive"), Frame::Preroll);
        assert!(!gate.expire(Instant::now() + HOLD_MAX - Duration::from_millis(50)));
        assert!(gate.expire(Instant::now() + HOLD_MAX));
        assert_eq!(gate.snapshot()["outcome"], "player-timeout");
        assert_eq!(gate.frame(40_000_000, None, "Progressive"), Frame::Show { first: true });
    }

    #[test]
    fn the_same_channel_put_up_again_after_it_was_let_go_is_not_held() {
        let gate = Gate::new(3, Some(7), true);
        assert!(gate.is_open());
        assert_eq!(gate.frame(1000, None, "Progressive"), Frame::Show { first: false });
    }
}
