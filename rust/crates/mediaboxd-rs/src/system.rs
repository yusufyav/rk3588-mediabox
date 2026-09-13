//! Appliance vitals, read straight from the kernel.
//!
//! Everything here comes from procfs, sysfs or statvfs — no shelling out, so a
//! diagnostics screen that refreshes every few seconds costs no processes. The
//! numbers are reported raw and unit-tagged; deciding what counts as "hot" or
//! "full" is the interface's job, not this module's.

use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::sync::Mutex;

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|text| text.trim().to_string())
}

fn load_average() -> Value {
    let Some(text) = read_trimmed("/proc/loadavg") else {
        return Value::Null;
    };
    let fields: Vec<&str> = text.split_whitespace().collect();
    let parse = |index: usize| {
        fields
            .get(index)
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    json!({"one": parse(0), "five": parse(1), "fifteen": parse(2)})
}

/// How busy the processors actually are, as a fraction between 0 and 1.
///
/// Not the load average, which is what this used to report. Load average is
/// the number of tasks wanting to run, averaged over a minute: on an eight-core
/// board a load of two reads as "25%" while the processors are very nearly
/// idle, which is exactly what the television was showing. Utilisation is the
/// share of time the processors spent doing anything at all, and the only way
/// to get it is to compare two readings of /proc/stat.
///
/// The previous reading is kept here, so the answer is the work done since the
/// last time anybody asked. The first call after start has nothing to compare
/// against and says so rather than guessing.
fn cpu_usage() -> Value {
    static LAST: Mutex<Option<(u64, u64)>> = Mutex::new(None);

    let Some(text) = read_trimmed("/proc/stat") else {
        return Value::Null;
    };
    let Some(line) = text.lines().find(|line| line.starts_with("cpu ")) else {
        return Value::Null;
    };
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse::<u64>().ok())
        .collect();
    // user nice system idle iowait irq softirq steal …
    if fields.len() < 5 {
        return Value::Null;
    }
    let total: u64 = fields.iter().sum();
    // Waiting for a disk is not the processor being busy, so iowait counts as
    // idle here the way every other tool counts it.
    let idle = fields[3] + fields[4];

    let Ok(mut last) = LAST.lock() else {
        return Value::Null;
    };
    let previous = last.replace((total, idle));
    let Some((last_total, last_idle)) = previous else {
        return Value::Null;
    };
    let total_delta = total.saturating_sub(last_total);
    let idle_delta = idle.saturating_sub(last_idle);
    if total_delta == 0 {
        return Value::Null;
    }
    let busy = total_delta.saturating_sub(idle_delta) as f64 / total_delta as f64;
    json!(busy.clamp(0.0, 1.0))
}

fn cpu_count() -> u32 {
    fs::read_to_string("/proc/cpuinfo")
        .map(|text| {
            text.lines()
                .filter(|line| line.starts_with("processor"))
                .count() as u32
        })
        .unwrap_or(0)
}

fn memory() -> Value {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return Value::Null;
    };
    let field = |name: &str| -> u64 {
        text.lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u64>().ok())
            .map(|kilobytes| kilobytes * 1024)
            .unwrap_or(0)
    };
    let total = field("MemTotal:");
    let available = field("MemAvailable:");
    json!({
        "totalBytes": total,
        "availableBytes": available,
        "usedBytes": total.saturating_sub(available),
        "swapTotalBytes": field("SwapTotal:"),
        "swapFreeBytes": field("SwapFree:"),
    })
}

fn storage(path: &str) -> Value {
    let Ok(c_path) = std::ffi::CString::new(path) else {
        return Value::Null;
    };
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string and `stats` is a
    // correctly sized, zeroed statvfs that the call fills in.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
        return Value::Null;
    }
    let block = stats.f_frsize as u64;
    let total = stats.f_blocks as u64 * block;
    let available = stats.f_bavail as u64 * block;
    json!({
        "path": path,
        "totalBytes": total,
        "availableBytes": available,
        "usedBytes": total.saturating_sub(available),
    })
}

fn temperatures() -> Vec<Value> {
    let Ok(entries) = fs::read_dir("/sys/class/thermal") else {
        return Vec::new();
    };
    let mut found: Vec<Value> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("thermal_zone"))
        .filter_map(|entry| {
            let millidegrees = read_trimmed(entry.path().join("temp"))?
                .parse::<i64>()
                .ok()?;
            let label = read_trimmed(entry.path().join("type"))
                .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            Some(json!({"name": label, "celsius": millidegrees as f64 / 1000.0}))
        })
        .collect();
    found.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["name"].as_str().unwrap_or_default())
    });
    found
}

/// The display pipeline as KMS reports it — which connector is attached, and
/// what mode it is in.
fn drm() -> Vec<Value> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Connectors are `card0-HDMI-A-1`; the bare `card0` is the device.
            if !name.contains('-') {
                return None;
            }
            let status = read_trimmed(entry.path().join("status"))?;
            let mode = read_trimmed(entry.path().join("modes"))
                .and_then(|modes| modes.lines().next().map(str::to_owned));
            let enabled = read_trimmed(entry.path().join("enabled"));
            Some(json!({
                "connector": name,
                "status": status,
                "mode": mode,
                "enabled": enabled,
            }))
        })
        .collect()
}

/// The interface packets leave by, and the gateway they go to.
fn network() -> Value {
    let Ok(text) = fs::read_to_string("/proc/net/route") else {
        return Value::Null;
    };
    let default = text.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Destination 00000000 is the default route.
        (fields.len() > 2 && fields[1] == "00000000").then(|| {
            let gateway = u32::from_str_radix(fields[2], 16).unwrap_or(0);
            let octets = gateway.to_le_bytes();
            json!({
                "interface": fields[0],
                "gateway": format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]),
            })
        })
    });
    let interfaces: Vec<Value> = fs::read_dir("/sys/class/net")
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name == "lo" {
                        return None;
                    }
                    Some(json!({
                        "name": name,
                        "state": read_trimmed(entry.path().join("operstate")),
                        "mac": read_trimmed(entry.path().join("address")),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({"default": default, "interfaces": interfaces})
}

/// Every ffmpeg the appliance is running.
///
/// The media core reports the sessions it believes it owns; this counts the
/// processes that actually exist. A gap between the two is exactly the orphan
/// encoder worth knowing about, so both numbers are reported rather than one.
fn ffmpeg_processes() -> Vec<Value> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        })
        .filter_map(|entry| {
            let comm = read_trimmed(entry.path().join("comm"))?;
            if comm != "ffmpeg" {
                return None;
            }
            let pid = entry.file_name().to_string_lossy().parse::<u32>().ok()?;
            let started = read_trimmed(entry.path().join("stat"))
                .and_then(|stat| stat.rsplit(')').next().map(str::to_owned))
                .and_then(|tail| {
                    tail.split_whitespace()
                        .nth(19)
                        .and_then(|ticks| ticks.parse::<u64>().ok())
                });
            Some(json!({"pid": pid, "startTicks": started}))
        })
        .collect()
}

/// Everything the diagnostics screen reads, in one snapshot.
pub fn diagnostics() -> Value {
    json!({
        "cpu": {"count": cpu_count(), "load": load_average(), "usage": cpu_usage()},
        "memory": memory(),
        "storage": [storage("/"), storage("/var/tmp")],
        "temperatures": temperatures(),
        "drm": drm(),
        "network": network(),
        "ffmpeg": ffmpeg_processes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_carries_every_section() {
        let snapshot = diagnostics();
        for key in ["cpu", "memory", "storage", "temperatures", "drm", "network", "ffmpeg"] {
            assert!(snapshot.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn cpu_and_memory_are_read_from_this_machine() {
        // procfs exists wherever these tests run, so these are real values and
        // a zero here means the parser broke rather than the machine being odd.
        let snapshot = diagnostics();
        assert!(snapshot["cpu"]["count"].as_u64().unwrap_or(0) > 0);
        assert!(snapshot["memory"]["totalBytes"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn storage_reports_the_root_filesystem() {
        let root = &diagnostics()["storage"][0];
        assert_eq!(root["path"], "/");
        assert!(root["totalBytes"].as_u64().unwrap_or(0) > 0);
    }
}
