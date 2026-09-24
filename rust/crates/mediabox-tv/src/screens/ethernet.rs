//! One wired port's address, from the sofa: taken from the network or written
//! by hand, on trial until it is kept.
//!
//! The same shape as the display page, because it is the same kind of
//! decision: a **draft** that nothing outside this process has seen, "Uygula"
//! to put it on trial, and a question the daemon holds the clock for. What is
//! different is typing: an address is digits and dots, so a card opens a
//! number pad a remote can walk, and a keyboard can type straight into it.
//!
//! Nothing is decided here. What an acceptable address is lives in
//! `mediabox_core::EthernetConfig::validate`, which the daemon runs again
//! before anything reaches netplan.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use mediabox_core::{ETHERNET_TRIAL_SECONDS, EthernetConfig, EthernetPort, EthernetStatus, prefix_mask};

use super::output::CONFIRM_GUARD;

/// What a press asks the application to do beyond redrawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    Nothing,
    Changed,
    /// Put this on the port, on trial.
    Try(String, EthernetConfig),
    Keep,
    Revert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Moved,
    Unchanged,
    /// Left from the cards: back to the card that opened the page.
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Mode,
    Address,
    Mask,
    Gateway,
    Dns1,
    Dns2,
}

impl Field {
    fn title(self) -> &'static str {
        match self {
            Field::Mode => "Adres alma",
            Field::Address => "IP adresi",
            Field::Mask => "Ağ maskesi",
            Field::Gateway => "Ağ geçidi",
            Field::Dns1 => "DNS 1",
            Field::Dns2 => "DNS 2",
        }
    }

    /// Whether it may be left empty.
    fn optional(self) -> bool {
        matches!(self, Field::Gateway | Field::Dns1 | Field::Dns2)
    }
}

/// The draft, as typed: strings, so an address can be half-written while the
/// pad is open, and empty where a field is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    fixed: bool,
    address: String,
    prefix: u8,
    gateway: String,
    dns1: String,
    dns2: String,
}

impl Draft {
    fn from_config(config: &EthernetConfig) -> Self {
        match config {
            EthernetConfig::Dhcp => Draft {
                fixed: false,
                address: String::new(),
                prefix: 24,
                gateway: String::new(),
                dns1: String::new(),
                dns2: String::new(),
            },
            EthernetConfig::Static { address, prefix, gateway, dns } => Draft {
                fixed: true,
                address: address.to_string(),
                prefix: *prefix,
                gateway: gateway.map(|g| g.to_string()).unwrap_or_default(),
                dns1: dns.first().map(ToString::to_string).unwrap_or_default(),
                dns2: dns.get(1).map(ToString::to_string).unwrap_or_default(),
            },
        }
    }

    /// The draft as the daemon would take it, or why it would not.
    fn config(&self) -> Result<EthernetConfig, String> {
        if !self.fixed {
            return Ok(EthernetConfig::Dhcp);
        }
        let parse = |field: Field, text: &str| -> Result<Option<Ipv4Addr>, String> {
            if text.is_empty() {
                return if field.optional() {
                    Ok(None)
                } else {
                    Err(format!("{} girilmedi.", field.title()))
                };
            }
            text.parse()
                .map(Some)
                .map_err(|_| format!("{}: {text} geçerli bir IPv4 adresi değil.", field.title()))
        };
        let address = parse(Field::Address, &self.address)?.expect("required");
        let gateway = parse(Field::Gateway, &self.gateway)?;
        let dns = [parse(Field::Dns1, &self.dns1)?, parse(Field::Dns2, &self.dns2)?]
            .into_iter()
            .flatten()
            .collect();
        let config = EthernetConfig::Static { address, prefix: self.prefix, gateway, dns };
        config.validate()?;
        Ok(config)
    }

    fn text(&self, field: Field) -> &str {
        match field {
            Field::Address => &self.address,
            Field::Gateway => &self.gateway,
            Field::Dns1 => &self.dns1,
            Field::Dns2 => &self.dns2,
            Field::Mode | Field::Mask => "",
        }
    }

    fn set_text(&mut self, field: Field, text: String) {
        match field {
            Field::Address => self.address = text,
            Field::Gateway => self.gateway = text,
            Field::Dns1 => self.dns1 = text,
            Field::Dns2 => self.dns2 = text,
            Field::Mode | Field::Mask => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Mode,
    Mask,
}

/// The masks a list offers: /8 to /30, the range `validate` accepts.
const PREFIXES: std::ops::RangeInclusive<u8> = 8..=30;

/// The number pad, as rows of (label, span) on a grid four columns wide.
/// Words need two columns — the shared key grid draws a one-column key in the
/// letters' size — so the dot, the zero and "Sil" take the right-hand column
/// and "Tamam" sits under "Sil", where the hand already is.
const PAD: [&[(&str, u32)]; 4] = [
    &[("1", 1), ("2", 1), ("3", 1), (".", 1)],
    &[("4", 1), ("5", 1), ("6", 1), ("0", 1)],
    &[("7", 1), ("8", 1), ("9", 1), ("Sil", 1)],
    &[("Temizle", 2), ("Tamam", 2)],
];

/// The longest thing that can be typed: `255.255.255.255`.
const ENTRY_MAX: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    field: Field,
    text: String,
    row: usize,
    col: usize,
    error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Sheet {
    None,
    Pick { kind: Pick, focus: usize },
    Entry(Entry),
    /// "Bu ağ ayarı kalsın mı?", until `deadline`; 0 keeps, 1 goes back.
    Confirm { deadline: Instant, focus: usize, since: Instant },
}

pub struct Ethernet {
    status: Option<EthernetStatus>,
    port: String,
    /// "Ethernet" or "Ethernet 2": how the page and its card are called.
    title: String,
    draft: Option<Draft>,
    base: Option<Draft>,
    /// The card the remote is on; `rows().len()` for the buttons.
    row: usize,
    act: usize,
    sheet: Sheet,
    note: String,
    /// What was last asked of the daemon about a trial, so its end can be
    /// told apart from the clock running out.
    answered: Option<Press>,
    /// Asked once for the daemon's account after the countdown reached zero.
    polled: bool,
}

impl Ethernet {
    pub fn new() -> Self {
        Self {
            status: None,
            port: String::new(),
            title: String::new(),
            draft: None,
            base: None,
            row: 0,
            act: 0,
            sheet: Sheet::None,
            note: String::new(),
            answered: None,
            polled: false,
        }
    }

    /// The daemon's latest account of the ports. The draft is taken from it
    /// once per page, and again after a trial ends; in between it is the
    /// viewer's, so a poll never undoes what somebody is typing.
    pub fn load(&mut self, status: Option<EthernetStatus>) {
        self.status = status;
        let Some(port) = self.current().cloned() else { return };
        let trial = self
            .status
            .as_ref()
            .and_then(|status| status.trial.as_ref())
            .filter(|trial| trial.interface == self.port)
            .cloned();
        let kept = trial
            .as_ref()
            .map(|trial| Draft::from_config(&trial.config))
            .unwrap_or_else(|| Draft::from_config(&port.config));
        if self.draft.is_none() {
            self.draft = Some(kept.clone());
        }
        self.base = Some(kept);
        match (&trial, &self.sheet) {
            (Some(trial), Sheet::None | Sheet::Pick { .. } | Sheet::Entry(_)) => self.ask(trial.seconds_left),
            (None, Sheet::Confirm { .. }) => {
                let note = match self.answered.take() {
                    Some(Press::Keep) => "Kaydedildi.",
                    Some(Press::Revert) => "Önceki ayara dönüldü.",
                    _ => "Onay gelmedi; önceki ayara dönüldü.",
                };
                self.sheet = Sheet::None;
                self.draft = Some(Draft::from_config(&port.config));
                self.base = self.draft.clone();
                self.note = note.into();
            }
            _ => {}
        }
        self.settle();
    }

    fn current(&self) -> Option<&EthernetPort> {
        self.status.as_ref()?.ports.iter().find(|port| port.name == self.port)
    }

    /// Opens the page for one port.
    pub fn open(&mut self, port: &str, title: &str) -> bool {
        if !self
            .status
            .as_ref()
            .is_some_and(|status| status.ports.iter().any(|p| p.name == port))
        {
            return false;
        }
        self.port = port.to_string();
        self.title = title.to_string();
        self.draft = None;
        self.base = None;
        self.row = 0;
        self.act = 0;
        self.sheet = Sheet::None;
        self.note.clear();
        let status = self.status.take();
        self.load(status);
        true
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn port(&self) -> &str {
        &self.port
    }

    pub fn available(&self) -> bool {
        self.current().is_some() && self.draft.is_some()
    }

    pub fn asking(&self) -> bool {
        matches!(self.sheet, Sheet::Confirm { .. })
    }

    /// Whether keys typed on a keyboard belong to the number pad.
    pub fn typing(&self) -> bool {
        matches!(self.sheet, Sheet::Entry(_))
    }

    fn ask(&mut self, seconds: u32) {
        let now = Instant::now();
        self.sheet = Sheet::Confirm {
            deadline: now + Duration::from_secs(seconds.into()),
            focus: 0,
            since: now,
        };
        self.polled = false;
    }

    fn confirm_ready(&self) -> bool {
        match self.sheet {
            Sheet::Confirm { since, .. } => since.elapsed() >= CONFIRM_GUARD,
            _ => true,
        }
    }

    /// The seconds left on the question, and whether its buttons take Ok.
    pub fn countdown(&self) -> Option<(u32, bool)> {
        match self.sheet {
            Sheet::Confirm { deadline, .. } => Some((
                deadline.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u32,
                self.confirm_ready(),
            )),
            _ => None,
        }
    }

    /// True once, when the countdown has run out and the daemon's account
    /// should be asked for: it has taken the address back by now.
    pub fn due_for_status(&mut self) -> bool {
        if self.countdown().is_some_and(|(seconds, _)| seconds == 0) && !self.polled {
            self.polled = true;
            return true;
        }
        false
    }

    /// What was sent to the daemon, so the end of the trial is described by
    /// what caused it.
    pub fn sent(&mut self, press: &Press) {
        if matches!(press, Press::Keep | Press::Revert) {
            self.answered = Some(press.clone());
        }
    }

    pub fn refused(&mut self, error: String) {
        self.note = error;
    }

    fn draft(&self) -> Draft {
        self.draft.clone().unwrap_or_else(|| Draft::from_config(&EthernetConfig::Dhcp))
    }

    fn fields(&self) -> Vec<Field> {
        if self.draft().fixed {
            vec![Field::Mode, Field::Address, Field::Mask, Field::Gateway, Field::Dns1, Field::Dns2]
        } else {
            vec![Field::Mode]
        }
    }

    pub fn unsaved(&self) -> bool {
        match (&self.draft, &self.base) {
            (Some(draft), Some(base)) => {
                // Two drafts that say the same thing are the same, however
                // they were typed.
                match (draft.config(), base.config()) {
                    (Ok(a), Ok(b)) => a != b,
                    _ => draft != base,
                }
            }
            _ => false,
        }
    }

    fn settle(&mut self) {
        self.row = self.row.min(self.fields().len());
        self.act = self.act.min(1);
    }

    #[cfg(test)]
    pub fn chosen(&self) -> Result<EthernetConfig, String> {
        self.draft().config()
    }

    // ------------------------------------------------------------- moving

    pub fn step(&mut self, dx: i32, dy: i32) -> Nav {
        match &mut self.sheet {
            Sheet::Confirm { focus, .. } => {
                if dx != 0 {
                    *focus = usize::from(dx > 0);
                    return Nav::Moved;
                }
                return Nav::Unchanged;
            }
            Sheet::Pick { kind, focus } => {
                let count = match kind {
                    Pick::Mode => 2,
                    Pick::Mask => PREFIXES.count(),
                };
                if dy == 0 {
                    return Nav::Unchanged;
                }
                let next = (*focus as i32 + dy).clamp(0, count as i32 - 1) as usize;
                let moved = next != *focus;
                *focus = next;
                return if moved { Nav::Moved } else { Nav::Unchanged };
            }
            Sheet::Entry(entry) => return step_pad(entry, dx, dy),
            Sheet::None => {}
        }
        let rows = self.fields().len();
        let before = (self.row, self.act);
        if self.row == rows {
            if dy < 0 {
                self.row = rows - 1;
            } else if dx < 0 && self.act == 0 {
                return Nav::Leave;
            } else if dx != 0 {
                self.act = (self.act as i32 + dx).clamp(0, 1) as usize;
            }
        } else if dx < 0 {
            return Nav::Leave;
        } else if dy != 0 {
            self.row = (self.row as i32 + dy).clamp(0, rows as i32) as usize;
            if self.row == rows {
                self.act = 0;
            }
        }
        if before == (self.row, self.act) {
            Nav::Unchanged
        } else {
            self.note.clear();
            Nav::Moved
        }
    }

    // ------------------------------------------------------------ pressing

    pub fn press(&mut self) -> Press {
        match self.sheet.clone() {
            Sheet::Confirm { focus, .. } => {
                if !self.confirm_ready() {
                    return Press::Nothing;
                }
                return if focus == 0 { Press::Keep } else { Press::Revert };
            }
            Sheet::Pick { kind, focus } => {
                let mut draft = self.draft();
                match kind {
                    Pick::Mode => {
                        let fixed = focus == 1;
                        if fixed && !draft.fixed && draft.address.is_empty() {
                            self.prefill(&mut draft);
                        }
                        draft.fixed = fixed;
                    }
                    Pick::Mask => draft.prefix = PREFIXES.start() + focus as u8,
                }
                self.draft = Some(draft);
                self.sheet = Sheet::None;
                self.settle();
                return Press::Changed;
            }
            Sheet::Entry(entry) => return self.press_pad(entry),
            Sheet::None => {}
        }
        self.note.clear();
        let fields = self.fields();
        if self.row == fields.len() {
            return match self.act {
                0 if self.unsaved() => match self.draft().config() {
                    Ok(config) => Press::Try(self.port.clone(), config),
                    Err(error) => {
                        self.note = error;
                        Press::Changed
                    }
                },
                0 => Press::Nothing,
                _ if self.unsaved() => {
                    self.draft = self.base.clone();
                    self.note = "Taslak bırakıldı.".into();
                    self.settle();
                    Press::Changed
                }
                _ => Press::Nothing,
            };
        }
        let draft = self.draft();
        match fields[self.row] {
            Field::Mode => {
                self.sheet = Sheet::Pick { kind: Pick::Mode, focus: usize::from(draft.fixed) };
            }
            Field::Mask => {
                self.sheet = Sheet::Pick {
                    kind: Pick::Mask,
                    focus: usize::from(draft.prefix.clamp(*PREFIXES.start(), *PREFIXES.end()) - PREFIXES.start()),
                };
            }
            field => {
                self.sheet = Sheet::Entry(Entry {
                    field,
                    text: draft.text(field).to_string(),
                    row: 0,
                    col: 0,
                    error: String::new(),
                });
            }
        }
        Press::Changed
    }

    /// Switching to a hand-written address starts from the one the port has
    /// now, so the common case — keep the address the router gave, stop it
    /// changing — is one press.
    fn prefill(&self, draft: &mut Draft) {
        let Some(port) = self.current() else { return };
        if let Some((address, prefix)) = port
            .addresses
            .first()
            .and_then(|cidr| cidr.split_once('/'))
            .and_then(|(address, prefix)| Some((address.to_string(), prefix.parse::<u8>().ok()?)))
        {
            draft.address = address;
            draft.prefix = prefix;
        }
        if let Some(gateway) = &port.gateway {
            draft.gateway = gateway.clone();
        }
    }

    fn press_pad(&mut self, mut entry: Entry) -> Press {
        let Some((label, _)) = PAD.get(entry.row).and_then(|row| row.get(entry.col)).copied() else {
            return Press::Nothing;
        };
        entry.error.clear();
        match label {
            "Sil" => {
                entry.text.pop();
            }
            "Temizle" => entry.text.clear(),
            "Tamam" => return self.commit(entry),
            key => {
                if entry.text.len() < ENTRY_MAX {
                    entry.text.push_str(key);
                }
            }
        }
        self.sheet = Sheet::Entry(entry);
        Press::Changed
    }

    /// Takes what was typed into the draft, if it is an address — or nothing,
    /// for a field that may be empty.
    fn commit(&mut self, mut entry: Entry) -> Press {
        let ok = if entry.text.is_empty() {
            entry.field.optional()
        } else {
            entry.text.parse::<Ipv4Addr>().is_ok()
        };
        if !ok {
            entry.error = if entry.text.is_empty() {
                format!("{} boş bırakılamaz.", entry.field.title())
            } else {
                format!("{} geçerli bir IPv4 adresi değil.", entry.text)
            };
            self.sheet = Sheet::Entry(entry);
            return Press::Changed;
        }
        let mut draft = self.draft();
        draft.set_text(entry.field, entry.text);
        self.draft = Some(draft);
        self.sheet = Sheet::None;
        Press::Changed
    }

    /// A key from a keyboard, while the pad is open: digits and dots typed,
    /// Enter as "Tamam".
    pub fn typed(&mut self, c: char) -> bool {
        let Sheet::Entry(mut entry) = self.sheet.clone() else { return false };
        match c {
            '0'..='9' | '.' if entry.text.len() < ENTRY_MAX => {
                entry.text.push(c);
                entry.error.clear();
            }
            '\r' | '\n' => {
                self.commit(entry);
                return true;
            }
            _ => return false,
        }
        self.sheet = Sheet::Entry(entry);
        true
    }

    /// Backspace on a keyboard: a digit while the pad is open.
    pub fn backspace(&mut self) -> bool {
        let Sheet::Entry(mut entry) = self.sheet.clone() else { return false };
        entry.text.pop();
        entry.error.clear();
        self.sheet = Sheet::Entry(entry);
        true
    }

    /// Back. `None` when there is nothing for it to do here, so the page
    /// closes.
    pub fn back(&mut self) -> Option<Press> {
        match self.sheet {
            Sheet::Confirm { .. } => Some(Press::Revert),
            Sheet::Pick { .. } | Sheet::Entry(_) => {
                self.sheet = Sheet::None;
                Some(Press::Changed)
            }
            Sheet::None => None,
        }
    }

    // ------------------------------------------------------------ the view

    pub fn view(&self) -> View {
        let Some(port) = self.current() else {
            return View {
                message: "Bu port artık görünmüyor.".into(),
                ..View::default()
            };
        };
        let draft = self.draft();
        let unsaved = self.unsaved();
        let valid = draft.config();
        let focus = |row: usize| self.sheet == Sheet::None && self.row == row;
        let cards = self
            .fields()
            .into_iter()
            .enumerate()
            .map(|(index, field)| {
                let (value, hint, icon) = match field {
                    Field::Mode => (
                        if draft.fixed { "Elle (statik)" } else { "Otomatik (DHCP)" }.to_string(),
                        if draft.fixed {
                            "Adres aşağıda elle yazılır"
                        } else {
                            "Adresi ağdaki yönlendirici verir"
                        }
                        .to_string(),
                        "network",
                    ),
                    Field::Address => (
                        or_word(&draft.address, "Girilmedi"),
                        "Bu portun adresi".into(),
                        "network",
                    ),
                    Field::Mask => (
                        format!("{} (/{})", prefix_mask(draft.prefix), draft.prefix),
                        "Ağın büyüklüğü".into(),
                        "resolution",
                    ),
                    Field::Gateway => (
                        or_word(&draft.gateway, "Yok"),
                        "İnternete çıkış; boş bırakılabilir".into(),
                        "restart",
                    ),
                    Field::Dns1 | Field::Dns2 => (
                        or_word(draft.text(field), "Yok"),
                        "Ad sunucusu; boş bırakılabilir".into(),
                        "info",
                    ),
                };
                CardView {
                    icon: icon.into(),
                    label: field.title().into(),
                    hint,
                    value,
                    focused: focus(index),
                }
            })
            .collect::<Vec<_>>();
        let rows = cards.len();
        let actions = ["Uygula", "Geri al"]
            .iter()
            .enumerate()
            .map(|(index, label)| ActionView {
                label: (*label).into(),
                enabled: unsaved && (index == 1 || valid.is_ok()),
                focused: self.sheet == Sheet::None && self.row == rows && self.act == index,
            })
            .collect();

        let note = if !self.note.is_empty() {
            self.note.clone()
        } else if let (true, Err(error)) = (unsaved, &valid) {
            error.clone()
        } else if unsaved {
            format!(
                "Taslak henüz uygulanmadı. Uygula'ya basınca {ETHERNET_TRIAL_SECONDS} saniye içinde onay istenir; onaylanmazsa önceki ayara dönülür."
            )
        } else {
            "Ayar bu port için saklanır ve yeniden başlatmadan sonra da geçerlidir.".into()
        };
        let (state, state_tone) = if self.asking() {
            ("Onay bekliyor", "warn")
        } else if unsaved {
            ("Uygulanmadı", "warn")
        } else {
            ("Etkin", "good")
        };

        let (picker_open, picker_title, picker, picker_focus) = match self.sheet {
            Sheet::Pick { kind: Pick::Mode, focus } => (
                true,
                "Adres alma".to_string(),
                vec![
                    choice("Otomatik (DHCP)", "Adresi ağdaki yönlendirici verir", !draft.fixed, focus == 0),
                    choice("Elle (statik)", "Adresi, maskeyi ve ağ geçidini siz yazarsınız", draft.fixed, focus == 1),
                ],
                focus,
            ),
            Sheet::Pick { kind: Pick::Mask, focus } => (
                true,
                "Ağ maskesi".to_string(),
                PREFIXES
                    .enumerate()
                    .map(|(index, prefix)| {
                        let hosts = (1u64 << (32 - u32::from(prefix))) - 2;
                        choice(
                            &prefix_mask(prefix).to_string(),
                            &format!("/{prefix} · {hosts} adres"),
                            prefix == draft.prefix,
                            index == focus,
                        )
                    })
                    .collect(),
                focus,
            ),
            _ => (false, String::new(), Vec::new(), 0),
        };

        let (entry_open, entry_title, entry_text, entry_hint, entry_error, key_row, key_col) =
            match &self.sheet {
                Sheet::Entry(entry) => (
                    true,
                    entry.field.title().to_string(),
                    entry.text.clone(),
                    if entry.field.optional() {
                        "Örnek: 192.168.1.1 · boş bırakıp Tamam'a basılabilir".to_string()
                    } else {
                        "Örnek: 192.168.1.50".to_string()
                    },
                    entry.error.clone(),
                    entry.row,
                    entry.col,
                ),
                _ => (false, String::new(), String::new(), String::new(), String::new(), 0, 0),
            };

        let (sheet, seconds, confirm_focus) = match &self.sheet {
            Sheet::Confirm { deadline, focus, .. } => (
                1,
                deadline.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u32,
                *focus,
            ),
            _ => (0, 0, 0),
        };
        let trial = self
            .status
            .as_ref()
            .and_then(|status| status.trial.as_ref())
            .filter(|trial| trial.interface == self.port);
        let describe = |config: &EthernetConfig| match config {
            EthernetConfig::Dhcp => "Otomatik (DHCP)".to_string(),
            EthernetConfig::Static { address, prefix, gateway, .. } => match gateway {
                Some(gateway) => format!("Elle · {address}/{prefix} · ağ geçidi {gateway}"),
                None => format!("Elle · {address}/{prefix}"),
            },
        };

        View {
            available: true,
            message: String::new(),
            cards,
            actions,
            note,
            state: state.into(),
            state_tone: state_tone.into(),
            current_title: port.addresses.first().cloned().unwrap_or_else(|| "Adres yok".into()),
            current_line: if port.carrier { "Kablo takılı" } else { "Kablo takılı değil" }.into(),
            current_badges: vec![
                (
                    if matches!(port.config, EthernetConfig::Dhcp) { "DHCP" } else { "ELLE" }.into(),
                    String::new(),
                ),
                if port.carrier {
                    ("BAĞLI".into(), "good".into())
                } else {
                    ("KABLO YOK".into(), "warn".into())
                },
            ],
            current_rows: vec![
                ("Arayüz".into(), port.name.clone()),
                ("MAC".into(), port.mac.clone().unwrap_or_else(|| "—".into())),
                ("Ağ geçidi".into(), port.gateway.clone().unwrap_or_else(|| "—".into())),
            ],
            picker_open,
            picker_title,
            picker,
            picker_focus,
            entry_open,
            entry_title,
            entry_text,
            entry_hint,
            entry_error,
            keys: PAD
                .iter()
                .map(|row| row.iter().map(|(label, span)| ((*label).to_string(), *span)).collect())
                .collect(),
            key_row,
            key_col,
            sheet,
            seconds,
            arc: crate::vitals::arc(f64::from(seconds) / f64::from(ETHERNET_TRIAL_SECONDS)),
            confirm_ready: self.confirm_ready(),
            confirm_focus,
            trial: trial.map(|trial| describe(&trial.config)).unwrap_or_default(),
            previous: trial.map(|trial| describe(&trial.previous)).unwrap_or_default(),
            trial_now: format!(
                "Şu an: {} · {}",
                port.addresses.first().map(String::as_str).unwrap_or("adres yok"),
                if port.carrier { "kablo takılı" } else { "kablo takılı değil" }
            ),
        }
    }
}

fn or_word(text: &str, word: &str) -> String {
    if text.is_empty() { word.into() } else { text.into() }
}

fn choice(title: &str, sub: &str, selected: bool, focused: bool) -> ChoiceView {
    ChoiceView { title: title.into(), sub: sub.into(), selected, focused }
}

/// The remote on the number pad: rows of different widths line up by the
/// column each key starts at, so Down from "Sil" lands on "Tamam".
fn step_pad(entry: &mut Entry, dx: i32, dy: i32) -> Nav {
    let before = (entry.row, entry.col);
    if dx != 0 {
        let len = PAD[entry.row].len() as i32;
        entry.col = (entry.col as i32 + dx).clamp(0, len - 1) as usize;
    } else if dy != 0 {
        let start: u32 = PAD[entry.row][..entry.col].iter().map(|(_, span)| span).sum();
        let row = (entry.row as i32 + dy).clamp(0, PAD.len() as i32 - 1) as usize;
        let mut at = 0u32;
        let mut col = PAD[row].len() - 1;
        for (index, (_, span)) in PAD[row].iter().enumerate() {
            if start < at + span {
                col = index;
                break;
            }
            at += span;
        }
        entry.row = row;
        entry.col = col;
    }
    if before == (entry.row, entry.col) { Nav::Unchanged } else { Nav::Moved }
}

#[derive(Debug, Clone, Default)]
pub struct CardView {
    pub icon: String,
    pub label: String,
    pub hint: String,
    pub value: String,
    pub focused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ActionView {
    pub label: String,
    pub enabled: bool,
    pub focused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ChoiceView {
    pub title: String,
    pub sub: String,
    pub selected: bool,
    pub focused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct View {
    pub available: bool,
    pub message: String,
    pub cards: Vec<CardView>,
    pub actions: Vec<ActionView>,
    pub note: String,
    pub state: String,
    pub state_tone: String,
    pub current_title: String,
    pub current_line: String,
    pub current_badges: Vec<(String, String)>,
    pub current_rows: Vec<(String, String)>,
    pub picker_open: bool,
    pub picker_title: String,
    pub picker: Vec<ChoiceView>,
    pub picker_focus: usize,
    pub entry_open: bool,
    pub entry_title: String,
    pub entry_text: String,
    pub entry_hint: String,
    pub entry_error: String,
    pub keys: Vec<Vec<(String, u32)>>,
    pub key_row: usize,
    pub key_col: usize,
    pub sheet: i32,
    pub seconds: u32,
    pub arc: String,
    pub confirm_ready: bool,
    pub confirm_focus: usize,
    pub trial: String,
    pub previous: String,
    pub trial_now: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::EthernetTrial;

    fn status(config: EthernetConfig, trial: Option<EthernetTrial>) -> EthernetStatus {
        EthernetStatus {
            ports: vec![
                EthernetPort {
                    name: "end0".into(),
                    mac: Some("c0:74:2b:00:00:01".into()),
                    carrier: true,
                    addresses: vec!["192.168.2.50/24".into()],
                    gateway: Some("192.168.2.1".into()),
                    config,
                },
                EthernetPort { name: "end1".into(), ..EthernetPort::default() },
            ],
            trial,
            error: None,
        }
    }

    fn page() -> Ethernet {
        let mut page = Ethernet::new();
        page.load(Some(status(EthernetConfig::Dhcp, None)));
        assert!(page.open("end0", "Ethernet 1"));
        page
    }

    fn after_guard(page: &mut Ethernet) {
        if let Sheet::Confirm { since, .. } = &mut page.sheet {
            *since -= CONFIRM_GUARD;
        }
    }

    /// Types `text` on the pad with the remote, key by key, and presses Tamam.
    fn type_on_pad(page: &mut Ethernet, text: &str) {
        let Sheet::Entry(entry) = &mut page.sheet else { panic!("no pad open") };
        entry.text.clear();
        for c in text.chars() {
            let (row, col) = PAD
                .iter()
                .enumerate()
                .find_map(|(r, keys)| keys.iter().position(|(l, _)| l.starts_with(c)).map(|k| (r, k)))
                .unwrap();
            let Sheet::Entry(entry) = &mut page.sheet else { unreachable!() };
            entry.row = row;
            entry.col = col;
            assert_eq!(page.press(), Press::Changed);
        }
        let Sheet::Entry(entry) = &mut page.sheet else { unreachable!() };
        entry.row = 3;
        entry.col = 1;
        page.press();
    }

    #[test]
    fn a_dhcp_port_shows_one_card() {
        let page = page();
        let view = page.view();
        assert_eq!(view.cards.len(), 1);
        assert_eq!(view.cards[0].value, "Otomatik (DHCP)");
        assert_eq!(view.current_title, "192.168.2.50/24");
        assert!(!page.unsaved());
    }

    /// "Elle" starts from the address the port has now, so keeping it is one
    /// press away — and nothing is sent until Uygula.
    #[test]
    fn switching_to_a_written_address_starts_from_the_current_one() {
        let mut page = page();
        assert_eq!(page.press(), Press::Changed); // the list
        page.step(0, 1); // Elle
        assert_eq!(page.press(), Press::Changed);
        let view = page.view();
        assert_eq!(view.cards.len(), 6);
        assert_eq!(view.cards[1].value, "192.168.2.50");
        assert_eq!(view.cards[2].value, "255.255.255.0 (/24)");
        assert_eq!(view.cards[3].value, "192.168.2.1");
        assert!(page.unsaved());
        assert_eq!(
            page.chosen(),
            Ok(EthernetConfig::Static {
                address: "192.168.2.50".parse().unwrap(),
                prefix: 24,
                gateway: Some("192.168.2.1".parse().unwrap()),
                dns: vec![],
            })
        );
    }

    #[test]
    fn an_address_is_typed_on_the_pad_and_applied_on_trial() {
        let mut page = page();
        page.press();
        page.step(0, 1);
        page.press(); // Elle
        page.step(0, 1); // IP adresi
        page.press();
        assert!(page.typing());
        type_on_pad(&mut page, "192.168.2.80");
        assert!(!page.typing());
        page.step(0, 3); // DNS 1
        page.press();
        type_on_pad(&mut page, "1.1.1.1");
        for _ in 0..3 {
            page.step(0, 1);
        }
        let expected = EthernetConfig::Static {
            address: "192.168.2.80".parse().unwrap(),
            prefix: 24,
            gateway: Some("192.168.2.1".parse().unwrap()),
            dns: vec!["1.1.1.1".parse().unwrap()],
        };
        assert_eq!(page.press(), Press::Try("end0".into(), expected));
    }

    #[test]
    fn a_bad_address_stays_on_the_pad_and_says_why() {
        let mut page = page();
        page.press();
        page.step(0, 1);
        page.press();
        page.step(0, 1);
        page.press();
        type_on_pad(&mut page, "192.168.2.300");
        assert!(page.typing(), "the pad stays open");
        assert!(page.view().entry_error.contains("geçerli bir IPv4"));
        // Back closes it and the draft keeps the address it had.
        assert_eq!(page.back(), Some(Press::Changed));
        assert_eq!(page.view().cards[1].value, "192.168.2.50");
    }

    /// A draft the rules refuse cannot be applied, and the note says which
    /// rule.
    #[test]
    fn a_draft_the_rules_refuse_cannot_be_applied() {
        let mut page = page();
        page.press();
        page.step(0, 1);
        page.press();
        page.step(0, 3); // Ağ geçidi
        page.press();
        type_on_pad(&mut page, "10.0.0.1");
        let view = page.view();
        assert!(view.note.contains("ağının içinde değil"), "{}", view.note);
        assert!(!view.actions[0].enabled, "Uygula");
        assert!(view.actions[1].enabled, "Geri al");
    }

    #[test]
    fn a_keyboard_types_into_the_pad() {
        let mut page = page();
        page.press();
        page.step(0, 1);
        page.press();
        page.step(0, 1);
        page.press();
        let Sheet::Entry(entry) = &mut page.sheet else { panic!() };
        entry.text.clear();
        for c in "10.0.0.9".chars() {
            assert!(page.typed(c));
        }
        assert!(!page.typed('x'), "only digits and dots");
        assert!(page.backspace());
        assert!(page.typed('8'));
        assert!(page.typed('\r'));
        assert!(!page.typing());
        assert_eq!(page.view().cards[1].value, "10.0.0.8");
    }

    #[test]
    fn the_mask_is_chosen_from_a_list_on_the_current_one() {
        let mut page = page();
        page.press();
        page.step(0, 1);
        page.press();
        page.step(0, 2); // Ağ maskesi
        page.press();
        let view = page.view();
        assert!(view.picker_open);
        assert_eq!(view.picker[view.picker_focus].title, "255.255.255.0");
        page.step(0, -8); // /16
        page.press();
        assert_eq!(page.view().cards[2].value, "255.255.0.0 (/16)");
    }

    /// Down from the middle of the pad's last full row lands on the wide key.
    #[test]
    fn the_pad_walks_by_columns() {
        let mut entry = Entry { field: Field::Address, text: String::new(), row: 2, col: 3, error: String::new() };
        step_pad(&mut entry, 0, 1);
        assert_eq!((entry.row, entry.col), (3, 1), "Sil -> Tamam");
        step_pad(&mut entry, 0, -1);
        assert_eq!((entry.row, entry.col), (2, 2), "Tamam -> 9, the column it starts in");
        step_pad(&mut entry, 0, 1);
        step_pad(&mut entry, -1, 0);
        step_pad(&mut entry, 0, -1);
        assert_eq!((entry.row, entry.col), (2, 0), "Temizle -> 7");
    }

    // --------------------------------------------------------------- trial

    fn on_trial(page: &mut Ethernet, config: EthernetConfig) {
        page.load(Some(status(
            EthernetConfig::Dhcp,
            Some(EthernetTrial {
                interface: "end0".into(),
                config,
                previous: EthernetConfig::Dhcp,
                seconds_left: ETHERNET_TRIAL_SECONDS,
            }),
        )));
    }

    fn fixed() -> EthernetConfig {
        EthernetConfig::Static {
            address: "192.168.2.80".parse().unwrap(),
            prefix: 24,
            gateway: None,
            dns: vec![],
        }
    }

    #[test]
    fn a_trial_is_asked_about_and_ok_waits_for_the_guard() {
        let mut page = page();
        on_trial(&mut page, fixed());
        assert!(page.asking());
        let view = page.view();
        assert_eq!(view.trial, "Elle · 192.168.2.80/24");
        assert_eq!(view.previous, "Otomatik (DHCP)");
        assert!(!view.confirm_ready);
        assert_eq!(page.press(), Press::Nothing, "the press that applied it");
        after_guard(&mut page);
        assert_eq!(page.press(), Press::Keep);
        page.step(1, 0);
        assert_eq!(page.press(), Press::Revert);
        assert_eq!(page.back(), Some(Press::Revert), "Back is never held");
    }

    /// The end of a trial is described by what ended it.
    #[test]
    fn the_end_of_a_trial_says_what_ended_it() {
        for (sent, note) in [
            (Some(Press::Keep), "Kaydedildi."),
            (Some(Press::Revert), "Önceki ayara dönüldü."),
            (None, "Onay gelmedi; önceki ayara dönüldü."),
        ] {
            let mut page = page();
            on_trial(&mut page, fixed());
            if let Some(press) = &sent {
                page.sent(press);
            }
            let kept = if sent == Some(Press::Keep) { fixed() } else { EthernetConfig::Dhcp };
            page.load(Some(status(kept.clone(), None)));
            assert!(!page.asking());
            assert_eq!(page.view().note, note);
            assert!(!page.unsaved(), "the draft is what the daemon has now");
            assert_eq!(page.chosen(), Ok(kept));
        }
    }

    #[test]
    fn a_poll_does_not_undo_what_is_being_typed() {
        let mut page = page();
        page.press();
        page.step(0, 1);
        page.press();
        page.load(Some(status(EthernetConfig::Dhcp, None)));
        assert!(page.unsaved());
        assert_eq!(page.view().cards.len(), 6);
    }

    #[test]
    fn left_leaves_the_page_and_back_closes_what_is_open() {
        let mut page = page();
        assert_eq!(page.step(-1, 0), Nav::Leave);
        page.press();
        assert_eq!(page.back(), Some(Press::Changed));
        assert_eq!(page.back(), None);
    }
}
