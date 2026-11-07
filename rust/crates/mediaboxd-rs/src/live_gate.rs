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
//! taken for the newer one's.
//!
//! The channel goes only on the display it was matched to and with its sound
//! device open. Anything else fails it, shut (`End::Failed`, a code said on the
//! timeline, in the journal, the player's health file and the film's end): a
//! mode refused, failed, taken over or not reported in [`MODE_WAIT`]; a sound
//! device that will not open; a picture service with no rate in
//! [`RATE_PATIENCE`]. Nothing of it is shown or heard, a late answer opens
//! nothing (the player's gate is failed), and the next channel opens as ever.
//! A radio service has no picture to match: the receiver's reading of its PMT
//! says so, and it goes at once.

use std::time::Duration;

use mediabox_core::Cadence;
use serde_json::Value;

use crate::output::ContentProgress;

/// How often the receiver's status and the player's gate are read.
pub const POLL: Duration = Duration::from_millis(10);
/// The owner commits a mode in 136-240 ms (FAZ 2E, the Plus).
pub const MODE_WAIT: Duration = Duration::from_secs(2);
/// A picture service with neither an SPS nor a picture this long cannot be
/// matched (a scrambled picture, a stream with no sequence header): failed.
pub const RATE_PATIENCE: Duration = Duration::from_secs(10);

/// The next opening's mode failing on purpose (`gate-faults`): one word in
/// [`FAULT_FILE`], taken once, so the channel after it opens as ever. The
/// display's owner is not asked, so nothing of a real commit is touched. A
/// product build has `None` as a constant.
#[cfg(feature = "gate-faults")]
pub const FAULT_FILE: &str = "/run/mediabox-gate-fault";

#[cfg(feature = "gate-faults")]
fn injected() -> Option<ContentProgress> {
    let word = std::fs::read_to_string(FAULT_FILE).ok()?;
    let _ = std::fs::remove_file(FAULT_FILE);
    match word.trim() {
        "mode-failed" => Some(ContentProgress::Failed("injected".into())),
        "mode-timeout" => Some(ContentProgress::Pending),
        "mode-superseded" => Some(ContentProgress::Superseded),
        _ => None,
    }
}

#[cfg(not(feature = "gate-faults"))]
#[inline(always)]
fn injected() -> Option<ContentProgress> {
    None
}

/// What the opening needs of the rest of the control plane.
pub(crate) trait GateIo {
    /// The channel is still the one opened last.
    fn current(&self) -> bool;
    /// The receiver's reading of the selected service's SPS.
    fn stream_rate(&self) -> Option<f64>;
    /// The receiver's reading of its PMT: whether it has a picture stream.
    /// `None`: not read yet.
    fn has_picture(&self) -> Option<bool>;
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
    /// The channel cannot be let go: its player told, and it and its
    /// receiver ended with `code` as the film's end. False: a newer channel
    /// took over meanwhile, and nothing was done.
    async fn give_up(&self, code: &str) -> bool;
    /// The code this channel's player failed it with by itself (its own
    /// limit, a sound device it never got), once that player is gone and the
    /// channel with it: its last word on this channel. `None`: not that.
    fn own_failure(&self) -> Option<String> {
        None
    }
}

/// The channel is not the one opened last any more: its player ended it with
/// its own code, or a newer channel took over at `step`. Nothing is done
/// either way -- what is on now is not this channel's.
fn gone<I: GateIo>(io: &I, step: &'static str) -> End {
    io.own_failure().map_or(End::Stale(step), End::Failed)
}

/// How an opening ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// Let go: how (`fast`, `matched`, `radio`, ...).
    Opened(String),
    /// A newer channel took over, at this step.
    Stale(&'static str),
    /// Failed shut: the code (`mode-failed`, `sound-failed`, ...).
    Failed(String),
}

/// Open the channel, or fail it and end it: see the module.
pub(crate) async fn open_channel<I: GateIo>(io: &I) -> End {
    match open_it(io).await {
        End::Failed(code) => {
            eprintln!("mediaboxd-rs: live channel: not opened: {code}");
            if io.current() && io.give_up(&code).await { End::Failed(code) } else { gone(io, "give-up") }
        }
        end => end,
    }
}

async fn open_it<I: GateIo>(io: &I) -> End {
    let started = tokio::time::Instant::now();
    // The stream's rate.
    let found = loop {
        if !io.current() {
            return gone(io, "rate");
        }
        if let Some(cadence) = io.stream_rate().and_then(Cadence::from_fps) {
            break Some((cadence, "sps"));
        }
        // No picture stream in its PMT: nothing to match, nothing to wait for.
        if io.has_picture() == Some(false) {
            break None;
        }
        if let Some(gate) = io.player_gate().await {
            if let Some(code) = gate.get("failed").and_then(Value::as_str) {
                return End::Failed(code.to_string());
            }
            if gate.get("open").and_then(Value::as_bool) == Some(true) {
                // A branch put up again after it was let go.
                return End::Opened(gate.get("outcome").and_then(Value::as_str).unwrap_or("player").to_string());
            }
            // Pictures, and still no SPS: the receiver read the same bytes
            // first, so it cannot read this one.
            if gate.get("preroll").and_then(Value::as_u64).unwrap_or(0) >= 2
                && let Some(cadence) = gate.get("rate").and_then(Value::as_f64).and_then(Cadence::from_fps)
            {
                break Some((cadence, "decoder"));
            }
        }
        if started.elapsed() >= RATE_PATIENCE {
            return End::Failed(if io.has_picture().is_some() { "no-rate" } else { "no-service" }.to_string());
        }
        tokio::time::sleep(POLL).await;
    };
    let Some((cadence, source)) = found else {
        mediabox_core::timeline!("gate-cadence", "cadence=none source=pmt ms={}", started.elapsed().as_millis());
        return sound_and_open(io, "radio").await;
    };
    mediabox_core::timeline!("gate-cadence", "cadence={} source={source} ms={}", cadence.label(), started.elapsed().as_millis());
    // The mode.
    let fault = injected();
    let answer = if fault.is_some() { Ok((false, 0)) } else { io.start_mode(cadence) };
    let (applied, request) = match answer {
        Ok(answer) => answer,
        Err(why) => {
            eprintln!("mediaboxd-rs: live channel: display not matched to {} fps: {why}", cadence.label());
            return End::Failed("mode-refused".to_string());
        }
    };
    mediabox_core::timeline!("gate-mode", "request={request} applied={}", u8::from(applied));
    let mut outcome = "fast";
    if !applied {
        let asked = tokio::time::Instant::now();
        let failed = loop {
            if !io.current() {
                return gone(io, "mode");
            }
            match fault.clone().unwrap_or_else(|| io.mode_progress(request)) {
                ContentProgress::Applied => {
                    io.mode_changed();
                    break None;
                }
                ContentProgress::Failed(why) => {
                    eprintln!("mediaboxd-rs: live channel: the owner could not commit the mode for {} fps: {why}", cadence.label());
                    break Some("mode-failed");
                }
                // Another match took its place, and not a newer channel's
                // (asked above): the display changed by a person, say. This
                // channel's rate is not what it will be on.
                ContentProgress::Superseded => break Some("mode-superseded"),
                ContentProgress::Pending if asked.elapsed() >= MODE_WAIT => {
                    eprintln!("mediaboxd-rs: live channel: the owner did not report the mode for {} fps in {} s", cadence.label(), MODE_WAIT.as_secs());
                    break Some("mode-timeout");
                }
                ContentProgress::Pending => tokio::time::sleep(POLL).await,
            }
        };
        mediabox_core::timeline!("gate-mode-done", "request={request} outcome={} ms={}", failed.unwrap_or("matched"), asked.elapsed().as_millis());
        if let Some(code) = failed {
            return End::Failed(code.to_string());
        }
        outcome = "matched";
    }
    io.matched(cadence);
    sound_and_open(io, outcome).await
}

/// The sound device, opened again after a commit that stopped it, and the
/// channel let go.
async fn sound_and_open<I: GateIo>(io: &I, outcome: &str) -> End {
    // Stopped by a commit, or found taken: the player's word on it, read after
    // the commit's. No answer yet is a player just started, whose own opening
    // says so if it comes to that.
    let device = io.player_gate().await.and_then(|gate| gate.get("sound").and_then(Value::as_str).map(str::to_string));
    if !io.sound_current() || matches!(device.as_deref(), Some("stand-in" | "given-up")) {
        if !io.current() {
            return gone(io, "sound");
        }
        let reopened = io.reload_sound().await;
        mediabox_core::timeline!("gate-reload", "ok={}", u8::from(reopened.is_ok()));
        if let Err(why) = reopened {
            eprintln!("mediaboxd-rs: live channel: the sound device was not opened again after the mode change: {why}");
            return End::Failed("sound-failed".to_string());
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
            return gone(io, "open");
        }
        match io.open(outcome).await {
            Ok(()) => return End::Opened(outcome.to_string()),
            Err(why) if why == NO_ANSWER && asked.elapsed() < OPEN_PATIENCE => tokio::time::sleep(POLL).await,
            Err(why) if why == NO_ANSWER => return End::Failed("player-silent".to_string()),
            Err(why) if why.starts_with("stale") => return End::Stale("open"),
            // Its gate failed already, by its own limit: that code.
            Err(why) => return End::Failed(why.strip_prefix("failed: ").unwrap_or("open-refused").to_string()),
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
        refuse: RefCell<Option<String>>,
        silent_opens: Cell<u32>,
        matched: RefCell<Option<Cadence>>,
        has_picture: Cell<Option<bool>>,
        given_up: RefCell<Vec<String>>,
        newer_at_give_up: Cell<bool>,
        own_failure: RefCell<Option<String>>,
    }

    impl GateIo for Fake {
        fn current(&self) -> bool {
            self.polls.set(self.polls.get() + 1);
            self.current_until.get().is_none_or(|until| self.polls.get() <= until)
        }
        fn stream_rate(&self) -> Option<f64> {
            (self.polls.get() > self.sps_after.get()).then(|| *self.sps.borrow()).flatten()
        }
        fn has_picture(&self) -> Option<bool> {
            self.has_picture.get()
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
            match self.refuse.borrow().clone() {
                Some(why) => Err(why),
                None => Ok(()),
            }
        }
        fn matched(&self, cadence: Cadence) {
            *self.matched.borrow_mut() = Some(cadence);
        }
        async fn give_up(&self, code: &str) -> bool {
            if self.newer_at_give_up.get() {
                return false;
            }
            self.given_up.borrow_mut().push(code.to_string());
            true
        }
        fn own_failure(&self) -> Option<String> {
            self.own_failure.borrow().clone()
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
            let started = tokio::time::Instant::now();
            assert!(matches!(open_channel(&io).await, End::Stale(_)), "at poll {until}");
            assert!(started.elapsed() < Duration::from_millis(200), "ended at once, not after a wait: {:?} (poll {until})", started.elapsed());
            assert!(io.opened.borrow().is_empty(), "nothing opened for a channel replaced (poll {until})");
            assert_eq!(io.reloads.get(), 0);
        }
    }

    /// Nothing of a failed channel is let go, its player and receiver are
    /// ended with the code, and it stops being matched.
    fn failed_shut(io: &Fake, end: End, code: &str) {
        assert_eq!(end, End::Failed(code.into()));
        assert!(io.opened.borrow().is_empty(), "{code}: nothing let go");
        assert_eq!(*io.given_up.borrow(), [code], "{code}: ended, with its code");
    }

    #[tokio::test(start_paused = true)]
    async fn a_match_taken_over_fails_the_channel_at_once() {
        let io = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![ContentProgress::Superseded]), ..Fake::default() };
        let started = tokio::time::Instant::now();
        failed_shut(&io, open_channel(&io).await, "mode-superseded");
        assert!(started.elapsed() < Duration::from_millis(50));
        assert_eq!(*io.matched.borrow(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_mode_never_reported_fails_the_channel_after_its_wait() {
        let io = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![ContentProgress::Pending]), ..Fake::default() };
        let started = tokio::time::Instant::now();
        failed_shut(&io, open_channel(&io).await, "mode-timeout");
        assert!(started.elapsed() >= MODE_WAIT && started.elapsed() < MODE_WAIT + Duration::from_millis(100));
        assert_eq!(io.reloads.get(), 0, "nothing committed: nothing stopped");
        assert_eq!(*io.matched.borrow(), None, "not matched: no format change judged against it");
    }

    #[tokio::test(start_paused = true)]
    async fn a_mode_the_owner_could_not_commit_fails_the_channel_at_once() {
        let io = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![ContentProgress::Failed("commit: EINVAL".into())]), ..Fake::default() };
        let started = tokio::time::Instant::now();
        failed_shut(&io, open_channel(&io).await, "mode-failed");
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn a_mode_applied_after_the_wait_opens_nothing() {
        // Pending for all of the wait, Applied the poll after: too late, and
        // nothing asks again after the end.
        let polls_in_wait = (MODE_WAIT.as_millis() / POLL.as_millis()) as usize + 1;
        let mut script = vec![ContentProgress::Pending; polls_in_wait];
        script.push(ContentProgress::Applied);
        let io = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(script), ..Fake::default() };
        failed_shut(&io, open_channel(&io).await, "mode-timeout");
        assert_eq!(*io.mode.borrow(), [ContentProgress::Applied], "the report after the wait is not read");
    }

    #[tokio::test(start_paused = true)]
    async fn a_sound_device_that_will_not_open_fails_the_channel_picture_and_all() {
        let io = Fake {
            sps: RefCell::new(Some(30.0)),
            mode: RefCell::new(vec![ContentProgress::Applied]),
            reload: RefCell::new(Some(Err("no answer".into()))),
            ..Fake::default()
        };
        failed_shut(&io, open_channel(&io).await, "sound-failed");
        assert_eq!(io.reloads.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sound_device_found_taken_on_a_display_that_fits_fails_too() {
        for (device, works) in [("stand-in", true), ("given-up", false)] {
            let gate = serde_json::json!({"open": false, "sound": device});
            let reload = if works { Ok(()) } else { Err("busy".to_string()) };
            let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), gate: RefCell::new(Some(gate)), reload: RefCell::new(Some(reload)), ..Fake::default() };
            let end = open_channel(&io).await;
            assert_eq!(io.reloads.get(), 1, "{device}");
            if works {
                assert_eq!(end, End::Opened("fast".into()), "{device}: opened once it is open");
            } else {
                failed_shut(&io, end, "sound-failed");
            }
        }
        let gate = serde_json::json!({"open": false, "sound": "open"});
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), gate: RefCell::new(Some(gate)), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Opened("fast".into()));
        assert_eq!(io.reloads.get(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_with_a_newer_channel_on_ends_nothing_of_it() {
        let io = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![ContentProgress::Superseded]), newer_at_give_up: Cell::new(true), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Stale("give-up"));
        assert!(io.given_up.borrow().is_empty());
        assert!(io.opened.borrow().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_next_channel_after_each_failure_opens() {
        // Each failure leaves nothing for the channel after it: another
        // opening, on the state the failed one left (the mode's change
        // counted, the sound device not open).
        for mode in [ContentProgress::Failed("commit".into()), ContentProgress::Superseded, ContentProgress::Pending] {
            let failed = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![mode]), ..Fake::default() };
            assert!(matches!(open_channel(&failed).await, End::Failed(_)));
            let next = Fake { sps: RefCell::new(Some(50.0)), mode: RefCell::new(vec![ContentProgress::Applied]), changes: Cell::new(failed.changes.get()), ..Fake::default() };
            assert_eq!(open_channel(&next).await, End::Opened("matched".into()));
            assert_eq!(*next.opened.borrow(), ["matched"]);
        }
        let failed = Fake { sps: RefCell::new(Some(30.0)), mode: RefCell::new(vec![ContentProgress::Applied]), reload: RefCell::new(Some(Err("busy".into()))), ..Fake::default() };
        assert!(matches!(open_channel(&failed).await, End::Failed(_)));
        let next = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), changes: Cell::new(failed.changes.get()), ..Fake::default() };
        assert_eq!(open_channel(&next).await, End::Opened("fast".into()));
        assert_eq!(next.reloads.get(), 1, "the device the failed one left stopped is opened for it");
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
    async fn a_radio_service_goes_at_once_with_no_mode_asked() {
        let io = Fake { has_picture: Cell::new(Some(false)), ..Fake::default() };
        let started = tokio::time::Instant::now();
        assert_eq!(open_channel(&io).await, End::Opened("radio".into()));
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(io.modes_asked.borrow().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_picture_service_with_no_rate_fails_after_its_patience() {
        let io = Fake { has_picture: Cell::new(Some(true)), ..Fake::default() };
        let started = tokio::time::Instant::now();
        failed_shut(&io, open_channel(&io).await, "no-rate");
        assert!(started.elapsed() >= RATE_PATIENCE);
        let io = Fake::default();
        failed_shut(&io, open_channel(&io).await, "no-service");
    }

    #[tokio::test(start_paused = true)]
    async fn a_gate_the_player_failed_itself_is_that_failure() {
        let gate = serde_json::json!({"open": false, "failed": "player-timeout", "preroll": 300, "rate": 30.0});
        let io = Fake { gate: RefCell::new(Some(gate)), has_picture: Cell::new(Some(true)), ..Fake::default() };
        failed_shut(&io, open_channel(&io).await, "player-timeout");
    }

    /// A player that fails its channel by itself and goes, before the rate is
    /// read: the channel is not current any more, and its end is that code,
    /// not a newer channel's taking over. Nothing is ended or let go for it.
    #[tokio::test(start_paused = true)]
    async fn a_player_gone_by_its_own_failure_is_that_failure_at_any_step() {
        for until in [1, 8, 12] {
            let io = Fake {
                current_until: Cell::new(Some(until)),
                sps: RefCell::new(Some(30.0)),
                sps_after: Cell::new(5),
                mode: RefCell::new(vec![ContentProgress::Pending]),
                own_failure: RefCell::new(Some("sound-failed".into())),
                ..Fake::default()
            };
            assert_eq!(open_channel(&io).await, End::Failed("sound-failed".into()), "at poll {until}");
            assert!(io.opened.borrow().is_empty(), "nothing let go (poll {until})");
            assert!(io.given_up.borrow().is_empty(), "nothing ended: it is not current (poll {until})");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_player_not_answering_yet_is_asked_again() {
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), silent_opens: Cell::new(3), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Opened("fast".into()));
        assert_eq!(io.opened.borrow().len(), 4);
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), silent_opens: Cell::new(10_000), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Failed("player-silent".into()), "not for ever");
        assert_eq!(*io.given_up.borrow(), ["player-silent"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_player_on_a_newer_number_is_stale_and_a_failed_gate_is_failed() {
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), refuse: RefCell::new(Some("stale: epoch 1".into())), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Stale("open"));
        assert!(io.given_up.borrow().is_empty());
        let io = Fake { sps: RefCell::new(Some(50.0)), on_wire: Cell::new(true), refuse: RefCell::new(Some("failed: player-timeout".into())), ..Fake::default() };
        assert_eq!(open_channel(&io).await, End::Failed("player-timeout".into()));
        assert_eq!(*io.given_up.borrow(), ["player-timeout"]);
    }
}
