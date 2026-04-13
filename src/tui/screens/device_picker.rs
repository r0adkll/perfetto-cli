use std::collections::HashMap;

use anyhow::Result;

use crate::adb;
use crate::adb::DeviceState;
use crate::db::Database;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    Online,
    Offline,
    Unauthorized,
    Other(String),
    NotConnected,
}

impl From<DeviceState> for EntryState {
    fn from(s: DeviceState) -> Self {
        match s {
            DeviceState::Online => EntryState::Online,
            DeviceState::Offline => EntryState::Offline,
            DeviceState::Unauthorized => EntryState::Unauthorized,
            DeviceState::Other(x) => EntryState::Other(x),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub serial: String,
    pub nickname: Option<String>,
    pub model: Option<String>,
    pub state: EntryState,
}

pub(crate) async fn load_entries(db: Database) -> Result<Vec<DeviceEntry>> {
    let live = adb::list_live_devices().await?;
    for d in &live {
        db.upsert_device_seen(&d.serial, d.model.as_deref())?;
    }
    let known = db.list_known_devices()?;

    let mut live_map: HashMap<String, adb::Device> = HashMap::new();
    for d in live {
        live_map.insert(d.serial.clone(), d);
    }

    let mut entries: Vec<DeviceEntry> = known
        .into_iter()
        .map(|rec| {
            let live = live_map.remove(&rec.serial);
            let state = live
                .as_ref()
                .map(|d| d.state.clone().into())
                .unwrap_or(EntryState::NotConnected);
            let model = live
                .as_ref()
                .and_then(|d| d.model.clone())
                .or(rec.model);
            DeviceEntry {
                serial: rec.serial,
                nickname: rec.nickname,
                model,
                state,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        let a_conn = !matches!(a.state, EntryState::NotConnected);
        let b_conn = !matches!(b.state, EntryState::NotConnected);
        b_conn.cmp(&a_conn).then(a.serial.cmp(&b.serial))
    });

    Ok(entries)
}
