//! The Rust receiver against real sessions of the hardware, frame for frame.
//!
//! The reference implementation logs every bridge command it sends and every
//! reply it gets (`  USB TX:` / `  USB RX:` in kanky-link-linux/logs/
//! tune-hw-*.log). This test replays such a log: each command the Rust stack
//! sends must be the next one the log recorded, byte for byte, and it is
//! answered with the reply the hardware gave. Every decision the stack takes
//! (how long to poll, which table, lock or no lock) is driven by those
//! replies.
//!
//! Those sessions powered the LNB; MediaBox does not. Their LNB frames -- the
//! writes to the supply pins (0x120008/0x12000C), everything in the DiSEqC
//! block (0x16C000..0x16C0FF), and the gain reading taken right after the
//! tone came on -- are taken out of the expected sequence, and the stack's
//! own read-only checks of the LNB pins are answered with the values the log
//! read after the demodulator's reset. The sequence numbers move with the
//! frames taken out, so a frame is compared without its sequence number and
//! checksum; a reply gets the request's. Any write the stack sends to the LNB
//! pins or the DiSEqC block fails the test.
//!
//! The logs and the vendor library are the owner's files, never in this
//! repository; without them the test says so and passes:
//!
//!   MEDIABOX_DVB_REPLAY_LOGS=<dir with tune-hw-lnb-*.log>
//!   MEDIABOX_DVB_FIRMWARE_DIR=<dir with libdvb_usb_phy.so>

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use mediabox_dvb::bridge::{Bridge, checksum};
use mediabox_dvb::firmware::Firmware;
use mediabox_dvb::log::Log;
use mediabox_dvb::tune::{self, Streamed, TuneParams};
use mediabox_dvb::usb::{Transport, UsbError};
use mediabox_dvb::Clock;

const DEMOD: u8 = 0x14 << 1;
const LNB_PINS: [u32; 2] = [0x12000C, 0x120008];
const CMD_I2C_RD: u8 = 0x2A;
const CMD_I2C_WR: u8 = 0x2B;

fn unhex(text: &str) -> Vec<u8> {
    text.split_whitespace().map(|byte| u8::from_str_radix(byte, 16).expect("hex byte")).collect()
}

/// (command, payload) of a TX frame: what is compared.
fn body(frame: &[u8]) -> (u8, &[u8]) {
    (frame[2], &frame[4..frame.len() - 2])
}

/// The demod address a generic-I2C write to the AVL62X1 carries, and whether
/// it writes data (more than the 3 address bytes).
fn demod_target(frame: &[u8]) -> Option<(u32, bool)> {
    let (cmd, payload) = body(frame);
    if cmd != CMD_I2C_WR || payload.len() < 6 || payload[2] != DEMOD {
        return None;
    }
    let address = u32::from_be_bytes([0, payload[3], payload[4], payload[5]]);
    Some((address, payload[0] > 3))
}

fn is_lnb(address: u32) -> bool {
    LNB_PINS.contains(&address) || (0x16C000..=0x16C0FF).contains(&address)
}

fn reply(mut rx: Vec<u8>, seq: u8) -> Vec<u8> {
    rx[1] = seq;
    let n = rx.len();
    let sum = checksum(&rx[..n - 2]);
    rx[n - 2..].copy_from_slice(&sum.to_be_bytes());
    rx
}

struct Replay {
    frames: RefCell<VecDeque<(Vec<u8>, Vec<u8>)>>,
    pending: RefCell<Option<Vec<u8>>>,
    /// The LNB pins as the log read them after the demod reset.
    lnb_at_reset: HashMap<u32, Vec<u8>>,
    /// A read-only LNB pin check the log does not have: its read is answered here.
    synthetic_read: RefCell<Option<u32>>,
    sent: Cell<usize>,
    removed: usize,
    synthesised: Cell<usize>,
    lnb_writes: Cell<usize>,
    mismatch: RefCell<Option<String>>,
}

impl Replay {
    fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("log");
        let mut pairs = Vec::new();
        let mut tx = None;
        for line in text.lines() {
            if let Some((_, frame)) = line.split_once("  USB TX: ") {
                assert!(tx.is_none(), "two TX without RX in {}", path.display());
                tx = Some(unhex(frame));
            } else if let Some((_, frame)) = line.split_once("  USB RX: ") {
                pairs.push((tx.take().expect("RX without TX"), unhex(frame)));
            }
        }
        // The LNB pins read after the reset: an address-only write, then a read.
        let mut lnb_at_reset = HashMap::new();
        for window in pairs.windows(2) {
            if let Some((address, false)) = demod_target(&window[0].0)
                && LNB_PINS.contains(&address)
                && !lnb_at_reset.contains_key(&address)
            {
                lnb_at_reset.insert(address, window[1].1.clone());
            }
        }
        // Take the LNB frames out.
        let mut frames = VecDeque::new();
        let mut removed = 0;
        let mut i = 0;
        let mut after_lnb = false;
        while i < pairs.len() {
            let target = demod_target(&pairs[i].0);
            let lnb_write = target.is_some_and(|(address, writes)| writes && is_lnb(address));
            let diseqc_read = target.is_some_and(|(address, writes)| !writes && (0x16C000..=0x16C0FF).contains(&address));
            let gain_after_tone = after_lnb && target == Some((0x0000E8, false));
            if lnb_write {
                removed += 1;
                i += 1;
                after_lnb = true;
                continue;
            }
            if diseqc_read || gain_after_tone {
                removed += 2; // the address write and its read
                i += 2;
                after_lnb = diseqc_read;
                continue;
            }
            after_lnb = false;
            frames.push_back(pairs[i].clone());
            i += 1;
        }
        Self {
            frames: RefCell::new(frames),
            pending: RefCell::new(None),
            lnb_at_reset,
            synthetic_read: RefCell::new(None),
            sent: Cell::new(0),
            removed,
            synthesised: Cell::new(0),
            lnb_writes: Cell::new(0),
            mismatch: RefCell::new(None),
        }
    }

    fn fail(&self, what: String) -> UsbError {
        self.mismatch.borrow_mut().get_or_insert(what);
        UsbError { code: -1, name: "replay mismatch".into(), during: "replay" }
    }

    fn ack(seq: u8, data: &[u8]) -> Vec<u8> {
        let mut rx = vec![(data.len() + 4) as u8, seq, 0];
        rx.extend_from_slice(data);
        rx.extend_from_slice(&[0, 0]);
        reply(rx, seq)
    }
}

impl Transport for Replay {
    fn write(&self, endpoint: u8, data: &[u8], _timeout_ms: u32) -> Result<usize, UsbError> {
        let index = self.sent.get();
        let seq = data[3];
        if let Some((address, writes)) = demod_target(data)
            && is_lnb(address)
        {
            if writes {
                self.lnb_writes.set(self.lnb_writes.get() + 1);
                return Err(self.fail(format!("frame {index}: the stack WROTE 0x{address:06X} (LNB / DiSEqC)")));
            }
            let next_is_same = self.frames.borrow().front().is_some_and(|(tx, _)| body(tx) == body(data));
            if !next_is_same && LNB_PINS.contains(&address) {
                // A read-only pin check the reference did not make here.
                *self.synthetic_read.borrow_mut() = Some(address);
                *self.pending.borrow_mut() = Some(Self::ack(seq, &[]));
                self.synthesised.set(self.synthesised.get() + 1);
                return Ok(data.len());
            }
        }
        if let Some(address) = self.synthetic_read.borrow_mut().take() {
            let (cmd, payload) = body(data);
            if cmd != CMD_I2C_RD || payload.get(2) != Some(&DEMOD) {
                return Err(self.fail(format!("frame {index}: expected the read of 0x{address:06X}, sent {data:02x?}")));
            }
            let recorded = &self.lnb_at_reset[&address];
            *self.pending.borrow_mut() = Some(reply(recorded.clone(), seq));
            return Ok(data.len());
        }
        let Some((expected, rx)) = self.frames.borrow_mut().pop_front() else {
            return Err(self.fail(format!("frame {index}: the log has ended, the stack sent {data:02x?}")));
        };
        if endpoint != 0x02 || body(data) != body(&expected) {
            return Err(self.fail(format!("frame {index}: expected {expected:02x?}, sent {data:02x?} on 0x{endpoint:02x}")));
        }
        self.sent.set(index + 1);
        *self.pending.borrow_mut() = Some(reply(rx, seq));
        Ok(data.len())
    }

    fn read(&self, endpoint: u8, _length: usize, _timeout_ms: u32) -> Result<Vec<u8>, UsbError> {
        if endpoint != 0x81 {
            return Err(self.fail(format!("read on 0x{endpoint:02x}")));
        }
        self.pending.borrow_mut().take().ok_or_else(|| self.fail("read without a write".into()))
    }
}

/// Virtual time: a delay moves the clock, nothing waits.
#[derive(Default)]
struct Virtual(Cell<u64>);

impl Clock for Virtual {
    fn delay_ms(&self, ms: u64) {
        self.0.set(self.0.get() + ms);
    }
    fn now(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0.get())
    }
}

/// The session's parameters, from its own first line (TuneParams(...)).
fn params(path: &Path) -> Option<TuneParams> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().find(|line| line.contains("params=TuneParams("))?;
    let field = |name: &str| -> Option<&str> {
        let start = line.find(&format!("{name}="))? + name.len() + 1;
        let rest = &line[start..];
        Some(rest[..rest.find([',', ')']).unwrap_or(rest.len())].trim())
    };
    // Only sessions of the code this crate ports: the 5 s lock budget, no
    // GPIO14 opt-in, no I2C ACK probe.
    if field("gpio14")? != "False" || field("i2c_ack_probe")? != "False" {
        return None;
    }
    Some(TuneParams {
        rf_khz: field("rf_khz")?.parse().ok()?,
        symbol_rate_ksps: field("symbol_rate_ksps")?.parse().ok()?,
        lnb: None,
        lock_timeout_ms: field("lock_timeout_ms")?.parse().ok(),
        search_offsets_khz: Vec::new(),
        first_offset_khz: None,
    })
}

fn logs() -> Option<(Vec<PathBuf>, PathBuf)> {
    let dir = std::env::var_os("MEDIABOX_DVB_REPLAY_LOGS")?;
    let firmware = std::env::var_os("MEDIABOX_DVB_FIRMWARE_DIR")?;
    let mut logs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("tune-hw-lnb-") && n.ends_with(".log")))
        .collect();
    logs.sort();
    Some((logs, PathBuf::from(firmware)))
}

#[test]
fn every_recorded_hardware_session_is_replayed_frame_for_frame() {
    let Some((logs, firmware_dir)) = logs() else {
        eprintln!("replay: MEDIABOX_DVB_REPLAY_LOGS / MEDIABOX_DVB_FIRMWARE_DIR not set; nothing to replay");
        return;
    };
    let firmware = Firmware::load(&firmware_dir).expect("firmware");
    let mut replayed = 0;
    for path in &logs {
        let Some(params) = params(path) else { continue };
        let text = std::fs::read_to_string(path).unwrap();
        // The reference took its post-capture status sample from 2a7ef8a
        // (2026-10-22 00:09) on; a locked session logged before that is an
        // older protocol than the one ported here.
        if text.contains("RESULT: SUCCESS") && !text.contains("ACQ post-capture") {
            eprintln!("replay {}: skipped (reference before the post-capture sample)", path.display());
            continue;
        }
        let replay = Replay::load(path);
        let total = replay.frames.borrow().len();
        let removed = replay.removed;
        let log = Log::silent();
        let clock = Virtual::default();
        let mut bridge = Bridge::new(&replay, &log, &clock, []);
        let stop = AtomicBool::new(false);
        let mut streamed = false;
        let res = tune::run(&mut bridge, &firmware, &params, &stop, |_, _, _| {
            streamed = true;
            Streamed { no_device: false, retune: None }
        });
        let name = path.file_name().unwrap().to_string_lossy();
        if let Some(mismatch) = replay.mismatch.borrow().as_ref() {
            panic!("{name}: {mismatch}");
        }
        assert_eq!(replay.lnb_writes.get(), 0, "{name}: LNB / DiSEqC writes");
        assert_eq!(replay.sent.get(), total, "{name}: {} of {total} frames sent", replay.sent.get());
        assert_eq!(res.lnb_pins_untouched, Some(true), "{name}: LNB pins");
        assert!(res.error.is_none(), "{name}: {:?}", res.error);
        assert!(res.cleanup_done && res.cleanup_ok(), "{name}: cleanup {res:?}");
        assert_eq!(res.gpio_restored, Some(true), "{name}");
        assert_eq!(res.fec_locked(), streamed, "{name}: streamed exactly when locked");
        let verdict = std::fs::read_to_string(path).unwrap();
        let reference_locked = verdict.contains("RESULT: SUCCESS");
        assert_eq!(res.fec_locked(), reference_locked, "{name}: lock verdict differs from the reference");
        eprintln!(
            "replay {name}: {total} frames compared, {removed} LNB frames taken out, {} pin reads answered, lock={} polls={:?} verdict={:?} lnb_pins_at_reset={:x?}",
            replay.synthesised.get(),
            res.fec_locked(),
            res.lock_polls,
            res.rf_verdict,
            res.lnb_pins_at_reset
        );
        replayed += 1;
    }
    assert!(replayed > 0, "no replayable session in the logs");
}
