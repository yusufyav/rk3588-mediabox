//! The radios: Wi-Fi and Bluetooth, as the television can manage them.
//!
//! This lives in the daemon rather than the interface for the same reason the
//! indicator lights and the colour mode do. Switching a radio on is rfkill,
//! which is root's; writing a network down is `/etc/netplan`, which is root's;
//! and the interface's unit mounts `/sys` read-only. None of that is the
//! interface's to hold.
//!
//! **Wi-Fi goes through netplan.** Both boards already have their Ethernet
//! configured by it — Armbian ships `10-dhcp-all-interfaces.yaml` and netplan
//! renders it into systemd-networkd — and `networkctl` reports the wireless
//! link as `unmanaged` precisely because nothing has written it down. Adding
//! NetworkManager beside that would give the board two managers reaching for
//! one radio, which is the classic way to get a link that connects and then
//! drops a second later. So this writes a second netplan file and applies it.
//!
//! **The passphrase is not kept.** `wpa_passphrase` derives the PSK and only
//! the derived key reaches the disk. What the viewer typed never goes into a
//! journal line, an error string or the remembered-networks file.
//!
//! **Bluetooth goes through bluetoothctl** rather than the D-Bus API, which
//! would mean a new dependency for a handful of calls that are all one verb
//! and an address.

use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const IW: &str = "/usr/sbin/iw";
const IP: &str = "/usr/sbin/ip";
const RFKILL: &str = "/usr/sbin/rfkill";
const NETPLAN: &str = "/usr/sbin/netplan";
const WPA_PASSPHRASE: &str = "/usr/bin/wpa_passphrase";
const BLUETOOTHCTL: &str = "/usr/bin/bluetoothctl";

/// Written by this daemon and nothing else. The number puts it after
/// Armbian's own file, so the Ethernet rules stay exactly as they were.
const NETPLAN_FILE: &str = "/etc/netplan/20-mediabox-wifi.yaml";
/// The networks the viewer has joined, as {ssid: psk}. The passphrase is never
/// here — only what `wpa_passphrase` derived from it.
const REMEMBERED: &str = "/var/lib/mediabox/wifi-networks.json";

/// How long a scan is given. Measured on both boards: a full two-band pass
/// comes back in four to six seconds, and a radio that has just been switched
/// on takes the long end of that.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);
/// How long `netplan apply` plus DHCP is given before the answer is "it did
/// not come up". Association is usually under two seconds; the lease is what
/// takes the rest.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(35);

// ------------------------------------------------------------------ helpers

async fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("{program} çalıştırılamadı: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        Err(format!(
            "{} başarısız oldu: {}",
            Path::new(program)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| program.to_string()),
            detail
        ))
    }
}

/// `run`, but the command is not allowed to sit there forever. A scan on a
/// radio that has just come up can hang rather than fail.
async fn run_within(limit: Duration, program: &str, args: &[&str]) -> Result<String, String> {
    match tokio::time::timeout(limit, run(program, args)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "{} zaman aşımına uğradı",
            Path::new(program)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| program.to_string())
        )),
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

/// The board's wireless interface, whatever the kernel decided to call it.
///
/// It is not the same name on the two boards — the Ultra's Broadcom SDIO part
/// comes up as `wlan0` and the Plus's Realtek PCIe part as `wlP2p33s0` — so
/// nothing here may hard-code one.
pub fn wifi_interface() -> Option<String> {
    let entries = std::fs::read_dir("/sys/class/net").ok()?;
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("wl"))
        .collect();
    names.sort();
    names.into_iter().next()
}

// -------------------------------------------------------------------- Wi-Fi

/// Whether the Wi-Fi radio is soft-blocked, as rfkill sees it.
async fn wifi_blocked() -> Option<bool> {
    let text = run(RFKILL, &["--output", "TYPE,SOFT", "--noheadings"])
        .await
        .ok()?;
    let mut seen = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some("wlan") {
            let blocked = fields.next() == Some("blocked");
            // Any one of them being unblocked is enough to transmit.
            seen = Some(seen.map_or(blocked, |prior: bool| prior && blocked));
        }
    }
    seen
}

/// The address the interface currently holds, if any.
fn address_of(interface: &str) -> Option<String> {
    let text = std::fs::read_to_string("/proc/net/fib_trie").ok()?;
    // Cheap and dependency-free: ask `ip` instead of parsing the trie.
    let _ = text;
    None.or_else(|| {
        let output = std::process::Command::new(IP)
            .args(["-4", "-brief", "addr", "show", interface])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&output.stdout);
        line.split_whitespace()
            .nth(2)
            .map(|cidr| cidr.split('/').next().unwrap_or(cidr).to_string())
    })
}

/// What the link is doing right now: the network it is on and how strong it is.
async fn link_of(interface: &str) -> (Option<String>, Option<i32>) {
    let Ok(text) = run(IW, &["dev", interface, "link"]).await else {
        return (None, None);
    };
    if text.trim().starts_with("Not connected") {
        return (None, None);
    }
    let mut ssid = None;
    let mut signal = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("SSID: ") {
            ssid = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("signal: ") {
            signal = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<i32>().ok());
        }
    }
    (ssid, signal)
}

pub async fn wifi_status() -> Value {
    let Some(interface) = wifi_interface() else {
        return json!({
            "present": false,
            "reason": "Bu kartta kablosuz arayüz görünmüyor",
        });
    };
    let operstate = read_trimmed(format!("/sys/class/net/{interface}/operstate"));
    let (ssid, signal) = link_of(&interface).await;
    let blocked = wifi_blocked().await;
    json!({
        "present": true,
        "interface": interface,
        "powered": blocked.map(|blocked| !blocked),
        "state": operstate,
        "connected": ssid.is_some(),
        "ssid": ssid,
        "signal": signal,
        "address": address_of(&interface),
        "remembered": remembered_networks().keys().cloned().collect::<Vec<_>>(),
    })
}

/// One network as the scan saw it.
fn network_json(ssid: &str, signal: i32, secured: bool, band: &str) -> Value {
    json!({ "ssid": ssid, "signal": signal, "secured": secured, "band": band })
}

pub async fn wifi_scan() -> Result<Value, String> {
    let interface = wifi_interface().ok_or("Bu kartta kablosuz arayüz yok")?;
    // A scan on a down interface returns nothing at all rather than failing,
    // which reads to the viewer as "there are no networks here".
    let _ = run(IP, &["link", "set", &interface, "up"]).await;
    let text = run_within(SCAN_TIMEOUT, IW, &["dev", &interface, "scan"]).await?;

    // One entry per BSS; the same SSID usually appears several times, once per
    // radio and per band, so they are merged and the strongest is kept.
    let mut best: std::collections::HashMap<String, (i32, bool, String)> =
        std::collections::HashMap::new();
    let mut ssid = String::new();
    let mut signal = 0i32;
    let mut secured = false;
    let mut band = String::new();
    let mut have = false;

    let mut flush = |ssid: &mut String, signal: &mut i32, secured: &mut bool, band: &mut String| {
        if !ssid.is_empty() {
            let entry = best
                .entry(std::mem::take(ssid))
                .or_insert((*signal, *secured, band.clone()));
            if *signal > entry.0 {
                *entry = (*signal, *secured, band.clone());
            }
        }
        *signal = 0;
        *secured = false;
        band.clear();
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("BSS ") {
            if have {
                flush(&mut ssid, &mut signal, &mut secured, &mut band);
            }
            have = true;
            ssid.clear();
        } else if let Some(rest) = trimmed.strip_prefix("SSID: ") {
            ssid = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("signal: ") {
            signal = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<f32>().ok())
                .map(|value| value as i32)
                .unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("freq: ") {
            // `iw` prints this as a float -- "freq: 5580.0" -- so parsing it as
            // an integer silently yields 0 and files every network under
            // 2,4 GHz. Measured on the Plus, where a 5580 MHz network was
            // being labelled 2,4 GHz in the scan list.
            let freq = rest.trim().parse::<f32>().unwrap_or(0.0);
            band = if freq >= 5000.0 { "5 GHz" } else { "2,4 GHz" }.to_string();
        } else if trimmed.starts_with("RSN:") || trimmed.starts_with("WPA:") {
            secured = true;
        }
    }
    if have {
        flush(&mut ssid, &mut signal, &mut secured, &mut band);
    }

    let remembered = remembered_networks();
    let mut networks: Vec<Value> = best
        .into_iter()
        .filter(|(ssid, _)| !ssid.is_empty())
        .map(|(ssid, (signal, secured, band))| {
            let mut entry = network_json(&ssid, signal, secured, &band);
            entry["remembered"] = json!(remembered.contains_key(&ssid));
            entry
        })
        .collect();
    // Strongest first: on a sofa the top of the list is the one you want.
    networks.sort_by(|a, b| {
        b["signal"]
            .as_i64()
            .unwrap_or(-999)
            .cmp(&a["signal"].as_i64().unwrap_or(-999))
    });
    Ok(json!({ "interface": interface, "networks": networks }))
}

// ------------------------------------------------- remembered networks + yaml

fn remembered_networks() -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(REMEMBERED)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_remembered(
    networks: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    if let Some(parent) = Path::new(REMEMBERED).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{} oluşturulamadı: {error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(networks).map_err(|error| error.to_string())?;
    write_private(REMEMBERED, &text)
}

/// A file only root may read. Both of these carry key material.
fn write_private(path: &str, contents: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("{path} yazılamadı: {error}"))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("{path} yazılamadı: {error}"))?;
    Ok(())
}

/// Render every remembered network into one netplan file.
///
/// `optional: true` matters: without it a board that cannot see its network —
/// moved to another room, router rebooted — waits for the link at every boot
/// before it reaches the television. `route-metric` matters for the same
/// reason a wired appliance should prefer its wire: both may be up, and the
/// cable is the one that carries 4K without thinking about it.
fn render_netplan(interface: &str) -> Result<bool, String> {
    let networks = remembered_networks();
    if networks.is_empty() {
        if Path::new(NETPLAN_FILE).exists() {
            std::fs::remove_file(NETPLAN_FILE)
                .map_err(|error| format!("{NETPLAN_FILE} silinemedi: {error}"))?;
        }
        return Ok(false);
    }
    let mut yaml = String::from(
        "# Written by mediaboxd-rs. Edited here only through the television's\n\
         # settings screen; anything added by hand is overwritten.\n\
         network:\n  version: 2\n  renderer: networkd\n  wifis:\n",
    );
    yaml.push_str(&format!("    {interface}:\n"));
    yaml.push_str("      dhcp4: true\n      dhcp6: true\n      optional: true\n");
    yaml.push_str("      dhcp4-overrides:\n        route-metric: 600\n");
    yaml.push_str("      dhcp6-overrides:\n        route-metric: 600\n");
    yaml.push_str("      access-points:\n");
    for (ssid, psk) in &networks {
        // Quoted and escaped: an SSID may contain spaces, a colon or a quote.
        let escaped = ssid.replace('\\', "\\\\").replace('"', "\\\"");
        yaml.push_str(&format!("        \"{escaped}\":\n"));
        if psk.is_empty() {
            yaml.push_str("          {}\n");
        } else {
            yaml.push_str(&format!("          password: \"{psk}\"\n"));
        }
    }
    write_private(NETPLAN_FILE, &yaml)?;
    Ok(true)
}

/// The key to write when the caller sent no passphrase: the one already
/// remembered for this network, or nothing at all for an open one.
fn keep_or_none(
    remembered: &std::collections::BTreeMap<String, String>,
    ssid: &str,
) -> String {
    remembered.get(ssid).cloned().unwrap_or_default()
}

/// Derive the PSK so the passphrase itself never reaches the disk.
async fn derive_psk(ssid: &str, passphrase: &str) -> Result<String, String> {
    // Already a derived key: 64 hex characters. Accepted as-is so a viewer who
    // pastes one is not forced through the derivation twice.
    if passphrase.len() == 64 && passphrase.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(passphrase.to_lowercase());
    }
    if passphrase.len() < 8 {
        return Err("Parola en az 8 karakter olmalı".into());
    }
    let text = run(WPA_PASSPHRASE, &[ssid, passphrase]).await.map_err(
        // Never let the tool's own echo of its arguments escape.
        |_| "Parola işlenemedi".to_string(),
    )?;
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("psk=").map(str::to_string))
        .filter(|psk| !psk.is_empty())
        .ok_or_else(|| "Parola işlenemedi".to_string())
}

pub async fn wifi_connect(ssid: &str, passphrase: Option<&str>) -> Result<Value, String> {
    let interface = wifi_interface().ok_or("Bu kartta kablosuz arayüz yok")?;
    if ssid.trim().is_empty() {
        return Err("Ağ adı boş".into());
    }
    let mut networks = remembered_networks();
    let psk = match passphrase {
        Some(phrase) if !phrase.is_empty() => derive_psk(ssid, phrase).await?,
        // Nothing typed: keep whatever this board already knows for this
        // network.
        //
        // This is how rejoining a remembered network erased its key. The
        // screen sends no passphrase when it does not need one — the point of
        // remembering it — and this arm used to write an empty string in its
        // place. netplan then rendered `"YuHome5": {}`, wpa_supplicant read
        // `key_mgmt=NONE`, and a WPA2 network the board had been sitting on
        // all afternoon became one it could not join at all. Found on the Plus
        // after a reboot, by reading the generated wpa config: 82 bytes, no
        // key in it.
        _ => keep_or_none(&networks, ssid),
    };
    let previous = networks.insert(ssid.to_string(), psk);
    write_remembered(&networks)?;
    render_netplan(&interface)?;

    // The radio has to be on before netplan can do anything with it.
    let _ = run(RFKILL, &["unblock", "wifi"]).await;
    if let Err(error) = run_within(CONNECT_TIMEOUT, NETPLAN, &["apply"]).await {
        // Put the remembered set back the way it was, so a failed attempt does
        // not leave a network the board will keep trying at every boot.
        let mut rollback = remembered_networks();
        match previous {
            Some(old) => {
                rollback.insert(ssid.to_string(), old);
            }
            None => {
                rollback.remove(ssid);
            }
        }
        let _ = write_remembered(&rollback);
        let _ = render_netplan(&interface);
        return Err(error);
    }

    // Association and a lease, polled rather than assumed.
    let deadline = std::time::Instant::now() + CONNECT_TIMEOUT;
    loop {
        let (on, _) = link_of(&interface).await;
        if on.as_deref() == Some(ssid) && address_of(&interface).is_some() {
            return Ok(wifi_status().await);
        }
        if std::time::Instant::now() >= deadline {
            let status = wifi_status().await;
            let joined = status
                .get("ssid")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(match joined {
                Some(name) if name == ssid => "Ağa katıldı ama adres alamadı".into(),
                _ => "Ağa bağlanılamadı — parola yanlış olabilir".to_string(),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn wifi_forget(ssid: &str) -> Result<Value, String> {
    let interface = wifi_interface().ok_or("Bu kartta kablosuz arayüz yok")?;
    let mut networks = remembered_networks();
    if networks.remove(ssid).is_none() {
        return Err("Bu ağ zaten kayıtlı değil".into());
    }
    write_remembered(&networks)?;
    render_netplan(&interface)?;
    let _ = run_within(CONNECT_TIMEOUT, NETPLAN, &["apply"]).await;
    Ok(wifi_status().await)
}

pub async fn wifi_disconnect() -> Result<Value, String> {
    let interface = wifi_interface().ok_or("Bu kartta kablosuz arayüz yok")?;
    // Leave the network remembered; this is "drop the link now", not "forget".
    let _ = run(IW, &["dev", &interface, "disconnect"]).await;
    let _ = run(IP, &["link", "set", &interface, "down"]).await;
    Ok(wifi_status().await)
}

pub async fn wifi_power(on: bool) -> Result<Value, String> {
    run(RFKILL, &[if on { "unblock" } else { "block" }, "wifi"]).await?;
    if on {
        if let Some(interface) = wifi_interface() {
            let _ = run(IP, &["link", "set", &interface, "up"]).await;
            // Re-apply, so a radio switched back on rejoins what it knows.
            if render_netplan(&interface)? {
                let _ = run_within(CONNECT_TIMEOUT, NETPLAN, &["apply"]).await;
            }
        }
    }
    Ok(wifi_status().await)
}

// ---------------------------------------------------------------- Bluetooth

/// `bluetoothctl` with one command, which is how every verb below is shaped.
async fn bctl(args: &[&str]) -> Result<String, String> {
    run(BLUETOOTHCTL, args).await
}

fn parse_controller(text: &str) -> Value {
    let mut address = None;
    let mut name = None;
    let mut powered = None;
    let mut discovering = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Controller ") {
            address = rest.split_whitespace().next().map(str::to_string);
        } else if let Some(rest) = trimmed.strip_prefix("Name: ") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("Powered: ") {
            powered = Some(rest.trim() == "yes");
        } else if let Some(rest) = trimmed.strip_prefix("Discovering: ") {
            discovering = Some(rest.trim() == "yes");
        }
    }
    json!({
        "address": address,
        "name": name,
        "powered": powered,
        "discovering": discovering,
    })
}

/// The devices bluez knows about, with what it knows about each.
async fn device_list() -> Vec<Value> {
    let Ok(text) = bctl(&["devices"]).await else {
        return Vec::new();
    };
    let mut devices = Vec::new();
    for line in text.lines() {
        let mut fields = line.trim().splitn(3, ' ');
        if fields.next() != Some("Device") {
            continue;
        }
        let Some(address) = fields.next() else {
            continue;
        };
        let name = fields.next().unwrap_or(address).trim().to_string();
        let info = bctl(&["info", address]).await.unwrap_or_default();
        let flag = |key: &str| {
            info.lines()
                .map(str::trim)
                .find_map(|line| line.strip_prefix(key))
                .map(|rest| rest.trim() == "yes")
                .unwrap_or(false)
        };
        devices.push(json!({
            "address": address,
            // bluez falls back to the address when no name was resolved; a row
            // reading "28-56-5A-A0-A1-8A" twice is no use on a television.
            "name": if name.replace('-', ":").eq_ignore_ascii_case(address) { Value::Null } else { json!(name) },
            "paired": flag("Paired:"),
            "trusted": flag("Trusted:"),
            "connected": flag("Connected:"),
            "icon": info
                .lines()
                .map(str::trim)
                .find_map(|line| line.strip_prefix("Icon: "))
                .map(|rest| rest.trim().to_string()),
        }));
    }
    devices
}

async fn bluetooth_blocked() -> Option<bool> {
    let text = run(RFKILL, &["--output", "TYPE,SOFT", "--noheadings"])
        .await
        .ok()?;
    let mut seen = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some("bluetooth") {
            let blocked = fields.next() == Some("blocked");
            seen = Some(seen.map_or(blocked, |prior: bool| prior && blocked));
        }
    }
    seen
}

pub async fn bluetooth_status() -> Value {
    let controller = bctl(&["show"])
        .await
        .map(|text| parse_controller(&text))
        .unwrap_or_else(|_| json!({}));
    let present = controller
        .get("address")
        .and_then(Value::as_str)
        .is_some_and(|address| !address.is_empty());
    json!({
        "present": present,
        "blocked": bluetooth_blocked().await,
        "controller": controller,
        "devices": if present { device_list().await } else { Vec::new() },
    })
}

pub async fn bluetooth_power(on: bool) -> Result<Value, String> {
    if on {
        let _ = run(RFKILL, &["unblock", "bluetooth"]).await;
        // The radio needs a moment after the GPIO goes high before bluez sees
        // a controller; on the Ultra the attach service has to run the
        // firmware down the UART first.
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    bctl(&["power", if on { "on" } else { "off" }]).await?;
    if !on {
        let _ = run(RFKILL, &["block", "bluetooth"]).await;
    }
    Ok(bluetooth_status().await)
}

pub async fn bluetooth_scan(seconds: u64) -> Result<Value, String> {
    let _ = bctl(&["power", "on"]).await;
    // `--timeout` makes bluetoothctl run discovery and exit by itself; without
    // it the command returns immediately and nothing is ever discovered.
    let limit = Duration::from_secs(seconds + 8);
    let seconds = seconds.to_string();
    let _ = run_within(limit, BLUETOOTHCTL, &["--timeout", &seconds, "scan", "on"]).await;
    Ok(bluetooth_status().await)
}

pub async fn bluetooth_pair(address: &str) -> Result<Value, String> {
    bctl(&["pair", address]).await?;
    // Trust it, or the device has to be authorised by hand every time it comes
    // back — which on a television means a remote that stops working after a
    // power cut and no way to say yes to it.
    let _ = bctl(&["trust", address]).await;
    let _ = bctl(&["connect", address]).await;
    Ok(bluetooth_status().await)
}

pub async fn bluetooth_connect(address: &str) -> Result<Value, String> {
    bctl(&["connect", address]).await?;
    Ok(bluetooth_status().await)
}

pub async fn bluetooth_disconnect(address: &str) -> Result<Value, String> {
    bctl(&["disconnect", address]).await?;
    Ok(bluetooth_status().await)
}

pub async fn bluetooth_remove(address: &str) -> Result<Value, String> {
    bctl(&["remove", address]).await?;
    Ok(bluetooth_status().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_controller_block_is_read_off_bluetoothctl() {
        let text = "Controller AA:BB:CC:DD:EE:FF (public)\n\tName: board\n\tPowered: yes\n\tDiscovering: no\n";
        let parsed = parse_controller(text);
        assert_eq!(parsed["address"], "AA:BB:CC:DD:EE:FF");
        assert_eq!(parsed["name"], "board");
        assert_eq!(parsed["powered"], true);
        assert_eq!(parsed["discovering"], false);
    }

    #[test]
    fn an_empty_controller_block_is_not_a_controller() {
        let parsed = parse_controller("No default controller available\n");
        assert!(parsed["address"].is_null());
    }

    #[test]
    fn rejoining_a_remembered_network_keeps_its_key() {
        let mut remembered = std::collections::BTreeMap::new();
        remembered.insert("home".to_string(), "abc123".to_string());
        // The screen sends no passphrase for a network the board knows; the
        // key it already has must survive that.
        assert_eq!(keep_or_none(&remembered, "home"), "abc123");
    }

    #[test]
    fn a_network_nobody_has_joined_gets_no_key() {
        let remembered = std::collections::BTreeMap::new();
        assert_eq!(keep_or_none(&remembered, "open"), "");
    }

    #[tokio::test]
    async fn a_derived_key_is_taken_as_it_is() {
        let key = "a".repeat(64);
        assert_eq!(derive_psk("net", &key).await.unwrap(), key);
    }

    #[tokio::test]
    async fn a_short_passphrase_is_refused_before_it_reaches_the_tool() {
        let error = derive_psk("net", "short").await.unwrap_err();
        assert!(error.contains("8 karakter"));
    }

    #[tokio::test]
    async fn a_refused_passphrase_is_not_echoed_back() {
        // Whatever goes wrong, the phrase itself must not turn up in the text
        // shown on a television or written to a journal.
        let error = derive_psk("net", "hunter2").await.unwrap_err();
        assert!(!error.contains("hunter2"));
    }
}
