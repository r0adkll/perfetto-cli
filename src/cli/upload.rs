use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

use super::{Ctx, UploadView, resolve_command_sets, resolve_session};
use crate::cloud::{self, UploadProgress};
use crate::db::traces::TraceRecord;
use crate::perfetto::capture::Cancel;
use crate::perfetto::commands::StartupCommand;
use crate::session::Session;
use crate::ui_server::UiServer;

pub const DEFAULT_OPEN_TIMEOUT_SECS: u64 = 60;

#[derive(Args)]
pub struct UploadArgs {
    /// Session id, name, or folder slug.
    session: String,
    /// Trace id to upload (repeatable). Defaults to every trace in the session.
    #[arg(long = "trace", value_name = "ID", conflicts_with = "latest")]
    traces: Vec<i64>,
    /// Upload only the most recent trace.
    #[arg(long)]
    latest: bool,
    /// Cloud provider (`google_drive`, `amazon_s3`). Defaults to the
    /// provider chosen in the TUI.
    #[arg(long)]
    provider: Option<String>,
}

#[derive(Args)]
pub struct OpenArgs {
    /// Trace id, or a path to any `.pftrace` file.
    trace: String,
    /// Use these saved startup command sets instead of the session's
    /// commands (repeatable; merged in `command-sets` order).
    #[arg(long = "commands", value_name = "NAME")]
    commands: Vec<String>,
    /// Seconds to wait for the browser to fetch the trace.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_OPEN_TIMEOUT_SECS)]
    timeout: u64,
}

pub async fn run_upload(ctx: &Ctx, args: UploadArgs) -> Result<()> {
    let session = resolve_session(&ctx.db, &args.session)?;
    let all = ctx.db.list_traces(session.id.context("session has no id")?)?;

    // `list_traces` is newest-first, so `--latest` is the head.
    let selected: Vec<TraceRecord> = if args.latest {
        all.into_iter().take(1).collect()
    } else if args.traces.is_empty() {
        all
    } else {
        args.traces
            .iter()
            .map(|id| {
                all.iter()
                    .find(|t| t.id == *id)
                    .cloned()
                    .with_context(|| format!("trace #{id} is not in session '{}'", session.name))
            })
            .collect::<Result<_>>()?
    };
    if selected.is_empty() {
        bail!("session '{}' has no traces to upload", session.name);
    }

    let view = upload_traces(ctx, &session, &selected, args.provider.as_deref()).await?;
    ctx.emit(&view, || view.print())
}

/// Authenticate (if needed) and upload `traces`, logging progress to
/// stderr. Mirrors the TUI's `App::initiate_upload` → `start_upload` flow.
pub(super) async fn upload_traces(
    ctx: &Ctx,
    session: &Session,
    traces: &[TraceRecord],
    provider_id: Option<&str>,
) -> Result<UploadView> {
    let provider = match provider_id {
        Some(id) => cloud::provider_by_id(id).with_context(|| {
            let ids: Vec<String> = cloud::all_providers()
                .iter()
                .map(|p| p.id().to_string())
                .collect();
            format!("unknown provider '{id}' (expected one of: {})", ids.join(", "))
        })?,
        None => cloud::default_provider(&ctx.db),
    };

    if !provider.is_authenticated(&ctx.db).await {
        eprintln!("• Authenticating with {}…", provider.name());
        provider
            .authenticate(&ctx.db)
            .await
            .with_context(|| format!("{} authentication failed", provider.name()))?;
    }

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<UploadProgress>();
    let printer = tokio::spawn(async move {
        let mut announced = None;
        while let Some(p) = progress_rx.recv().await {
            if announced != Some(p.file_index) {
                announced = Some(p.file_index);
                eprintln!(
                    "• Uploading {} ({}/{}, {} KB)",
                    p.file_name,
                    p.file_index + 1,
                    p.total_files,
                    p.total_bytes / 1024
                );
            }
        }
    });

    let cancel = Cancel::new();
    let result = cloud::upload::upload_traces(
        provider.as_ref(),
        &ctx.db,
        session,
        traces,
        &progress_tx,
        &cancel,
    )
    .await;
    drop(progress_tx);
    let _ = printer.await;
    let result = result?;
    eprintln!("✓ Uploaded {} trace(s) to {}", result.traces.len(), provider.name());

    Ok(UploadView::new(provider.name(), result))
}

pub async fn run_open(ctx: &Ctx, args: OpenArgs) -> Result<()> {
    #[derive(Serialize)]
    struct Opened {
        path: String,
        url: String,
    }

    let (path, session_commands) = resolve_trace_target(ctx, &args.trace)?;
    let commands = if args.commands.is_empty() {
        session_commands
    } else {
        resolve_command_sets(&ctx.db, &args.commands)?
    };

    let url = open_in_ui(&path, &commands, args.timeout).await?;
    let opened = Opened {
        path: path.display().to_string(),
        url,
    };
    ctx.emit(&opened, || println!("Opened {} → {}", opened.path, opened.url))
}

/// Resolve a trace id (or a filesystem path) to the file and the startup
/// commands of the session that owns it. Paths that aren't registered in the
/// DB get no commands.
fn resolve_trace_target(ctx: &Ctx, target: &str) -> Result<(PathBuf, Vec<StartupCommand>)> {
    let as_path = PathBuf::from(target);
    let id = target.parse::<i64>().ok().filter(|_| !as_path.exists());

    for session in ctx.db.list_sessions()? {
        let Some(sid) = session.id else { continue };
        let hit = ctx.db.list_traces(sid)?.into_iter().find(|t| match id {
            Some(id) => t.id == id,
            None => same_file(&t.file_path, &as_path),
        });
        if let Some(t) = hit {
            return Ok((t.file_path, session.config.startup_commands));
        }
    }

    match id {
        Some(id) => bail!("no trace with id #{id}"),
        None if as_path.is_file() => Ok((as_path, Vec::new())),
        None => bail!("{target} is neither a trace id nor an existing file"),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Serve `path` to ui.perfetto.dev and block until the browser has fetched
/// it (the server shuts itself down after one successful GET).
pub(super) async fn open_in_ui(
    path: &Path,
    commands: &[StartupCommand],
    timeout_secs: u64,
) -> Result<String> {
    let server = UiServer::start()?;
    let url = server.serve(path, commands)?;
    eprintln!("• Waiting for ui.perfetto.dev to load the trace…");

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while server.is_alive() {
        if Instant::now() >= deadline {
            bail!("browser did not fetch the trace within {timeout_secs}s ({url})");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    server.join();
    Ok(url)
}
