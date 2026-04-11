use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;
use std::path::{Path, PathBuf};

use super::Database;

#[derive(Debug, Clone)]
pub struct TraceRecord {
    pub id: i64,
    #[allow(dead_code)]
    pub session_id: i64,
    pub file_path: PathBuf,
    pub label: Option<String>,
    pub duration_ms: Option<u64>,
    pub size_bytes: Option<u64>,
    pub captured_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

impl Database {
    pub fn create_trace(
        &self,
        session_id: i64,
        file_path: &Path,
        label: Option<&str>,
        duration_ms: Option<u64>,
        size_bytes: Option<u64>,
    ) -> Result<i64> {
        let conn = self.lock();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO traces (session_id, file_path, label, duration_ms, size_bytes, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id,
                file_path.to_string_lossy(),
                label,
                duration_ms.map(|v| v as i64),
                size_bytes.map(|v| v as i64),
                now,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_traces(&self, session_id: i64) -> Result<Vec<TraceRecord>> {
        let conn = self.lock();

        let mut traces: Vec<TraceRecord> = {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, file_path, label, duration_ms, size_bytes, captured_at
                 FROM traces WHERE session_id = ?1 ORDER BY captured_at DESC",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                let captured_at_str: String = row.get(6)?;
                Ok(TraceRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    file_path: PathBuf::from(row.get::<_, String>(2)?),
                    label: row.get(3)?,
                    duration_ms: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    size_bytes: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                    captured_at: DateTime::parse_from_rfc3339(&captured_at_str)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    tags: Vec::new(),
                })
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };

        // Hydrate tags in a single lateral query rather than N+1 round trips.
        let mut tag_stmt = conn.prepare(
            "SELECT tt.trace_id, tt.tag_name
             FROM trace_tags tt JOIN traces t ON t.id = tt.trace_id
             WHERE t.session_id = ?1
             ORDER BY tt.trace_id, tt.tag_name",
        )?;
        let tag_rows =
            tag_stmt.query_map(params![session_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
        for row in tag_rows {
            let (trace_id, tag) = row?;
            if let Some(trace) = traces.iter_mut().find(|t| t.id == trace_id) {
                trace.tags.push(tag);
            }
        }

        Ok(traces)
    }

    pub fn rename_trace(&self, id: i64, label: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE traces SET label = ?1 WHERE id = ?2",
            params![label, id],
        )?;
        Ok(())
    }

    pub fn delete_trace(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM traces WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_trace_tags(&self, id: i64, tags: &[String]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM trace_tags WHERE trace_id = ?1", params![id])?;
        for tag in tags {
            tx.execute(
                "INSERT OR IGNORE INTO tags (name) VALUES (?1)",
                params![tag],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO trace_tags (trace_id, tag_name) VALUES (?1, ?2)",
                params![id, tag],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
