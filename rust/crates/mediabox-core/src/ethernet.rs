//! A wired port's address: taken from the network (DHCP) or written by hand.
//!
//! The rules a hand-written address has to meet live here, beside the types,
//! so the television refuses a bad one while it is being typed and the daemon
//! refuses it again before anything reaches netplan — one set of rules, not
//! two that can drift.

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

/// How long a new address is on trial before the earlier one comes back.
///
/// Longer than the display's: a lease has to be let go of and an address
/// taken, and whoever changed it may want to check from another device that
/// the box is reachable at the new one before keeping it.
pub const ETHERNET_TRIAL_SECONDS: u32 = 30;

/// How many name servers a hand-written address may carry. systemd-resolved
/// takes more, but a television has two fields for them.
pub const ETHERNET_DNS_MAX: usize = 2;

/// How one wired port gets its address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EthernetConfig {
    /// From the network, as the board does out of the box.
    #[default]
    Dhcp,
    /// Written by hand.
    Static {
        address: Ipv4Addr,
        /// The network's size in bits, 24 for 255.255.255.0.
        prefix: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gateway: Option<Ipv4Addr>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dns: Vec<Ipv4Addr>,
    },
}

/// `24` as `255.255.255.0`.
pub fn prefix_mask(prefix: u8) -> Ipv4Addr {
    let bits = if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix.min(32))) };
    Ipv4Addr::from(bits)
}

fn network_of(address: Ipv4Addr, prefix: u8) -> u32 {
    u32::from(address) & u32::from(prefix_mask(prefix))
}

/// Whether an address can be a host's: not nothing, not loopback, not a group,
/// not the whole network's.
fn host_like(address: Ipv4Addr) -> bool {
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_link_local())
}

impl EthernetConfig {
    /// Refuses what would leave the port unusable, in words a person can act
    /// on. DHCP is always acceptable.
    pub fn validate(&self) -> Result<(), String> {
        let EthernetConfig::Static { address, prefix, gateway, dns } = self else {
            return Ok(());
        };
        let (address, prefix) = (*address, *prefix);
        if !(8..=30).contains(&prefix) {
            return Err("Ağ maskesi /8 ile /30 arasında olmalı.".into());
        }
        if !host_like(address) {
            return Err(format!("{address} bir cihaz adresi olamaz."));
        }
        let host_bits = u32::from(address) & !u32::from(prefix_mask(prefix));
        let all_ones = !u32::from(prefix_mask(prefix));
        if host_bits == 0 {
            return Err(format!("{address} bu maskeyle ağın kendi adresi; cihaza verilemez."));
        }
        if host_bits == all_ones {
            return Err(format!("{address} bu maskeyle ağın yayın adresi; cihaza verilemez."));
        }
        if let Some(gateway) = *gateway {
            if gateway == address {
                return Err("Ağ geçidi cihazın kendi adresi olamaz.".into());
            }
            if !host_like(gateway) || network_of(gateway, prefix) != network_of(address, prefix) {
                return Err(format!(
                    "Ağ geçidi {gateway}, {address}/{prefix} ağının içinde değil."
                ));
            }
        }
        if dns.len() > ETHERNET_DNS_MAX {
            return Err(format!("En çok {ETHERNET_DNS_MAX} DNS sunucusu girilebilir."));
        }
        for (index, server) in dns.iter().enumerate() {
            if !host_like(*server) {
                return Err(format!("{server} bir DNS sunucusu adresi olamaz."));
            }
            if dns[..index].contains(server) {
                return Err(format!("{server} iki kez girilmiş."));
            }
        }
        Ok(())
    }

    /// As it reads on a card: `Otomatik (DHCP)`, `Elle · 192.168.2.80/24`.
    pub fn label(&self) -> String {
        match self {
            EthernetConfig::Dhcp => "Otomatik (DHCP)".into(),
            EthernetConfig::Static { address, prefix, .. } => format!("Elle · {address}/{prefix}"),
        }
    }
}

/// One wired port, as the daemon found it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EthernetPort {
    /// The kernel's name for it (`end0`, `enP3p49s0`); it differs by board.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// A cable is in and the other end answers.
    pub carrier: bool,
    /// What it has now, `192.168.2.80/24`, whoever gave it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// The default route through this port, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// What is written down for it: DHCP unless somebody chose otherwise.
    #[serde(default)]
    pub config: EthernetConfig,
}

/// A new address on trial: in effect, not written down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EthernetTrial {
    pub interface: String,
    pub config: EthernetConfig,
    pub previous: EthernetConfig,
    pub seconds_left: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EthernetStatus {
    #[serde(default)]
    pub ports: Vec<EthernetPort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<EthernetTrial>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(address: &str, prefix: u8, gateway: Option<&str>, dns: &[&str]) -> EthernetConfig {
        EthernetConfig::Static {
            address: address.parse().unwrap(),
            prefix,
            gateway: gateway.map(|g| g.parse().unwrap()),
            dns: dns.iter().map(|d| d.parse().unwrap()).collect(),
        }
    }

    #[test]
    fn masks_are_what_people_type() {
        assert_eq!(prefix_mask(24), "255.255.255.0".parse::<Ipv4Addr>().unwrap());
        assert_eq!(prefix_mask(16), "255.255.0.0".parse::<Ipv4Addr>().unwrap());
        assert_eq!(prefix_mask(30), "255.255.255.252".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn an_ordinary_home_address_is_accepted() {
        assert_eq!(fixed("192.168.2.80", 24, Some("192.168.2.1"), &["192.168.2.1", "1.1.1.1"]).validate(), Ok(()));
        // No gateway and no DNS: a port on a network of its own.
        assert_eq!(fixed("10.0.0.5", 8, None, &[]).validate(), Ok(()));
        assert_eq!(EthernetConfig::Dhcp.validate(), Ok(()));
    }

    #[test]
    fn the_network_and_broadcast_addresses_are_refused() {
        assert!(fixed("192.168.2.0", 24, None, &[]).validate().unwrap_err().contains("ağın kendi"));
        assert!(fixed("192.168.2.255", 24, None, &[]).validate().unwrap_err().contains("yayın"));
        // The same address is a host's with a wider mask.
        assert_eq!(fixed("192.168.2.255", 16, None, &[]).validate(), Ok(()));
    }

    #[test]
    fn a_gateway_outside_the_network_is_refused() {
        let error = fixed("192.168.2.80", 24, Some("192.168.3.1"), &[]).validate().unwrap_err();
        assert!(error.contains("ağının içinde değil"), "{error}");
        assert!(fixed("192.168.2.80", 24, Some("192.168.2.80"), &[]).validate().is_err());
    }

    #[test]
    fn nonsense_addresses_are_refused() {
        for address in ["0.0.0.0", "127.0.0.1", "224.0.0.1", "169.254.1.1", "255.255.255.255"] {
            assert!(fixed(address, 24, None, &[]).validate().is_err(), "{address}");
        }
        assert!(fixed("192.168.2.80", 31, None, &[]).validate().is_err());
        assert!(fixed("192.168.2.80", 4, None, &[]).validate().is_err());
    }

    #[test]
    fn name_servers_are_checked_too() {
        assert!(fixed("192.168.2.80", 24, None, &["1.1.1.1", "1.1.1.1"]).validate().is_err());
        assert!(fixed("192.168.2.80", 24, None, &["1.1.1.1", "8.8.8.8", "9.9.9.9"]).validate().is_err());
        assert!(fixed("192.168.2.80", 24, None, &["0.0.0.0"]).validate().is_err());
    }

    #[test]
    fn it_travels_as_the_daemon_and_the_television_expect() {
        let config = fixed("192.168.2.80", 24, Some("192.168.2.1"), &["1.1.1.1"]);
        let text = serde_json::to_string(&config).unwrap();
        assert_eq!(
            text,
            r#"{"mode":"static","address":"192.168.2.80","prefix":24,"gateway":"192.168.2.1","dns":["1.1.1.1"]}"#
        );
        assert_eq!(serde_json::from_str::<EthernetConfig>(&text).unwrap(), config);
        assert_eq!(serde_json::to_string(&EthernetConfig::Dhcp).unwrap(), r#"{"mode":"dhcp"}"#);
    }

    #[test]
    fn a_card_says_it_in_one_line() {
        assert_eq!(EthernetConfig::Dhcp.label(), "Otomatik (DHCP)");
        assert_eq!(fixed("192.168.2.80", 24, None, &[]).label(), "Elle · 192.168.2.80/24");
    }
}
