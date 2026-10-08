//! What this box sends on its own, decided without a bus.
//!
//! Every automatic CEC message -- at start, at shutdown, and in answer to
//! the television -- is decided here from the kept settings, what this boot
//! has done ([`CecSession`]) and the message that arrived. Nothing here
//! touches an adapter, so every decision is a test.
//!
//! The model is AOSP's playback device (`HdmiCecLocalDevicePlayback`):
//!
//! * One Touch Play at boot: `<Image View On>`, then `<Active Source>`.
//! * `<Set Stream Path>` or `<Routing Change>` to this box's physical
//!   address makes it the active source, and it says so.
//! * `<Request Active Source>` is answered only by the active source.
//! * `<Give Device Power Status>` is answered "on": a box that is running
//!   is on.
//! * At standby, `<Standby>` goes out only if this box was the active
//!   source (`onStandby`: `wasActiveSource`), so shutting the box down
//!   while somebody watches another input does not turn their television
//!   off. AOSP requires a positive answer; here a bus that has said nothing
//!   either way counts as this box's, because a box whose start sequence is
//!   turned off would otherwise never turn the television off at all.
//! * Another device's `<Active Source>`, or the television routing away,
//!   is "active source lost".
//! * At power-off, an active source that is not turning the television off
//!   tells it `<Inactive Source>` (AOSP `onStandby` with
//!   `power_control_mode` `none`), so the television can go back to another
//!   input. Which one is the television's choice: no standard message lets a
//!   playback device switch a television's input.

use mediabox_core::{ActiveSource, CecEvent, CecSession, CecSettings, CecSourceLost, TvPower};

pub const TV: u8 = 0;
pub const BROADCAST: u8 = 15;

const ROUTING_CHANGE: u8 = 0x80;
const ACTIVE_SOURCE: u8 = 0x82;
const REQUEST_ACTIVE_SOURCE: u8 = 0x85;
const SET_STREAM_PATH: u8 = 0x86;
const GIVE_DEVICE_POWER_STATUS: u8 = 0x8f;
const REPORT_POWER_STATUS: u8 = 0x90;
const INACTIVE_SOURCE: u8 = 0x9d;
const STANDBY: u8 = 0x36;

/// Why the daemon is stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The machine is powering off or halting.
    PowerOff,
    /// The machine is restarting: it comes straight back, so nothing that
    /// belongs to "the box is going away" is done.
    Reboot,
    /// Only the daemon is stopping -- a deploy, a restart of the unit.
    Daemon,
}

/// What to send at start, in order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Start {
    /// Ask the television its power state first, so "put it back as it was"
    /// knows what "as it was" is.
    pub ask_power: bool,
    pub wake: bool,
    pub active_source: bool,
}

impl Start {
    pub fn anything(&self) -> bool {
        self.ask_power || self.wake || self.active_source
    }
}

pub fn start(settings: &CecSettings, session: &CecSession) -> Start {
    if !settings.enabled || session.start_done {
        return Start::default();
    }
    Start {
        ask_power: settings.wake_tv_on_start || settings.restore_power_on_shutdown,
        wake: settings.wake_tv_on_start,
        active_source: settings.active_source_on_start,
    }
}

/// Whether `<Image View On>` should actually go: not to a television that
/// said it is already on, which would only switch its input as a side
/// effect `active_source` was not asked for.
pub fn wake_needed(plan: &Start, tv: TvPower) -> bool {
    plan.wake && tv != TvPower::On
}

/// What goes out at shutdown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shutdown {
    /// Where `<Standby>` goes; empty for none.
    pub standby: Vec<u8>,
    /// `<Inactive Source>` to the television, after the standbys.
    pub inactive_source: bool,
}

pub fn shutdown(settings: &CecSettings, session: &CecSession, stop: Stop) -> Shutdown {
    if !settings.enabled || stop != Stop::PowerOff {
        return Shutdown::default();
    }
    if session.active_source == ActiveSource::Other {
        return Shutdown::default();
    }
    let standby = if settings.standby_tv_on_shutdown {
        settings.power_target.destinations().to_vec()
    } else if settings.restore_power_on_shutdown
        && session.woke_tv
        && session.tv_power_at_start == TvPower::Standby
    {
        // Only the television's starting state is known, so only the
        // television is put back.
        vec![TV]
    } else {
        Vec::new()
    };
    // Only a box the bus has seen as the source says it is leaving; a
    // television being turned off has nothing to switch to.
    let tv_off = standby.iter().any(|to| *to == TV || *to == BROADCAST);
    Shutdown {
        inactive_source: session.active_source == ActiveSource::Us && !tv_off,
        standby,
    }
}

/// Whether the policy reads this opcode at all; everything else -- the
/// remote's keys above all -- never reaches it.
pub fn relevant(opcode: Option<u8>) -> bool {
    matches!(
        opcode,
        Some(
            ROUTING_CHANGE
                | ACTIVE_SOURCE
                | REQUEST_ACTIVE_SOURCE
                | SET_STREAM_PATH
                | GIVE_DEVICE_POWER_STATUS
                | REPORT_POWER_STATUS
                | INACTIVE_SOURCE
                | STANDBY
        )
    )
}

/// What to do about a message that arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Broadcast `<Active Source>` with this box's physical address.
    AnnounceActiveSource,
    /// `<Report Power Status>` "on" to this address.
    ReportPowerOn(u8),
    /// The television went to another input while this box was on it, and
    /// the setting says to stand by.
    SourceLost,
}

fn address(operands: &[u8], at: usize) -> Option<u16> {
    Some(((*operands.get(at)? as u16) << 8) | *operands.get(at + 1)? as u16)
}

/// Updates the session from `event` and says what to answer.
///
/// `own_logical` and `own_physical` are the adapter's as the kernel holds
/// them now. Nothing is answered with CEC off: the adapters are released
/// then, so this is the second lock on the same door.
pub fn receive(
    settings: &CecSettings,
    session: &mut CecSession,
    own_logical: u8,
    own_physical: u16,
    event: &CecEvent,
) -> Vec<Reaction> {
    if !settings.enabled || event.initiator == own_logical {
        return Vec::new();
    }
    let Some(opcode) = event.opcode else {
        return Vec::new();
    };
    let lost = |session: &mut CecSession| -> Vec<Reaction> {
        let was = session.active_source;
        session.active_source = ActiveSource::Other;
        if was == ActiveSource::Us && settings.on_active_source_lost == CecSourceLost::Standby {
            vec![Reaction::SourceLost]
        } else {
            Vec::new()
        }
    };
    let routed = |session: &mut CecSession, to: Option<u16>| -> Vec<Reaction> {
        match to {
            Some(to) if to == own_physical => {
                session.active_source = ActiveSource::Us;
                vec![Reaction::AnnounceActiveSource]
            }
            Some(_) => lost(session),
            None => Vec::new(),
        }
    };
    match opcode {
        ACTIVE_SOURCE => match address(&event.operands, 0) {
            Some(from) if from != own_physical => lost(session),
            _ => Vec::new(),
        },
        // Only the television routes.
        SET_STREAM_PATH if event.initiator == TV => routed(session, address(&event.operands, 0)),
        ROUTING_CHANGE if event.initiator == TV => routed(session, address(&event.operands, 2)),
        REQUEST_ACTIVE_SOURCE if session.active_source == ActiveSource::Us => {
            vec![Reaction::AnnounceActiveSource]
        }
        INACTIVE_SOURCE => {
            if session.active_source == ActiveSource::Other {
                session.active_source = ActiveSource::Unknown;
            }
            Vec::new()
        }
        GIVE_DEVICE_POWER_STATUS if event.destination == own_logical => {
            vec![Reaction::ReportPowerOn(event.initiator)]
        }
        REPORT_POWER_STATUS if event.initiator == TV => {
            if let Some(status) = event.operands.first() {
                session.tv_power = TvPower::from_report(*status);
            }
            Vec::new()
        }
        STANDBY if event.initiator == TV => {
            // The television is off: it shows nobody.
            session.tv_power = TvPower::Standby;
            session.active_source = ActiveSource::Unknown;
            Vec::new()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::{CecChange, CecPowerTarget};

    const US: u8 = 4;
    const PA: u16 = 0x1000;

    fn msg(initiator: u8, destination: u8, bytes: &[u8]) -> CecEvent {
        CecEvent {
            initiator,
            destination,
            opcode: bytes.first().copied(),
            operands: bytes.get(1..).unwrap_or_default().to_vec(),
            timestamp_ns: 0,
        }
    }

    fn on() -> CecSettings {
        CecSettings::default()
    }

    #[test]
    fn nothing_is_sent_with_cec_off_whatever_else_is_set() {
        let settings = CecSettings {
            wake_tv_on_start: true,
            active_source_on_start: true,
            standby_tv_on_shutdown: true,
            on_active_source_lost: CecSourceLost::Standby,
            ..on()
        }
        .with(CecChange::Enabled(false));
        let mut session = CecSession { active_source: ActiveSource::Us, ..Default::default() };
        assert!(!start(&settings, &session).anything());
        assert!(shutdown(&settings, &session, Stop::PowerOff).standby.is_empty());
        for bytes in [&[0x86, 0x10, 0x00][..], &[0x85], &[0x8f], &[0x82, 0x20, 0x00]] {
            assert!(receive(&settings, &mut session, US, PA, &msg(0, 15, bytes)).is_empty());
        }
    }

    #[test]
    fn start_follows_the_two_switches_and_runs_once() {
        assert!(!start(&on(), &CecSession::default()).anything());
        let both = CecSettings { wake_tv_on_start: true, active_source_on_start: true, ..on() };
        let plan = start(&both, &CecSession::default());
        assert!(plan.ask_power && plan.wake && plan.active_source);
        let only_source = CecSettings { active_source_on_start: true, ..on() };
        let plan = start(&only_source, &CecSession::default());
        assert!(!plan.wake && plan.active_source && !plan.ask_power);
        let done = CecSession { start_done: true, ..Default::default() };
        assert!(!start(&both, &done).anything());
    }

    #[test]
    fn a_television_that_is_on_is_not_woken() {
        let plan = Start { wake: true, ..Default::default() };
        assert!(!wake_needed(&plan, TvPower::On));
        assert!(wake_needed(&plan, TvPower::Standby));
        assert!(wake_needed(&plan, TvPower::Unknown));
    }

    #[test]
    fn reboot_and_a_daemon_restart_send_nothing() {
        let settings = CecSettings { standby_tv_on_shutdown: true, ..on() };
        let session = CecSession { active_source: ActiveSource::Us, ..Default::default() };
        assert_eq!(shutdown(&settings, &session, Stop::PowerOff).standby, [TV]);
        assert!(shutdown(&settings, &session, Stop::Reboot).standby.is_empty());
        assert!(shutdown(&settings, &session, Stop::Daemon).standby.is_empty());
    }

    #[test]
    fn shutdown_goes_to_the_chosen_target() {
        let session = CecSession::default();
        for (target, expected) in [
            (CecPowerTarget::Tv, vec![0]),
            (CecPowerTarget::TvAndAudioSystem, vec![0, 5]),
            (CecPowerTarget::Broadcast, vec![15]),
        ] {
            let settings = CecSettings { standby_tv_on_shutdown: true, power_target: target, ..on() };
            assert_eq!(shutdown(&settings, &session, Stop::PowerOff).standby, expected);
        }
    }

    #[test]
    fn shutdown_leaves_a_television_showing_another_input_alone() {
        let settings = CecSettings { standby_tv_on_shutdown: true, ..on() };
        let other = CecSession { active_source: ActiveSource::Other, ..Default::default() };
        assert!(shutdown(&settings, &other, Stop::PowerOff).standby.is_empty());
    }

    #[test]
    fn restore_puts_back_only_a_television_this_box_woke_from_standby() {
        let settings = CecSettings { restore_power_on_shutdown: true, ..on() };
        let case = |at_start, woke| CecSession {
            tv_power_at_start: at_start,
            woke_tv: woke,
            active_source: ActiveSource::Us,
            ..Default::default()
        };
        assert_eq!(shutdown(&settings, &case(TvPower::Standby, true), Stop::PowerOff).standby, [TV]);
        // Already on: left on.
        assert!(shutdown(&settings, &case(TvPower::On, false), Stop::PowerOff).standby.is_empty());
        // Unknown: left alone.
        assert!(shutdown(&settings, &case(TvPower::Unknown, true), Stop::PowerOff).standby.is_empty());
        // In standby but not woken by this box (wake was off).
        assert!(shutdown(&settings, &case(TvPower::Standby, false), Stop::PowerOff).standby.is_empty());
        // Restore is the television only, whatever the target says.
        let wide = CecSettings { power_target: CecPowerTarget::Broadcast, ..settings };
        assert_eq!(shutdown(&wide, &case(TvPower::Standby, true), Stop::PowerOff).standby, [TV]);
        // And not on a reboot.
        assert!(shutdown(&settings, &case(TvPower::Standby, true), Stop::Reboot).standby.is_empty());
    }

    #[test]
    fn an_active_source_that_leaves_the_television_on_says_it_is_leaving() {
        let us = CecSession { active_source: ActiveSource::Us, tv_power_at_start: TvPower::On, ..Default::default() };
        // Measured 2026-10-08 on the Plus: active source on start + restore,
        // TV on at start -> no standby; the TV was left on our input.
        let restore = CecSettings { active_source_on_start: true, restore_power_on_shutdown: true, ..on() };
        assert_eq!(shutdown(&restore, &us, Stop::PowerOff), Shutdown { standby: vec![], inactive_source: true });
        assert!(shutdown(&on(), &us, Stop::PowerOff).inactive_source);
        // Not when the television is being turned off.
        let off = CecSettings { standby_tv_on_shutdown: true, ..on() };
        assert!(!shutdown(&off, &us, Stop::PowerOff).inactive_source);
        let all = CecSettings { power_target: CecPowerTarget::Broadcast, ..off };
        assert!(!shutdown(&all, &us, Stop::PowerOff).inactive_source);
        // Not by a box the bus never saw as the source, not on a reboot,
        // and not with CEC off.
        let unknown = CecSession::default();
        assert!(!shutdown(&on(), &unknown, Stop::PowerOff).inactive_source);
        assert!(!shutdown(&on(), &us, Stop::Reboot).inactive_source);
        assert!(!shutdown(&on(), &us, Stop::Daemon).inactive_source);
        assert!(!shutdown(&on().with(CecChange::Enabled(false)), &us, Stop::PowerOff).inactive_source);
    }

    #[test]
    fn set_stream_path_to_this_box_makes_it_active_and_it_says_so() {
        let mut session = CecSession::default();
        let reactions = receive(&on(), &mut session, US, PA, &msg(0, 15, &[0x86, 0x10, 0x00]));
        assert_eq!(reactions, [Reaction::AnnounceActiveSource]);
        assert_eq!(session.active_source, ActiveSource::Us);
        // Request Active Source is now ours to answer.
        let reactions = receive(&on(), &mut session, US, PA, &msg(0, 15, &[0x85]));
        assert_eq!(reactions, [Reaction::AnnounceActiveSource]);
    }

    #[test]
    fn request_active_source_is_not_answered_by_a_box_that_is_not_it() {
        let mut session = CecSession::default();
        assert!(receive(&on(), &mut session, US, PA, &msg(0, 15, &[0x85])).is_empty());
    }

    #[test]
    fn losing_the_source_stands_by_only_when_asked() {
        let standby = CecSettings { on_active_source_lost: CecSourceLost::Standby, ..on() };
        // Another player announces itself.
        let mut session = CecSession { active_source: ActiveSource::Us, ..Default::default() };
        let reactions = receive(&standby, &mut session, US, PA, &msg(8, 15, &[0x82, 0x20, 0x00]));
        assert_eq!(reactions, [Reaction::SourceLost]);
        assert_eq!(session.active_source, ActiveSource::Other);
        // Again: already lost, nothing more.
        assert!(receive(&standby, &mut session, US, PA, &msg(8, 15, &[0x82, 0x20, 0x00])).is_empty());
        // The default stays on.
        let mut session = CecSession { active_source: ActiveSource::Us, ..Default::default() };
        assert!(receive(&on(), &mut session, US, PA, &msg(8, 15, &[0x82, 0x20, 0x00])).is_empty());
        assert_eq!(session.active_source, ActiveSource::Other);
        // The television routing away is the same loss.
        let mut session = CecSession { active_source: ActiveSource::Us, ..Default::default() };
        let reactions =
            receive(&standby, &mut session, US, PA, &msg(0, 15, &[0x80, 0x10, 0x00, 0x30, 0x00]));
        assert_eq!(reactions, [Reaction::SourceLost]);
    }

    #[test]
    fn routing_is_only_believed_from_the_television() {
        let mut session = CecSession::default();
        assert!(receive(&on(), &mut session, US, PA, &msg(8, 15, &[0x86, 0x10, 0x00])).is_empty());
        assert_eq!(session.active_source, ActiveSource::Unknown);
    }

    #[test]
    fn power_status_is_answered_and_recorded() {
        let mut session = CecSession::default();
        assert_eq!(
            receive(&on(), &mut session, US, PA, &msg(0, US, &[0x8f])),
            [Reaction::ReportPowerOn(0)]
        );
        // Not ours: a question to another device.
        assert!(receive(&on(), &mut session, US, PA, &msg(0, 8, &[0x8f])).is_empty());
        receive(&on(), &mut session, US, PA, &msg(0, US, &[0x90, 0x01]));
        assert_eq!(session.tv_power, TvPower::Standby);
        receive(&on(), &mut session, US, PA, &msg(0, US, &[0x90, 0x00]));
        assert_eq!(session.tv_power, TvPower::On);
    }

    #[test]
    fn a_television_going_to_standby_shows_nobody() {
        let mut session = CecSession { active_source: ActiveSource::Us, ..Default::default() };
        receive(&on(), &mut session, US, PA, &msg(0, 15, &[0x36]));
        assert_eq!(session.tv_power, TvPower::Standby);
        assert_eq!(session.active_source, ActiveSource::Unknown);
    }

    #[test]
    fn inactive_source_of_the_other_device_makes_it_unknown() {
        let mut session = CecSession { active_source: ActiveSource::Other, ..Default::default() };
        receive(&on(), &mut session, US, PA, &msg(8, 0, &[0x9d, 0x20, 0x00]));
        assert_eq!(session.active_source, ActiveSource::Unknown);
    }

    #[test]
    fn truncated_routing_is_ignored() {
        let mut session = CecSession { active_source: ActiveSource::Us, ..Default::default() };
        let standby = CecSettings { on_active_source_lost: CecSourceLost::Standby, ..on() };
        assert!(receive(&standby, &mut session, US, PA, &msg(0, 15, &[0x86, 0x10])).is_empty());
        assert_eq!(session.active_source, ActiveSource::Us);
    }
}
