//! What the box can be told, from the sofa.
//!
//! Sections down the left, rows down the right, and the same rule as the rest
//! of the interface: the remote is on exactly one of the two panes and crossing
//! between them is Left and Right, never a guess.
//!
//! Most rows are readings rather than switches. This is an appliance and the
//! things worth changing from a television remote are few; what belongs here is
//! being able to *see* that the box is on the network, that CEC found the
//! television, that the player is where it should be — and to act on the three
//! or four things a person actually does: wake the television, send it to
//! standby, restart the player, restart the box.
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
    /// Set the board's indicator lights to this mode. The row carries the mode
    /// it would move to rather than a "cycle" instruction, so what a press
    /// does is decided while the screen is composed and is visible in the row
    /// itself.
    SetLeds(LedMode),
    /// Send the television this colour mode, or `None` to let the measurement
    /// decide. The row carries the value it would move to, for the same reason
    /// the lights do: what a press does is settled while the screen is
    /// composed, and is readable in the row.
    SetColorMode(Option<(mediabox_core::ColorFormat, u8)>),
    /// A resolution from the list, or `Auto`.
    SetResolution(mediabox_core::ResolutionChoice),
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub hint: String,
    /// "" for a reading, "good" / "warn" / "bad" for one that has a verdict.
    pub tone: String,
    pub action: Option<Action>,
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
        ..Row::act("Wi-Fi", "Ağları ara ve bağlan", Action::OpenWifi)
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
        ..Row::act("Bluetooth", "Aygıt ara ve eşleştir", Action::OpenBluetooth)
    }
}

impl Row {
    fn reading(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            hint: String::new(),
            tone: String::new(),
            action: None,
        }
    }

    fn toned(label: &str, value: impl Into<String>, tone: &str) -> Self {
        Self {
            tone: tone.into(),
            ..Row::reading(label, value)
        }
    }

    fn act(label: &str, hint: &str, action: Action) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            hint: hint.into(),
            tone: String::new(),
            action: Some(action),
        }
    }

    pub fn selectable(&self) -> bool {
        self.action.is_some()
    }
}

pub struct Group {
    pub title: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sections,
    Rows,
}

pub struct Settings {
    pub groups: Vec<Group>,
    pub section: usize,
    pub pane: Pane,
    positions: Vec<usize>,
    /// The fan curve editor, which is the whole of the "Soğutma" section: a
    /// graph and a list of points rather than rows of readings.
    pub cooling: super::cooling::Cooling,
}

impl Settings {
    pub fn new() -> Self {
        let mut settings = Self {
            groups: Vec::new(),
            section: 0,
            pane: Pane::Sections,
            positions: Vec::new(),
            cooling: super::cooling::Cooling::new(),
        };
        settings.compose(None, None, None);
        settings
    }

    pub fn row_index(&self) -> usize {
        self.positions.get(self.section).copied().unwrap_or(0)
    }

    pub fn rows(&self) -> &[Row] {
        self.groups
            .get(self.section)
            .map(|g| g.rows.as_slice())
            .unwrap_or(&[])
    }

    pub fn focused(&self) -> Option<&Row> {
        self.rows().get(self.row_index())
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        // The fan editor has its own list and its own buttons; the section
        // list only hands the remote over and takes it back.
        if self.is_cooling() {
            match self.pane {
                Pane::Sections if dx > 0 => {
                    if !self.cooling.available() {
                        return false;
                    }
                    self.pane = Pane::Rows;
                    self.cooling.enter();
                    return true;
                }
                Pane::Rows => {
                    return match self.cooling.step(dx, dy) {
                        super::cooling::Nav::Moved => true,
                        super::cooling::Nav::Unchanged => false,
                        super::cooling::Nav::Leave => {
                            self.pane = Pane::Sections;
                            true
                        }
                    };
                }
                Pane::Sections => {}
            }
        }
        match self.pane {
            Pane::Sections => {
                if dx > 0 {
                    if self.rows().is_empty() {
                        return false;
                    }
                    self.pane = Pane::Rows;
                    self.settle(1);
                    return true;
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
            Pane::Rows => {
                if dx < 0 {
                    self.pane = Pane::Sections;
                    return true;
                }
                if dy == 0 {
                    return false;
                }
                let before = self.row_index();
                self.settle_from(before as i32 + dy, dy);
                self.row_index() != before
            }
        }
    }

    /// Lands on the nearest row that can actually be chosen.
    ///
    /// A settings screen is mostly readings, and focus sitting on a line that
    /// does nothing when Ok is pressed is focus a viewer cannot trust. Rows
    /// that are only information are stepped over.
    fn settle(&mut self, direction: i32) {
        let start = self.row_index() as i32;
        self.settle_from(start, direction);
    }

    fn settle_from(&mut self, start: i32, direction: i32) {
        let rows = self.rows().len() as i32;
        if rows == 0 {
            return;
        }
        let direction = if direction == 0 {
            1
        } else {
            direction.signum()
        };
        let mut at = start;
        while at >= 0 && at < rows {
            if self.rows()[at as usize].selectable() {
                self.positions[self.section] = at as usize;
                return;
            }
            at += direction;
        }
        // Nothing selectable that way; leave the remote where it was.
    }

    /// Rebuilt from whatever the control plane last answered. Keeps the pane
    /// and the position, because this runs on a timer.
    pub fn compose(
        &mut self,
        status: Option<&Value>,
        diagnostics: Option<&Value>,
        display: Option<&DisplayStatus>,
    ) {
        self.cooling.load(fan_status(status));
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
        self.settle(1);
    }
}

fn fan_status(status: Option<&Value>) -> Option<FanStatus> {
    serde_json::from_value(status?.get("fan")?.clone()).ok()
}

impl Settings {
    /// Whether the section on screen is the fan editor, which draws and moves
    /// by its own rules.
    /// Whether keys belong to the fan editor: its section, with the focus in
    /// it rather than on the section list.
    pub fn typing_cooling(&self) -> bool {
        self.is_cooling() && self.pane == Pane::Rows
    }

    pub fn is_cooling(&self) -> bool {
        self.groups
            .get(self.section)
            .is_some_and(|group| group.title == COOLING)
    }
}

/// The section the fan editor lives in.
pub const COOLING: &str = "Soğutma";

/// What the section list says before the editor can open: why there is no
/// fan to edit.
fn cooling(status: Option<&Value>) -> Vec<Row> {
    let reason = match fan_status(status) {
        Some(fan) if fan.available => return vec![Row::reading("Fan eğrisi", "Sağa basın")],
        Some(fan) => fan.error.unwrap_or_else(|| "Fan okunamadı".into()),
        None => "Fan okunuyor…".into(),
    };
    vec![Row::reading("Fan", reason)]
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

/// The board's indicator lights.
///
/// Three lights, and only two of them are anybody's to change. The red one is
/// wired to the supply and appears nowhere in the device tree, so it is a
/// reading here that says as much — a person who has just turned the other two
/// off and can still see a light needs to be told why, on the screen, rather
/// than left to wonder whether the setting worked.
///
/// The choosable row carries the *next* mode rather than a cycle: the value on
/// the right is where the lights are now, and pressing Ok moves to the mode
/// named in the hint. A remote has three buttons that matter and no text
/// field, so stepping round a ring of three is the whole interaction.
fn leds(status: Option<&Value>) -> Vec<Row> {
    let red = Row::reading("Kırmızı ışık", "Donanımdan yanar — kapatılamaz");

    if flag(status, "/leds/available") != Some(true) {
        // Either the daemon has not answered yet, or this is not a board whose
        // lights are on gpio-leds. Either way there is nothing to press.
        let reason =
            text(status, "/leds/error").unwrap_or_else(|| "Denetlenebilir ışık bulunamadı".into());
        return vec![Row::reading("Yeşil ve mavi ışık", reason), red];
    }

    let mode = match text(status, "/leds/mode").as_deref() {
        Some("off") => LedMode::Off,
        Some("on") => LedMode::On,
        _ => LedMode::Heartbeat,
    };
    let next = mode.next();

    vec![
        Row {
            label: "Yeşil ve mavi ışık".into(),
            value: mode.label().into(),
            hint: format!("Ok: {}", next.label()),
            tone: if mode == LedMode::Off {
                "good".into()
            } else {
                String::new()
            },
            action: Some(Action::SetLeds(next)),
        },
        red,
        Row::reading("Kalıcılık", "Seçim yeniden başlatmadan sonra korunur"),
    ]
}

/// What the television can be sent, and what it is being told to send.
///
/// The list is measured, never fixed: it is the formats the sink advertises
/// intersected with what the link can carry at each mode it lists. Those are
/// two different facts and only their intersection is true. A Sony
/// KD-65XE9005 declares its HDMI 1 a 300 MHz port and its HDMI 3 a 600 MHz
/// one, so at 4K60 the first will take nothing but YCbCr 4:2:0 at eight bits
/// -- and eight-bit HDR bands where anybody can see it, which is why the row
/// says HDR does not fit there rather than offering it.
///
/// One row to press, in the shape the lights already use: it shows what it is
/// on and what the next press moves to, so nothing about the choice is hidden
/// behind a menu a remote has to walk into.
/// The display: resolution first, then the colour modes of the resolution in
/// use -- the order the reference Android box puts them in.
///
/// Both lists come from the interface's own display: every mode the kernel
/// lists for the television plugged in now, and for each the formats the
/// HDMI rules allow (mediabox_platform::video). Nothing is listed that this
/// television did not declare, and nothing that the link cannot carry.
fn display_rows(status: Option<&Value>) -> Vec<Row> {
    let Some(offer) = crate::platform::display_offer() else {
        return vec![Row::reading("Çözünürlük", "Ölçülecek bir ekran yok")];
    };
    let timings = offer
        .get("timings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let current = offer.get("current").and_then(Value::as_str).unwrap_or("—").to_string();
    let auto = offer.get("auto").and_then(Value::as_str).unwrap_or("—").to_string();
    let choice: mediabox_core::ResolutionChoice = offer
        .get("choice")
        .and_then(|choice| serde_json::from_value(choice.clone()).ok())
        .unwrap_or_default();

    let mut rows = Vec::new();
    rows.push(Row::toned("Çözünürlük", current.clone(), "good"));
    rows.push(Row {
        label: "Otomatik".into(),
        value: auto,
        hint: if choice == mediabox_core::ResolutionChoice::Auto {
            "Seçili".into()
        } else {
            "Ok: seç".into()
        },
        tone: if choice == mediabox_core::ResolutionChoice::Auto {
            "good".into()
        } else {
            String::new()
        },
        action: Some(Action::SetResolution(mediabox_core::ResolutionChoice::Auto)),
    });
    for timing in &timings {
        let label = timing.get("label").and_then(Value::as_str).unwrap_or("?").to_string();
        let fixed = mediabox_core::ResolutionChoice::Fixed {
            width: timing.get("width").and_then(Value::as_u64).unwrap_or(0) as u16,
            height: timing.get("height").and_then(Value::as_u64).unwrap_or(0) as u16,
            refresh_mhz: timing.get("refresh_mhz").and_then(Value::as_u64).unwrap_or(0) as u32,
            interlaced: timing.get("interlaced").and_then(Value::as_bool).unwrap_or(false),
        };
        let allowed = timing.get("allowed").and_then(Value::as_array).map_or(0, Vec::len);
        let hdr = timing.get("hdr10").and_then(Value::as_bool) == Some(true);
        let selected = choice == fixed;
        rows.push(Row {
            label,
            value: if allowed == 0 {
                "bu bağlantıya sığmıyor".into()
            } else if hdr {
                "HDR10".into()
            } else {
                String::new()
            },
            hint: if selected { "Seçili".into() } else { "Ok: seç".into() },
            tone: if selected { "good".into() } else { String::new() },
            action: (allowed > 0).then_some(Action::SetResolution(fixed)),
        });
    }

    // The colour modes of the resolution in use.
    let colour: Option<(mediabox_core::ColorFormat, u8)> = status
        .and_then(|status| status.pointer("/display_color/choice"))
        .and_then(|choice| serde_json::from_value::<mediabox_core::ColorChoice>(choice.clone()).ok())
        .and_then(|choice| choice.mode())
        .map(|mode| (mode.format, mode.bits));
    let here = timings
        .iter()
        .find(|timing| timing.get("label").and_then(Value::as_str) == Some(current.as_str()));
    let allowed: Vec<mediabox_core::ColorMode> = here
        .and_then(|timing| timing.get("allowed"))
        .and_then(|allowed| serde_json::from_value(allowed.clone()).ok())
        .unwrap_or_default();
    let auto_colour = here
        .and_then(|timing| timing.get("auto_sdr"))
        .and_then(|mode| serde_json::from_value::<mediabox_core::ColorMode>(mode.clone()).ok())
        .map(|mode| mode.label())
        .unwrap_or_else(|| "—".into());
    rows.push(Row::toned(
        "Renk modu",
        match colour {
            None => format!("Otomatik · {auto_colour}"),
            Some((format, bits)) => format!("{} {}bit", format.label(), bits),
        },
        "good",
    ));
    rows.push(Row {
        label: "Otomatik".into(),
        value: auto_colour,
        hint: if colour.is_none() { "Seçili".into() } else { "Ok: seç".into() },
        tone: if colour.is_none() { "good".into() } else { String::new() },
        action: Some(Action::SetColorMode(None)),
    });
    for mode in allowed {
        let selected = colour == Some((mode.format, mode.bits));
        rows.push(Row {
            label: mode.label(),
            value: if mode.carries_hdr() { "HDR10 taşır".into() } else { String::new() },
            hint: if selected { "Seçili".into() } else { "Ok: seç".into() },
            tone: if selected { "good".into() } else { String::new() },
            action: Some(Action::SetColorMode(Some((mode.format, mode.bits)))),
        });
    }

    // What the numbers above come from.
    let ceiling = status
        .and_then(|status| status.pointer("/display_color/max_character_rate_khz"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    rows.push(Row::reading(
        "Bağlantı tavanı",
        if ceiling > 0 {
            format!("{} MHz (ekran bildirdi)", ceiling / 1000)
        } else {
            "ekran bildirmedi".into()
        },
    ));
    rows
}

/// The Stremio account.
///
/// The same three readings the web interface shows, from the same place in the
/// status, and one row to press. Connected, it offers the way out; not
/// connected, it offers the way in — there is never both, because a screen that
/// shows a sign-out button to somebody who is not signed in is a screen that
/// has not read its own state.
fn account(status: Option<&Value>) -> Vec<Row> {
    let signed_in = flag(status, "/media/provider/authenticated").unwrap_or(false);
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
            Row::toned("Durum", "Bağlı değil", "warn"),
            note,
            Row::act(
                "Giriş yap",
                "E-posta ve parolanızı kumandayla girin",
                Action::OpenAccount,
            ),
        ];
    }

    vec![
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
        Row::act("Çıkış yap", "Hesabın bağlantısını keser", Action::SignOut),
    ]
}

fn compose(
    status: Option<&Value>,
    diagnostics: Option<&Value>,
    display: Option<&DisplayStatus>,
) -> Vec<Group> {
    let dash = || "—".to_string();

    let owner = display
        .and_then(|d| d.owner.clone())
        .map(|id| match id.as_str() {
            "mediabox" => "MediaBox".to_string(),
            "kodi" => "Oynatıcı".to_string(),
            "browser" => "Tarayıcı".to_string(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "Boşta".into());

    let (cec_state, cec_tone) = yes_no(flag(status, "/cec/available"));
    let kodi_running = flag(status, "/kodi/running").unwrap_or(false);
    let (mode_width, mode_height, refresh) = crate::platform::active_mode();

    vec![
        Group {
            title: "Oynatma".into(),
            rows: vec![
                Row::toned(
                    "Oynatıcı",
                    if kodi_running {
                        "Çalışıyor"
                    } else {
                        "Kapalı"
                    },
                    if kodi_running { "good" } else { "" },
                ),
                Row::reading("Ekranı tutan", owner),
                Row::reading("Görüntü", "Kopyalanır — yeniden kodlanmaz"),
                Row::reading("Ses", "Gerekirse AC-3'e çevrilir"),
                Row::act(
                    "Oynatıcıyı yeniden başlat",
                    "Kodi'yi kapatıp açar",
                    Action::RestartPlayer,
                ),
            ],
        },
        Group {
            title: "Hesap".into(),
            rows: account(status),
        },
        Group {
            title: "Ağ".into(),
            rows: vec![
                Row::reading(
                    "Arayüz",
                    text(diagnostics, "/network/default/interface").unwrap_or_else(dash),
                ),
                Row::reading(
                    "Adres",
                    text(diagnostics, "/network/default/address").unwrap_or_else(dash),
                ),
                Row::reading(
                    "Ağ geçidi",
                    text(diagnostics, "/network/default/gateway").unwrap_or_else(dash),
                ),
                Row::reading("Uzaktan kumanda", "http://<cihaz>:8788"),
                wifi_row(diagnostics),
            ],
        },
        Group {
            title: "Bluetooth".into(),
            rows: vec![bluetooth_row(diagnostics)],
        },
        Group {
            title: "HDMI ve CEC".into(),
            rows: vec![
                Row::toned("CEC", cec_state, cec_tone),
                Row::reading(
                    "Bağdaştırıcı",
                    text(status, "/cec/adapter").unwrap_or_else(dash),
                ),
                Row::reading(
                    "Fiziksel adres",
                    text(status, "/cec/physical_address").unwrap_or_else(dash),
                ),
                Row::act(
                    "Televizyonu uyandır",
                    "CEC ile açılış isteği",
                    Action::WakeTelevision,
                ),
                Row::act(
                    "Televizyonu beklemeye al",
                    "CEC ile standby",
                    Action::StandbyTelevision,
                ),
            ],
        },
        Group {
            title: "Ekran".into(),
            rows: {
                let mut rows = vec![
                    Row::reading(
                        "Kip",
                        if mode_width > 0 {
                            format!("{mode_width}×{mode_height} @ {refresh} Hz")
                        } else {
                            dash()
                        },
                    ),
                    Row::reading("Kaynak", "Panelin tercih ettiği kip"),
                    Row::reading("Çıkış", "HDMI-A · doğrudan tarama"),
                ];
                rows.extend(display_rows(status));
                rows
            },
        },
        Group {
            title: "Ses".into(),
            rows: vec![
                Row::reading(
                    "Çıkış",
                    text(diagnostics, "/audio/default").unwrap_or_else(|| "HDMI".into()),
                ),
                Row::reading("Geçirgen kodekler", "AC-3 · E-AC-3 · DTS"),
                Row::reading("Nesne tabanlı ses", "AC-3'e çevrilir"),
            ],
        },
        Group {
            title: "Işıklar".into(),
            rows: leds(status),
        },
        Group {
            title: COOLING.into(),
            rows: cooling(status),
        },
        Group {
            title: "Sistem".into(),
            rows: vec![
                Row::reading("Sürüm", text(status, "/version").unwrap_or_else(dash)),
                Row::reading("Makine", text(status, "/hostname").unwrap_or_else(dash)),
                Row::reading("Çekirdek", text(status, "/kernel").unwrap_or_else(dash)),
                Row::reading(
                    "Açık kalma",
                    status
                        .and_then(|s| s.pointer("/uptime_seconds"))
                        .and_then(Value::as_u64)
                        .map(duration)
                        .unwrap_or_else(dash),
                ),
                Row::act("Yeniden başlat", "Cihazı kapatıp açar", Action::Restart),
                Row::act("Kapat", "Cihazı kapatır", Action::Shutdown),
            ],
        },
        Group {
            title: "Tanılama".into(),
            rows: vec![Row::act(
                "Tanılamayı aç",
                "İşlemci, bellek, sıcaklık, ekran, servisler",
                Action::OpenDiagnostics,
            )],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_lands_on_something_that_can_be_pressed() {
        let settings = Settings::new();
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

    #[test]
    fn the_brief_s_sections_are_all_here() {
        let settings = Settings::new();
        let titles: Vec<&str> = settings.groups.iter().map(|g| g.title.as_str()).collect();
        for wanted in [
            "Oynatma",
            "Ağ",
            "Bluetooth",
            "HDMI ve CEC",
            "Ekran",
            "Ses",
            "Soğutma",
            "Sistem",
            "Tanılama",
        ] {
            assert!(titles.contains(&wanted), "{wanted} missing from {titles:?}");
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

    #[test]
    fn readings_are_stepped_over() {
        let mut settings = Settings::new();
        // "Sistem" — four readings then two actions.
        settings.section = settings
            .groups
            .iter()
            .position(|g| g.title == "Sistem")
            .expect("system section");
        settings.pane = Pane::Rows;
        settings.settle(1);
        assert_eq!(
            settings.focused().and_then(|r| r.action),
            Some(Action::Restart)
        );
        settings.step(0, 1);
        assert_eq!(
            settings.focused().and_then(|r| r.action),
            Some(Action::Shutdown)
        );
    }

    /// The whole point of the settings screen's power rows.
    #[test]
    fn every_destructive_row_asks_first() {
        let settings = Settings::new();
        for group in &settings.groups {
            for row in &group.rows {
                let Some(action) = row.action else { continue };
                if matches!(
                    action,
                    Action::Restart | Action::Shutdown | Action::RestartPlayer
                ) {
                    assert!(action.confirms(), "{:?} does not confirm", action);
                    assert!(!action.question().is_empty());
                }
            }
        }
    }

    #[test]
    fn nothing_harmless_drags_a_confirmation_along() {
        assert!(!Action::OpenDiagnostics.confirms());
        assert!(!Action::WakeTelevision.confirms());
        assert!(!Action::StandbyTelevision.confirms());
        // An indicator light is not somebody's evening.
        for mode in LedMode::ALL {
            assert!(!Action::SetLeds(mode).confirms());
            assert!(Action::SetLeds(mode).question().is_empty());
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
        compose(status, None, None)
            .into_iter()
            .find(|group| group.title == "Hesap")
            .expect("account section")
            .rows
    }

    /// The web interface has had this since the beginning; the native shell is
    /// where a person actually sits.
    #[test]
    fn the_settings_screen_has_an_account_section() {
        let settings = Settings::new();
        let titles: Vec<&str> = settings.groups.iter().map(|g| g.title.as_str()).collect();
        assert!(titles.contains(&"Hesap"), "{titles:?}");
    }

    #[test]
    fn a_box_with_no_account_offers_the_way_in() {
        let status = provider(false);
        let rows = account_rows(Some(&status));
        assert_eq!(rows[0].value, "Bağlı değil");
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
        assert_eq!(rows[0].value, "Bağlı");
        assert!(rows.iter().any(|row| row.value == "someone@example.com"));
        assert!(rows.iter().any(|row| row.value == "7"));
        let actions: Vec<Action> = rows.iter().filter_map(|row| row.action).collect();
        assert_eq!(actions, [Action::SignOut]);
    }

    /// An unanswered status is a box that is not signed in, which is also what
    /// a box that has never been asked looks like.
    #[test]
    fn an_unanswered_status_does_not_claim_an_account() {
        let rows = account_rows(None);
        assert_eq!(rows[0].value, "Bağlı değil");
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

    /// Nothing about the account section may put a secret on the panel. The
    /// rows are built from the status, and the status has no password in it —
    /// this holds the screen to that even if one ever appeared.
    #[test]
    fn no_account_row_can_carry_a_secret() {
        let mut status = provider(true);
        status["media"]["provider"]["password"] = "hunter2".into();
        status["media"]["provider"]["authKey"] = "sekritkey".into();
        for row in account_rows(Some(&status)) {
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

    fn lights(available: bool, mode: &str) -> Vec<Row> {
        let status = answered(available, mode);
        compose(Some(&status), None, None)
            .into_iter()
            .find(|group| group.title == "Işıklar")
            .expect("lights section")
            .rows
    }

    /// One press moves one step, and the ring closes. Off leads, because that
    /// is the mode this setting exists to reach.
    #[test]
    fn the_lights_row_steps_round_the_ring() {
        for (now, next) in [
            ("off", LedMode::On),
            ("on", LedMode::Heartbeat),
            ("heartbeat", LedMode::Off),
        ] {
            let rows = lights(true, now);
            let row = rows.first().expect("a row");
            assert!(row.selectable(), "{now} is not selectable");
            assert_eq!(row.action, Some(Action::SetLeds(next)), "from {now}");
            // The value is where the lights are, not where they are going.
            assert_eq!(
                row.value,
                LedMode::ALL[LedMode::ALL
                    .iter()
                    .position(|m| m.next() == next)
                    .expect("a predecessor")]
                .label()
            );
        }
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
        assert!(rows.iter().all(|row| !row.selectable()));
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

    /// Right from the section list opens the editor, Left from its first
    /// button comes back, and neither changes the curve.
    #[test]
    fn the_cooling_section_hands_the_remote_to_the_editor_and_back() {
        let mut settings = Settings::new();
        settings.compose(Some(&fan_answer()), None, None);
        settings.section = settings
            .groups
            .iter()
            .position(|g| g.title == COOLING)
            .expect("cooling section");
        assert!(settings.is_cooling());
        let before = settings.cooling.draft().cloned();
        assert!(settings.step(1, 0));
        assert_eq!(settings.pane, Pane::Rows);
        assert!(settings.step(0, 1));
        assert!(settings.step(-1, 0));
        assert_eq!(settings.pane, Pane::Sections);
        assert_eq!(settings.cooling.draft().cloned(), before);
    }

    /// No fan, no editor: the section says why and the remote stays put.
    #[test]
    fn a_board_with_no_fan_does_not_open_the_editor() {
        let mut settings = Settings::new();
        let status = serde_json::json!({"fan": {
            "available": false, "rpm_available": false, "control_backend": "kernel-pwm-fan",
            "pending_reboot": false, "board_fix": "not_needed", "boot_config_ready": false,
            "error": "bu kartta pwm-fan denetimli bir fan bulunamadı",
        }});
        settings.compose(Some(&status), None, None);
        settings.section = settings
            .groups
            .iter()
            .position(|g| g.title == COOLING)
            .unwrap();
        assert!(!settings.step(1, 0));
        assert_eq!(settings.pane, Pane::Sections);
        assert!(settings.rows()[0].value.contains("pwm-fan"));
    }

    /// Before the daemon has answered, the screen must not claim a mode.
    #[test]
    fn an_unanswered_status_does_not_invent_a_mode() {
        let rows = compose(None, None, None)
            .into_iter()
            .find(|group| group.title == "Işıklar")
            .expect("lights section")
            .rows;
        assert!(rows.iter().all(|row| !row.selectable()));
        assert!(!rows.is_empty());
    }
}
