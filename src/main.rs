mod adb;
mod app;
mod config;
mod db;
mod perfetto;
mod session;
mod tui;
mod ui_server;

use anyhow::Result;
use clap::Parser;
use tracing_appender::non_blocking::WorkerGuard;

#[derive(Parser)]
#[command(name = "perfetto-cli", about = "TUI for managing Android perfetto trace sessions")]
struct Cli {}

#[tokio::main]
async fn main() -> Result<()> {
    let _cli = Cli::parse();

    let paths = config::Paths::resolve()?;
    paths.ensure()?;

    let _log_guard = init_tracing(&paths)?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "perfetto-cli starting");

    let db = db::Database::open(&paths.db_file())?;
    db.migrate()?;

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
