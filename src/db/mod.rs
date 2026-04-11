pub mod devices;
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
        Ok(())
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("db mutex poisoned")
    }
}
