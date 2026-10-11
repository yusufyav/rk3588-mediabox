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
//! and an address -- each with a time limit, one controller change at a time
//! and one verb per device; see the Bluetooth section and docs/bluetooth.md.

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const IW: &str = "/usr/sbin/iw";
const IP: &str = "/usr/sbin/ip";
const RFKILL: &str = "/usr/sbin/rfkill";
const NETPLAN: &str = "/usr/sbin/netplan";
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const WPA_CLI: &str = "/usr/sbin/wpa_cli";
const RESOLVECTL: &str = "/usr/bin/resolvectl";
/// Where wpa_cli opens the socket the supplicant answers on: somewhere both
/// processes see the same directory (see `wpa`).
const WPA_REPLY_DIR: &str = "/run/mediabox";
const WPA_PASSPHRASE: &str = "/usr/bin/wpa_passphrase";
const BLUETOOTHCTL: &str = "/usr/bin/bluetoothctl";

/// Written by this daemon and nothing else. The number puts it after
/// Armbian's own file, so the Ethernet rules stay exactly as they were.
const NETPLAN_FILE: &str = "/etc/netplan/20-mediabox-wifi.yaml";
/// The networks the viewer has joined, as {ssid: psk}. The passphrase is never
/// here — only what `wpa_passphrase` derived from it.
const REMEMBERED: &str = "/var/lib/mediabox/wifi-networks.json";
/// The remembered networks that do not announce themselves ("Gizli ağ"), as a
/// list of SSIDs: netplan has to be told to probe for them by name.
const HIDDEN: &str = "/var/lib/mediabox/wifi-hidden.json";
/// Where netplan's files are, this daemon's and everybody else's.
const NETPLAN_DIR: &str = "/etc/netplan";
/// A wireless-only file written by someone else is renamed to this once its
/// networks are this daemon's: netplan reads `*.yaml` only, and the original
/// stays on the disk for anyone who wants to know where a network came from.
const ADOPTED_SUFFIX: &str = ".mediabox-adopted";
/// Scratch space for rendering one foreign file on its own. The unit may write
/// here and nowhere else under /run that is not netplan's.
const ADOPT_SCRATCH: &str = "/run/mediabox/netplan-adopt";

/// How long a scan is given. Measured on both boards: a full two-band pass
/// comes back in four to six seconds, and a radio that has just been switched
/// on takes the long end of that.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);
/// How long `netplan apply` plus DHCP is given before the answer is "it did
/// not come up". Association is usually under two seconds; the lease is what
/// takes the rest.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(35);

/// One change to the radio at a time. Two joins that overlapped each ran
/// `netplan apply`, and the second restarted the supplicant 3 ms after the
/// first had associated -- measured on the Plus, the link going up and down
/// four times in a minute while every scan under it failed with EBUSY.
static CHANGING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The last full scan, kept for the screen that opens next. The kernel's own
/// cache forgets a network 30 s after it last heard it, and with nothing
/// scanning while the screen is shut it held only the network the board is
/// on: the list opened empty for a scan's length. A phone opens on a full
/// list; this is that list, refreshed by the scan that follows.
static LAST_SCAN: std::sync::Mutex<Option<(std::time::Instant, Vec<Seen>)>> =
    std::sync::Mutex::new(None);
/// One full scan at a time. A second asked for while one runs waits for it
/// and takes its answer: started on its own it met EBUSY and came back at once
/// with whatever the kernel held, which ended the screen's spinner early.
static SCANNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How old a remembered scan may be and still open the screen.
const LAST_SCAN_AGE: Duration = Duration::from_secs(10 * 60);

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
    let known = known_networks(&interface);
    let details = if ssid.is_some() { link_details(&interface).await } else { json!({}) };
    json!({
        "present": true,
        "interface": interface,
        "powered": blocked.map(|blocked| !blocked),
        "state": operstate,
        "connected": ssid.is_some(),
        "ssid": ssid,
        "signal": signal,
        "address": address_of(&interface),
        // What the details page of the network the board is on lists under
        // "Ağ ayrıntıları": frequency, MAC, mask, gateway, DNS.
        "details": details,
        // Every network the board joins by itself, whichever file wrote it
        // down -- not only the ones this daemon wrote.
        "remembered": known.keys().cloned().collect::<Vec<_>>(),
        // Of those, the ones written down with no key at all. Together with a
        // scan that says the network is secured, that is a network the board
        // will never join until somebody types its password again.
        "open": known
            .iter()
            .filter(|(_, open)| **open)
            .map(|(ssid, _)| ssid.clone())
            .collect::<Vec<_>>(),
    })
}

/// The facts of the link the board is on, for the network's details page.
async fn link_details(interface: &str) -> Value {
    let mac = read_trimmed(format!("/sys/class/net/{interface}/address"));
    let frequency = run(IW, &["dev", interface, "link"])
        .await
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("freq: ").map(|freq| band_of(freq).to_string()))
        });
    let prefix = run(IP, &["-4", "-o", "addr", "show", "dev", interface])
        .await
        .ok()
        .and_then(|text| {
            text.split_whitespace()
                .find(|word| word.contains('/') && word.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .and_then(|cidr| cidr.split('/').nth(1))
                .and_then(|bits| bits.parse::<u32>().ok())
        });
    let gateway = run(IP, &["-4", "route", "show", "default", "dev", interface])
        .await
        .ok()
        .and_then(|text| {
            let words: Vec<&str> = text.split_whitespace().collect();
            words.iter().position(|word| *word == "via").and_then(|at| words.get(at + 1)).map(|w| w.to_string())
        });
    let dns: Vec<String> = run(RESOLVECTL, &["dns", interface])
        .await
        .ok()
        .and_then(|text| text.split_once(':').map(|(_, servers)| servers.split_whitespace().map(str::to_string).collect()))
        .unwrap_or_default();
    json!({
        "mac": mac,
        "frequency": frequency,
        "netmask": prefix.map(netmask),
        "gateway": gateway,
        "dns": dns,
    })
}

/// A prefix length as the dotted mask Android lists ("255.255.255.0").
fn netmask(bits: u32) -> String {
    let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits.min(32)) };
    std::net::Ipv4Addr::from(mask).to_string()
}

/// One network as the scan saw it.
fn network_json(ssid: &str, signal: i32, secured: bool, band: &str) -> Value {
    json!({ "ssid": ssid, "signal": signal, "secured": secured, "band": band })
}

/// One network as a scan saw it: the strongest of its radios.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    ssid: String,
    signal: i32,
    /// A key has to be typed to join it.
    secured: bool,
    band: String,
    /// SettingsLib's name for what protects it: "WPA2-Personal", "Yok"…
    security: String,
    /// Every band one of its radios was heard on, for the "2.4/5GHz" mark.
    bands: std::collections::BTreeSet<String>,
}

/// What one BSS said about its protection, as `iw` prints it.
#[derive(Default, Clone)]
struct Protection {
    rsn: bool,
    wpa: bool,
    privacy: bool,
    psk: bool,
    sae: bool,
    owe: bool,
    eap: bool,
}

impl Protection {
    fn secured(&self) -> bool {
        self.psk || self.sae || self.eap || ((self.rsn || self.wpa) && !self.owe) || (self.privacy && !self.rsn && !self.wpa)
    }

    /// The words SettingsLib uses (values-tr: wifi_security_*).
    fn name(&self) -> &'static str {
        if self.eap {
            return "WPA/WPA2/WPA3-Enterprise";
        }
        if self.owe {
            return "Enhanced Open";
        }
        match (self.psk, self.sae, self.wpa, self.rsn) {
            (true, true, _, _) => "WPA2/WPA3-Personal",
            (false, true, _, _) => "WPA3-Personal",
            (true, false, true, true) => "WPA/WPA2-Personal",
            (true, false, true, false) => "WPA-Personal",
            (true, false, false, _) => "WPA2-Personal",
            _ if self.rsn => "WPA2-Personal",
            _ if self.wpa => "WPA-Personal",
            _ if self.privacy => "WEP",
            _ => "Yok",
        }
    }
}

/// `iw`'s scan text, one entry per network, strongest first.
///
/// One entry per BSS comes in; the same SSID usually appears several times,
/// once per radio and per band, so they are merged and the strongest is kept.
fn parse_scan(text: &str) -> Vec<Seen> {
    // One entry per BSS comes in; the same SSID usually appears several times,
    // once per radio and per band, so they are merged and the strongest is kept.
    let mut best: std::collections::HashMap<String, Seen> = std::collections::HashMap::new();
    let mut current: Option<(Seen, Protection)> = None;

    let mut flush = |current: &mut Option<(Seen, Protection)>| {
        if let Some((mut seen, protection)) = current.take()
            && !seen.ssid.is_empty()
        {
            seen.secured = protection.secured();
            seen.security = protection.name().to_string();
            if !seen.band.is_empty() {
                seen.bands.insert(seen.band.clone());
            }
            match best.get_mut(&seen.ssid) {
                Some(kept) if kept.signal >= seen.signal => {
                    kept.bands.extend(seen.bands);
                }
                Some(kept) => {
                    seen.bands.extend(std::mem::take(&mut kept.bands));
                    *kept = seen;
                }
                None => {
                    best.insert(seen.ssid.clone(), seen);
                }
            }
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("BSS ") {
            flush(&mut current);
            current = Some((
                Seen {
                    ssid: String::new(),
                    signal: 0,
                    secured: false,
                    band: String::new(),
                    security: String::new(),
                    bands: Default::default(),
                },
                Protection::default(),
            ));
            continue;
        }
        let Some((seen, protection)) = current.as_mut() else {
            continue;
        };
        if let Some(rest) = trimmed.strip_prefix("SSID: ") {
            seen.ssid = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("signal: ") {
            seen.signal = rest
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
            seen.band = band_of(rest).to_string();
        } else if let Some(rest) = trimmed.strip_prefix("capability: ") {
            protection.privacy = rest.split_whitespace().any(|word| word == "Privacy");
        } else if trimmed.starts_with("RSN:") {
            protection.rsn = true;
        } else if trimmed.starts_with("WPA:") {
            protection.wpa = true;
        } else if let Some(rest) = trimmed
            .strip_prefix("* Authentication suites:")
            .or_else(|| trimmed.strip_prefix("Authentication suites:"))
        {
            for suite in rest.split_whitespace() {
                match suite {
                    "PSK" | "PSK/SHA-256" | "FT/PSK" => protection.psk = true,
                    "SAE" | "FT/SAE" | "SAE-EXT-KEY" => protection.sae = true,
                    "OWE" => protection.owe = true,
                    "IEEE" | "802.1X" | "802.1X/SHA-256" | "FT/802.1X" => protection.eap = true,
                    _ => {}
                }
            }
        }
    }
    flush(&mut current);

    let mut seen: Vec<Seen> = best.into_values().collect();
    // Strongest first: on a sofa the top of the list is the one you want.
    seen.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
    seen
}

/// SettingsLib's band words (wifi_band_24ghz, wifi_band_5ghz).
fn band_of(freq: &str) -> &'static str {
    let freq = freq.trim().parse::<f32>().unwrap_or(0.0);
    if freq >= 5925.0 {
        "6 GHz"
    } else if freq >= 5000.0 {
        "5 GHz"
    } else {
        "2,4 GHz"
    }
}

/// What is in the air.
///
/// `cached` answers from what the kernel already holds from the supplicant's
/// own background scans: 8 ms on the Plus, where a fresh two-band pass takes
/// 11.8 s and comes back with a different count each time (22, 28, 28 BSS
/// measured back to back). A screen that opens on the cached list and then
/// refines it is usable at once instead of twelve seconds later. An empty
/// cache -- a radio that has just come up -- falls through to a real scan.
pub async fn wifi_scan(cached: bool) -> Result<Value, String> {
    let interface = wifi_interface().ok_or("Bu kartta kablosuz arayüz yok")?;
    // A scan on a down interface returns nothing at all rather than failing,
    // which reads to the viewer as "there are no networks here".
    let _ = run(IP, &["link", "set", &interface, "up"]).await;

    let mut seen = Vec::new();
    let mut from_cache = false;
    if cached && let Ok(text) = run(IW, &["dev", &interface, "scan", "dump"]).await {
        seen = parse_scan(&text);
        // What the kernel still holds wins; the remembered scan fills in what
        // it has forgotten.
        if let Some((at, last)) = LAST_SCAN.lock().unwrap().as_ref()
            && at.elapsed() < LAST_SCAN_AGE
        {
            for network in last {
                if !seen.iter().any(|heard| heard.ssid == network.ssid) {
                    seen.push(network.clone());
                }
            }
            seen.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
        }
        from_cache = !seen.is_empty();
    }
    if seen.is_empty() {
        let asked = std::time::Instant::now();
        // Held until this scan has answered.
        let _scanning = SCANNING.lock().await;
        let finished_meanwhile = LAST_SCAN
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(at, _)| *at >= asked)
            .map(|(_, last)| last.clone());
        if let Some(last) = finished_meanwhile {
            seen = last;
        } else {
            let fresh = run_within(SCAN_TIMEOUT, IW, &["dev", &interface, "scan"]).await;
            // A scan cut short -- the link dropping under it, as "Bağlantıyı
            // kes" does, ends one with "scan aborted!" and nothing after it --
            // or one that heard nothing is not an answer that the air is
            // empty. The supplicant scanning at the same moment answers
            // EBUSY. In each case what the kernel holds is the better answer.
            let heard = fresh.as_ref().ok().map(|text| parse_scan(text)).unwrap_or_default();
            if heard.is_empty() {
                seen = run(IW, &["dev", &interface, "scan", "dump"])
                    .await
                    .map(|text| parse_scan(&text))
                    .unwrap_or_default();
                from_cache = !seen.is_empty();
                if let Err(error) = fresh
                    && seen.is_empty()
                {
                    return Err(error);
                }
            } else {
                *LAST_SCAN.lock().unwrap() = Some((std::time::Instant::now(), heard.clone()));
                seen = heard;
            }
        }
    }

    let known = known_networks(&interface);
    let networks: Vec<Value> = seen
        .iter()
        .map(|network| {
            let mut entry =
                network_json(&network.ssid, network.signal, network.secured, &network.band);
            entry["security"] = json!(network.security);
            entry["bands"] = json!(network.bands);
            let open = known.get(&network.ssid).copied();
            entry["remembered"] = json!(open.is_some());
            // Written down with no key, in the air with one: joining it with
            // what the board has cannot work, so the screen asks again.
            entry["keyMissing"] = json!(network.secured && open == Some(true));
            entry
        })
        .collect();
    Ok(json!({ "interface": interface, "networks": networks, "cached": from_cache }))
}

// ------------------------------------------------- remembered networks + yaml

fn remembered_networks() -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(REMEMBERED)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn hidden_networks() -> std::collections::BTreeSet<String> {
    std::fs::read_to_string(HIDDEN)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_hidden(hidden: &std::collections::BTreeSet<String>) -> Result<(), String> {
    if hidden.is_empty() {
        let _ = std::fs::remove_file(HIDDEN);
        return Ok(());
    }
    let text = serde_json::to_string_pretty(hidden).map_err(|error| error.to_string())?;
    write_private(HIDDEN, &text)
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

// ------------------------------------------------ what the supplicant holds

/// One network block of a wpa_supplicant configuration.
#[derive(Debug, Clone, Default, PartialEq)]
struct Configured {
    ssid: String,
    /// `key_mgmt=NONE`: written down with no key.
    open: bool,
    /// The `psk=` value as written: 64 hex digits, or a quoted passphrase.
    psk: Option<String>,
}

/// wpa_supplicant's string escapes (`\"`, `\\`, `\n`, `\t`, `\xNN`), back to
/// the bytes they stand for. `wpa_cli list_networks` writes every byte past
/// ASCII this way, so a Turkish SSID only matches once it is undone.
fn unescape_wpa(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\' && at + 1 < bytes.len() {
            match bytes[at + 1] {
                b'n' => {
                    out.push(b'\n');
                    at += 2;
                }
                b't' => {
                    out.push(b'\t');
                    at += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    at += 2;
                }
                b'e' => {
                    out.push(0x1b);
                    at += 2;
                }
                b'x' if hex_byte(bytes.get(at + 2..at + 4)).is_some() => {
                    out.extend(hex_byte(bytes.get(at + 2..at + 4)));
                    at += 4;
                }
                other => {
                    out.push(other);
                    at += 2;
                }
            }
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_byte(pair: Option<&[u8]>) -> Option<u8> {
    u8::from_str_radix(std::str::from_utf8(pair?).ok()?, 16).ok()
}

/// The three spellings of an SSID in a configuration file: `"text"`,
/// `P"escaped text"` (what netplan writes) and bare hex.
fn wpa_ssid(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix("P\"").and_then(|rest| rest.strip_suffix('"')) {
        return Some(unescape_wpa(inner));
    }
    if let Some(inner) = value.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) {
        return Some(inner.to_string());
    }
    if !value.is_empty() && value.len().is_multiple_of(2) && value.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes: Option<Vec<u8>> = (0..value.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&value[at..at + 2], 16).ok())
            .collect();
        return bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    }
    None
}

fn parse_wpa_conf(text: &str) -> Vec<Configured> {
    let mut found = Vec::new();
    let mut current: Option<Configured> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("network={") {
            current = Some(Configured::default());
            continue;
        }
        if line == "}" {
            if let Some(block) = current.take()
                && !block.ssid.is_empty()
            {
                found.push(block);
            }
            continue;
        }
        let Some(block) = current.as_mut() else {
            continue;
        };
        if let Some(value) = line.strip_prefix("ssid=") {
            block.ssid = wpa_ssid(value).unwrap_or_default();
        } else if let Some(value) = line.strip_prefix("key_mgmt=") {
            block.open = value.trim() == "NONE";
        } else if let Some(value) = line.strip_prefix("psk=") {
            block.psk = Some(value.trim().to_string());
        }
    }
    found
}

/// Every network the board joins by itself, as {ssid: written down open}.
///
/// What netplan rendered for the supplicant -- whichever file it came from --
/// and what this daemon remembers. Reading only the second is how a network
/// from Armbian's first-login file was joined at every boot, drawn on the
/// television as a stranger and impossible to forget.
fn known_networks(interface: &str) -> BTreeMap<String, bool> {
    let mut known = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(format!("/run/netplan/wpa-{interface}.conf")) {
        for block in parse_wpa_conf(&text) {
            known.insert(block.ssid, block.open);
        }
    }
    for (ssid, psk) in remembered_networks() {
        known.entry(ssid).or_insert(psk.is_empty());
    }
    known
}

/// The key of a mapping line, quotes taken off; and what follows the colon.
fn yaml_entry(line: &str) -> (String, &str) {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('"')
        && let Some(close) = rest.find('"')
    {
        let after = rest[close + 1..].trim_start();
        return (rest[..close].to_string(), after.strip_prefix(':').unwrap_or(after).trim());
    }
    match line.split_once(':') {
        Some((key, value)) => (key.trim().to_string(), value.trim()),
        None => (line.to_string(), ""),
    }
}

/// Whether a netplan file says nothing but which networks one wireless
/// interface joins, with DHCP -- the shape Armbian's first-login script
/// writes. Only such a file is folded into this daemon's own; a file with
/// anything more in it is somebody's configuration and stays theirs.
fn wifi_only(text: &str, interface: &str) -> bool {
    // (indent, key) of the mapping each line sits in.
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut has_networks = false;
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        while path.last().is_some_and(|(at, _)| *at >= indent) {
            path.pop();
        }
        let (key, value) = yaml_entry(trimmed);
        let allowed = match path.len() {
            0 => key == "network",
            1 => match key.as_str() {
                "version" | "wifis" => true,
                "renderer" => value == "networkd",
                _ => false,
            },
            2 => path[1].1 == "wifis" && key == interface,
            3 => matches!(
                key.as_str(),
                "dhcp4" | "dhcp6" | "optional" | "ipv6-privacy" | "access-points"
                    | "dhcp4-overrides" | "dhcp6-overrides"
            ),
            _ => matches!(
                path[3].1.as_str(),
                "access-points" | "dhcp4-overrides" | "dhcp6-overrides"
            ),
        };
        if !allowed {
            return false;
        }
        if path.len() == 4 && path[3].1 == "access-points" {
            has_networks = true;
        }
        path.push((indent, key));
    }
    has_networks
}

/// What netplan renders for the supplicant from one file on its own.
async fn rendered_alone(file: &Path, interface: &str) -> Result<Vec<Configured>, String> {
    let root = PathBuf::from(ADOPT_SCRATCH);
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("etc/netplan");
    std::fs::create_dir_all(&dir).map_err(|error| format!("{} oluşturulamadı: {error}", dir.display()))?;
    let name = file.file_name().ok_or("dosya adı yok")?;
    std::fs::copy(file, dir.join(name))
        .map_err(|error| format!("{} kopyalanamadı: {error}", file.display()))?;
    let generated = run(NETPLAN, &["generate", "--root-dir", ADOPT_SCRATCH]).await;
    let text = std::fs::read_to_string(root.join(format!("run/netplan/wpa-{interface}.conf")));
    // Key material: gone as soon as it has been read.
    let _ = std::fs::remove_dir_all(&root);
    generated?;
    Ok(text.map(|text| parse_wpa_conf(&text)).unwrap_or_default())
}

/// Fold every wireless-only netplan file written by someone else into this
/// daemon's list, and set the file aside.
///
/// Found on the Plus: Armbian's `30-wifis-dhcp.yaml` named a network this
/// daemon knew nothing about. The board joined it at boot; the television drew
/// it as unknown and could not forget it. Once its networks are here there is
/// one list again, the one the screen edits. A file with anything else in it
/// is left exactly as it is.
async fn adopt_foreign(interface: &str) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(NETPLAN_DIR) else {
        return Ok(());
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "yaml")
                && path != Path::new(NETPLAN_FILE)
        })
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !wifi_only(&text, interface) {
            continue;
        }
        let found = rendered_alone(&file, interface).await?;
        let mut networks = remembered_networks();
        for block in found {
            // What the viewer typed here wins over what was there before.
            if networks.contains_key(&block.ssid) {
                continue;
            }
            let psk = match block.psk.as_deref() {
                None => String::new(),
                Some(quoted) if quoted.starts_with('"') => {
                    let phrase = quoted
                        .strip_prefix('"')
                        .and_then(|rest| rest.strip_suffix('"'))
                        .unwrap_or(quoted);
                    derive_psk(&block.ssid, phrase).await?
                }
                Some(hex) => hex.to_lowercase(),
            };
            networks.insert(block.ssid, psk);
        }
        write_remembered(&networks)?;
        render_netplan(interface)?;
        let aside = PathBuf::from(format!("{}{ADOPTED_SUFFIX}", file.display()));
        std::fs::rename(&file, &aside)
            .map_err(|error| format!("{} kenara alınamadı: {error}", file.display()))?;
        eprintln!(
            "mediaboxd-rs: {} içindeki Wi-Fi ağları devralındı ({})",
            file.display(),
            aside.display()
        );
    }
    Ok(())
}

/// Bring what is under /run in line with /etc/netplan without touching the
/// running supplicant. `netplan generate` cannot do it from this unit --
/// measured: "cannot create file /run/systemd/system/netplan-ovs-cleanup.service:
/// Read-only file system" -- but netplan's generator runs in PID 1 on a
/// reload, which is exactly what `netplan apply` itself relies on.
async fn regenerate() -> Result<(), String> {
    run_within(CONNECT_TIMEOUT, SYSTEMCTL, &["daemon-reload"]).await.map(|_| ())
}

/// One verb to the supplicant netplan started for this interface. It says
/// "FAIL" on stdout, with a zero exit, when it refuses.
///
/// `-s` puts wpa_cli's own reply socket in /run/mediabox. Its default is /tmp,
/// which under this unit's PrivateTmp is a directory the supplicant cannot
/// see: the request arrived, the answer went nowhere, and every call waited
/// out its timeout. Measured on the Plus -- that wait sent "Ağı unut" down the
/// `netplan apply` fallback, which restarted the supplicant and dropped the
/// link the box was reached over.
async fn wpa(interface: &str, args: &[&str]) -> Result<String, String> {
    let mut full = vec!["-s", WPA_REPLY_DIR, "-i", interface];
    full.extend_from_slice(args);
    let text = run_within(Duration::from_secs(5), WPA_CLI, &full).await?;
    if text.trim_start().starts_with("FAIL") {
        return Err(format!("wpa_supplicant {} isteğini reddetti", args.first().unwrap_or(&"")));
    }
    Ok(text)
}

/// `list_networks`, as (id, ssid).
fn parse_network_ids(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next()?.trim();
            let ssid = fields.next()?;
            (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
                .then(|| (id.to_string(), unescape_wpa(ssid)))
        })
        .collect()
}

/// The supplicant's own number for a network, if it holds one. `Err` is "the
/// supplicant could not be asked", which is not the same as "it does not
/// have it".
async fn live_id(interface: &str, ssid: &str) -> Result<Option<String>, String> {
    let text = wpa(interface, &["list_networks"]).await?;
    Ok(parse_network_ids(&text)
        .into_iter()
        .find(|(_, name)| name == ssid)
        .map(|(id, _)| id))
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
    let hidden = hidden_networks();
    for (ssid, psk) in &networks {
        // Quoted and escaped: an SSID may contain spaces, a colon or a quote.
        let escaped = ssid.replace('\\', "\\\\").replace('"', "\\\"");
        yaml.push_str(&format!("        \"{escaped}\":\n"));
        let probe = hidden.contains(ssid);
        if psk.is_empty() && !probe {
            yaml.push_str("          {}\n");
        }
        if probe {
            // Measured with netplan 1.1: renders scan_ssid=1.
            yaml.push_str("          hidden: true\n");
        }
        if !psk.is_empty() {
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

pub async fn wifi_connect(
    ssid: &str,
    passphrase: Option<&str>,
    hidden: bool,
) -> Result<Value, String> {
    let _changing = CHANGING.lock().await;
    let interface = wifi_interface().ok_or("Bu kartta kablosuz arayüz yok")?;
    if ssid.trim().is_empty() {
        return Err("Ağ adı boş".into());
    }
    // Before anything is written: a network from someone else's file is joined
    // with no passphrase sent, and the key it keeps must be the one it had.
    if let Err(error) = adopt_foreign(&interface).await {
        eprintln!("mediaboxd-rs: Wi-Fi dosyaları devralınamadı: {error}");
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
    // Rejoining with the key it already had changes nothing, and a failure
    // then has nothing to undo.
    let unchanged = networks.get(ssid) == Some(&psk) && (!hidden || hidden_networks().contains(ssid));
    let previous = networks.insert(ssid.to_string(), psk);
    write_remembered(&networks)?;
    if hidden {
        let mut probe = hidden_networks();
        probe.insert(ssid.to_string());
        write_hidden(&probe)?;
    }
    render_netplan(&interface)?;

    // The radio has to be on before netplan can do anything with it.
    let _ = run(RFKILL, &["unblock", "wifi"]).await;
    if let Err(error) = run_within(CONNECT_TIMEOUT, NETPLAN, &["apply"]).await {
        // Put the remembered set back the way it was, so a failed attempt does
        // not leave a network the board will keep trying at every boot.
        restore(&interface, ssid, previous).await;
        return Err(error);
    }

    // Asked for this one, not whichever the supplicant would pick. Rejoining a
    // remembered network changes nothing on disk, so `apply` restarts nothing
    // and the board stayed where it was until the deadline said "could not
    // join". `select_network` sets the others aside for the attempt; they are
    // put back below whatever happens.
    let selected = select(&interface, ssid).await;

    // Association and a lease, polled rather than assumed.
    let deadline = std::time::Instant::now() + CONNECT_TIMEOUT;
    let outcome = loop {
        let (on, _) = link_of(&interface).await;
        if on.as_deref() == Some(ssid) && address_of(&interface).is_some() {
            break Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let (joined, _) = link_of(&interface).await;
            break Err(joined.as_deref() == Some(ssid));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    if selected {
        let _ = wpa(&interface, &["enable_network", "all"]).await;
    }
    match outcome {
        Ok(()) => Ok(wifi_status().await),
        Err(true) => Err("Ağa katıldı ama adres alamadı".into()),
        Err(false) => {
            // A key that did not work is not kept: the network goes back to
            // what it was -- unknown, or its old key.
            if !unchanged {
                restore(&interface, ssid, previous).await;
            }
            Err("Ağa bağlanılamadı — parola yanlış olabilir".into())
        }
    }
}

/// The supplicant told to go to this network now. Answers whether the other
/// networks were set aside and need enabling again. Right after `apply` the
/// supplicant may still be starting, so it is given a few seconds to answer.
async fn select(interface: &str, ssid: &str) -> bool {
    for _ in 0..10 {
        match live_id(interface, ssid).await {
            Ok(Some(id)) => return wpa(interface, &["select_network", &id]).await.is_ok(),
            Ok(None) | Err(_) => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
    // A supplicant that was told to stay off the air keeps to it otherwise.
    let _ = wpa(interface, &["reconnect"]).await;
    false
}

/// Undo one network's entry after a failed join.
async fn restore(interface: &str, ssid: &str, previous: Option<String>) {
    let mut rollback = remembered_networks();
    let had = previous.is_some();
    match previous {
        Some(old) => {
            rollback.insert(ssid.to_string(), old);
        }
        None => {
            rollback.remove(ssid);
        }
    }
    let _ = write_remembered(&rollback);
    let _ = render_netplan(interface);
    if had {
        // The old key back in the running supplicant too.
        let _ = run_within(CONNECT_TIMEOUT, NETPLAN, &["apply"]).await;
    } else {
        let _ = regenerate().await;
        if let Ok(Some(id)) = live_id(interface, ssid).await {
            let _ = wpa(interface, &["remove_network", &id]).await;
        }
    }
}

pub async fn wifi_forget(ssid: &str) -> Result<Value, String> {
    let _changing = CHANGING.lock().await;
    let interface = wifi_interface().ok_or("Bu kartta kablosuz arayüz yok")?;
    if let Err(error) = adopt_foreign(&interface).await {
        eprintln!("mediaboxd-rs: Wi-Fi dosyaları devralınamadı: {error}");
    }
    let mut networks = remembered_networks();
    if networks.remove(ssid).is_none() {
        // Known to the board but not ours to edit: a file with more in it
        // than networks, which adoption deliberately leaves alone.
        return Err(if known_networks(&interface).contains_key(ssid) {
            "Bu ağ sistemin başka bir ağ dosyasında tanımlı; buradan kaldırılamıyor".into()
        } else {
            "Bu ağ zaten kayıtlı değil".into()
        });
    }
    write_remembered(&networks)?;
    let mut probe = hidden_networks();
    if probe.remove(ssid) {
        write_hidden(&probe)?;
    }
    render_netplan(&interface)?;
    // The next boot, and /run, without the network.
    if let Err(error) = regenerate().await {
        eprintln!("mediaboxd-rs: netplan yeniden üretilemedi: {error}");
    }
    // And the running supplicant, without restarting it: `netplan apply`
    // restarts it, which drops the link the board is on -- seven seconds
    // measured -- even when the network being forgotten is another one. If it
    // is the one the board is on, the supplicant moves to the next it knows.
    let live = match live_id(&interface, ssid).await {
        Ok(Some(id)) => wpa(&interface, &["remove_network", &id]).await.map(|_| ()),
        Ok(None) => Ok(()),
        Err(error) => Err(error),
    };
    if live.is_err() {
        let _ = run_within(CONNECT_TIMEOUT, NETPLAN, &["apply"]).await;
    }
    Ok(wifi_status().await)
}

pub async fn wifi_disconnect() -> Result<Value, String> {
    let _changing = CHANGING.lock().await;
    let interface = wifi_interface().ok_or("Bu kartta kablosuz arayüz yok")?;
    // Leave the network remembered; this is "drop the link now", not "forget".
    //
    // The supplicant's own verb: it stays off the air until it is asked to
    // join again or the board restarts. `iw disconnect` was undone by the
    // supplicant a second later, and taking the interface down with it, as
    // this used to, also emptied every scan until something brought it up.
    if wpa(&interface, &["disconnect"]).await.is_err() {
        let _ = run(IW, &["dev", &interface, "disconnect"]).await;
    }
    Ok(wifi_status().await)
}

pub async fn wifi_power(on: bool) -> Result<Value, String> {
    let _changing = CHANGING.lock().await;
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
//
// Measured on the Plus (RTL8852BU on USB, bluez 5.82): `show`, `devices` and
// an `info` for each of three devices answer in 83 ms together; power off and
// on take 85 and 197 ms; a search is running 94 ms after it is asked for and
// stops 199 ms after the bluetoothctl that owns it goes away. None of that is
// slow enough to want a resident D-Bus client, so this stays on bluetoothctl.
// What needed fixing was around it: no verb had a time limit (`connect` to a
// keyboard that was switched off had not returned after 40 s, and the screen
// said "Bağlanıyor…" for as long), a search that failed exited 0 and said so
// only in text nobody read, nothing kept two verbs from crossing, and bluez
// forgets a found device 30 s after it last heard it, so the list emptied
// under the viewer.

/// How long each verb is given.
const BT_QUERY: Duration = Duration::from_secs(5);
const BT_POWER: Duration = Duration::from_secs(10);
/// Pairing waits on the other device and, for a keyboard, on a person.
const BT_PAIR: Duration = Duration::from_secs(35);
const BT_CONNECT: Duration = Duration::from_secs(20);
/// Disconnect, remove, trust.
const BT_LINK: Duration = Duration::from_secs(10);
/// One search. bluez keeps what it found for 30 s after it last heard it.
const BT_SEARCH_SECONDS: &str = "12";
/// How long a found device stays on the list after bluez has forgotten it:
/// two searches' worth, so one that misses it does not move the list.
const BT_FOUND_AGE: Duration = Duration::from_secs(120);
/// How long a pairing listens for a device bluez had forgotten.
const BT_REFIND: Duration = Duration::from_secs(10);
/// How long a new link is watched before it counts as connected.
const BT_SETTLE: Duration = Duration::from_secs(2);
/// What a time limit reads as, so it can be told from bluez's own errors.
const BT_TIMED_OUT: &str = "zaman aşımı";

/// Power and recovery hold the controller alone; the per-device verbs and the
/// search share it.
static BT_CONTROLLER: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());
/// Pairability is the controller's, so one pairing at a time opens and shuts
/// it.
static BT_PAIRING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// The addresses a verb is running on. A second verb on one of them is
/// refused rather than sent to bluez on top of the first.
static BT_BUSY: std::sync::Mutex<BTreeSet<String>> = std::sync::Mutex::new(BTreeSet::new());
/// The running search: the bluetoothctl that holds discovery open. bluez ties
/// a discovery to the client that asked for it, so ending this ends it.
static BT_SEARCH: std::sync::Mutex<Option<tokio::process::Child>> = std::sync::Mutex::new(None);
/// One search started at a time: two asked for together would each start one.
static BT_STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// What the searches found, by address.
static BT_FOUND: std::sync::Mutex<BTreeMap<String, Found>> = std::sync::Mutex::new(BTreeMap::new());
/// Each device's icon, asked of `info` once: what a device is does not change.
static BT_ICONS: std::sync::Mutex<BTreeMap<String, Option<String>>> =
    std::sync::Mutex::new(BTreeMap::new());

#[derive(Debug, Clone)]
struct Found {
    name: Option<String>,
    icon: Option<String>,
    heard: std::time::Instant,
}

/// A verb running on one address, for as long as this is held.
struct Claim(String);

impl Drop for Claim {
    fn drop(&mut self) {
        BT_BUSY.lock().expect("bluetooth busy").remove(&self.0);
    }
}

fn claim(address: &str) -> Result<Claim, String> {
    let address = address.trim().to_ascii_uppercase();
    if !is_address(&address) {
        return Err("Geçersiz Bluetooth adresi".into());
    }
    if !BT_BUSY.lock().expect("bluetooth busy").insert(address.clone()) {
        return Err("Bu cihazda bir işlem zaten sürüyor".into());
    }
    Ok(Claim(address))
}

/// Six pairs of hex digits: the only shape an address reaches bluetoothctl in.
fn is_address(text: &str) -> bool {
    let parts: Vec<&str> = text.split(':').collect();
    parts.len() == 6
        && parts.iter().all(|part| part.len() == 2 && part.chars().all(|c| c.is_ascii_hexdigit()))
}

/// bluetoothctl's colours and prompt, off its text.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c != '\u{1}' && c != '\u{2}' {
            out.push(c);
        }
    }
    out
}

/// `bluetoothctl` with one command, within `limit`. A failure is a non-zero
/// exit or bluez's error in the text: a search that cannot start exits 0.
/// Run past its limit, the child is killed with the future.
async fn bctl(limit: Duration, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(BLUETOOTHCTL);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(limit, command.output()).await {
        Err(_) => return Err(BT_TIMED_OUT.into()),
        Ok(Err(error)) => return Err(format!("bluetoothctl çalıştırılamadı: {error}")),
        Ok(Ok(output)) => output,
    };
    let text = plain(&format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ));
    let refused = text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("Failed to") || line.contains("org.bluez.Error.") || line.contains("not available")
    });
    if output.status.success() && !refused {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

/// bluez's answer in words a viewer can act on. Nothing here is a secret --
/// bluetoothctl never prints a key -- but the raw line is no use on a sofa.
fn bt_error(raw: &str) -> String {
    let has = |needle: &str| raw.contains(needle);
    if raw == BT_TIMED_OUT || has("AuthenticationTimeout") {
        "cihaz yanıt vermedi".into()
    } else if has("profile-unavailable") || has("NotSupported") {
        "bu cihazın türü bu kutuda desteklenmiyor".into()
    } else if has("AuthenticationFailed") || has("AuthenticationRejected") {
        "cihaz eşleşmeyi reddetti".into()
    } else if has("AuthenticationCanceled") {
        "eşleştirme iptal edildi".into()
    } else if has("not available") || has("DoesNotExist") {
        "cihaz şu an görünmüyor; açık ve eşleştirme modunda mı?".into()
    } else if has("page-timeout") || has("Host is down") || has("connection-abort") || has("ConnectionAttemptFailed") {
        "cihaz yanıt vermedi; açık ve yakında mı?".into()
    } else if has("InProgress") {
        "bu cihazda bir işlem zaten sürüyor".into()
    } else if has("NotReady") || has("mgmt_socket") {
        "Bluetooth denetleyicisi hazır değil".into()
    } else {
        // bluez's own last line, which names the error.
        raw.lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("bilinmeyen hata")
            .to_string()
    }
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

/// `devices` as (address, name). bluez falls back to the address when no
/// name was resolved; a row reading "28-56-5A-A0-A1-8A" is no use on a
/// television, so that is no name.
fn parse_devices(text: &str) -> Vec<(String, Option<String>)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.trim().splitn(3, ' ');
            if fields.next() != Some("Device") {
                return None;
            }
            let address = fields.next().filter(|address| is_address(address))?.to_string();
            let name = fields
                .next()
                .map(str::trim)
                .filter(|name| !name.is_empty() && !name.replace('-', ":").eq_ignore_ascii_case(&address))
                .map(str::to_string);
            Some((address, name))
        })
        .collect()
}

/// A `yes`/`no` line of `info`.
fn info_flag(info: &str, key: &str) -> bool {
    info.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(key))
        .is_some_and(|rest| rest.trim() == "yes")
}

fn info_icon(info: &str) -> Option<String> {
    info.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Icon: "))
        .map(|rest| rest.trim().to_string())
}

/// "Battery Percentage: 0x50 (80)", which bluez reports for a connected
/// device with a battery service.
fn info_battery(info: &str) -> Option<u8> {
    info.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("Battery Percentage: "))
        .and_then(|rest| rest.split('(').nth(1))
        .and_then(|rest| rest.trim_end_matches(')').trim().parse().ok())
}

/// The devices bluez knows about, with what it knows about each, and the ones
/// a recent search found that bluez has dropped since.
///
/// Three listings and an `info` only where one is needed -- a connected
/// device, for its battery, and a device met for the first time, for its
/// icon -- where this used to ask `info` of every device on every look.
async fn device_list() -> Vec<Value> {
    let (all, paired, connected) = tokio::join!(
        bctl(BT_QUERY, &["devices"]),
        bctl(BT_QUERY, &["devices", "Paired"]),
        bctl(BT_QUERY, &["devices", "Connected"]),
    );
    let addresses = |text: Result<String, String>| -> BTreeSet<String> {
        parse_devices(&text.unwrap_or_default())
            .into_iter()
            .map(|(address, _)| address)
            .collect()
    };
    let paired = addresses(paired);
    let connected = addresses(connected);
    let now = std::time::Instant::now();
    let mut devices = Vec::new();
    let mut listed = BTreeSet::new();
    for (address, name) in parse_devices(&all.unwrap_or_default()) {
        let is_connected = connected.contains(&address);
        let cached = BT_ICONS.lock().expect("bluetooth icons").get(&address).cloned();
        let (icon, battery) = if is_connected || cached.is_none() {
            let info = bctl(BT_QUERY, &["info", &address]).await.unwrap_or_default();
            let icon = info_icon(&info);
            BT_ICONS
                .lock()
                .expect("bluetooth icons")
                .insert(address.clone(), icon.clone());
            (icon, info_battery(&info))
        } else {
            (cached.flatten(), None)
        };
        let is_paired = paired.contains(&address);
        if !is_paired {
            BT_FOUND.lock().expect("bluetooth found").insert(
                address.clone(),
                Found { name: name.clone(), icon: icon.clone(), heard: now },
            );
        }
        devices.push(json!({
            "address": address,
            "name": name,
            "paired": is_paired,
            "connected": is_connected,
            "icon": icon,
            "battery": battery,
            "inRange": true,
        }));
        listed.insert(address);
    }
    let mut found = BT_FOUND.lock().expect("bluetooth found");
    found.retain(|address, seen| {
        now.duration_since(seen.heard) < BT_FOUND_AGE && !paired.contains(address)
    });
    for (address, seen) in found.iter().filter(|(address, _)| !listed.contains(*address)) {
        devices.push(json!({
            "address": address,
            "name": seen.name,
            "paired": false,
            "connected": false,
            "icon": seen.icon,
            "battery": null,
            "inRange": false,
        }));
    }
    devices
}

async fn bluetooth_blocked() -> Option<bool> {
    rfkill_state("SOFT").await
}

/// Whether every Bluetooth radio is blocked in `column` (SOFT or HARD).
async fn rfkill_state(column: &str) -> Option<bool> {
    let columns = format!("TYPE,{column}");
    let text = run(RFKILL, &["--output", &columns, "--noheadings"])
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

/// Whether bluez can see a controller at all.
async fn controller_present() -> bool {
    bctl(BT_QUERY, &["show"])
        .await
        .map(|text| text.contains("Controller "))
        .unwrap_or(false)
}

/// Bring back a controller bluez lists but cannot power: the UART part on
/// the Ultra.
///
/// Dropping the GPIO there takes the firmware with it and the part comes back
/// mute: the controller is still listed -- `bluetoothctl show` prints it even
/// while hci0 sits DOWN, which is why asking "is there a controller" is not
/// the test -- but powering it answers "Unable to open mgmt_socket", and after
/// the firmware is pushed again, "org.bluez.Error.Failed" until bluez itself
/// is restarted. Measured on the Ultra; without this, switching Bluetooth off
/// from the settings screen and on again loses it until a reboot.
///
/// Only a board that has one of these units is touched. The Plus has none --
/// its Bluetooth is on USB and powers off and on in 85 and 197 ms -- so on it
/// this restarts nothing, bluez included. One attempt per power-on, under the
/// controller's lock, so two never run at once.
async fn recover_controller() {
    let started = std::time::Instant::now();
    for unit in [
        "ap6611s-bluetooth.service",
        "brcm-bluetooth.service",
        "hciuart.service",
    ] {
        if run(SYSTEMCTL, &["cat", unit]).await.is_err() {
            continue;
        }
        eprintln!("mediaboxd.bluetooth recovery: controller does not power on; re-attaching via {unit}");
        if run(SYSTEMCTL, &["restart", unit]).await.is_err() {
            continue;
        }
        // Firmware down a 1.5 Mbaud line takes a few seconds.
        tokio::time::sleep(Duration::from_millis(5000)).await;
        // And bluez has to be restarted on top of it: it was holding a
        // management socket on the controller that went away with the
        // firmware. The pairing agent follows it by BindsTo.
        let _ = run(SYSTEMCTL, &["restart", "bluetooth.service"]).await;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if controller_present().await {
                break;
            }
        }
        eprintln!(
            "mediaboxd.bluetooth recovery: done in {} ms",
            started.elapsed().as_millis()
        );
        return;
    }
    eprintln!("mediaboxd.bluetooth recovery: no attach unit on this board; bluez left alone");
}

/// Whether a search is running, reaping the one that has ended.
fn searching() -> bool {
    let mut slot = BT_SEARCH.lock().expect("bluetooth search");
    match slot.as_mut().map(tokio::process::Child::try_wait) {
        Some(Ok(None)) => true,
        Some(_) => {
            *slot = None;
            false
        }
        None => false,
    }
}

/// End the search, and wait the moment bluez takes to stop it.
async fn stop_search() {
    let child = BT_SEARCH.lock().expect("bluetooth search").take();
    let Some(mut child) = child else {
        return;
    };
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    for _ in 0..20 {
        let discovering = bctl(BT_QUERY, &["show"])
            .await
            .map(|text| parse_controller(&text)["discovering"] == true)
            .unwrap_or(false);
        if !discovering {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Start a search unless one is running, and come back once bluez says it is
/// discovering -- or with why it is not.
async fn start_search() -> Result<(), String> {
    let _starting = BT_STARTING.lock().await;
    if searching() {
        return Ok(());
    }
    let mut child = Command::new(BLUETOOTHCTL)
        .args(["--timeout", BT_SEARCH_SECONDS, "scan", "on"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("bluetoothctl çalıştırılamadı: {error}"))?;
    let started = std::time::Instant::now();
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return Err("Arama başlatılamadı".into());
        }
        let controller = bctl(BT_QUERY, &["show"])
            .await
            .map(|text| parse_controller(&text))
            .unwrap_or_else(|_| json!({}));
        if controller["discovering"] == true {
            break;
        }
        if started.elapsed() > Duration::from_secs(3) {
            let _ = child.start_kill();
            return Err("Arama başlatılamadı: denetleyici yanıt vermedi".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eprintln!(
        "mediaboxd.bluetooth search started in {} ms",
        started.elapsed().as_millis()
    );
    *BT_SEARCH.lock().expect("bluetooth search") = Some(child);
    Ok(())
}

pub async fn bluetooth_status() -> Value {
    let controller = bctl(BT_QUERY, &["show"])
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
        "scanning": searching(),
        "devices": if present { device_list().await } else { Vec::new() },
    })
}

pub async fn bluetooth_power(on: bool) -> Result<Value, String> {
    let _controller = BT_CONTROLLER.write().await;
    if on {
        let _ = run(RFKILL, &["unblock", "bluetooth"]).await;
        // The radio needs a moment after the GPIO goes high before bluez sees
        // a controller: asked for until it does, not slept on.
        for _ in 0..30 {
            if controller_present().await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    } else {
        stop_search().await;
    }
    let verb = if on { "on" } else { "off" };
    if let Err(error) = bctl(BT_POWER, &["power", verb]).await {
        if !on {
            // Nothing to rescue on the way down.
            return Ok(bluetooth_status().await);
        }
        if rfkill_state("HARD").await == Some(true) {
            return Err("Bluetooth donanım tarafından kapalı".into());
        }
        eprintln!("mediaboxd.bluetooth power on failed: {}", bt_error(&error));
        recover_controller().await;
        bctl(BT_POWER, &["power", verb])
            .await
            .map_err(|error| format!("Bluetooth denetleyicisi yanıt vermiyor: {}", bt_error(&error)))?;
    }
    if on {
        // On, but not open: an appliance that sits there pairable is one
        // anybody in the building can pair with. Pairable only for the length
        // of a pairing the viewer asked for; discoverable never.
        let _ = bctl(BT_QUERY, &["pairable", "off"]).await;
        let _ = bctl(BT_QUERY, &["discoverable", "off"]).await;
    } else {
        let _ = run(RFKILL, &["block", "bluetooth"]).await;
    }
    Ok(bluetooth_status().await)
}

/// Start a search, or with `stop`, end the one running. Answers at once: what
/// the search finds reaches the screen through the status it asks for while
/// it runs, so the list fills as devices are heard rather than all at once
/// twelve seconds later.
pub async fn bluetooth_scan(stop: bool) -> Result<Value, String> {
    if stop {
        stop_search().await;
        return Ok(bluetooth_status().await);
    }
    let _controller = BT_CONTROLLER.read().await;
    // It used to power the radio on by itself here; a search does not
    // overrule the switch the viewer set.
    let controller = bctl(BT_QUERY, &["show"])
        .await
        .map(|text| parse_controller(&text))
        .map_err(|error| bt_error(&error))?;
    if controller["powered"] != true {
        return Err("Bluetooth kapalı".into());
    }
    start_search().await?;
    Ok(bluetooth_status().await)
}

/// Listen until bluez has heard `address` again: a device found by an earlier
/// search that bluez dropped 30 s later cannot be paired until it has.
async fn refind(address: &str) -> Result<(), String> {
    start_search().await?;
    let started = std::time::Instant::now();
    let heard = loop {
        if bctl(BT_QUERY, &["info", address]).await.is_ok() {
            break true;
        }
        if started.elapsed() > BT_REFIND {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    stop_search().await;
    if heard {
        Ok(())
    } else {
        Err(bt_error("not available"))
    }
}

/// Connect, and on a time limit take the attempt back: bluez goes on trying a
/// connection after the client that asked has gone -- measured on the Plus,
/// the switched-off keyboard's attempt ended only on a `disconnect`.
async fn connect(address: &str) -> Result<(), String> {
    match bctl(BT_CONNECT, &["connect", address]).await {
        Ok(_) => held(address).await,
        Err(error) => {
            if error == BT_TIMED_OUT {
                let _ = bctl(BT_LINK, &["disconnect", address]).await;
            }
            Err(bt_error(&error))
        }
    }
}

/// Whether a link that came up is still up a moment later.
///
/// bluez says "Connected: yes" as soon as the baseband link is there, before
/// any profile has. Measured on the Plus with a headset and no audio
/// endpoint: connected straight after pairing, gone two seconds later with
/// "a2dp-sink profile connect failed". A link that does not hold is not a
/// connection, and is not reported as one.
async fn held(address: &str) -> Result<(), String> {
    tokio::time::sleep(BT_SETTLE).await;
    let info = bctl(BT_QUERY, &["info", address]).await.unwrap_or_default();
    if info_flag(&info, "Connected:") {
        Ok(())
    } else {
        Err("bağlantı kuruldu ama hemen koptu; bu cihazın türü desteklenmiyor olabilir".into())
    }
}

/// Pair, trust, connect -- three steps, reported as three.
///
/// A pairing that worked and a connection that did not is a paired device
/// that is not connected, and the answer says exactly that under `outcome`
/// rather than claiming either all of it or none of it.
pub async fn bluetooth_pair(address: &str) -> Result<Value, String> {
    let claim = claim(address)?;
    let address = claim.0.as_str();
    let _controller = BT_CONTROLLER.read().await;
    let _window = BT_PAIRING.lock().await;
    // CachedBluetoothDevice.startPairing cancels discovery before it bonds;
    // the radio then pages the one device instead of inquiring for all.
    stop_search().await;
    let before = match bctl(BT_QUERY, &["info", address]).await {
        Ok(info) => info,
        Err(_) => {
            refind(address).await.map_err(|error| format!("Eşleştirilemedi: {error}"))?;
            bctl(BT_QUERY, &["info", address]).await.unwrap_or_default()
        }
    };
    let was_paired = info_flag(&before, "Paired:");

    // Pairable for exactly as long as this takes.
    //
    // The agent answers "yes" without asking -- a television has no keypad to
    // type a passkey on -- so the protection is the window, not the answer:
    // the appliance accepts a pairing only while somebody standing in front
    // of it has just pressed a device they found, and it stops accepting them
    // again on the way out of here, whether the pairing worked or not.
    let _ = bctl(BT_QUERY, &["pairable", "on"]).await;
    let paired = bctl(BT_PAIR, &["pair", address]).await;
    if paired.is_err() {
        let _ = bctl(BT_QUERY, &["cancel-pairing", address]).await;
    }
    let _ = bctl(BT_QUERY, &["pairable", "off"]).await;
    match paired {
        Ok(_) => {}
        Err(error) if error.contains("AlreadyExists") => {}
        Err(error) => {
            // A pairing that failed half way can leave a bond bluez thinks it
            // has and the device does not, which then refuses every later
            // attempt. A device that was not paired before goes back to not
            // being paired; one that was keeps what it had.
            let after = bctl(BT_QUERY, &["info", address]).await.unwrap_or_default();
            if !was_paired && (info_flag(&after, "Paired:") || info_flag(&after, "Bonded:")) {
                let _ = bctl(BT_LINK, &["remove", address]).await;
                eprintln!("mediaboxd.bluetooth pair failed half way; the partial bond was removed");
            }
            return Err(format!("Eşleştirilemedi: {}", bt_error(&error)));
        }
    }
    // Trusted, or the device has to be authorised by hand every time it comes
    // back -- on a television, a remote that stops working after a power cut
    // and nothing to say yes to it with.
    let trusted = bctl(BT_LINK, &["trust", address]).await.is_ok();
    // A keyboard or a remote connects by itself the moment it is paired; ask
    // only for what did not.
    let now = bctl(BT_QUERY, &["info", address]).await.unwrap_or_default();
    let link = if info_flag(&now, "Connected:") {
        held(address).await
    } else {
        connect(address).await
    };
    let mut status = bluetooth_status().await;
    status["outcome"] = json!({
        "paired": true,
        "trusted": trusted,
        "connected": link.is_ok(),
        "error": link.err(),
    });
    Ok(status)
}

pub async fn bluetooth_connect(address: &str) -> Result<Value, String> {
    let claim = claim(address)?;
    let _controller = BT_CONTROLLER.read().await;
    // A radio that is inquiring shares its time with the page this needs.
    stop_search().await;
    connect(&claim.0)
        .await
        .map_err(|error| format!("Bağlanamadı: {error}"))?;
    Ok(bluetooth_status().await)
}

pub async fn bluetooth_disconnect(address: &str) -> Result<Value, String> {
    let claim = claim(address)?;
    let _controller = BT_CONTROLLER.read().await;
    bctl(BT_LINK, &["disconnect", &claim.0])
        .await
        .map_err(|error| format!("Bağlantı kesilemedi: {}", bt_error(&error)))?;
    Ok(bluetooth_status().await)
}

/// Remove a pairing. bluez drops the link first if there is one; any other
/// device's connection is its own and is not touched.
pub async fn bluetooth_remove(address: &str) -> Result<Value, String> {
    let claim = claim(address)?;
    let _controller = BT_CONTROLLER.read().await;
    bctl(BT_LINK, &["remove", &claim.0])
        .await
        .map_err(|error| format!("Eşleşme kaldırılamadı: {}", bt_error(&error)))?;
    BT_ICONS.lock().expect("bluetooth icons").remove(&claim.0);
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

    // ------------------------------------------------ Bluetooth, from real text

    #[test]
    fn devices_are_read_with_their_names_and_without_made_up_ones() {
        // `bluetoothctl devices` on the Plus, one nameless device added.
        let text = "Device C5:A3:30:E0:02:07 ROG AZOTH\nDevice A4:C1:38:7C:25:32 UR02\nDevice 28:56:5A:A0:A1:8A 28-56-5A-A0-A1-8A\nnot a device line\nDevice nonsense name\n";
        assert_eq!(
            parse_devices(text),
            vec![
                ("C5:A3:30:E0:02:07".to_string(), Some("ROG AZOTH".to_string())),
                ("A4:C1:38:7C:25:32".to_string(), Some("UR02".to_string())),
                ("28:56:5A:A0:A1:8A".to_string(), None),
            ]
        );
    }

    #[test]
    fn info_gives_flags_icon_and_battery() {
        let info = "Device DE:00:45:62:12:B7 (random)\n\tName: MX Master 3S\n\tIcon: input-mouse\n\tPaired: yes\n\tBonded: yes\n\tConnected: no\n\tBattery Percentage: 0x50 (80)\n";
        assert!(info_flag(info, "Paired:"));
        assert!(!info_flag(info, "Connected:"));
        assert!(!info_flag(info, "Trusted:"));
        assert_eq!(info_icon(info).as_deref(), Some("input-mouse"));
        assert_eq!(info_battery(info), Some(80));
        assert_eq!(info_battery("Device x\n"), None);
    }

    #[test]
    fn only_an_address_reaches_bluetoothctl() {
        assert!(is_address("A4:C1:38:7C:25:32"));
        assert!(!is_address("--help"));
        assert!(!is_address("A4:C1:38:7C:25"));
        assert!(!is_address("A4:C1:38:7C:25:3G"));
    }

    #[test]
    fn a_verb_already_running_on_a_device_is_refused_and_released_after() {
        let first = claim("aa:bb:cc:dd:ee:01").unwrap();
        // The same device, however it is spelt: refused, not sent twice.
        assert!(claim("AA:BB:CC:DD:EE:01").is_err());
        // Another device is not held up by it.
        let other = claim("AA:BB:CC:DD:EE:02").unwrap();
        drop(first);
        assert!(claim("AA:BB:CC:DD:EE:01").is_ok());
        drop(other);
    }

    #[test]
    fn bluez_errors_are_said_in_words() {
        // As bluetoothctl printed them on the Plus.
        assert_eq!(bt_error(BT_TIMED_OUT), "cihaz yanıt vermedi");
        assert!(bt_error("Device 11:22:33:44:55:66 not available").contains("görünmüyor"));
        assert!(bt_error("Failed to connect: org.bluez.Error.Failed br-connection-profile-unavailable").contains("desteklenmiyor"));
        assert!(bt_error("Failed to pair: org.bluez.Error.AuthenticationFailed").contains("reddetti"));
        assert!(bt_error("SetDiscoveryFilter failed: org.bluez.Error.NotReady\nFailed to start discovery: org.bluez.Error.NotReady").contains("hazır değil"));
        assert_eq!(bt_error("Failed to x: org.bluez.Error.Odd\n"), "Failed to x: org.bluez.Error.Odd");
    }

    #[test]
    fn colours_and_prompts_are_taken_off() {
        assert_eq!(plain("[\u{1b}[0;93mCHG\u{1b}[0m] Device"), "[CHG] Device");
        assert_eq!(plain("\u{1}\u{1b}[0;94m\u{2}[bluetoothctl]> x"), "[bluetoothctl]> x");
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

    // ------------------------------------------------- Wi-Fi, from real text

    /// Armbian's first-login file on the Plus, the password replaced.
    const ARMBIAN_WIFI: &str = "# Created by Armbian firstlogin script\nnetwork:\n  wifis:\n    wlP2p33s0:\n      dhcp4: yes\n      dhcp6: yes\n      access-points:\n        \"DENKER-WiLAN-SWDD\":\n         password: \"not-the-real-one\"\n";

    #[test]
    fn a_first_login_wifi_file_is_one_to_take_over() {
        assert!(wifi_only(ARMBIAN_WIFI, "wlP2p33s0"));
        // Another interface's file is not this one's.
        assert!(!wifi_only(ARMBIAN_WIFI, "wlan0"));
    }

    #[test]
    fn a_file_with_anything_more_in_it_stays_its_owners() {
        let ethernet = "network:\n  version: 2\n  renderer: networkd\n  ethernets:\n    all-eth-interfaces:\n      match:\n        name: \"e*\"\n      dhcp4: yes\n";
        assert!(!wifi_only(ethernet, "wlP2p33s0"));
        let fixed = ARMBIAN_WIFI.replace("      dhcp4: yes\n", "      addresses: [10.0.0.5/24]\n");
        assert!(!wifi_only(&fixed, "wlP2p33s0"));
        let manager = ARMBIAN_WIFI.replace("network:\n", "network:\n  renderer: NetworkManager\n");
        assert!(!wifi_only(&manager, "wlP2p33s0"));
        // Nothing to take: an interface with no networks.
        assert!(!wifi_only("network:\n  wifis:\n    wlP2p33s0:\n      dhcp4: yes\n", "wlP2p33s0"));
    }

    #[test]
    fn the_supplicant_configuration_is_read_with_its_keys() {
        // The shape netplan renders, keys replaced.
        let text = "ctrl_interface=/run/wpa_supplicant\n\nnetwork={\n  ssid=P\"YuHome5\"\n  key_mgmt=NONE\n}\nnetwork={\n  ssid=P\"Kahve \\xc4\\xb1\\\"\"\n  key_mgmt=WPA-PSK WPA-PSK-SHA256 SAE\n  psk=\"a passphrase\"\n}\nnetwork={\n  ssid=486f6d65\n  psk=0123\n}\n";
        let found = parse_wpa_conf(text);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0], Configured { ssid: "YuHome5".into(), open: true, psk: None });
        assert_eq!(found[1].ssid, "Kahve ı\"");
        assert!(!found[1].open);
        assert_eq!(found[1].psk.as_deref(), Some("\"a passphrase\""));
        assert_eq!(found[2].ssid, "Home");
    }

    #[test]
    fn the_supplicants_numbers_match_a_turkish_name() {
        let text = "network id / ssid / bssid / flags\n0\tYuHome5\tany\t\n1\tYuHome\tany\t[CURRENT]\n2\tKulakl\\xc4\\xb1k\tany\t[DISABLED]\n";
        assert_eq!(
            parse_network_ids(text),
            vec![
                ("0".to_string(), "YuHome5".to_string()),
                ("1".to_string(), "YuHome".to_string()),
                ("2".to_string(), "Kulaklık".to_string()),
            ]
        );
    }

    #[test]
    fn a_scan_keeps_each_network_once_at_its_strongest() {
        let text = "BSS aa:aa(on wl)\n\tfreq: 2422.0\n\tcapability: ESS Privacy (0x0411)\n\tsignal: -61.00 dBm\n\tSSID: YuHome\n\tRSN:\t * Version: 1\n\t\t * Authentication suites: PSK\nBSS bb:bb(on wl)\n\tfreq: 5580.0\n\tsignal: -48.00 dBm\n\tSSID: YuHome\n\tRSN:\t * Version: 1\n\t\t * Authentication suites: PSK\nBSS cc:cc(on wl)\n\tfreq: 2412.0\n\tsignal: -70.00 dBm\n\tSSID: Kafe\nBSS dd:dd(on wl)\n\tfreq: 2412.0\n\tsignal: -30.00 dBm\n\tSSID: \n";
        assert_eq!(
            parse_scan(text),
            vec![
                Seen {
                    ssid: "YuHome".into(),
                    signal: -48,
                    secured: true,
                    band: "5 GHz".into(),
                    security: "WPA2-Personal".into(),
                    bands: ["2,4 GHz".to_string(), "5 GHz".to_string()].into(),
                },
                Seen {
                    ssid: "Kafe".into(),
                    signal: -70,
                    secured: false,
                    band: "2,4 GHz".into(),
                    security: "Yok".into(),
                    bands: ["2,4 GHz".to_string()].into(),
                },
            ]
        );
    }

    #[test]
    fn protection_is_named_in_settingslibs_words() {
        let one = |extra: &str| {
            let text = format!("BSS aa:aa(on wl)\n\tfreq: 5180.0\n\tcapability: ESS Privacy (0x0411)\n\tsignal: -50.00 dBm\n\tSSID: N\n{extra}");
            let seen = parse_scan(&text).remove(0);
            (seen.security, seen.secured)
        };
        assert_eq!(one("\tRSN:\t * Version: 1\n\t\t * Authentication suites: PSK SAE\n"), ("WPA2/WPA3-Personal".into(), true));
        assert_eq!(one("\tRSN:\t * Version: 1\n\t\t * Authentication suites: SAE\n"), ("WPA3-Personal".into(), true));
        assert_eq!(one("\tWPA:\t * Version: 1\n\t\t * Authentication suites: PSK\n\tRSN:\t * Version: 1\n\t\t * Authentication suites: PSK\n"), ("WPA/WPA2-Personal".into(), true));
        assert_eq!(one("\tRSN:\t * Version: 1\n\t\t * Authentication suites: OWE\n"), ("Enhanced Open".into(), false));
        assert_eq!(one(""), ("WEP".into(), true));
        assert_eq!(one("\tRSN:\t * Version: 1\n\t\t * Authentication suites: IEEE 802.1X\n"), ("WPA/WPA2/WPA3-Enterprise".into(), true));
    }

    #[test]
    fn a_prefix_is_listed_as_a_dotted_mask() {
        assert_eq!(netmask(24), "255.255.255.0");
        assert_eq!(netmask(20), "255.255.240.0");
        assert_eq!(netmask(0), "0.0.0.0");
    }
}
