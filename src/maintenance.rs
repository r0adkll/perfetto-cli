use crate::config::Paths;
use anyhow::{Context, Result};
use std::io::{self, Write};

/// Remove the SQLite index and all session folders. Themes and logs are
/// preserved — themes may contain user-authored files, logs are diagnostic.
pub fn clear_cache(paths: &Paths, skip_confirm: bool) -> Result<()> {
    let db = paths.db_file();
    let sessions = paths.sessions_dir();

    let db_exists = db.exists();
    let sessions_exists = sessions.exists();

    if !db_exists && !sessions_exists {
        println!("Nothing to clear — no database or sessions found at {}.", paths.config_dir.display());
        return Ok(());
    }

    println!("This will permanently delete:");
    if db_exists {
        println!("  • {}", db.display());
    }
    if sessions_exists {
        println!("  • {} (all captured traces)", sessions.display());
    }
    println!("Themes and logs will be preserved.");

    if !skip_confirm && !prompt_yes("Continue? Type 'yes' to confirm: ")? {
        println!("Aborted.");
        return Ok(());
    }

    if sessions_exists {
        std::fs::remove_dir_all(&sessions)
            .with_context(|| format!("failed to remove {}", sessions.display()))?;
    }
    if db_exists {
        std::fs::remove_file(&db)
            .with_context(|| format!("failed to remove {}", db.display()))?;
    }

    // Recreate the empty sessions dir so subsequent launches have a clean slate.
    std::fs::create_dir_all(paths.sessions_dir())?;

    println!("Cleared.");
    Ok(())
}

fn prompt_yes(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("yes"))
}
