use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::Database;
use crate::perfetto::TraceConfig;
use crate::session::Session;

impl Database {
    pub fn create_session(&self, session: &Session) -> Result<i64> {
        let conn = self.lock();
        let config_json = serde_json::to_string(&session.config)?;
        conn.execute(
            "INSERT INTO sessions (name, package_name, device_serial, config_json, folder_path, created_at, notes, is_imported, benchmark_json_path, import_source_dir)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                session.name,
                session.package_name,
                session.device_serial,
                config_json,
                session.folder_path.to_string_lossy(),
                session.created_at.to_rfc3339(),
                session.notes,
                session.is_imported as i64,
                session.benchmark_json_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                session.import_source_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, package_name, device_serial, config_json, folder_path, created_at, notes, is_imported, benchmark_json_path, import_source_dir
             FROM sessions ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Row {
                id: row.get(0)?,
                name: row.get(1)?,
                package: row.get(2)?,
                device_serial: row.get(3)?,
                config_json: row.get(4)?,
                folder_path: row.get(5)?,
                created_at: row.get(6)?,
                notes: row.get(7)?,
                is_imported: row.get::<_, i64>(8)? != 0,
                benchmark_json_path: row.get(9)?,
                import_source_dir: row.get(10)?,
            })
        })?;

        let mut out = Vec::new();
        for r in rows {
            let r = r?;
            let mut config: TraceConfig = serde_json::from_str(&r.config_json)
                .context("deserialize session.config_json")?;
            config.migrate_legacy();
            let created_at: DateTime<Utc> = DateTime::parse_from_rfc3339(&r.created_at)
                .context("parse session.created_at")?
                .with_timezone(&Utc);
            out.push(Session {
                id: Some(r.id),
                name: r.name,
                package_name: r.package,
                device_serial: r.device_serial,
                config,
                folder_path: r.folder_path.into(),
                created_at,
                notes: r.notes,
                is_imported: r.is_imported,
                benchmark_json_path: r.benchmark_json_path.map(Into::into),
                import_source_dir: r.import_source_dir.map(Into::into),
            });
        }
        Ok(out)
    }

    /// Distinct package names previously used in any session, ordered by
    /// most-recently created. Powers the new-session wizard's package
    /// suggestions so the user doesn't have to retype the same strings.
    pub fn list_recent_packages(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT package_name FROM sessions
             WHERE package_name IS NOT NULL AND package_name <> ''
             GROUP BY package_name
             ORDER BY MAX(created_at) DESC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn delete_session(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn update_session_config(&self, id: i64, config: &TraceConfig) -> Result<()> {
        let conn = self.lock();
        let config_json = serde_json::to_string(config)?;
        conn.execute(
            "UPDATE sessions SET config_json = ?1 WHERE id = ?2",
            params![config_json, id],
        )?;
        Ok(())
    }
}

struct Row {
    id: i64,
    name: String,
    package: String,
    device_serial: Option<String>,
    config_json: String,
    folder_path: String,
    created_at: String,
    notes: Option<String>,
    is_imported: bool,
    benchmark_json_path: Option<String>,
    import_source_dir: Option<String>,
}
