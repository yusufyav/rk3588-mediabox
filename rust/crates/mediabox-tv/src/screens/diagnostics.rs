//! What the machine is doing, in one place and out of the way.
//!
//! This screen exists because the home screen used to be a dashboard: the
//! processor, the memory, the temperature and the disk were the first thing
//! anybody saw when the television came on. That is a picture of the appliance
//! rather than of what is on it, so the readings moved here, where somebody
//! looking for them will find them and nobody else has to.
//!
//! Raw JSON is not the view. Every reading is named in the product's own words
//! and carries a verdict where there is one to give.

use serde_json::Value;

use crate::model::DisplayStatus;

#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub label: String,
    pub value: String,
    /// "" / "good" / "warn" / "bad".
    pub tone: String,
}

pub struct Group {
    pub title: String,
    pub rows: Vec<Reading>,
}

pub struct Diagnostics {
    pub groups: Vec<Group>,
    /// Which group the remote is on. The rows are read, not operated, so the
    /// focus model here is one column.
    pub group: usize,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            group: 0,
        }
    }

    pub fn step(&mut self, _dx: i32, dy: i32) -> bool {
        if dy == 0 || self.groups.is_empty() {
            return false;
        }
        let next = (self.group as i32 + dy).clamp(0, self.groups.len() as i32 - 1) as usize;
        if next == self.group {
            return false;
        }
        self.group = next;
        true
    }

    pub fn compose(
        &mut self,
        status: Option<&Value>,
        diagnostics: Option<&Value>,
        display: Option<&DisplayStatus>,
    ) {
        self.groups = compose(status, diagnostics, display);
        self.group = self.group.min(self.groups.len().saturating_sub(1));
    }
}

fn reading(label: &str, value: impl Into<String>) -> Reading {
    Reading {
        label: label.into(),
        value: value.into(),
        tone: String::new(),
    }
}

fn toned(label: &str, value: impl Into<String>, tone: &str) -> Reading {
    Reading {
        label: label.into(),
        value: value.into(),
        tone: tone.into(),
    }
}

fn human_size(bytes: f64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn number(root: Option<&Value>, pointer: &str) -> Option<f64> {
    root?.pointer(pointer)?.as_f64()
}

fn string(root: Option<&Value>, pointer: &str) -> Option<String> {
    let value = root?.pointer(pointer)?;
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn heat_tone(celsius: f64) -> &'static str {
    if celsius >= 85.0 {
        "bad"
    } else if celsius >= 70.0 {
        "warn"
    } else {
        "good"
    }
}

fn share_tone(fraction: f64) -> &'static str {
    if fraction >= 0.92 {
        "bad"
    } else if fraction >= 0.8 {
        "warn"
    } else {
        "good"
    }
}

fn compose(
    status: Option<&Value>,
    diagnostics: Option<&Value>,
    display: Option<&DisplayStatus>,
) -> Vec<Group> {
    let dash = || "—".to_string();
    let mut groups = Vec::new();

    // ---------------------------------------------------------------- machine
    let mut machine = vec![reading("Kart", "Orange Pi 5 Ultra · RK3588")];
    if let Some(usage) = number(diagnostics, "/cpu/usage") {
        machine.push(toned(
            "İşlemci",
            format!("%{:.0}", usage * 100.0),
            share_tone(usage),
        ));
    }
    if let Some(count) = number(diagnostics, "/cpu/count") {
        machine.push(reading("Çekirdek", format!("{count:.0}")));
    }
    if let Some(one) = number(diagnostics, "/cpu/load/one") {
        machine.push(reading("Yük", format!("{one:.2}")));
    }
    let used = number(diagnostics, "/memory/usedBytes");
    let total = number(diagnostics, "/memory/totalBytes");
    if let (Some(used), Some(total)) = (used, total) {
        if total > 0.0 {
            machine.push(toned(
                "Bellek",
                format!("{} / {}", human_size(used), human_size(total)),
                share_tone(used / total),
            ));
        }
    }
    if let Some(entries) = diagnostics
        .and_then(|d| d.get("temperatures"))
        .and_then(Value::as_array)
    {
        for entry in entries.iter().take(4) {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("sıcaklık");
            if let Some(celsius) = entry.get("celsius").and_then(Value::as_f64) {
                machine.push(toned(name, format!("{celsius:.0} °C"), heat_tone(celsius)));
            }
        }
    }
    groups.push(Group {
        title: "Makine".into(),
        rows: machine,
    });

    // ---------------------------------------------------------------- storage
    let mut storage = Vec::new();
    if let Some(volumes) = diagnostics
        .and_then(|d| d.get("storage"))
        .and_then(Value::as_array)
    {
        for (index, volume) in volumes.iter().take(5).enumerate() {
            // The control plane does not always name a volume, and two rows
            // both called "bölüm" with the same figures read as a bug rather
            // than as two mounts of one disk.
            let fallback = format!("Bölüm {}", index + 1);
            let name = volume
                .get("mount")
                .or_else(|| volume.get("mountPoint"))
                .or_else(|| volume.get("name"))
                .or_else(|| volume.get("device"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(&fallback);
            let used = volume.get("usedBytes").and_then(Value::as_f64);
            let total = volume.get("totalBytes").and_then(Value::as_f64);
            if let (Some(used), Some(total)) = (used, total) {
                if total > 0.0 {
                    storage.push(toned(
                        name,
                        format!("{} / {}", human_size(used), human_size(total)),
                        share_tone(used / total),
                    ));
                }
            }
        }
    }
    if storage.is_empty() {
        storage.push(reading("Bölüm", dash()));
    }
    groups.push(Group {
        title: "Depolama".into(),
        rows: storage,
    });

    // ---------------------------------------------------------------- network
    let mut network = vec![
        reading(
            "Arayüz",
            string(diagnostics, "/network/default/interface").unwrap_or_else(dash),
        ),
        reading(
            "Adres",
            string(diagnostics, "/network/default/address").unwrap_or_else(dash),
        ),
        reading(
            "Ağ geçidi",
            string(diagnostics, "/network/default/gateway").unwrap_or_else(dash),
        ),
    ];
    if let Some(hostname) = string(status, "/hostname") {
        network.push(reading("Makine adı", hostname));
    }
    groups.push(Group {
        title: "Ağ".into(),
        rows: network,
    });

    // ------------------------------------------------------------ display/GPU
    let (width, height, refresh) = crate::platform::active_mode();
    let phases = crate::platform::last_report();
    let mut screen = vec![
        reading(
            "Kip",
            if width > 0 {
                format!("{width}×{height} @ {refresh} Hz")
            } else {
                dash()
            },
        ),
        reading("Bağlayıcı", "HDMI-A"),
        reading("Tarama", "PRIME dma-buf · doğrudan sayfa çevirme"),
        reading("Oluşturucu", "Mali-G610 · GBM armsoc · GLES"),
        reading("Bileştirici", "yok"),
    ];
    if phases.frames > 0 {
        screen.push(reading("Kare hızı", format!("{:.1} fps", phases.fps)));
        screen.push(reading("Sahne", format!("{:.1} ms", phases.draw_ms)));
        screen.push(reading(
            "Sayfa çevirme beklemesi",
            format!("{:.1} ms", phases.flip_ms),
        ));
    }
    groups.push(Group {
        title: "Ekran ve GPU".into(),
        rows: screen,
    });

    // --------------------------------------------------------------- playback
    let kodi_running = status
        .and_then(|s| s.pointer("/kodi/running"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut playback = vec![
        toned(
            "Oynatıcı",
            if kodi_running {
                "Çalışıyor"
            } else {
                "Kapalı"
            },
            if kodi_running { "good" } else { "" },
        ),
        reading(
            "Durum",
            string(status, "/kodi/state").unwrap_or_else(|| "idle".into()),
        ),
        reading(
            "Ekranı tutan",
            display
                .and_then(|d| d.owner.clone())
                .unwrap_or_else(|| "boşta".into()),
        ),
    ];
    if let Some(sessions) = number(status, "/media/sessions") {
        playback.push(reading("Medya oturumu", format!("{sessions:.0}")));
    }
    groups.push(Group {
        title: "Oynatma".into(),
        rows: playback,
    });

    // -------------------------------------------------------------------- CEC
    let available = status
        .and_then(|s| s.pointer("/cec/available"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut cec = vec![
        toned(
            "Bağdaştırıcı",
            if available { "Hazır" } else { "Yok" },
            if available { "good" } else { "bad" },
        ),
        reading("Aygıt", string(status, "/cec/adapter").unwrap_or_else(dash)),
        reading(
            "Fiziksel adres",
            string(status, "/cec/physical_address").unwrap_or_else(dash),
        ),
        reading("Sahibi", "mediaboxd-rs"),
    ];
    if let Some(errors) = status.and_then(|s| s.pointer("/cec/errors")) {
        let sum: u64 = [
            "receive",
            "transmit",
            "nack",
            "low_drive",
            "arbitration_lost",
        ]
        .iter()
        .filter_map(|key| errors.get(key).and_then(Value::as_u64))
        .sum();
        cec.push(toned(
            "Hata",
            sum.to_string(),
            if sum == 0 { "good" } else { "warn" },
        ));
    }
    groups.push(Group {
        title: "CEC".into(),
        rows: cec,
    });

    // --------------------------------------------------------------- services
    let mut services = Vec::new();
    if let Some(entries) = status
        .and_then(|s| s.get("services"))
        .and_then(Value::as_array)
    {
        for entry in entries {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("servis");
            let healthy = entry
                .get("healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let detail = entry
                .get("detail")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if healthy {
                        "çalışıyor".into()
                    } else {
                        "durdu".into()
                    }
                });
            services.push(toned(name, detail, if healthy { "good" } else { "bad" }));
        }
    }
    if services.is_empty() {
        services.push(reading("Servis", dash()));
    }
    groups.push(Group {
        title: "Servisler".into(),
        rows: services,
    });

    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_group_has_rows_even_with_no_answer() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.compose(None, None, None);
        assert!(!diagnostics.groups.is_empty());
        for group in &diagnostics.groups {
            assert!(!group.rows.is_empty(), "{} is empty", group.title);
        }
    }

    #[test]
    fn the_brief_s_groups_are_all_here() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.compose(None, None, None);
        let titles: Vec<&str> = diagnostics
            .groups
            .iter()
            .map(|g| g.title.as_str())
            .collect();
        for wanted in [
            "Makine",
            "Depolama",
            "Ağ",
            "Ekran ve GPU",
            "Oynatma",
            "CEC",
            "Servisler",
        ] {
            assert!(titles.contains(&wanted), "{wanted} missing from {titles:?}");
        }
    }

    #[test]
    fn a_hot_board_says_so() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.compose(
            None,
            Some(&json!({"temperatures": [{"name": "soc", "celsius": 91.0}]})),
            None,
        );
        let machine = &diagnostics.groups[0];
        let row = machine
            .rows
            .iter()
            .find(|r| r.label == "soc")
            .expect("soc reading");
        assert_eq!(row.tone, "bad");
    }

    #[test]
    fn focus_never_leaves_the_groups() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.compose(None, None, None);
        for _ in 0..40 {
            diagnostics.step(0, 1);
        }
        assert!(diagnostics.group < diagnostics.groups.len());
        for _ in 0..40 {
            diagnostics.step(0, -1);
        }
        assert_eq!(diagnostics.group, 0);
    }
}
