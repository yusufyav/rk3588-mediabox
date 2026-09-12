use mediabox_core::{Surface, SurfaceStatus};
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::Mutex;

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("Kodi unit adı geçersiz")]
    InvalidUnit,
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
