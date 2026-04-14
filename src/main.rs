mod adb;
mod app;
mod cloud;
mod config;
mod db;
mod import;
mod maintenance;
mod perfetto;
mod session;
mod trace_processor;
mod tui;
mod ui_server;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_appender::non_blocking::WorkerGuard;

#[derive(Parser)]
#[command(name = "perfetto-cli", about = "TUI for managing Android perfetto trace sessions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Remove all cached sessions, traces, and the local database.
    Clear {
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        yes: bool,
    },
    /// Import an Android Macrobenchmark output directory. Creates one
    /// read-only session per `@Test` method, copying the benchmarkData JSON
    /// and every matching iteration trace into its session folder.
    Import {
        /// Path to the directory containing `*-benchmarkData.json` and
        /// `*_iter*.perfetto-trace` files (typically under
        /// `build/outputs/connected_android_test_additional_output/...`).
        dir: PathBuf,
        /// Optional prefix for generated session names (e.g. a run ID).
        #[arg(long)]
        name: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let paths = config::Paths::resolve()?;
    paths.ensure()?;

    // `clear` runs before tracing init so we don't rotate a log file just to
    // delete the DB next to it.
    if let Some(Command::Clear { yes }) = cli.command {
        return maintenance::clear_cache(&paths, yes);
    }

    let _log_guard = init_tracing(&paths)?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "perfetto-cli starting");

    let db = db::Database::open(&paths.db_file())?;
    db.migrate()?;

    match cli.command {
        Some(Command::Import { dir, name }) => run_import(&db, &paths, &dir, name.as_deref()),
        Some(Command::Clear { .. }) => unreachable!("handled above"),
        None => run_tui(db, paths).await,
    }
}

fn run_import(
    db: &db::Database,
    paths: &config::Paths,
    dir: &std::path::Path,
    name: Option<&str>,
) -> Result<()> {
    let outcomes = import::import_directory(db, paths, dir, name)?;
    println!("Imported {} session(s) from {}", outcomes.len(), dir.display());
    for o in &outcomes {
        println!(
            "  #{:<5} {}  ({} trace{})  → {}",
            o.session_id,
            o.session_name,
            o.trace_count,
            if o.trace_count == 1 { "" } else { "s" },
            o.folder_path.display(),
        );
    }
    Ok(())
}

async fn run_tui(db: db::Database, paths: config::Paths) -> Result<()> {
    // Write the embedded Perfetto theme to disk and activate the persisted
    // (or default) theme before the TUI renders its first frame.
    config::ensure_default_theme(&paths);
    let saved_theme = db.get_setting("theme").ok().flatten();
    config::init_theme(&paths, saved_theme.as_deref());

    let mut terminal = tui::init()?;
    let result = app::App::new(db, paths).run(&mut terminal).await;
    tui::restore()?;
    result
}

fn init_tracing(paths: &config::Paths) -> Result<WorkerGuard> {
    use tracing_subscriber::{EnvFilter, fmt};
    let appender = tracing_appender::rolling::daily(paths.log_dir(), "perfetto-cli.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    Ok(guard)
}
