//! The RDA5815M satellite tuner behind the AVL62X1 I2C repeater (7-bit 0x0C).
//!
//! Kanky 2.109 RDA5815M_Initialize / RDA5815M_Lock, as kankylink/rda5815m.py:
//! init reads reg 0x00 (discarded), 0x00 and 0x01 to pick a table, writes
//! reg04=0x04, reg04=0x05 and the table; lock is write-only (the vendor's lock
//! status is a constant "locked"). The caller holds the repeater open.

use serde::Serialize;

use crate::bridge::Bridge;
use crate::error::{Error, Result};
use crate::firmware::TunerTables;

pub const I2C_ADDR: u8 = 0x0C;
const PLL_FACTOR: u64 = 0x12F68; // N = f_MHz * 0x12F68 (~2^21 / 27 MHz crystal)
pub const RF_MIN_HZ: u32 = 950_000_000;
pub const RF_MAX_HZ: u32 = 2_150_000_000;

#[derive(Debug, Clone, Serialize)]
pub struct TunerInit {
    pub id: (u8, u8),
    pub recognised: Option<&'static str>,
    pub table: &'static str,
    pub reason: String,
}

fn read_reg(bridge: &mut Bridge, reg: u8) -> Result<u8> {
    bridge.i2c_write(I2C_ADDR, &[reg])?;
    Ok(bridge.i2c_read(I2C_ADDR, 1)?[0])
}

fn write_reg(bridge: &mut Bridge, reg: u8, value: u8) -> Result<()> {
    bridge.i2c_write(I2C_ADDR, &[reg, value])
}

fn recognise(r0: u8, r1: u8) -> Option<&'static str> {
    if r0 == 0x58 && r1 == 0xF8 {
        Some("RDA5815M (0x58/0xF8, Kanky table 0x2b409)")
    } else if r0 == 0xB0 && r1 >= 0xF0 {
        Some("RDA5815 variant (0xB0/>=0xF0, Kanky table 0x2b51f)")
    } else {
        None
    }
}

/// The vendor's table choice: B for the variant, A otherwise -- also when
/// the ID is unreadable, which through this board's repeater it is (0xFF).
fn select_table(r0: u8, r1: u8) -> (&'static str, String) {
    if r0 == 0x58 && r1 == 0xF8 {
        ("A", "ID 0x58/0xF8".into())
    } else if r0 == 0xB0 && r1 >= 0xF0 {
        ("B", "ID 0xB0/>=0xF0".into())
    } else {
        ("A", format!("fallback (ID 0x{r0:02X}/0x{r1:02X} unrecognised, as vendor)"))
    }
}

pub fn init(bridge: &mut Bridge, tables: &TunerTables) -> Result<TunerInit> {
    bridge.delay(1);
    read_reg(bridge, 0x00)?; // the vendor discards the first read
    let r0 = read_reg(bridge, 0x00)?;
    let r1 = read_reg(bridge, 0x01)?;
    let recognised = recognise(r0, r1);
    bridge.log.line(format!(
        "TUNER 0x{I2C_ADDR:02X}: reg00=0x{r0:02X} reg01=0x{r1:02X} -> {}",
        recognised.unwrap_or("unrecognised")
    ));
    let (table, reason) = select_table(r0, r1);
    let writes = if table == "A" { &tables.a } else { &tables.b };
    bridge.log.line(format!("TUNER init: table {table} ({} writes), {reason}", writes.len()));
    write_reg(bridge, 0x04, 0x04)?;
    write_reg(bridge, 0x04, 0x05)?;
    for &(reg, value) in writes {
        write_reg(bridge, reg, value)?;
    }
    bridge.delay(10);
    Ok(TunerInit { id: (r0, r1), recognised, table, reason })
}

/// The register writes of RDA5815M_Lock for an L-band RF and an LPF, in Hz.
pub fn lock_writes(rf_hz: u32, lpf_hz: u32) -> Result<[(u8, u8); 9]> {
    if !(RF_MIN_HZ..=RF_MAX_HZ).contains(&rf_hz) {
        return Err(Error::Protocol(format!("RF {rf_hz} Hz outside {RF_MIN_HZ}..{RF_MAX_HZ}")));
    }
    let n = (rf_hz as u64 / 1_000_000) * PLL_FACTOR;
    let bw_mhz = lpf_hz / 1_000_000;
    let lpf_reg = if bw_mhz < 4 {
        0x44
    } else if bw_mhz < 41 {
        0x40 | (bw_mhz as u8 & 0x3F)
    } else {
        0x68
    };
    Ok([
        (0x04, 0xC1),
        (0x2B, 0x95),
        (0x07, (n >> 24) as u8),
        (0x08, (n >> 16) as u8),
        (0x09, (n >> 8) as u8),
        (0x0A, n as u8),
        (0x0B, lpf_reg),
        (0x04, 0xC3),
        (0x2B, 0x97),
    ])
}

pub fn lock(bridge: &mut Bridge, rf_hz: u32, lpf_hz: u32) -> Result<()> {
    let writes = lock_writes(rf_hz, lpf_hz)?;
    bridge.log.line(format!(
        "TUNER lock RF {:.0} MHz LPF {:.1} MHz: {}",
        rf_hz as f64 / 1e6,
        lpf_hz as f64 / 1e6,
        writes.iter().map(|(r, v)| format!("{r:02X}={v:02X}")).collect::<Vec<_>>().join(" ")
    ));
    for (reg, value) in writes {
        write_reg(bridge, reg, value)?;
    }
    bridge.delay(5);
    Ok(())
}
