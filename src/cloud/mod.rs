pub mod google_drive;
pub mod oauth;
pub mod upload;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use crate::db::Database;
use crate::perfetto::capture::Cancel;

/// Progress update emitted during a cloud upload.
#[derive(Debug, Clone)]
pub struct UploadProgress {
    pub file_name: String,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    /// Zero-based index of the current file in a batch upload.
    pub file_index: usize,
    /// Total number of files in this upload batch.
    pub total_files: usize,
}

/// Result of a single file upload from the provider.
#[derive(Debug, Clone)]
pub struct FileUploadResult {
    /// Shareable link if the provider supplies one.
    pub remote_url: Option<String>,
}

/// Result of uploading traces, with trace IDs attached by the orchestration layer.
#[derive(Debug, Clone)]
pub struct UploadResult {
    /// Per-trace results: (trace DB id, shareable URL).
    pub traces: Vec<(i64, Option<String>)>,
    /// Link to the session folder on the cloud provider.
    pub folder_url: Option<String>,
}

/// Trait that every cloud storage provider implements.
///
/// Designed for extensibility — add a new provider by implementing this trait
/// and registering it alongside Google Drive.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    /// Human-readable name, e.g. "Google Drive".
    fn name(&self) -> &str;

    /// Unique key used for settings storage, e.g. "google_drive".
    fn id(&self) -> &str;

    /// Whether the provider currently has valid (or refreshable) credentials.
    async fn is_authenticated(&self, db: &Database) -> bool;

    /// Run the full OAuth flow: open browser, wait for redirect, store tokens.
    async fn authenticate(&self, db: &Database) -> Result<()>;

    /// Clear stored credentials.
    async fn logout(&self, db: &Database) -> Result<()>;

    /// Upload a single file to `remote_folder` (created if it doesn't exist).
    /// Reports progress via `progress_tx`. Honors `cancel` between chunks.
    async fn upload_file(
        &self,
        db: &Database,
        local_path: &Path,
        remote_folder: &str,
        progress_tx: &UnboundedSender<UploadProgress>,
        cancel: &Cancel,
    ) -> Result<FileUploadResult>;

    /// Return a browser URL for the given remote folder path, if supported.
    async fn folder_url(
        &self,
        db: &Database,
        remote_folder: &str,
    ) -> Result<Option<String>>;

    /// The configured root folder for uploads (read from settings, with a
    /// provider-specific default).
    fn upload_folder(&self, db: &Database) -> String;

    /// Settings key for the upload folder, so the UI can persist changes.
    fn folder_settings_key(&self) -> String;
}

// ---------------------------------------------------------------------------
// Provider registry
// ---------------------------------------------------------------------------

/// All registered cloud providers.
pub fn all_providers() -> Vec<Arc<dyn CloudProvider>> {
    vec![Arc::new(google_drive::GoogleDriveProvider)]
}

/// The ID of the user's preferred default provider. Falls back to the first
/// registered provider if none is set.
pub fn default_provider_id(db: &Database) -> String {
    db.get_setting("cloud.default_provider")
        .ok()
        .flatten()
        .unwrap_or_else(|| "google_drive".into())
}

/// Look up a provider by its ID.
pub fn provider_by_id(id: &str) -> Option<Arc<dyn CloudProvider>> {
    all_providers().into_iter().find(|p| p.id() == id)
}

/// Returns the default cloud provider.
pub fn default_provider(db: &Database) -> Arc<dyn CloudProvider> {
    let id = default_provider_id(db);
    provider_by_id(&id).unwrap_or_else(|| Arc::new(google_drive::GoogleDriveProvider))
}

/// Build the remote folder path for a session using the provider's configured
/// root folder.
pub fn remote_folder_for_session(provider: &dyn CloudProvider, db: &Database, session_name: &str) -> String {
    let root = provider.upload_folder(db);
    format!("{root}/{session_name}")
}
