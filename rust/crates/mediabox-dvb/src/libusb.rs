//! libusb-1.0, opened at run time.
//!
//! Linked, the cross build would need an aarch64 libusb to link against, and
//! this repository's cross build is deliberately plain (see
//! rust/.cargo/config.toml). The appliance carries `libusb-1.0.so.0`
//! (Armbian trixie, measured); it is asked for by its soname, the ABI the
//! declarations below are written against (libusb.h, 1.0.x).

use std::ffi::{CStr, c_char, c_int, c_uchar, c_uint, c_void};
use std::sync::OnceLock;

pub enum Context {}
pub enum DeviceHandle {}

pub const TRANSFER_TYPE_BULK: c_uchar = 2;

pub const TRANSFER_COMPLETED: c_int = 0;
pub const TRANSFER_ERROR: c_int = 1;
pub const TRANSFER_TIMED_OUT: c_int = 2;
pub const TRANSFER_CANCELLED: c_int = 3;
pub const TRANSFER_STALL: c_int = 4;
pub const TRANSFER_NO_DEVICE: c_int = 5;
pub const TRANSFER_OVERFLOW: c_int = 6;

pub const ERROR_TIMEOUT: c_int = -7;
pub const ERROR_NO_DEVICE: c_int = -4;
pub const ERROR_NOT_FOUND: c_int = -5;

pub type TransferCallback = extern "C" fn(*mut Transfer);

/// `struct libusb_transfer`, without its trailing isochronous descriptors
/// (none are allocated: `libusb_alloc_transfer(0)`).
#[repr(C)]
pub struct Transfer {
    pub dev_handle: *mut DeviceHandle,
    pub flags: u8,
    pub endpoint: c_uchar,
    pub kind: c_uchar,
    pub timeout: c_uint,
    pub status: c_int,
    pub length: c_int,
    pub actual_length: c_int,
    pub callback: Option<TransferCallback>,
    pub user_data: *mut c_void,
    pub buffer: *mut c_uchar,
    pub num_iso_packets: c_int,
}

#[repr(C)]
pub struct Timeval {
    pub tv_sec: libc::time_t,
    pub tv_usec: libc::suseconds_t,
}

pub struct Lib {
    pub init: unsafe extern "C" fn(*mut *mut Context) -> c_int,
    pub exit: unsafe extern "C" fn(*mut Context),
    pub open_device_with_vid_pid: unsafe extern "C" fn(*mut Context, u16, u16) -> *mut DeviceHandle,
    pub close: unsafe extern "C" fn(*mut DeviceHandle),
    pub kernel_driver_active: unsafe extern "C" fn(*mut DeviceHandle, c_int) -> c_int,
    pub get_configuration: unsafe extern "C" fn(*mut DeviceHandle, *mut c_int) -> c_int,
    pub claim_interface: unsafe extern "C" fn(*mut DeviceHandle, c_int) -> c_int,
    pub release_interface: unsafe extern "C" fn(*mut DeviceHandle, c_int) -> c_int,
    pub bulk_transfer:
        unsafe extern "C" fn(*mut DeviceHandle, c_uchar, *mut c_uchar, c_int, *mut c_int, c_uint) -> c_int,
    pub alloc_transfer: unsafe extern "C" fn(c_int) -> *mut Transfer,
    pub free_transfer: unsafe extern "C" fn(*mut Transfer),
    pub submit_transfer: unsafe extern "C" fn(*mut Transfer) -> c_int,
    pub cancel_transfer: unsafe extern "C" fn(*mut Transfer) -> c_int,
    pub handle_events_timeout_completed: unsafe extern "C" fn(*mut Context, *mut Timeval, *mut c_int) -> c_int,
    pub error_name: unsafe extern "C" fn(c_int) -> *const c_char,
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

/// The library, loaded once; the reason it could not be, otherwise.
pub fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(load).as_ref().map_err(Clone::clone)
}

// Each transmute's target is the field it initialises, declared above with
// libusb.h's prototype.
#[allow(clippy::missing_transmute_annotations)]
fn load() -> Result<Lib, String> {
    const SONAME: &CStr = c"libusb-1.0.so.0";
    // SAFETY: dlopen with a constant, NUL-terminated name; the handle is kept
    // for the life of the process (never dlclosed), so every symbol below
    // stays valid.
    let handle = unsafe { libc::dlopen(SONAME.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: dlerror returns a thread-local message or null.
        let reason = unsafe { libc::dlerror() };
        let reason = if reason.is_null() {
            "unknown".to_string()
        } else {
            unsafe { CStr::from_ptr(reason) }.to_string_lossy().into_owned()
        };
        return Err(format!("libusb-1.0.so.0 yüklenemedi: {reason}"));
    }
    macro_rules! symbol {
        ($name:literal) => {{
            // SAFETY: the symbol is looked up by its exported name and cast
            // to the prototype libusb.h declares for it.
            let pointer = unsafe { libc::dlsym(handle, concat!($name, "\0").as_ptr().cast()) };
            if pointer.is_null() {
                return Err(format!("libusb: {} yok", $name));
            }
            unsafe { std::mem::transmute::<*mut c_void, _>(pointer) }
        }};
    }
    Ok(Lib {
        init: symbol!("libusb_init"),
        exit: symbol!("libusb_exit"),
        open_device_with_vid_pid: symbol!("libusb_open_device_with_vid_pid"),
        close: symbol!("libusb_close"),
        kernel_driver_active: symbol!("libusb_kernel_driver_active"),
        get_configuration: symbol!("libusb_get_configuration"),
        claim_interface: symbol!("libusb_claim_interface"),
        release_interface: symbol!("libusb_release_interface"),
        bulk_transfer: symbol!("libusb_bulk_transfer"),
        alloc_transfer: symbol!("libusb_alloc_transfer"),
        free_transfer: symbol!("libusb_free_transfer"),
        submit_transfer: symbol!("libusb_submit_transfer"),
        cancel_transfer: symbol!("libusb_cancel_transfer"),
        handle_events_timeout_completed: symbol!("libusb_handle_events_timeout_completed"),
        error_name: symbol!("libusb_error_name"),
    })
}

/// libusb's own name for an error code.
pub fn error_name(code: c_int) -> String {
    match lib() {
        // SAFETY: libusb_error_name returns a static string for any code.
        Ok(lib) => unsafe { CStr::from_ptr((lib.error_name)(code)) }.to_string_lossy().into_owned(),
        Err(_) => format!("libusb error {code}"),
    }
}

pub fn transfer_status_name(status: c_int) -> &'static str {
    match status {
        TRANSFER_COMPLETED => "TRANSFER_COMPLETED",
        TRANSFER_ERROR => "TRANSFER_ERROR",
        TRANSFER_TIMED_OUT => "TRANSFER_TIMED_OUT",
        TRANSFER_CANCELLED => "TRANSFER_CANCELLED",
        TRANSFER_STALL => "TRANSFER_STALL",
        TRANSFER_NO_DEVICE => "TRANSFER_NO_DEVICE",
        TRANSFER_OVERFLOW => "TRANSFER_OVERFLOW",
        _ => "TRANSFER_UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_transfer_struct_has_libusb_s_layout() {
        // libusb.h on LP64: handle 0, flags 8, endpoint 9, type 10, timeout 12,
        // status 16, length 20, actual_length 24, callback 32, user_data 40,
        // buffer 48, num_iso_packets 56.
        assert_eq!(std::mem::offset_of!(super::Transfer, timeout), 12);
        assert_eq!(std::mem::offset_of!(super::Transfer, actual_length), 24);
        assert_eq!(std::mem::offset_of!(super::Transfer, callback), 32);
        assert_eq!(std::mem::offset_of!(super::Transfer, buffer), 48);
        assert_eq!(std::mem::offset_of!(super::Transfer, num_iso_packets), 56);
    }
}
