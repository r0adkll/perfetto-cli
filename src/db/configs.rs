use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;

use super::Database;
use crate::perfetto::TraceConfig;

#[derive(Debug, Clone)]
pub struct SavedConfig {
    pub id: i64,
    pub name: String,
    pub config: TraceConfig,
}

impl Database {
    pub fn create_config(&self, name: &str, config: &TraceConfig) -> Result<i64> {
        let conn = self.lock();
        let json = serde_json::to_string(config)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO configs (name, config_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![name, json, now, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_config(&self, id: i64, config: &TraceConfig) -> Result<()> {
        let conn = self.lock();
        let json = serde_json::to_string(config)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE configs SET config_json = ?1, updated_at = ?2 WHERE id = ?3",
            params![json, now, id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn rename_config(&self, id: i64, name: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE configs SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    pub fn list_configs(&self) -> Result<Vec<SavedConfig>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, config_json FROM configs ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, name, json) = r?;
            let mut config: TraceConfig =
                serde_json::from_str(&json).context("deserialize saved config")?;
            config.migrate_legacy();
            out.push(SavedConfig { id, name, config });
        }
        Ok(out)
    }

    pub fn delete_config(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM configs WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn duplicate_config(&self, id: i64, new_name: &str) -> Result<i64> {
        let conn = self.lock();
        let json: String = conn.query_row(
            "SELECT config_json FROM configs WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO configs (name, config_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![new_name, json, now, now],
        )?;
        Ok(conn.last_insert_rowid())
    }
}
