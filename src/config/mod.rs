use anyhow::{Context, Result};
use std::path::PathBuf;

/// Resolved filesystem locations for perfetto-cli state.
///
/// Per the project spec, everything lives under `~/.config/perfetto-cli/` on
/// every platform — not the OS-specific config dir that `directories` would
/// otherwise give us.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let base = directories::BaseDirs::new()
            .context("failed to resolve home directory")?;
        Ok(Self {
            config_dir: base.home_dir().join(".config").join("perfetto-cli"),
        })
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(self.sessions_dir())?;
        std::fs::create_dir_all(self.log_dir())?;
        Ok(())
    }

    pub fn db_file(&self) -> PathBuf {
        self.config_dir.join("perfetto-cli.db")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.config_dir.join("sessions")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.config_dir.join("logs")
    }
}
