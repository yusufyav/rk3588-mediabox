use mediabox_core::{Application, ApplicationStatus, DisplayStatus, Surface, SurfaceStatus};
use std::path::Path;
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::Mutex;

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("Kodi unit adı geçersiz")]
    InvalidUnit,
    #[error("böyle bir uygulama yok: {0}")]
    UnknownApplication(String),
    #[error("uygulama tablosu okunamadı: {0}")]
    Registry(String),
    #[error("systemctl çalıştırılamadı: {0}")]
    Io(#[from] std::io::Error),
    #[error("systemctl restart başarısız: {0}")]
    Failed(String),
    #[error("systemctl restart zaman aşımı")]
    Timeout,
}

#[derive(Clone)]
pub struct KodiLifecycle {
    unit: String,
}

impl KodiLifecycle {
    pub fn new(unit: &str) -> Result<Self, LifecycleError> {
        if !valid_unit(unit) {
            return Err(LifecycleError::InvalidUnit);
        }
        Ok(Self {
            unit: unit.to_string(),
        })
    }

    pub async fn restart(&self) -> Result<(), LifecycleError> {
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            Command::new("/usr/bin/systemctl")
                .arg("restart")
                .arg(&self.unit)
                .output(),
        )
        .await
        .map_err(|_| LifecycleError::Timeout)??;
        if !output.status.success() {
            return Err(LifecycleError::Failed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }
}

/// Validates a systemd unit name before it is ever passed to systemctl.
fn valid_unit(unit: &str) -> bool {
    unit.ends_with(".service")
        && !unit.is_empty()
        && unit.len() <= 64
        && unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@._-".contains(&b))
}

async fn systemctl(args: &[&str]) -> Result<std::process::Output, LifecycleError> {
    Ok(tokio::time::timeout(
        Duration::from_secs(40),
        Command::new("/usr/bin/systemctl").args(args).output(),
    )
    .await
    .map_err(|_| LifecycleError::Timeout)??)
}

async fn unit_is_active(unit: &str) -> bool {
    // `is-active` exits non-zero for an inactive unit, so the status code is
    // the answer and a failure to run systemctl reads as "not active".
    systemctl(&["is-active", "--quiet", unit])
        .await
        .is_ok_and(|output| output.status.success())
}

async fn unit_exists(unit: &str) -> bool {
    systemctl(&["cat", unit])
        .await
        .is_ok_and(|output| output.status.success())
}

/// Owns the appliance display.
///
/// Kodi and the TV-local product UI both want DRM master, so exactly one of
/// them may run. Every transition goes through here: stop the incumbent, wait
/// for it to actually be gone, then start the successor. The mutex makes two
/// concurrent switches impossible, which is what would otherwise leave both
/// units down or both fighting for the same CRTC.
#[derive(Clone)]
pub struct SurfaceManager {
    kodi_unit: String,
    ui_unit: String,
    gate: std::sync::Arc<Mutex<()>>,
}

impl SurfaceManager {
    pub fn new(kodi_unit: &str, ui_unit: &str) -> Result<Self, LifecycleError> {
        if !valid_unit(kodi_unit) || !valid_unit(ui_unit) {
            return Err(LifecycleError::InvalidUnit);
        }
        Ok(Self {
            kodi_unit: kodi_unit.to_string(),
            ui_unit: ui_unit.to_string(),
            gate: std::sync::Arc::new(Mutex::new(())),
        })
    }

    pub async fn status(&self) -> SurfaceStatus {
        let (kodi_active, ui_active, ui_installed) = tokio::join!(
            unit_is_active(&self.kodi_unit),
            unit_is_active(&self.ui_unit),
            unit_exists(&self.ui_unit),
        );
        SurfaceStatus {
            // Kodi wins the label when both are somehow up: it is the one that
            // is actually visible, because it took the master away.
            active: if kodi_active {
                Surface::Kodi
            } else if ui_active {
                Surface::Ui
            } else {
                Surface::Idle
            },
            kodi_active,
            ui_active,
            ui_installed,
        }
    }

    pub async fn switch(&self, target: Surface) -> Result<SurfaceStatus, LifecycleError> {
        let _guard = self.gate.lock().await;
        let (stop, start) = match target {
            Surface::Kodi => (self.ui_unit.clone(), Some(self.kodi_unit.clone())),
            Surface::Ui => (self.kodi_unit.clone(), Some(self.ui_unit.clone())),
            Surface::Idle => {
                self.stop_unit(&self.kodi_unit).await?;
                self.stop_unit(&self.ui_unit).await?;
                return Ok(self.status().await);
            }
        };
        if target == Surface::Ui && !unit_exists(&self.ui_unit).await {
            return Err(LifecycleError::Failed(format!(
                "{} kurulu değil; TV-local UI bu cihazda yok",
                self.ui_unit
            )));
        }
        self.stop_unit(&stop).await?;
        if let Some(unit) = start {
            let output = systemctl(&["start", &unit]).await?;
            if !output.status.success() {
                return Err(LifecycleError::Failed(
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                ));
            }
        }
        Ok(self.status().await)
    }

    async fn stop_unit(&self, unit: &str) -> Result<(), LifecycleError> {
        if !unit_is_active(unit).await {
            return Ok(());
        }
        let output = systemctl(&["stop", unit]).await?;
        if !output.status.success() {
            return Err(LifecycleError::Failed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        // `systemctl stop` returns once the unit is inactive, but the DRM
        // master handle is released by the kernel when the last fd closes.
        // A short settle keeps the successor from racing that teardown.
        tokio::time::sleep(Duration::from_millis(600)).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_validation_blocks_command_injection() {
        assert!(KodiLifecycle::new("kodi.service").is_ok());
        assert!(KodiLifecycle::new("kodi.service;reboot").is_err());
        assert!(KodiLifecycle::new("../../bin/sh.service").is_err());
    }

    #[test]
    fn surface_manager_validates_both_units() {
        assert!(SurfaceManager::new("kodi.service", "mediabox-tv-ui.service").is_ok());
        assert!(SurfaceManager::new("kodi.service", "rm -rf /").is_err());
        assert!(SurfaceManager::new("kodi.service; reboot", "mediabox-tv-ui.service").is_err());
    }
}

// ------------------------------------------------------------- applications

/// The id that means "nobody": release the display and start nothing.
pub const IDLE: &str = "idle";

/// The application id of the television's browser.
pub const BROWSER: &str = "browser";

/// Where the browser reads the address it should open, once, at start.
/// `packaging/mediabox-browser` consumes and deletes it.
const BROWSER_REQUEST: &str = "/var/lib/mediabox-browser/url";

/// What the appliance can run, and which of it is on the television.
///
/// This is the generalisation of `SurfaceManager`, and it exists because
/// MediaBox is a light environment for this board rather than a player with a
/// settings page: Kodi and the product UI are two rows of a table that a
/// browser and a screen receiver join later, by being listed rather than by
/// being coded for.
///
/// The rules it enforces are the ones the hardware imposes. Only one process
/// may hold DRM master, so at most one display-owning application runs at a
/// time; every change of hands goes through one mutex, so two of them cannot
/// race; and the incumbent is stopped and waited for before the successor is
/// started, because the kernel releases the master handle when the last file
/// descriptor closes rather than when systemctl returns.
#[derive(Clone)]
pub struct ApplicationManager {
    applications: std::sync::Arc<Vec<Application>>,
    gate: std::sync::Arc<Mutex<()>>,
}

impl ApplicationManager {
    pub fn new(applications: Vec<Application>) -> Result<Self, LifecycleError> {
        if applications.iter().any(|application| !application.valid()) {
            return Err(LifecycleError::InvalidUnit);
        }
        Ok(Self {
            applications: std::sync::Arc::new(applications),
            gate: std::sync::Arc::new(Mutex::new(())),
        })
    }

    /// Read the table from a file, falling back to the two applications this
    /// appliance has always had when there is no file to read.
    ///
    /// A missing file is not an error: an appliance that has never been
    /// configured still has Kodi and its own interface, and refusing to start
    /// over a file nobody wrote would be a worse answer than the obvious one.
    pub fn load(
        path: Option<&Path>,
        kodi_unit: &str,
        ui_unit: &str,
    ) -> Result<Self, LifecycleError> {
        let Some(path) = path else {
            return Self::new(Self::builtin(kodi_unit, ui_unit));
        };
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let applications: Vec<Application> = serde_json::from_str(&raw)
                    .map_err(|error| LifecycleError::Registry(error.to_string()))?;
                Self::new(applications)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::new(Self::builtin(kodi_unit, ui_unit))
            }
            Err(error) => Err(LifecycleError::Registry(error.to_string())),
        }
    }

    fn builtin(kodi_unit: &str, ui_unit: &str) -> Vec<Application> {
        vec![
            Application {
                id: "mediabox".into(),
                name: "MediaBox".into(),
                summary: Some("Filmler, diziler ve bu cihazın kitaplığı".into()),
                unit: ui_unit.to_string(),
                owns_display: true,
            },
            Application {
                id: "kodi".into(),
                name: "Kodi".into(),
                summary: Some("Donanım kod çözücüyle oynatma".into()),
                unit: kodi_unit.to_string(),
                owns_display: true,
            },
        ]
    }

    pub fn applications(&self) -> &[Application] {
        &self.applications
    }

    fn find(&self, id: &str) -> Option<&Application> {
        self.applications
            .iter()
            .find(|application| application.id == id)
    }

    pub async fn status(&self) -> DisplayStatus {
        let mut applications = Vec::with_capacity(self.applications.len());
        let mut owner = None;
        for application in self.applications.iter() {
            let (active, installed) = tokio::join!(
                unit_is_active(&application.unit),
                unit_exists(&application.unit)
            );
            if active && application.owns_display && owner.is_none() {
                owner = Some(application.id.clone());
            }
            applications.push(ApplicationStatus {
                application: application.clone(),
                installed,
                active,
            });
        }
        DisplayStatus {
            owner,
            applications,
        }
    }

    /// Put one application on the television, or release the display.
    /// Put a web address in front of the browser, then give it the display.
    ///
    /// Only plain http(s) is accepted, and only as a whole address: whatever
    /// arrives here ends up as a process argument, so a value that is not an
    /// address is refused rather than passed on.
    pub async fn browser_open(&self, url: &str) -> Result<DisplayStatus, LifecycleError> {
        let url = url.trim();
        let ok = (url.starts_with("https://") || url.starts_with("http://"))
            && url.len() > 8
            && url.len() <= 2000
            && !url.contains(char::is_whitespace)
            && !url.contains(['\'', '"', '\\', '\0']);
        if !ok {
            return Err(LifecycleError::Failed(format!("geçersiz adres: {url}")));
        }
        if let Some(parent) = std::path::Path::new(BROWSER_REQUEST).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| LifecycleError::Failed(error.to_string()))?;
        }
        tokio::fs::write(BROWSER_REQUEST, url)
            .await
            .map_err(|error| LifecycleError::Failed(error.to_string()))?;
        self.launch(BROWSER).await
    }

    pub async fn launch(&self, id: &str) -> Result<DisplayStatus, LifecycleError> {
        let _guard = self.gate.lock().await;
        let target = if id == IDLE {
            None
        } else {
            Some(
                self.find(id)
                    .ok_or_else(|| LifecycleError::UnknownApplication(id.to_string()))?,
            )
        };

        if let Some(target) = target
            && target.owns_display
            && !unit_exists(&target.unit).await
        {
            return Err(LifecycleError::Failed(format!(
                "{} kurulu değil; {} bu cihazda yok",
                target.unit, target.name
            )));
        }

        // Everything that could be holding the display goes first, including
        // the target's own stale unit, and each stop is waited out before the
        // successor is asked for.
        for application in self.applications.iter() {
            if !application.owns_display {
                continue;
            }
            if target.is_some_and(|wanted| wanted.id == application.id) {
                continue;
            }
            self.stop_unit(&application.unit).await?;
        }

        if let Some(target) = target
            && !unit_is_active(&target.unit).await
        {
            let output = systemctl(&["start", &target.unit]).await?;
            if !output.status.success() {
                return Err(LifecycleError::Failed(
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                ));
            }
        }
        Ok(self.status().await)
    }

    async fn stop_unit(&self, unit: &str) -> Result<(), LifecycleError> {
        if !unit_is_active(unit).await {
            return Ok(());
        }
        let output = systemctl(&["stop", unit]).await?;
        if !output.status.success() {
            return Err(LifecycleError::Failed(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        // `systemctl stop` returns once the unit is inactive, but the DRM
        // master handle is released by the kernel when the last fd closes.
        // A short settle keeps the successor from racing that teardown.
        tokio::time::sleep(Duration::from_millis(600)).await;
        Ok(())
    }
}

#[cfg(test)]
mod application_tests {
    use super::*;

    fn app(id: &str, unit: &str) -> Application {
        Application {
            id: id.into(),
            name: id.into(),
            summary: None,
            unit: unit.into(),
            owns_display: true,
        }
    }

    #[test]
    fn a_registry_cannot_smuggle_a_command_into_systemctl() {
        assert!(ApplicationManager::new(vec![app("kodi", "kodi.service")]).is_ok());
        assert!(ApplicationManager::new(vec![app("kodi", "kodi.service; reboot")]).is_err());
        assert!(ApplicationManager::new(vec![app("kodi", "../../bin/sh.service")]).is_err());
        assert!(ApplicationManager::new(vec![app("Kodi Ana", "kodi.service")]).is_err());
    }

    #[test]
    fn a_missing_registry_still_has_the_two_the_box_has_always_had() {
        let manager =
            ApplicationManager::load(None, "kodi.service", "mediabox-tv-ui.service").unwrap();
        let ids: Vec<&str> = manager
            .applications()
            .iter()
            .map(|application| application.id.as_str())
            .collect();
        assert_eq!(ids, ["mediabox", "kodi"]);
    }

    #[test]
    fn a_registry_file_replaces_the_builtin_table() {
        let directory = std::env::temp_dir().join(format!("mb-apps-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("applications.json");
        std::fs::write(
            &path,
            r#"[{"id":"browser","name":"Tarayıcı","unit":"mediabox-browser.service"}]"#,
        )
        .unwrap();
        let manager =
            ApplicationManager::load(Some(&path), "kodi.service", "mediabox-tv-ui.service")
                .unwrap();
        assert_eq!(manager.applications().len(), 1);
        assert_eq!(manager.applications()[0].id, "browser");
        // Absent from the file, so it takes the default: it owns the display.
        assert!(manager.applications()[0].owns_display);
        std::fs::remove_dir_all(&directory).ok();
    }
}
