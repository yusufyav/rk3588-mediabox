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
use std::collections::BTreeMap;

/// One network, as the scan handed it over.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Network {
    pub ssid: String,
    pub signal: i32,
    pub secured: bool,
    pub remembered: bool,
    /// Remembered with no key while the air says it has one: joining it with
    /// what the board holds cannot work, so Ok asks for the password.
    pub key_missing: bool,
    /// In a recent scan, rather than known only because the board is on it.
    pub heard: bool,
    /// Scans in a row that did not hear it. One miss keeps it on the list.
    pub missed: u8,
    /// SettingsLib's words for its protection ("WPA2-Personal") and band.
    pub security: String,
    pub band: String,
    /// Every band one of its radios was heard on.
    pub bands: Vec<String>,
}

impl Network {
    /// The mark beside the name: "5GHz", "2.4/5GHz", nothing for 2,4 GHz
    /// alone -- as the reference screen the user gave draws it.
    pub fn tag(&self) -> &'static str {
        let low = self.bands.iter().any(|band| band.starts_with('2'));
        let high = self.bands.iter().any(|band| !band.starts_with('2'));
        match (low, high) {
            (true, true) => "2.4/5GHz",
            (false, true) => "5GHz",
            _ => "",
        }
    }

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

/// Which face the Wi-Fi screen is showing. The list is Android's "İnternet"
/// page (`NetworkProviderSettings`), Saved its "Kayıtlı ağlar"
/// (`SavedAccessPointsWifiSettings2`), Details a network's page
/// (`WifiNetworkDetailsFragment`), Password the dialog that asks for a key or
/// adds a network (`WifiDialog2` / `AddNetworkFragment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    List,
    Saved,
    Details,
    Password,
}

/// Where the remote is while the password face is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordFocus {
    /// "Ağ adı", only when a network is being added by hand.
    Ssid,
    /// "Güvenlik", the same.
    Security,
    Field,
    /// The switch that turns the bullets back into letters. Beside the field
    /// rather than under it: a password you cannot read is a password you
    /// cannot check, and on a television the only way to check it is to look.
    Show,
    Keys,
    Join,
}

/// A row of the list face, top to bottom, in network_provider_settings.xml's
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// "Kablosuz", the radio's switch.
    Toggle,
    Network(String),
    /// With the radio off, what to do about it. Drawn, never focused.
    Note(&'static str),
    /// "Kayıtlı ağlar" above its networks. Drawn, never focused.
    Header(&'static str),
    /// "Diğer ağlar" above the rest, with "Yenile" at its right -- the
    /// spinner while a scan runs. Ok on "Yenile" scans now.
    Others,
    /// "Ağ ekleyin".
    Add,
    /// "Kayıtlı ağlar": every saved network, in range or not.
    Saved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// Nothing that the caller has to act on.
    None,
    /// Join `target`; the password, if there is one, is `password()`.
    Join,
    /// Turn the radio on or off.
    Power(bool),
    /// Something from a network's page.
    Do(PeerOp),
    /// "Yenile": scan now.
    Rescan,
}

/// What a choice on a network's or a device's menu or page does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerOp {
    WifiJoin(String),
    WifiDisconnect,
    WifiForget(String),
    /// "Değiştir": the password dialog for a remembered network.
    WifiModify(String),
    BtConnect(String),
    BtDisconnect(String),
    BtForget(String),
}

/// One row of a menu: an action, under a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerChoice {
    pub label: &'static str,
    pub value: String,
    pub op: Option<PeerOp>,
    /// Asked before it happens.
    pub ask: Option<String>,
}

impl PeerChoice {
    fn act(label: &'static str, op: PeerOp) -> Self {
        PeerChoice { label, value: String::new(), op: Some(op), ask: None }
    }
}

/// Scans in a row that may miss a network before it leaves the list.
const MISSES_TO_DROP: u8 = 2;

/// The signal as WifiEntry's level, 0 to 4.
fn level(signal: i32) -> u8 {
    match signal {
        s if s >= -55 => 4,
        s if s >= -67 => 3,
        s if s >= -78 => 2,
        s if s >= -88 => 1,
        _ => 0,
    }
}

/// Settings' `wifi_signal` array, indexed by that level, in its Turkish words.
pub fn strength(signal: i32) -> &'static str {
    ["Yetersiz", "Zayıf", "Yeterli", "İyi", "Mükemmel"][level(signal) as usize]
}

/// What the details page of the network the board is on lists under "Ağ
/// ayrıntıları", as the daemon reported it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinkDetails {
    pub mac: Option<String>,
    pub frequency: Option<String>,
    pub netmask: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
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
    /// How strong the link is, for the row of a network the scan missed.
    connected_signal: i32,
    pub address: Option<String>,
    pub link: LinkDetails,
    pub networks: Vec<Network>,
    /// Every network the board knows, from the daemon's last word on it, and
    /// which of them are written down with no key. `None` until it has said.
    known_ssids: Option<Vec<String>>,
    open: Vec<String>,
    /// The focused row among the focusable ones of the face.
    pub index: usize,
    /// On a network's row, the (i) beside it rather than the row: Right
    /// reaches it, Ok on it opens the network's page.
    pub info: bool,
    /// The focused row of "Kayıtlı ağlar", kept while a network's page is up.
    pub saved_index: usize,
    pub face: Face,
    /// The network the details page is about, and the face Back returns to.
    pub details: Option<String>,
    details_from: Face,
    /// The focused button of the details page.
    pub button: usize,
    pub focus: PasswordFocus,
    pub busy: bool,
    /// The network a join is running for: its row says "Bağlanıyor…" until
    /// the answer, as the reference screen's does.
    pub joining: Option<String>,
    /// A scan is running: the spinner stands where "Yenile" is.
    pub scanning: bool,
    /// The number of the scan asked for last. Only its answer ends
    /// `scanning`: leaving the screen and coming straight back used to start
    /// a second scan whose spinner the first one's late answer switched off.
    scan_gen: u64,
    pub notice: String,
    /// The network the password face is for. While a network is being added
    /// by hand it is `None`, and `ssid` and `secure` hold what is typed.
    pub target: Option<Network>,
    pub adding: bool,
    pub ssid: String,
    /// "Güvenlik" of a network added by hand: a key, or none.
    pub secure: bool,
    /// The letter grid types into "Ağ adı" rather than "Şifre".
    typing_ssid: bool,
    secret: String,
    /// Whether the secret is drawn as itself.
    pub show: bool,
    pub keys: Grid,
}

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
            connected_signal: -99,
            address: None,
            link: LinkDetails::default(),
            networks: Vec::new(),
            known_ssids: None,
            open: Vec::new(),
            index: 0,
            info: false,
            saved_index: 0,
            face: Face::List,
            details: None,
            details_from: Face::List,
            button: 0,
            focus: PasswordFocus::Field,
            busy: false,
            joining: None,
            scanning: false,
            scan_gen: 0,
            notice: String::new(),
            target: None,
            adding: false,
            ssid: String::new(),
            secure: true,
            typing_ssid: false,
            secret: String::new(),
            show: false,
            keys: Grid::text(),
        }
    }

    /// Opened fresh from the settings screen.
    pub fn open(&mut self) {
        self.face = Face::List;
        self.index = 0;
        self.notice.clear();
        self.leave_password();
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

    fn typing_into_ssid(&self) -> bool {
        self.adding && (self.focus == PasswordFocus::Ssid || (self.focus == PasswordFocus::Keys && self.typing_ssid))
    }

    /// A letter from a real keyboard. Typing never moves the focus, so
    /// somebody who starts on the keyboard and reaches for the remote finds it
    /// where they left it.
    pub fn typed(&mut self, c: char) -> bool {
        if self.face != Face::Password {
            return false;
        }
        let into_ssid = self.typing_into_ssid();
        let text = if into_ssid { &mut self.ssid } else { &mut self.secret };
        if c == '\u{8}' {
            return text.pop().is_some();
        }
        if c == '\r' || c == '\n' || c.is_control() {
            return false;
        }
        if !into_ssid && self.adding && !self.secure {
            return false;
        }
        text.push(c);
        true
    }

    /// Enter on a real keyboard, which joins if there is enough to join with.
    pub fn typed_enter(&mut self) -> Press {
        if self.face != Face::Password {
            return Press::None;
        }
        self.join_if_ready()
    }

    // ------------------------------------------------------------ the list

    /// The networks on the list: the one the board is on, then what the last
    /// scan heard. A remembered network out of range is on "Kayıtlı ağlar",
    /// as on Android, not here.
    fn listed(&self) -> impl Iterator<Item = &Network> {
        self.networks.iter().filter(|network| self.in_range(network))
    }

    /// Every row the list face draws, as the reference screen the user gave
    /// lays it out: the saved networks in range under "Kayıtlı ağlar", the
    /// one the board is on first; the rest under "Diğer ağlar"; then AOSP's
    /// "Ağ ekleyin" and "Kayıtlı ağlar" rows.
    pub fn list_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Toggle];
        if self.known && !self.powered {
            rows.push(Row::Note("Kullanılabilir ağları görmek için Wi-Fi'yi açın."));
        } else {
            let (saved, other): (Vec<&Network>, Vec<&Network>) = self
                .listed()
                .partition(|network| network.remembered || self.is_connected(&network.ssid));
            if !saved.is_empty() {
                rows.push(Row::Header("Kayıtlı ağlar"));
                rows.extend(saved.iter().map(|network| Row::Network(network.ssid.clone())));
            }
            rows.push(Row::Others);
            rows.extend(other.iter().map(|network| Row::Network(network.ssid.clone())));
            rows.push(Row::Add);
        }
        rows.push(Row::Saved);
        rows
    }

    fn focusable(&self) -> Vec<Row> {
        self.list_rows()
            .into_iter()
            .filter(|row| !matches!(row, Row::Note(_) | Row::Header(_)))
            .collect()
    }

    /// The row the remote is on, on the list face.
    pub fn focused_row(&self) -> Option<Row> {
        self.focusable().get(self.index).cloned()
    }

    /// Every remembered network, by name: "Kayıtlı ağlar".
    pub fn saved(&self) -> Vec<String> {
        // WifiEntry.TITLE_COMPARATOR.
        let mut saved = self.known_ssids.clone().unwrap_or_default();
        saved.sort();
        saved
    }

    /// The network the remote is on, on the list or on "Kayıtlı ağlar".
    pub fn highlighted(&self) -> Option<&Network> {
        match self.face {
            Face::List => match self.focused_row()? {
                Row::Network(ssid) => self.network(&ssid),
                _ => None,
            },
            Face::Saved => self.network(self.saved().get(self.saved_index)?),
            Face::Details => self.network(self.details.as_deref()?),
            Face::Password => None,
        }
    }

    pub fn network(&self, ssid: &str) -> Option<&Network> {
        self.networks.iter().find(|network| network.ssid == ssid)
    }

    pub fn is_connected(&self, ssid: &str) -> bool {
        self.connected_to.as_deref() == Some(ssid)
    }

    /// Fold a status answer from the daemon in.
    pub fn take_status(&mut self, value: &serde_json::Value) {
        let was = self.focused_row();
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
        let details = value.get("details");
        let text = |key: &str| {
            details
                .and_then(|details| details.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        self.link = LinkDetails {
            mac: text("mac"),
            frequency: text("frequency"),
            netmask: text("netmask"),
            gateway: text("gateway"),
            dns: details
                .and_then(|details| details.get("dns"))
                .and_then(serde_json::Value::as_array)
                .map(|items| items.iter().filter_map(serde_json::Value::as_str).map(str::to_string).collect())
                .unwrap_or_default(),
        };
        // What the board knows, from the daemon's answer and not from the
        // last scan: after "Unut" the answer is the one that says the network
        // is gone. Folding in only the link used to leave a forgotten network
        // drawn as remembered until the next scan happened to come back.
        let list = |key: &str| -> Option<Vec<String>> {
            value.get(key).and_then(serde_json::Value::as_array).map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
        };
        if let Some(known) = list("remembered") {
            self.known_ssids = Some(known);
            self.open = list("open").unwrap_or_default();
        }
        self.connected_signal = value
            .get("signal")
            .and_then(serde_json::Value::as_i64)
            .map_or(-99, |signal| signal as i32);
        self.reconcile(was);
    }

    /// One list out of three sources: the last scan, the link and what the
    /// board knows. The network the board is on is shown at once, before any
    /// scan has answered; a remembered one out of range is kept for "Kayıtlı
    /// ağlar" and its page.
    fn reconcile(&mut self, was: Option<Row>) {
        let connected = self.connected_to.clone();
        if let Some(known) = &self.known_ssids {
            for network in &mut self.networks {
                network.remembered = known.contains(&network.ssid);
                network.key_missing =
                    network.secured && network.remembered && self.open.contains(&network.ssid);
            }
        }
        let known = self.known_ssids.clone().unwrap_or_default();
        self.networks.retain(|network| {
            network.heard
                || connected.as_deref() == Some(network.ssid.as_str())
                || known.contains(&network.ssid)
        });
        if let Some(ssid) = connected
            && !self.networks.iter().any(|network| network.ssid == ssid)
        {
            self.networks.push(Network {
                ssid,
                signal: self.connected_signal,
                secured: true,
                remembered: true,
                ..Network::default()
            });
        }
        for ssid in &known {
            if !self.networks.iter().any(|network| &network.ssid == ssid) {
                self.networks.push(Network {
                    ssid: ssid.clone(),
                    signal: -999,
                    secured: !self.open.contains(ssid),
                    remembered: true,
                    ..Network::default()
                });
            }
        }
        // WifiEntry.WIFI_PICKER_COMPARATOR: the one the board is on, then
        // those it can join, then the saved ones, then by signal level and
        // name.
        let connected = self.connected_to.clone();
        self.networks.sort_by(|a, b| {
            let key = |n: &Network| {
                let on = connected.as_deref() == Some(n.ssid.as_str());
                (!on, !(n.heard || on), !n.remembered, std::cmp::Reverse(level(n.signal)))
            };
            key(a).cmp(&key(b)).then_with(|| a.ssid.cmp(&b.ssid))
        });
        // The remote stays on the row it was on, not on the same number.
        let rows = self.focusable();
        if let Some(was) = was
            && let Some(at) = rows.iter().position(|row| *row == was)
        {
            self.index = at;
        }
        self.index = self.index.min(rows.len().saturating_sub(1));
        self.saved_index = self.saved_index.min(self.saved().len().saturating_sub(1));
        if self.face == Face::Details
            && self.details.as_deref().is_some_and(|ssid| self.network(ssid).is_none())
        {
            self.face = self.details_from;
        }
        self.button = self.button.min(self.details_buttons().len().saturating_sub(1));
    }

    /// Heard by the last scan, or the network the board is on.
    pub fn in_range(&self, network: &Network) -> bool {
        network.heard || self.is_connected(&network.ssid)
    }

    /// Fold a scan answer in.
    ///
    /// A network one scan did not hear stays; the second miss in a row takes
    /// it off. That is WifiPickerTrackerHelper's rule -- results kept for
    /// MAX_SCAN_AGE_MILLIS = 15 s, a scan every SCAN_INTERVAL_MILLIS = 10 s --
    /// counted in scans. Replacing the list with each answer emptied it
    /// whenever one came back with nothing, as a scan cut short by
    /// "Bağlantıyı kes" did.
    pub fn take_networks(&mut self, networks: Vec<Network>) {
        let was = self.focused_row();
        let kept: Vec<Network> = self
            .networks
            .iter()
            .filter(|old| !networks.iter().any(|network| network.ssid == old.ssid))
            .filter_map(|old| {
                let missed = old.missed.saturating_add(1);
                if old.heard && missed < MISSES_TO_DROP {
                    Some(Network { missed, ..old.clone() })
                } else if self.is_connected(&old.ssid) {
                    // The network the board is on stays, heard or not.
                    Some(Network { heard: false, missed, ..old.clone() })
                } else {
                    None
                }
            })
            .collect();
        self.networks = networks;
        self.networks.extend(kept);
        self.reconcile(was);
    }

    // -------------------------------------------------------------- moving

    pub fn step(&mut self, dy: i32) {
        let last = match self.face {
            Face::List => self.focusable().len(),
            Face::Saved => self.saved().len(),
            Face::Details => return,
            Face::Password => return self.step_password(dy),
        }
        .saturating_sub(1) as i32;
        let at = if self.face == Face::Saved { &mut self.saved_index } else { &mut self.index };
        *at = (*at as i32 + dy).clamp(0, last.max(0)) as usize;
        self.info = false;
    }

    /// The order the password face's rows come in, top to bottom.
    fn password_rows(&self) -> Vec<PasswordFocus> {
        let mut rows = Vec::new();
        if self.adding {
            rows.push(PasswordFocus::Ssid);
            rows.push(PasswordFocus::Security);
        }
        if !self.adding || self.secure {
            rows.push(PasswordFocus::Field);
        }
        rows.push(PasswordFocus::Keys);
        rows.push(PasswordFocus::Join);
        rows
    }

    fn step_password(&mut self, dy: i32) {
        if self.focus == PasswordFocus::Keys && self.keys.step(0, dy) {
            return;
        }
        let rows = self.password_rows();
        let here = match self.focus {
            PasswordFocus::Show => PasswordFocus::Field,
            other => other,
        };
        let Some(at) = rows.iter().position(|row| *row == here) else {
            return;
        };
        let next = at as i32 + dy;
        if next < 0 || next as usize >= rows.len() {
            return;
        }
        let next = rows[next as usize];
        if next == PasswordFocus::Keys {
            // Down into the grid from a field types into that field.
            if dy > 0 {
                self.typing_ssid = here == PasswordFocus::Ssid
                    || (here == PasswordFocus::Security && self.adding && !self.secure);
            }
            self.keys.enter(dy > 0);
        }
        self.focus = next;
    }

    pub fn sideways(&mut self, dx: i32) {
        match self.face {
            Face::List => {
                self.info = dx > 0 && matches!(self.focused_row(), Some(Row::Network(_)));
            }
            Face::Details => {
                let last = self.details_buttons().len().saturating_sub(1) as i32;
                self.button = (self.button as i32 + dx).clamp(0, last.max(0)) as usize;
            }
            Face::Password => match (self.focus, dx) {
                (PasswordFocus::Keys, _) => {
                    self.keys.step(dx, 0);
                }
                (PasswordFocus::Field, 1) => self.focus = PasswordFocus::Show,
                (PasswordFocus::Show, -1) => self.focus = PasswordFocus::Field,
                (PasswordFocus::Security, _) => self.secure = !self.secure,
                _ => {}
            },
            _ => {}
        }
    }

    // ------------------------------------------------------------- pressing

    /// Ok.
    pub fn press(&mut self) -> Press {
        match self.face {
            Face::List => self.press_list(),
            Face::Saved => {
                if let Some(ssid) = self.saved().get(self.saved_index).cloned() {
                    self.open_details(ssid, Face::Saved);
                }
                Press::None
            }
            Face::Details => self
                .details_buttons()
                .get(self.button)
                .map_or(Press::None, |(_, op)| Press::Do(op.clone())),
            Face::Password => self.press_password(),
        }
    }

    /// NetworkProviderSettings.onSelectedWifiPreferenceClick, and the click
    /// listeners of the rows around the networks.
    fn press_list(&mut self) -> Press {
        match self.focused_row() {
            Some(Row::Toggle) => Press::Power(!self.powered),
            Some(Row::Add) => {
                self.start_adding();
                Press::None
            }
            Some(Row::Saved) => {
                self.face = Face::Saved;
                self.saved_index = 0;
                self.info = false;
                Press::None
            }
            Some(Row::Network(ssid)) => {
                let Some(network) = self.network(&ssid).cloned() else {
                    return Press::None;
                };
                if self.info || self.is_connected(&ssid) {
                    self.open_details(ssid, Face::List);
                    return Press::None;
                }
                if network.key_missing {
                    self.ask_password(network);
                    return Press::None;
                }
                if network.remembered || !network.secured {
                    self.target = Some(network);
                    self.secret.clear();
                    return Press::Join;
                }
                self.ask_password(network);
                Press::None
            }
            Some(Row::Others) => {
                if self.scanning {
                    return Press::None;
                }
                Press::Rescan
            }
            Some(Row::Note(_) | Row::Header(_)) | None => Press::None,
        }
    }

    pub fn open_details(&mut self, ssid: String, from: Face) {
        self.info = false;
        self.details = Some(ssid);
        self.details_from = from;
        self.face = Face::Details;
        self.button = 0;
    }

    /// The details page's buttons, as WifiDetailPreferenceController2 shows
    /// them: "Unut" for a remembered network, then "Bağlan" or "Bağlantıyı
    /// kes".
    pub fn details_buttons(&self) -> Vec<(&'static str, PeerOp)> {
        let Some(ssid) = self.details.clone() else {
            return Vec::new();
        };
        let Some(network) = self.network(&ssid) else {
            return Vec::new();
        };
        let mut buttons = Vec::new();
        if network.remembered {
            buttons.push(("Unut", PeerOp::WifiForget(ssid.clone())));
        }
        if self.is_connected(&ssid) {
            buttons.push(("Bağlantıyı kes", PeerOp::WifiDisconnect));
        } else if self.in_range(network) {
            buttons.push(("Bağlan", PeerOp::WifiJoin(ssid)));
        }
        buttons
    }

    /// The status line under the network's name on its page.
    pub fn details_status(&self) -> &'static str {
        let Some(network) = self.details.as_deref().and_then(|ssid| self.network(ssid)) else {
            return "";
        };
        if self.is_connected(&network.ssid) {
            "Bağlı"
        } else if !self.in_range(network) {
            "Kapsama alanı dışında"
        } else if network.key_missing {
            "Şifreyi kontrol edin ve tekrar deneyin"
        } else if network.remembered {
            "Kaydedildi"
        } else {
            "Bağlı değil"
        }
    }

    /// The password face, for a network that needs one typed: a new network,
    /// or a remembered one whose key is missing or is being changed.
    pub fn ask_password(&mut self, network: Network) {
        self.leave_password();
        self.target = Some(network);
        self.face = Face::Password;
        self.focus = PasswordFocus::Field;
    }

    /// "Ağ ekleyin": the same face, asking for the name too.
    pub fn start_adding(&mut self) {
        self.leave_password();
        self.adding = true;
        self.face = Face::Password;
        self.focus = PasswordFocus::Ssid;
    }

    /// What the row says under a network's name, as the reference screen
    /// shows it: "Bağlı" for the one the board is on, "Bağlanıyor…" while a
    /// join for it runs, nothing for a saved one otherwise. A saved key that
    /// does not fit keeps AOSP's words.
    pub fn summary(&self, network: &Network) -> &'static str {
        if self.joining.as_deref() == Some(network.ssid.as_str()) {
            "Bağlanıyor…"
        } else if self.is_connected(&network.ssid) {
            "Bağlı"
        } else if network.key_missing {
            "Şifreyi kontrol edin ve tekrar deneyin"
        } else {
            ""
        }
    }

    /// A join answered: the dialog has done its job.
    pub fn finish_password(&mut self) {
        if self.face == Face::Password {
            self.leave_password();
            self.face = Face::List;
        }
    }

    fn leave_password(&mut self) {
        self.target = None;
        self.adding = false;
        self.ssid.clear();
        self.secure = true;
        self.typing_ssid = false;
        self.secret.clear();
        self.show = false;
        self.keys = Grid::text();
        self.focus = PasswordFocus::Field;
    }

    fn press_password(&mut self) -> Press {
        match self.focus {
            PasswordFocus::Ssid | PasswordFocus::Field => {
                self.typing_ssid = self.focus == PasswordFocus::Ssid;
                self.focus = PasswordFocus::Keys;
                self.keys.enter(true);
                Press::None
            }
            PasswordFocus::Security => {
                self.secure = !self.secure;
                if !self.secure {
                    self.secret.clear();
                }
                Press::None
            }
            PasswordFocus::Show => {
                self.show = !self.show;
                Press::None
            }
            PasswordFocus::Keys => {
                let into_ssid = self.adding && self.typing_ssid;
                let text = if into_ssid { &mut self.ssid } else { &mut self.secret };
                match self.keys.press() {
                    Some(Edit::Insert(c)) => text.push(c),
                    Some(Edit::Backspace) => {
                        text.pop();
                    }
                    Some(Edit::Clear) => text.clear(),
                    // The grid changed its own case; the text is untouched.
                    Some(Edit::Handled) | None => {}
                }
                Press::None
            }
            PasswordFocus::Join => self.join_if_ready(),
        }
    }

    fn join_if_ready(&mut self) -> Press {
        if self.adding {
            if self.ssid.trim().is_empty() {
                self.notice = "Ağ adı boş".into();
                return Press::None;
            }
            self.target = Some(Network {
                ssid: self.ssid.trim().to_string(),
                secured: self.secure,
                ..Network::default()
            });
            if !self.secure {
                return Press::Join;
            }
        }
        // The same rule WPA itself has. Refused here rather than after a
        // round trip, so the viewer is told before the radio tries.
        if self.secret.chars().count() < 8 {
            self.notice = "Şifre en az 8 karakter olmalı".into();
            return Press::None;
        }
        Press::Join
    }

    /// Back. Answers whether the screen handled it; `false` means leave.
    pub fn back(&mut self) -> bool {
        match self.face {
            Face::Password => {
                self.leave_password();
                self.notice.clear();
                self.face = Face::List;
                true
            }
            Face::Details => {
                self.face = self.details_from;
                true
            }
            Face::Saved => {
                self.face = Face::List;
                true
            }
            Face::List => false,
        }
    }

    /// The menu a long press opens on a network of the list:
    /// NetworkProviderSettings.onCreateContextMenu — "Bağlan", "Bağlantıyı
    /// kes", "Unut", "Değiştir", whichever apply, under the network's name.
    pub fn menu(&self) -> Option<(String, Vec<PeerChoice>)> {
        if self.face != Face::List {
            return None;
        }
        let network = self.highlighted()?;
        let ssid = network.ssid.clone();
        let connected = self.is_connected(&ssid);
        let mut choices = Vec::new();
        if !connected && self.in_range(network) {
            choices.push(PeerChoice::act("Bağlan", PeerOp::WifiJoin(ssid.clone())));
        }
        if connected {
            choices.push(PeerChoice::act("Bağlantıyı kes", PeerOp::WifiDisconnect));
        }
        if network.remembered {
            choices.push(PeerChoice::act("Unut", PeerOp::WifiForget(ssid.clone())));
            choices.push(PeerChoice::act("Değiştir", PeerOp::WifiModify(ssid.clone())));
        }
        Some((ssid, choices))
    }

    /// A scan is asked for: its number, which its answer carries back.
    pub fn begin_scan(&mut self) -> u64 {
        self.scan_gen += 1;
        self.scanning = true;
        self.scan_gen
    }

    /// The answer to scan `scan` is in; the spinner stops only for the last
    /// one asked for.
    pub fn end_scan(&mut self, scan: u64) {
        if scan == self.scan_gen {
            self.scanning = false;
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
    pub battery: Option<u8>,
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

    /// The glyph at the row's left, from what bluez says the device is.
    pub fn glyph(&self) -> &'static str {
        match self.icon.as_deref() {
            Some("audio-headset") | Some("audio-headphones") => "headphones",
            Some("audio-card") => "speaker",
            Some("input-keyboard") => "keyboard",
            Some("input-mouse") | Some("input-tablet") => "mouse",
            Some("input-gaming") => "remote",
            Some("phone") => "phone",
            Some("computer") => "monitor",
            Some("tv") | Some("video-display") => "tv",
            _ => "bluetooth",
        }
    }
}

/// A verb on one device, running until its own answer comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtOp {
    Pair,
    Connect,
    Disconnect,
    Forget,
}

impl BtOp {
    /// What the device's row says while it runs.
    fn running(self) -> &'static str {
        match self {
            BtOp::Pair => "Eşleşiyor…",
            BtOp::Connect => "Bağlanıyor…",
            BtOp::Disconnect => "Bağlantı kesiliyor…",
            BtOp::Forget => "Kaldırılıyor…",
        }
    }
}

/// A row of the Bluetooth list, top to bottom, as the Wi-Fi list is laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtLine {
    /// "Bluetooth", the radio's switch.
    Toggle,
    /// With the radio off, what to do about it. Drawn, never focused.
    Note(&'static str),
    /// "Eşleşmiş cihazlar" above the paired ones. Drawn, never focused.
    Header(&'static str),
    /// "Kullanılabilir cihazlar" with "Yenile" at its right -- the spinner
    /// while a search runs. Ok on "Yenile" searches.
    Available,
    Device(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtPress {
    None,
    Power(bool),
    Scan,
    Do(BtOp, String),
    /// The device's actions: what Ok on a connected device opens.
    Menu,
}

/// Which request an answer from the daemon is the answer to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtReply {
    /// A look, on opening or while the screen is open.
    Status,
    /// A search started (or refused).
    Scan,
    Power,
    Done(String, BtOp),
}

pub struct Bluetooth {
    /// As on [`Wifi`]: nothing is claimed before the daemon has answered.
    pub known: bool,
    pub present: bool,
    pub powered: bool,
    pub controller: Option<String>,
    pub devices: Vec<Device>,
    /// Into the focusable rows.
    pub index: usize,
    /// The radio is being switched: the list waits for that answer.
    pub busy: bool,
    pub scanning: bool,
    /// A search asked for and not answered yet: a look that set out before
    /// it does not stop the spinner it started.
    scan_asked: bool,
    /// The devices a verb is running on. One at a time per device; another
    /// device is not held up by it.
    pub pending: BTreeMap<String, BtOp>,
    pub notice: String,
}

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
            index: 0,
            busy: false,
            scanning: false,
            scan_asked: false,
            pending: BTreeMap::new(),
            notice: String::new(),
        }
    }

    pub fn open(&mut self) {
        self.index = 0;
        self.notice.clear();
    }

    pub fn device(&self, address: &str) -> Option<&Device> {
        self.devices.iter().find(|device| device.address == address)
    }

    /// Every row, as AOSP's Bluetooth settings group them: the switch; the
    /// paired devices, the connected ones first; then the ones a search
    /// found. A found device with no name is left out, as Android leaves it
    /// out unless a developer option says otherwise: a search in a block of
    /// flats turns up a dozen anonymous addresses nobody can tell apart.
    pub fn rows(&self) -> Vec<BtLine> {
        let mut rows = vec![BtLine::Toggle];
        if !self.known || !self.present {
            return rows;
        }
        if !self.powered {
            rows.push(BtLine::Note("Yakındaki cihazları görmek için Bluetooth'u açın."));
            return rows;
        }
        let mut paired: Vec<&Device> = self.devices.iter().filter(|device| device.paired).collect();
        paired.sort_by_key(|device| (!device.connected, device.title().to_lowercase()));
        if !paired.is_empty() {
            rows.push(BtLine::Header("Eşleşmiş cihazlar"));
            rows.extend(paired.iter().map(|device| BtLine::Device(device.address.clone())));
        }
        rows.push(BtLine::Available);
        // By name, not by when it was heard: a list that reorders itself
        // every two seconds is one the remote cannot stay on.
        let mut found: Vec<&Device> = self
            .devices
            .iter()
            .filter(|device| !device.paired && device.name.is_some())
            .collect();
        found.sort_by_key(|device| device.title().to_lowercase());
        rows.extend(found.iter().map(|device| BtLine::Device(device.address.clone())));
        rows
    }

    fn focusable(&self) -> Vec<BtLine> {
        self.rows()
            .into_iter()
            .filter(|row| !matches!(row, BtLine::Note(_) | BtLine::Header(_)))
            .collect()
    }

    pub fn focused_row(&self) -> Option<BtLine> {
        self.focusable().get(self.index).cloned()
    }

    pub fn highlighted(&self) -> Option<&Device> {
        match self.focused_row()? {
            BtLine::Device(address) => self.device(&address),
            _ => None,
        }
    }

    /// Fold a status answer in, keeping the remote on the row it was on.
    pub fn take_status(&mut self, value: &serde_json::Value) {
        let was = self.focused_row();
        self.known = true;
        // Switched off is not absent. On the Plus, rfkill cuts the radio's
        // power and the USB controller leaves with it: bluez then has no
        // controller to show, and reading that as "no Bluetooth on this
        // board" greyed the switch out -- the radio could be turned off from
        // here and never on again.
        let blocked = value.get("blocked").and_then(serde_json::Value::as_bool) == Some(true);
        self.present = blocked
            || value
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
        let scanning = value
            .get("scanning")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        self.scanning = self.powered && (scanning || self.scan_asked);
        self.devices = value
            .get("devices")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
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
                        battery: item
                            .get("battery")
                            .and_then(serde_json::Value::as_u64)
                            .map(|level| level.min(100) as u8),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rows = self.focusable();
        if let Some(at) = was.and_then(|was| rows.iter().position(|row| *row == was)) {
            self.index = at;
        }
        self.index = self.index.min(rows.len().saturating_sub(1));
    }

    /// Fold in the daemon's answer to `reply`.
    ///
    /// Only an action's own answer ends it: the looks taken every two seconds
    /// while the screen is open used to clear "busy" under a pairing that was
    /// still running, and the next Ok started a second one on top of it.
    pub fn take_answer(&mut self, answer: Result<serde_json::Value, String>, reply: &BtReply) {
        match reply {
            BtReply::Power => self.busy = false,
            BtReply::Done(address, _) => {
                self.pending.remove(address);
            }
            BtReply::Scan => self.scan_asked = false,
            BtReply::Status => {}
        }
        match answer {
            Ok(value) => {
                // While the radio is being switched a look sees it half way;
                // the switch's own answer says where it ended up.
                if self.busy && *reply != BtReply::Power {
                    return;
                }
                self.take_status(&value);
                match reply {
                    BtReply::Power => self.notice.clear(),
                    BtReply::Done(_, op) => {
                        let outcome = value.get("outcome");
                        let linked = outcome
                            .and_then(|o| o.get("connected"))
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(true);
                        self.notice = if *op == BtOp::Pair && !linked {
                            // Paired is paired: the row says so, and this
                            // says what did not happen.
                            let why = outcome
                                .and_then(|o| o.get("error"))
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("bilinmeyen hata");
                            format!("Eşleşti, ancak bağlanamadı: {why}")
                        } else {
                            String::new()
                        };
                    }
                    _ => {}
                }
            }
            Err(error) => {
                if *reply == BtReply::Scan {
                    self.scanning = false;
                }
                // A failed look does not cover what an action said.
                if *reply != BtReply::Status || self.notice.is_empty() {
                    self.notice = error;
                }
            }
        }
    }

    /// A search is being asked for.
    pub fn begin_scan(&mut self) {
        self.scanning = true;
        self.scan_asked = true;
        self.notice.clear();
    }

    /// A verb on a device is being asked for.
    pub fn begin(&mut self, op: BtOp, address: &str) {
        self.pending.insert(address.to_string(), op);
        self.notice.clear();
        // Pairing and connecting end the search in the daemon.
        if matches!(op, BtOp::Pair | BtOp::Connect) {
            self.scanning = false;
        }
    }

    pub fn step(&mut self, dy: i32) {
        let last = self.focusable().len().saturating_sub(1);
        let next = self.index as i32 + dy;
        self.index = next.clamp(0, last as i32) as usize;
    }

    /// What Ok does on the focused row: AOSP's
    /// BluetoothDevicePreference.onClicked -- a found device pairs, a paired
    /// one connects, a connected one offers what can be done to it.
    pub fn press(&self) -> BtPress {
        match self.focused_row() {
            Some(BtLine::Toggle) if self.known && self.present && !self.busy => {
                BtPress::Power(!self.powered)
            }
            Some(BtLine::Available) if self.powered && !self.scanning && !self.busy => BtPress::Scan,
            Some(BtLine::Device(address)) if !self.busy && !self.pending.contains_key(&address) => {
                match self.device(&address) {
                    Some(device) if device.connected => BtPress::Menu,
                    Some(device) if device.paired => BtPress::Do(BtOp::Connect, address),
                    Some(_) => BtPress::Do(BtOp::Pair, address),
                    None => BtPress::None,
                }
            }
            _ => BtPress::None,
        }
    }

    /// Under a device's name: what is happening to it, or that it is
    /// connected, with its battery when it reports one.
    pub fn under(&self, device: &Device) -> String {
        if let Some(op) = self.pending.get(&device.address) {
            return op.running().into();
        }
        match (device.connected, device.battery) {
            (true, Some(level)) => format!("Bağlı, pil %{level}"),
            (true, None) => "Bağlı".into(),
            _ => String::new(),
        }
    }

    /// One line at the top: how many are paired, and connected.
    pub fn summary(&self) -> String {
        if !self.known {
            return String::new();
        }
        if !self.present {
            return "Bu kartta Bluetooth denetleyicisi yok".into();
        }
        if !self.powered {
            return "Kapalı".into();
        }
        let paired = self.devices.iter().filter(|device| device.paired).count();
        let connected = self.devices.iter().filter(|device| device.connected).count();
        match (paired, connected) {
            (0, _) => "Eşleşmiş cihaz yok".into(),
            (paired, 0) => format!("{paired} eşleşmiş cihaz"),
            (paired, connected) => format!("{paired} eşleşmiş cihaz · {connected} bağlı"),
        }
    }

    /// What a long Ok -- or Ok on a connected device, or Menu -- offers on a
    /// paired device.
    pub fn menu(&self) -> Option<(String, Vec<PeerChoice>)> {
        let device = self.highlighted().filter(|device| device.paired)?;
        if self.busy || self.pending.contains_key(&device.address) {
            return None;
        }
        let title = device.title();
        let address = device.address.clone();
        let link = if device.connected {
            PeerChoice::act("Bağlantıyı kes", PeerOp::BtDisconnect(address.clone()))
        } else {
            PeerChoice::act("Bağlan", PeerOp::BtConnect(address.clone()))
        };
        let forget = PeerChoice {
            ask: Some(format!("{title} eşleşmesi kaldırılsın mı?")),
            ..PeerChoice::act("Eşleşmeyi kaldır", PeerOp::BtForget(address))
        };
        Some((title, vec![link, forget]))
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
            heard: true,
            security: if secured { "WPA2-Personal".into() } else { "Yok".into() },
            band: "5 GHz".into(),
            bands: vec!["5 GHz".into()],
            ..Network::default()
        }
    }

    fn ops(menu: &[PeerChoice]) -> Vec<Option<PeerOp>> {
        menu.iter().map(|choice| choice.op.clone()).collect()
    }

    /// The board on YuHome, YuHome5 saved and heard, OnePlus saved and out of
    /// range, two strangers.
    fn home() -> Wifi {
        let mut wifi = Wifi::new();
        wifi.take_status(&json!({
            "present": true, "powered": true, "ssid": "YuHome", "signal": -45,
            "address": "10.27.27.34", "remembered": ["YuHome", "YuHome5", "OnePlus"], "open": [],
            "details": {"mac": "b0:ac:82:e0:9c:ab", "frequency": "2,4 GHz", "netmask": "255.255.255.0",
                        "gateway": "10.27.27.1", "dns": ["10.27.27.9"]}
        }));
        wifi.take_networks(vec![
            net("Komşu", -40, true),
            net("YuHome", -45, true),
            net("YuHome5", -60, true),
            net("Kafe", -70, false),
        ]);
        wifi
    }

    fn focus_on(wifi: &mut Wifi, wanted: Row) {
        wifi.index = wifi.focusable().iter().position(|row| *row == wanted).expect("row is listed");
    }

    #[test]
    fn saved_networks_and_other_networks_are_listed_under_their_own_headings() {
        let wifi = home();
        assert_eq!(
            wifi.list_rows(),
            vec![
                Row::Toggle,
                Row::Header("Kayıtlı ağlar"),
                Row::Network("YuHome".into()),
                Row::Network("YuHome5".into()),
                Row::Others,
                Row::Network("Komşu".into()),
                Row::Network("Kafe".into()),
                Row::Add,
                Row::Saved,
            ],
            "the one the board is on first; out-of-range saved ones only on the Kayıtlı ağlar page"
        );
        assert!(wifi.focusable().iter().all(|row| !matches!(row, Row::Header(_))), "a heading is never focused");
        // "Yenile" at the right of "Diğer ağlar" is.
        let mut wifi = wifi;
        focus_on(&mut wifi, Row::Others);
        assert_eq!(wifi.press(), Press::Rescan);
        wifi.scanning = true;
        assert_eq!(wifi.press(), Press::None, "a scan running is the one Yenile would start");
        // Nothing saved in range: no "Kayıtlı ağlar" heading at all.
        let mut alone = Wifi::new();
        alone.take_status(&json!({"present": true, "powered": true, "remembered": []}));
        alone.take_networks(vec![net("Kafe", -70, false)]);
        assert_eq!(
            alone.list_rows(),
            vec![Row::Toggle, Row::Others, Row::Network("Kafe".into()), Row::Add, Row::Saved]
        );
        let mut off = home();
        off.powered = false;
        assert_eq!(
            off.list_rows(),
            vec![Row::Toggle, Row::Note("Kullanılabilir ağları görmek için Wi-Fi'yi açın."), Row::Saved]
        );
    }

    #[test]
    fn a_saved_network_says_nothing_until_it_is_being_joined() {
        let mut wifi = home();
        let five = wifi.network("YuHome5").unwrap().clone();
        assert_eq!(wifi.summary(&five), "", "no Kaydedildi under a saved network");
        wifi.joining = Some("YuHome5".into());
        assert_eq!(wifi.summary(&five), "Bağlanıyor…");
        let home_net = wifi.network("YuHome").unwrap().clone();
        assert_eq!(wifi.summary(&home_net), "Bağlı");
    }

    #[test]
    fn right_reaches_the_info_button_and_ok_there_opens_the_page() {
        let mut wifi = home();
        focus_on(&mut wifi, Row::Network("Komşu".into()));
        wifi.sideways(1);
        assert!(wifi.info);
        assert_eq!(wifi.press(), Press::None);
        assert_eq!(wifi.face, Face::Details);
        assert_eq!(wifi.details.as_deref(), Some("Komşu"));
        wifi.back();
        // Moving off the row, or Left, puts the remote back on the row.
        wifi.sideways(1);
        wifi.sideways(-1);
        assert!(!wifi.info);
        wifi.sideways(1);
        wifi.step(1);
        assert!(!wifi.info);
        // Not on a network: nothing to the right.
        focus_on(&mut wifi, Row::Add);
        wifi.sideways(1);
        assert!(!wifi.info);
    }

    #[test]
    fn the_band_mark_is_drawn_as_the_reference_screen_does() {
        let mut both = net("x", -50, true);
        both.bands = vec!["2,4 GHz".into(), "5 GHz".into()];
        assert_eq!(both.tag(), "2.4/5GHz");
        assert_eq!(net("x", -50, true).tag(), "5GHz");
        let mut low = net("x", -50, true);
        low.bands = vec!["2,4 GHz".into()];
        assert_eq!(low.tag(), "");
    }

    #[test]
    fn ok_on_each_kind_of_network_does_what_android_does() {
        // The one the board is on: its page.
        let mut wifi = home();
        focus_on(&mut wifi, Row::Network("YuHome".into()));
        assert_eq!(wifi.press(), Press::None);
        assert_eq!(wifi.face, Face::Details);
        assert_eq!(wifi.details.as_deref(), Some("YuHome"));
        // A saved one in range: joined with the key it has.
        let mut wifi = home();
        focus_on(&mut wifi, Row::Network("YuHome5".into()));
        assert_eq!(wifi.press(), Press::Join);
        assert!(wifi.password().is_empty());
        // An open stranger: joined.
        focus_on(&mut wifi, Row::Network("Kafe".into()));
        assert_eq!(wifi.press(), Press::Join);
        // A secured stranger: the password dialog.
        focus_on(&mut wifi, Row::Network("Komşu".into()));
        assert_eq!(wifi.press(), Press::None);
        assert_eq!(wifi.face, Face::Password);
        assert_eq!(wifi.target.as_ref().unwrap().ssid, "Komşu");
    }

    #[test]
    fn a_saved_network_whose_key_does_not_fit_asks_for_it() {
        let mut wifi = home();
        wifi.take_status(&json!({
            "present": true, "powered": true, "ssid": "YuHome",
            "remembered": ["YuHome", "YuHome5"], "open": ["YuHome5"]
        }));
        focus_on(&mut wifi, Row::Network("YuHome5".into()));
        assert!(wifi.highlighted().unwrap().key_missing);
        assert_eq!(wifi.press(), Press::None);
        assert_eq!(wifi.face, Face::Password);
    }

    #[test]
    fn a_long_press_offers_androids_context_menu() {
        let mut wifi = home();
        focus_on(&mut wifi, Row::Network("YuHome".into()));
        let (title, menu) = wifi.menu().unwrap();
        assert_eq!(title, "YuHome");
        let labels: Vec<&str> = menu.iter().map(|c| c.label).collect();
        assert_eq!(labels, ["Bağlantıyı kes", "Unut", "Değiştir"]);
        assert!(menu.iter().all(|c| c.ask.is_none() && c.value.is_empty()), "Android asks nothing");
        focus_on(&mut wifi, Row::Network("YuHome5".into()));
        assert_eq!(
            ops(&wifi.menu().unwrap().1),
            vec![
                Some(PeerOp::WifiJoin("YuHome5".into())),
                Some(PeerOp::WifiForget("YuHome5".into())),
                Some(PeerOp::WifiModify("YuHome5".into())),
            ]
        );
        focus_on(&mut wifi, Row::Network("Komşu".into()));
        assert_eq!(ops(&wifi.menu().unwrap().1), vec![Some(PeerOp::WifiJoin("Komşu".into()))]);
        focus_on(&mut wifi, Row::Add);
        assert!(wifi.menu().is_none());
    }

    #[test]
    fn the_details_page_has_androids_buttons() {
        let mut wifi = home();
        wifi.open_details("YuHome".into(), Face::List);
        let labels: Vec<&str> = wifi.details_buttons().iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, ["Unut", "Bağlantıyı kes"]);
        assert_eq!(wifi.details_status(), "Bağlı");
        assert_eq!(wifi.press(), Press::Do(PeerOp::WifiForget("YuHome".into())));
        wifi.sideways(1);
        assert_eq!(wifi.press(), Press::Do(PeerOp::WifiDisconnect));
        assert!(wifi.back());
        assert_eq!(wifi.face, Face::List);
        // Out of range: only "Unut".
        wifi.open_details("OnePlus".into(), Face::Saved);
        assert_eq!(wifi.details_status(), "Kapsama alanı dışında");
        let labels: Vec<&str> = wifi.details_buttons().iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, ["Unut"]);
        assert!(wifi.back());
        assert_eq!(wifi.face, Face::Saved, "Back goes where it came from");
    }

    #[test]
    fn saved_networks_lists_every_saved_one_by_name_and_opens_its_page() {
        let mut wifi = home();
        focus_on(&mut wifi, Row::Saved);
        assert_eq!(wifi.press(), Press::None);
        assert_eq!(wifi.face, Face::Saved);
        assert_eq!(wifi.saved(), ["OnePlus", "YuHome", "YuHome5"]);
        wifi.press();
        assert_eq!(wifi.face, Face::Details);
        assert_eq!(wifi.details.as_deref(), Some("OnePlus"));
        // Forgotten: the answer no longer knows it, and its page closes.
        wifi.take_status(&json!({"present": true, "powered": true, "ssid": "YuHome", "remembered": ["YuHome", "YuHome5"]}));
        assert_eq!(wifi.face, Face::Saved);
        assert_eq!(wifi.saved(), ["YuHome", "YuHome5"]);
    }

    #[test]
    fn a_forgotten_network_stops_being_saved_on_the_answer() {
        let mut wifi = home();
        wifi.take_status(&json!({"present": true, "powered": true, "ssid": "YuHome", "remembered": ["YuHome"]}));
        assert!(!wifi.network("YuHome5").unwrap().remembered);
        focus_on(&mut wifi, Row::Network("YuHome5".into()));
        assert_eq!(ops(&wifi.menu().unwrap().1), vec![Some(PeerOp::WifiJoin("YuHome5".into()))]);
    }

    #[test]
    fn a_network_is_added_by_hand_with_its_name_security_and_key() {
        let mut wifi = home();
        focus_on(&mut wifi, Row::Add);
        wifi.press();
        assert_eq!(wifi.face, Face::Password);
        assert!(wifi.adding);
        assert_eq!(wifi.focus, PasswordFocus::Ssid);
        for c in "Gizli".chars() {
            assert!(wifi.typed(c));
        }
        assert_eq!(wifi.ssid, "Gizli");
        wifi.step(1);
        assert_eq!(wifi.focus, PasswordFocus::Security);
        wifi.step(1);
        assert_eq!(wifi.focus, PasswordFocus::Field);
        for c in "12345678".chars() {
            wifi.typed(c);
        }
        wifi.focus = PasswordFocus::Join;
        assert_eq!(wifi.press(), Press::Join);
        assert_eq!(wifi.target.as_ref().unwrap().ssid, "Gizli");
        assert_eq!(wifi.password(), "12345678");
        // "Güvenlik: Yok" needs no key and skips the field.
        let mut wifi = home();
        wifi.start_adding();
        wifi.ssid = "Açık".into();
        wifi.focus = PasswordFocus::Security;
        wifi.press();
        assert!(!wifi.secure);
        wifi.step(1);
        assert_eq!(wifi.focus, PasswordFocus::Keys, "no password field");
        wifi.focus = PasswordFocus::Join;
        assert_eq!(wifi.press(), Press::Join);
        assert!(!wifi.target.as_ref().unwrap().secured);
    }

    #[test]
    fn a_short_password_is_refused_before_the_radio_is_asked() {
        let mut wifi = home();
        focus_on(&mut wifi, Row::Network("Komşu".into()));
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
        let mut wifi = home();
        focus_on(&mut wifi, Row::Network("Komşu".into()));
        wifi.press();
        wifi.secret.push_str("something");
        assert!(wifi.back());
        assert_eq!(wifi.face, Face::List);
        assert!(wifi.password().is_empty());
        // And a second Back leaves the screen.
        assert!(!wifi.back());
    }

    #[test]
    fn one_scan_that_hears_nothing_does_not_empty_the_list() {
        let mut wifi = home();
        // "Bağlantıyı kes": the link goes, and the scan running under it
        // comes back with nothing.
        wifi.take_status(&json!({"present": true, "powered": true, "ssid": null,
                                 "remembered": ["YuHome", "YuHome5", "OnePlus"], "open": []}));
        wifi.take_networks(Vec::new());
        let listed: Vec<Row> = wifi.list_rows().into_iter().filter(|r| matches!(r, Row::Network(_))).collect();
        assert_eq!(listed.len(), 4, "every network one scan missed is still listed: {listed:?}");
        // Heard again: back to no misses.
        wifi.take_networks(vec![net("YuHome", -45, true)]);
        assert_eq!(wifi.network("YuHome").unwrap().missed, 0);
        // A second miss in a row takes the others off, as Android's 15 s age does.
        wifi.take_networks(vec![net("YuHome", -45, true)]);
        assert!(wifi.network("Komşu").is_none());
        assert!(wifi.network("Kafe").is_none());
        assert!(wifi.network("YuHome5").is_some_and(|n| !wifi.in_range(n)), "saved: kept for its page");
    }

    #[test]
    fn only_the_last_scan_asked_for_stops_the_spinner() {
        let mut wifi = home();
        // In, out and straight back in: two scans, the first answering late.
        let first = wifi.begin_scan();
        let second = wifi.begin_scan();
        wifi.end_scan(first);
        assert!(wifi.scanning, "the first scan's answer must not stop the second's spinner");
        wifi.end_scan(second);
        assert!(!wifi.scanning);
    }

    #[test]
    fn a_rescan_keeps_the_remote_on_the_same_network() {
        let mut wifi = home();
        focus_on(&mut wifi, Row::Network("Kafe".into()));
        wifi.take_networks(vec![net("Kafe", -40, false), net("YuHome", -45, true), net("Komşu", -70, true)]);
        assert_eq!(wifi.highlighted().unwrap().ssid, "Kafe");
    }

    #[test]
    fn signal_strength_is_in_settings_words() {
        assert_eq!(strength(-30), "Mükemmel");
        assert_eq!(strength(-60), "İyi");
        assert_eq!(strength(-70), "Yeterli");
        assert_eq!(strength(-85), "Zayıf");
        assert_eq!(strength(-95), "Yetersiz");
        assert_eq!(net("x", -60, true).bars(), 3);
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
    fn the_network_the_board_is_on_is_listed_at_once() {
        let mut wifi = Wifi::new();
        wifi.take_status(&serde_json::json!({
            "present": true, "powered": true, "ssid": "YuHome5", "signal": -42, "address": "10.27.27.34"
        }));
        // Before any scan: the connected network is there, under "Kayıtlı ağlar".
        assert_eq!(wifi.list_rows()[1..3], [Row::Header("Kayıtlı ağlar"), Row::Network("YuHome5".into())]);
        assert_eq!(wifi.networks[0].bars(), 4);
        // A cached scan that missed it does not drop it.
        wifi.take_networks(vec![net("Komşu", -50, true)]);
        assert_eq!(wifi.list_rows()[2], Row::Network("YuHome5".into()));
        // Left: gone, since it was known only because the board was on it.
        wifi.take_status(&serde_json::json!({"present": true, "powered": true, "ssid": null, "remembered": []}));
        assert!(wifi.network("YuHome5").is_none());
    }

    // ------------------------------------------------------------- Bluetooth

    /// The Plus as it answered, a found device or two added.
    fn plus() -> Bluetooth {
        let mut bt = Bluetooth::new();
        bt.take_status(&json!({
            "present": true,
            "controller": {"address": "B0:AC:82:E0:9C:AC", "powered": true},
            "scanning": false,
            "devices": [
                {"address": "C5", "name": "ROG AZOTH", "paired": true, "connected": false, "icon": "input-keyboard"},
                {"address": "DE", "name": "MX Master 3S", "paired": true, "connected": true, "icon": "input-mouse", "battery": 80},
                {"address": "A4", "name": "UR02", "paired": true, "connected": false},
                {"address": "70", "name": "VESTEL TV", "paired": false, "connected": false},
                {"address": "28", "name": null, "paired": false, "connected": false},
                {"address": "64", "name": "Android TV", "paired": false, "connected": false},
            ],
        }));
        bt
    }

    fn focus_bt(bt: &mut Bluetooth, wanted: BtLine) {
        bt.index = bt.focusable().iter().position(|row| *row == wanted).expect("row on the list");
    }

    fn device(address: &str) -> BtLine {
        BtLine::Device(address.into())
    }

    #[test]
    fn paired_devices_and_available_ones_are_under_their_own_headings() {
        let bt = plus();
        assert_eq!(
            bt.rows(),
            vec![
                BtLine::Toggle,
                BtLine::Header("Eşleşmiş cihazlar"),
                // Connected first, then by name.
                device("DE"),
                device("C5"),
                device("A4"),
                BtLine::Available,
                // By name; the nameless one is not listed.
                device("64"),
                device("70"),
            ]
        );
        assert_eq!(bt.summary(), "3 eşleşmiş cihaz · 1 bağlı");
        assert_eq!(bt.under(bt.device("DE").unwrap()), "Bağlı, pil %80");
        assert_eq!(bt.under(bt.device("C5").unwrap()), "");
        assert_eq!(bt.device("C5").unwrap().glyph(), "keyboard");
        assert_eq!(bt.device("A4").unwrap().glyph(), "bluetooth");
    }

    #[test]
    fn with_the_radio_off_the_list_says_how_to_see_devices() {
        let mut bt = Bluetooth::new();
        bt.take_status(&json!({"present": true, "controller": {"address": "B0", "powered": false}, "devices": [
            {"address": "C5", "name": "ROG AZOTH", "paired": true, "connected": false}
        ]}));
        assert!(matches!(bt.rows()[..], [BtLine::Toggle, BtLine::Note(_)]));
        assert_eq!(bt.press(), BtPress::Power(true));
    }

    /// The Plus after "Bluetooth" was switched off from this screen, as the
    /// daemon answered it.
    #[test]
    fn a_radio_switched_off_can_be_switched_on_again() {
        let mut bt = Bluetooth::new();
        bt.take_status(&json!({"present": false, "blocked": true, "controller": {}, "scanning": false, "devices": []}));
        assert!(bt.present);
        assert!(!bt.powered);
        assert_eq!(bt.summary(), "Kapalı");
        assert!(matches!(bt.rows()[..], [BtLine::Toggle, BtLine::Note(_)]));
        assert_eq!(bt.press(), BtPress::Power(true));
        // A board with no Bluetooth at all still says so.
        let mut none = Bluetooth::new();
        none.take_status(&json!({"present": false, "blocked": null}));
        assert!(!none.present);
        assert_eq!(none.press(), BtPress::None);
    }

    #[test]
    fn what_ok_does_to_a_device_depends_on_where_it_already_is() {
        let mut bt = plus();
        focus_bt(&mut bt, device("DE"));
        assert_eq!(bt.press(), BtPress::Menu);
        focus_bt(&mut bt, device("C5"));
        assert_eq!(bt.press(), BtPress::Do(BtOp::Connect, "C5".into()));
        focus_bt(&mut bt, device("70"));
        assert_eq!(bt.press(), BtPress::Do(BtOp::Pair, "70".into()));
        focus_bt(&mut bt, BtLine::Available);
        assert_eq!(bt.press(), BtPress::Scan);
    }

    #[test]
    fn a_second_ok_on_a_device_does_nothing_while_the_first_runs() {
        let mut bt = plus();
        focus_bt(&mut bt, device("70"));
        bt.begin(BtOp::Pair, "70");
        assert_eq!(bt.press(), BtPress::None);
        assert_eq!(bt.under(bt.device("70").unwrap()), "Eşleşiyor…");
        // Another device is not held up by it.
        focus_bt(&mut bt, device("C5"));
        assert_eq!(bt.press(), BtPress::Do(BtOp::Connect, "C5".into()));
        // A look taken meanwhile does not end it.
        let look = json!({"present": true, "controller": {"address": "B0", "powered": true}, "devices": []});
        bt.take_answer(Ok(look), &BtReply::Status);
        assert!(bt.pending.contains_key("70"));
        // Its own answer does.
        bt.take_answer(Err("Eşleştirilemedi: cihaz yanıt vermedi".into()), &BtReply::Done("70".into(), BtOp::Pair));
        assert!(bt.pending.is_empty());
        assert_eq!(bt.notice, "Eşleştirilemedi: cihaz yanıt vermedi");
    }

    #[test]
    fn paired_but_not_connected_is_not_shown_as_done() {
        let mut bt = plus();
        bt.begin(BtOp::Pair, "70");
        let answer = json!({
            "present": true,
            "controller": {"address": "B0", "powered": true},
            "devices": [{"address": "70", "name": "VESTEL TV", "paired": true, "connected": false}],
            "outcome": {"paired": true, "connected": false, "error": "cihaz yanıt vermedi"},
        });
        bt.take_answer(Ok(answer), &BtReply::Done("70".into(), BtOp::Pair));
        assert_eq!(bt.notice, "Eşleşti, ancak bağlanamadı: cihaz yanıt vermedi");
        assert!(bt.device("70").unwrap().paired);
        assert!(!bt.device("70").unwrap().connected);
        assert_eq!(bt.under(bt.device("70").unwrap()), "");
    }

    #[test]
    fn a_look_taken_while_the_radio_switches_is_not_shown() {
        let mut bt = plus();
        bt.busy = true;
        let off = json!({"present": false});
        bt.take_answer(Ok(off), &BtReply::Status);
        assert!(bt.present, "half-way state ignored");
        assert_eq!(bt.press(), BtPress::None);
        let on = json!({"present": true, "controller": {"address": "B0", "powered": false}});
        bt.take_answer(Ok(on), &BtReply::Power);
        assert!(!bt.busy);
        assert!(!bt.powered);
    }

    #[test]
    fn only_the_search_asked_for_or_the_daemon_ends_the_spinner() {
        let mut bt = plus();
        focus_bt(&mut bt, BtLine::Available);
        bt.begin_scan();
        // A look that set out before the search started.
        let idle = json!({"present": true, "controller": {"address": "B0", "powered": true}, "scanning": false});
        bt.take_answer(Ok(idle.clone()), &BtReply::Status);
        assert!(bt.scanning);
        assert_eq!(bt.press(), BtPress::None, "no second search on top");
        let running = json!({"present": true, "controller": {"address": "B0", "powered": true}, "scanning": true});
        bt.take_answer(Ok(running), &BtReply::Scan);
        assert!(bt.scanning);
        bt.take_answer(Ok(idle), &BtReply::Status);
        assert!(!bt.scanning);
        // A search that could not start says why and stops turning.
        bt.begin_scan();
        bt.take_answer(Err("Arama başlatılamadı".into()), &BtReply::Scan);
        assert!(!bt.scanning);
        assert_eq!(bt.notice, "Arama başlatılamadı");
    }

    #[test]
    fn the_remote_stays_on_its_device_when_the_list_changes() {
        let mut bt = plus();
        focus_bt(&mut bt, device("70"));
        // A search hears a new device that sorts above it, and the mouse
        // drops off.
        bt.take_status(&json!({
            "present": true,
            "controller": {"address": "B0", "powered": true},
            "devices": [
                {"address": "C5", "name": "ROG AZOTH", "paired": true, "connected": false},
                {"address": "11", "name": "AAA Kulaklık", "paired": false, "connected": false},
                {"address": "70", "name": "VESTEL TV", "paired": false, "connected": false},
            ],
        }));
        assert_eq!(bt.focused_row(), Some(device("70")));
        // Its device gone, the remote stays on the list.
        bt.take_status(&json!({"present": true, "controller": {"address": "B0", "powered": true}, "devices": []}));
        assert!(bt.focused_row().is_some());
    }

    #[test]
    fn an_unpaired_device_cannot_be_forgotten() {
        let mut bt = plus();
        focus_bt(&mut bt, device("70"));
        assert!(bt.menu().is_none());
    }

    #[test]
    fn a_paired_device_can_be_forgotten_but_not_while_a_verb_runs_on_it() {
        let mut bt = plus();
        focus_bt(&mut bt, device("DE"));
        let (title, menu) = bt.menu().unwrap();
        assert_eq!(title, "MX Master 3S");
        assert_eq!(
            ops(&menu),
            vec![Some(PeerOp::BtDisconnect("DE".into())), Some(PeerOp::BtForget("DE".into()))]
        );
        assert!(menu[1].ask.is_some(), "removing a pairing asks first");
        bt.begin(BtOp::Disconnect, "DE");
        assert!(bt.menu().is_none());
    }
}
