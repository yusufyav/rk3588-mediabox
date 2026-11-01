//! The satellite receiver's USB link: whether it is on the bus, and the speed
//! it enumerated at. Read by the receiver before it makes its output, and by
//! the interface (through the control plane) while live television is open,
//! as Tvheadend watches /dev/dvb for its adapters (`linuxdvb_adapter.c`):
//! the Kanky makes no /dev/dvb node, so its sysfs entry is what is watched.
//!
//! The transport stream of a DVB-S2 transponder (up to ~8 MB/s) needs a
//! high-speed link (480 Mbit/s), and the bridge's TS output is set up for one
//! (512-byte bulk packets, `bridge::TS_INIT`). A receiver whose high-speed
//! handshake fails comes up at full speed instead (12 Mbit/s, on the
//! companion OHCI controller): it answers commands and its tuner locks, but
//! not one TS transfer completes. Measured 2026-11-01: the box moved, the
//! port saw `device descriptor read/64, error -71` four times at high speed,
//! and the Kanky came back at full speed ("not running at top speed"); every
//! tune then locked and streamed nothing, until the link was made again.

const KANKY: (&str, &str) = ("23e2", "5270");

/// The speed the link needs, in Mbit/s.
pub const NEEDED_MBPS: u32 = 480;

fn read(path: &str) -> Option<String> {
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

/// The receiver's sysfs name ("3-1") and its link speed in Mbit/s, as the
/// kernel reports it (`/sys/bus/usb/devices/*/speed`).
pub fn kanky_speed() -> Option<(String, u32)> {
    std::fs::read_dir("/sys/bus/usb/devices").ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        let at = format!("/sys/bus/usb/devices/{name}");
        (read(&format!("{at}/idVendor")).as_deref() == Some(KANKY.0)
            && read(&format!("{at}/idProduct")).as_deref() == Some(KANKY.1))
        .then(|| {
            // "1.5", "12", "480", "5000": below 1 counts as 1.
            let speed = read(&format!("{at}/speed"))?.parse::<f64>().ok()? as u32;
            Some((name, speed.max(1)))
        })
        .flatten()
    })
}

/// What live television can expect of the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Not on the bus.
    Absent,
    /// On the bus at a speed that cannot carry a stream (Mbit/s).
    Slow(u32),
    /// On the bus at high speed or above (Mbit/s).
    Ready(u32),
}

/// The receiver's link now.
pub fn link() -> Link {
    match kanky_speed() {
        None => Link::Absent,
        Some((_, speed)) if speed < NEEDED_MBPS => Link::Slow(speed),
        Some((_, speed)) => Link::Ready(speed),
    }
}

/// What the viewer is told while live television cannot use the receiver;
/// none when it can.
pub fn problem(link: Link) -> Option<String> {
    match link {
        Link::Ready(_) => None,
        Link::Absent => Some("Uydu alıcısı takılı değil. Alıcı takıldığında kanal kendiliğinden açılır.".into()),
        Link::Slow(speed) => Some(format!(
            "Uydu alıcısı USB'ye düşük hızda bağlı ({speed} Mbit/s); yayın için {NEEDED_MBPS} Mbit/s gerekir. \
             Alıcının bağlantısını kontrol edin; düzelince kanal kendiliğinden açılır."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_high_speed_link_is_quiet() {
        assert_eq!(problem(Link::Ready(480)), None);
        assert_eq!(problem(Link::Ready(5000)), None);
        assert!(problem(Link::Slow(12)).is_some_and(|text| text.contains("12 Mbit/s")));
        assert!(problem(Link::Absent).is_some_and(|text| text.contains("takılı değil")));
    }
}
