//! The receiver's USB port power, cut and restored through its hub: the
//! hub's ClearPortFeature/SetPortFeature(PORT_POWER) (USB 2.0 11.24.2),
//! for a hub with per-port power switching, as the RK3588's EHCI root hubs
//! are (wHubCharacteristics 0x0009). Measured 2026-10-29: the bridge comes
//! back COLD (firmware 0.0.0.0) and streams again, as after a replug.

use std::sync::Mutex;
use std::time::{Duration, Instant};

const KANKY: (&str, &str) = ("23e2", "5270");
const PORT_POWER: u16 = 8;
const USBDEVFS_CONTROL: libc::c_ulong = 0xC018_5500;

#[repr(C)]
struct CtrlTransfer {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
    timeout: u32,
    data: *mut libc::c_void,
}

static LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether a power cycle is allowed now: at most one per `every`.
pub fn may_cycle(every: Duration) -> bool {
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    if last.is_some_and(|at| at.elapsed() < every) {
        return false;
    }
    *last = Some(Instant::now());
    true
}

fn read(path: &str) -> std::io::Result<String> {
    Ok(std::fs::read_to_string(path)?.trim().to_string())
}

/// Cuts the Kanky's port power for `off`, then restores it. Returns the
/// port's sysfs name.
pub fn cycle_kanky(off: Duration) -> Result<String, String> {
    let devices = std::fs::read_dir("/sys/bus/usb/devices").map_err(|e| e.to_string())?;
    let name = devices
        .flatten()
        .map(|d| d.file_name().to_string_lossy().into_owned())
        .find(|n| {
            let at = format!("/sys/bus/usb/devices/{n}");
            read(&format!("{at}/idVendor")).ok().as_deref() == Some(KANKY.0)
                && read(&format!("{at}/idProduct")).ok().as_deref() == Some(KANKY.1)
        })
        .ok_or("no Kanky on the bus")?;
    // "3-1" is port 1 of root hub usb3; "3-1.2" is port 2 of hub 3-1.
    let (hub, port) = match name.rsplit_once('.') {
        Some((hub, port)) => (hub.to_string(), port),
        None => {
            let (bus, port) = name.split_once('-').ok_or("unexpected device name")?;
            (format!("usb{bus}"), port)
        }
    };
    let port: u16 = port.parse().map_err(|_| "unexpected port")?;
    let at = format!("/sys/bus/usb/devices/{hub}");
    let bus: u32 = read(&format!("{at}/busnum")).map_err(|e| e.to_string())?.parse().map_err(|_| "busnum")?;
    let dev: u32 = read(&format!("{at}/devnum")).map_err(|e| e.to_string())?.parse().map_err(|_| "devnum")?;
    let node = std::ffi::CString::new(format!("/dev/bus/usb/{bus:03}/{dev:03}")).map_err(|e| e.to_string())?;
    let fd = unsafe { libc::open(node.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        return Err(format!("{node:?}: {}", std::io::Error::last_os_error()));
    }
    let request = |request: u8| {
        let mut transfer = CtrlTransfer { request_type: 0x23, request, value: PORT_POWER, index: port, length: 0, timeout: 1000, data: std::ptr::null_mut() };
        let result = unsafe { libc::ioctl(fd, USBDEVFS_CONTROL as _, &mut transfer as *mut CtrlTransfer) };
        if result < 0 { Err(std::io::Error::last_os_error().to_string()) } else { Ok(()) }
    };
    let result = request(1).and_then(|()| {
        std::thread::sleep(off);
        request(3)
    });
    unsafe { libc::close(fd) };
    result.map(|()| format!("{hub} port {port}"))
}
