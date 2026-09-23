//! Keeping the display lit while the television changes hands.
//!
//! When the last process with the display device open closes it, the Rockchip
//! driver's `lastclose` puts the kernel console's own mode back on the
//! connector: the sink's preferred one, 1920x1080 on one of the Sony's inputs.
//! Kodi then starts on that, records it as the mode to restore, and puts it
//! back when it exits. That is the 1080p a handover showed.
//!
//! So the interface does not let go of the device when it stops. It leaves its
//! last frame on the CRTC, gives up DRM master, and hands its open device to
//! systemd's file descriptor store. The file stays open, so `lastclose` never
//! runs, the mode stays on the wire, and the next owner starts from it. When
//! the interface starts again it takes the stored file back and closes it once
//! its own first frame is on the panel.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Mutex;

/// The name the device is stored under.
const NAME: &str = "mediabox-tv-drm";

/// Files systemd handed back at start, held until the first frame is shown.
static INHERITED: Mutex<Vec<OwnedFd>> = Mutex::new(Vec::new());

/// Take the stored device back, if systemd passed one. Call once, at start.
pub fn adopt() {
    let pid_matches = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
        == Some(std::process::id());
    let count = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|n| n.parse::<i32>().ok())
        .unwrap_or(0);
    let names = std::env::var("LISTEN_FDNAMES").unwrap_or_default();
    // SAFETY: the environment is read and cleared before any thread that
    // could read it is started.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    if !pid_matches || count <= 0 {
        return;
    }
    let names: Vec<&str> = names.split(':').collect();
    let mut held = INHERITED.lock().unwrap_or_else(|e| e.into_inner());
    for index in 0..count {
        let fd: RawFd = 3 + index;
        // SAFETY: systemd passed these descriptors to this process; nothing
        // else in it owns them.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        if names.get(index as usize).copied() == Some(NAME) {
            held.push(owned);
        }
    }
    if !held.is_empty() {
        eprintln!("mediabox-tv.handover took back the display device kept across the last stop");
    }
}

/// The interface's own frame is on the panel: the kept device can go.
pub fn release_inherited() {
    let mut held = INHERITED.lock().unwrap_or_else(|e| e.into_inner());
    if held.is_empty() {
        return;
    }
    held.clear();
    let _ = notify(&format!("FDSTOREREMOVE=1\nFDNAME={NAME}"), None);
    eprintln!("mediabox-tv.handover the kept display device is closed");
}

/// Hand the open display device to systemd so it outlives this process.
pub fn keep(device: RawFd) -> bool {
    // Anything left over from a previous stop goes first, so the store holds
    // exactly the device that is on the wire now.
    let _ = notify(&format!("FDSTOREREMOVE=1\nFDNAME={NAME}"), None);
    match notify(&format!("FDSTORE=1\nFDNAME={NAME}"), Some(device)) {
        Ok(()) => {
            // The message is only worth something if systemd has read it before
            // this process is gone.
            let _ = barrier();
            true
        }
        Err(e) => {
            eprintln!("mediabox-tv.handover could not keep the display device: {e}");
            false
        }
    }
}

fn notify(message: &str, fd: Option<RawFd>) -> Result<(), String> {
    let path = std::env::var("NOTIFY_SOCKET").map_err(|_| "no NOTIFY_SOCKET".to_string())?;
    // SAFETY: plain socket syscalls on descriptors owned here.
    unsafe {
        let socket = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if socket < 0 {
            return Err(format!("socket: {}", std::io::Error::last_os_error()));
        }
        let socket = OwnedFd::from_raw_fd(socket);

        let mut address: libc::sockaddr_un = std::mem::zeroed();
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_bytes();
        if bytes.is_empty() || bytes.len() >= address.sun_path.len() {
            return Err("bad NOTIFY_SOCKET".to_string());
        }
        for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        // An abstract socket is written with a leading '@'.
        if bytes[0] == b'@' {
            address.sun_path[0] = 0;
        }
        let address_len =
            (std::mem::size_of::<libc::sa_family_t>() + bytes.len()) as libc::socklen_t;

        let text = CString::new(message).map_err(|e| e.to_string())?;
        let mut iov = libc::iovec {
            iov_base: text.as_ptr() as *mut libc::c_void,
            iov_len: message.len(),
        };
        let mut header: libc::msghdr = std::mem::zeroed();
        header.msg_name = &mut address as *mut _ as *mut libc::c_void;
        header.msg_namelen = address_len;
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;

        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
        let mut control = vec![0u8; space];
        if let Some(fd) = fd {
            header.msg_control = control.as_mut_ptr() as *mut libc::c_void;
            header.msg_controllen = space as _;
            let cmsg = libc::CMSG_FIRSTHDR(&header);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
            std::ptr::copy_nonoverlapping(
                &fd as *const RawFd as *const u8,
                libc::CMSG_DATA(cmsg),
                std::mem::size_of::<RawFd>(),
            );
        }
        if libc::sendmsg(socket.as_raw_fd(), &header, libc::MSG_NOSIGNAL) < 0 {
            return Err(format!("sendmsg: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

/// Wait until systemd has processed everything sent before this.
fn barrier() -> Result<(), String> {
    let mut pipe = [0 as RawFd; 2];
    // SAFETY: a pipe whose two ends are owned here.
    unsafe {
        if libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let read = OwnedFd::from_raw_fd(pipe[0]);
        let write = OwnedFd::from_raw_fd(pipe[1]);
        notify("BARRIER=1", Some(write.as_raw_fd()))?;
        drop(write);
        let mut poll = libc::pollfd {
            fd: read.as_raw_fd(),
            events: 0,
            revents: 0,
        };
        libc::poll(&mut poll, 1, 5_000);
    }
    Ok(())
}
