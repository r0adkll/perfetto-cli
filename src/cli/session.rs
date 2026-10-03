use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{
    ConfigOverrides, Ctx, SessionView, TraceView, resolve_command_sets, resolve_device,
    resolve_session,
};
use crate::db::Database;
use crate::perfetto::TraceConfig;
use crate::session::Session;

#[derive(Subcommand)]
pub enum SessionCommand {
    /// List all sessions, newest first.
    List,
    /// Show one session and its traces.
    Show {
        /// Session id, name, or folder slug.
        session: String,
    },
    /// Create a session. Defaults to the only online device when --device
    /// is omitted.
    Create(CreateArgs),
    /// Change a session's package, device, startup commands, or capture
    /// settings.
    Update(UpdateArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// Session name.
    #[arg(long)]
    name: String,
    /// Target app package, e.g. `com.example.app`.
    #[arg(long)]
    package: String,
    /// adb serial of the device to capture on.
    #[arg(long)]
    device: Option<String>,
    /// Saved trace config to start from (see `perfetto-cli configs`).
    #[arg(long, value_name = "NAME")]
    config: Option<String>,
    /// Saved startup command set to attach (repeatable; sets merge in the
    /// order shown by `perfetto-cli command-sets`).
    #[arg(long = "commands", value_name = "NAME")]
    commands: Vec<String>,
    /// Return the existing session with this name instead of failing.
    #[arg(long)]
    if_not_exists: bool,
    #[command(flatten)]
    overrides: ConfigOverrides,
}

#[derive(Args)]
pub struct UpdateArgs {
    /// Session id, name, or folder slug.
    session: String,
    /// Change the target app package.
    #[arg(long)]
    package: Option<String>,
    /// adb serial of the device to capture on.
    #[arg(long)]
    device: Option<String>,
    /// Replace the startup commands with saved command sets (repeatable;
    /// sets merge in the order shown by `perfetto-cli command-sets`).
    #[arg(long = "commands", value_name = "NAME", conflicts_with = "clear_commands")]
    commands: Vec<String>,
    /// Remove all startup commands.
    #[arg(long)]
    clear_commands: bool,
    #[command(flatten)]
    overrides: ConfigOverrides,
}

pub async fn run_session(ctx: &Ctx, cmd: SessionCommand) -> Result<()> {
    match cmd {
        SessionCommand::List => list(ctx),
        SessionCommand::Show { session } => show(ctx, &session),
        SessionCommand::Create(args) => create(ctx, args).await,
        SessionCommand::Update(args) => update(ctx, args).await,
    }
}

fn session_view(db: &Database, s: &Session) -> Result<SessionView> {
    let count = match s.id {
        Some(id) => db.list_traces(id)?.len(),
        None => 0,
    };
    Ok(SessionView::new(s, count, &db.list_command_sets()?))
}

fn list(ctx: &Ctx) -> Result<()> {
    let views = ctx
        .db
        .list_sessions()?
        .iter()
        .map(|s| session_view(&ctx.db, s))
        .collect::<Result<Vec<_>>>()?;
    ctx.emit(&views, || {
        if views.is_empty() {
            println!("No sessions.");
        }
        views.iter().for_each(SessionView::print_line);
    })
}

fn show(ctx: &Ctx, selector: &str) -> Result<()> {
    #[derive(Serialize)]
    struct Detail {
        session: SessionView,
        traces: Vec<TraceView>,
    }

    let session = resolve_session(&ctx.db, selector)?;
    let traces = ctx.db.list_traces(session.id.unwrap_or_default())?;
    let detail = Detail {
        session: session_view(&ctx.db, &session)?,
        traces: traces.iter().map(TraceView::from).collect(),
    };
    ctx.emit(&detail, || {
        detail.session.print_detail();
        detail.traces.iter().for_each(TraceView::print_line);
    })
}

async fn create(ctx: &Ctx, args: CreateArgs) -> Result<()> {
    let name = args.name.trim();
    let package = args.package.trim();
    if name.is_empty() || package.is_empty() {
        bail!("--name and --package must be non-empty");
    }

    if let Some(existing) = ctx
        .db
        .list_sessions()?
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
    {
        if !args.if_not_exists {
            bail!(
                "session '{}' already exists (#{}); pass --if-not-exists to reuse it",
                existing.name,
                existing.id.unwrap_or_default()
            );
        }
        if existing.package_name != package {
            bail!(
                "session '{}' exists but targets {}, not {package}",
                existing.name,
                existing.package_name
            );
        }
        let view = session_view(&ctx.db, &existing)?;
        return ctx.emit(&view, || view.print_detail());
    }

    let mut config = match &args.config {
        Some(config_name) => saved_config(&ctx.db, config_name)?,
        None => TraceConfig::default(),
    };
    if !args.commands.is_empty() {
        config.startup_commands = resolve_command_sets(&ctx.db, &args.commands)?;
    }
    args.overrides.apply(&mut config)?;

    let serial = resolve_device(args.device.as_deref(), None).await?;
    ctx.db.upsert_device_seen(&serial, None)?;

    let mut session = Session {
        id: None,
        name: name.to_string(),
        package_name: package.to_string(),
        device_serial: Some(serial),
        config,
        folder_path: Session::unique_folder_path(&ctx.paths.sessions_dir(), name),
        created_at: Utc::now(),
        notes: None,
        is_imported: false,
        benchmark_json_path: None,
        import_source_dir: None,
    };
    session.ensure_filesystem().context("create session folder")?;
    session.id = Some(ctx.db.create_session(&session)?);

    let view = session_view(&ctx.db, &session)?;
    ctx.emit(&view, || {
        println!("Created session:");
        view.print_detail();
    })
}

async fn update(ctx: &Ctx, args: UpdateArgs) -> Result<()> {
    let mut session = resolve_session(&ctx.db, &args.session)?;
    if session.is_imported {
        bail!("imported sessions are read-only");
    }
    let id = session.id.context("session has no id")?;

    if let Some(package) = &args.package {
        let package = package.trim();
        if package.is_empty() {
            bail!("--package must be non-empty");
        }
        session.package_name = package.to_string();
    }

    if let Some(serial) = args.device.as_deref() {
        let serial = resolve_device(Some(serial), None).await?;
        ctx.db.upsert_device_seen(&serial, None)?;
        ctx.db.update_session_device(id, &serial)?;
        session.device_serial = Some(serial);
    }

    if !args.commands.is_empty() {
        session.config.startup_commands = resolve_command_sets(&ctx.db, &args.commands)?;
    }
    if args.clear_commands {
        session.config.startup_commands.clear();
    }
    args.overrides.apply(&mut session.config)?;
    ctx.db
        .update_session(id, &session.package_name, &session.config)?;
    session.ensure_filesystem()?;

    let view = session_view(&ctx.db, &session)?;
    ctx.emit(&view, || {
        println!("Updated session:");
        view.print_detail();
    })
}

fn saved_config(db: &Database, name: &str) -> Result<TraceConfig> {
    db.list_configs()?
        .into_iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
        .map(|c| c.config)
        .with_context(|| format!("no saved config named '{name}' (see `perfetto-cli configs`)"))
}
