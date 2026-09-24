//! What the box can be told, from the sofa.
//!
//! Seven categories down the left, the chosen one's contents on the right, and
//! the same rule as the rest of the interface: the remote is on exactly one
//! place at a time, crossing between the column and the right-hand side is
//! Left and Right, and Back always goes one level up, never a guess.
//!
//! A category's contents are cards and readings. A card does something or
//! leads somewhere — a page of its own on the same right-hand side (an account,
//! the display, the fan curve) or another screen (Wi-Fi, the diagnostics) — and
//! it is the only thing focus ever lands on. A reading only says what is true:
//! that the box is on the network, that CEC found the television, which
//! version this is. This is an appliance and the things worth changing from a
//! television remote are few; what belongs here is being able to *see* the
//! rest, and to act on the handful of things a person actually does.
//!
//! The destructive rows are the reason [`Action::confirms`] exists. Nothing on
//! this screen restarts or shuts down the appliance on one press.

use mediabox_core::{FanStatus, LedMode};
use serde_json::Value;

use crate::model::DisplayStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Opens the diagnostics screen.
    OpenDiagnostics,
    /// Opens the Wi-Fi screen, and the Bluetooth one. Both are their own route
    /// for the reason the account form is: a list of networks the size of the
    /// panel, and a letter grid under it, do not belong in the right-hand
    /// column of a two-pane screen.
    OpenWifi,
    OpenBluetooth,
    /// Opens the Stremio sign-in screen. The form lives on its own route
    /// because it needs a letter grid, and a letter grid does not fit in the
    /// right-hand column of a two-pane settings screen.
    OpenAccount,
    /// Disconnect the Stremio account. Somebody's add-ons stop being the ones
    /// this box uses, so it asks first.
    SignOut,
    /// Opens the list of the lights' modes, with the one they are in marked.
    ChooseLeds,
    /// Set the board's indicator lights to this mode: what choosing one in
    /// that list does.
    SetLeds(LedMode),
    /// CEC: wake the television, or send it to standby. Neither touches this
    /// appliance's own power state — the daemon owns /dev/cec0 and this is a
    /// message to the panel.
    WakeTelevision,
    StandbyTelevision,
    /// Restart Kodi. Playback is somebody else's evening, so this confirms too.
    RestartPlayer,
    /// The appliance itself.
    Restart,
    Shutdown,
}

impl Action {
    /// Whether choosing this row opens a confirmation rather than doing it.
    ///
    /// Everything that interrupts what is on the television, and everything
    /// that ends the session, is behind a second press on a sheet whose focus
    /// starts on "Vazgeç".
    pub fn confirms(self) -> bool {
        matches!(
            self,
            Action::RestartPlayer | Action::Restart | Action::Shutdown | Action::SignOut
        )
    }

    pub fn question(self) -> &'static str {
        match self {
            Action::RestartPlayer => "Oynatıcı yeniden başlatılsın mı?",
            Action::SignOut => "Stremio hesabının bağlantısı kesilsin mi?",
            Action::Restart => "Cihaz yeniden başlatılsın mı?",
            Action::Shutdown => "Cihaz kapatılsın mı?",
            _ => "",
        }
    }

    /// Whether this goes somewhere rather than doing something, and so carries
    /// a chevron.
    fn leads_somewhere(self) -> bool {
        matches!(
            self,
            Action::OpenDiagnostics
                | Action::OpenWifi
                | Action::OpenBluetooth
                | Action::OpenAccount
                | Action::ChooseLeds
        )
    }
}

/// A page a card opens on the same right-hand side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Playback,
    Account,
    /// The display editor.
    Output,
    /// The fan curve editor.
    Cooling,
    /// One wired port's address. Last, because the list pages index arrays by
    /// their place and this is not one of them.
    Ethernet,
}

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Page::Playback => "Oynatma",
            Page::Account => "Hesap",
            Page::Output => OUTPUT,
            Page::Cooling => COOLING,
            Page::Ethernet => "Ethernet",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Page::Playback => "Oynatıcının durumu ve yeniden başlatılması",
            Page::Account => "Stremio hesabı ve eklentileri",
            Page::Output => "Çözünürlük, yenileme hızı ve renk",
            Page::Cooling => "Fan eğrisi ve işlemci sıcaklığı",
            Page::Ethernet => "Adres ayarı",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Page::Playback => "play",
            Page::Account => "user",
            Page::Output => "monitor",
            Page::Cooling => "fan",
            Page::Ethernet => "network",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// How a row is drawn, and whether focus can land on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Header,
    Reading,
    /// A card that leads somewhere.
    Link,
    /// A card that does something.
    Action,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Header => "header",
            Kind::Reading => "reading",
            Kind::Link => "link",
            Kind::Action => "action",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub hint: String,
    /// "" for a reading, "good" / "warn" / "bad" for one that has a verdict.
    pub tone: String,
    /// A mark from the interface's own table, "" for none.
    pub icon: &'static str,
    pub header: bool,
    pub action: Option<Action>,
    pub page: Option<Page>,
    /// The wired port an Ethernet card opens.
    pub port: Option<String>,
}

/// The Wi-Fi door, with what the daemon could tell us without asking the
/// radio: whether the interface is up. Everything else — the network, the
/// signal, the address — is behind the door, because reading it costs a call
/// to `iw` and this row is composed every time the settings screen repaints.
fn wifi_row(diagnostics: Option<&Value>) -> Row {
    let state = text(diagnostics, "/wireless/wifi/state");
    let (value, tone) = match state.as_deref() {
        Some("yok") => ("Bu kartta yok".to_string(), "warn"),
        Some("kapalı") => ("Kapalı".to_string(), "warn"),
        Some("bağlı") => (
            text(diagnostics, "/wireless/wifi/ssid").unwrap_or_else(|| "Bağlı".into()),
            "good",
        ),
        Some(_) => ("Bağlı değil".to_string(), ""),
        None => (String::new(), ""),
    };
    Row {
        value,
        tone: tone.into(),
        ..Row::act("Wi-Fi", "Ağları ara ve bağlan", "wifi", Action::OpenWifi)
    }
}

fn bluetooth_row(diagnostics: Option<&Value>) -> Row {
    let state = text(diagnostics, "/wireless/bluetooth/state");
    let (value, tone) = match state.as_deref() {
        Some("yok") => ("Bu kartta yok".to_string(), "warn"),
        Some("kapalı") => ("Kapalı".to_string(), "warn"),
        Some("açık") => {
            let paired = text(diagnostics, "/wireless/bluetooth/paired")
                .and_then(|count| count.parse::<u32>().ok())
                .unwrap_or(0);
            match paired {
                0 => ("Açık".to_string(), "good"),
                n => (format!("Açık · {n} aygıt eşli"), "good"),
            }
        }
        Some(_) | None => (String::new(), ""),
    };
    Row {
        value,
        tone: tone.into(),
        ..Row::act("Bluetooth", "Aygıt ara ve eşleştir", "bluetooth", Action::OpenBluetooth)
    }
}

impl Row {
    fn reading(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            hint: String::new(),
            tone: String::new(),
            icon: "",
            header: false,
            action: None,
            page: None,
            port: None,
        }
    }

    fn toned(label: &str, value: impl Into<String>, tone: &str) -> Self {
        Self {
            tone: tone.into(),
            ..Row::reading(label, value)
        }
    }

    fn header(label: &str) -> Self {
        Self {
            header: true,
            ..Row::reading(label, "")
        }
    }

    fn act(label: &str, hint: &str, icon: &'static str, action: Action) -> Self {
        Self {
            hint: hint.into(),
            icon,
            action: Some(action),
            ..Row::reading(label, "")
        }
    }

    fn link(page: Page, hint: &str, value: impl Into<String>, tone: &str) -> Self {
        Self {
            hint: hint.into(),
            icon: page.icon(),
            page: Some(page),
            tone: tone.into(),
            ..Row::reading(page.title(), value)
        }
    }

    pub fn selectable(&self) -> bool {
        self.action.is_some() || self.page.is_some()
    }

    pub fn kind(&self) -> Kind {
        if self.header {
            Kind::Header
        } else if self.page.is_some() || self.action.is_some_and(Action::leads_somewhere) {
            Kind::Link
        } else if self.action.is_some() {
            Kind::Action
        } else {
            Kind::Reading
        }
    }
}

pub struct Group {
    pub title: String,
    pub blurb: &'static str,
    pub icon: &'static str,
    pub rows: Vec<Row>,
}

/// Where the remote is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The category column.
    Sections,
    /// The chosen category's cards.
    Rows,
    /// A page a card opened.
    Page,
}

const PAGES: usize = 4;

/// A short list opened from a card, as the screen draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceView {
    pub title: String,
    pub note: String,
    pub focus: usize,
    /// (title, line under it, chosen now)
    pub items: Vec<(String, String, bool)>,
}

/// What each of the lights' modes does, in the list that chooses one.
fn led_line(mode: LedMode) -> &'static str {
    match mode {
        LedMode::Off => "Yeşil ve mavi led söner",
        LedMode::On => "Sürekli yanar",
        LedMode::Heartbeat => "Kalp atışı gibi yanıp söner — kartın kendi ayarı",
    }
}

pub struct Settings {
    pub groups: Vec<Group>,
    pub section: usize,
    pub pane: Pane,
    positions: Vec<usize>,
    /// The page open on the right-hand side while `pane` is `Page`.
    page: Option<Page>,
    /// The rows of the pages that are lists, composed with everything else.
    page_rows: [Vec<Row>; PAGES],
    page_positions: [usize; PAGES],
    /// The mode the lights are in, when the board has lights anybody can set.
    leds: Option<LedMode>,
    /// The lights' list, open, with the remote on this option.
    choice: Option<usize>,
    /// The fan curve editor, which is the whole of the "Soğutma" page: a
    /// graph and a list of points rather than rows of readings.
    pub cooling: super::cooling::Cooling,
    /// The display editor, which is the whole of the "Ekran" page.
    pub output: super::output::Output,
    /// One wired port's address editor: the "Ethernet" pages.
    pub ethernet: super::ethernet::Ethernet,
}

impl Settings {
    pub fn new() -> Self {
        let mut settings = Self {
            groups: Vec::new(),
            section: 0,
            pane: Pane::Sections,
            positions: Vec::new(),
            page: None,
            page_rows: Default::default(),
            page_positions: [0; PAGES],
            leds: None,
            choice: None,
            cooling: super::cooling::Cooling::new(),
            output: super::output::Output::new(),
            ethernet: super::ethernet::Ethernet::new(),
        };
        settings.compose(None, None, None);
        settings
    }

    /// The page on screen, if one is.
    pub fn page(&self) -> Option<Page> {
        if self.pane == Pane::Page { self.page } else { None }
    }

    /// Whether the rows on the right are a list page's rather than the
    /// category's.
    fn in_list_page(&self) -> bool {
        matches!(self.page(), Some(Page::Playback | Page::Account))
    }

    pub fn row_index(&self) -> usize {
        match self.page() {
            Some(page) if self.in_list_page() => self.page_positions[page.index()],
            _ => self.positions.get(self.section).copied().unwrap_or(0),
        }
    }

    /// The rows on the right-hand side: a list page's, or the category's.
    pub fn rows(&self) -> &[Row] {
        match self.page() {
            Some(page) if self.in_list_page() => &self.page_rows[page.index()],
            _ => self
                .groups
                .get(self.section)
                .map(|g| g.rows.as_slice())
                .unwrap_or(&[]),
        }
    }

    pub fn focused(&self) -> Option<&Row> {
        self.rows().get(self.row_index())
    }

    /// The page's name in the column: its own, or the port's.
    pub fn page_label(&self) -> String {
        match self.page() {
            Some(Page::Ethernet) => self.ethernet.title().to_string(),
            Some(page) => page.title().to_string(),
            None => String::new(),
        }
    }

    /// What the right-hand side is headed with.
    pub fn heading(&self) -> (String, String) {
        match self.page() {
            Some(Page::Output) if self.output.advanced() => {
                ("Gelişmiş ekran ayarları".into(), self.output.sink_line())
            }
            Some(Page::Ethernet) => (
                self.ethernet.title().to_string(),
                format!("Adres ayarı · {}", self.ethernet.port()),
            ),
            Some(page) => (page.title().into(), page.blurb().into()),
            None => self
                .groups
                .get(self.section)
                .map(|g| (g.title.clone(), g.blurb.to_string()))
                .unwrap_or_default(),
        }
    }

    fn set_row_index(&mut self, at: usize) {
        match self.page() {
            Some(page) if self.in_list_page() => self.page_positions[page.index()] = at,
            _ => {
                if let Some(slot) = self.positions.get_mut(self.section) {
                    *slot = at;
                }
            }
        }
    }

    /// Whether a list opened from a card holds the remote.
    pub fn choosing(&self) -> bool {
        self.choice.is_some()
    }

    /// Opens the lights' list on the mode they are in now. Nothing changes
    /// until one is chosen.
    pub fn open_choice(&mut self) -> bool {
        let Some(mode) = self.leds else { return false };
        self.choice = Some(LedMode::ALL.iter().position(|m| *m == mode).unwrap_or(0));
        true
    }

    /// Ok in the list: the mode under the focus, to be set. The list closes.
    pub fn choose(&mut self) -> Option<Action> {
        let at = self.choice.take()?;
        let mode = LedMode::ALL.get(at).copied()?;
        (self.leds != Some(mode)).then_some(Action::SetLeds(mode))
    }

    pub fn choice_view(&self) -> Option<ChoiceView> {
        let focus = self.choice?;
        Some(ChoiceView {
            title: "Yeşil ve mavi led".into(),
            note: "Kırmızı led donanımdan yanar; bu seçim onu etkilemez.".into(),
            focus,
            items: LedMode::ALL
                .iter()
                .map(|mode| (mode.label().to_string(), led_line(*mode).to_string(), self.leds == Some(*mode)))
                .collect(),
        })
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        if let Some(at) = self.choice {
            if dy == 0 {
                return false;
            }
            let next = (at as i32 + dy).clamp(0, LedMode::ALL.len() as i32 - 1) as usize;
            self.choice = Some(next);
            return next != at;
        }
        match self.pane {
            Pane::Page => match self.page {
                // The editors have their own lists and buttons; this only
                // hands the remote over and takes it back.
                Some(Page::Output) => match self.output.step(dx, dy) {
                    super::output::Nav::Moved => true,
                    super::output::Nav::Unchanged => false,
                    super::output::Nav::Leave => {
                        self.close_page();
                        true
                    }
                },
                Some(Page::Ethernet) => match self.ethernet.step(dx, dy) {
                    super::ethernet::Nav::Moved => true,
                    super::ethernet::Nav::Unchanged => false,
                    super::ethernet::Nav::Leave => {
                        self.close_page();
                        true
                    }
                },
                Some(Page::Cooling) => match self.cooling.step(dx, dy) {
                    super::cooling::Nav::Moved => true,
                    super::cooling::Nav::Unchanged => false,
                    super::cooling::Nav::Leave => {
                        self.close_page();
                        true
                    }
                },
                Some(_) => {
                    if dx < 0 {
                        self.close_page();
                        return true;
                    }
                    self.move_rows(dy)
                }
                None => false,
            },
            Pane::Rows => {
                if dx < 0 {
                    self.pane = Pane::Sections;
                    return true;
                }
                self.move_rows(dy)
            }
            Pane::Sections => {
                if dx > 0 {
                    return self.enter_rows();
                }
                if dx < 0 || dy == 0 {
                    return false;
                }
                let next =
                    (self.section as i32 + dy).clamp(0, self.groups.len() as i32 - 1) as usize;
                if next == self.section {
                    return false;
                }
                self.section = next;
                true
            }
        }
    }

    /// From the column onto the category's first card, or where the remote
    /// was last time. A category with nothing to press keeps the remote on the
    /// column: focus sitting on a line that does nothing is focus a viewer
    /// cannot trust.
    fn enter_rows(&mut self) -> bool {
        let rows = self.groups.get(self.section).map_or(&[][..], |g| g.rows.as_slice());
        if !rows.iter().any(Row::selectable) {
            return false;
        }
        self.pane = Pane::Rows;
        self.settle(1);
        true
    }

    fn move_rows(&mut self, dy: i32) -> bool {
        if dy == 0 {
            return false;
        }
        let before = self.row_index();
        if let Some(at) = next_selectable(self.rows(), before as i32 + dy, dy) {
            self.set_row_index(at);
        }
        self.row_index() != before
    }

    /// Lands on the nearest row that can actually be chosen.
    ///
    /// A settings screen is mostly readings, and rows that are only
    /// information are stepped over.
    fn settle(&mut self, direction: i32) {
        let start = self.row_index() as i32;
        let found = next_selectable(self.rows(), start, direction)
            .or_else(|| next_selectable(self.rows(), start, -direction));
        if let Some(at) = found {
            self.set_row_index(at);
        }
    }

    /// Opens the page a card leads to. An editor with nothing to edit does
    /// not open: its card is a reading that says why in the first place.
    pub fn open(&mut self, page: Page) -> bool {
        match page {
            Page::Output => {
                if !self.output.available() {
                    return false;
                }
                self.output.enter();
            }
            Page::Cooling => {
                if !self.cooling.available() {
                    return false;
                }
                self.cooling.enter();
            }
            Page::Ethernet => {
                let Some(row) = self.focused().filter(|row| row.page == Some(Page::Ethernet)) else {
                    return false;
                };
                let (port, title) = (row.port.clone().unwrap_or_default(), row.label.clone());
                if !self.ethernet.open(&port, &title) {
                    return false;
                }
            }
            Page::Playback | Page::Account => {}
        }
        self.page = Some(page);
        self.pane = Pane::Page;
        if self.in_list_page() {
            self.settle(1);
        }
        true
    }

    fn close_page(&mut self) {
        self.pane = Pane::Rows;
        self.page = None;
    }

    /// Back, once the editor on screen has had its say: one level up. False
    /// on the column itself, where Back leaves the screen.
    pub fn back(&mut self) -> bool {
        if self.choice.take().is_some() {
            return true;
        }
        match self.pane {
            Pane::Page => {
                self.close_page();
                true
            }
            Pane::Rows => {
                self.pane = Pane::Sections;
                true
            }
            Pane::Sections => false,
        }
    }

    /// Rebuilt from whatever the control plane last answered. Keeps the pane,
    /// the page and every position, because this runs on a timer.
    pub fn compose(
        &mut self,
        status: Option<&Value>,
        diagnostics: Option<&Value>,
        display: Option<&DisplayStatus>,
    ) {
        self.cooling.load(fan_status(status));
        self.output.load(output_status(status));
        self.ethernet.load(ethernet_status(status));
        self.leds = led_mode(status);
        // Lights that stopped answering have nothing left to choose.
        if self.leds.is_none() {
            self.choice = None;
        }
        let groups = compose(status, diagnostics, display);
        let changed = self.groups.len() != groups.len();
        self.groups = groups;
        if changed || self.positions.len() != self.groups.len() {
            self.positions = vec![0; self.groups.len()];
        }
        self.section = self.section.min(self.groups.len().saturating_sub(1));
        for (index, group) in self.groups.iter().enumerate() {
            let slot = self.positions[index].min(group.rows.len().saturating_sub(1));
            self.positions[index] = slot;
        }
        for page in [Page::Playback, Page::Account] {
            let rows = page_rows(page, status, display);
            let slot = self.page_positions[page.index()].min(rows.len().saturating_sub(1));
            self.page_rows[page.index()] = rows;
            self.page_positions[page.index()] = slot;
        }
        // An editor whose display or fan went away while it was open has
        // nothing left to edit; its category says why.
        match self.page() {
            Some(Page::Output) if !self.output.available() => self.close_page(),
            Some(Page::Cooling) if !self.cooling.available() => self.close_page(),
            Some(Page::Ethernet) if !self.ethernet.available() => self.close_page(),
            _ => {}
        }
        if self.pane == Pane::Rows && !self.rows().iter().any(Row::selectable) {
            self.pane = Pane::Sections;
        }
        if self.pane != Pane::Sections {
            self.settle(1);
        }
    }
}

/// The first row from `start` that can be chosen, walking in `direction`.
fn next_selectable(rows: &[Row], start: i32, direction: i32) -> Option<usize> {
    let direction = if direction == 0 { 1 } else { direction.signum() };
    let mut at = start;
    while at >= 0 && (at as usize) < rows.len() {
        if rows[at as usize].selectable() {
            return Some(at as usize);
        }
        at += direction;
    }
    None
}

fn fan_status(status: Option<&Value>) -> Option<FanStatus> {
    serde_json::from_value(status?.get("fan")?.clone()).ok()
}

fn output_status(status: Option<&Value>) -> Option<mediabox_core::OutputStatus> {
    serde_json::from_value(status?.get("output")?.clone()).ok()
}

fn ethernet_status(status: Option<&Value>) -> Option<mediabox_core::EthernetStatus> {
    serde_json::from_value(status?.get("ethernet")?.clone()).ok()
}

impl Settings {
    /// Whether keys belong to the fan editor: its page, open.
    pub fn typing_cooling(&self) -> bool {
        self.is_cooling()
    }

    /// Whether the page on screen is the fan editor, which draws and moves by
    /// its own rules.
    pub fn is_cooling(&self) -> bool {
        self.page() == Some(Page::Cooling)
    }

    /// Whether the page on screen is the display editor.
    pub fn is_output(&self) -> bool {
        self.page() == Some(Page::Output)
    }

    /// Whether the remote belongs to the display editor.
    pub fn in_output(&self) -> bool {
        self.is_output()
    }

    /// Whether the page on screen is a wired port's address.
    pub fn is_ethernet(&self) -> bool {
        self.page() == Some(Page::Ethernet)
    }

    /// Whether keys typed on a keyboard belong to the port's number pad.
    pub fn typing_ethernet(&self) -> bool {
        self.is_ethernet() && self.ethernet.typing()
    }
}

/// A card per wired port, to set its address: as many as the daemon found,
/// named as the "Ağ" rows name them.
fn ethernet_cards(status: Option<&Value>) -> Vec<Row> {
    let Some(ethernet) = ethernet_status(status) else {
        return Vec::new();
    };
    let mut ports = ethernet.ports;
    ports.sort_by(|a, b| a.name.cmp(&b.name));
    let many = ports.len() > 1;
    ports
        .iter()
        .enumerate()
        .map(|(index, port)| {
            let label = if many { format!("Ethernet {}", index + 1) } else { "Ethernet".into() };
            let on_trial = ethernet.trial.as_ref().is_some_and(|trial| trial.interface == port.name);
            Row {
                label,
                value: if on_trial { "Onay bekliyor".into() } else { port.config.label() },
                tone: if on_trial { "warn".into() } else { String::new() },
                hint: format!("Adres ayarı · {}", port.name),
                icon: "network",
                page: Some(Page::Ethernet),
                port: Some(port.name.clone()),
                ..Row::reading("", "")
            }
        })
        .collect()
}

/// One network card, as the daemon found it on this board.
struct Link {
    name: String,
    address: Option<String>,
    up: bool,
    wireless: bool,
}

fn links(diagnostics: Option<&Value>) -> Option<Vec<Link>> {
    let list = diagnostics?.pointer("/network/interfaces")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|entry| {
                Some(Link {
                    name: entry.get("name")?.as_str()?.to_string(),
                    address: entry
                        .get("address")
                        .and_then(Value::as_str)
                        .filter(|address| !address.is_empty())
                        .map(str::to_string),
                    up: entry.get("state").and_then(Value::as_str) == Some("up"),
                    wireless: entry.get("wireless").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect(),
    )
}

/// A radio's strength in words, with the figure beside them: four steps, the
/// same ones the Wi-Fi screen and the home screen's bars use.
fn signal(dbm: i64) -> (String, &'static str) {
    let (word, tone) = match crate::vitals::signal_bars(dbm) {
        4 => ("Çok iyi", "good"),
        3 => ("İyi", "good"),
        2 => ("Orta", "warn"),
        _ => ("Zayıf", "bad"),
    };
    (format!("{word} · {dbm} dBm"), tone)
}

/// The "Ağ" heading and what is under it: the Wi-Fi network and its signal,
/// then every network card on its own line with its own address.
///
/// The cards are whatever the daemon found on this board — one wired port on
/// an Ultra, two on a Plus — so nothing here counts them in advance. With more
/// than one wired port each is numbered and named by its interface, since the
/// two sockets are otherwise indistinguishable on the screen.
fn network(diagnostics: Option<&Value>) -> Vec<Row> {
    let dash = || "—".to_string();
    let mut rows = vec![Row::header("Ağ")];
    let Some(links) = links(diagnostics) else {
        rows.push(Row::reading("Durum", "Okunuyor…"));
        return rows;
    };
    let default = text(diagnostics, "/network/default/interface");

    let mut wired: Vec<&Link> = links.iter().filter(|link| !link.wireless).collect();
    wired.sort_by(|a, b| a.name.cmp(&b.name));
    let wired_label = |index: usize| {
        if wired.len() > 1 { format!("Ethernet {}", index + 1) } else { "Ethernet".to_string() }
    };
    let label_of = |name: &str| -> String {
        if let Some(index) = wired.iter().position(|link| link.name == name) {
            wired_label(index)
        } else if links.iter().any(|link| link.wireless && link.name == name) {
            "Wi-Fi".into()
        } else {
            name.to_string()
        }
    };

    // The radio.
    let radio = links.iter().find(|link| link.wireless);
    let state = text(diagnostics, "/wireless/wifi/state");
    if radio.is_some() || matches!(state.as_deref(), Some(s) if s != "yok") {
        if state.as_deref() == Some("bağlı") {
            rows.push(Row::toned(
                "Wi-Fi ağı",
                text(diagnostics, "/wireless/wifi/ssid").unwrap_or_else(dash),
                "good",
            ));
            if let Some(dbm) = diagnostics
                .and_then(|value| value.pointer("/wireless/wifi/signal"))
                .and_then(Value::as_i64)
            {
                let (value, tone) = signal(dbm);
                rows.push(Row::toned("Wi-Fi sinyali", value, tone));
            }
            let address = text(diagnostics, "/wireless/wifi/address")
                .or_else(|| radio.and_then(|link| link.address.clone()));
            rows.push(Row::reading(
                "Wi-Fi adresi",
                match (address, radio) {
                    (Some(address), Some(link)) => format!("{address} · {}", link.name),
                    (Some(address), None) => address,
                    (None, _) => "Adres bekleniyor".into(),
                },
            ));
        } else {
            rows.push(Row::reading(
                "Wi-Fi ağı",
                if state.as_deref() == Some("kapalı") { "Kapalı" } else { "Bağlı değil" },
            ));
        }
    }

    // The wires, each on its own line.
    for (index, link) in wired.iter().enumerate() {
        let (value, tone) = match (&link.address, link.up) {
            (Some(address), _) => (format!("{address} · {}", link.name), "good"),
            (None, true) => (format!("Bağlı, adres bekleniyor · {}", link.name), "warn"),
            (None, false) => (format!("Kablo takılı değil · {}", link.name), ""),
        };
        rows.push(Row::toned(&wired_label(index), value, tone));
    }
    if wired.is_empty() {
        rows.push(Row::reading("Ethernet", "Bu kartta yok"));
    }

    // Which of them carries the traffic, and through where.
    rows.push(Row::reading(
        "Varsayılan bağlantı",
        default.as_deref().map(label_of).unwrap_or_else(|| "Yok".into()),
    ));
    rows.push(Row::reading(
        "Ağ geçidi",
        text(diagnostics, "/network/default/gateway").unwrap_or_else(dash),
    ));
    rows
}

/// The display editor's page.
pub const OUTPUT: &str = "Ekran";

/// The card that opens the display editor, or — when there is no display to
/// set — a reading that says why.
fn output(status: Option<&Value>) -> Row {
    match output_status(status) {
        Some(output) if output.offer.is_some() => {
            let now = output
                .offer
                .as_ref()
                .zip(output.wire_mode())
                .and_then(|(offer, wire)| offer.mode(&wire))
                .map(super::output::label)
                .unwrap_or_default();
            Row::link(Page::Output, Page::Output.blurb(), now, "")
        }
        Some(output) => Row::toned(
            OUTPUT,
            output.error.unwrap_or_else(|| "Ekran okunamadı".into()),
            "warn",
        ),
        None => Row::reading(OUTPUT, "Okunuyor…"),
    }
}

/// The fan editor's page.
pub const COOLING: &str = "Soğutma";

/// The card that opens the fan editor, or a reading that says why there is no
/// fan to edit.
fn cooling(status: Option<&Value>) -> Row {
    match fan_status(status) {
        Some(fan) if fan.available => {
            let now = match (fan.temperature_c, fan.pwm_percent) {
                (Some(celsius), Some(duty)) => format!("{celsius:.0} °C · fan %{duty:.0}"),
                (Some(celsius), None) => format!("{celsius:.0} °C"),
                _ => String::new(),
            };
            Row::link(Page::Cooling, "Fan eğrisi", now, "")
        }
        Some(fan) => Row::toned(
            "Fan",
            fan.error.unwrap_or_else(|| "Fan okunamadı".into()),
            "warn",
        ),
        None => Row::reading("Fan", "Fan okunuyor…"),
    }
}

fn text(root: Option<&Value>, pointer: &str) -> Option<String> {
    let value = root?.pointer(pointer)?;
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "evet".into() } else { "hayır".into() }),
        _ => None,
    }
}

fn flag(root: Option<&Value>, pointer: &str) -> Option<bool> {
    root?.pointer(pointer)?.as_bool()
}

fn yes_no(value: Option<bool>) -> (String, &'static str) {
    match value {
        Some(true) => ("Bağlı".into(), "good"),
        Some(false) => ("Yok".into(), "bad"),
        None => ("—".into(), ""),
    }
}

fn duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3600;
    let minutes = (seconds % 3600) / 60;
    if days > 0 {
        format!("{days} gün {hours} saat")
    } else if hours > 0 {
        format!("{hours} saat {minutes} dakika")
    } else {
        format!("{minutes} dakika")
    }
}

/// The mode the lights are in, when the board has lights anybody can set.
fn led_mode(status: Option<&Value>) -> Option<LedMode> {
    if flag(status, "/leds/available") != Some(true) {
        return None;
    }
    Some(match text(status, "/leds/mode").as_deref() {
        Some("off") => LedMode::Off,
        Some("on") => LedMode::On,
        _ => LedMode::Heartbeat,
    })
}

/// The board's indicator lights: the card, and the readings that go under
/// the "Işıklar" heading.
///
/// Three lights, and only two of them are anybody's to change. The red one is
/// wired to the supply and appears nowhere in the device tree, so it is a
/// reading here that says as much — a person who has just turned the other two
/// off and can still see a light needs to be told why, on the screen, rather
/// than left to wonder whether the setting worked.
///
/// The card says where the lights are now and opens the list of the three
/// modes, as the display's cards do. It used to step round the ring on each
/// press, and two presses of Ok — one meant, one not — left the lights in a
/// mode nobody had picked.
fn leds(status: Option<&Value>) -> (Option<Row>, Vec<Row>) {
    let red = Row::reading("Kırmızı led", "Donanımdan yanar — kapatılamaz");

    if flag(status, "/leds/available") != Some(true) {
        // Either the daemon has not answered yet, or this is not a board whose
        // lights are on gpio-leds. Either way there is nothing to press.
        let reason =
            text(status, "/leds/error").unwrap_or_else(|| "Denetlenebilir led bulunamadı".into());
        return (None, vec![Row::reading("Yeşil ve mavi led", reason), red]);
    }

    let mode = led_mode(status).unwrap_or_default();

    (
        Some(Row {
            value: mode.label().into(),
            tone: if mode == LedMode::Off {
                "good".into()
            } else {
                String::new()
            },
            ..Row::act(
                "Yeşil ve mavi led",
                "Kapalı, sürekli açık ya da nabız",
                "bulb",
                Action::ChooseLeds,
            )
        }),
        vec![
            red,
            Row::reading("Kalıcılık", "Seçim yeniden başlatmadan sonra korunur"),
        ],
    )
}

fn signed_in(status: Option<&Value>) -> bool {
    flag(status, "/media/provider/authenticated").unwrap_or(false)
}

/// The Stremio account.
///
/// The same three readings the web interface shows, from the same place in the
/// status, and one card to press. Connected, it offers the way out; not
/// connected, it offers the way in — there is never both, because a screen that
/// shows a sign-out button to somebody who is not signed in is a screen that
/// has not read its own state.
fn account(status: Option<&Value>) -> Vec<Row> {
    let signed_in = signed_in(status);
    let note = Row::reading(
        "Eklentiler",
        if signed_in {
            "Hesabınızın eklentileri kullanılıyor"
        } else {
            "Varsayılan koleksiyon — çoğu başlıkta akış gelmez"
        },
    );

    if !signed_in {
        return vec![
            Row::act(
                "Giriş yap",
                "E-posta ve parolanızı kumandayla girin",
                "signin",
                Action::OpenAccount,
            ),
            Row::header("Durum"),
            Row::toned("Durum", "Bağlı değil", "warn"),
            note,
        ];
    }

    vec![
        Row::act("Çıkış yap", "Hesabın bağlantısını keser", "signout", Action::SignOut),
        Row::header("Durum"),
        Row::toned("Durum", "Bağlı", "good"),
        Row::reading(
            "Hesap",
            text(status, "/media/provider/email").unwrap_or_else(|| "—".into()),
        ),
        Row::reading(
            "Eklenti sayısı",
            status
                .and_then(|value| value.pointer("/media/provider/addonCount"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .to_string(),
        ),
        note,
    ]
}

fn kodi_running(status: Option<&Value>) -> bool {
    flag(status, "/kodi/running").unwrap_or(false)
}

/// The player: what it is doing, and the one thing to do about it.
fn playback(status: Option<&Value>, display: Option<&DisplayStatus>) -> Vec<Row> {
    let owner = display
        .and_then(|d| d.owner.clone())
        .map(|id| match id.as_str() {
            "mediabox" => "MediaBox".to_string(),
            "kodi" => "Oynatıcı".to_string(),
            "browser" => "Tarayıcı".to_string(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "Boşta".into());
    let running = kodi_running(status);
    vec![
        Row::act(
            "Oynatıcıyı yeniden başlat",
            "Kodi'yi kapatıp açar",
            "restart",
            Action::RestartPlayer,
        ),
        Row::header("Durum"),
        Row::toned(
            "Oynatıcı",
            if running { "Çalışıyor" } else { "Kapalı" },
            if running { "good" } else { "" },
        ),
        Row::reading("Ekranı tutan", owner),
        Row::reading("Görüntü", "Kopyalanır — yeniden kodlanmaz"),
        Row::reading("Ses", "Gerekirse AC-3'e çevrilir"),
    ]
}

fn page_rows(page: Page, status: Option<&Value>, display: Option<&DisplayStatus>) -> Vec<Row> {
    match page {
        Page::Playback => playback(status, display),
        Page::Account => account(status),
        Page::Output | Page::Cooling | Page::Ethernet => Vec::new(),
    }
}

fn compose(
    status: Option<&Value>,
    diagnostics: Option<&Value>,
    _display: Option<&DisplayStatus>,
) -> Vec<Group> {
    let dash = || "—".to_string();

    let (cec_state, cec_tone) = yes_no(flag(status, "/cec/available"));
    let running = kodi_running(status);
    let signed_in = signed_in(status);
    let (led_card, led_readings) = leds(status);

    let mut device = Vec::new();
    device.extend(led_card);
    device.push(cooling(status));
    device.push(Row::header("Led"));
    device.extend(led_readings);

    vec![
        Group {
            title: "Medya".into(),
            blurb: "Oynatma ve hesap ile ilgili ayarlar",
            icon: "play",
            rows: vec![
                Row::link(
                    Page::Playback,
                    "Oynatıcının durumu ve yeniden başlatılması",
                    if running { "Çalışıyor" } else { "Kapalı" },
                    if running { "good" } else { "" },
                ),
                Row::link(
                    Page::Account,
                    "Stremio hesabı ve eklentileri",
                    if signed_in { "Bağlı" } else { "Bağlı değil" },
                    if signed_in { "good" } else { "warn" },
                ),
            ],
        },
        Group {
            title: "Görüntü ve Ses".into(),
            blurb: "Ekran çözünürlüğü, renk ve ses çıkışı",
            icon: "monitor",
            rows: vec![
                output(status),
                Row::header("Ses"),
                Row::reading(
                    "Çıkış",
                    text(diagnostics, "/audio/default").unwrap_or_else(|| "HDMI".into()),
                ),
                Row::reading("Geçirgen kodekler", "AC-3 · E-AC-3 · DTS"),
                Row::reading("Nesne tabanlı ses", "AC-3'e çevrilir"),
            ],
        },
        Group {
            title: "Bağlantılar".into(),
            blurb: "Ağ ve kablosuz bağlantılar",
            icon: "wifi",
            rows: vec![
                wifi_row(diagnostics),
                bluetooth_row(diagnostics),
            ]
            .into_iter()
            .chain(ethernet_cards(status))
            .chain(network(diagnostics))
            .collect(),
        },
        Group {
            title: "TV ve Kumanda".into(),
            blurb: "Televizyonu HDMI-CEC ile yönetin",
            icon: "remote",
            rows: vec![
                Row::act(
                    "Televizyonu uyandır",
                    "CEC ile açılış isteği",
                    "tv",
                    Action::WakeTelevision,
                ),
                Row::act(
                    "Televizyonu beklemeye al",
                    "CEC ile standby",
                    "moon",
                    Action::StandbyTelevision,
                ),
                Row::header("HDMI-CEC"),
                Row::toned("CEC", cec_state, cec_tone),
                Row::reading(
                    "Bağdaştırıcı",
                    text(status, "/cec/adapter").unwrap_or_else(dash),
                ),
                Row::reading(
                    "Fiziksel adres",
                    text(status, "/cec/physical_address").unwrap_or_else(dash),
                ),
                Row::header("Telefon kumandası"),
                Row::reading("Uzaktan kumanda", "http://<cihaz>:8788"),
            ],
        },
        Group {
            title: "Cihaz".into(),
            blurb: "Led ve soğutma",
            icon: "chip",
            rows: device,
        },
        Group {
            title: "Sistem".into(),
            blurb: "Sürüm, yeniden başlatma ve kapatma",
            icon: "info",
            rows: vec![
                Row::act("Yeniden başlat", "Cihazı kapatıp açar", "restart", Action::Restart),
                Row::act("Kapat", "Cihazı kapatır", "power", Action::Shutdown),
                Row::header("Sürüm"),
                Row::reading("Sürüm", text(status, "/version").unwrap_or_else(dash)),
                Row::reading("Makine", text(status, "/hostname").unwrap_or_else(dash)),
                Row::reading(
                    "Açık kalma",
                    status
                        .and_then(|s| s.pointer("/uptime_seconds"))
                        .and_then(Value::as_u64)
                        .map(duration)
                        .unwrap_or_else(dash),
                ),
            ],
        },
        Group {
            title: "Tanılama".into(),
            blurb: "İşlemci, bellek, sıcaklık, ekran ve servisler",
            icon: "bars",
            rows: vec![Row::act(
                "Tanılamayı aç",
                "Salt okunur teknik ayrıntılar",
                "bars",
                Action::OpenDiagnostics,
            )],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(settings: &mut Settings, title: &str) {
        settings.section = settings
            .groups
            .iter()
            .position(|g| g.title == title)
            .unwrap_or_else(|| panic!("no {title} section"));
    }

    /// Every card on every category and list page, with what it does.
    fn reachable(settings: &Settings) -> (Vec<Action>, Vec<Page>) {
        let mut actions = Vec::new();
        let mut pages = Vec::new();
        let rows = settings
            .groups
            .iter()
            .flat_map(|g| g.rows.iter())
            .chain(settings.page_rows.iter().flatten());
        for row in rows {
            actions.extend(row.action);
            pages.extend(row.page);
        }
        (actions, pages)
    }

    #[test]
    fn focus_lands_on_something_that_can_be_pressed() {
        let mut settings = Settings::new();
        assert!(settings.step(1, 0));
        assert!(
            settings
                .focused()
                .map(|row| row.selectable())
                .unwrap_or(false)
        );
    }

    #[test]
    fn every_section_has_at_least_one_row() {
        let settings = Settings::new();
        assert!(!settings.groups.is_empty());
        for group in &settings.groups {
            assert!(!group.rows.is_empty(), "{} is empty", group.title);
        }
    }

    /// The categories of the design, in its order.
    #[test]
    fn the_categories_are_the_design_s() {
        let settings = Settings::new();
        let titles: Vec<&str> = settings.groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Medya",
                "Görüntü ve Ses",
                "Bağlantılar",
                "TV ve Kumanda",
                "Cihaz",
                "Sistem",
                "Tanılama"
            ]
        );
        for group in &settings.groups {
            assert!(!group.icon.is_empty() && !group.blurb.is_empty(), "{}", group.title);
        }
    }

    /// The move to seven categories must not lose anything the old sections
    /// could do: every action and every editor is still one card away.
    #[test]
    fn nothing_the_old_sections_could_do_is_lost() {
        let status = serde_json::json!({
            "leds": {"available": true, "mode": "off", "leds": []},
            "fan": fan_answer()["fan"],
            "output": output_answer(),
        });
        let mut settings = Settings::new();
        settings.compose(Some(&status), Some(&plus_like(true)), None);
        let (actions, pages) = reachable(&settings);
        for wanted in [
            Action::OpenDiagnostics,
            Action::OpenWifi,
            Action::OpenBluetooth,
            Action::OpenAccount,
            Action::WakeTelevision,
            Action::StandbyTelevision,
            Action::RestartPlayer,
            Action::Restart,
            Action::Shutdown,
            Action::ChooseLeds,
        ] {
            assert!(actions.contains(&wanted), "{wanted:?} is not reachable");
        }
        for wanted in [Page::Playback, Page::Account, Page::Output, Page::Cooling] {
            assert!(pages.contains(&wanted), "{wanted:?} is not reachable");
        }
        // And the readings the old sections showed are still somewhere.
        let labels: Vec<&str> = settings
            .groups
            .iter()
            .flat_map(|g| g.rows.iter())
            .chain(settings.page_rows.iter().flatten())
            .map(|row| row.label.as_str())
            .collect();
        for wanted in [
            "Oynatıcı", "Ekranı tutan", "Ağ geçidi", "Uzaktan kumanda", "CEC",
            "Bağdaştırıcı", "Fiziksel adres", "Çıkış", "Geçirgen kodekler", "Kırmızı led",
            "Sürüm", "Makine", "Açık kalma",
        ] {
            assert!(labels.contains(&wanted), "{wanted} is gone");
        }
    }

    #[test]
    fn crossing_panes_is_left_and_right_only() {
        let mut settings = Settings::new();
        assert_eq!(settings.pane, Pane::Sections);
        assert!(!settings.step(-1, 0));
        assert!(settings.step(1, 0));
        assert_eq!(settings.pane, Pane::Rows);
        assert!(settings.step(-1, 0));
        assert_eq!(settings.pane, Pane::Sections);
    }

    /// Up and down on the column change the category; the right-hand side
    /// follows, and nothing is entered.
    #[test]
    fn the_column_walks_the_categories() {
        let mut settings = Settings::new();
        assert!(!settings.step(0, -1), "already at the top");
        assert!(settings.step(0, 1));
        assert_eq!(settings.section, 1);
        assert_eq!(settings.pane, Pane::Sections);
        for _ in 0..20 {
            settings.step(0, 1);
        }
        assert_eq!(settings.section, settings.groups.len() - 1);
    }

    #[test]
    fn readings_and_headings_are_stepped_over() {
        let mut settings = Settings::new();
        section(&mut settings, "Sistem");
        assert!(settings.step(1, 0));
        assert_eq!(
            settings.focused().and_then(|r| r.action),
            Some(Action::Restart)
        );
        assert!(settings.step(0, 1));
        assert_eq!(
            settings.focused().and_then(|r| r.action),
            Some(Action::Shutdown)
        );
        // Below are only a heading and readings: the remote stays.
        assert!(!settings.step(0, 1));
        assert_eq!(
            settings.focused().and_then(|r| r.action),
            Some(Action::Shutdown)
        );
    }

    /// Each category keeps the card the remote was on.
    #[test]
    fn each_category_remembers_its_row() {
        let mut settings = Settings::new();
        section(&mut settings, "Sistem");
        settings.step(1, 0);
        settings.step(0, 1);
        settings.step(-1, 0);
        settings.step(0, -1); // Cihaz
        settings.step(0, 1); // Sistem again
        settings.step(1, 0);
        assert_eq!(
            settings.focused().and_then(|r| r.action),
            Some(Action::Shutdown)
        );
    }

    /// A list page opens on its first card, Left and Back both come back up
    /// to the card that opened it.
    #[test]
    fn a_page_opens_and_back_or_left_comes_back_to_its_card() {
        let mut settings = Settings::new();
        settings.step(1, 0);
        assert_eq!(settings.focused().and_then(|r| r.page), Some(Page::Playback));
        assert!(settings.open(Page::Playback));
        assert_eq!(settings.page(), Some(Page::Playback));
        assert_eq!(settings.heading().0, "Oynatma");
        assert_eq!(
            settings.focused().and_then(|r| r.action),
            Some(Action::RestartPlayer)
        );
        assert!(settings.step(-1, 0));
        assert_eq!(settings.pane, Pane::Rows);
        assert_eq!(settings.focused().and_then(|r| r.page), Some(Page::Playback));
        settings.open(Page::Playback);
        assert!(settings.back());
        assert_eq!(settings.pane, Pane::Rows);
        assert!(settings.back());
        assert_eq!(settings.pane, Pane::Sections);
        assert!(!settings.back(), "the column is where Back leaves the screen");
    }

    /// A poll while a page is open keeps the page and the position.
    #[test]
    fn a_poll_keeps_the_page_open() {
        let mut settings = Settings::new();
        settings.step(1, 0);
        settings.step(0, 1);
        settings.open(Page::Account);
        settings.compose(Some(&provider(true)), None, None);
        assert_eq!(settings.page(), Some(Page::Account));
        assert_eq!(settings.focused().and_then(|r| r.action), Some(Action::SignOut));
    }

    /// The whole point of the settings screen's power rows.
    #[test]
    fn every_destructive_row_asks_first() {
        let settings = Settings::new();
        let (actions, _) = reachable(&settings);
        for action in actions {
            if matches!(
                action,
                Action::Restart | Action::Shutdown | Action::RestartPlayer | Action::SignOut
            ) {
                assert!(action.confirms(), "{:?} does not confirm", action);
                assert!(!action.question().is_empty());
            }
        }
    }

    #[test]
    fn nothing_harmless_drags_a_confirmation_along() {
        assert!(!Action::OpenDiagnostics.confirms());
        assert!(!Action::WakeTelevision.confirms());
        assert!(!Action::StandbyTelevision.confirms());
        assert!(!Action::ChooseLeds.confirms());
        // An indicator light is not somebody's evening.
        for mode in LedMode::ALL {
            assert!(!Action::SetLeds(mode).confirms());
            assert!(Action::SetLeds(mode).question().is_empty());
        }
    }

    /// Cards that go somewhere carry a chevron; cards that do something do
    /// not; readings and headings are neither.
    #[test]
    fn a_row_is_drawn_as_what_it_does() {
        let settings = Settings::new();
        for row in settings.groups.iter().flat_map(|g| g.rows.iter()) {
            match row.kind() {
                Kind::Link => assert!(
                    row.page.is_some() || row.action.is_some_and(Action::leads_somewhere),
                    "{}",
                    row.label
                ),
                Kind::Action => assert!(row.action.is_some() && row.page.is_none()),
                Kind::Header | Kind::Reading => assert!(!row.selectable(), "{}", row.label),
            }
        }
    }

    // ------------------------------------------------------------ the account

    fn provider(authenticated: bool) -> serde_json::Value {
        serde_json::json!({"media": {"provider": {
            "authenticated": authenticated,
            "email": "someone@example.com",
            "addonCount": 7,
        }}})
    }

    fn account_rows(status: Option<&Value>) -> Vec<Row> {
        page_rows(Page::Account, status, None)
    }

    fn value_of<'a>(rows: &'a [Row], label: &str) -> &'a str {
        &rows
            .iter()
            .find(|row| row.label == label && !row.header)
            .unwrap_or_else(|| panic!("no {label} row"))
            .value
    }

    /// The web interface has had this since the beginning; the native shell is
    /// where a person actually sits.
    #[test]
    fn the_settings_screen_has_an_account_page() {
        let settings = Settings::new();
        let (_, pages) = reachable(&settings);
        assert!(pages.contains(&Page::Account));
    }

    #[test]
    fn a_box_with_no_account_offers_the_way_in() {
        let status = provider(false);
        let rows = account_rows(Some(&status));
        assert_eq!(value_of(&rows, "Durum"), "Bağlı değil");
        let actions: Vec<Action> = rows.iter().filter_map(|row| row.action).collect();
        assert_eq!(actions, [Action::OpenAccount]);
        // A sign-out button in front of somebody who is not signed in is a
        // screen that has not read its own state.
        assert!(!actions.contains(&Action::SignOut));
    }

    #[test]
    fn a_connected_box_shows_the_account_and_offers_the_way_out() {
        let status = provider(true);
        let rows = account_rows(Some(&status));
        assert_eq!(value_of(&rows, "Durum"), "Bağlı");
        assert!(rows.iter().any(|row| row.value == "someone@example.com"));
        assert!(rows.iter().any(|row| row.value == "7"));
        let actions: Vec<Action> = rows.iter().filter_map(|row| row.action).collect();
        assert_eq!(actions, [Action::SignOut]);
    }

    /// The card on the category says the same thing the page does.
    #[test]
    fn the_account_card_tells_whether_it_is_connected() {
        for (authenticated, value) in [(true, "Bağlı"), (false, "Bağlı değil")] {
            let status = provider(authenticated);
            let groups = compose(Some(&status), None, None);
            let card = groups[0]
                .rows
                .iter()
                .find(|row| row.page == Some(Page::Account))
                .expect("account card");
            assert_eq!(card.value, value);
        }
    }

    /// An unanswered status is a box that is not signed in, which is also what
    /// a box that has never been asked looks like.
    #[test]
    fn an_unanswered_status_does_not_claim_an_account() {
        let rows = account_rows(None);
        assert_eq!(value_of(&rows, "Durum"), "Bağlı değil");
        assert_eq!(
            rows.iter().filter_map(|row| row.action).collect::<Vec<_>>(),
            [Action::OpenAccount]
        );
    }

    /// Somebody's add-ons stop being the ones this box uses, so it asks first.
    #[test]
    fn signing_out_asks_first_and_signing_in_does_not() {
        assert!(Action::SignOut.confirms());
        assert!(!Action::SignOut.question().is_empty());
        assert!(!Action::OpenAccount.confirms());
        assert!(Action::OpenAccount.question().is_empty());
    }

    /// Nothing about the account may put a secret on the panel. The rows are
    /// built from the status, and the status has no password in it — this
    /// holds the screen to that even if one ever appeared.
    #[test]
    fn no_account_row_can_carry_a_secret() {
        let mut status = provider(true);
        status["media"]["provider"]["password"] = "hunter2".into();
        status["media"]["provider"]["authKey"] = "sekritkey".into();
        let groups = compose(Some(&status), None, None);
        let rows = account_rows(Some(&status))
            .into_iter()
            .chain(groups.into_iter().flat_map(|g| g.rows));
        for row in rows {
            for field in [&row.label, &row.value, &row.hint] {
                assert!(!field.contains("hunter2"), "{field}");
                assert!(!field.contains("sekritkey"), "{field}");
            }
        }
    }

    // ------------------------------------------------------------- the lights

    fn answered(available: bool, mode: &str) -> serde_json::Value {
        serde_json::json!({
            "leds": {
                "available": available,
                "mode": mode,
                "leds": ["blue_led", "green_led"],
                "error": if available { serde_json::Value::Null } else { "test".into() },
            }
        })
    }

    fn device(status: Option<&Value>) -> Vec<Row> {
        compose(status, None, None)
            .into_iter()
            .find(|group| group.title == "Cihaz")
            .expect("device section")
            .rows
    }

    fn lights(available: bool, mode: &str) -> Vec<Row> {
        device(Some(&answered(available, mode)))
    }

    fn lights_card(rows: &[Row]) -> Option<&Row> {
        rows.iter().find(|row| row.action == Some(Action::ChooseLeds))
    }

    /// The card says where the lights are, and a press opens the list rather
    /// than changing anything.
    #[test]
    fn the_lights_card_shows_the_mode_and_opens_a_list() {
        for (now, mode) in [("off", LedMode::Off), ("on", LedMode::On), ("heartbeat", LedMode::Heartbeat)] {
            let rows = lights(true, now);
            let card = lights_card(&rows).expect("a lights card");
            assert_eq!(card.value, mode.label(), "from {now}");
            assert_eq!(card.kind(), Kind::Link, "it opens something");
        }
    }

    fn lit(mode: &str) -> Settings {
        let mut settings = Settings::new();
        settings.compose(Some(&answered(true, mode)), None, None);
        section(&mut settings, "Cihaz");
        assert!(settings.step(1, 0));
        assert_eq!(settings.focused().and_then(|r| r.action), Some(Action::ChooseLeds));
        settings
    }

    /// The list opens on the mode the lights are in, marked as chosen.
    #[test]
    fn the_list_opens_on_the_current_mode() {
        let mut settings = lit("on");
        assert!(settings.open_choice());
        let view = settings.choice_view().expect("an open list");
        assert_eq!(view.focus, 1);
        let chosen: Vec<&str> = view
            .items
            .iter()
            .filter(|(_, _, now)| *now)
            .map(|(title, _, _)| title.as_str())
            .collect();
        assert_eq!(chosen, [LedMode::On.label()]);
        assert_eq!(view.items.len(), LedMode::ALL.len());
    }

    /// Moving never sets anything; Ok sets the one under the focus and closes.
    #[test]
    fn ok_sets_the_mode_under_the_focus() {
        let mut settings = lit("heartbeat");
        settings.open_choice();
        assert!(settings.step(0, -1));
        assert!(settings.step(0, -1));
        assert!(!settings.step(0, -1), "Kapalı is the top");
        assert!(!settings.step(1, 0), "sideways does nothing in a list");
        assert_eq!(settings.choose(), Some(Action::SetLeds(LedMode::Off)));
        assert!(!settings.choosing());
    }

    /// Choosing the mode the lights are already in changes nothing.
    #[test]
    fn choosing_the_current_mode_asks_for_nothing() {
        let mut settings = lit("off");
        settings.open_choice();
        assert_eq!(settings.choose(), None);
        assert!(!settings.choosing());
    }

    /// Back closes the list and leaves the remote on the card.
    #[test]
    fn back_closes_the_list_without_setting_anything() {
        let mut settings = lit("on");
        settings.open_choice();
        settings.step(0, 1);
        assert!(settings.back());
        assert!(!settings.choosing());
        assert_eq!(settings.pane, Pane::Rows);
        assert_eq!(settings.focused().and_then(|r| r.action), Some(Action::ChooseLeds));
    }

    /// No lights to set, no list — even if one was open when they went away.
    #[test]
    fn lights_that_go_away_close_the_list() {
        let mut settings = lit("on");
        settings.open_choice();
        settings.compose(Some(&answered(false, "off")), None, None);
        assert!(!settings.choosing());
        assert!(!settings.open_choice());
    }

    /// The question that started this: the lights were turned off and one was
    /// still lit. The screen has to answer it without anybody measuring a pin.
    #[test]
    fn the_red_light_is_explained_rather_than_offered() {
        for available in [true, false] {
            let rows = lights(available, "off");
            let red = rows
                .iter()
                .find(|row| row.label.contains("Kırmızı"))
                .unwrap_or_else(|| panic!("no red row, available={available}"));
            assert!(!red.selectable(), "the red light must not look changeable");
            assert!(red.value.contains("kapatılamaz"));
        }
    }

    /// Every board that is not this one. A row that would do nothing is worse
    /// than a reading that says why.
    #[test]
    fn a_board_with_no_controllable_lights_offers_nothing_to_press() {
        let rows = lights(false, "off");
        assert!(lights_card(&rows).is_none());
    }

    /// Before the daemon has answered, the screen must not claim a mode.
    #[test]
    fn an_unanswered_status_does_not_invent_a_mode() {
        let rows = device(None);
        assert!(lights_card(&rows).is_none());
        assert!(rows.iter().any(|row| row.label == "Yeşil ve mavi led"));
    }

    // --------------------------------------------------------------- the fan

    fn fan_answer() -> serde_json::Value {
        serde_json::json!({"fan": {
            "available": true,
            "temperature_c": 52.4,
            "pwm": 50,
            "pwm_percent": 19.6,
            "rpm_available": false,
            "control_backend": "kernel-pwm-fan",
            "curve": mediabox_core::FanCurve::balanced(),
            "pending_reboot": false,
            "board_fix": "active",
            "boot_config_ready": true,
        }})
    }

    fn output_answer() -> serde_json::Value {
        serde_json::to_value(super::super::output::tests::status_for_tests()).unwrap()
    }

    /// The fan card opens the editor, Left from its first button comes back
    /// to the card, and neither changes the curve.
    #[test]
    fn the_cooling_card_hands_the_remote_to_the_editor_and_back() {
        let mut settings = Settings::new();
        settings.compose(Some(&fan_answer()), None, None);
        section(&mut settings, "Cihaz");
        let before = settings.cooling.draft().cloned();
        assert!(settings.step(1, 0));
        let card = settings
            .rows()
            .iter()
            .find(|row| row.page == Some(Page::Cooling))
            .expect("a cooling card")
            .clone();
        assert!(card.value.contains("52 °C"), "{}", card.value);
        assert!(settings.open(Page::Cooling));
        assert!(settings.is_cooling());
        assert_eq!(settings.heading().0, "Soğutma");
        assert!(settings.step(0, 1));
        assert!(settings.step(-1, 0));
        assert_eq!(settings.pane, Pane::Rows);
        assert!(!settings.is_cooling());
        assert_eq!(settings.cooling.draft().cloned(), before);
    }

    /// No fan, no editor: the category says why and nothing opens.
    #[test]
    fn a_board_with_no_fan_does_not_open_the_editor() {
        let mut settings = Settings::new();
        let status = serde_json::json!({"fan": {
            "available": false, "rpm_available": false, "control_backend": "kernel-pwm-fan",
            "pending_reboot": false, "board_fix": "not_needed", "boot_config_ready": false,
            "error": "bu kartta pwm-fan denetimli bir fan bulunamadı",
        }});
        settings.compose(Some(&status), None, None);
        let rows = device(Some(&status));
        let fan = rows.iter().find(|row| row.label == "Fan").expect("a fan reading");
        assert!(!fan.selectable());
        assert!(fan.value.contains("pwm-fan"));
        assert!(!settings.open(Page::Cooling));
        assert_eq!(settings.pane, Pane::Sections);
    }

    // ------------------------------------------------------------ the network

    fn connections(diagnostics: &Value) -> Vec<Row> {
        compose(None, Some(diagnostics), None)
            .into_iter()
            .find(|group| group.title == "Bağlantılar")
            .expect("connections")
            .rows
    }

    fn plus_like(wired_up: bool) -> Value {
        serde_json::json!({
            "network": {
                "default": {"interface": if wired_up { "end1" } else { "wlan0" }, "gateway": "192.0.2.1"},
                "interfaces": [
                    {"name": "wlan0", "address": "192.0.2.74", "state": "up", "wireless": true},
                    {"name": "end1", "address": if wired_up { Value::from("192.0.2.80") } else { Value::Null },
                     "state": if wired_up { "up" } else { "down" }, "wireless": false},
                    {"name": "end0", "address": null, "state": "down", "wireless": false},
                ],
            },
            "wireless": {"wifi": {"state": "bağlı", "ssid": "Ev Ağı", "signal": -72,
                                  "address": "192.0.2.74", "interface": "wlan0"}},
        })
    }

    /// A Plus has two wired ports: both are listed, numbered, each with its
    /// own address — and the radio's address is its own line.
    #[test]
    fn every_wired_port_is_listed_with_its_own_address() {
        let rows = connections(&plus_like(true));
        assert_eq!(value_of(&rows, "Ethernet 1"), "Kablo takılı değil · end0");
        assert_eq!(value_of(&rows, "Ethernet 2"), "192.0.2.80 · end1");
        assert_eq!(value_of(&rows, "Wi-Fi adresi"), "192.0.2.74 · wlan0");
        assert_eq!(value_of(&rows, "Varsayılan bağlantı"), "Ethernet 2");
        assert!(rows.iter().all(|row| row.label != "Ethernet"), "numbered when there are two");
    }

    /// An Ultra has one: it is simply "Ethernet".
    #[test]
    fn a_single_wired_port_is_not_numbered() {
        let diagnostics = serde_json::json!({"network": {
            "default": {"interface": "end0", "gateway": "192.0.2.1"},
            "interfaces": [{"name": "end0", "address": "192.0.2.9", "state": "up", "wireless": false}],
        }});
        let rows = connections(&diagnostics);
        assert_eq!(value_of(&rows, "Ethernet"), "192.0.2.9 · end0");
        assert_eq!(value_of(&rows, "Varsayılan bağlantı"), "Ethernet");
        assert!(
            rows.iter().all(|row| !["Wi-Fi ağı", "Wi-Fi sinyali", "Wi-Fi adresi"].contains(&row.label.as_str())),
            "no radio, no radio rows"
        );
    }

    /// The network the radio is on, and how strong it is, in words and dBm.
    #[test]
    fn the_radio_says_its_network_and_its_signal() {
        let rows = connections(&plus_like(false));
        assert_eq!(value_of(&rows, "Wi-Fi ağı"), "Ev Ağı");
        let signal = rows.iter().find(|row| row.label == "Wi-Fi sinyali").expect("a signal row");
        assert_eq!(signal.value, "Orta · -72 dBm");
        assert_eq!(signal.tone, "warn", "a verdict, and a word beside the colour");
        assert_eq!(value_of(&rows, "Varsayılan bağlantı"), "Wi-Fi");
    }

    /// Nothing about the network is claimed before the daemon has answered.
    #[test]
    fn an_unanswered_network_claims_nothing() {
        let rows = compose(None, None, None)
            .into_iter()
            .find(|group| group.title == "Bağlantılar")
            .unwrap()
            .rows;
        assert_eq!(value_of(&rows, "Durum"), "Okunuyor…");
        assert!(rows.iter().all(|row| !row.label.starts_with("Ethernet")));
    }

    fn two_ports() -> Value {
        serde_json::json!({"ethernet": {"ports": [
            {"name": "end1", "carrier": false, "config": {"mode": "dhcp"}},
            {"name": "end0", "carrier": true, "addresses": ["192.0.2.50/24"],
             "config": {"mode": "static", "address": "192.0.2.50", "prefix": 24}},
        ]}})
    }

    /// A card per wired port the daemon found, numbered as the "Ağ" rows
    /// number them, each saying how it takes its address.
    #[test]
    fn every_wired_port_has_a_card() {
        let groups = compose(Some(&two_ports()), None, None);
        let rows = &groups.iter().find(|g| g.title == "Bağlantılar").unwrap().rows;
        let cards: Vec<(&str, &str, Option<&str>)> = rows
            .iter()
            .filter(|row| row.page == Some(Page::Ethernet))
            .map(|row| (row.label.as_str(), row.value.as_str(), row.port.as_deref()))
            .collect();
        assert_eq!(
            cards,
            [
                ("Ethernet 1", "Elle · 192.0.2.50/24", Some("end0")),
                ("Ethernet 2", "Otomatik (DHCP)", Some("end1")),
            ]
        );
        // A daemon that says nothing about ports: no cards, nothing invented.
        let groups = compose(None, None, None);
        let rows = &groups.iter().find(|g| g.title == "Bağlantılar").unwrap().rows;
        assert!(rows.iter().all(|row| row.page != Some(Page::Ethernet)));
    }

    /// The card opens its own port's page, headed with its own name, and Left
    /// comes back to it.
    #[test]
    fn a_port_card_opens_that_port() {
        let mut settings = Settings::new();
        settings.compose(Some(&two_ports()), None, None);
        section(&mut settings, "Bağlantılar");
        assert!(settings.step(1, 0)); // Wi-Fi
        settings.step(0, 1); // Bluetooth
        settings.step(0, 1); // Ethernet 1
        settings.step(0, 1); // Ethernet 2
        assert_eq!(settings.focused().and_then(|r| r.port.clone()).as_deref(), Some("end1"));
        assert!(settings.open(Page::Ethernet));
        assert!(settings.is_ethernet());
        assert_eq!(settings.page_label(), "Ethernet 2");
        assert_eq!(settings.heading(), ("Ethernet 2".into(), "Adres ayarı · end1".into()));
        assert!(settings.step(-1, 0));
        assert_eq!(settings.pane, Pane::Rows);
        assert_eq!(settings.focused().and_then(|r| r.port.clone()).as_deref(), Some("end1"));
    }

    // ------------------------------------------------------------ the display

    /// The display card says what is on the wire, opens the simple face, and
    /// the advanced face is named in the heading.
    #[test]
    fn the_display_card_opens_the_editor() {
        let mut settings = Settings::new();
        let status = serde_json::json!({"output": output_answer()});
        settings.compose(Some(&status), None, None);
        section(&mut settings, "Görüntü ve Ses");
        assert!(settings.step(1, 0));
        let card = settings.focused().expect("the display card").clone();
        assert_eq!(card.page, Some(Page::Output));
        assert_eq!(card.value, "3840×2160 · 60 Hz");
        assert!(settings.open(Page::Output));
        assert!(settings.in_output());
        assert_eq!(settings.heading().0, "Ekran");
        // Left from the simple face comes back to the card.
        assert!(settings.step(-1, 0));
        assert_eq!(settings.pane, Pane::Rows);
    }

    /// No display to set: a reading that says why, not a card that does
    /// nothing.
    #[test]
    fn no_display_is_a_reading() {
        let status = serde_json::json!({"output": {"setting": {}, "error": "EDID okunamadı"}});
        let groups = compose(Some(&status), None, None);
        let row = &groups[1].rows[0];
        assert!(!row.selectable());
        assert_eq!(row.value, "EDID okunamadı");
    }
}
