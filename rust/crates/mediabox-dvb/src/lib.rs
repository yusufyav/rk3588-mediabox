//! The Kanky Link-4K satellite receiver, natively.
//!
//! An ITE IT9303 USB bridge (23e2:5270) in front of an Availink AVL62X1
//! DVB-S2 demodulator and an RDA5815M tuner. The board can also drive an LNB
//! supply and a 22 kHz tone through the demodulator's pins; MediaBox does
//! not: the RF arrives prepared from outside, and nothing here can write
//! those pins (`avl62x1::Scope`). Every register,
//! command and order here is the one verified on the hardware by the
//! reference implementation (`yusufyav/kanky-link-linux`, Python); this crate
//! is that protocol in Rust, and `tests/replay.rs` holds it to the bytes of a
//! real session.
//!
//! The layers, bottom up:
//!
//! * [`libusb`] -- libusb-1.0, opened at run time.
//! * [`usb`] -- one context, one handle, interface 0 claimed once; bulk
//!   commands on EP 0x02/0x81 and queued asynchronous reads on EP 0x84 over
//!   the same handle.
//! * [`bridge`] -- the IT930x command frame, its registers, GPIO and I2C, and
//!   its firmware.
//! * [`patch`], [`avl62x1`], [`rda5815m`] -- the demodulator, its firmware
//!   script, the tuner behind its repeater.
//! * [`tune`] -- the stages from reset to FEC lock, and the cleanup that
//!   always runs after them.
//! * [`ts`] -- MPEG-TS: integrity counters, PSI, and the selector that turns a
//!   multiplex into one service's single-programme stream.
//! * [`service`] -- the process the control plane starts: source, selector,
//!   output, status, and the end of it all.

pub mod avl62x1;
pub mod bridge;
pub mod error;
pub mod firmware;
pub mod libusb;
pub mod log;
pub mod patch;
pub mod rda5815m;
pub mod selftest;
pub mod service;
pub mod sha256;
pub mod tkgs;
pub mod ts;
pub mod tune;
pub mod usbpower;
pub mod usb;

pub use error::{Error, Result};

/// Time, as the protocol needs it: a pause between steps and a monotonic
/// reading. Real on the hardware, free in a replay.
pub trait Clock {
    fn delay_ms(&self, ms: u64);
    fn now(&self) -> std::time::Duration;
}

/// The wall clock.
pub struct RealClock {
    start: std::time::Instant,
}

impl Default for RealClock {
    fn default() -> Self {
        Self { start: std::time::Instant::now() }
    }
}

impl Clock for RealClock {
    fn delay_ms(&self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    fn now(&self) -> std::time::Duration {
        self.start.elapsed()
    }
}
