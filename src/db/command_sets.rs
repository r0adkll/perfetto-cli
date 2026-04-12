use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;

use super::Database;
use crate::perfetto::commands::StartupCommand;

#[derive(Debug, Clone)]
pub struct SavedCommandSet {
    pub id: i64,
    pub name: String,
    pub commands: Vec<StartupCommand>,
}

impl Database {
    pub fn create_command_set(
        &self,
        name: &str,
        commands: &[StartupCommand],
    ) -> Result<i64> {
        let conn = self.lock();
        let json = serde_json::to_string(commands)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO command_sets (name, commands_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![name, json, now, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_command_set(
        &self,
        id: i64,
        commands: &[StartupCommand],
    ) -> Result<()> {
        let conn = self.lock();
        let json = serde_json::to_string(commands)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE command_sets SET commands_json = ?1, updated_at = ?2 WHERE id = ?3",
            params![json, now, id],
        )?;
        Ok(())
    }

    pub fn list_command_sets(&self) -> Result<Vec<SavedCommandSet>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, commands_json FROM command_sets ORDER BY updated_at DESC",
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
            let commands: Vec<StartupCommand> =
                serde_json::from_str(&json).context("deserialize command set")?;
            out.push(SavedCommandSet { id, name, commands });
        }
        Ok(out)
    }

    pub fn delete_command_set(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM command_sets WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }
}
