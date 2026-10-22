//! The AVL62X1 firmware patch script: header and interpreter.
//!
//! Format and semantics follow __avl62x1_exec_patchscript() of the GPL-2.0
//! Availink driver (avl62x1_lib.c); Kanky's IBase_DownloadPatch_AVL62X1
//! implements the same 14 opcodes, and kankylink/avl_patch.py is the
//! reference this mirrors step for step. All words are big-endian 32-bit.

use crate::error::{Error, Result};

const CMD_VALIDATE_CRC: u32 = 0;
const CMD_PING: u32 = 1;
const CMD_LD_TO_DEVICE: u32 = 2;
const CMD_DMA: u32 = 3;
const CMD_EXTRACT: u32 = 4;
const CMD_ASSERT_CPU_RESET: u32 = 5;
const CMD_RELEASE_CPU_RESET: u32 = 6;
const CMD_LD_TO_DEVICE_IMM: u32 = 7;
const CMD_RD_FROM_DEVICE: u32 = 8;
const CMD_DMA_HW: u32 = 9;
const CMD_SET_COND_IMM: u32 = 10;
const CMD_EXIT: u32 = 11;
const CMD_POLL_WAIT: u32 = 12;
const CMD_LD_TO_DEVICE_PACKED: u32 = 13;

const NAMES: [&str; 14] = [
    "VALIDATE_CRC", "PING", "LD_TO_DEVICE", "DMA", "EXTRACT", "ASSERT_CPU_RESET", "RELEASE_CPU_RESET",
    "LD_TO_DEVICE_IMM", "RD_FROM_DEVICE", "DMA_HW", "SET_COND_IMM", "EXIT", "POLL_WAIT", "LD_TO_DEVICE_PACKED",
];

const OP_ADDR_MODE_VAR_IDX: u32 = 0;
const OP_UNARY_LOGICAL_NEGATE: u32 = 1;
const OP_UNARY_BITWISE_NEGATE: u32 = 2;
const OP_BINARY_STORE: u32 = 6;

// Demod firmware commands sent through c_AVL62X1_DMD_command_saddr.
const FW_CMD_DMA: u16 = 21;
const FW_CMD_CALC_CRC: u16 = 22;
const FW_CMD_PING: u16 = 23;
const FW_CMD_DECOMPRESS: u16 = 24;

const REG_FW_COMMAND_ARGS_ADDR: u32 = 0x000258;
const REG_CPUCORE_SRST: u32 = 0x110840;
const REG_DMA_SYS_STATUS: u32 = 0x110048;
const REG_DMA_SYS_CMD: u32 = 0x110050;

const VAR_ARRAY_SIZE: usize = 32;

fn word(data: &[u8], index: usize) -> Result<u32> {
    data.get(index..index + 4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or_else(|| Error::Patch(format!("patch read beyond end at byte {index}")))
}

fn slice(data: &[u8], start: usize, length: usize) -> Result<&[u8]> {
    data.get(start..start + length)
        .ok_or_else(|| Error::Patch(format!("patch data {start}+{length} beyond end")))
}

#[derive(Debug, Clone)]
pub struct Header {
    pub total_words: u32,
    pub standard: u32,
    pub args_addr: u32,
    /// In words.
    pub data_section_offset: u32,
    pub reserved: Vec<u32>,
    /// In words.
    pub script_start: u32,
    pub script_len: u32,
}

impl Header {
    /// (major, minor, build) the patch says it is.
    pub fn version(&self) -> (u32, u32, u32) {
        let word = self.reserved.first().copied().unwrap_or(0);
        (word >> 24, (word >> 16) & 0xFF, word & 0xFFFF)
    }
}

pub fn parse_header(data: &[u8]) -> Result<Header> {
    let mut index = 4;
    let mut words = [0u32; 5];
    for slot in &mut words {
        *slot = word(data, index)?;
        index += 4;
    }
    let [total, standard, args_addr, data_offset, reserved_len] = words;
    let reserved = (0..reserved_len as usize).map(|i| word(data, index + 4 * i)).collect::<Result<Vec<_>>>()?;
    index += 4 * reserved_len as usize;
    let script_len = word(data, index)?;
    index += 4;
    if (index / 4) as u64 + script_len as u64 != data_offset as u64 {
        return Err(Error::Patch("script length does not match data section offset".into()));
    }
    if total as usize * 4 != data.len() {
        return Err(Error::Patch(format!("header length {} != blob length {}", total as usize * 4, data.len())));
    }
    Ok(Header {
        total_words: total,
        standard,
        args_addr,
        data_section_offset: data_offset,
        reserved,
        script_start: (index / 4) as u32,
        script_len,
    })
}

/// What the script runs against: the demodulator's memory, its firmware
/// command channel, and time (with a cap on how many waits it may ask for).
pub trait Target {
    fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>>;
    fn write(&mut self, address: u32, data: &[u8]) -> Result<()>;
    fn send_cmd(&mut self, cmd: u16) -> Result<bool>;
    fn delay(&mut self, ms: u64) -> Result<()>;
    fn log(&mut self, line: String);
}

pub struct Executor<'d> {
    data: &'d [u8],
    header: Header,
    var: [u64; VAR_ARRAY_SIZE],
}

impl<'d> Executor<'d> {
    pub fn new(data: &'d [u8]) -> Result<Self> {
        Ok(Self { data, header: parse_header(data)?, var: [0; VAR_ARRAY_SIZE] })
    }

    fn var_mut(&mut self, index: u32) -> Result<&mut u64> {
        self.var.get_mut(index as usize).ok_or_else(|| Error::Patch(format!("variable index {index} out of range")))
    }

    fn read_n(target: &mut dyn Target, address: u32, length: usize) -> Result<u64> {
        if length > 8 {
            return Err(Error::Patch(format!("read of {length} bytes is wider than a value")));
        }
        Ok(target.read(address, length)?.iter().fold(0u64, |value, &byte| value << 8 | byte as u64))
    }

    fn write_n(target: &mut dyn Target, address: u32, length: usize, value: u64) -> Result<()> {
        let bytes = value.to_be_bytes();
        target.write(address, &bytes[8 - length..])
    }

    fn fw_cmd(target: &mut dyn Target, cmd: u16) -> Result<()> {
        if !target.send_cmd(cmd)? {
            return Err(Error::Patch(format!("firmware command {cmd} failed or timed out")));
        }
        Ok(())
    }

    fn poll(target: &mut dyn Target, address: u32, done: impl Fn(u64) -> bool, label: &str) -> Result<()> {
        for _ in 0..21 {
            target.delay(10)?;
            if done(Self::read_n(target, address, 4)?) {
                return Ok(());
            }
        }
        Err(Error::Patch(format!("{label} timeout at {address:#x}")))
    }

    /// Run the script; the number of commands executed.
    pub fn run(&mut self, target: &mut dyn Target) -> Result<u32> {
        let d = self.data;
        let header = self.header.clone();
        let data_offset = header.data_section_offset as usize;
        let mut idx = header.script_start as usize * 4;
        let end = (header.script_start + header.script_len) as usize * 4;
        let mut executed = 0;
        while idx < end {
            let num_cmd_words = word(d, idx)? as usize;
            idx += 4;
            if num_cmd_words == 0 {
                return Err(Error::Patch(format!("zero-length command at byte {}", idx - 4)));
            }
            let next_idx = idx + (num_cmd_words - 1) * 4;
            let num_cond = word(d, idx)?;
            idx += 4;
            let mut condition: u64 = 1;
            for _ in 0..num_cond {
                let operation = word(d, idx)?;
                let mut value = word(d, idx + 4)? as u64;
                idx += 8;
                let (unary, binary, mode) = ((operation >> 8) & 0xFF, operation & 0xFF, (operation >> 16) & 3);
                if mode == OP_ADDR_MODE_VAR_IDX && binary != OP_BINARY_STORE {
                    value = *self.var_mut(value as u32)?;
                }
                if unary == OP_UNARY_LOGICAL_NEGATE {
                    value = (value == 0) as u64;
                } else if unary == OP_UNARY_BITWISE_NEGATE {
                    value = !value & 0xFFFF_FFFF;
                }
                condition = match binary {
                    0 => value,
                    OP_BINARY_STORE => {
                        *self.var_mut(value as u32)? = condition;
                        condition
                    }
                    1 => (condition != 0 && value != 0) as u64,
                    2 => (condition != 0 || value != 0) as u64,
                    3 => condition & value,
                    4 => condition | value,
                    5 => (condition == value) as u64,
                    7 => (condition != value) as u64,
                    other => return Err(Error::Patch(format!("unknown binary op {other}"))),
                };
            }
            if condition == 0 {
                idx = next_idx;
                continue;
            }

            let cmd = word(d, idx)?;
            idx += 4;
            executed += 1;
            target.log(format!("PATCH @{:05x} {}", idx - 4, NAMES.get(cmd as usize).copied().unwrap_or("?")));
            // Every argument is read before its command moves `idx` on.
            let at = idx;
            let w = |n: usize| word(d, at + 4 * n);

            match cmd {
                CMD_PING => {
                    Self::fw_cmd(target, FW_CMD_PING)?;
                    *self.var_mut(w(1)?)? = 1;
                    idx += 4 * (1 + w(0)? as usize);
                }
                CMD_VALIDATE_CRC => {
                    let (exp, start, length) = (w(0)?, w(1)?, w(2)?);
                    Self::write_n(target, REG_FW_COMMAND_ARGS_ADDR, 4, header.args_addr as u64)?;
                    Self::write_n(target, header.args_addr, 4, start as u64)?;
                    Self::write_n(target, header.args_addr + 4, 4, length as u64)?;
                    Self::fw_cmd(target, FW_CMD_CALC_CRC)?;
                    let crc = Self::read_n(target, header.args_addr + 8, 4)?;
                    *self.var_mut(w(4)?)? = (crc == exp as u64) as u64;
                    target.log(format!("PATCH   CRC start={start:#x} len={length:#x} exp={exp:#010x} got={crc:#010x}"));
                    idx += 4 * (3 + 1 + w(3)? as usize);
                }
                CMD_LD_TO_DEVICE => {
                    let (words, dest, src) = (w(0)? as usize, w(1)?, (w(2)? as usize + data_offset) * 4);
                    target.write(dest, slice(d, src, words * 4)?)?;
                    idx += 4 * 3;
                    idx += 4 * (1 + word(d, idx)? as usize);
                }
                CMD_LD_TO_DEVICE_IMM => {
                    let (length, dest, value) = (w(0)? as usize, w(1)?, w(2)?);
                    if matches!(length, 1 | 2 | 4) {
                        Self::write_n(target, dest, length, value as u64)?;
                    }
                    idx += 4 * 3;
                    idx += 4 * (1 + word(d, idx)? as usize);
                }
                CMD_LD_TO_DEVICE_PACKED => {
                    let src = (w(1)? as usize + data_offset) * 4 + 2;
                    let head = slice(d, src, 6)?;
                    let records = (head[0] as usize) << 8 | head[1] as usize;
                    let dest = u32::from_be_bytes([head[2], head[3], head[4], head[5]]);
                    let mut pos = src + 6;
                    for _ in 0..records {
                        let record = slice(d, pos, 4)?;
                        let offset = u16::from_be_bytes([record[0], record[1]]) as u32;
                        let length = u16::from_be_bytes([record[2], record[3]]) as usize;
                        pos += 4;
                        target.write(dest + offset, slice(d, pos, length)?)?;
                        pos += length;
                    }
                    idx += 4 * 2;
                    idx += 4 * (1 + word(d, idx)? as usize);
                }
                CMD_RD_FROM_DEVICE => {
                    let (length, src, num_rvs, rv0) = (w(0)? as usize, w(1)?, w(2)? as usize, w(3)?);
                    if matches!(length, 1 | 2 | 4) {
                        let value = Self::read_n(target, src, length)?;
                        *self.var_mut(rv0)? = value;
                    }
                    idx += 4 * (3 + num_rvs);
                }
                CMD_DMA => {
                    let (descr, num) = (w(0)?, w(1)? as usize);
                    idx += 8;
                    // Vendor and upstream both pass num*12 including the 3
                    // address bytes, so the last 3 descriptor bytes are not sent.
                    target.write(descr, slice(d, idx, (num * 12).saturating_sub(3))?)?;
                    idx += 12 * num;
                    Self::write_n(target, REG_FW_COMMAND_ARGS_ADDR, 4, descr as u64)?;
                    Self::fw_cmd(target, FW_CMD_DMA)?;
                    idx += 4 * (1 + word(d, idx)? as usize);
                }
                CMD_EXTRACT => {
                    let (kind, src, dest) = (w(0)?, w(1)?, w(2)?);
                    Self::write_n(target, REG_FW_COMMAND_ARGS_ADDR, 4, header.args_addr as u64)?;
                    for (i, value) in [kind, src, dest].into_iter().enumerate() {
                        Self::write_n(target, header.args_addr + 4 * i as u32, 4, value as u64)?;
                    }
                    Self::fw_cmd(target, FW_CMD_DECOMPRESS)?;
                    idx += 4 * 3;
                    idx += 4 * (1 + word(d, idx)? as usize);
                }
                CMD_ASSERT_CPU_RESET | CMD_RELEASE_CPU_RESET => {
                    let value = (cmd == CMD_ASSERT_CPU_RESET) as u64;
                    Self::write_n(target, REG_CPUCORE_SRST, 4, value)?;
                    idx += 4 * (1 + w(0)? as usize);
                }
                CMD_DMA_HW => {
                    let (descr, num) = (w(0)?, w(1)? as usize);
                    idx += 8;
                    if num > 0 {
                        target.write(descr, slice(d, idx, num * 12)?)?;
                    }
                    idx += 12 * num;
                    Self::poll(target, REG_DMA_SYS_STATUS, |status| status & 1 != 0, "DMA ready")?;
                    Self::write_n(target, REG_DMA_SYS_CMD, 4, descr as u64)?;
                    Self::poll(target, REG_DMA_SYS_STATUS, |status| status & 0x100 == 0, "DMA done")?;
                    idx += 4 * (1 + word(d, idx)? as usize);
                }
                CMD_SET_COND_IMM => {
                    let value = w(0)? as u64;
                    *self.var_mut(w(2)?)? = value;
                    idx += 4 * (2 + w(1)? as usize);
                }
                CMD_EXIT => return Ok(executed),
                CMD_POLL_WAIT => {
                    let (length, src, wanted, max_polls) = (w(0)? as usize, w(1)?, w(2)? as u64, w(3)?);
                    let mut matched = false;
                    for _ in 0..max_polls.max(1) {
                        if Self::read_n(target, src, length)? == wanted {
                            matched = true;
                            break;
                        }
                        target.delay(10)?;
                    }
                    if !matched {
                        return Err(Error::Patch(format!("POLL_WAIT {src:#x} != {wanted:#x} after {max_polls} polls")));
                    }
                    idx += 4 * 4;
                    idx += 4 * (1 + word(d, idx)? as usize);
                }
                other => return Err(Error::Patch(format!("unknown patch command {other}"))),
            }
        }
        Ok(executed)
    }
}
