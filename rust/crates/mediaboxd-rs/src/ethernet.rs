//! The wired ports: which there are, what each has, and an address written by
//! hand on trial.
//!
//! **Through netplan, like the radio.** Armbian's `10-dhcp-all-interfaces.yaml`
//! gives every `e*` port DHCP and netplan renders that for systemd-networkd;
//! a hand-written address is one more netplan file on top, never an edit to
//! Armbian's. Going back to DHCP is taking a port out of that file.
//!
//! **The stanza is named so it wins.** networkd applies the first `.network`
//! file, in lexical order, that matches a link, and netplan names the file
//! after the stanza: a stanza called `enP3p49s0` renders
//! `10-netplan-enP3p49s0.network`, which sorts after Armbian's
//! `10-netplan-all-eth-interfaces.network` and is ignored. Measured on the
//! Plus with `netplan generate --root-dir`. So each stanza is
//! `0-mediabox-<port>` and matches its port by name.
//!
//! **A trial lives in /run.** netplan reads `/run/netplan` over `/etc/netplan`
//! when both have a file of the same name, and /run is a tmpfs. So the address
//! on trial is written there and nowhere else: not keeping it in
//! [`ETHERNET_TRIAL_SECONDS`] takes it back, a crash of this daemon takes it
//! back at the next start, and a reboot takes it back by itself — the one
//! failure the display's trial never had to consider, since a mode is never
//! written to disk until it is kept.
//!
//! **Only the port that changed is touched.** `netplan apply` would restart
//! the radio too; this renders with `netplan generate` and asks networkd to
//! reload and reconfigure the one link.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mediabox_core::{ETHERNET_TRIAL_SECONDS, EthernetConfig, EthernetPort, EthernetStatus, EthernetTrial};
use tokio::process::Command;

const NETPLAN: &str = "/usr/sbin/netplan";
const NETWORKCTL: &str = "/usr/bin/networkctl";
const IP: &str = "/usr/sbin/ip";

/// How long each of the three commands is given.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// Every file this module reads or writes, so a test can point all of them
/// at a temporary directory.
#[derive(Debug, Clone)]
pub struct EthernetPaths {
    /// `/sys/class/net`.
    pub net: PathBuf,
    /// What was kept, as {port: config}: the source the netplan file is
    /// rendered from.
    pub state: PathBuf,
    /// The kept netplan file. After Armbian's and the radio's.
    pub kept: PathBuf,
    /// The same name under /run: the address on trial.
    pub trial: PathBuf,
}

impl EthernetPaths {
    pub fn system() -> Self {
        Self {
            net: "/sys/class/net".into(),
            state: "/var/lib/mediabox/ethernet.json".into(),
            kept: "/etc/netplan/60-mediabox-ethernet.yaml".into(),
            trial: "/run/netplan/60-mediabox-ethernet.yaml".into(),
        }
    }

    pub fn under(dir: &Path) -> Self {
        Self {
            net: dir.join("net"),
            state: dir.join("ethernet.json"),
            kept: dir.join("etc-netplan/60-mediabox-ethernet.yaml"),
            trial: dir.join("run-netplan/60-mediabox-ethernet.yaml"),
        }
    }
}

struct Trial {
    interface: String,
    config: EthernetConfig,
    previous: EthernetConfig,
    deadline: Instant,
    id: u64,
}

#[derive(Default)]
struct State {
    kept: BTreeMap<String, EthernetConfig>,
    trial: Option<Trial>,
    next_trial: u64,
}

pub struct Ethernet {
    paths: EthernetPaths,
    /// False in the tests: files are written and nothing is run.
    commands: bool,
    state: Mutex<State>,
}

impl Ethernet {
    pub fn system() -> Arc<Self> {
        Self::new(EthernetPaths::system(), true)
    }

    /// Restores what was kept. A file that cannot be read is a board nobody
    /// has told anything, which is DHCP everywhere — never an error that stops
    /// the daemon starting.
    pub fn new(paths: EthernetPaths, commands: bool) -> Arc<Self> {
        let kept = std::fs::read_to_string(&paths.state)
            .ok()
            .and_then(|text| serde_json::from_str(text.trim()).ok())
            .unwrap_or_default();
        Arc::new(Self {
            paths,
            commands,
            state: Mutex::new(State {
                kept,
                ..Default::default()
            }),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A trial left behind by a daemon that died during it is taken back.
    /// Called once at start.
    pub async fn recover(&self) {
        if self.paths.trial.exists() {
            eprintln!("mediaboxd-rs: an ethernet trial outlived the last run; taking it back");
            if let Err(error) = std::fs::remove_file(&self.paths.trial) {
                eprintln!("mediaboxd-rs: {} not removed: {error}", self.paths.trial.display());
                return;
            }
            if let Err(error) = self.apply(None).await {
                eprintln!("mediaboxd-rs: ethernet not re-applied: {error}");
            }
        }
    }

    /// The wired ports, by name, sorted: physical, not a radio, an Ethernet
    /// link. One on the Ultra, two on the Plus; nothing here counts them.
    pub fn ports(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.paths.net) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                let path = entry.path();
                path.join("device").exists()
                    && !path.join("wireless").exists()
                    && !path.join("phy80211").exists()
                    && read_trimmed(path.join("type")).as_deref() == Some("1")
            })
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    pub fn status(&self) -> EthernetStatus {
        let ports = self.ports();
        let state = self.state();
        EthernetStatus {
            ports: ports
                .iter()
                .map(|name| {
                    let dir = self.paths.net.join(name);
                    let (addresses, gateway) = if self.commands {
                        (addresses_of(name), gateway_of(name))
                    } else {
                        (Vec::new(), None)
                    };
                    EthernetPort {
                        name: name.clone(),
                        mac: read_trimmed(dir.join("address")),
                        carrier: read_trimmed(dir.join("carrier")).as_deref() == Some("1"),
                        addresses,
                        gateway,
                        config: state.kept.get(name).cloned().unwrap_or_default(),
                    }
                })
                .collect(),
            trial: state.trial.as_ref().map(|trial| EthernetTrial {
                interface: trial.interface.clone(),
                config: trial.config.clone(),
                previous: trial.previous.clone(),
                seconds_left: trial
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs_f32()
                    .ceil() as u32,
            }),
            error: None,
        }
    }

    /// Put `config` on `interface`, on trial.
    pub async fn try_config(
        self: &Arc<Self>,
        interface: &str,
        config: EthernetConfig,
    ) -> Result<EthernetStatus, String> {
        if !self.ports().iter().any(|port| port == interface) {
            return Err(format!("{interface} bu kartta kablolu bir port değil."));
        }
        config.validate()?;
        let (id, text) = {
            let mut state = self.state();
            if let Some(trial) = &state.trial
                && trial.interface != interface
            {
                return Err(format!(
                    "{} için onay bekleyen bir ayar var; önce onu koruyun ya da geri alın.",
                    trial.interface
                ));
            }
            // A second change during a trial is measured against what was
            // kept, not against what is still on trial.
            let previous = state.kept.get(interface).cloned().unwrap_or_default();
            if previous == config && state.trial.is_none() {
                return Err("Bu port zaten böyle ayarlı.".into());
            }
            let mut with = state.kept.clone();
            set(&mut with, interface, config.clone());
            state.next_trial += 1;
            let id = state.next_trial;
            state.trial = Some(Trial {
                interface: interface.to_string(),
                config,
                previous,
                deadline: Instant::now() + Duration::from_secs(ETHERNET_TRIAL_SECONDS.into()),
                id,
            });
            (id, render(&with))
        };
        // Always a file under /run, even an empty one: it has to shadow the
        // kept file for DHCP on trial to be DHCP.
        let written = write_private(&self.paths.trial, &text.unwrap_or_else(empty));
        let applied = match written {
            Ok(()) => self.apply(Some(interface)).await,
            Err(error) => Err(error),
        };
        if let Err(error) = applied {
            // Nothing half-done is left behind: the trial goes, and whatever
            // did make it to networkd is put back.
            self.state().trial = None;
            let _ = std::fs::remove_file(&self.paths.trial);
            let _ = self.apply(Some(interface)).await;
            return Err(error);
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(ETHERNET_TRIAL_SECONDS.into())).await;
            this.take_back(Some(id), true).await;
        });
        Ok(self.status())
    }

    /// Keep the address on trial: written down under /etc, and the /run file
    /// that carried it removed.
    pub async fn keep(&self) -> Result<EthernetStatus, String> {
        let text = {
            let mut state = self.state();
            let Some(trial) = state.trial.take() else {
                return Err("Onay bekleyen bir ağ ayarı yok.".into());
            };
            let mut kept = state.kept.clone();
            set(&mut kept, &trial.interface, trial.config.clone());
            let json = serde_json::to_string(&kept).map_err(|error| error.to_string())?;
            write_private(&self.paths.state, &format!("{json}\n"))?;
            state.kept = kept;
            render(&state.kept)
        };
        // /etc first, then /run: at no moment is DHCP what networkd would read.
        match text {
            Some(text) => write_private(&self.paths.kept, &text)?,
            None => remove_if_there(&self.paths.kept)?,
        }
        remove_if_there(&self.paths.trial)?;
        self.apply(None).await?;
        Ok(self.status())
    }

    pub async fn revert(&self) -> Result<EthernetStatus, String> {
        if !self.take_back(None, false).await {
            return Err("Onay bekleyen bir ağ ayarı yok.".into());
        }
        Ok(self.status())
    }

    /// End the trial `id` (or whichever is running): the /run file goes and
    /// the port is given back what was kept.
    async fn take_back(&self, id: Option<u64>, timed_out: bool) -> bool {
        let interface = {
            let mut state = self.state();
            match &state.trial {
                Some(trial) if id.is_none_or(|id| trial.id == id) => {}
                _ => return false,
            }
            state.trial.take().expect("checked above").interface
        };
        if let Err(error) = remove_if_there(&self.paths.trial) {
            eprintln!("mediaboxd-rs: {error}");
        }
        if let Err(error) = self.apply(Some(&interface)).await {
            eprintln!("mediaboxd-rs: ethernet not taken back cleanly: {error}");
        }
        if timed_out {
            eprintln!("mediaboxd-rs: ethernet trial on {interface} not kept in time; the earlier address is back");
        }
        true
    }

    /// Render and hand to networkd: generate, reload, and reconfigure the
    /// port that changed.
    async fn apply(&self, interface: Option<&str>) -> Result<(), String> {
        if !self.commands {
            return Ok(());
        }
        run(NETPLAN, &["generate"]).await?;
        run(NETWORKCTL, &["reload"]).await?;
        if let Some(interface) = interface {
            run(NETWORKCTL, &["reconfigure", interface]).await?;
        }
        Ok(())
    }
}

fn set(configs: &mut BTreeMap<String, EthernetConfig>, interface: &str, config: EthernetConfig) {
    match config {
        EthernetConfig::Dhcp => {
            configs.remove(interface);
        }
        fixed => {
            configs.insert(interface.to_string(), fixed);
        }
    }
}

fn empty() -> String {
    "# Written by mediaboxd-rs: every wired port on DHCP.\nnetwork:\n  version: 2\n".into()
}

/// The netplan file for every port that has an address written by hand;
/// `None` when all of them are on DHCP.
fn render(configs: &BTreeMap<String, EthernetConfig>) -> Option<String> {
    let mut yaml = String::from(
        "# Written by mediaboxd-rs. Edited here only through the television's\n\
         # settings screen; anything added by hand is overwritten.\n\
         network:\n  version: 2\n  renderer: networkd\n  ethernets:\n",
    );
    let mut any = false;
    for (interface, config) in configs {
        let EthernetConfig::Static { address, prefix, gateway, dns } = config else {
            continue;
        };
        // Only a name the kernel could have given: it is written into YAML.
        if interface.is_empty()
            || interface.len() > 15
            || !interface.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            continue;
        }
        any = true;
        yaml.push_str(&format!("    0-mediabox-{interface}:\n"));
        yaml.push_str(&format!("      match:\n        name: \"{interface}\"\n"));
        // IPv6 stays as Armbian had it: a hand-written IPv4 address is not a
        // reason to lose the other one.
        yaml.push_str("      dhcp4: false\n      dhcp6: true\n      ipv6-privacy: true\n");
        yaml.push_str(&format!("      addresses:\n        - {address}/{prefix}\n"));
        if let Some(gateway) = gateway {
            yaml.push_str(&format!("      routes:\n        - to: default\n          via: {gateway}\n"));
        }
        if !dns.is_empty() {
            let list: Vec<String> = dns.iter().map(ToString::to_string).collect();
            yaml.push_str(&format!("      nameservers:\n        addresses: [{}]\n", list.join(", ")));
        }
    }
    any.then_some(yaml)
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|text| text.trim().to_string())
}

/// `ip -4 -o addr show dev X`: every IPv4 address with its prefix.
fn addresses_of(interface: &str) -> Vec<String> {
    let Ok(output) = std::process::Command::new(IP)
        .args(["-4", "-o", "addr", "show", "dev", interface])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            words.find(|word| *word == "inet")?;
            words.next().map(str::to_string)
        })
        .collect()
}

/// `ip -4 route show default dev X`: the gateway, if this port has the route.
fn gateway_of(interface: &str) -> Option<String> {
    let output = std::process::Command::new(IP)
        .args(["-4", "route", "show", "default", "dev", interface])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut words = text.split_whitespace();
    words.find(|word| *word == "via")?;
    words.next().map(str::to_string)
}

async fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let name = Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string());
    let output = tokio::time::timeout(COMMAND_TIMEOUT, Command::new(program).args(args).output())
        .await
        .map_err(|_| format!("{name} zaman aşımına uğradı"))?
        .map_err(|error| format!("{name} çalıştırılamadı: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("{name} {} başarısız oldu: {}", args.join(" "), stderr.trim()))
}

/// Owner-only, written beside and renamed over: netplan warns about files
/// others can read, and a reader never sees half of one.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("{} oluşturulamadı: {error}", parent.display()))?;
    }
    let partial = path.with_extension("partial");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)
        .map_err(|error| format!("{} yazılamadı: {error}", partial.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|error| format!("{} yazılamadı: {error}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|error| format!("{} yazılamadı: {error}", path.display()))
}

fn remove_if_there(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{} silinemedi: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board with two wired ports and a radio, as the Plus is.
    fn board() -> (tempfile::TempDir, Arc<Ethernet>) {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("net");
        for (name, wireless, carrier) in [("end0", false, "1"), ("end1", false, "0"), ("wlan0", true, "1")] {
            let port = net.join(name);
            std::fs::create_dir_all(port.join("device")).unwrap();
            if wireless {
                std::fs::create_dir_all(port.join("wireless")).unwrap();
            }
            std::fs::write(port.join("type"), "1\n").unwrap();
            std::fs::write(port.join("carrier"), format!("{carrier}\n")).unwrap();
            std::fs::write(port.join("address"), "c0:74:2b:00:00:01\n").unwrap();
        }
        // Not a port: no device behind it.
        std::fs::create_dir_all(net.join("lo")).unwrap();
        std::fs::write(net.join("lo/type"), "772\n").unwrap();
        let ethernet = Ethernet::new(EthernetPaths::under(dir.path()), false);
        (dir, ethernet)
    }

    fn fixed() -> EthernetConfig {
        EthernetConfig::Static {
            address: "192.168.2.80".parse().unwrap(),
            prefix: 24,
            gateway: Some("192.168.2.1".parse().unwrap()),
            dns: vec!["1.1.1.1".parse().unwrap()],
        }
    }

    #[test]
    fn the_wired_ports_are_what_the_board_has() {
        let (_dir, ethernet) = board();
        assert_eq!(ethernet.ports(), ["end0", "end1"]);
        let status = ethernet.status();
        assert!(status.ports[0].carrier && !status.ports[1].carrier);
        assert!(status.ports.iter().all(|port| port.config == EthernetConfig::Dhcp));
    }

    /// The stanza is named to sort before Armbian's catch-all, matches its
    /// port by name, and keeps IPv6.
    #[test]
    fn the_rendered_file_wins_over_armbian_s() {
        let mut configs = BTreeMap::new();
        configs.insert("end1".to_string(), fixed());
        let yaml = render(&configs).unwrap();
        assert!(yaml.contains("    0-mediabox-end1:\n      match:\n        name: \"end1\"\n"), "{yaml}");
        assert!(yaml.contains("dhcp4: false"));
        assert!(yaml.contains("dhcp6: true"));
        assert!(yaml.contains("- 192.168.2.80/24"));
        assert!(yaml.contains("via: 192.168.2.1"));
        assert!(yaml.contains("addresses: [1.1.1.1]"));
        // DHCP everywhere renders nothing.
        assert_eq!(render(&BTreeMap::new()), None);
    }

    #[test]
    fn a_name_that_could_break_the_file_is_not_written() {
        let mut configs = BTreeMap::new();
        configs.insert("end0\n  evil: 1".to_string(), fixed());
        assert_eq!(render(&configs), None);
    }

    /// A trial is written under /run only; keeping it moves it to /etc and
    /// the state file, and the /run file goes.
    #[tokio::test]
    async fn a_trial_lives_in_run_until_it_is_kept() {
        let (dir, ethernet) = board();
        let paths = EthernetPaths::under(dir.path());
        let status = ethernet.try_config("end1", fixed()).await.unwrap();
        let trial = status.trial.expect("on trial");
        assert_eq!(trial.interface, "end1");
        assert_eq!(trial.previous, EthernetConfig::Dhcp);
        assert!(paths.trial.exists());
        assert!(!paths.kept.exists(), "nothing under /etc before it is kept");
        assert_eq!(status.ports[1].config, EthernetConfig::Dhcp, "kept is still DHCP");

        let status = ethernet.keep().await.unwrap();
        assert!(status.trial.is_none());
        assert!(!paths.trial.exists());
        assert!(std::fs::read_to_string(&paths.kept).unwrap().contains("0-mediabox-end1"));
        assert_eq!(status.ports[1].config, fixed());
        // And it is still there after a restart.
        let again = Ethernet::new(paths.clone(), false);
        assert_eq!(again.status().ports[1].config, fixed());
    }

    /// Going back — by Back or by the clock — leaves no file behind and
    /// nothing written down.
    #[tokio::test]
    async fn a_trial_taken_back_leaves_nothing() {
        let (dir, ethernet) = board();
        let paths = EthernetPaths::under(dir.path());
        ethernet.try_config("end0", fixed()).await.unwrap();
        let status = ethernet.revert().await.unwrap();
        assert!(status.trial.is_none());
        assert!(!paths.trial.exists());
        assert!(!paths.kept.exists());
        assert!(!paths.state.exists());
        assert!(ethernet.revert().await.is_err(), "nothing left to take back");
    }

    #[tokio::test(start_paused = true)]
    async fn a_trial_nobody_keeps_is_taken_back_in_time() {
        let (dir, ethernet) = board();
        let paths = EthernetPaths::under(dir.path());
        ethernet.try_config("end0", fixed()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(ETHERNET_TRIAL_SECONDS.into()) + Duration::from_millis(50)).await;
        // Let the spawned clock run.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(ethernet.status().trial.is_none());
        assert!(!paths.trial.exists());
    }

    /// DHCP on trial for a port that was kept static still needs a file
    /// under /run, to shadow the one under /etc.
    #[tokio::test]
    async fn dhcp_on_trial_shadows_what_was_kept() {
        let (dir, ethernet) = board();
        let paths = EthernetPaths::under(dir.path());
        ethernet.try_config("end0", fixed()).await.unwrap();
        ethernet.keep().await.unwrap();
        ethernet.try_config("end0", EthernetConfig::Dhcp).await.unwrap();
        let shadow = std::fs::read_to_string(&paths.trial).unwrap();
        assert!(!shadow.contains("0-mediabox-end0"), "{shadow}");
        assert!(shadow.contains("version: 2"));
        ethernet.keep().await.unwrap();
        assert!(!paths.kept.exists(), "all DHCP: no file of ours under /etc");
        assert!(!paths.trial.exists());
    }

    #[tokio::test]
    async fn what_is_not_a_wired_port_or_not_valid_is_refused() {
        let (_dir, ethernet) = board();
        assert!(ethernet.try_config("wlan0", fixed()).await.is_err());
        assert!(ethernet.try_config("lo", fixed()).await.is_err());
        let bad = EthernetConfig::Static {
            address: "192.168.2.0".parse().unwrap(),
            prefix: 24,
            gateway: None,
            dns: vec![],
        };
        assert!(ethernet.try_config("end0", bad).await.is_err());
        assert!(ethernet.status().trial.is_none());
        assert!(ethernet.try_config("end0", EthernetConfig::Dhcp).await.unwrap_err().contains("zaten"));
    }

    /// One port at a time: a second port waits for the first to be answered.
    #[tokio::test]
    async fn one_trial_at_a_time() {
        let (_dir, ethernet) = board();
        ethernet.try_config("end0", fixed()).await.unwrap();
        assert!(ethernet.try_config("end1", fixed()).await.unwrap_err().contains("end0"));
    }

    /// A trial left by a daemon that died is gone after the next start.
    #[tokio::test]
    async fn a_trial_left_behind_is_taken_back_at_start() {
        let (dir, ethernet) = board();
        let paths = EthernetPaths::under(dir.path());
        ethernet.try_config("end0", fixed()).await.unwrap();
        drop(ethernet);
        let restarted = Ethernet::new(paths.clone(), false);
        restarted.recover().await;
        assert!(!paths.trial.exists());
        assert_eq!(restarted.status().ports[0].config, EthernetConfig::Dhcp);
    }
}
