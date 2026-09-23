//! Linux HDMI-CEC UAPI controller. No libCEC dependency is used.

use mediabox_core::{CecDevice, CecErrorCounters, CecEvent, CecStatus, InputAction};
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

const CEC_CAP_LOG_ADDRS: u32 = 1 << 1;
const CEC_CAP_TRANSMIT: u32 = 1 << 2;
const CEC_CAP_PASSTHROUGH: u32 = 1 << 3;
const CEC_CAP_RC: u32 = 1 << 4;
const CEC_CAP_MONITOR_ALL: u32 = 1 << 5;
const CEC_CAP_NEEDS_HPD: u32 = 1 << 6;
const CEC_MODE_INITIATOR: u32 = 1;
const CEC_MODE_EXCL_FOLLOWER: u32 = 2 << 4;
const CEC_LOG_ADDR_TYPE_PLAYBACK: u8 = 3;
const CEC_OP_PRIM_DEVTYPE_PLAYBACK: u8 = 4;
const CEC_OP_ALL_DEVTYPE_PLAYBACK: u8 = 1 << 3;
const CEC_VERSION_2_0: u8 = 6;
const CEC_VENDOR_ID_NONE: u32 = 0xffff_ffff;
const CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU: u32 = 1 << 1;
const CEC_PHYS_ADDR_INVALID: u16 = 0xffff;
const CEC_TX_STATUS_OK: u8 = 1;
const CEC_TX_STATUS_ARB_LOST: u8 = 1 << 1;
const CEC_TX_STATUS_NACK: u8 = 1 << 2;
const CEC_TX_STATUS_LOW_DRIVE: u8 = 1 << 3;
const CEC_MSG_IMAGE_VIEW_ON: u8 = 0x04;
const CEC_MSG_STANDBY: u8 = 0x36;
const CEC_MSG_USER_CONTROL_PRESSED: u8 = 0x44;
const CEC_MSG_USER_CONTROL_RELEASED: u8 = 0x45;
const CEC_MSG_ACTIVE_SOURCE: u8 = 0x82;
const CEC_MSG_REPORT_PHYSICAL_ADDR: u8 = 0x84;

const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;
const fn ioc(direction: u64, number: u64, size: usize) -> libc::c_ulong {
    ((direction << 30) | ((b'a' as u64) << 8) | number | ((size as u64) << 16)) as _
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawCaps {
    driver: [libc::c_char; 32],
    name: [libc::c_char; 32],
    available_log_addrs: u32,
    capabilities: u32,
    version: u32,
}

impl Default for RawCaps {
    fn default() -> Self {
        // SAFETY: all-zero is valid for this plain C UAPI struct.
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawLogAddrs {
    log_addr: [u8; 4],
    log_addr_mask: u16,
    cec_version: u8,
    num_log_addrs: u8,
    vendor_id: u32,
    flags: u32,
    osd_name: [libc::c_char; 15],
    primary_device_type: [u8; 4],
    log_addr_type: [u8; 4],
    all_device_types: [u8; 4],
    features: [[u8; 12]; 4],
}

impl Default for RawLogAddrs {
    fn default() -> Self {
        // SAFETY: all-zero is valid and then all caller-controlled fields are assigned.
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct RawMessage {
    tx_ts: u64,
    rx_ts: u64,
    len: u32,
    timeout: u32,
    sequence: u32,
    flags: u32,
    msg: [u8; 16],
    reply: u8,
    rx_status: u8,
    tx_status: u8,
    tx_arb_lost_cnt: u8,
    tx_nack_cnt: u8,
    tx_low_drive_cnt: u8,
    tx_error_cnt: u8,
}

impl Default for RawMessage {
    fn default() -> Self {
        // SAFETY: all-zero is the documented initializer for cec_msg.
        unsafe { std::mem::zeroed() }
    }
}

const CEC_ADAP_G_CAPS: libc::c_ulong = ioc(IOC_READ | IOC_WRITE, 0, std::mem::size_of::<RawCaps>());
const CEC_ADAP_G_PHYS_ADDR: libc::c_ulong = ioc(IOC_READ, 1, std::mem::size_of::<u16>());
const CEC_ADAP_G_LOG_ADDRS: libc::c_ulong = ioc(IOC_READ, 3, std::mem::size_of::<RawLogAddrs>());
const CEC_ADAP_S_LOG_ADDRS: libc::c_ulong =
    ioc(IOC_READ | IOC_WRITE, 4, std::mem::size_of::<RawLogAddrs>());
const CEC_TRANSMIT: libc::c_ulong = ioc(IOC_READ | IOC_WRITE, 5, std::mem::size_of::<RawMessage>());
const CEC_RECEIVE: libc::c_ulong = ioc(IOC_READ | IOC_WRITE, 6, std::mem::size_of::<RawMessage>());
const CEC_S_MODE: libc::c_ulong = ioc(IOC_WRITE, 9, std::mem::size_of::<u32>());

#[derive(Debug, Error)]
pub enum CecError {
    #[error("CEC adaptörü bulunamadı")]
    NotFound,
    #[error("CEC adaptörü başka bir süreç tarafından kullanılıyor: {0}")]
    Busy(String),
    #[error("CEC UAPI işlemi başarısız ({operation}): {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("CEC adaptörü gerekli yeteneği sunmuyor: {0}")]
    MissingCapability(&'static str),
    #[error("CEC fiziksel adresi HDMI bağlantısı tarafından henüz sağlanmadı")]
    NoPhysicalAddress,
    #[error("geçersiz CEC mesajı: {0}")]
    InvalidMessage(&'static str),
    #[error("CEC iletimi onaylanmadı (tx_status=0x{0:02x})")]
    Transmit(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMessage {
    pub event: CecEvent,
    pub input: Option<(InputAction, bool)>,
    pub reported_physical_address: Option<(u16, u8)>,
}

pub type ReceivedMessage = (ParsedMessage, Option<(InputAction, bool)>);

pub fn parse_message(bytes: &[u8], timestamp_ns: u64) -> Result<ParsedMessage, CecError> {
    if bytes.is_empty() || bytes.len() > 16 {
        return Err(CecError::InvalidMessage("uzunluk 1..16 olmalı"));
    }
    let initiator = bytes[0] >> 4;
    let destination = bytes[0] & 0x0f;
    let opcode = bytes.get(1).copied();
    let operands = bytes.get(2..).unwrap_or_default().to_vec();
    let input = match opcode {
        Some(CEC_MSG_USER_CONTROL_PRESSED) => {
            let key = *bytes.get(2).ok_or(CecError::InvalidMessage(
                "User Control Pressed operand içermiyor",
            ))?;
            cec_key_to_action(key).map(|action| (action, true))
        }
        Some(CEC_MSG_USER_CONTROL_RELEASED) => None,
        _ => None,
    };
    let reported_physical_address = if opcode == Some(CEC_MSG_REPORT_PHYSICAL_ADDR) {
        if bytes.len() != 5 {
            return Err(CecError::InvalidMessage(
                "Report Physical Address uzunluğu 5 olmalı",
            ));
        }
        Some((((bytes[2] as u16) << 8) | bytes[3] as u16, bytes[4]))
    } else {
        None
    };
    Ok(ParsedMessage {
        event: CecEvent {
            initiator,
            destination,
            opcode,
            operands,
            timestamp_ns,
        },
        input,
        reported_physical_address,
    })
}

pub fn cec_key_to_action(key: u8) -> Option<InputAction> {
    Some(match key {
        0x00 => InputAction::Ok,
        0x01 => InputAction::Up,
        0x02 => InputAction::Down,
        0x03 => InputAction::Left,
        0x04 => InputAction::Right,
        0x09 => InputAction::Home,
        0x0d => InputAction::Back,
        0x44 => InputAction::Play,
        0x45 => InputAction::Stop,
        0x46 => InputAction::Pause,
        0x48 | 0x4c => InputAction::SeekBackward,
        0x49 | 0x4b => InputAction::SeekForward,
        0x41 => InputAction::VolumeUp,
        0x42 => InputAction::VolumeDown,
        0x43 | 0x65 => InputAction::Mute,
        0x40 | 0x6b..=0x6d => InputAction::Power,
        _ => return None,
    })
}

pub fn active_source_message(logical: u8, physical: u16) -> Vec<u8> {
    vec![
        (logical << 4) | 0x0f,
        CEC_MSG_ACTIVE_SOURCE,
        (physical >> 8) as u8,
        physical as u8,
    ]
}

pub fn wake_tv_message(logical: u8) -> Vec<u8> {
    vec![logical << 4, CEC_MSG_IMAGE_VIEW_ON]
}
pub fn standby_tv_message(logical: u8) -> Vec<u8> {
    vec![logical << 4, CEC_MSG_STANDBY]
}

#[derive(Debug)]
struct State {
    status: CecStatus,
    devices: BTreeMap<u8, CecDevice>,
    pressed: BTreeMap<u8, InputAction>,
}

pub struct Adapter {
    file: File,
    path: PathBuf,
    physical_address: u16,
    logical_address: u8,
    state: Mutex<State>,
}

impl Adapter {
    pub fn discover(preferred: Option<&Path>) -> Result<Arc<Self>, CecError> {
        let path = if let Some(path) = preferred {
            path.to_path_buf()
        } else {
            let mut devices: Vec<_> = std::fs::read_dir("/dev")
                .map_err(|source| CecError::Io {
                    operation: "adapter discovery",
                    source,
                })?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"cec"))
                })
                .collect();
            devices.sort();
            devices.into_iter().next().ok_or(CecError::NotFound)?
        };
        Self::open(&path)
    }

    pub fn open(path: &Path) -> Result<Arc<Self>, CecError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| CecError::Io {
                operation: "open",
                source,
            })?;
        let fd = file.as_raw_fd();
        let mut caps = RawCaps::default();
        ioctl(fd, CEC_ADAP_G_CAPS, &mut caps, "G_CAPS")?;
        if caps.capabilities & CEC_CAP_LOG_ADDRS == 0 {
            return Err(CecError::MissingCapability("logical addresses"));
        }
        if caps.capabilities & CEC_CAP_TRANSMIT == 0 {
            return Err(CecError::MissingCapability("transmit"));
        }
        let mut mode = CEC_MODE_INITIATOR | CEC_MODE_EXCL_FOLLOWER;
        if let Err(error) = ioctl(fd, CEC_S_MODE, &mut mode, "S_MODE") {
            return match &error {
                CecError::Io { source, .. } if source.raw_os_error() == Some(libc::EBUSY) => {
                    Err(CecError::Busy(path.display().to_string()))
                }
                _ => Err(error),
            };
        }
        let mut physical = 0u16;
        ioctl(fd, CEC_ADAP_G_PHYS_ADDR, &mut physical, "G_PHYS_ADDR")?;
        // No physical address is not a reason to give up on the adapter. The
        // HDMI driver sets it when a sink is plugged into this socket and
        // clears it when it is taken out, and the kernel claims the logical
        // address below on its own the moment one arrives. Refusing here is
        // what left a box that booted with the television off without CEC
        // until the next restart.
        //
        // A configuration left by a previous process is cleared first:
        // setting logical addresses on a configured adapter is refused.
        let mut existing = RawLogAddrs::default();
        if ioctl(fd, CEC_ADAP_G_LOG_ADDRS, &mut existing, "G_LOG_ADDRS").is_ok()
            && existing.num_log_addrs > 0
        {
            let mut clear = RawLogAddrs::default();
            ioctl(fd, CEC_ADAP_S_LOG_ADDRS, &mut clear, "S_LOG_ADDRS(clear)")?;
        }
        let mut addresses = RawLogAddrs {
            cec_version: CEC_VERSION_2_0,
            num_log_addrs: 1,
            vendor_id: CEC_VENDOR_ID_NONE,
            flags: CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU,
            ..Default::default()
        };
        for (dst, src) in addresses.osd_name.iter_mut().zip(b"MediaBox\0") {
            *dst = *src as libc::c_char;
        }
        addresses.primary_device_type[0] = CEC_OP_PRIM_DEVTYPE_PLAYBACK;
        addresses.log_addr_type[0] = CEC_LOG_ADDR_TYPE_PLAYBACK;
        addresses.all_device_types[0] = CEC_OP_ALL_DEVTYPE_PLAYBACK;
        if let Err(error) = ioctl(fd, CEC_ADAP_S_LOG_ADDRS, &mut addresses, "S_LOG_ADDRS") {
            return match &error {
                CecError::Io { source, .. } if source.raw_os_error() == Some(libc::EBUSY) => {
                    Err(CecError::Busy(path.display().to_string()))
                }
                _ => Err(error),
            };
        }
        let logical = addresses.log_addr[0];
        if physical != CEC_PHYS_ADDR_INVALID && logical > 14 {
            return Err(CecError::Busy("Playback mantıksal adresi alınamadı".into()));
        }
        let driver = c_string(&caps.driver);
        let name = c_string(&caps.name);
        let capabilities = capability_names(caps.capabilities);
        let status = CecStatus {
            available: true,
            adapter: Some(format!("{} ({})", path.display(), name)),
            driver: Some(driver),
            capabilities,
            physical_address: (physical != CEC_PHYS_ADDR_INVALID)
                .then(|| format_physical_address(physical)),
            logical_addresses: (logical <= 14).then_some(logical).into_iter().collect(),
            ..Default::default()
        };
        Ok(Arc::new(Self {
            file,
            path: path.to_path_buf(),
            physical_address: physical,
            logical_address: logical,
            state: Mutex::new(State {
                status,
                devices: BTreeMap::new(),
                pressed: BTreeMap::new(),
            }),
        }))
    }

    /// Whether a sink is on this adapter's socket and the adapter holds a
    /// logical address there -- the one adapter that can talk to the
    /// television.
    pub fn is_live(&self) -> bool {
        matches!(self.addresses(), (physical, Some(_)) if physical != CEC_PHYS_ADDR_INVALID)
    }

    /// For waiting on several adapters at once.
    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        self.file.as_raw_fd()
    }

    /// Every adapter on the board that can act as a playback device.
    ///
    /// One per HDMI transmitter on this SoC, and which of them has the
    /// television is the kernel's to say -- it sets a physical address on the
    /// socket a sink is plugged into -- so all of them are configured and the
    /// live one is asked for when something is sent.
    pub fn open_all() -> Vec<Arc<Self>> {
        let Ok(entries) = std::fs::read_dir("/dev") else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.as_encoded_bytes().starts_with(b"cec"))
            })
            .collect();
        paths.sort();
        paths
            .into_iter()
            .filter_map(|path| match Self::open(&path) {
                Ok(adapter) => Some(adapter),
                Err(error) => {
                    eprintln!("CEC {}: {error}", path.display());
                    None
                }
            })
            .collect()
    }

    /// The device node this adapter was opened on.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The physical and logical address as the kernel holds them now.
    ///
    /// Neither is a constant. The physical address is the television input
    /// the cable is in -- 1.0.0.0 on the Sony's HDMI 1, 4.0.0.0 on its HDMI 4
    /// -- and the kernel updates it on every hotplug and claims the logical
    /// address again. The values read at open were the ones this used to send
    /// forever, which is an Active Source naming an input the box is no longer
    /// on.
    pub fn addresses(&self) -> (u16, Option<u8>) {
        let fd = self.file.as_raw_fd();
        let mut physical = self.physical_address;
        let _ = ioctl(fd, CEC_ADAP_G_PHYS_ADDR, &mut physical, "G_PHYS_ADDR");
        let mut addresses = RawLogAddrs::default();
        let logical = match ioctl(fd, CEC_ADAP_G_LOG_ADDRS, &mut addresses, "G_LOG_ADDRS") {
            Ok(()) if addresses.num_log_addrs >= 1 && addresses.log_addr[0] <= 14 => {
                Some(addresses.log_addr[0])
            }
            Ok(()) => None,
            Err(_) => Some(self.logical_address),
        };
        (physical, logical)
    }

    fn logical_now(&self) -> Result<u8, CecError> {
        match self.addresses() {
            (physical, Some(logical)) if physical != CEC_PHYS_ADDR_INVALID => Ok(logical),
            _ => Err(CecError::NoPhysicalAddress),
        }
    }

    pub fn status(&self) -> CecStatus {
        let (physical, logical) = self.addresses();
        let mut state = self.state.lock().expect("CEC state lock");
        state.status.physical_address =
            (physical != CEC_PHYS_ADDR_INVALID).then(|| format_physical_address(physical));
        state.status.logical_addresses = logical.into_iter().collect();
        state.status.known_devices = state.devices.values().cloned().collect();
        state.status.clone()
    }

    pub fn receive(&self, timeout_ms: u32) -> Result<Option<ReceivedMessage>, CecError> {
        let mut raw = RawMessage {
            timeout: timeout_ms,
            ..Default::default()
        };
        let result = ioctl(self.file.as_raw_fd(), CEC_RECEIVE, &mut raw, "RECEIVE");
        if let Err(CecError::Io { source, .. }) = &result
            && matches!(source.raw_os_error(), Some(code) if code == libc::EAGAIN || code == libc::ETIMEDOUT)
        {
            return Ok(None);
        }
        if let Err(error) = result {
            self.state
                .lock()
                .expect("CEC state lock")
                .status
                .errors
                .receive += 1;
            return Err(error);
        }
        if raw.len == 0 {
            return Ok(None);
        }
        let parsed = parse_message(&raw.msg[..raw.len.min(16) as usize], raw.rx_ts)?;
        let mut state = self.state.lock().expect("CEC state lock");
        state.status.last_rx = Some(parsed.event.clone());
        let now = parsed.event.timestamp_ns;
        let device = state
            .devices
            .entry(parsed.event.initiator)
            .or_insert(CecDevice {
                logical_address: parsed.event.initiator,
                physical_address: None,
                device_type: None,
                last_seen_ns: now,
            });
        device.last_seen_ns = now;
        if let Some((physical, kind)) = parsed.reported_physical_address {
            device.physical_address = Some(format_physical_address(physical));
            device.device_type = Some(device_type_name(kind).to_string());
        }
        let normalized = match parsed.event.opcode {
            Some(CEC_MSG_USER_CONTROL_PRESSED) => parsed.input.map(|(action, pressed)| {
                state.pressed.insert(parsed.event.initiator, action);
                (action, pressed)
            }),
            Some(CEC_MSG_USER_CONTROL_RELEASED) => state
                .pressed
                .remove(&parsed.event.initiator)
                .map(|action| (action, false)),
            _ => None,
        };
        Ok(Some((parsed, normalized)))
    }

    pub fn active_source(&self) -> Result<(), CecError> {
        let logical = self.logical_now()?;
        let (physical, _) = self.addresses();
        self.transmit(&active_source_message(logical, physical))
    }
    pub fn wake_tv(&self) -> Result<(), CecError> {
        self.transmit(&wake_tv_message(self.logical_now()?))
    }
    pub fn standby_tv(&self) -> Result<(), CecError> {
        self.transmit(&standby_tv_message(self.logical_now()?))
    }

    pub fn discover_devices(&self) -> Result<Vec<CecDevice>, CecError> {
        let own = self.logical_now()?;
        for destination in 0..15u8 {
            if destination == own {
                continue;
            }
            let poll = [(own << 4) | destination];
            if self.transmit(&poll).is_ok() {
                let now = wallclock_ns();
                self.state
                    .lock()
                    .expect("CEC state lock")
                    .devices
                    .entry(destination)
                    .or_insert(CecDevice {
                        logical_address: destination,
                        physical_address: None,
                        device_type: None,
                        last_seen_ns: now,
                    });
                let _ = self.transmit(&[(own << 4) | destination, 0x83]);
            }
        }
        Ok(self.status().known_devices)
    }

    fn transmit(&self, bytes: &[u8]) -> Result<(), CecError> {
        if bytes.is_empty() || bytes.len() > 16 {
            return Err(CecError::InvalidMessage("iletim uzunluğu 1..16 olmalı"));
        }
        let mut raw = RawMessage {
            len: bytes.len() as u32,
            timeout: 1000,
            ..Default::default()
        };
        raw.msg[..bytes.len()].copy_from_slice(bytes);
        ioctl(self.file.as_raw_fd(), CEC_TRANSMIT, &mut raw, "TRANSMIT")?;
        let event = parse_message(bytes, raw.tx_ts.max(wallclock_ns()))?.event;
        let mut state = self.state.lock().expect("CEC state lock");
        state.status.last_tx = Some(event);
        state.status.errors.arbitration_lost += raw.tx_arb_lost_cnt as u64;
        state.status.errors.nack += raw.tx_nack_cnt as u64;
        state.status.errors.low_drive += raw.tx_low_drive_cnt as u64;
        state.status.errors.transmit += raw.tx_error_cnt as u64;
        if raw.tx_status & CEC_TX_STATUS_OK == 0 {
            if raw.tx_status & CEC_TX_STATUS_ARB_LOST != 0 {
                state.status.errors.arbitration_lost += 1;
            }
            if raw.tx_status & CEC_TX_STATUS_NACK != 0 {
                state.status.errors.nack += 1;
            }
            if raw.tx_status & CEC_TX_STATUS_LOW_DRIVE != 0 {
                state.status.errors.low_drive += 1;
            }
            state.status.errors.transmit += 1;
            return Err(CecError::Transmit(raw.tx_status));
        }
        Ok(())
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        let mut clear = RawLogAddrs::default();
        // Best-effort: releasing all logical addresses is the kernel-defined clean shutdown.
        let _ = unsafe { libc::ioctl(self.file.as_raw_fd(), CEC_ADAP_S_LOG_ADDRS, &mut clear) };
    }
}

fn ioctl<T>(
    fd: libc::c_int,
    request: libc::c_ulong,
    value: &mut T,
    operation: &'static str,
) -> Result<(), CecError> {
    // SAFETY: request codes and repr(C) payloads match linux/cec.h; fd stays open for this call.
    if unsafe { libc::ioctl(fd, request, value) } < 0 {
        Err(CecError::Io {
            operation,
            source: io::Error::last_os_error(),
        })
    } else {
        Ok(())
    }
}

fn c_string(value: &[libc::c_char]) -> String {
    // Kernel guarantees NUL termination of cec_caps strings; the buffer itself bounds the read.
    let bytes: Vec<u8> = value.iter().map(|item| *item as u8).collect();
    CStr::from_bytes_until_nul(&bytes)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn capability_names(value: u32) -> Vec<String> {
    [
        (CEC_CAP_LOG_ADDRS, "logical-addresses"),
        (CEC_CAP_TRANSMIT, "transmit"),
        (CEC_CAP_PASSTHROUGH, "passthrough"),
        (CEC_CAP_RC, "remote-control"),
        (CEC_CAP_MONITOR_ALL, "monitor-all"),
        (CEC_CAP_NEEDS_HPD, "needs-hpd"),
    ]
    .into_iter()
    .filter(|(bit, _)| value & bit != 0)
    .map(|(_, name)| name.to_string())
    .collect()
}

pub fn format_physical_address(value: u16) -> String {
    format!(
        "{}.{}.{}.{}",
        value >> 12,
        (value >> 8) & 0xf,
        (value >> 4) & 0xf,
        value & 0xf
    )
}

fn device_type_name(value: u8) -> &'static str {
    match value {
        0 => "tv",
        1 => "recording",
        3 => "tuner",
        4 => "playback",
        5 => "audio-system",
        _ => "other",
    }
}

fn wallclock_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

pub fn unavailable_status(path: Option<&Path>, error: &CecError) -> CecStatus {
    CecStatus {
        adapter: path.map(|p| p.display().to_string()),
        error: Some(error.to_string()),
        errors: CecErrorCounters::default(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_mapping_covers_product_remote() {
        assert_eq!(cec_key_to_action(0x01), Some(InputAction::Up));
        assert_eq!(cec_key_to_action(0x02), Some(InputAction::Down));
        assert_eq!(cec_key_to_action(0x03), Some(InputAction::Left));
        assert_eq!(cec_key_to_action(0x04), Some(InputAction::Right));
        assert_eq!(cec_key_to_action(0x00), Some(InputAction::Ok));
        assert_eq!(cec_key_to_action(0x0d), Some(InputAction::Back));
        assert_eq!(cec_key_to_action(0x44), Some(InputAction::Play));
        assert_eq!(cec_key_to_action(0x46), Some(InputAction::Pause));
        assert_eq!(cec_key_to_action(0x45), Some(InputAction::Stop));
    }

    #[test]
    fn parses_user_control_and_topology() {
        let parsed = parse_message(&[0x04, 0x44, 0x01], 7).unwrap();
        assert_eq!(parsed.input, Some((InputAction::Up, true)));
        let report = parse_message(&[0x0f, 0x84, 0x00, 0x00, 0x00], 8).unwrap();
        assert_eq!(report.reported_physical_address, Some((0, 0)));
    }

    #[test]
    fn invalid_messages_fail_closed() {
        assert!(parse_message(&[], 0).is_err());
        assert!(parse_message(&[0x04, 0x44], 0).is_err());
        assert!(parse_message(&[0x0f, 0x84, 0x10], 0).is_err());
    }

    #[test]
    fn transmit_construction_is_exact() {
        assert_eq!(active_source_message(4, 0x1000), [0x4f, 0x82, 0x10, 0x00]);
        assert_eq!(wake_tv_message(4), [0x40, 0x04]);
        assert_eq!(standby_tv_message(4), [0x40, 0x36]);
    }

    #[test]
    fn uapi_layout_matches_linux_header() {
        assert_eq!(std::mem::size_of::<RawCaps>(), 76);
        assert_eq!(std::mem::size_of::<RawLogAddrs>(), 92);
        assert_eq!(std::mem::size_of::<RawMessage>(), 56);
    }
}
