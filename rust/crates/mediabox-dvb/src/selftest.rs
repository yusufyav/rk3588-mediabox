//! What can be checked without the receiver, on the box it will run on.
//!
//! Nothing here opens USB: there is no `usb::HardwareAccess` in this module,
//! and `usb::Session::open` cannot be called without one. libusb is loaded
//! and its symbols resolved, but no context is created and no device is
//! listed, opened, reset or written. The report says how many times this
//! process tried to open the receiver (`usb_open_attempts`); it must be 0.

use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use crate::firmware::Firmware;
use crate::ts::{self, Analyser, PACKET, Selector};

/// The TRT HABER HD sequence parameter set (SID 10801, Türksat 12188 V),
/// with its emulation-prevention bytes: 1920x1080, coded as fields, 25 frames
/// a second. A coding parameter set, not programme content.
pub const TRT_HABER_HD_SPS: &str = "67640028ac72140780447de02d1000000300100000030329cb1c308580";

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: Value,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub ok: bool,
    pub usb_open_attempts: u64,
    pub checks: Vec<Check>,
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).filter_map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

fn packet(pid: u16, unit_start: bool, cc: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0x47, (if unit_start { 0x40 } else { 0 }) | (pid >> 8) as u8, pid as u8, 0x10 | (cc & 0xF)];
    p.extend_from_slice(&payload[..payload.len().min(PACKET - 4)]);
    p.resize(PACKET, 0xFF);
    p
}

fn section(table: u8, ext: u16, body: &[u8]) -> Vec<u8> {
    let length = 5 + body.len() + 4;
    let mut s = vec![table, 0xB0 | (length >> 8) as u8, length as u8];
    s.extend_from_slice(&ext.to_be_bytes());
    s.extend_from_slice(&[0xC1, 0, 0]);
    s.extend_from_slice(body);
    let crc = ts::crc32_mpeg(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

/// A two-programme multiplex with 10801's video carrying the real SPS.
pub fn synthetic_multiplex() -> Vec<u8> {
    let pat = section(0x00, 41306, &[0x2A, 0x31, 0xE0, 34, 0x2A, 0x32, 0xE0, 35]);
    let pmt1 = section(0x02, 10801, &[0xE8, 0xFD, 0xF0, 0, 0x1B, 0xE8, 0xFD, 0xF0, 0, 0x03, 0xE9, 0x61, 0xF0, 0]);
    let pmt2 = section(0x02, 10802, &[0xE8, 0xFE, 0xF0, 0, 0x1B, 0xE8, 0xFE, 0xF0, 0]);
    let mut pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1];
    pes.extend_from_slice(&[0, 0, 0, 1, 0x09, 0xF0]); // access unit delimiter
    pes.extend_from_slice(&[0, 0, 0, 1]);
    pes.extend_from_slice(&unhex(TRT_HABER_HD_SPS));
    pes.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xEE, 0x3C, 0x80]); // a PPS
    let psi = |pid, cc, s: &[u8]| {
        let mut payload = vec![0];
        payload.extend_from_slice(s);
        packet(pid, true, cc, &payload)
    };
    let mut out = Vec::new();
    for round in 0..3u8 {
        out.extend(psi(0, round, &pat));
        out.extend(psi(34, round, &pmt1));
        out.extend(psi(35, round, &pmt2));
        out.extend(packet(2301, true, round * 2, &pes));
        out.extend(packet(2301, false, round * 2 + 1, &[0; 8]));
        out.extend(packet(2302, true, round, &[0, 0, 1, 0xE0]));
        out.extend(packet(2401, true, round, &[0, 0, 1, 0xC0]));
        out.extend(packet(ts::PID_NULL, false, 0, &[]));
    }
    out
}

fn check_libusb() -> Check {
    match crate::libusb::lib() {
        Ok(_) => Check { name: "libusb", ok: true, detail: json!("libusb-1.0.so.0 loaded, every symbol resolved; no context created") },
        Err(error) => Check { name: "libusb", ok: false, detail: json!(error) },
    }
}

fn check_firmware(dir: &Path) -> Check {
    match Firmware::load(dir) {
        Ok(firmware) => Check {
            name: "firmware",
            ok: firmware.bridge_blocks.is_some(),
            detail: json!({
                "avl62x1_patch_sha256": crate::sha256::hex(&firmware.patch),
                "rda5815m_tables": firmware.tables.release,
                "bridge_firmware_blocks": firmware.bridge_blocks.as_ref().map(Vec::len),
            }),
        },
        Err(error) => Check { name: "firmware", ok: false, detail: json!(error.to_string()) },
    }
}

fn check_pipeline() -> Check {
    let mut analyser = Analyser::new();
    let mut selector = Selector::new(10801);
    let mut out = Vec::new();
    analyser.feed(&synthetic_multiplex(), |p| selector.packet(p, &mut out));
    let mut again = Analyser::new();
    again.feed(&out, |_| {});
    let selection = selector.selection();
    let video = selection.video.clone();
    let ok = selector.ready()
        && selection.pids == [34, 2301, 2401]
        && again.integrity().cc_errors == 0
        && again.pat.len() == 1
        && video.as_ref().is_some_and(|v| {
            v.width == 1920 && v.height == 1080 && v.interlaced && v.frame_rate == Some((25, 1)) && v.field_rate == Some((50, 1))
        });
    Check {
        name: "ts_pipeline",
        ok,
        detail: json!({"pids": selection.pids, "output_cc_errors": again.integrity().cc_errors, "video": video}),
    }
}

fn check_fifo() -> Check {
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;
    let dir = std::env::temp_dir().join(format!("mediabox-dvb-selftest-{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: a NUL-terminated path.
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut reader = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path)?;
        let mut writer = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&path)?;
        writer.write_all(&[0x47; PACKET])?;
        let mut back = [0u8; PACKET];
        reader.read_exact(&mut back)?;
        if back != [0x47; PACKET] {
            return Err(std::io::Error::other("FIFO returned other bytes"));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(()) => Check { name: "fifo", ok: true, detail: json!("created, written, read back, removed") },
        Err(error) => Check { name: "fifo", ok: false, detail: json!(error.to_string()) },
    }
}

use std::os::unix::fs::OpenOptionsExt;

fn check_recording(path: &Path, service: u16) -> Check {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return Check { name: "recording", ok: false, detail: json!(format!("{}: {error}", path.display())) },
    };
    let mut analyser = Analyser::new();
    let mut selector = Selector::new(service);
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut output_bytes = 0usize;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                analyser.feed(&buffer[..n], |p| selector.packet(p, &mut out));
                output_bytes += out.len();
                out.clear();
            }
            Err(error) => return Check { name: "recording", ok: false, detail: json!(error.to_string()) },
        }
    }
    let integrity = analyser.integrity();
    let selection = selector.selection();
    let name = analyser.sdt.get(&service).and_then(|s| s.name.clone());
    Check {
        name: "recording",
        ok: selector.ready() && integrity.sync_errors == 0 && integrity.tei == 0 && integrity.cc_errors == 0,
        detail: json!({
            "service": service,
            "name": name,
            "pids": selection.pids,
            "video": selection.video,
            "input": {"packets": integrity.packets, "sync_errors": integrity.sync_errors, "tei": integrity.tei, "cc_errors": integrity.cc_errors},
            "output_bytes": output_bytes,
        }),
    }
}

pub fn run(firmware_dir: &Path, recording: Option<&Path>, service: u16) -> Report {
    let mut checks = vec![check_libusb(), check_firmware(firmware_dir), check_pipeline(), check_fifo()];
    if let Some(path) = recording {
        checks.push(check_recording(path, service));
    }
    let usb_open_attempts = crate::usb::OPEN_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst);
    Report { ok: usb_open_attempts == 0 && checks.iter().all(|check| check.ok), usb_open_attempts, checks }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trt_sps_reads_as_1080i25() {
        let sps = unhex(TRT_HABER_HD_SPS);
        let info = ts::parse_h264_sps(&sps[1..]).unwrap();
        assert_eq!((info.width, info.height, info.interlaced), (1920, 1080, true));
        assert_eq!(info.frame_rate, Some((25, 1)));
        assert_eq!(info.field_rate, Some((50, 1)));
    }

    #[test]
    fn the_hardware_free_checks_pass_and_open_nothing() {
        for check in [check_pipeline(), check_fifo()] {
            assert!(check.ok, "{}: {}", check.name, check.detail);
        }
        assert_eq!(crate::usb::OPEN_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
