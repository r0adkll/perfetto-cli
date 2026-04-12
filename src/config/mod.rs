use anyhow::{Context, Result};
use std::path::PathBuf;

/// Embedded default theme — always written to the themes dir so the opaline
/// discovery system finds it alongside user-supplied custom themes.
const DEFAULT_THEME_TOML: &str = include_str!("../../assets/perfetto.toml");

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
        std::fs::create_dir_all(self.themes_dir())?;
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

    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }
}

/// Write the embedded Perfetto theme to the themes directory so opaline's
/// discovery finds it. Always overwrites so version upgrades propagate
/// color changes.
pub fn ensure_default_theme(paths: &Paths) {
    let dest = paths.themes_dir().join("perfetto.toml");
    if let Err(e) = std::fs::write(&dest, DEFAULT_THEME_TOML) {
        tracing::warn!(?e, "failed to write default theme");
    }
}

/// Initialize the opaline global theme. Tries the persisted choice first,
/// falls back to the embedded Perfetto theme.
pub fn init_theme(paths: &Paths, persisted_name: Option<&str>) {
    // Try the persisted theme name first.
    if let Some(name) = persisted_name {
        if let Ok(()) =
            opaline::load_theme_by_name_in_dirs(name, vec![paths.themes_dir()])
        {
            return;
        }
        tracing::warn!(name, "persisted theme not found, falling back to default");
    }

    // Fall back to the embedded default.
    match opaline::load_from_str(DEFAULT_THEME_TOML, None) {
        Ok(theme) => opaline::set_theme(theme),
        Err(e) => tracing::error!(?e, "failed to load embedded Perfetto theme"),
    }
}
