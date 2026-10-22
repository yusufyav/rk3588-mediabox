//! From reset to FEC lock, and the cleanup that always follows (Kanky vendor
//! order; kankylink/tune.py, verified on the hardware):
//!
//! ```text
//! bridge firmware 1.4.0.0 (loaded first if the device came up COLD)
//!   -> GPIO2 reset -> AVL62X1 boot -> tuner I2C
//!   -> receiver init (ADC, polarity, AGC, MPEG serial, MPEG out on)
//!   -> repeater on, RDA5815M init, repeater off
//!   -> the LNB pins must still be at their reset value (read only)
//!   -> bridge TS init -> HALT, carrier plan, repeater on, tuner lock,
//!      repeater off, clear stream state, LockTP, poll FEC lock
//!   -> FEC locked: the stream runs (the caller's), then one status sample
//! cleanup: MPEG out off, HALT, repeater closed, EP4 TX off, LNB pins
//!          unchanged, GPIO5/14 unchanged, GPIO2 restored (demod back in reset)
//! ```
//!
//! No LNB supply, no 22 kHz tone, no DiSEqC: MediaBox takes the RF as it is
//! prepared outside, and the demodulator's write policy has no scope that
//! could reach those pins (`avl62x1::Scope`). The reference's LNB-armed
//! sessions are replayed with their LNB frames taken out (`tests/replay.rs`).
//!
//! Nothing is retried. Every other read the reference makes is made here, in
//! the same order, including the ones that only feed the report.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use crate::avl62x1::{self, AcqStatus, Avl62x1, BootInfo, Carrier, Scope};
use crate::bridge::{self, Bridge, GPIO2, GPIO5, GPIO14, GpioSnapshot};
use crate::error::{Error, Result};
use crate::firmware::Firmware;
use crate::rda5815m::{self, TunerInit};

pub const LOW_BAND_LO_KHZ: u32 = 9_750_000;
pub const HIGH_BAND_LO_KHZ: u32 = 10_600_000;
pub const BAND_SPLIT_KHZ: u32 = 11_700_000;

/// The L-band IF of a Ku-band transponder behind a universal LNB: the low
/// band's 9.75 GHz oscillator below 11.7 GHz, the high band's 10.6 GHz above.
/// Which band the LNB is in (its 22 kHz tone) and which polarisation (its
/// supply voltage) are the RF source's, prepared outside; this only says
/// where the tuner must look for the transponder once they are.
pub fn universal_if_khz(frequency_khz: u32) -> u32 {
    let high = frequency_khz >= BAND_SPLIT_KHZ;
    frequency_khz - if high { HIGH_BAND_LO_KHZ } else { LOW_BAND_LO_KHZ }
}

#[derive(Debug, Clone, Serialize)]
pub struct TuneParams {
    /// L-band frequency at the tuner input.
    pub rf_khz: u32,
    pub symbol_rate_ksps: u32,
    /// None: the vendor's AVL_FE_LockChannel budget (70/100/150 x 10 ms).
    pub lock_timeout_ms: Option<u32>,
    /// Where else to put the tuner, relative to `rf_khz`, when the carrier is
    /// not found where the frequency plan says, tried in order until one
    /// locks; each try is a retune (HALT, tuner, LockTP), not a reboot.
    /// Empty: the nominal frequency only.
    ///
    /// Measured on a central LNB system (2026-10-22, 12188 V 27500): the
    /// multiplex sat at 1600.7 MHz IF, 12.7 MHz above universal-LO 1588. The
    /// demodulator's own offset search locks within about 7 MHz (1594 MHz:
    /// -6.8 MHz, 431 ms; 1600 MHz: -0.7 MHz, 429-482 ms, 3 of 3) and reached
    /// 12.7 MHz from 1588 only now and then (4.3 s; 5 of 12 reference sessions).
    pub search_offsets_khz: Vec<i32>,
    /// The offset this transponder locked at last time, tried first: on the
    /// system measured above every cold start otherwise spent a second on
    /// the nominal IF that never locks.
    pub first_offset_khz: Option<i32>,
}

impl TuneParams {
    /// The offsets in the order they are tried: the one remembered, the
    /// nominal IF, then the search; none twice.
    pub fn offsets(&self) -> Vec<i32> {
        let mut out: Vec<i32> = Vec::new();
        let order = self.first_offset_khz.into_iter().chain(std::iter::once(0)).chain(self.search_offsets_khz.iter().copied());
        for offset in order {
            if !out.contains(&offset) {
                out.push(offset);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TuneResult {
    pub firmware: Option<String>,
    /// The bridge firmware was loaded in this session (the device was COLD).
    pub firmware_loaded: bool,
    pub boot: Option<BootInfo>,
    pub tuner: Option<TunerInit>,
    pub carrier: Option<Carrier>,
    /// The offset from the nominal IF the carrier was locked at, kHz.
    pub search_offset_khz: Option<i32>,
    pub lock_flags: Option<(u8, u8)>,
    pub lock_polls: Option<u32>,
    pub lock_ms: Option<u64>,
    pub agc_checkpoints: Vec<(String, u16)>,
    pub acq_trace: Vec<AcqStatus>,
    pub rf_verdict: Option<&'static str>,
    pub post_capture: Option<AcqStatus>,
    pub stopped: Option<String>,
    pub error: Option<String>,
    /// The device went away during the session.
    pub no_device: bool,
    /// The LNB pins (enable, select) as the demodulator's reset leaves them,
    /// and whether they were still so at the end. Read, never written.
    pub lnb_pins_at_reset: Option<(u32, u32)>,
    pub lnb_pins_untouched: Option<bool>,
    pub repeater_closed: Option<bool>,
    /// None: the TS output was never switched on.
    pub ts_ep4_off: Option<bool>,
    pub untouched_ok: Option<bool>,
    pub gpio_restored: Option<bool>,
    pub cleanup_done: bool,
}

impl TuneResult {
    pub fn fec_locked(&self) -> bool {
        self.lock_flags.is_some_and(|(_, fec)| fec != 0)
    }

    /// Everything the cleanup was meant to do, done.
    pub fn cleanup_ok(&self) -> bool {
        self.lnb_pins_untouched != Some(false)
            && self.repeater_closed != Some(false)
            && self.ts_ep4_off != Some(false)
            && self.untouched_ok == Some(true)
            && (self.gpio_restored == Some(true) || self.firmware.is_none())
    }
}

/// What the stream (the caller's) reports back: whether it ended because
/// the device went away, or wants another transponder -- tuned with the
/// demodulator as it is, booted, which is what makes a receiver that stays
/// up worth having (the boot is 3.5 s of patch upload).
pub struct Streamed {
    pub no_device: bool,
    pub retune: Option<TuneParams>,
}

fn checkpoint(bridge: &mut Bridge, demod: &Avl62x1, res: &mut TuneResult, name: &str) -> Result<()> {
    let gain = demod.rf_agc_gain(bridge)?;
    bridge.log.line(format!("AGC {name}: rfagc_gain={gain} ({} %)", avl62x1::rf_strength_pct(gain)));
    res.agc_checkpoints.push((name.to_string(), gain));
    Ok(())
}

fn with_repeater<T>(
    bridge: &mut Bridge,
    demod: &mut Avl62x1,
    action: impl FnOnce(&mut Bridge) -> Result<T>,
) -> Result<T> {
    demod.tuner_repeater(bridge, true)?;
    let result = action(bridge);
    let closed = demod.tuner_repeater(bridge, false);
    let value = result?;
    closed?;
    Ok(value)
}

/// One whole session: bring the receiver up, lock, run `stream` while
/// locked, and clean up whatever happened. `stop` ends it early at the next
/// step boundary (and the stream itself, which watches it too).
pub fn run(
    bridge: &mut Bridge,
    firmware: &Firmware,
    params: &TuneParams,
    stop: &AtomicBool,
    stream: impl FnMut(&mut Bridge, &mut Avl62x1, &TuneResult) -> Streamed,
) -> TuneResult {
    let mut res = TuneResult::default();
    if let Err(error) = run_inner(bridge, firmware, params, stop, stream, &mut res) {
        res.no_device |= error.is_no_device();
        bridge.log.line(format!("UNEXPECTED (cleanup attempted, nothing retried): {error}"));
        res.error.get_or_insert(error.to_string());
    }
    res
}

fn run_inner(
    bridge: &mut Bridge,
    firmware: &Firmware,
    params: &TuneParams,
    stop: &AtomicBool,
    stream: impl FnMut(&mut Bridge, &mut Avl62x1, &TuneResult) -> Streamed,
    res: &mut TuneResult,
) -> Result<()> {
    bridge.allow_writes(GPIO2.regs());
    bridge.allow_writes(bridge::ts_init_regs());
    let mut version = bridge.firmware_version()?;
    bridge.log.line(format!("Bridge firmware: {}", bridge::dotted(&version)));
    if version == [0; 4] {
        // COLD: every plug-in. The firmware is the owner's file, verified.
        let Some(blocks) = firmware.bridge_blocks.as_ref() else {
            res.stopped = Some(format!("bridge firmware COLD and {} is missing", crate::firmware::BRIDGE_FIRMWARE));
            return Ok(());
        };
        version = bridge::load_firmware(bridge, blocks)?;
        res.firmware_loaded = true;
    }
    res.firmware = Some(bridge::dotted(&version));
    if version != bridge::VERIFIED_FIRMWARE {
        res.stopped = Some("bridge firmware is not the verified 1.4.0.0".into());
        bridge.log.line("STOP: bridge firmware is not the verified 1.4.0.0; hiçbir yazma yapılmadı");
        return Ok(());
    }
    let untouched_before = [bridge::snapshot(bridge, GPIO5)?, bridge::snapshot(bridge, GPIO14)?];
    let mut demod = Avl62x1::new([Scope::Agc]);
    let guard = bridge::snapshot(bridge, GPIO2)?;

    let staged = stages(bridge, &mut demod, firmware, params, stop, stream, res);
    cleanup(bridge, &mut demod, res, &untouched_before);
    res.gpio_restored = Some(bridge::restore(bridge, &guard));
    res.cleanup_done = true;
    staged
}

/// Every register read again, then compared: all six are read whatever the
/// first one says, as the reference does.
fn same_as(bridge: &mut Bridge, before: &[GpioSnapshot; 2]) -> Result<bool> {
    let mut same = true;
    for snapshot in before {
        for (address, value) in snapshot.initial {
            same &= bridge.read_reg(address)? == value;
        }
    }
    Ok(same)
}

fn stopping(stop: &AtomicBool, res: &mut TuneResult, bridge: &Bridge) -> bool {
    if stop.load(Ordering::SeqCst) {
        res.stopped = Some("stop requested".into());
        bridge.log.line("STOP requested before streaming; cleanup runs");
        return true;
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn stages(
    bridge: &mut Bridge,
    demod: &mut Avl62x1,
    firmware: &Firmware,
    params: &TuneParams,
    stop: &AtomicBool,
    mut stream: impl FnMut(&mut Bridge, &mut Avl62x1, &TuneResult) -> Streamed,
    res: &mut TuneResult,
) -> Result<()> {
    bridge::demod_reset_pulse(bridge)?;
    if demod.chip_id(bridge)? != avl62x1::CHIP_ID {
        res.stopped = Some("unexpected chip id".into());
        return Ok(());
    }
    let lnb_reset_state = demod.lnb_pins(bridge)?;
    res.lnb_pins_at_reset = Some(lnb_reset_state);
    bridge.log.line(format!(
        "LNB pins after the demod reset: enable=0x{:X} select=0x{:X} (read only; 2 = high-Z)",
        lnb_reset_state.0, lnb_reset_state.1
    ));
    let boot = match demod.boot(bridge, &firmware.patch) {
        Ok(boot) => boot,
        Err(error @ (Error::Demod(_) | Error::Patch(_) | Error::Device { .. })) => {
            res.stopped = Some(format!("demod boot failed: {error}"));
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let sys_clk_hz = boot.sys_clk_hz;
    res.boot = Some(boot);
    if stopping(stop, res, bridge) {
        return Ok(());
    }
    demod.init_tuner_i2c(bridge, sys_clk_hz)?;
    demod.init_receiver(bridge)?;
    bridge.log.line("DEMOD receiver init done (ADC, polarity, AGC, MPEG serial, MPEG out on)");
    checkpoint(bridge, demod, res, "C0 receiver init, tuner not initialised")?;

    let diag = demod.tuner_i2c_diag(bridge)?;
    bridge.log.line(format!("TUNER I2C diag before init: {diag:x?}"));
    res.tuner = Some(with_repeater(bridge, demod, |bridge| rda5815m::init(bridge, &firmware.tables))?);
    let diag = demod.tuner_i2c_diag(bridge)?;
    bridge.log.line(format!("TUNER I2C diag after init: {diag:x?}"));
    checkpoint(bridge, demod, res, "C1 tuner initialised")?;
    if stopping(stop, res, bridge) {
        return Ok(());
    }

    if demod.lnb_pins(bridge)? != lnb_reset_state {
        res.lnb_pins_untouched = Some(false);
        res.stopped = Some("LNB pins changed although nothing here writes them".into());
        return Ok(());
    }

    res.ts_ep4_off = Some(false); // from here on cleanup must switch EP4 TX off
    bridge::ts_init(bridge)?;

    let mut params = params.clone();
    loop {
        lock_carrier(bridge, demod, &params, stop, res)?;
        if !res.fec_locked() {
            if res.stopped.is_none() {
                res.stopped = Some("no FEC lock".into());
            }
            return Ok(());
        }
        // The stream is told how it was tuned as it starts: what the control
        // plane learns from a tune (where the carrier locked) is known while
        // the channel plays, not only once the receiver has gone.
        let streamed = stream(bridge, demod, res);
        res.no_device |= streamed.no_device;
        if streamed.no_device {
            return Ok(()); // nothing left to ask; cleanup still tries
        }
        let Some(next) = streamed.retune else { break };
        bridge.log.line(format!("RETUNE {} kHz SR {} kS/s, the demodulator as it is", next.rf_khz, next.symbol_rate_ksps));
        res.lock_flags = None;
        res.lock_polls = None;
        res.lock_ms = None;
        res.acq_trace.clear();
        res.rf_verdict = None;
        params = next;
    }
    // Read-only, for the record (lock_status / SNR after the stream settled).
    match demod.acq_status(bridge) {
        Ok(status) => {
            bridge.log.line(format!("ACQ post-capture sample {status:?}"));
            res.post_capture = Some(status);
        }
        Err(error) => bridge.log.line(format!("ACQ post-capture sample failed: {error}")),
    }
    Ok(())
}

/// HALT, then the remembered offset, the nominal IF and the search until one
/// locks (`TuneParams::offsets`); each try a retune, not a reboot.
fn lock_carrier(bridge: &mut Bridge, demod: &mut Avl62x1, params: &TuneParams, stop: &AtomicBool, res: &mut TuneResult) -> Result<()> {
    if !demod.halt(bridge)? {
        res.stopped = Some("HALT failed".into());
        return Ok(());
    }
    let offsets = params.offsets();
    for (attempt, &offset_khz) in offsets.iter().enumerate() {
        let rf_khz = (params.rf_khz as i64 + offset_khz as i64) as u32;
        if attempt > 0 {
            bridge.log.line(format!("CARRIER SEARCH: no lock at the last IF; retuning {offset_khz:+} kHz from nominal"));
            if !demod.halt(bridge)? {
                res.stopped = Some("HALT failed".into());
                return Ok(());
            }
        }
        let carrier = avl62x1::optimize_carrier(rf_khz, params.symbol_rate_ksps * 1000);
        bridge.log.line(format!("CARRIER {rf_khz} kHz SR {} kS/s -> {carrier:?}", params.symbol_rate_ksps));
        res.carrier = Some(carrier);
        res.search_offset_khz = Some(offset_khz);
        with_repeater(bridge, demod, |bridge| rda5815m::lock(bridge, carrier.tuner_rf_hz, carrier.lpf_hz))?;
        checkpoint(bridge, demod, res, "C3 tuner locked to carrier, before ACQUIRE")?;
        demod.clear_stream_state(bridge)?;
        if !demod.lock_tp(bridge, params.symbol_rate_ksps * 1000, carrier.carrier_offset_hz)? {
            res.stopped = Some("ACQUIRE command failed".into());
            return Ok(());
        }
        acquire(bridge, demod, params, stop, res)?;
        if res.fec_locked() || res.stopped.is_some() {
            break;
        }
    }
    Ok(())
}

/// Poll as AVL_FE_LockChannel (10 ms steps, fec_lock); every 10th poll and
/// at the end take a read-only sample.
fn acquire(bridge: &mut Bridge, demod: &Avl62x1, params: &TuneParams, stop: &AtomicBool, res: &mut TuneResult) -> Result<()> {
    let budget = match params.lock_timeout_ms {
        None => avl62x1::fec_poll_budget(params.symbol_rate_ksps),
        Some(ms) => ms / 10,
    };
    bridge.log.line(format!(
        "ACQ polling up to {budget} x 10 ms ({})",
        if params.lock_timeout_ms.is_none() { "vendor budget" } else { "requested" }
    ));
    let start = bridge.clock.now();
    let elapsed = |bridge: &Bridge| (bridge.clock.now() - start).as_millis() as u64;
    let mut last = None;
    let mut poll = 0;
    for n in 1..=budget {
        poll = n;
        if stop.load(Ordering::SeqCst) {
            res.stopped = Some("stop requested".into());
            break;
        }
        bridge.delay(10);
        let flags = demod.lock_flags(bridge)?;
        res.lock_flags = Some(flags);
        if last != Some(flags) {
            bridge.log.line(format!("ACQ t={} ms poll {n}: lock_status={} fec_lock={}", elapsed(bridge), flags.0, flags.1));
            last = Some(flags);
        }
        if flags.1 != 0 || n % 10 == 0 || n == budget {
            let status = demod.acq_status(bridge)?;
            bridge.log.line(format!("ACQ sample poll {n} t={} ms {status:?}", elapsed(bridge)));
            res.acq_trace.push(status);
        }
        if flags.1 != 0 {
            break;
        }
    }
    res.lock_polls = Some(poll);
    res.lock_ms = Some(elapsed(bridge));
    res.rf_verdict = Some(avl62x1::classify_acquisition(&res.acq_trace));
    let flags = res.lock_flags.unwrap_or((0, 0));
    bridge.log.line(format!(
        "LOCK lock_status={} fec_lock={} after {poll} polls, {} ms; RF verdict {}",
        flags.0,
        flags.1,
        elapsed(bridge),
        res.rf_verdict.unwrap_or("?")
    ));
    Ok(())
}

fn cleanup(bridge: &mut Bridge, demod: &mut Avl62x1, res: &mut TuneResult, untouched: &[GpioSnapshot; 2]) {
    // Before anything is switched off: still at reset, as nothing wrote them.
    if let Some(at_reset) = res.lnb_pins_at_reset {
        match demod.lnb_pins(bridge) {
            Ok(now) => {
                let same = now == at_reset;
                res.lnb_pins_untouched = Some(same && res.lnb_pins_untouched != Some(false));
                bridge.log.line(format!("LNB pins unchanged: {same} (enable=0x{:X} select=0x{:X})", now.0, now.1));
            }
            Err(error) => bridge.log.line(format!("LNB pin check failed: {error}")),
        }
    }
    if res.boot.is_some() {
        if let Err(error) = demod.drive_mpeg_output(bridge, false) {
            bridge.log.line(format!("MPEG output off failed: {error}"));
        }
        if let Err(error) = demod.halt(bridge) {
            bridge.log.line(format!("demod HALT failed: {error}"));
        }
    }
    if demod.repeater_maybe_on {
        match demod.tuner_repeater(bridge, false) {
            Ok(()) => res.repeater_closed = Some(true),
            Err(error) => {
                res.repeater_closed = Some(false);
                bridge.log.line(format!("DEMOD tuner repeater close failed: {error}"));
            }
        }
    }
    if res.ts_ep4_off.is_some() {
        match bridge::ts_ep4_disable(bridge) {
            Ok(()) => res.ts_ep4_off = Some(true),
            Err(error) => bridge.log.line(format!("EP4 TX off failed: {error}")),
        }
    }
    match same_as(bridge, untouched) {
        Ok(same) => {
            res.untouched_ok = Some(same);
            bridge.log.line(format!("GPIO5/GPIO14 unchanged: {same}"));
        }
        Err(error) => bridge.log.line(format!("GPIO5/GPIO14 check failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offset_that_locked_last_time_is_tried_first() {
        let params = |first| TuneParams {
            rf_khz: 1_588_000,
            symbol_rate_ksps: 27_500,
            lock_timeout_ms: None,
            search_offsets_khz: vec![10_000, -10_000],
            first_offset_khz: first,
        };
        assert_eq!(params(None).offsets(), [0, 10_000, -10_000]);
        assert_eq!(params(Some(10_000)).offsets(), [10_000, 0, -10_000]);
        assert_eq!(params(Some(0)).offsets(), [0, 10_000, -10_000]);
    }
}

