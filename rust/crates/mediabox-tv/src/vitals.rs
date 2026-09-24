//! The machine's own vital signs, and the launcher beside them.
//!
//! Both are read from the control plane on the same slow timer and arrive at
//! the interface already formatted: the television draws strings and one arc
//! per ring, and decides nothing. Ten seconds, because load and temperature do
//! not move faster than that in any way a person can see, and a home screen
//! that re-renders four times a second is a home screen that is never idle.

use serde_json::Value;

use crate::VitalsModel;

/// How often the box is asked how it is doing. The web interface's own figure.
pub const EVERY: std::time::Duration = std::time::Duration::from_secs(10);

const DAYS: [&str; 7] = [
    "Pazar",
    "Pazartesi",
    "Salı",
    "Çarşamba",
    "Perşembe",
    "Cuma",
    "Cumartesi",
];

const MONTHS: [&str; 12] = [
    "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül", "Ekim",
    "Kasım", "Aralık",
];

fn human_size(bytes: f64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// The clock, as a television writes it: the time large, the day under it.
///
/// Worked out from the system clock rather than pulled in with a date library:
/// this is the only date arithmetic in the interface, and a civil calendar is
/// the one piece of arithmetic that has not changed since 1582.
fn clock() -> (String, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0);

    // The appliance runs on local time and systemd keeps it there, so the
    // offset is read from the C library rather than assumed to be UTC.
    let offset = local_offset(now);
    let local = now + offset;

    let days = local.div_euclid(86_400);
    let seconds = local.rem_euclid(86_400);
    let (hour, minute) = (seconds / 3600, (seconds % 3600) / 60);

    // 1970-01-01 was a Thursday.
    let weekday = (days + 4).rem_euclid(7) as usize;
    let (year, month, day) = civil_from_days(days);

    (
        format!("{hour:02}:{minute:02}"),
        format!(
            "{} {} {}, {}",
            day,
            MONTHS[(month - 1) as usize],
            year,
            DAYS[weekday]
        ),
    )
}

fn local_offset(utc: i64) -> i64 {
    // SAFETY: localtime_r writes into a struct this call owns, and the only
    // field read back is the offset the C library computed for it.
    unsafe {
        let time = utc as libc::time_t;
        let mut parts: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut parts).is_null() {
            return 0;
        }
        parts.tm_gmtoff as i64
    }
}

/// Howard Hinnant's civil-from-days, which is the algorithm every date library
/// uses underneath.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// One ring, as the arc that draws it.
///
/// A 24x24 viewbox with the ring on a radius of 10, starting at twelve
/// o'clock and sweeping clockwise — the same geometry the stylesheet's conic
/// gradient has, expressed as the one thing Slint can draw without a shader.
///
/// A full reading is written as two half-circles: a single arc of exactly 360
/// degrees has the same start and end point and renders as nothing at all.
pub(crate) fn arc(fraction: f64) -> String {
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction <= 0.001 {
        return String::new();
    }
    if fraction >= 0.999 {
        return "M 12 2 A 10 10 0 0 1 12 22 A 10 10 0 0 1 12 2".to_string();
    }

    let angle = fraction * std::f64::consts::TAU;
    let x = 12.0 + 10.0 * angle.sin();
    let y = 12.0 - 10.0 * angle.cos();
    let large = if fraction > 0.5 { 1 } else { 0 };
    format!("M 12 2 A 10 10 0 {large} 1 {x:.3} {y:.3}")
}

fn ratio(root: &Value, used: &str, total: &str) -> Option<(f64, f64, f64)> {
    let used = root.pointer(used).and_then(Value::as_f64)?;
    let total = root.pointer(total).and_then(Value::as_f64)?;
    (total > 0.0).then_some((used / total, used, total))
}

/// Turns one `diagnostics` answer into the strings the panel shows.
///
/// Every reading is optional and every one of them falls back to an em dash
/// rather than to a zero: a dial reading 0% and a dial that has not been told
/// anything look the same on a television, and only one of them is true.
pub fn read(answer: Option<&Value>) -> VitalsModel {
    let (time, date) = clock();

    let mut model = VitalsModel {
        time: time.into(),
        date: date.into(),
        cpu_arc: String::new().into(),
        cpu_text: "—".into(),
        memory_arc: String::new().into(),
        memory_text: "—".into(),
        performance_note: "—".into(),
        storage_text: "—".into(),
        storage_fraction: 0.0,
        network: "Ağ yok".into(),
        // Replaced below from the machine's own device tree. This is what a
        // board that has not answered yet is called, not what any board is.
        machine: "RK3588".into(),
        // Both ways down until the snapshot says otherwise.
        eth_up: false,
        eth_address: String::new().into(),
        wifi_up: false,
        wifi_ssid: String::new().into(),
        wifi_address: String::new().into(),
        wifi_bars: 0,
    };

    let Some(root) = answer else { return model };

    // Which board this is, in its own words rather than in a string literal.
    // A Plus used to tell the person holding it that it was an Ultra.
    if let Some(name) = root.get("machine").and_then(Value::as_str) {
        model.machine = name.into();
    }

    // What the processors are actually doing. The load average that used to be
    // shown here is a queue length, not a share of time: eight cores nearly
    // idle with two tasks waiting read as a quarter busy.
    let cpu = root
        .pointer("/cpu/usage")
        .and_then(Value::as_f64)
        .or_else(|| {
            let count = root
                .pointer("/cpu/count")
                .and_then(Value::as_f64)
                .unwrap_or(1.0)
                .max(1.0);
            let load = root.pointer("/cpu/load/one").and_then(Value::as_f64)?;
            Some(load / count)
        })
        .map(|value| value.clamp(0.0, 1.0));

    if let Some(cpu) = cpu {
        model.cpu_arc = arc(cpu).into();
        model.cpu_text = format!("%{:.0}", cpu * 100.0).into();
    }

    let memory = ratio(root, "/memory/usedBytes", "/memory/totalBytes");
    if let Some((fraction, _, _)) = memory {
        model.memory_arc = arc(fraction).into();
        model.memory_text = format!("%{:.0}", fraction * 100.0).into();
    }

    let hottest = root
        .get("temperatures")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("celsius").and_then(Value::as_f64))
                .fold(f64::NEG_INFINITY, f64::max)
        })
        .filter(|value| value.is_finite());

    let used = memory
        .map(|(_, used, total)| format!("{} / {}", human_size(used), human_size(total)))
        .unwrap_or_else(|| "—".into());
    let heat = hottest
        .map(|c| format!("{c:.0} °C"))
        .unwrap_or_else(|| "—".into());
    model.performance_note = format!("{used}  ·  {heat}").into();

    if let Some(volume) = root
        .get("storage")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
    {
        let used = volume.get("usedBytes").and_then(Value::as_f64);
        let total = volume.get("totalBytes").and_then(Value::as_f64);
        if let (Some(used), Some(total)) = (used, total) {
            if total > 0.0 {
                model.storage_fraction = (used / total) as f32;
                model.storage_text = format!("{} / {}", human_size(used), human_size(total)).into();
            }
        }
    }

    // The bar says whether the box is on the network, not which kernel device
    // it is on: "enP3p49s0" beside the clock on a television is the appliance
    // talking to itself. The interface's name is on the diagnostics screen,
    // where somebody who wants it will look.
    let online = root
        .pointer("/network/default/interface")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty());
    model.network = if online {
        "Bağlı".into()
    } else {
        "Ağ yok".into()
    };

    // The wire. The first non-wireless interface that is up and holds an
    // address: a board with two sockets has two, and the one carrying traffic
    // is the one worth naming.
    if let Some(interfaces) = root.pointer("/network/interfaces").and_then(Value::as_array) {
        for interface in interfaces {
            let wireless = interface
                .get("wireless")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let up = interface.get("state").and_then(Value::as_str) == Some("up");
            let address = interface
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !wireless && up && !address.is_empty() {
                model.eth_up = true;
                model.eth_address = address.into();
                break;
            }
        }
    }

    // The radio, which says which network it is on rather than which device
    // the kernel gave it.
    let wifi = root.pointer("/wireless/wifi");
    let ssid = wifi
        .and_then(|wifi| wifi.get("ssid"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let wifi_address = wifi
        .and_then(|wifi| wifi.get("address"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !ssid.is_empty() {
        model.wifi_up = true;
        model.wifi_ssid = ssid.into();
        model.wifi_address = wifi_address.into();
        model.wifi_bars = wifi
            .and_then(|wifi| wifi.get("signal"))
            .and_then(Value::as_i64)
            .map(signal_bars)
            .unwrap_or(0);
    }

    model
}

/// dBm to four steps. The same thresholds the Wi-Fi screen uses, so a network
/// does not read as three bars on one screen and four on the other.
fn signal_bars(dbm: i64) -> i32 {
    match dbm {
        s if s >= -55 => 4,
        s if s >= -67 => 3,
        s if s >= -78 => 2,
        _ => 1,
    }
}
