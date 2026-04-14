//! DAO for the `saved_queries` table.
//!
//! Saved queries are scoped by `package_name` — they belong to the app
//! being analysed, not the session, so they persist across captures of
//! the same app. The Analysis screen reads them at every `RunSummary`
//! refresh and renders results in a "Custom metrics" section.
//!
//! v1 exposes upsert / list / delete. No UI for list / delete yet; the
//! DAO method is here so a future management screen has a ready API.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::Database;

#[derive(Debug, Clone)]
pub struct SavedQuery {
    pub id: i64,
    pub package_name: String,
    pub name: String,
    pub sql: String,
    pub created_at: DateTime<Utc>,
}

impl Database {
    /// Insert a new saved query, or replace the `sql` of an existing one
    /// with the same `(package_name, name)` pair. Returns the row id of
    /// the inserted or updated row.
    pub fn upsert_saved_query(
        &self,
        package_name: &str,
        name: &str,
        sql: &str,
    ) -> Result<i64> {
        let conn = self.lock();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO saved_queries (package_name, name, sql, created_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(package_name, name) DO UPDATE SET \
               sql = excluded.sql",
            params![package_name, name, sql, now],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM saved_queries WHERE package_name = ?1 AND name = ?2",
            params![package_name, name],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// List every saved query for a package, oldest first so the set renders
    /// in stable creation order.
    pub fn list_saved_queries(&self, package_name: &str) -> Result<Vec<SavedQuery>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, package_name, name, sql, created_at \
             FROM saved_queries \
             WHERE package_name = ?1 \
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![package_name], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, package_name, name, sql, created_at) = r?;
            let created_at = DateTime::parse_from_rfc3339(&created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            out.push(SavedQuery {
                id,
                package_name,
                name,
                sql,
                created_at,
            });
        }
        Ok(out)
    }

    /// Remove one saved query by its `(package_name, name)` pair. Silently
    /// succeeds if no row matches — callers that need to distinguish can
    /// check the return value (rows affected is not surfaced here; add if
    /// anyone needs it).
    #[allow(dead_code)]
    pub fn delete_saved_query(&self, package_name: &str, name: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM saved_queries WHERE package_name = ?1 AND name = ?2",
            params![package_name, name],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn test_db() -> Database {
        // In-memory database pre-loaded with the real schema.
        let conn = Connection::open_in_memory().expect("open memory db");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(include_str!("schema.sql"))
            .expect("apply schema");
        Database::from_connection(Arc::new(Mutex::new(conn)))
    }

    #[test]
    fn upsert_inserts_then_updates_in_place() {
        let db = test_db();
        let id1 = db
            .upsert_saved_query("com.app", "count", "SELECT 1")
            .unwrap();
        let id2 = db
            .upsert_saved_query("com.app", "count", "SELECT 2")
            .unwrap();
        assert_eq!(id1, id2, "same (pkg, name) must update in place");

        let list = db.list_saved_queries("com.app").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].sql, "SELECT 2");
    }

    #[test]
    fn list_filters_by_package() {
        let db = test_db();
        db.upsert_saved_query("com.a", "x", "S1").unwrap();
        db.upsert_saved_query("com.a", "y", "S2").unwrap();
        db.upsert_saved_query("com.b", "x", "S3").unwrap();

        let a = db.list_saved_queries("com.a").unwrap();
        assert_eq!(a.len(), 2);
        let b = db.list_saved_queries("com.b").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].sql, "S3");
    }

    #[test]
    fn delete_targets_named_query_only() {
        let db = test_db();
        db.upsert_saved_query("com.app", "keep", "K").unwrap();
        db.upsert_saved_query("com.app", "drop", "D").unwrap();
        db.delete_saved_query("com.app", "drop").unwrap();

        let list = db.list_saved_queries("com.app").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "keep");
    }

    #[test]
    fn delete_noop_for_unknown_name() {
        let db = test_db();
        db.upsert_saved_query("com.app", "keep", "K").unwrap();
        // Should not error even though there's nothing to delete.
        db.delete_saved_query("com.app", "missing").unwrap();
        assert_eq!(db.list_saved_queries("com.app").unwrap().len(), 1);
    }

    #[test]
    fn list_orders_by_created_at_ascending() {
        let db = test_db();
        db.upsert_saved_query("com.app", "first", "F").unwrap();
        // Small sleep to guarantee distinct created_at tokens for the
        // second row — the DAO uses RFC3339 strings with ms resolution,
        // so a single insert-per-millisecond is usually enough in
        // practice; fall back to id-break for safety.
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.upsert_saved_query("com.app", "second", "S").unwrap();
        let list = db.list_saved_queries("com.app").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "first");
        assert_eq!(list[1].name, "second");
    }
}
