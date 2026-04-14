pub mod command_sets;
pub mod configs;
pub mod devices;
pub mod saved_queries;
pub mod sessions;
pub mod traces;

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn migrate(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch(include_str!("schema.sql"))?;

        // Add remote_url column to traces if upgrading from an older schema.
        let has_remote_url = conn
            .prepare("SELECT remote_url FROM traces LIMIT 0")
            .is_ok();
        if !has_remote_url {
            conn.execute_batch("ALTER TABLE traces ADD COLUMN remote_url TEXT")?;
        }

        // Columns added when the Macrobenchmark import feature landed. Older
        // databases predate these and need the ALTERs; new ones already have
        // them from schema.sql.
        let has_is_imported = conn
            .prepare("SELECT is_imported FROM sessions LIMIT 0")
            .is_ok();
        if !has_is_imported {
            conn.execute_batch(
                "ALTER TABLE sessions ADD COLUMN is_imported INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN benchmark_json_path TEXT;
                 ALTER TABLE sessions ADD COLUMN import_source_dir TEXT;",
            )?;
        }

        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::{OptionalExtension, params};
        let conn = self.lock();
        let val = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(val)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        use rusqlite::params;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        use rusqlite::params;
        let conn = self.lock();
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("db mutex poisoned")
    }

    /// Test-only constructor: wrap an existing `Arc<Mutex<Connection>>`
    /// (typically from `Connection::open_in_memory()`) so DAO tests can
    /// exercise `impl Database` methods without hitting the disk.
    #[cfg(test)]
    pub(crate) fn from_connection(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}
