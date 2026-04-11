use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::perfetto::TraceConfig;

const DEFAULT_FOLDER_NAME: &str = "session";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Option<i64>,
    pub name: String,
    pub package_name: String,
    pub device_serial: Option<String>,
    pub config: TraceConfig,
    pub folder_path: PathBuf,
    pub created_at: DateTime<Utc>,
    pub notes: Option<String>,
}

impl Session {
    /// Base on-disk folder name for a new session. Date-agnostic — a session
    /// can span multiple capture days without the folder drifting from its
    /// creation date. Collision handling is the caller's job (see
    /// `unique_folder_path`).
    pub fn folder_name(name: &str) -> String {
        let slug = slugify(name);
        if slug.is_empty() {
            DEFAULT_FOLDER_NAME.into()
        } else {
            slug
        }
    }

    /// Pick a folder path under `parent` that doesn't collide with anything
    /// already on disk. Starts from `<parent>/<name>`, then `<name>-2`,
    /// `<name>-3`, … until a free slot is found.
    pub fn unique_folder_path(parent: &Path, name: &str) -> PathBuf {
        let base = Self::folder_name(name);
        let first = parent.join(&base);
        if !first.exists() {
            return first;
        }
        for n in 2u32.. {
            let candidate = parent.join(format!("{base}-{n}"));
            if !candidate.exists() {
                return candidate;
            }
        }
        unreachable!("u32::MAX folder collisions")
    }

    pub fn traces_dir(&self) -> PathBuf {
        self.folder_path.join("traces")
    }

    pub fn session_json_path(&self) -> PathBuf {
        self.folder_path.join("session.json")
    }

    /// Create the session folder and write the self-describing `session.json`.
    pub fn ensure_filesystem(&self) -> Result<()> {
        std::fs::create_dir_all(self.traces_dir())?;
        let json = serde_json::to_string_pretty(self).context("serialize session.json")?;
        std::fs::write(self.session_json_path(), json)?;
        Ok(())
    }

    /// Best-effort removal of the session folder from disk.
    pub fn remove_from_disk(folder: &Path) -> Result<()> {
        if folder.exists() {
            std::fs::remove_dir_all(folder)?;
        }
        Ok(())
    }
}

pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(slugify("My Session!"), "my-session");
        assert_eq!(slugify("  spaced  out  "), "spaced-out");
        assert_eq!(slugify("a__b__c"), "a-b-c");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("A1/B2"), "a1-b2");
    }
}
