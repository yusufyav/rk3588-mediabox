//! A floor under a BLE peripheral's connection interval, as Android keeps one.
//!
//! A keyboard on this box asked for 7.5 ms and nothing longer (the ROG AZOTH:
//! MinInterval=MaxInterval=6 in bluez's record of it). Measured on the Plus
//! with an RTL8852BU: while it held that, the controller's ACL buffers to a
//! Bluetooth headset were full 98 % of the time against 7-19 % with the
//! keyboard away, and the headset went short of audio and cut in and out.
//!
//! Android raises a peripheral's request to at least 11.25 ms
//! (system/stack/l2cap/l2c_ble.cc `L2CA_AdjustConnectionIntervals`,
//! `BTM_BLE_CONN_INT_MIN_LIMIT` 0x0009), min and max both. Linux takes the
//! request as it is -- a floor was added in 2019 and reverted for the latency
//! it costs mice (commit 68d19d7d9957) -- so it is applied here instead: the
//! central may update a connection's parameters at any time, and does,
//! whenever an LE connection it is central of comes up or is updated below
//! the floor. A peripheral that asks again is raised again.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// 11.25 ms, in 1.25 ms units: Android's `BTM_BLE_CONN_INT_MIN_LIMIT`.
pub const FLOOR: u16 = 0x0009;

const AF_BLUETOOTH: libc::c_int = 31;
const BTPROTO_HCI: libc::c_int = 1;
const SOL_HCI: libc::c_int = 0;
const HCI_FILTER: libc::c_int = 2;
const HCI_CHANNEL_RAW: u16 = 0;
const HCI_COMMAND_PKT: u8 = 0x01;
const HCI_EVENT_PKT: u8 = 0x04;
const EVT_DISCONN_COMPLETE: u8 = 0x05;
const EVT_LE_META: u8 = 0x3e;
const LE_CONN_COMPLETE: u8 = 0x01;
const LE_CONN_UPDATE_COMPLETE: u8 = 0x03;
const LE_ENHANCED_CONN_COMPLETE: u8 = 0x0a;
/// HCI_LE_Connection_Update: OGF 0x08, OCF 0x0013.
const OP_LE_CONN_UPDATE: u16 = 0x2013;
const ROLE_CENTRAL: u8 = 0x00;

#[repr(C)]
struct SockaddrHci {
    family: libc::sa_family_t,
    dev: u16,
    channel: u16,
}

#[repr(C)]
struct HciFilter {
    type_mask: u32,
    event_mask: [u32; 2],
    opcode: u16,
}

/// What an HCI event says about an LE connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    /// Established, with the role this box has in it.
    Connected { handle: u16, central: bool, interval: u16, latency: u16, timeout: u16 },
    Updated { handle: u16, interval: u16, latency: u16, timeout: u16 },
    Gone { handle: u16 },
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

/// One HCI event packet, as read from the raw socket (packet type first).
fn parse(packet: &[u8]) -> Option<Event> {
    if *packet.first()? != HCI_EVENT_PKT {
        return None;
    }
    let code = *packet.get(1)?;
    let p = packet.get(3..)?;
    match code {
        EVT_DISCONN_COMPLETE if *p.first()? == 0 => Some(Event::Gone { handle: le16(p, 1)? & 0x0fff }),
        EVT_LE_META => {
            let status = *p.get(1)?;
            if status != 0 {
                return None;
            }
            match *p.first()? {
                // subevent, status, handle(2), role, peer type, peer(6), [local rpa(6), peer rpa(6)],
                // interval(2), latency(2), timeout(2)
                sub @ (LE_CONN_COMPLETE | LE_ENHANCED_CONN_COMPLETE) => {
                    let at = if sub == LE_ENHANCED_CONN_COMPLETE { 24 } else { 12 };
                    Some(Event::Connected {
                        handle: le16(p, 2)? & 0x0fff,
                        central: *p.get(4)? == ROLE_CENTRAL,
                        interval: le16(p, at)?,
                        latency: le16(p, at + 2)?,
                        timeout: le16(p, at + 4)?,
                    })
                }
                // subevent, status, handle(2), interval(2), latency(2), timeout(2)
                LE_CONN_UPDATE_COMPLETE => Some(Event::Updated {
                    handle: le16(p, 2)? & 0x0fff,
                    interval: le16(p, 4)?,
                    latency: le16(p, 6)?,
                    timeout: le16(p, 8)?,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// HCI_LE_Connection_Update raising `handle` to the floor, keeping the
/// peripheral's latency and supervision timeout -- the latency lowered if the
/// longer interval would break the rule the timeout has to satisfy:
/// timeout × 10 ms > (1 + latency) × interval × 2 (Core, Vol 6, Part B, 4.5.2).
fn raise(handle: u16, latency: u16, timeout: u16) -> Vec<u8> {
    // In 0.25 ms units to stay in integers: interval 1.25 ms = 5, timeout 10 ms = 40.
    let limit = u32::from(timeout) * 40;
    let mut latency = u32::from(latency);
    while latency > 0 && (1 + latency) * u32::from(FLOOR) * 5 * 2 >= limit {
        latency -= 1;
    }
    let mut command = vec![HCI_COMMAND_PKT];
    command.extend_from_slice(&OP_LE_CONN_UPDATE.to_le_bytes());
    command.push(14);
    for value in [handle, FLOOR, FLOOR, latency as u16, timeout, 0, 0] {
        command.extend_from_slice(&value.to_le_bytes());
    }
    command
}

/// Follow hci0's LE connections for as long as the daemon runs.
pub fn run() {
    loop {
        if let Err(error) = watch() {
            eprintln!("mediaboxd.bluetooth LE interval floor: {error}; again in 10 s");
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

fn watch() -> io::Result<()> {
    // SAFETY: plain socket calls on a descriptor this function owns.
    let fd = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_RAW | libc::SOCK_CLOEXEC, BTPROTO_HCI) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let address = SockaddrHci { family: AF_BLUETOOTH as libc::sa_family_t, dev: 0, channel: HCI_CHANNEL_RAW };
    let filter = HciFilter {
        type_mask: 1 << HCI_EVENT_PKT,
        event_mask: [1 << EVT_DISCONN_COMPLETE, 1 << (EVT_LE_META - 32)],
        opcode: 0,
    };
    unsafe {
        if libc::bind(
            fd.as_raw_fd(),
            &address as *const SockaddrHci as *const libc::sockaddr,
            std::mem::size_of::<SockaddrHci>() as libc::socklen_t,
        ) < 0
            || libc::setsockopt(
                fd.as_raw_fd(),
                SOL_HCI,
                HCI_FILTER,
                &filter as *const HciFilter as *const libc::c_void,
                std::mem::size_of::<HciFilter>() as libc::socklen_t,
            ) < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    eprintln!("mediaboxd.bluetooth LE interval floor: following hci0 (floor 11.25 ms)");
    let mut central = std::collections::BTreeSet::new();
    // How many times each connection has been raised, for the log.
    let mut raised = std::collections::BTreeMap::<u16, u64>::new();
    let mut packet = [0u8; 260];
    loop {
        let read = unsafe { libc::read(fd.as_raw_fd(), packet.as_mut_ptr() as *mut libc::c_void, packet.len()) };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let (handle, interval, latency, timeout) = match parse(&packet[..read as usize]) {
            Some(Event::Gone { handle }) => {
                central.remove(&handle);
                raised.remove(&handle);
                continue;
            }
            Some(Event::Connected { handle, central: true, interval, latency, timeout }) => {
                central.insert(handle);
                raised.remove(&handle);
                (handle, interval, latency, timeout)
            }
            Some(Event::Updated { handle, interval, latency, timeout }) if central.contains(&handle) => {
                (handle, interval, latency, timeout)
            }
            _ => continue,
        };
        if interval >= FLOOR {
            continue;
        }
        let command = raise(handle, latency, timeout);
        let sent = unsafe { libc::write(fd.as_raw_fd(), command.as_ptr() as *const libc::c_void, command.len()) };
        if sent < 0 {
            eprintln!(
                "mediaboxd.bluetooth LE handle {handle}: {:.2} ms not raised: {}",
                f32::from(interval) * 1.25,
                io::Error::last_os_error()
            );
        } else {
            let count = raised.entry(handle).or_insert(0);
            *count += 1;
            if worth_saying(*count) {
                eprintln!(
                    "mediaboxd.bluetooth LE handle {handle}: {:.2} ms raised to 11.25 ms ({count} times on this connection)",
                    f32::from(interval) * 1.25
                );
            }
        }
    }
}

/// Whether the `count`-th raise of one connection is logged: the first, and
/// then the tenth, hundredth and so on.
///
/// A keyboard that asks for 7.5 ms again every few seconds is raised every
/// few seconds, and is meant to be; logging every one was a quarter of the
/// Plus's journal over a night (8489 lines in sixteen hours), which pushed the
/// journal to its size limit and the night's other evidence out with it.
fn worth_saying(count: u64) -> bool {
    let mut step = 1;
    while step < count {
        step *= 10;
    }
    step == count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_raised_again_and_again_is_logged_by_powers_of_ten() {
        let said: Vec<u64> = (1..=10_000).filter(|count| worth_saying(*count)).collect();
        assert_eq!(said, [1, 10, 100, 1000, 10_000]);
    }

    #[test]
    fn the_keyboards_connection_is_read_off_the_events() {
        // LE Connection Complete as the Plus received the ROG AZOTH:
        // central, 7.5 ms, latency 30, timeout 2 s.
        let complete = [
            0x04, 0x3e, 0x13, 0x01, 0x00, 0x10, 0x00, 0x00, 0x01, 0x07, 0x02, 0xe0, 0x30, 0xa3, 0xc5,
            0x06, 0x00, 0x1e, 0x00, 0xc8, 0x00, 0x00,
        ];
        assert_eq!(
            parse(&complete),
            Some(Event::Connected { handle: 16, central: true, interval: 6, latency: 30, timeout: 200 })
        );
        // Its update back to 7.5 ms six seconds after being raised.
        let update = [0x04, 0x3e, 0x0a, 0x03, 0x00, 0x10, 0x00, 0x06, 0x00, 0x1e, 0x00, 0xc8, 0x00];
        assert_eq!(parse(&update), Some(Event::Updated { handle: 16, interval: 6, latency: 30, timeout: 200 }));
        assert_eq!(parse(&[0x04, 0x05, 0x04, 0x00, 0x10, 0x00, 0x13]), Some(Event::Gone { handle: 16 }));
        // A failed update is nothing to act on.
        let failed = [0x04, 0x3e, 0x0a, 0x03, 0x3b, 0x10, 0x00, 0x06, 0x00, 0x1e, 0x00, 0xc8, 0x00];
        assert_eq!(parse(&failed), None);
    }

    #[test]
    fn a_short_interval_is_raised_to_androids_floor() {
        let command = raise(16, 30, 200);
        assert_eq!(command[..4], [0x01, 0x13, 0x20, 14]);
        assert_eq!(le16(&command, 4), Some(16));
        assert_eq!((le16(&command, 6), le16(&command, 8)), (Some(9), Some(9)));
        // 2 s against (1 + 30) × 11.25 ms × 2 = 697.5 ms: latency kept.
        assert_eq!(le16(&command, 10), Some(30));
        assert_eq!(le16(&command, 12), Some(200));
    }

    #[test]
    fn the_latency_gives_way_when_the_timeout_would_not_hold() {
        // 1 s timeout: (1 + latency) × 22.5 ms must stay under 1000 ms -> 43.
        let command = raise(16, 99, 100);
        assert_eq!(le16(&command, 10), Some(43));
    }
}
