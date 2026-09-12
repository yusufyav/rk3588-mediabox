use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;

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
        let valid = unit.ends_with(".service")
            && !unit.is_empty()
            && unit.len() <= 64
            && unit
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"@._-".contains(&b));
        if !valid {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_validation_blocks_command_injection() {
        assert!(KodiLifecycle::new("kodi.service").is_ok());
        assert!(KodiLifecycle::new("kodi.service;reboot").is_err());
        assert!(KodiLifecycle::new("../../bin/sh.service").is_err());
    }
}
