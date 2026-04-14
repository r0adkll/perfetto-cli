mod adb;
mod app;
mod cloud;
mod config;
mod db;
mod maintenance;
mod perfetto;
mod session;
mod tui;
mod ui_server;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let paths = config::Paths::resolve()?;
    paths.ensure()?;

    if let Some(Command::Clear { yes }) = cli.command {
        return maintenance::clear_cache(&paths, yes);
    }

    let _log_guard = init_tracing(&paths)?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "perfetto-cli starting");

    let db = db::Database::open(&paths.db_file())?;
    db.migrate()?;

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
