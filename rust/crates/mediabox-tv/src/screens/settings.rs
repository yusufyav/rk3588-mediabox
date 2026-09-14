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

use serde_json::Value;

use crate::model::DisplayStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Opens the diagnostics screen.
    OpenDiagnostics,
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
        matches!(self, Action::RestartPlayer | Action::Restart | Action::Shutdown)
    }

    pub fn question(self) -> &'static str {
        match self {
            Action::RestartPlayer => "Oynatıcı yeniden başlatılsın mı?",
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
        Self { tone: tone.into(), ..Row::reading(label, value) }
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
}

impl Settings {
    pub fn new() -> Self {
        let mut settings =
            Self { groups: Vec::new(), section: 0, pane: Pane::Sections, positions: Vec::new() };
        settings.compose(None, None, None);
        settings
    }

    pub fn row_index(&self) -> usize {
        self.positions.get(self.section).copied().unwrap_or(0)
    }

    pub fn rows(&self) -> &[Row] {
        self.groups.get(self.section).map(|g| g.rows.as_slice()).unwrap_or(&[])
    }

    pub fn focused(&self) -> Option<&Row> {
        self.rows().get(self.row_index())
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
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
                let next = (self.section as i32 + dy).clamp(0, self.groups.len() as i32 - 1) as usize;
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
        let direction = if direction == 0 { 1 } else { direction.signum() };
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
                    if kodi_running { "Çalışıyor" } else { "Kapalı" },
                    if kodi_running { "good" } else { "" },
                ),
                Row::reading("Ekranı tutan", owner),
                Row::reading(
                    "Görüntü",
                    "Kopyalanır — yeniden kodlanmaz",
                ),
                Row::reading("Ses", "Gerekirse AC-3'e çevrilir"),
                Row::act("Oynatıcıyı yeniden başlat", "Kodi'yi kapatıp açar", Action::RestartPlayer),
            ],
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
            ],
        },
        Group {
            title: "Bluetooth".into(),
            rows: vec![Row::reading(
                "Durum",
                text(diagnostics, "/bluetooth/state")
                    .or_else(|| text(status, "/bluetooth/state"))
                    .unwrap_or_else(|| "Bu sürümde yönetilmiyor".into()),
            )],
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
                Row::act("Televizyonu uyandır", "CEC ile açılış isteği", Action::WakeTelevision),
                Row::act("Televizyonu beklemeye al", "CEC ile standby", Action::StandbyTelevision),
            ],
        },
        Group {
            title: "Ekran".into(),
            rows: vec![
                Row::reading(
                    "Kip",
                    if mode_width > 0 {
                        format!("{mode_width}×{mode_height} @ {refresh} Hz")
                    } else {
                        dash()
                    },
                ),
                Row::reading("Kaynak", "Panelin tercih ettiği kip"),
                Row::reading("Arayüz rengi", "SDR · BT.709"),
                Row::reading("Çıkış", "HDMI-A · doğrudan tarama"),
            ],
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
        assert!(settings.focused().map(|row| row.selectable()).unwrap_or(false));
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
        assert_eq!(settings.focused().and_then(|r| r.action), Some(Action::Restart));
        settings.step(0, 1);
        assert_eq!(settings.focused().and_then(|r| r.action), Some(Action::Shutdown));
    }

    /// The whole point of the settings screen's power rows.
    #[test]
    fn every_destructive_row_asks_first() {
        let settings = Settings::new();
        for group in &settings.groups {
            for row in &group.rows {
                let Some(action) = row.action else { continue };
                if matches!(action, Action::Restart | Action::Shutdown | Action::RestartPlayer) {
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
    }
}
