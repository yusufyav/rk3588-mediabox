//! The two radio screens: Wi-Fi and Bluetooth.
//!
//! Plain state and arithmetic, like every other screen here — where the remote
//! is, what moving means, what a press chooses. The talking to the daemon
//! happens in `main.rs`; nothing in this file opens a socket.
//!
//! The Wi-Fi screen has two faces and one focus, because a remote has one
//! focus. It is a list of networks until a secured one is chosen, and then it
//! is that network's password: the field, a letter grid and a button. Back
//! from the password returns to the list rather than leaving the screen, which
//! is what a viewer who picked the wrong network expects.

use crate::keyboard::{Edit, Grid};

/// One network, as the scan handed it over.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Network {
    pub ssid: String,
    pub signal: i32,
    pub secured: bool,
    pub band: String,
    pub remembered: bool,
}

impl Network {
    /// Four steps, because a television is read from a sofa and a number in
    /// dBm is not something anyone should have to interpret.
    pub fn bars(&self) -> u8 {
        match self.signal {
            s if s >= -55 => 4,
            s if s >= -67 => 3,
            s if s >= -78 => 2,
            _ => 1,
        }
    }
}

/// Which face the Wi-Fi screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    List,
    Password,
}

/// Where the remote is while the password face is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordFocus {
    Field,
    /// The switch that turns the bullets back into letters. Beside the field
    /// rather than under it: a password you cannot read is a password you
    /// cannot check, and on a television the only way to check it is to look.
    Show,
    Keys,
    Join,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Nothing that the caller has to act on.
    None,
    /// Run a scan.
    Scan,
    /// Join this network; the password, if there is one, is `password()`.
    Join,
    /// Forget the highlighted network.
    Forget,
    /// Turn the radio on or off.
    Power(bool),
    /// Leave the screen.
    Close,
}

pub struct Wifi {
    /// Whether the daemon has answered yet. Until it has, this screen knows
    /// nothing and says nothing: its first frame used to claim "Bu kartta
    /// kablosuz arayüz yok" on a board whose radio was up and connected,
    /// because `present` defaults to false and the answer takes a moment.
    pub known: bool,
    pub present: bool,
    pub powered: bool,
    pub connected_to: Option<String>,
    pub address: Option<String>,
    pub networks: Vec<Network>,
    /// 0 is the radio switch, 1 is "search again", and the networks follow.
    pub index: usize,
    pub face: Face,
    pub focus: PasswordFocus,
    pub busy: bool,
    /// Busy with a scan in particular, so the row can say "Aranıyor…" only
    /// when that is what is happening.
    pub scanning: bool,
    pub notice: String,
    /// The network the password face is for.
    pub target: Option<Network>,
    secret: String,
    /// Whether the secret is drawn as itself.
    pub show: bool,
    pub keys: Grid,
}

/// The two rows that sit above the list.
pub const WIFI_HEAD: usize = 2;

impl Default for Wifi {
    fn default() -> Self {
        Self::new()
    }
}

impl Wifi {
    pub fn new() -> Self {
        Self {
            known: false,
            present: false,
            powered: false,
            connected_to: None,
            address: None,
            networks: Vec::new(),
            index: 1,
            face: Face::List,
            focus: PasswordFocus::Field,
            busy: false,
            scanning: false,
            notice: String::new(),
            target: None,
            secret: String::new(),
            show: false,
            keys: Grid::text(),
        }
    }

    /// Opened fresh from the settings screen.
    pub fn open(&mut self) {
        self.face = Face::List;
        self.index = 1;
        self.notice.clear();
        self.secret.clear();
        self.show = false;
        self.target = None;
    }

    pub fn password(&self) -> &str {
        &self.secret
    }

    /// Bullets rather than letters, unless the viewer asked to see it. The
    /// count is the real one either way — somebody checking they typed twelve
    /// characters can count twelve.
    pub fn password_mask(&self) -> String {
        if self.show {
            self.secret.clone()
        } else {
            "•".repeat(self.secret.chars().count())
        }
    }

    /// A letter from a real keyboard. Typing never moves the focus, so
    /// somebody who starts on the keyboard and reaches for the remote finds it
    /// where they left it.
    pub fn typed(&mut self, c: char) -> bool {
        if self.face != Face::Password {
            return false;
        }
        if c == '\u{8}' {
            return self.secret.pop().is_some();
        }
        if c == '\r' || c == '\n' {
            return false;
        }
        if c.is_control() {
            return false;
        }
        self.secret.push(c);
        true
    }

    /// Enter on a real keyboard, which joins if there is enough to join with.
    pub fn typed_enter(&mut self) -> Press {
        if self.face != Face::Password {
            return Press::None;
        }
        self.join_if_ready()
    }

    pub fn rows(&self) -> usize {
        WIFI_HEAD + self.networks.len()
    }

    pub fn highlighted(&self) -> Option<&Network> {
        self.index
            .checked_sub(WIFI_HEAD)
            .and_then(|at| self.networks.get(at))
    }

    /// Fold a status answer from the daemon in.
    pub fn take_status(&mut self, value: &serde_json::Value) {
        self.known = true;
        self.present = value
            .get("present")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        self.powered = value
            .get("powered")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        self.connected_to = value
            .get("ssid")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        self.address = value
            .get("address")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
    }

    /// Fold a scan answer in, keeping the remote on the same network it was on
    /// rather than on the same row number: a second scan reorders the list.
    pub fn take_networks(&mut self, networks: Vec<Network>) {
        let was = self.highlighted().map(|network| network.ssid.clone());
        self.networks = networks;
        if let Some(ssid) = was {
            if let Some(at) = self.networks.iter().position(|n| n.ssid == ssid) {
                self.index = WIFI_HEAD + at;
                return;
            }
        }
        self.index = self.index.min(self.rows().saturating_sub(1));
    }

    pub fn step(&mut self, dy: i32) {
        match self.face {
            Face::List => {
                let last = self.rows().saturating_sub(1);
                let next = self.index as i32 + dy;
                self.index = next.clamp(0, last as i32) as usize;
            }
            Face::Password => self.step_password(dy),
        }
    }

    fn step_password(&mut self, dy: i32) {
        match (self.focus, dy) {
            (PasswordFocus::Field | PasswordFocus::Show, 1) => {
                self.focus = PasswordFocus::Keys;
                self.keys.enter(true);
            }
            (PasswordFocus::Keys, -1) => {
                if !self.keys.step(0, -1) {
                    self.focus = PasswordFocus::Field;
                }
            }
            (PasswordFocus::Keys, 1) => {
                if !self.keys.step(0, 1) {
                    self.focus = PasswordFocus::Join;
                }
            }
            (PasswordFocus::Join, -1) => {
                self.focus = PasswordFocus::Keys;
                self.keys.enter(false);
            }
            _ => {}
        }
    }

    pub fn sideways(&mut self, dx: i32) {
        if self.face != Face::Password {
            return;
        }
        match (self.focus, dx) {
            (PasswordFocus::Keys, _) => {
                self.keys.step(dx, 0);
            }
            (PasswordFocus::Field, 1) => self.focus = PasswordFocus::Show,
            (PasswordFocus::Show, -1) => self.focus = PasswordFocus::Field,
            _ => {}
        }
    }

    /// Ok.
    pub fn press(&mut self) -> Press {
        match self.face {
            Face::List => self.press_list(),
            Face::Password => self.press_password(),
        }
    }

    fn press_list(&mut self) -> Press {
        match self.index {
            0 => Press::Power(!self.powered),
            1 => Press::Scan,
            _ => {
                let Some(network) = self.highlighted().cloned() else {
                    return Press::None;
                };
                // A network this board already knows, or an open one, needs
                // nothing typed.
                if network.remembered || !network.secured {
                    self.target = Some(network);
                    self.secret.clear();
                    return Press::Join;
                }
                self.target = Some(network);
                self.secret.clear();
                self.face = Face::Password;
                self.focus = PasswordFocus::Field;
                self.keys = Grid::text();
                self.notice.clear();
                Press::None
            }
        }
    }

    fn press_password(&mut self) -> Press {
        match self.focus {
            PasswordFocus::Field => {
                self.focus = PasswordFocus::Keys;
                self.keys.enter(true);
                Press::None
            }
            PasswordFocus::Show => {
                self.show = !self.show;
                Press::None
            }
            PasswordFocus::Keys => {
                match self.keys.press() {
                    Some(Edit::Insert(c)) => self.secret.push(c),
                    Some(Edit::Backspace) => {
                        self.secret.pop();
                    }
                    Some(Edit::Clear) => self.secret.clear(),
                    // The grid changed its own case; the text is untouched.
                    Some(Edit::Handled) | None => {}
                }
                Press::None
            }
            PasswordFocus::Join => self.join_if_ready(),
        }
    }

    fn join_if_ready(&mut self) -> Press {
        // The same rule WPA itself has. Refused here rather than after a
        // round trip, so the viewer is told before the radio tries.
        if self.secret.chars().count() < 8 {
            self.notice = "Parola en az 8 karakter olmalı".into();
            return Press::None;
        }
        Press::Join
    }

    /// Back. Answers whether the screen handled it; `false` means leave.
    pub fn back(&mut self) -> bool {
        match self.face {
            Face::Password => {
                self.face = Face::List;
                self.secret.clear();
                self.show = false;
                self.target = None;
                self.notice.clear();
                true
            }
            Face::List => false,
        }
    }

    /// The long press that forgets a network, offered only where it means
    /// something.
    pub fn forget(&mut self) -> Press {
        if self.face != Face::List {
            return Press::None;
        }
        match self.highlighted() {
            Some(network) if network.remembered => Press::Forget,
            _ => Press::None,
        }
    }

    /// Wipe the typed secret the moment it is no longer needed.
    pub fn forget_secret(&mut self) {
        self.secret.clear();
    }
}

// ------------------------------------------------------------------ Bluetooth

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Device {
    pub address: String,
    pub name: Option<String>,
    pub paired: bool,
    pub connected: bool,
    pub icon: Option<String>,
}

impl Device {
    /// What to draw. bluez hands back the address as the name when nothing was
    /// resolved, and a row that reads the same twice is no use.
    pub fn title(&self) -> String {
        self.name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.address.clone())
    }

    /// The reference's own words for what this is.
    pub fn kind(&self) -> &'static str {
        match self.icon.as_deref() {
            Some("audio-card") | Some("audio-headset") | Some("audio-headphones") => "Ses aygıtı",
            Some("input-keyboard") => "Klavye",
            Some("input-mouse") => "Fare",
            Some("input-gaming") => "Oyun kolu",
            Some("phone") => "Telefon",
            Some("computer") => "Bilgisayar",
            Some("tv") | Some("video-display") => "Televizyon",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtPress {
    None,
    Scan,
    Power(bool),
    /// Pair and then connect; the address is `highlighted()`.
    Pair,
    Connect,
    Disconnect,
    Forget,
}

pub struct Bluetooth {
    /// As on [`Wifi`]: nothing is claimed before the daemon has answered.
    pub known: bool,
    pub present: bool,
    pub powered: bool,
    pub controller: Option<String>,
    pub devices: Vec<Device>,
    /// 0 is the radio switch, 1 is "search again", and the devices follow.
    pub index: usize,
    pub busy: bool,
    pub notice: String,
}

pub const BT_HEAD: usize = 2;

impl Default for Bluetooth {
    fn default() -> Self {
        Self::new()
    }
}

impl Bluetooth {
    pub fn new() -> Self {
        Self {
            known: false,
            present: false,
            powered: false,
            controller: None,
            devices: Vec::new(),
            index: 1,
            busy: false,
            notice: String::new(),
        }
    }

    pub fn open(&mut self) {
        self.index = 1;
        self.notice.clear();
    }

    pub fn rows(&self) -> usize {
        BT_HEAD + self.devices.len()
    }

    pub fn highlighted(&self) -> Option<&Device> {
        self.index
            .checked_sub(BT_HEAD)
            .and_then(|at| self.devices.get(at))
    }

    pub fn take_status(&mut self, value: &serde_json::Value) {
        self.known = true;
        self.present = value
            .get("present")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let controller = value.get("controller");
        self.controller = controller
            .and_then(|c| c.get("address"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        self.powered = controller
            .and_then(|c| c.get("powered"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let was = self.highlighted().map(|device| device.address.clone());
        self.devices = value
            .get("devices")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                let mut devices: Vec<Device> = items
                    .iter()
                    .map(|item| Device {
                        address: item
                            .get("address")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: item
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        paired: item
                            .get("paired")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        connected: item
                            .get("connected")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        icon: item
                            .get("icon")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                    })
                    .collect();
                // Paired first, then named, then the rest: a scan in a block of
                // flats turns up a dozen anonymous addresses and the viewer's
                // own headphones should not be below them.
                devices.sort_by_key(|device| {
                    (
                        !device.paired,
                        device.name.is_none(),
                        device.title().to_lowercase(),
                    )
                });
                devices
            })
            .unwrap_or_default();

        if let Some(address) = was {
            if let Some(at) = self.devices.iter().position(|d| d.address == address) {
                self.index = BT_HEAD + at;
                return;
            }
        }
        self.index = self.index.min(self.rows().saturating_sub(1));
    }

    pub fn step(&mut self, dy: i32) {
        let last = self.rows().saturating_sub(1);
        let next = self.index as i32 + dy;
        self.index = next.clamp(0, last as i32) as usize;
    }

    pub fn press(&mut self) -> BtPress {
        match self.index {
            0 => BtPress::Power(!self.powered),
            1 => BtPress::Scan,
            _ => match self.highlighted() {
                None => BtPress::None,
                Some(device) if device.connected => BtPress::Disconnect,
                Some(device) if device.paired => BtPress::Connect,
                Some(_) => BtPress::Pair,
            },
        }
    }

    /// What Ok would do, said in the row itself rather than discovered by
    /// pressing it.
    pub fn press_label(&self) -> &'static str {
        match self.index {
            0 => {
                if self.powered {
                    "Kapat"
                } else {
                    "Aç"
                }
            }
            1 => "Ara",
            _ => match self.highlighted() {
                None => "",
                Some(device) if device.connected => "Bağlantıyı kes",
                Some(device) if device.paired => "Bağlan",
                Some(_) => "Eşleştir",
            },
        }
    }

    pub fn forget(&mut self) -> BtPress {
        match self.highlighted() {
            Some(device) if device.paired => BtPress::Forget,
            _ => BtPress::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn net(ssid: &str, signal: i32, secured: bool) -> Network {
        Network {
            ssid: ssid.into(),
            signal,
            secured,
            band: "5 GHz".into(),
            remembered: false,
        }
    }

    #[test]
    fn the_head_rows_come_before_the_networks() {
        let mut wifi = Wifi::new();
        wifi.take_networks(vec![net("a", -40, true), net("b", -60, true)]);
        assert_eq!(wifi.rows(), 4);
        wifi.index = 0;
        assert_eq!(wifi.highlighted(), None);
        wifi.index = 2;
        assert_eq!(wifi.highlighted().unwrap().ssid, "a");
    }

    #[test]
    fn moving_stops_at_both_ends() {
        let mut wifi = Wifi::new();
        wifi.take_networks(vec![net("a", -40, true)]);
        wifi.index = 0;
        wifi.step(-1);
        assert_eq!(wifi.index, 0);
        wifi.step(1);
        wifi.step(1);
        wifi.step(1);
        assert_eq!(wifi.index, 2);
    }

    #[test]
    fn a_secured_network_asks_for_a_password_and_an_open_one_does_not() {
        let mut wifi = Wifi::new();
        wifi.take_networks(vec![net("locked", -40, true), net("open", -50, false)]);
        wifi.index = 2;
        assert_eq!(wifi.press(), Press::None);
        assert_eq!(wifi.face, Face::Password);

        wifi.back();
        wifi.index = 3;
        assert_eq!(wifi.press(), Press::Join);
        assert_eq!(wifi.face, Face::List);
    }

    #[test]
    fn a_remembered_network_is_rejoined_without_typing_anything() {
        let mut wifi = Wifi::new();
        let mut known = net("known", -40, true);
        known.remembered = true;
        wifi.take_networks(vec![known]);
        wifi.index = 2;
        assert_eq!(wifi.press(), Press::Join);
        assert!(wifi.password().is_empty());
    }

    #[test]
    fn a_short_password_is_refused_before_the_radio_is_asked() {
        let mut wifi = Wifi::new();
        wifi.take_networks(vec![net("locked", -40, true)]);
        wifi.index = 2;
        wifi.press();
        wifi.focus = PasswordFocus::Join;
        for c in "short".chars() {
            wifi.secret.push(c);
        }
        assert_eq!(wifi.press(), Press::None);
        assert!(wifi.notice.contains("8 karakter"));
    }

    #[test]
    fn back_from_the_password_returns_to_the_list_and_drops_the_secret() {
        let mut wifi = Wifi::new();
        wifi.take_networks(vec![net("locked", -40, true)]);
        wifi.index = 2;
        wifi.press();
        wifi.secret.push_str("something");
        assert!(wifi.back());
        assert_eq!(wifi.face, Face::List);
        assert!(wifi.password().is_empty());
        // And a second Back leaves the screen.
        assert!(!wifi.back());
    }

    #[test]
    fn a_rescan_keeps_the_remote_on_the_same_network() {
        let mut wifi = Wifi::new();
        wifi.take_networks(vec![net("a", -40, true), net("b", -60, true)]);
        wifi.index = 3; // "b"
        wifi.take_networks(vec![net("b", -55, true), net("a", -70, true)]);
        assert_eq!(wifi.highlighted().unwrap().ssid, "b");
    }

    #[test]
    fn signal_becomes_four_steps() {
        assert_eq!(net("x", -30, true).bars(), 4);
        assert_eq!(net("x", -60, true).bars(), 3);
        assert_eq!(net("x", -70, true).bars(), 2);
        assert_eq!(net("x", -90, true).bars(), 1);
    }

    #[test]
    fn bluetooth_puts_paired_devices_first() {
        let mut bt = Bluetooth::new();
        bt.take_status(&json!({
            "present": true,
            "controller": {"address": "AA:BB", "powered": true},
            "devices": [
                {"address": "11:11", "name": null, "paired": false, "connected": false},
                {"address": "22:22", "name": "Kulaklık", "paired": true, "connected": false},
                {"address": "33:33", "name": "TV", "paired": false, "connected": false}
            ]
        }));
        assert_eq!(bt.devices[0].address, "22:22");
        assert_eq!(bt.devices[1].address, "33:33");
        assert_eq!(bt.devices[2].address, "11:11");
    }

    #[test]
    fn what_ok_does_to_a_device_depends_on_where_it_already_is() {
        let mut bt = Bluetooth::new();
        bt.take_status(&json!({
            "present": true,
            "controller": {"address": "AA:BB", "powered": true},
            "devices": [
                {"address": "11:11", "name": "Bağlı", "paired": true, "connected": true},
                {"address": "22:22", "name": "Eşli", "paired": true, "connected": false},
                {"address": "33:33", "name": "Yeni", "paired": false, "connected": false}
            ]
        }));
        bt.index = BT_HEAD;
        assert_eq!(bt.press(), BtPress::Disconnect);
        bt.index = BT_HEAD + 1;
        assert_eq!(bt.press(), BtPress::Connect);
        bt.index = BT_HEAD + 2;
        assert_eq!(bt.press(), BtPress::Pair);
    }

    /// A screen that has not heard from the daemon says nothing about the
    /// radio. Measured on the Ultra: the first frame of the Wi-Fi screen read
    /// "Kapalı · Bu kartta kablosuz arayüz yok" while wlan0 was up, connected
    /// and at -39 dBm, because absence is what the fields default to.
    #[test]
    fn nothing_is_claimed_before_the_daemon_has_answered() {
        let wifi = Wifi::new();
        assert!(!wifi.known);
        let mut wifi = Wifi::new();
        wifi.take_status(&json!({"present": true, "powered": true, "ssid": "Ev"}));
        assert!(wifi.known);
        assert!(wifi.present);

        let bt = Bluetooth::new();
        assert!(!bt.known);
        let mut bt = Bluetooth::new();
        bt.take_status(&json!({"present": false}));
        assert!(bt.known);
        assert!(!bt.present);
    }

    #[test]
    fn an_unpaired_device_cannot_be_forgotten() {
        let mut bt = Bluetooth::new();
        bt.take_status(&json!({
            "present": true,
            "controller": {"address": "AA:BB", "powered": true},
            "devices": [{"address": "33:33", "name": "Yeni", "paired": false, "connected": false}]
        }));
        bt.index = BT_HEAD;
        assert_eq!(bt.forget(), BtPress::None);
    }
}
