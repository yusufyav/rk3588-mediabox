//! The Availink AVL62X1 (AVL6221/AVL6261) demodulator over the bridge's I2C.
//!
//! Register addresses and command semantics follow the GPL-2.0 Availink
//! driver (avl62x1_reg.h / avl62x1_lib.c / avl62x1_api.c). Board values are
//! the Kanky Link 2.109 chip object: I2C 0x14, 27 MHz crystal, 130 MHz MPEG
//! reference clock. kankylink/avl62x1.py is the reference, read for read.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::bridge::Bridge;
use crate::error::{Error, Result};
use crate::patch;

pub const I2C_ADDR: u8 = 0x14;
pub const CHIP_ID: u32 = 0x62615CA8;
pub const CORE_READY_WORD: u32 = 0x5AA57FF7;

const REG_HW_ID: u32 = 0x040000;
const REG_CPUCORE_SRST: u32 = 0x110840;
const REG_CORE_READY: u32 = 0x0000A0;
const REG_COMMAND: u32 = 0x000200;
const REG_XTAL: u32 = 0x000205;
const REG_SYS_CLK: u32 = 0x00083C;
const REG_FEC_CLK: u32 = 0x000840;
const REG_MPEG_CLK: u32 = 0x000848;
const REG_PATCH_VER: u32 = 0x0000A8;

const TUNER_I2C_SRST: u32 = 0x118000;
const TUNER_I2C_CNTRL: u32 = 0x118004;
const TUNER_RPT_CLK_DIV: u32 = 0x118018;
const TUNER_RPT_CNTRL: u32 = 0x11801C; // 6 = repeater off, 7 = repeater on
const GPIO_I2C_DATA2_SEL: u32 = 0x120014;
const GPIO_I2C_CLK2_SEL: u32 = 0x120018;
const GPIO_M3_SCL_SEL: u32 = 0x12009C;
const GPIO_M3_SDA_SEL: u32 = 0x1200A0;

/// Kanky AVL GPIO 4 (13/18 V select) and GPIO 3 (LNB power enable). Read,
/// never written: MediaBox does not power or steer the LNB (the RF arrives
/// prepared from outside), and no scope exists that would let a write here.
pub const LNB_CNTRL_1_SEL: u32 = 0x120008;
pub const LNB_CNTRL_0_SEL: u32 = 0x12000C;

const CMD_IDLE: u16 = 0;
const CMD_LD_DEFAULT: u16 = 1;
const CMD_ACQUIRE: u16 = 2;
const CMD_HALT: u16 = 3;
const CMD_FAILED: u16 = 255;

const REG_INPUT_FORMAT: u32 = 0x00082D;
const REG_TUNER_SPECTRUM_INVERT: u32 = 0x00082E;
const REG_INPUT_SELECT: u32 = 0x00082F;
const REG_RF_AGC_POL: u32 = 0x00080F;
const REG_RF_AGC_MAX_GAIN: u32 = 0x000864;
const REG_RF_AGC_MIN_GAIN: u32 = 0x000866;
const REG_AGC_SD_CONTROL: u32 = 0x160900;
const REG_NOM_SYMBOL_RATE: u32 = 0x000804;
const REG_NOM_CARRIER_FREQ: u32 = 0x000830;
const REG_BLIND_SYM_RATE_ENABLE: u32 = 0x000851;
const REG_BLIND_CFO_ENABLE: u32 = 0x000852;
const REG_LOCK_STATUS: u32 = 0x000408;
const REG_FEC_LOCK: u32 = 0x00040A;
const REG_SP_STREAM_TYPE: u32 = 0xA00605;
const REG_SP_PLP_ID: u32 = 0xA00606;
const REG_SP_WANTED_STREAM_ID: u32 = 0xA0060A;
const REG_SP_T2MI_PID_CUR: u32 = 0xA0061A;
const REG_MPEG_BUS_OFF: u32 = 0xB08410;
const REG_MPEG_BUS_E_B: u32 = 0xB08420;

// Read-only status (upstream avl62x1_reg.h; 0x0E8 and 0x412 are also what
// Kanky 2.110 AVL62X1_GetSignalStrength / AVL62X1_GetSNR read).
const REG_RFAGC_GAIN: u32 = 0x0000E8;
const REG_SNR_X100: u32 = 0x000412;
const REG_SYMBOL_RATE: u32 = 0x00041C;
const REG_ACQ_RETRY: u32 = 0x00042B;
const REG_CARRIER_FREQ: u32 = 0x00045C;
const REG_CARRIER_FREQ_ERR: u32 = 0x000460;
/// avl62x1_get_signal_strength: gain <= 25000 is 100 %, >= 55000 is 10 %.
pub const RFAGC_STRONG: u16 = 25000;
pub const RFAGC_WEAK: u16 = 55000;

/// IRx_SetMpegMode with the Kanky chip object.
const KANKY_MPEG_CONFIG: [(u32, u8); 8] = [
    (0xA0020C, 1), // sp_ts_serial: serial
    (0xA00209, 0), // sp_ts0_tsp1: TS (not TS+parity)
    (0xA0020B, 0), // sp_ts_clock_edge: falling
    (0xA00237, 0), // sp_ts_clock_phase: 0
    (0xA00235, 1), // sp_ts_adapt_clk: adaptive
    (0xA0020F, 0), // sp_ts_serial_outpin: MPEG_DATA_0
    (0xA0020A, 1), // sp_enable_ts_continuous
    (0xA00236, 3), // sp_serial_ts_ds
];

const TUNER_I2C_DIAG: [u32; 5] = [0x118004, 0x11800C, 0x118010, 0x118014, 0x11801C];

pub const XTAL_27MHZ: u8 = 1;
pub const KANKY_MPEG_REF_CLK_HZ: u32 = 130_000_000;

/// Blocks this stack must not touch. The board GPIO block holds the LNB
/// supply pins; the DiSEqC block is the 22 kHz tone and DiSEqC. Only the four
/// tuner-I2C pin selects and, under its scope, the two AGC pin selects may be
/// written in the first; nothing at all in the second.
const FORBIDDEN_RANGES: [(u32, u32, &str); 2] =
    [(0x120000, 0x1200FF, "gpio_debug (LNB / board GPIO)"), (0x16C000, 0x16C0FF, "diseqc (22 kHz / DiSEqC)")];
const ALLOWED_IN_FORBIDDEN: [u32; 4] = [GPIO_I2C_DATA2_SEL, GPIO_I2C_CLK2_SEL, GPIO_M3_SCL_SEL, GPIO_M3_SDA_SEL];
const AGC1_SEL: u32 = 0x120000;
const AGC2_SEL: u32 = 0x120010;

const BMS_WRITE_CHUNK: usize = 32; // 3 address bytes + 32 <= 40
const BMS_READ_CHUNK: usize = 32;

pub const MAX_LOCK_TIMEOUT_MS: u32 = 5000;

/// Opt-in write scopes on top of the four I2C pin selects. There is no LNB
/// or DiSEqC scope, by decision: the tuner and the demodulator run without
/// either, and the RF source (supply, tone, switching) is prepared outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// IRx_DriveAGC pin mux (6 = AGC, 2 = high-Z): the tuner's gain loop.
    Agc,
}

pub struct Avl62x1 {
    pub addr: u8,
    pub scopes: BTreeSet<Scope>,
    allowed: BTreeSet<u32>,
    pub max_patch_polls: u32,
    /// Every demod write as (address, length), for the report.
    pub writes: usize,
    pub repeater_maybe_on: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootInfo {
    pub sys_clk_hz: u32,
    pub fec_clk_hz: u32,
    pub mpeg_clk_hz: u32,
    pub srst: u32,
    pub ready_word: String,
    pub ready_after_polls: u32,
    pub running_patch: String,
    pub patch_commands: u32,
}

/// A read-only acquisition snapshot. Upstream reads SNR, symbol rate and
/// carrier only once locked; unlocked values are raw, not trusted.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AcqStatus {
    pub lock: u8,
    pub fec: u8,
    pub rfagc: u16,
    pub strength_pct: u32,
    pub snr_db: f64,
    pub acq_retries: u8,
    pub symbol_rate_hz: u32,
    pub carrier_hz: i32,
    pub carrier_err_hz: i32,
}

impl Avl62x1 {
    pub fn new(scopes: impl IntoIterator<Item = Scope>) -> Self {
        let scopes: BTreeSet<Scope> = scopes.into_iter().collect();
        let mut allowed: BTreeSet<u32> = ALLOWED_IN_FORBIDDEN.into_iter().collect();
        if scopes.contains(&Scope::Agc) {
            allowed.extend([AGC1_SEL, AGC2_SEL]);
        }
        Self { addr: I2C_ADDR, scopes, allowed, max_patch_polls: 300, writes: 0, repeater_maybe_on: false }
    }

    /// Refuse a write whose byte range touches a forbidden block, unless it is
    /// exactly one approved 32-bit pin select, or the DiSEqC block under its scope.
    fn check_write(&self, address: u32, length: usize) -> Result<()> {
        if length == 0 {
            return Ok(());
        }
        let end = address + length as u32;
        for (lo, hi, label) in FORBIDDEN_RANGES {
            if address <= hi && end > lo {
                if self.allowed.contains(&address) && length == 4 {
                    continue;
                }
                return Err(Error::Policy(format!(
                    "demod write 0x{address:06X}+{length} overlaps {label} 0x{lo:06X}..0x{hi:06X}; outside the approved scope"
                )));
            }
        }
        Ok(())
    }

    // -- BMS access ------------------------------------------------------------
    pub fn read(&self, bridge: &mut Bridge, address: u32, length: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(length);
        while out.len() < length {
            let n = BMS_READ_CHUNK.min(length - out.len());
            let at = address + out.len() as u32;
            bridge.i2c_write(self.addr, &at.to_be_bytes()[1..])?;
            out.extend(bridge.i2c_read(self.addr, n)?);
        }
        Ok(out)
    }

    pub fn write(&mut self, bridge: &mut Bridge, address: u32, data: &[u8]) -> Result<()> {
        self.check_write(address, data.len())?;
        self.writes += 1;
        for (i, chunk) in data.chunks(BMS_WRITE_CHUNK).enumerate() {
            let at = address + (i * BMS_WRITE_CHUNK) as u32;
            let mut frame = at.to_be_bytes()[1..].to_vec();
            frame.extend_from_slice(chunk);
            bridge.i2c_write(self.addr, &frame)?;
        }
        Ok(())
    }

    pub fn read32(&self, bridge: &mut Bridge, address: u32) -> Result<u32> {
        let b = self.read(bridge, address, 4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read16(&self, bridge: &mut Bridge, address: u32) -> Result<u16> {
        let b = self.read(bridge, address, 2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn read8(&self, bridge: &mut Bridge, address: u32) -> Result<u8> {
        Ok(self.read(bridge, address, 1)?[0])
    }

    pub fn write32(&mut self, bridge: &mut Bridge, address: u32, value: u32) -> Result<()> {
        self.write(bridge, address, &value.to_be_bytes())
    }

    pub fn write16(&mut self, bridge: &mut Bridge, address: u32, value: u16) -> Result<()> {
        self.write(bridge, address, &value.to_be_bytes())
    }

    pub fn write8(&mut self, bridge: &mut Bridge, address: u32, value: u8) -> Result<()> {
        self.write(bridge, address, &[value])
    }

    // -- identification / state --------------------------------------------------
    pub fn chip_id(&self, bridge: &mut Bridge) -> Result<u32> {
        self.read32(bridge, REG_HW_ID)
    }

    pub fn core_state(&self, bridge: &mut Bridge) -> Result<(u32, u32)> {
        Ok((self.read32(bridge, REG_CPUCORE_SRST)?, self.read32(bridge, REG_CORE_READY)?))
    }

    fn is_ready(&self, bridge: &mut Bridge) -> Result<bool> {
        let (srst, ready) = self.core_state(bridge)?;
        Ok(srst != 1 && ready == CORE_READY_WORD)
    }

    // -- firmware command channel (__avl62x1_send_cmd) -----------------------------
    fn wait_cmd_idle(&self, bridge: &mut Bridge) -> Result<bool> {
        for _ in 0..=50 {
            match self.read16(bridge, REG_COMMAND)? {
                CMD_IDLE => return Ok(true),
                CMD_FAILED => {
                    bridge.log.line("DEMOD command channel reports CMD_FAILED");
                    return Ok(false);
                }
                _ => bridge.delay(10),
            }
        }
        Ok(false)
    }

    pub fn send_cmd(&mut self, bridge: &mut Bridge, cmd: u16) -> Result<bool> {
        if !self.wait_cmd_idle(bridge)? {
            bridge.log.line(format!("DEMOD cmd {cmd}: channel busy before send"));
            return Ok(false);
        }
        self.write16(bridge, REG_COMMAND, cmd)?;
        let ok = self.wait_cmd_idle(bridge)?;
        bridge.log.line(format!("DEMOD cmd {cmd}: {}", if ok { "done" } else { "TIMEOUT" }));
        Ok(ok)
    }

    // -- boot (avl62x1_initialize, first part) --------------------------------------
    /// Download + boot + load defaults, then verify the running core, in the
    /// order of Kanky AVL62X1_Initialize and upstream avl62x1_initialize().
    pub fn boot(&mut self, bridge: &mut Bridge, script: &[u8]) -> Result<BootInfo> {
        let expected = patch::parse_header(script)?.version();
        let mut executor = patch::Executor::new(script)?;
        let commands = executor.run(&mut PatchTarget { demod: self, bridge, polls: 0 })?;
        bridge.log.line(format!("DEMOD patch script: {commands} commands executed"));
        self.write32(bridge, REG_MPEG_CLK, KANKY_MPEG_REF_CLK_HZ)?;
        self.write8(bridge, REG_XTAL, XTAL_27MHZ)?;
        if !self.send_cmd(bridge, CMD_LD_DEFAULT)? {
            return Err(Error::Demod("LD_DEFAULT failed or timed out".into()));
        }
        let sys_clk_hz = self.read32(bridge, REG_SYS_CLK)?;
        let fec_clk_hz = self.read32(bridge, REG_FEC_CLK)?;
        let mpeg_clk_hz = self.read32(bridge, REG_MPEG_CLK)?;
        let mut ready_after = None;
        for attempt in 0..=100 {
            if self.is_ready(bridge)? {
                ready_after = Some(attempt);
                break;
            }
            bridge.delay(50);
        }
        let Some(ready_after_polls) = ready_after else {
            let (srst, word) = self.core_state(bridge)?;
            return Err(Error::Demod(format!("core not ready: srst={srst} ready=0x{word:08X}")));
        };
        let (srst, word) = self.core_state(bridge)?;
        if srst != 0 {
            return Err(Error::Demod(format!("CPU still in soft reset after boot (srst={srst})")));
        }
        if word != CORE_READY_WORD {
            return Err(Error::Demod(format!("ready word 0x{word:08X} != 0x{CORE_READY_WORD:08X}")));
        }
        let raw = self.read32(bridge, REG_PATCH_VER)?;
        let running = (raw >> 24, (raw >> 16) & 0xFF, raw & 0xFFFF);
        if running != expected {
            return Err(Error::Demod(format!("running patch {running:?} != downloaded {expected:?}")));
        }
        if !(10_000_000..=500_000_000).contains(&sys_clk_hz) {
            return Err(Error::Demod(format!("implausible core clock {sys_clk_hz}")));
        }
        let info = BootInfo {
            sys_clk_hz,
            fec_clk_hz,
            mpeg_clk_hz,
            srst,
            ready_word: format!("0x{word:08X}"),
            ready_after_polls,
            running_patch: format!("{}.{}.{}", running.0, running.1, running.2),
            patch_commands: commands,
        };
        bridge.log.line(format!("DEMOD ready: {info:?}"));
        Ok(info)
    }

    // -- tuner I2C repeater ---------------------------------------------------------
    pub fn init_tuner_i2c(&mut self, bridge: &mut Bridge, core_clk_hz: u32) -> Result<u32> {
        if !(10_000_000..=500_000_000).contains(&core_clk_hz) {
            return Err(Error::Demod(format!("implausible core clock {core_clk_hz}")));
        }
        self.write32(bridge, TUNER_I2C_SRST, 1)?;
        self.write32(bridge, TUNER_I2C_SRST, 0)?;
        let cntrl = self.read32(bridge, TUNER_I2C_CNTRL)?;
        self.write32(bridge, TUNER_I2C_CNTRL, cntrl & 0xFFFF_FFFE)?;
        self.write32(bridge, TUNER_RPT_CNTRL, 0x6)?;
        let divider = (0x2Au64 * (core_clk_hz as u64 / 1000) / 240_000) as u32;
        self.write32(bridge, TUNER_RPT_CLK_DIV, divider)?;
        self.write32(bridge, GPIO_I2C_CLK2_SEL, 7)?;
        self.write32(bridge, GPIO_I2C_DATA2_SEL, 8)?;
        self.write32(bridge, GPIO_M3_SCL_SEL, 6)?;
        self.write32(bridge, GPIO_M3_SDA_SEL, 5)?;
        Ok(divider)
    }

    pub fn tuner_repeater(&mut self, bridge: &mut Bridge, enable: bool) -> Result<()> {
        if enable {
            self.repeater_maybe_on = true; // before the first write: cleanup must try
        }
        for _ in 0..3 {
            // vendor and upstream write it three times
            self.write32(bridge, TUNER_RPT_CNTRL, if enable { 0x7 } else { 0x6 })?;
        }
        if !enable {
            self.repeater_maybe_on = false;
        }
        Ok(())
    }

    pub fn lnb_pins(&self, bridge: &mut Bridge) -> Result<(u32, u32)> {
        Ok((self.read32(bridge, LNB_CNTRL_0_SEL)?, self.read32(bridge, LNB_CNTRL_1_SEL)?))
    }

    // -- receiver init after boot (Kanky AVL62X1_Initialize tail) -------------------
    /// IRx_Initialize, SetTunerPola, DriveAGC(on), SetMpegMode,
    /// DriveMpegOutput(on), with the Kanky chip object's values.
    pub fn init_receiver(&mut self, bridge: &mut Bridge) -> Result<()> {
        self.write8(bridge, REG_INPUT_FORMAT, 0)?; // 2's complement
        self.write8(bridge, REG_INPUT_SELECT, 1)?; // ADC input
        self.write8(bridge, REG_TUNER_SPECTRUM_INVERT, 1)?;
        self.drive_agc(bridge, true)?;
        for (address, value) in KANKY_MPEG_CONFIG {
            self.write8(bridge, address, value)?;
        }
        self.drive_mpeg_output(bridge, true)
    }

    pub fn drive_agc(&mut self, bridge: &mut Bridge, on: bool) -> Result<()> {
        let sd = self.read32(bridge, REG_AGC_SD_CONTROL)?;
        let pin = if on {
            // No tuner AGC-slope callback in Kanky -> slope -1: polarity 1,
            // min/max gain voltages 3200/100 mV of 3300 mV.
            self.write8(bridge, REG_RF_AGC_POL, 1)?;
            self.write16(bridge, REG_RF_AGC_MIN_GAIN, ((3300 - 3200) * 65536 / 3300) as u16)?;
            self.write16(bridge, REG_RF_AGC_MAX_GAIN, ((3300 - 100) * 65536 / 3300) as u16)?;
            self.write32(bridge, REG_AGC_SD_CONTROL, sd | 0x2)?;
            6
        } else {
            self.write32(bridge, REG_AGC_SD_CONTROL, sd & !0x2)?;
            2
        };
        self.write32(bridge, AGC1_SEL, pin)?;
        self.write32(bridge, AGC2_SEL, pin)
    }

    pub fn drive_mpeg_output(&mut self, bridge: &mut Bridge, on: bool) -> Result<()> {
        self.write8(bridge, REG_MPEG_BUS_OFF, if on { 0x00 } else { 0xFF })?;
        self.write32(bridge, REG_MPEG_BUS_E_B, if on { 0x000 } else { 0xFFF })
    }

    // -- acquisition (AVL_FE_LockChannel / AVL62X1_LockTP) --------------------------
    pub fn halt(&mut self, bridge: &mut Bridge) -> Result<bool> {
        self.send_cmd(bridge, CMD_HALT)
    }

    pub fn clear_stream_state(&mut self, bridge: &mut Bridge) -> Result<()> {
        self.write8(bridge, REG_SP_PLP_ID, 0)?;
        self.write16(bridge, REG_SP_T2MI_PID_CUR, 0)
    }

    pub fn lock_tp(&mut self, bridge: &mut Bridge, symbol_rate_hz: u32, carrier_offset_hz: i32) -> Result<bool> {
        if !self.halt(bridge)? {
            return Err(Error::Demod("HALT before acquisition failed".into()));
        }
        self.write8(bridge, REG_BLIND_SYM_RATE_ENABLE, 0)?;
        self.write8(bridge, REG_BLIND_CFO_ENABLE, 1)?;
        self.write32(bridge, REG_NOM_SYMBOL_RATE, symbol_rate_hz)?;
        self.write32(bridge, REG_NOM_CARRIER_FREQ, carrier_offset_hz as u32)?;
        self.write8(bridge, REG_SP_STREAM_TYPE, 0xFF)?; // no stream selection
        self.write8(bridge, REG_SP_WANTED_STREAM_ID, 0)?;
        self.send_cmd(bridge, CMD_ACQUIRE)
    }

    pub fn lock_flags(&self, bridge: &mut Bridge) -> Result<(u8, u8)> {
        Ok((self.read8(bridge, REG_LOCK_STATUS)?, self.read8(bridge, REG_FEC_LOCK)?))
    }

    pub fn rf_agc_gain(&self, bridge: &mut Bridge) -> Result<u16> {
        self.read16(bridge, REG_RFAGC_GAIN)
    }

    pub fn acq_status(&self, bridge: &mut Bridge) -> Result<AcqStatus> {
        let (lock, fec) = self.lock_flags(bridge)?;
        let rfagc = self.rf_agc_gain(bridge)?;
        Ok(AcqStatus {
            lock,
            fec,
            rfagc,
            strength_pct: rf_strength_pct(rfagc),
            snr_db: self.read16(bridge, REG_SNR_X100)? as i16 as f64 / 100.0,
            acq_retries: self.read8(bridge, REG_ACQ_RETRY)?,
            symbol_rate_hz: self.read32(bridge, REG_SYMBOL_RATE)?,
            carrier_hz: self.read32(bridge, REG_CARRIER_FREQ)? as i32,
            carrier_err_hz: self.read32(bridge, REG_CARRIER_FREQ_ERR)? as i32,
        })
    }

    /// Read-only snapshot of the tuner I2C block.
    pub fn tuner_i2c_diag(&self, bridge: &mut Bridge) -> Result<Vec<(u32, u32)>> {
        TUNER_I2C_DIAG.iter().map(|&address| Ok((address, self.read32(bridge, address)?))).collect()
    }
}

/// The script's view of the demodulator: its waits are capped (the script
/// asks for a million polls).
struct PatchTarget<'x, 'a> {
    demod: &'x mut Avl62x1,
    bridge: &'x mut Bridge<'a>,
    polls: u32,
}

impl patch::Target for PatchTarget<'_, '_> {
    fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>> {
        self.demod.read(self.bridge, address, length)
    }
    fn write(&mut self, address: u32, data: &[u8]) -> Result<()> {
        self.demod.write(self.bridge, address, data)
    }
    fn send_cmd(&mut self, cmd: u16) -> Result<bool> {
        self.demod.send_cmd(self.bridge, cmd)
    }
    fn delay(&mut self, ms: u64) -> Result<()> {
        self.polls += 1;
        if self.polls > self.demod.max_patch_polls {
            return Err(Error::Demod(format!("patch poll budget ({}) exhausted", self.demod.max_patch_polls)));
        }
        self.bridge.delay(ms);
        Ok(())
    }
    fn log(&mut self, line: String) {
        self.bridge.log.line(line);
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Carrier {
    pub tuner_rf_hz: u32,
    pub lpf_hz: u32,
    pub carrier_offset_hz: i32,
}

/// Kanky AVL62X1_Optimize_Carrier with RDA5815M limits (4..40 MHz LPF, no
/// tuner offset callback: 250 kHz step, 1 MHz LPF margin). Below 15 MS/s the
/// tuner is placed bw/2 below the carrier, which keeps the carrier off DC.
pub fn optimize_carrier(rf_khz: u32, symbol_rate_hz: u32) -> Carrier {
    let mut bw: i64 = (symbol_rate_hz as i64 / 100) * 0x87 + (250_000 >> 1) + 5_000_000;
    let mut offset_hz = 0i64;
    let mut tuner_khz = rf_khz as i64;
    if symbol_rate_hz < 15_000_000 {
        tuner_khz = rf_khz as i64 - bw / 2000;
        offset_hz = bw >> 1;
        bw *= 2;
    }
    let lpf = ((bw >> 1) + (1_000_000 >> 1) + 2_500_000).clamp(4_000_000, 40_000_000);
    Carrier { tuner_rf_hz: (tuner_khz * 1000) as u32, lpf_hz: lpf as u32, carrier_offset_hz: offset_hz as i32 }
}

pub fn rf_strength_pct(gain: u16) -> u32 {
    if gain <= RFAGC_STRONG {
        return 100;
    }
    if gain >= RFAGC_WEAK {
        return 10;
    }
    (RFAGC_WEAK - gain) as u32 * 90 / (RFAGC_WEAK - RFAGC_STRONG) as u32 + 10
}

/// A coarse verdict from the samples taken after ACQUIRE.
pub fn classify_acquisition(trace: &[AcqStatus]) -> &'static str {
    if trace.is_empty() {
        "NO_SAMPLES"
    } else if trace.iter().any(|sample| sample.fec != 0) {
        "FEC_LOCK"
    } else if trace.iter().any(|sample| sample.rfagc < RFAGC_WEAK) {
        "RF_NO_LOCK"
    } else {
        "NO_RF"
    }
}

/// 10 ms polls of the FEC lock flag, as AVL_FE_LockChannel.
pub fn fec_poll_budget(symbol_rate_ksps: u32) -> u32 {
    if symbol_rate_ksps > 10000 {
        70
    } else if symbol_rate_ksps > 5000 {
        100
    } else {
        150
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_carrier_plan_for_12188_v_27500_is_the_reference_s() {
        // tune-hw-lnb-20261022-003017.log: CARRIER 1588000 kHz SR 27500 kS/s
        let carrier = optimize_carrier(1_588_000, 27_500_000);
        let bw = 275_000 * 0x87 + 125_000 + 5_000_000;
        assert_eq!(carrier, Carrier { tuner_rf_hz: 1_588_000_000, lpf_hz: (bw / 2 + 3_000_000).min(40_000_000), carrier_offset_hz: 0 });
    }

    #[test]
    fn the_lnb_pins_and_the_diseqc_block_can_never_be_written() {
        let demod = Avl62x1::new([Scope::Agc]);
        for address in [LNB_CNTRL_0_SEL, LNB_CNTRL_1_SEL] {
            assert!(demod.check_write(address, 4).is_err(), "LNB pin 0x{address:06X}");
            assert!(demod.check_write(address, 1).is_err());
        }
        // A wider write that starts outside and reaches into the pins.
        assert!(demod.check_write(0x120004, 8).is_err());
        for address in (0x16C000..=0x16C0FF).step_by(4) {
            assert!(demod.check_write(address, 4).is_err(), "DiSEqC 0x{address:06X}");
        }
        // What the tuner needs is still there.
        assert!(demod.check_write(GPIO_M3_SCL_SEL, 4).is_ok());
        assert!(demod.check_write(AGC1_SEL, 4).is_ok());
        assert!(Avl62x1::new([]).check_write(AGC1_SEL, 4).is_err());
    }

    #[test]
    fn strength_follows_upstream() {
        assert_eq!(rf_strength_pct(65535), 10);
        assert_eq!(rf_strength_pct(20000), 100);
        assert_eq!(rf_strength_pct(40000), 55);
    }
}
