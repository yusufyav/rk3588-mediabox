//! A live channel's opening, from the control plane's side: the display
//! matched to the stream before a picture of it is shown.
//!
//! Every channel opened, cold or by a zap, is numbered (its epoch,
//! [`crate::player::PlayerManager::next_epoch`]) and the player holds its
//! pictures at its opening gate (mediabox-live `gate`). This reads the rate
//! the stream is at -- the receiver's reading of the SPS of the service
//! selected, which comes with the first GOP and so before the player has a
//! picture to show; the player's own pictures only for a stream whose SPS
//! the receiver cannot read -- matches the display to it, waits for the
//! owner's report of that match (that request, not the film's last one),
//! opens the sound device again if a mode commit has stopped it since it
//! was opened, and lets the channel go. A display that fits already lets it
//! go at once, before its first picture is decoded.
//!
//! Each step is the channel's only while it is the one opened last: a newer
//! one ends this at once (`stale`), and nothing of this one -- a mode it
//! asked for, the owner's late report, a sound device opened again -- is
//! taken for the newer one's. A mode the owner does not commit in
//! [`MODE_WAIT`], or reports it could not, still lets the channel go, at the
//! mode on the wire, and says why: the channel is not left dark, and its
//! display is not changed under it later.

use std::time::Duration;

use mediabox_core::Cadence;
use serde_json::Value;

use crate::output::ContentProgress;

/// How often the receiver's status and the player's gate are read.
pub const POLL: Duration = Duration::from_millis(10);
/// The owner commits a mode in 136-240 ms (FAZ 2E, the Plus).
pub const MODE_WAIT: Duration = Duration::from_secs(2);
/// A channel with neither an SPS nor a picture this long has none to match
/// (a radio service, a scrambled picture): let go as it is.
pub const RATE_PATIENCE: Duration = Duration::from_secs(10);

/// What the opening needs of the rest of the control plane.
pub(crate) trait GateIo {
    /// The channel is still the one opened last.
    fn current(&self) -> bool;
    /// The receiver's reading of the selected service's SPS.
    fn stream_rate(&self) -> Option<f64>;
    /// The player's gate (`mediabox-gate`), if it answers.
    async fn player_gate(&self) -> Option<Value>;
    /// Match the display to `cadence`: whether it is on the wire already,
    /// and the request's number.
    fn start_mode(&self, cadence: Cadence) -> Result<(bool, u64), String>;
    fn mode_progress(&self, request: u64) -> ContentProgress;
    fn mode_changed(&self);
    fn sound_current(&self) -> bool;
    async fn reload_sound(&self) -> Result<(), String>;
    async fn open(&self, outcome: &str) -> Result<(), String>;
    /// The display was matched to `cadence` for this channel.
    fn matched(&self, cadence: Cadence);
}

/// How an opening ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// Let go: how (`fast`, `matched`, `mode-timeout`, ...).
    Opened(String),
    /// A newer channel took over, at this step.
    Stale(&'static str),
    /// The player would not take it.
    Refused(String),
}

/// Open the channel: see the module.
pub(crate) async fn open_channel<I: GateIo>(io: &I) -> End {
    let started = tokio::time::Instant::now();
    // The stream's rate.
    let (cadence, source) = loop {
        if !io.current() {
            return End::Stale("rate");
        }
        if let Some(cadence) = io.stream_rate().and_then(Cadence::from_fps) {
            break (cadence, "sps");
        }
        if let Some(gate) = io.player_gate().await {
            if gate.get("open").and_then(Value::as_bool) == Some(true) {
                // Opened by the player itself (its own limit), or a branch
                // put up again: nothing to match any more.
                return End::Opened(gate.get("outcome").and_then(Value::as_str).unwrap_or("player").to_string());
            }
            // Pictures, and still no SPS: the receiver read the same bytes
            // first, so it cannot read this one.
            if gate.get("preroll").and_then(Value::as_u64).unwrap_or(0) >= 2
                && let Some(cadence) = gate.get("rate").and_then(Value::as_f64).and_then(Cadence::from_fps)
            {
                break (cadence, "decoder");
            }
        }
        if started.elapsed() >= RATE_PATIENCE {
            return finish(io, "no-rate").await;
        }
        tokio::time::sleep(POLL).await;
    };
    mediabox_core::timeline!("gate-cadence", "cadence={} source={source} ms={}", cadence.label(), started.elapsed().as_millis());
    // The mode.
    let (applied, request) = match io.start_mode(cadence) {
        Ok(answer) => answer,
        Err(why) => {
            eprintln!("mediaboxd-rs: live channel: display not matched to {} fps: {why}", cadence.label());
            return finish(io, "mode-refused").await;
        }
    };
    mediabox_core::timeline!("gate-mode", "request={request} applied={}", u8::from(applied));
    let mut outcome = "fast";
    if !applied {
        let asked = tokio::time::Instant::now();
        outcome = loop {
            if !io.current() {
                return End::Stale("mode");
            }
            match io.mode_progress(request) {
                ContentProgress::Applied => {
                    io.mode_changed();
                    break "matched";
                }
                ContentProgress::Failed(why) => {
                    eprintln!("mediaboxd-rs: live channel: the owner could not commit the mode for {} fps: {why}", cadence.label());
                    break "mode-failed";
                }
                // Another match took its place: a newer channel's, or the
                // display changed by a person.
                ContentProgress::Superseded => return End::Stale("mode-superseded"),
                ContentProgress::Pending if asked.elapsed() >= MODE_WAIT => {
                    eprintln!("mediaboxd-rs: live channel: the owner did not report the mode for {} fps in {} s", cadence.label(), MODE_WAIT.as_secs());
                    break "mode-timeout";
                }
                ContentProgress::Pending => tokio::time::sleep(POLL).await,
            }
        };
        mediabox_core::timeline!("gate-mode-done", "request={request} outcome={outcome} ms={}", asked.elapsed().as_millis());
    }
    if outcome == "fast" || outcome == "matched" {
        io.matched(cadence);
    }
    // The sound device, opened again after a commit that stopped it.
    if !io.sound_current() {
        if !io.current() {
            return End::Stale("sound");
        }
        let reopened = io.reload_sound().await;
        mediabox_core::timeline!("gate-reload", "ok={}", u8::from(reopened.is_ok()));
        if let Err(why) = reopened {
            eprintln!("mediaboxd-rs: live channel: the sound device was not opened again after the mode change: {why}");
            outcome = match outcome {
                "matched" => "matched-sound-failed",
                other => other,
            };
        }
    }
    finish(io, outcome).await
}

/// A player just started may not have its socket up yet: asked again while
/// it does not answer, this long at most.
pub const OPEN_PATIENCE: Duration = Duration::from_secs(5);

async fn finish<I: GateIo>(io: &I, outcome: &str) -> End {
    let asked = tokio::time::Instant::now();
    loop {
        if !io.current() {
            return End::Stale("open");
        }
        match io.open(outcome).await {
            Ok(()) => return End::Opened(outcome.to_string()),
            Err(why) if why == NO_ANSWER && asked.elapsed() < OPEN_PATIENCE => tokio::time::sleep(POLL).await,
            Err(why) => return End::Refused(why),
        }
    }
}

/// What [`GateIo::open`] says when the player did not answer at all.
pub const NO_ANSWER: &str = "no answer";

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// The rest of the control plane, scripted.
    #[derive(Default)]
    struct Fake {
        current_until: Cell<Option<usize>>,
        polls: Cell<usize>,
        sps: RefCell<Option<f64>>,
        sps_after: Cell<usize>,
        gate: RefCell<Option<Value>>,
        on_wire: Cell<bool>,
        mode: RefCell<Vec<ContentProgress>>,
        modes_asked: RefCell<Vec<String>>,
        changes: Cell<u64>,
        opened_after: Cell<u64>,
        reload: RefCell<Option<Result<(), String>>>,
        reloads: Cell<u32>,
        opened: RefCell<Vec<String>>,
        refuse: Cell<bool>,
        silent_opens: Cell<u32>,
        matched: RefCell<Option<Cadence>>,
    }

    impl GateIo for Fake {
        fn current(&self) -> bool {
            self.polls.set(self.polls.get() + 1);
            self.current_until.get().is_none_or(|until| self.polls.get() <= until)
        }
        fn stream_rate(&self) -> Option<f64> {
            (self.polls.get() > self.sps_after.get()).then(|| *self.sps.borrow()).flatten()
        }
        async fn player_gate(&self) -> Option<Value> {
            self.gate.borrow().clone()
        }
        fn start_mode(&self, cadence: Cadence) -> Result<(bool, u64), String> {
            self.modes_asked.borrow_mut().push(cadence.label());
            Ok((self.on_wire.get(), 9))
        }
        fn mode_progress(&self, request: u64) -> ContentProgress {
            assert_eq!(request, 9);
            let mut script = self.mode.borrow_mut();
            if script.len() > 1 { script.remove(0) } else { script.first().cloned().unwrap_or(ContentProgress::Pending) }
        }
        fn mode_changed(&self) {
            self.changes.set(self.changes.get() + 1);
        }
        fn sound_current(&self) -> bool {
            self.opened_after.get() >= self.changes.get()
        }
        async fn reload_sound(&self) -> Result<(), String> {
            self.reloads.set(self.reloads.get() + 1);
            let result = self.reload.borrow().clone().unwrap_or(Ok(()));
            if result.is_ok() {
                self.opened_after.set(self.changes.get());
            }
            result
        }
        async fn open(&self, outcome: &str) -> Result<(), String> {
            self.opened.borrow_mut().push(outcome.to_string());
            if self.silent_opens.get() > 0 {
                self.silent_opens.set(self.silent_opens.get() - 1);
                return Err(NO_ANSWER.into());
            }
            if self.refuse.get() { Err("stale: epoch 1".into()) } else { Ok(()) }
        }
        fn matched(&self, cadence: Cadence) {
            *self.matched.borrow_mut() = Some(cadence);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_display_that_fits_lets_the_channel_go_at_once() {
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Opened("fast".into()));
        assert_eq!(io.reloads.get(), 0, "no mode change, no sound device opened again");
        assert_eq!(*io.opened.borrow(), ["fast"]);
        assert_eq!(*io.matched.borrow(), Cadence::from_fps(50.0));
    }

    #[tokio::test(start_paused = true)]
    async fn another_rate_is_matched_and_the_sound_opened_again_before_it_goes() {
        let io = Fake {
            sps: RefCell::new(Some(30.0)),
            sps_after: Cell::new(5),
            mode: RefCell::new(vec![ContentProgress::Pending, ContentProgress::Pending, ContentProgress::Applied]),
            ..Fake::default()
        };
        assert_eq!(open_channel(&io).await, End::Opened("matched".into()));
        assert_eq!(*io.modes_asked.borrow(), ["30"]);
        assert_eq!(io.reloads.get(), 1);
        assert_eq!(*io.opened.borrow(), ["matched"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_newer_channel_ends_it_at_any_step_and_nothing_is_let_go() {
        for until in [1, 8, 12] {
            let io = Fake {
                current_until: Cell::new(Some(until)),
                sps: RefCell::new(Some(30.0)),
                sps_after: Cell::new(5),
                mode: RefCell::new(vec![ContentProgress::Pending]),
                ..Fake::default()
            };
            assert!(matches!(open_channel(&io).await, End::Stale(_)), "at poll {until}");
            assert!(io.opened.borrow().is_empty(), "nothing opened for a channel replaced (poll {until})");
            assert_eq!(io.reloads.get(), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_match_taken_over_is_not_waited_for() {
        let io = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![ContentProgress::Superseded]), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Stale("mode-superseded"));
        assert!(io.opened.borrow().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_mode_never_reported_lets_the_channel_go_after_its_wait_and_says_so() {
        let io = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![ContentProgress::Pending]), ..Fake::default() };
        let started = tokio::time::Instant::now();
        assert_eq!(open_channel(&io).await, End::Opened("mode-timeout".into()));
        assert!(started.elapsed() >= MODE_WAIT && started.elapsed() < MODE_WAIT + Duration::from_millis(100));
        assert_eq!(io.reloads.get(), 0, "nothing committed: nothing stopped");
        assert_eq!(*io.matched.borrow(), None, "not matched: no format change judged against it");
    }

    #[tokio::test(start_paused = true)]
    async fn a_mode_the_owner_could_not_commit_is_said_at_once() {
        let io = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![ContentProgress::Failed("commit: EINVAL".into())]), ..Fake::default() };
        let started = tokio::time::Instant::now();
        assert_eq!(open_channel(&io).await, End::Opened("mode-failed".into()));
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn a_sound_device_that_will_not_open_is_said_and_the_picture_goes() {
        let io = Fake {
            sps: RefCell::new(Some(30.0)),
            mode: RefCell::new(vec![ContentProgress::Applied]),
            reload: RefCell::new(Some(Err("no answer".into()))),
            ..Fake::default()
        };
        assert_eq!(open_channel(&io).await, End::Opened("matched-sound-failed".into()));
        assert_eq!(io.reloads.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sound_device_stopped_by_an_earlier_commit_is_opened_again_even_on_a_fit() {
        // A channel before this one changed the mode, and its reload never
        // came: this one's display fits, but the device is still stopped.
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), changes: Cell::new(1), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Opened("fast".into()));
        assert_eq!(io.reloads.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_players_pictures_give_the_rate_only_when_the_receiver_cannot() {
        let gate = serde_json::json!({"open": false, "preroll": 3, "rate": 25.0});
        let io = Fake { gate: RefCell::new(Some(gate)), on_wire: Cell::new(true), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Opened("fast".into()));
        assert_eq!(*io.modes_asked.borrow(), ["25"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_channel_with_nothing_to_match_is_let_go_after_its_patience() {
        let io = Fake::default();
        let started = tokio::time::Instant::now();
        assert_eq!(open_channel(&io).await, End::Opened("no-rate".into()));
        assert!(started.elapsed() >= RATE_PATIENCE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_player_not_answering_yet_is_asked_again() {
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), silent_opens: Cell::new(3), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Opened("fast".into()));
        assert_eq!(io.opened.borrow().len(), 4);
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), silent_opens: Cell::new(10_000), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Refused(NO_ANSWER.into()), "not for ever");
    }

    #[tokio::test(start_paused = true)]
    async fn a_player_that_refuses_the_number_is_said() {
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), refuse: Cell::new(true), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Refused("stale: epoch 1".into()));
    }
}
