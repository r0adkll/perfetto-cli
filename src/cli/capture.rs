use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;
use tokio::sync::mpsc;

use super::upload::{DEFAULT_OPEN_TIMEOUT_SECS, open_in_ui, upload_traces};
use super::{ConfigOverrides, Ctx, TraceView, UploadView, resolve_device, resolve_session};
use crate::perfetto::capture::{self, Cancel, CaptureEvent, CaptureRequest, LogLevel};

#[derive(Args)]
pub struct CaptureArgs {
    /// Session id, name, or folder slug.
    session: String,
    /// Trace filename stem (no extension). Defaults to a timestamp.
    #[arg(long, value_name = "STEM")]
    name: Option<String>,
    /// Capture on this device instead of the session's.
    #[arg(long)]
    device: Option<String>,
    /// Tag the trace (repeatable).
    #[arg(long = "tag", value_name = "TAG")]
    tags: Vec<String>,
    /// Open the trace in ui.perfetto.dev with the session's startup commands.
    #[arg(long)]
    open: bool,
    /// Upload the trace once captured.
    #[arg(long)]
    upload: bool,
    /// Cloud provider for --upload (`google_drive`, `amazon_s3`). Defaults
    /// to the provider chosen in the TUI.
    #[arg(long, requires = "upload")]
    provider: Option<String>,
    #[command(flatten)]
    overrides: ConfigOverrides,
}

#[derive(Serialize)]
struct CaptureOutput {
    session_id: i64,
    session_name: String,
    cancelled: bool,
    trace: TraceView,
    opened_url: Option<String>,
    upload: Option<UploadView>,
}

/// Run one capture against a session and register the trace. Blocks until
/// perfetto finishes; Ctrl-C stops early and keeps the partial trace (a
/// second Ctrl-C aborts outright).
pub async fn run_capture(ctx: &Ctx, args: CaptureArgs) -> Result<()> {
    let session = resolve_session(&ctx.db, &args.session)?;
    if session.is_imported {
        bail!("imported sessions can't capture new traces");
    }
    let session_id = session.id.context("session has no id")?;
    let device_serial =
        resolve_device(args.device.as_deref(), session.device_serial.as_deref()).await?;

    let mut config = session.config.clone();
    args.overrides.apply(&mut config)?;
    let custom_filename = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches(".pftrace").replace(' ', "-"));

    eprintln!(
        "Capturing {} on {device_serial} ({:.1}s, {})",
        session.package_name,
        config.duration_ms as f64 / 1000.0,
        if config.cold_start { "cold start" } else { "warm" },
    );

    let cancel = Cancel::new();
    let sigint_cancel = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("! stopping — waiting for perfetto to flush (Ctrl-C again to abort)");
            sigint_cancel.cancel();
        }
        if tokio::signal::ctrl_c().await.is_ok() {
            std::process::exit(130);
        }
    });

    let (tx, mut rx) = mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let CaptureEvent::Log(entry) = ev {
                let icon = match entry.level {
                    LogLevel::Info => "•",
                    LogLevel::Ok => "✓",
                    LogLevel::Warn => "⚠",
                    LogLevel::Err => "✗",
                };
                eprintln!("{icon} {}", entry.message);
            }
        }
    });

    let request = CaptureRequest {
        session_id,
        session_folder: session.folder_path.clone(),
        device_serial,
        package_name: session.package_name.clone(),
        config,
        custom_filename,
    };
    let result = capture::run(request, tx, cancel).await;
    let _ = printer.await;
    let result = result?;

    let trace_id = ctx.db.create_trace(
        session_id,
        &result.trace_path,
        None,
        Some(result.duration_ms),
        Some(result.size_bytes),
    )?;
    if !args.tags.is_empty() {
        let mut tags: Vec<String> = args.tags.iter().map(|t| t.trim().to_string()).collect();
        tags.retain(|t| !t.is_empty());
        tags.sort();
        tags.dedup();
        ctx.db.set_trace_tags(trace_id, &tags)?;
    }

    let opened_url = if args.open {
        Some(
            open_in_ui(
                &result.trace_path,
                &session.config.startup_commands,
                DEFAULT_OPEN_TIMEOUT_SECS,
            )
            .await?,
        )
    } else {
        None
    };

    let find_trace = || -> Result<_> {
        ctx.db
            .list_traces(session_id)?
            .into_iter()
            .find(|t| t.id == trace_id)
            .context("captured trace missing from db")
    };

    let upload = if args.upload {
        let record = find_trace()?;
        Some(upload_traces(ctx, &session, &[record], args.provider.as_deref()).await?)
    } else {
        None
    };

    let output = CaptureOutput {
        session_id,
        session_name: session.name.clone(),
        cancelled: result.cancelled,
        trace: TraceView::from(&find_trace()?),
        opened_url,
        upload,
    };
    ctx.emit(&output, || {
        let verb = if output.cancelled { "Stopped early" } else { "Captured" };
        println!(
            "{verb}: {} KB in {:.1}s",
            result.size_bytes / 1024,
            result.duration_ms as f64 / 1000.0
        );
        output.trace.print_line();
        if let Some(url) = &output.opened_url {
            println!("Opened: {url}");
        }
    })
}
