//! The ITE IT930x USB bridge: command frame, registers, guarded GPIO, generic
//! I2C, the TS output, and the bridge's own firmware.
//!
//! Framing and command codes follow Linux drivers/media/usb/dvb-usb-v2/
//! af9035.c; they were verified on the Kanky Link-4K by the reference
//! implementation (kankylink/it930x.py, scripts/load-it930x-firmware.py).

use std::collections::BTreeSet;

use crate::error::{Error, Result};
use crate::log::Log;
use crate::usb::{EP_IN, EP_OUT, Transport};
use crate::Clock;

pub const TIMEOUT_MS: u32 = 2000;

pub const CMD_MEM_RD: u8 = 0x00;
pub const CMD_MEM_WR: u8 = 0x01;
pub const CMD_FW_QUERYINFO: u8 = 0x22;
pub const CMD_FW_BOOT: u8 = 0x23;
pub const CMD_FW_SCATTER_WR: u8 = 0x29;
pub const CMD_GENERIC_I2C_RD: u8 = 0x2A;
pub const CMD_GENERIC_I2C_WR: u8 = 0x2B;

/// Kanky drvi2c_dvb_fe_ite_Write uses the direct path up to 0x28 bytes.
pub const I2C_MAX_WRITE: usize = 40;
pub const I2C_MAX_READ: usize = 40;
pub const MAX_PAYLOAD: usize = 58;

/// The bridge firmware the whole stack was verified on (Linux
/// dvb-usb-it9303-01.fw).
pub const VERIFIED_FIRMWARE: [u8; 4] = [1, 4, 0, 0];
pub const CHIP_IT9306: u16 = 0x9306;

/// An IT930x GPIOH register triplet; names as in Linux af9035 (gpiot2_o/en/on).
#[derive(Debug, Clone, Copy)]
pub struct Gpio {
    pub name: &'static str,
    pub o: u16,
    pub en: u16,
    pub on: u16,
}

impl Gpio {
    pub fn regs(&self) -> [u16; 3] {
        [self.o, self.en, self.on]
    }
}

pub const GPIO2: Gpio = Gpio { name: "gpiot2 (demod reset, active-low)", o: 0xD8B7, en: 0xD8B8, on: 0xD8B9 };
pub const GPIO5: Gpio = Gpio { name: "GPIO5 (input on Kanky, PID>=0x5271 only)", o: 0xD8BB, en: 0xD8BC, on: 0xD8BD };
pub const GPIO14: Gpio = Gpio { name: "GPIO14 (set by Kanky open(), function unknown)", o: 0xD8E3, en: 0xD8E4, on: 0xD8E5 };
pub const GPIO_BIT: u8 = 0x01;

pub fn checksum(data: &[u8]) -> u16 {
    let total = data
        .iter()
        .enumerate()
        .skip(1)
        .map(|(i, &byte)| if i & 1 == 1 { (byte as u32) << 8 } else { byte as u32 })
        .fold(0u32, u32::wrapping_add);
    (!total & 0xFFFF) as u16
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(" ")
}

pub struct Bridge<'a> {
    pub device: &'a dyn Transport,
    pub log: &'a Log,
    pub clock: &'a dyn Clock,
    writable: BTreeSet<u16>,
    seq: u8,
    pub i2c_bus: u8,
}

impl<'a> Bridge<'a> {
    pub fn new(device: &'a dyn Transport, log: &'a Log, clock: &'a dyn Clock, writable: impl IntoIterator<Item = u16>) -> Self {
        Self { device, log, clock, writable: writable.into_iter().collect(), seq: 0, i2c_bus: 3 }
    }

    pub fn delay(&self, ms: u64) {
        self.clock.delay_ms(ms);
    }

    pub fn allow_writes(&mut self, regs: impl IntoIterator<Item = u16>) {
        self.writable.extend(regs);
    }

    // -- transport ---------------------------------------------------------
    pub fn command(&mut self, cmd: u8, payload: &[u8], rx_count: usize) -> Result<Vec<u8>> {
        if payload.len() > MAX_PAYLOAD {
            return Err(Error::Protocol("payload exceeds the known command-frame limit".into()));
        }
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        let mut frame = vec![(payload.len() + 5) as u8, 0x00, cmd, seq];
        frame.extend_from_slice(payload);
        let sum = checksum(&frame);
        frame.extend_from_slice(&sum.to_be_bytes());

        self.log.line(format!("  USB TX: {}", hex(&frame)));
        let written = self.device.write(EP_OUT, &frame, TIMEOUT_MS)?;
        if written != frame.len() {
            return Err(Error::Protocol(format!("short USB transfer ({written}/{})", frame.len())));
        }
        let reply = self.device.read(EP_IN, rx_count + 5, TIMEOUT_MS)?;
        self.log.line(format!("  USB RX: {}", hex(&reply)));
        if reply.len() >= 5 && reply[1] == seq && reply[2] != 0 {
            return Err(Error::Device { status: reply[2], cmd });
        }
        if reply.len() != rx_count + 5 {
            return Err(Error::Protocol(format!("unexpected reply length: {}", reply.len())));
        }
        if reply[0] as usize != rx_count + 4 || reply[1] != seq {
            return Err(Error::Protocol(format!("bad reply header: {}", hex(&reply))));
        }
        let got = u16::from_be_bytes([reply[reply.len() - 2], reply[reply.len() - 1]]);
        if got != checksum(&reply[..reply.len() - 2]) {
            return Err(Error::Protocol("reply checksum mismatch".into()));
        }
        Ok(reply[3..reply.len() - 2].to_vec())
    }

    pub fn firmware_version(&mut self) -> Result<[u8; 4]> {
        let reply = self.command(CMD_FW_QUERYINFO, &[0x01], 4)?;
        Ok([reply[0], reply[1], reply[2], reply[3]])
    }

    // -- registers -----------------------------------------------------------
    pub fn read_reg(&mut self, address: u16) -> Result<u8> {
        let [hi, lo] = address.to_be_bytes();
        Ok(self.command(CMD_MEM_RD, &[1, 2, 0, 0, hi, lo], 1)?[0])
    }

    pub fn write_reg(&mut self, address: u16, value: u8) -> Result<()> {
        if !self.writable.contains(&address) {
            return Err(Error::Policy(format!("bridge write to 0x{address:04X} is outside the approved scope")));
        }
        let [hi, lo] = address.to_be_bytes();
        self.command(CMD_MEM_WR, &[1, 2, 0, 0, hi, lo, value], 0)?;
        Ok(())
    }

    /// af9035_wr_reg_mask() with a read-back check.
    pub fn write_reg_mask(&mut self, address: u16, value: u8, mask: u8, label: &str) -> Result<()> {
        let old = self.read_reg(address)?;
        let new = (old & !mask) | (value & mask);
        if new != old {
            self.write_reg(address, new)?;
        }
        let check = self.read_reg(address)?;
        self.log.line(format!(
            "{label} 0x{address:04X}: 0x{old:02X} -> 0x{new:02X} (mask 0x{mask:02X}), read-back 0x{check:02X}"
        ));
        if check != new {
            return Err(Error::GpioVerify(format!("0x{address:04X} read-back 0x{check:02X} != 0x{new:02X}")));
        }
        Ok(())
    }

    // -- generic I2C ---------------------------------------------------------
    pub fn i2c_write(&mut self, addr7: u8, data: &[u8]) -> Result<()> {
        if data.len() > I2C_MAX_WRITE {
            return Err(Error::Protocol("I2C write longer than the direct generic-I2C limit".into()));
        }
        let mut payload = vec![data.len() as u8, self.i2c_bus, addr7 << 1];
        payload.extend_from_slice(data);
        self.command(CMD_GENERIC_I2C_WR, &payload, 0)?;
        Ok(())
    }

    pub fn i2c_read(&mut self, addr7: u8, count: usize) -> Result<Vec<u8>> {
        if count == 0 || count > I2C_MAX_READ {
            return Err(Error::Protocol("I2C read length outside the direct generic-I2C limit".into()));
        }
        self.command(CMD_GENERIC_I2C_RD, &[count as u8, self.i2c_bus, addr7 << 1], count)
    }
}

// ------------------------------------------------------------------ GPIO

/// A GPIO triplet as found, to be put back exactly.
pub struct GpioSnapshot {
    pub gpio: Gpio,
    pub initial: [(u16, u8); 3],
}

pub fn snapshot(bridge: &mut Bridge, gpio: Gpio) -> Result<GpioSnapshot> {
    let mut initial = [(0u16, 0u8); 3];
    for (slot, address) in initial.iter_mut().zip(gpio.regs()) {
        *slot = (address, bridge.read_reg(address)?);
    }
    bridge.log.line(format!(
        "GPIO {} initial: {}",
        gpio.name,
        initial.iter().map(|(a, v)| format!("0x{a:04X}=0x{v:02X}")).collect::<Vec<_>>().join(", ")
    ));
    Ok(GpioSnapshot { gpio, initial })
}

/// Restore and verify, in the order on, en, o, so the line returns to its
/// undriven state without an extra drive phase. A failed restore is
/// reported, never retried with another reset.
pub fn restore(bridge: &mut Bridge, snapshot: &GpioSnapshot) -> bool {
    bridge.log.line(format!("RESTORE: {} initial values", snapshot.gpio.name));
    let value = |address: u16| snapshot.initial.iter().find(|(a, _)| *a == address).map(|(_, v)| *v).unwrap_or(0);
    let mut ok = true;
    for address in [snapshot.gpio.on, snapshot.gpio.en, snapshot.gpio.o] {
        let initial = value(address);
        let step = (|| -> Result<u8> {
            bridge.write_reg_mask(address, initial, GPIO_BIT, "GPIO")?;
            let check = bridge.read_reg(address)?;
            if check != initial {
                return Err(Error::GpioVerify(format!("full byte 0x{check:02X} != initial 0x{initial:02X}")));
            }
            Ok(check)
        })();
        match step {
            Ok(check) => bridge.log.line(format!("RESTORE OK 0x{address:04X} = 0x{check:02X}")),
            Err(error) => {
                ok = false;
                bridge.log.line(format!("RESTORE FAILED 0x{address:04X}: {error}"));
            }
        }
    }
    if !ok {
        bridge.log.line(
            "RESTORE INCOMPLETE: GPIO başlangıç durumu doğrulanamadı; ikinci reset denenmedi. \
             USB çıkar/tak ile cihazı güç-açılış durumuna döndürün",
        );
    }
    ok
}

/// Android fcn 0x4c9f4 / Linux it930x_frontend_attach gpiot2 sequence.
pub fn demod_reset_pulse(bridge: &mut Bridge) -> Result<()> {
    bridge.log.line("GPIO: gpiot2 reset pulse");
    bridge.write_reg_mask(GPIO2.o, 1, GPIO_BIT, "GPIO")?; // latch high (released) before driving
    bridge.write_reg_mask(GPIO2.en, 1, GPIO_BIT, "GPIO")?;
    bridge.write_reg_mask(GPIO2.on, 1, GPIO_BIT, "GPIO")?;
    bridge.delay(200);
    bridge.write_reg_mask(GPIO2.o, 0, GPIO_BIT, "GPIO")?; // assert reset (active-low)
    bridge.delay(200);
    bridge.write_reg_mask(GPIO2.o, 1, GPIO_BIT, "GPIO")?; // release reset
    bridge.delay(200);
    Ok(())
}

/// Kanky open() fcn 0x4cbc0 order: 0xD8E4=1, 0xD8E5=1, 0xD8E3=1 (bit 0 only).
pub fn enable_gpio14(bridge: &mut Bridge) -> Result<()> {
    bridge.log.line("GPIO: GPIO14 enable (Kanky open() order)");
    bridge.write_reg_mask(GPIO14.en, 1, GPIO_BIT, "GPIO")?;
    bridge.write_reg_mask(GPIO14.on, 1, GPIO_BIT, "GPIO")?;
    bridge.write_reg_mask(GPIO14.o, 1, GPIO_BIT, "GPIO")
}

// ------------------------------------------------------------------ TS output

const FRAME: u16 = (816 * 188 / 4) as u16;
const PACKET: u8 = (512 / 4) as u8;

/// Linux af9035 it930x_init() for a USB 2.0 high-speed link, single TS (EP4),
/// serial TS input; the GPIO "suspend" rows and the EP5 frame rows are left
/// out, as the reference does (Kanky 2.110 IT9300_configOutput sets EP4 only).
pub const TS_INIT: [(u16, u8, u8); 23] = [
    (0xDA1A, 0x00, 0x01), // ignore_sync_byte
    (0xF41F, 0x04, 0x04), // dvbt_inten
    (0xDA10, 0x00, 0x01), // mpeg_full_speed
    (0xF41A, 0x01, 0x01), // dvbt_en
    (0xDA1D, 0x01, 0x01), // mp2_sw_rst, reset EP4
    (0xDD11, 0x00, 0x20), // ep4_tx_en: disable
    (0xDD13, 0x00, 0x20), // ep4_tx_nak: disable
    (0xDD11, 0x20, 0x20), // ep4_tx_en: enable
    (0xDD11, 0x00, 0x40), // ep5_tx_en: disable (single tuner)
    (0xDD13, 0x00, 0x40), // ep5_tx_nak: disable
    (0xDD88, (FRAME & 0xFF) as u8, 0xFF),
    (0xDD89, (FRAME >> 8) as u8, 0xFF),
    (0xDD0C, PACKET, 0xFF),
    (0xDA1D, 0x00, 0x01), // mp2_sw_rst: release
    (0xD833, 0x01, 0xFF), // slew rate
    (0xD830, 0x00, 0xFF), // output driving bit 0
    (0xD831, 0x01, 0xFF), // output driving bit 1
    (0xD832, 0x00, 0xFF), // output driving bit 2
    (0xDA58, 0x00, 0x01), // ts_in_src: serial (Kanky IT9300_setInTsType -> 0)
    (0xDA73, 0x01, 0xFF), // ts0_aggre_mode
    (0xDA78, 0x47, 0xFF), // ts0_sync_byte
    (0xDA4C, 0x01, 0xFF), // ts0_en
    (0xDA5A, 0x1F, 0xFF), // ts_fail_ignore (also DRV_AVL6221_initialize)
];

pub fn ts_init_regs() -> BTreeSet<u16> {
    TS_INIT.iter().map(|(address, _, _)| *address).collect()
}

pub fn ts_init(bridge: &mut Bridge) -> Result<()> {
    bridge.log.line("BRIDGE TS init (Linux it930x_init subset, EP4 = 0x84)");
    for (address, value, mask) in TS_INIT {
        bridge.write_reg_mask(address, value, mask, "TS")?;
    }
    Ok(())
}

/// Cleanup: ep4_tx_en off.
pub fn ts_ep4_disable(bridge: &mut Bridge) -> Result<()> {
    bridge.log.line("BRIDGE TS cleanup: EP4 TX off");
    bridge.write_reg_mask(0xDD11, 0x00, 0x20, "TS")
}

// ------------------------------------------------------------------ firmware

/// The bridge's chip type (register 0x1222, three bytes).
pub fn chip_type(bridge: &mut Bridge) -> Result<u16> {
    let chip = bridge.command(CMD_MEM_RD, &[3, 2, 0, 0, 0x12, 0x22], 3)?;
    Ok(chip[1] as u16 | (chip[2] as u16) << 8)
}

/// Upload and boot the bridge firmware on a device that came up COLD (every
/// plug-in does): the scatter-write blocks of `crate::firmware::bridge_blocks`
/// as Linux af9035 sends them, then FW_BOOT, then the version must be the
/// verified one.
pub fn load_firmware(bridge: &mut Bridge, blocks: &[Vec<u8>]) -> Result<[u8; 4]> {
    let chip = chip_type(bridge)?;
    bridge.log.line(format!("BRIDGE chip type 0x{chip:04X}"));
    if chip != CHIP_IT9306 {
        return Err(Error::Firmware(format!("unexpected USB bridge 0x{chip:04X}")));
    }
    for (index, block) in blocks.iter().enumerate() {
        bridge.command(CMD_FW_SCATTER_WR, block, 0)?;
        if index + 1 == blocks.len() || (index + 1) % 8 == 0 {
            bridge.log.line(format!("BRIDGE firmware block {}/{}", index + 1, blocks.len()));
        }
    }
    bridge.log.line("BRIDGE firmware boot requested");
    bridge.command(CMD_FW_BOOT, &[], 0)?;
    bridge.delay(500);
    let version = bridge.firmware_version()?;
    bridge.log.line(format!("BRIDGE firmware after boot: {}", dotted(&version)));
    if version == [0; 4] {
        return Err(Error::Firmware("bridge firmware did not start".into()));
    }
    Ok(version)
}

pub fn dotted(version: &[u8]) -> String {
    version.iter().map(u8::to_string).collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_frame_of_a_real_session_is_rebuilt_byte_for_byte() {
        // tune-hw-lnb-20261022-003017.log, first line: FW_QUERYINFO, seq 0.
        let frame = [0x06, 0x00, 0x22, 0x00, 0x01];
        assert_eq!(checksum(&frame), 0xFFDC);
        let read = [0x0B, 0x00, 0x00, 0x01, 0x01, 0x02, 0x00, 0x00, 0xD8, 0xBB];
        assert_eq!(checksum(&read), 0x4126);
    }

    #[test]
    fn ts_init_frame_constants_are_the_bridge_s() {
        assert_eq!(FRAME, 38352); // 816 * 188 / 4 = 0x95D0
        assert_eq!(TS_INIT[10].1, 0xD0);
        assert_eq!(TS_INIT[11].1, 0x95);
        assert_eq!(TS_INIT[12].1, 0x80);
    }
}
