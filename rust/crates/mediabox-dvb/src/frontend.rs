//! The demodulator's own state, sampled read-only on a thread of its own for
//! the trace (`trace`, kind 20): `MEDIABOX_DVB_RF_EVERY_MS`, off unless set
//! and the trace is on.
//!
//! - One demodulator transaction at a time (`transaction`): a register read
//!   is an address written then bytes read, two bridge commands; another
//!   thread's command between them would read another register. Every
//!   `Avl62x1` read and write takes it; the time a caller waited is kept
//!   per thread (`take_waited`) for the trace.
//! - Read-only: the lock (0x408), the FEC lock (0x40A), the SNR (0x412,
//!   dB x 100), the RF AGC gain (0x0E8) and the acquisition retries (0x42B),
//!   the registers the stream already reads. No register is written.
//! - Not a Linux DVB frontend: FE_HAS_SIGNAL/CARRIER/VITERBI/SYNC, TIMEDOUT,
//!   REINIT, pre/post-FEC bit counts, block counts and LDPC/BCH counters are
//!   not read and are marked `UNSUPPORTED`, never written as zero.
//! - A sample starts only after the one before ended (one thread, in turn);
//!   one over its budget stops between registers and says so. A register
//!   read is bounded by the bridge's command timeout (`bridge::TIMEOUT_MS`,
//!   twice: the address, then the bytes).

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

static TRANSACTION: Mutex<()> = Mutex::new(());

thread_local! {
    static WAITED: Cell<u64> = const { Cell::new(0) };
}

/// The demodulator to this thread until the guard goes.
pub fn transaction() -> MutexGuard<'static, ()> {
    if let Ok(guard) = TRANSACTION.try_lock() {
        return guard;
    }
    let started = Instant::now();
    let guard = TRANSACTION.lock().unwrap_or_else(|p| p.into_inner());
    WAITED.with(|w| w.set(w.get() + started.elapsed().as_nanos() as u64));
    guard
}

/// The ns this thread waited for `transaction` since last asked.
pub fn take_waited() -> u64 {
    WAITED.with(|w| w.replace(0))
}

pub const FE_HAS_SIGNAL: u16 = 1 << 0;
pub const FE_HAS_CARRIER: u16 = 1 << 1;
pub const FE_HAS_VITERBI: u16 = 1 << 2;
pub const FE_HAS_SYNC: u16 = 1 << 3;
pub const FE_TIMEDOUT: u16 = 1 << 4;
pub const FE_REINIT: u16 = 1 << 5;
pub const PRE_FEC_BITS: u16 = 1 << 6;
pub const POST_FEC_BITS: u16 = 1 << 7;
pub const ERROR_BLOCKS: u16 = 1 << 8;
pub const LDPC_BCH: u16 = 1 << 9;
/// What this demodulator is not asked for (see the module's notes).
pub const UNSUPPORTED: u16 = FE_HAS_SIGNAL
    | FE_HAS_CARRIER
    | FE_HAS_VITERBI
    | FE_HAS_SYNC
    | FE_TIMEDOUT
    | FE_REINIT
    | PRE_FEC_BITS
    | POST_FEC_BITS
    | ERROR_BLOCKS
    | LDPC_BCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Lock,
    Fec,
    Snr,
    Agc,
    AcqRetries,
}

pub const FIELDS: [Field; 5] = [Field::Lock, Field::Fec, Field::Snr, Field::Agc, Field::AcqRetries];

pub trait Probe {
    fn read(&mut self, field: Field) -> Result<u32, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Status {
    Ok = 0,
    Error = 1,
    OverBudget = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub status: Status,
    /// A value per field of `FIELDS`, None for one not read.
    pub values: [Option<u32>; 5],
    pub start_raw: u64,
    pub end_raw: u64,
    pub number: u64,
}

/// One sample, the fields in turn, stopping past `budget`.
pub fn sample(probe: &mut dyn Probe, budget: Duration, number: u64) -> Outcome {
    let started = Instant::now();
    let start_raw = mediabox_core::bintrace::raw_now();
    let mut values = [None; 5];
    let mut status = Status::Ok;
    for (i, field) in FIELDS.iter().enumerate() {
        if started.elapsed() > budget {
            status = Status::OverBudget;
            break;
        }
        match probe.read(*field) {
            Ok(v) => values[i] = Some(v),
            Err(_) => {
                status = Status::Error;
                break;
            }
        }
    }
    if status == Status::Ok && started.elapsed() > budget {
        status = Status::OverBudget;
    }
    Outcome { status, values, start_raw, end_raw: mediabox_core::bintrace::raw_now(), number }
}

/// Sample every `every` until `stop`; a late sample is not made up for.
pub fn run(probe: &mut dyn Probe, every: Duration, budget: Duration, stop: &AtomicBool, emit: &mut dyn FnMut(&Outcome)) {
    let mut number = 0;
    let mut next = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        let out = sample(probe, budget, number);
        number += 1;
        emit(&out);
        next += every;
        let now = Instant::now();
        if next < now {
            next = now + every;
        }
        while !stop.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now >= next {
                break;
            }
            std::thread::sleep((next - now).min(Duration::from_millis(20)));
        }
    }
}

/// The sampling thread; stopped and joined when dropped.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    pub fn spawn<P, F>(make: F, every: Duration, budget: Duration, mut emit: impl FnMut(&Outcome) + Send + 'static) -> Option<Self>
    where
        F: FnOnce(&mut dyn FnMut(&mut dyn Probe)) + Send + 'static,
        P: Probe,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("rf-sampler".into())
            .spawn(move || {
                lower_priority();
                make(&mut |probe: &mut dyn Probe| run(probe, every, budget, &flag, &mut emit));
            })
            .ok()?;
        Some(Self { stop, handle: Some(handle) })
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Nice 10 for this thread: the stream's threads come first.
fn lower_priority() {
    // SAFETY: gettid and setpriority on this thread only.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
}

/// The real demodulator behind its own bridge handle on the session.
pub struct Avl<'a> {
    pub bridge: crate::bridge::Bridge<'a>,
    pub demod: crate::avl62x1::Avl62x1,
}

impl Probe for Avl<'_> {
    fn read(&mut self, field: Field) -> Result<u32, String> {
        let r = match field {
            Field::Lock => self.demod.lock_flags(&mut self.bridge).map(|(lock, _)| u32::from(lock)),
            Field::Fec => self.demod.lock_flags(&mut self.bridge).map(|(_, fec)| u32::from(fec)),
            Field::Snr => self.demod.snr_x100(&mut self.bridge).map(|s| u32::from(s as u16)),
            Field::Agc => self.demod.rf_agc_gain(&mut self.bridge).map(u32::from),
            Field::AcqRetries => self.demod.acq_retries(&mut self.bridge).map(u32::from),
        };
        r.map_err(|e| e.to_string())
    }
}

/// `MEDIABOX_DVB_RF_EVERY_MS`, with the trace on.
pub fn every_from_env() -> Option<Duration> {
    crate::trace::get()?;
    let ms: u64 = std::env::var("MEDIABOX_DVB_RF_EVERY_MS").ok()?.parse().ok()?;
    (ms >= 20).then(|| Duration::from_millis(ms))
}

/// Start sampling the session's demodulator into the trace.
pub fn start(session: Arc<crate::usb::Session>, every: Duration) -> Option<Sampler> {
    let trace = crate::trace::get()?;
    trace.now(40, 0, 0, every.as_nanos() as u64, 0, 0);
    // Half the interval, at most 200 ms: past it a sample stops.
    let budget = (every / 2).min(Duration::from_millis(200));
    Sampler::spawn::<Avl, _>(
        move |run: &mut dyn FnMut(&mut dyn Probe)| {
            let log = crate::log::Log::silent();
            let clock = crate::RealClock::default();
            let bridge = crate::bridge::Bridge::new(&*session, &log, &clock, []);
            let mut probe = Avl { bridge, demod: crate::avl62x1::Avl62x1::new([]) };
            run(&mut probe);
        },
        every,
        budget,
        move |out: &Outcome| {
            let v = |i: usize| out.values[i];
            let n = v(0).map_or(0xFF, |x| x & 0xFF)
                | (v(1).map_or(0xFF, |x| x & 0xFF) << 8)
                | (v(4).map_or(0xFF, |x| x & 0xFF) << 16)
                | (u32::from(v(0).is_some()) << 24)
                | (u32::from(v(1).is_some()) << 25)
                | (u32::from(v(2).is_some()) << 26)
                | (u32::from(v(3).is_some()) << 27)
                | (u32::from(v(4).is_some()) << 28);
            let b = u64::from(v(2).unwrap_or(0) & 0xFFFF) | (u64::from(v(3).unwrap_or(0) & 0xFFFF) << 16);
            let c = (u64::from(UNSUPPORTED) << 48) | (out.number & 0xFFFF_FFFF_FFFF);
            let (mono, raw) = mediabox_core::bintrace::clocks();
            trace.put(mediabox_core::bintrace::Record { kind: 20, id: out.status as u16, n, seq: 0, mono, raw, a: out.start_raw, b, c });
            let _ = out.end_raw;
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{Bridge, CMD_GENERIC_I2C_RD, CMD_GENERIC_I2C_WR, checksum};
    use crate::usb::{Transport, UsbError};
    use std::collections::HashMap;
    use std::sync::atomic::AtomicI32;

    struct Slow {
        each: Duration,
        reads: Vec<Field>,
        fail: Option<Field>,
    }
    impl Probe for Slow {
        fn read(&mut self, field: Field) -> Result<u32, String> {
            std::thread::sleep(self.each);
            self.reads.push(field);
            if self.fail == Some(field) { Err("no".into()) } else { Ok(1) }
        }
    }

    #[test]
    fn a_sample_reads_every_field_within_its_budget() {
        let mut p = Slow { each: Duration::ZERO, reads: vec![], fail: None };
        let out = sample(&mut p, Duration::from_millis(100), 0);
        assert_eq!(out.status, Status::Ok);
        assert_eq!(out.values, [Some(1); 5]);
        assert_eq!(p.reads, FIELDS);
    }

    #[test]
    fn a_sample_past_its_budget_stops_and_says_so() {
        let mut p = Slow { each: Duration::from_millis(30), reads: vec![], fail: None };
        let out = sample(&mut p, Duration::from_millis(50), 0);
        assert_eq!(out.status, Status::OverBudget);
        assert!(p.reads.len() < 5, "{:?}", p.reads);
        assert_eq!(out.values.iter().filter(|v| v.is_none()).count(), 5 - p.reads.len());
    }

    #[test]
    fn a_failed_read_is_an_error_not_a_zero() {
        let mut p = Slow { each: Duration::ZERO, reads: vec![], fail: Some(Field::Snr) };
        let out = sample(&mut p, Duration::from_millis(100), 0);
        assert_eq!(out.status, Status::Error);
        assert_eq!(out.values, [Some(1), Some(1), None, None, None]);
    }

    #[test]
    fn samples_do_not_overlap_and_a_late_one_is_not_made_up() {
        let stop = AtomicBool::new(false);
        let mut p = Slow { each: Duration::from_millis(15), reads: vec![], fail: None };
        let mut outs: Vec<Outcome> = vec![];
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(400));
                stop.store(true, Ordering::SeqCst);
            });
            run(&mut p, Duration::from_millis(20), Duration::from_secs(1), &stop, &mut |o| outs.push(*o));
        });
        // 75 ms a sample, every 20 ms asked: about 400/75, not 400/20.
        assert!((3..=7).contains(&outs.len()), "{}", outs.len());
        for w in outs.windows(2) {
            assert!(w[1].start_raw >= w[0].end_raw, "a sample began before the one before ended");
        }
    }

    #[test]
    fn unsupported_statistics_are_marked_not_zero() {
        assert_eq!(UNSUPPORTED.count_ones(), 10);
        assert_ne!(UNSUPPORTED & PRE_FEC_BITS, 0);
        assert_ne!(UNSUPPORTED & ERROR_BLOCKS, 0);
    }

    /// The bridge's I2C commands answered, with a count of demodulator
    /// transactions in flight (address written, bytes not yet read).
    #[derive(Default)]
    struct Bus {
        frames: Mutex<HashMap<std::thread::ThreadId, Vec<u8>>>,
        inside: AtomicI32,
        most: AtomicI32,
    }
    impl Transport for Bus {
        fn write(&self, _: u8, data: &[u8], _: u32) -> Result<usize, UsbError> {
            if data[2] == CMD_GENERIC_I2C_WR {
                let now = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
                self.most.fetch_max(now, Ordering::SeqCst);
            }
            self.frames.lock().unwrap().insert(std::thread::current().id(), data.to_vec());
            Ok(data.len())
        }
        fn read(&self, _: u8, _: usize, _: u32) -> Result<Vec<u8>, UsbError> {
            std::thread::sleep(Duration::from_micros(300));
            let frame = self.frames.lock().unwrap()[&std::thread::current().id()].clone();
            let count = if frame[2] == CMD_GENERIC_I2C_RD { usize::from(frame[4]) } else { 0 };
            if frame[2] == CMD_GENERIC_I2C_RD {
                self.inside.fetch_sub(1, Ordering::SeqCst);
            }
            let mut reply = vec![(count + 4) as u8, frame[3], 0];
            reply.extend(std::iter::repeat_n(1u8, count));
            let sum = checksum(&reply);
            reply.extend_from_slice(&sum.to_be_bytes());
            Ok(reply)
        }
    }

    #[test]
    fn two_threads_never_ask_the_demodulator_at_once() {
        let bus = Bus::default();
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    let log = crate::log::Log::silent();
                    let clock = crate::RealClock::default();
                    let mut bridge = Bridge::new(&bus, &log, &clock, []);
                    let demod = crate::avl62x1::Avl62x1::new([]);
                    for _ in 0..50 {
                        demod.lock_flags(&mut bridge).unwrap();
                    }
                });
            }
        });
        assert_eq!(bus.most.load(Ordering::SeqCst), 1, "one transaction at a time");
        assert_eq!(bus.inside.load(Ordering::SeqCst), 0);
    }
}
