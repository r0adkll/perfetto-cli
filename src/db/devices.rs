use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, params};

use super::Database;

#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub serial: String,
    pub nickname: Option<String>,
    pub model: Option<String>,
    #[allow(dead_code)]
    pub last_seen: Option<String>,
}

impl Database {
    /// Insert-or-update on observation. Preserves any existing nickname and
    /// only overwrites the model when a non-empty value is observed.
    pub fn upsert_device_seen(&self, serial: &str, model: Option<&str>) -> Result<()> {
        let conn = self.lock();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO devices (serial, model, last_seen) VALUES (?1, ?2, ?3)
             ON CONFLICT(serial) DO UPDATE SET
               model     = COALESCE(excluded.model, devices.model),
               last_seen = excluded.last_seen",
            params![serial, model, now],
        )?;
        Ok(())
    }

    pub fn list_known_devices(&self) -> Result<Vec<DeviceRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT serial, nickname, model, last_seen FROM devices
             ORDER BY last_seen DESC NULLS LAST, serial",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(DeviceRecord {
                    serial: row.get(0)?,
                    nickname: row.get(1)?,
                    model: row.get(2)?,
                    last_seen: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    #[allow(dead_code)]
    pub fn get_device_nickname(&self, serial: &str) -> Result<Option<String>> {
        let conn = self.lock();
        let nick = conn
            .query_row(
                "SELECT nickname FROM devices WHERE serial = ?1",
                params![serial],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        Ok(nick)
    }

}
