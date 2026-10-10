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
use std::collections::BTreeMap;
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

/// Whether bluez can see a controller at all.
async fn controller_present() -> bool {
    bctl(&["show"])
        .await
        .map(|text| text.contains("Controller "))
        .unwrap_or(false)
}

/// Re-run whichever unit attaches a UART Bluetooth part on this board.
///
/// The name is the board vendor's, not ours, and differs between them, so the
/// known ones are tried in turn and a board that has none is unharmed: its
/// Bluetooth is on USB or SDIO and never lost its firmware in the first place.
async fn reattach_bluetooth() {
    for unit in [
        "ap6611s-bluetooth.service",
        "brcm-bluetooth.service",
        "hciuart.service",
    ] {
        if run("/usr/bin/systemctl", &["restart", unit]).await.is_ok() {
            // Firmware down a 1.5 Mbaud line takes a few seconds.
            tokio::time::sleep(Duration::from_millis(5000)).await;
            // And bluez has to be restarted on top of it. It was holding a
            // management socket on the controller that went away with the
            // firmware, and until it lets go the new one cannot be raised:
            // measured on the Ultra, where `bluetoothctl power on` answered
            // "Unable to open mgmt_socket" and then
            // "org.bluez.Error.Failed" with hci0 sitting DOWN, and came up
            // only once bluetoothd had been restarted after the attach.
            // The pairing agent follows it by BindsTo.
            let _ = run("/usr/bin/systemctl", &["restart", "bluetooth.service"]).await;
            for _ in 0..30 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if controller_present().await {
                    return;
                }
            }
        }
    }
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
        // a controller.
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    // Try, repair, try again.
    //
    // On a board whose Bluetooth is attached over a UART, dropping the GPIO
    // takes the firmware with it and the part comes back mute: the controller
    // is still listed — `bluetoothctl show` prints it even while hci0 sits
    // DOWN, which is why asking "is there a controller" is not the test —
    // but powering it answers "Unable to open mgmt_socket", and after the
    // firmware is pushed again, "org.bluez.Error.Failed" until bluez itself
    // is restarted. Measured on the Ultra; without this, switching Bluetooth
    // off from the settings screen and on again loses it until a reboot.
    let verb = if on { "on" } else { "off" };
    if bctl(&["power", verb]).await.is_err() {
        if !on {
            // Nothing to rescue on the way down.
            return Ok(bluetooth_status().await);
        }
        reattach_bluetooth().await;
        bctl(&["power", verb])
            .await
            .map_err(|_| "Bluetooth denetleyicisi yanıt vermiyor".to_string())?;
    }
    if on {
        // On, but not open: an appliance that sits there pairable is one
        // anybody in the building can pair with. Both are turned on only for
        // the length of a pairing the viewer asked for.
        let _ = bctl(&["pairable", "off"]).await;
        let _ = bctl(&["discoverable", "off"]).await;
    } else {
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
    // Pairable for exactly as long as this takes.
    //
    // The agent answers "yes" without asking — a television has no keypad to
    // type a passkey on — so the protection is the window, not the answer:
    // the appliance accepts a pairing only while somebody standing in front
    // of it has just pressed Eşleştir on a device they found, and it stops
    // accepting them again on the way out of this function, whether the
    // pairing worked or not.
    let _ = bctl(&["pairable", "on"]).await;
    let paired = bctl(&["pair", address]).await;
    let _ = bctl(&["pairable", "off"]).await;
    paired?;
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
