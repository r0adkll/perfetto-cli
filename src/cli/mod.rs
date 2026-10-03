//! Headless subcommands for scripted / agentic use.
//!
//! Every command prints human-readable output by default and a single JSON
//! document on stdout with `--json`. Progress and capture logs always go to
//! stderr so stdout stays machine-parseable. Sessions are addressed by id,
//! exact name (case-insensitive), or folder slug — see `resolve_session`.

mod capture;
mod resources;
mod session;
mod skills;
mod upload;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::Args;
use serde::Serialize;

use crate::adb::{self, DeviceState};
use crate::cloud::UploadResult;
use crate::config::Paths;
use crate::db::Database;
use crate::db::command_sets::{
    SavedCommandSet, command_sets_matching_commands, merge_selected_command_sets,
};
use crate::db::traces::{TraceRecord, UploadLinks};
use crate::perfetto::TraceConfig;
use crate::perfetto::commands::StartupCommand;
use crate::session::Session;

pub use capture::{CaptureArgs, run_capture};
pub use resources::{list_command_sets, list_configs, list_devices};
pub use session::{SessionCommand, run_session};
pub use skills::{SkillsCommand, run_skills};
pub use upload::{OpenArgs, UploadArgs, run_open, run_upload};

/// Shared state every subcommand needs.
pub struct Ctx {
    pub db: Database,
    pub paths: Paths,
    pub json: bool,
}

impl Ctx {
    /// Print `value` as pretty JSON in `--json` mode, otherwise run `human`.
    fn emit<T: Serialize>(&self, value: &T, human: impl FnOnce()) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(value)?);
        } else {
            human();
        }
        Ok(())
    }
}

/// Per-invocation tweaks to a session's `TraceConfig`. Persisted by
/// `session create` / `session update`, ephemeral on `capture`.
#[derive(Args, Debug, Default)]
pub struct ConfigOverrides {
    /// Capture duration in seconds.
    #[arg(long, value_name = "SECS")]
    duration: Option<f64>,
    /// Force-stop and relaunch the app inside the trace (cold start).
    #[arg(long, conflicts_with = "warm")]
    cold: bool,
    /// Trace the app as-is without relaunching it.
    #[arg(long)]
    warm: bool,
    /// Activity to launch on cold start (`.MainActivity` or `pkg/class`).
    #[arg(long, value_name = "ACTIVITY")]
    launch_activity: Option<String>,
}

impl ConfigOverrides {
    fn apply(&self, cfg: &mut TraceConfig) -> Result<()> {
        if let Some(secs) = self.duration {
            if secs.is_nan() || secs <= 0.0 {
                bail!("--duration must be positive");
            }
            cfg.duration_ms = (secs * 1000.0).round() as u32;
        }
        if self.cold {
            cfg.cold_start = true;
        }
        if self.warm {
            cfg.cold_start = false;
        }
        if let Some(activity) = &self.launch_activity {
            let trimmed = activity.trim();
            cfg.launch_activity = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
        Ok(())
    }
}

/// Find a session by numeric id, exact name (case-insensitive), or folder
/// slug. Errors on no match or an ambiguous name.
fn resolve_session(db: &Database, selector: &str) -> Result<Session> {
    let sessions = db.list_sessions()?;
    if let Ok(id) = selector.parse::<i64>()
        && let Some(s) = sessions.iter().find(|s| s.id == Some(id))
    {
        return Ok(s.clone());
    }
    let matches: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            s.name.eq_ignore_ascii_case(selector)
                || s.folder_path.file_name().is_some_and(|f| f == selector)
        })
        .collect();
    match matches.as_slice() {
        [] => bail!("no session matching '{selector}' (see `perfetto-cli session list`)"),
        [one] => Ok((*one).clone()),
        many => {
            let ids: Vec<String> = many
                .iter()
                .filter_map(|s| s.id.map(|id| format!("#{id}")))
                .collect();
            bail!(
                "'{selector}' matches multiple sessions ({}); pass the id instead",
                ids.join(", ")
            )
        }
    }
}

/// Merge the named saved command sets into one command list. Sets are
/// concatenated in the TUI's display order (not argument order) so the
/// session detail picker can recover the selection afterwards.
fn resolve_command_sets(db: &Database, names: &[String]) -> Result<Vec<StartupCommand>> {
    let sets = db.list_command_sets()?;
    let mut selected = vec![false; sets.len()];
    for name in names {
        let idx = sets
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
            .with_context(|| {
                format!("no startup command set named '{name}' (see `perfetto-cli command-sets`)")
            })?;
        selected[idx] = true;
    }
    Ok(merge_selected_command_sets(&sets, &selected))
}

/// Pick the device a capture or new session should use: the explicit
/// `--device`, else `fallback` (the session's stored device), else the only
/// online device. The chosen device must be online.
async fn resolve_device(explicit: Option<&str>, fallback: Option<&str>) -> Result<String> {
    let online: Vec<adb::Device> = adb::list_live_devices()
        .await?
        .into_iter()
        .filter(|d| d.state == DeviceState::Online)
        .collect();
    let online_serials = || {
        if online.is_empty() {
            "none".to_string()
        } else {
            online.iter().map(|d| d.serial.as_str()).collect::<Vec<_>>().join(", ")
        }
    };

    if let Some(serial) = explicit.or(fallback) {
        if online.iter().any(|d| d.serial == serial) {
            return Ok(serial.to_string());
        }
        bail!(
            "device {serial} is not online (online: {}); pass --device to pick another",
            online_serials()
        );
    }
    match online.as_slice() {
        [one] => Ok(one.serial.clone()),
        [] => bail!("no online devices — connect one and check `adb devices`"),
        _ => bail!(
            "multiple devices online ({}); pass --device <SERIAL>",
            online_serials()
        ),
    }
}

// ---------------------------------------------------------------------------
// JSON views
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SessionView {
    id: i64,
    name: String,
    package: String,
    device: Option<String>,
    folder: String,
    created_at: DateTime<Utc>,
    imported: bool,
    duration_ms: u32,
    cold_start: bool,
    launch_activity: Option<String>,
    startup_commands: Vec<StartupCommand>,
    /// Saved sets whose concatenation equals `startup_commands`; empty when
    /// the commands don't map exactly onto saved sets.
    command_sets: Vec<String>,
    trace_count: usize,
}

impl SessionView {
    fn new(s: &Session, trace_count: usize, sets: &[SavedCommandSet]) -> Self {
        let command_sets = command_sets_matching_commands(sets, &s.config.startup_commands)
            .into_iter()
            .zip(sets)
            .filter(|(selected, _)| *selected)
            .map(|(_, set)| set.name.clone())
            .collect();
        Self {
            id: s.id.unwrap_or_default(),
            name: s.name.clone(),
            package: s.package_name.clone(),
            device: s.device_serial.clone(),
            folder: s.folder_path.display().to_string(),
            created_at: s.created_at,
            imported: s.is_imported,
            duration_ms: s.config.duration_ms,
            cold_start: s.config.cold_start,
            launch_activity: s.config.launch_activity.clone(),
            startup_commands: s.config.startup_commands.clone(),
            command_sets,
            trace_count,
        }
    }

    fn print_line(&self) {
        println!(
            "#{:<4} {}  [{}]  device={}  traces={}",
            self.id,
            self.name,
            self.package,
            self.device.as_deref().unwrap_or("-"),
            self.trace_count,
        );
    }

    fn print_detail(&self) {
        self.print_line();
        println!("  folder:    {}", self.folder);
        println!(
            "  capture:   {:.1}s {}{}",
            self.duration_ms as f64 / 1000.0,
            if self.cold_start { "cold start" } else { "warm" },
            self.launch_activity
                .as_deref()
                .map(|a| format!(" ({a})"))
                .unwrap_or_default(),
        );
        let sets = if self.command_sets.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.command_sets.join(" + "))
        };
        println!(
            "  commands:  {} startup command(s){sets}",
            self.startup_commands.len()
        );
    }
}

#[derive(Serialize)]
struct TraceView {
    id: i64,
    session_id: i64,
    path: String,
    label: Option<String>,
    duration_ms: Option<u64>,
    size_bytes: Option<u64>,
    captured_at: DateTime<Utc>,
    tags: Vec<String>,
    uploads: UploadLinks,
}

impl From<&TraceRecord> for TraceView {
    fn from(t: &TraceRecord) -> Self {
        Self {
            id: t.id,
            session_id: t.session_id,
            path: t.file_path.display().to_string(),
            label: t.label.clone(),
            duration_ms: t.duration_ms,
            size_bytes: t.size_bytes,
            captured_at: t.captured_at,
            tags: t.tags.clone(),
            uploads: t.uploads.clone(),
        }
    }
}

impl TraceView {
    fn print_line(&self) {
        let size = self
            .size_bytes
            .map(|b| format!("{} KB", b / 1024))
            .unwrap_or_else(|| "?".into());
        let tags = if self.tags.is_empty() {
            String::new()
        } else {
            format!("  tags={}", self.tags.join(","))
        };
        println!("  #{:<5} {}  {size}{tags}", self.id, self.path);
        for (provider, url) in &self.uploads {
            println!("         {provider}: {url}");
        }
    }
}

#[derive(Serialize)]
struct UploadView {
    provider: String,
    folder_url: Option<String>,
    traces: Vec<UploadedTrace>,
}

#[derive(Serialize)]
struct UploadedTrace {
    id: i64,
    url: Option<String>,
}

impl UploadView {
    fn new(provider: &str, result: UploadResult) -> Self {
        Self {
            provider: provider.to_string(),
            folder_url: result.folder_url,
            traces: result
                .traces
                .into_iter()
                .map(|(id, url)| UploadedTrace { id, url })
                .collect(),
        }
    }

    fn print(&self) {
        println!("Uploaded {} trace(s) to {}", self.traces.len(), self.provider);
        for t in &self.traces {
            println!("  #{:<5} {}", t.id, t.url.as_deref().unwrap_or("(no link)"));
        }
        if let Some(url) = &self.folder_url {
            println!("  folder: {url}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db_with(names: &[&str]) -> Database {
        let db = Database::open(Path::new(":memory:")).unwrap();
        db.migrate().unwrap();
        for name in names {
            let session = Session {
                id: None,
                name: name.to_string(),
                package_name: "com.example".into(),
                device_serial: None,
                config: TraceConfig::default(),
                folder_path: Path::new("/sessions").join(crate::session::slugify(name)),
                created_at: Utc::now(),
                notes: None,
                is_imported: false,
                benchmark_json_path: None,
                import_source_dir: None,
            };
            db.create_session(&session).unwrap();
        }
        db
    }

    #[test]
    fn resolve_by_id_name_and_slug() {
        let db = db_with(&["Cold Start", "Scroll"]);
        assert_eq!(resolve_session(&db, "1").unwrap().name, "Cold Start");
        assert_eq!(resolve_session(&db, "cold start").unwrap().name, "Cold Start");
        assert_eq!(resolve_session(&db, "cold-start").unwrap().name, "Cold Start");
        assert!(resolve_session(&db, "missing").is_err());
    }

    #[test]
    fn resolve_numeric_name_when_no_id_matches() {
        let db = db_with(&["2024"]);
        assert_eq!(resolve_session(&db, "2024").unwrap().name, "2024");
    }

    #[test]
    fn resolve_ambiguous_name_errors() {
        let db = db_with(&["Run", "run"]);
        let err = resolve_session(&db, "RUN").unwrap_err().to_string();
        assert!(err.contains("multiple sessions"), "{err}");
    }

    #[test]
    fn command_sets_merge_in_display_order_and_round_trip() {
        let db = db_with(&[]);
        let cmd = |id: &str| StartupCommand {
            id: id.into(),
            args: Vec::new(),
        };
        db.create_command_set("Tracks", &[cmd("pin")]).unwrap();
        db.create_command_set("Queries", &[cmd("query")]).unwrap();
        // Bump "Tracks" so it sorts first (list order is updated_at DESC).
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.update_command_set(1, &[cmd("pin")]).unwrap();

        let merged =
            resolve_command_sets(&db, &["queries".into(), "TRACKS".into()]).unwrap();
        assert_eq!(merged, vec![cmd("pin"), cmd("query")]);
        assert!(resolve_command_sets(&db, &["nope".into()]).is_err());

        let session = Session {
            id: Some(1),
            name: "s".into(),
            package_name: "p".into(),
            device_serial: None,
            config: TraceConfig {
                startup_commands: merged,
                ..TraceConfig::default()
            },
            folder_path: "/s".into(),
            created_at: Utc::now(),
            notes: None,
            is_imported: false,
            benchmark_json_path: None,
            import_source_dir: None,
        };
        let view = SessionView::new(&session, 0, &db.list_command_sets().unwrap());
        assert_eq!(view.command_sets, vec!["Tracks", "Queries"]);
    }

    #[test]
    fn overrides_apply() {
        let mut cfg = TraceConfig::default();
        let o = ConfigOverrides {
            duration: Some(2.5),
            cold: true,
            launch_activity: Some(" .Main ".into()),
            ..Default::default()
        };
        o.apply(&mut cfg).unwrap();
        assert_eq!(cfg.duration_ms, 2500);
        assert!(cfg.cold_start);
        assert_eq!(cfg.launch_activity.as_deref(), Some(".Main"));

        let o = ConfigOverrides {
            warm: true,
            launch_activity: Some(String::new()),
            ..Default::default()
        };
        o.apply(&mut cfg).unwrap();
        assert!(!cfg.cold_start);
        assert_eq!(cfg.launch_activity, None);

        let bad = ConfigOverrides {
            duration: Some(0.0),
            ..Default::default()
        };
        assert!(bad.apply(&mut cfg).is_err());
    }
}
