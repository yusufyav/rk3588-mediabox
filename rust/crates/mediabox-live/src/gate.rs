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
//! A gate is opened by the control plane's word and by nothing else. A
//! channel it cannot let go is failed (`mediabox-gate fail N code`), and a
//! control plane that does not answer within [`HOLD_MAX`] of the first picture
//! or [`ARM_MAX`] of the arming fails it here (`player-timeout`): the gate
//! stays shut for good, nothing of the channel is shown or heard, a late
//! answer finds it failed, and the player ends saying why. A wrong mode with a
//! picture on it is not a better answer than an error. A channel with no
//! number (a diagnostic run) has no gate to wait for and never times out.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The longest a channel's pictures are held back. The control plane's own
/// limits come first: 2 s for the mode's commit, 3 s for the sound device.
pub const HOLD_MAX: Duration = Duration::from_secs(6);
/// The longest a numbered channel waits for its answer with no picture at
/// all. The control plane's come first: 10 s for the stream's rate, then the
/// mode's and the sound's, then 5 s for a player whose socket is not up yet.
pub const ARM_MAX: Duration = Duration::from_secs(20);

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
    /// Failed: shut for good, why.
    failed: Option<String>,
    /// The channel was left while it was shut: nobody will open it.
    retired: bool,
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
    /// The last picture held: nothing of the sound before it can be heard.
    last_held: Option<u64>,
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
            state.last_held = Some(pts);
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
        if let Some(why) = self.lock().failed.clone() {
            return Err(format!("failed: {why}"));
        }
        Ok(self.open_now(outcome))
    }

    /// The control plane gives channel `epoch` up: shut for good. `Ok(true)`
    /// failed now, `Ok(false)` it had failed already; an error for another
    /// channel's number, or one already let go.
    pub fn fail(&self, epoch: u64, code: &str) -> Result<bool, String> {
        if self.epoch != Some(epoch) {
            return Err(format!("stale: epoch {epoch}, channel {:?}", self.epoch));
        }
        let mut state = self.lock();
        if state.open {
            return Err(format!("open: epoch {epoch}"));
        }
        if state.failed.is_some() {
            return Ok(false);
        }
        state.failed = Some(code.to_string());
        state.outcome = Some(code.to_string());
        self.changed.notify_all();
        Ok(true)
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

    /// No answer within its limits: failed here (`player-timeout`), once.
    pub fn expire(&self, now: Instant) -> bool {
        let mut state = self.lock();
        let due = !state.open
            && state.failed.is_none()
            && (state.first_held.is_some_and(|at| now.saturating_duration_since(at) >= HOLD_MAX)
                || now.saturating_duration_since(self.armed_at) >= ARM_MAX);
        if due {
            state.failed = Some("player-timeout".to_string());
            state.outcome = Some("player-timeout".to_string());
            self.changed.notify_all();
        }
        due
    }

    pub fn is_open(&self) -> bool {
        self.lock().open
    }

    /// Where the picture starts, and whether the gate is shut: the sound
    /// waits for that with no limit of its own (`Programme::sound`) -- it
    /// goes with the picture or not at all.
    pub fn picture(&self) -> (Option<u64>, bool) {
        let state = self.lock();
        (state.picture, !state.open)
    }

    /// Sound ending at `end` is before every picture that can still be the
    /// first: it need not wait to be told so. Waiting, it filled the sound's
    /// queue in 3 s, the reader behind it stopped, and with it the receiver
    /// and the pictures: a shut gate held the stream it was waiting to read.
    /// The channel is being left: whoever waits on this gate stops waiting.
    /// A sound frame waiting here holds its streaming thread, and setting the
    /// branch to NULL waits for that thread: without this a channel left
    /// while it was shut was never taken down.
    pub fn retire(&self) {
        self.lock().retired = true;
        self.changed.notify_all();
    }

    /// Shut for good: failed or retired. Nothing waits for it any more.
    pub fn ended(&self) -> bool {
        let state = self.lock();
        !state.open && (state.failed.is_some() || state.retired)
    }

    pub fn behind_held(&self, end: u64) -> bool {
        let state = self.lock();
        !state.open && state.last_held.is_some_and(|held| end <= held)
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
            "failed": state.failed,
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
        assert_eq!(gate.picture(), (None, true), "the sound waits while it is shut");
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
    fn sound_before_the_pictures_held_need_not_wait_and_no_other_is_given_up() {
        let gate = Gate::new(1, Some(7), false);
        assert!(!gate.behind_held(1000), "no picture held: it may yet be the start");
        assert_eq!(gate.frame(40_000_000, Some(25.0), "Progressive"), Frame::Preroll);
        assert!(gate.behind_held(40_000_000));
        assert!(!gate.behind_held(40_000_001), "reaches past the last one held: may take in the first");
        assert_eq!(gate.open(7, "matched"), Ok(true));
        assert!(!gate.behind_held(0), "open: the first picture says");
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
    fn a_gate_nobody_answers_fails_shut_and_stays_shut() {
        let gate = Gate::new(1, Some(7), false);
        let start = Instant::now();
        assert!(!gate.expire(start + HOLD_MAX), "nothing held and within its own limit: nothing to time");
        assert_eq!(gate.frame(1000, None, "Progressive"), Frame::Preroll);
        assert!(!gate.expire(Instant::now() + HOLD_MAX - Duration::from_millis(50)));
        assert!(gate.expire(Instant::now() + HOLD_MAX));
        assert!(!gate.expire(Instant::now() + HOLD_MAX), "once");
        let snapshot = gate.snapshot();
        assert_eq!(snapshot["open"], false);
        assert_eq!(snapshot["failed"], "player-timeout");
        assert_eq!(gate.frame(40_000_000, None, "Progressive"), Frame::Preroll, "nothing shown");
        assert_eq!(gate.picture(), (None, true), "nothing heard");
        assert_eq!(gate.open(7, "matched"), Err("failed: player-timeout".into()), "a late answer opens nothing");
        assert!(!gate.is_open());
    }

    #[test]
    fn a_gate_with_no_picture_and_no_answer_fails_too() {
        let gate = Gate::new(1, Some(7), false);
        assert!(!gate.expire(Instant::now() + ARM_MAX - Duration::from_millis(50)));
        assert!(gate.expire(Instant::now() + ARM_MAX));
        assert_eq!(gate.snapshot()["failed"], "player-timeout");
    }

    #[test]
    fn a_channel_with_no_number_has_no_gate_to_time_out() {
        let gate = Gate::new(1, None, false);
        assert!(!gate.expire(Instant::now() + ARM_MAX + HOLD_MAX));
        assert!(gate.is_open());
        assert_eq!(gate.snapshot()["outcome"], "ungated");
    }

    #[test]
    fn a_failed_gate_is_failed_for_good_and_only_by_its_own_number() {
        let gate = Gate::new(2, Some(8), false);
        assert!(gate.fail(7, "mode-failed").is_err(), "another channel's");
        assert!(gate.snapshot()["failed"].is_null());
        assert_eq!(gate.fail(8, "mode-failed"), Ok(true));
        assert_eq!(gate.fail(8, "mode-timeout"), Ok(false), "the first word stands");
        assert_eq!(gate.snapshot()["failed"], "mode-failed");
        assert_eq!(gate.open(8, "matched"), Err("failed: mode-failed".into()));
        assert_eq!(gate.frame(1000, None, "Progressive"), Frame::Preroll);
        // The next channel is another gate: nothing of this one is in it.
        let next = Gate::new(3, Some(9), false);
        assert_eq!(next.open(9, "matched"), Ok(true));
        // One let go is not failed after it.
        assert!(next.fail(9, "mode-failed").is_err());
        assert!(next.is_open());
        assert!(!next.ended());
        assert!(gate.ended());
        let left = Gate::new(4, Some(10), false);
        assert!(!left.ended());
        left.retire();
        assert!(left.ended(), "left while shut");
    }

    #[test]
    fn the_same_channel_put_up_again_after_it_was_let_go_is_not_held() {
        let gate = Gate::new(3, Some(7), true);
        assert!(gate.is_open());
        assert_eq!(gate.frame(1000, None, "Progressive"), Frame::Show { first: false });
    }
}
