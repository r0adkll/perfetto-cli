use std::path::Path;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use crate::cloud::{CloudProvider, FileUploadResult, UploadProgress};
use crate::db::Database;
use crate::perfetto::capture::Cancel;

use super::oauth;

/// Google OAuth credentials, injected at compile time via env vars.
/// Set `PERFETTO_GOOGLE_CLIENT_ID` and `PERFETTO_GOOGLE_CLIENT_SECRET`
/// before building — locally in your shell, or via GHA secrets in CI.
const CLIENT_ID: &str = env!("PERFETTO_GOOGLE_CLIENT_ID");
const CLIENT_SECRET: &str = env!("PERFETTO_GOOGLE_CLIENT_SECRET");

const PROVIDER_ID: &str = "google_drive";

/// Upload chunk size: 5 MiB (must be a multiple of 256 KiB for resumable uploads).
const CHUNK_SIZE: usize = 5 * 1024 * 1024;

const DRIVE_FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const DRIVE_UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";

pub struct GoogleDriveProvider;

#[async_trait]
impl CloudProvider for GoogleDriveProvider {
    fn name(&self) -> &str {
        "Google Drive"
    }

    fn id(&self) -> &str {
        PROVIDER_ID
    }

    async fn is_authenticated(&self, db: &Database) -> bool {
        db.get_setting(&format!("cloud.{PROVIDER_ID}.refresh_token"))
            .ok()
            .flatten()
            .is_some()
    }

    async fn authenticate(&self, db: &Database) -> Result<()> {
        let verifier = oauth::generate_code_verifier();
        let challenge = oauth::generate_code_challenge(&verifier);
        let state = oauth::generate_state();

        let auth_url = oauth::build_auth_url(CLIENT_ID, &challenge, &state);
        tracing::info!("opening browser for Google OAuth");
        webbrowser::open(&auth_url).context("failed to open browser for OAuth")?;

        // Block on the redirect listener (runs in the tokio blocking pool).
        let expected_state = state.clone();
        let code = tokio::task::spawn_blocking(move || oauth::wait_for_redirect(&expected_state))
            .await
            .context("OAuth listener task panicked")??;

        oauth::exchange_code(CLIENT_ID, CLIENT_SECRET, &code, &verifier, db, PROVIDER_ID).await?;
        tracing::info!("Google Drive authentication successful");
        Ok(())
    }

    async fn logout(&self, db: &Database) -> Result<()> {
        oauth::clear_tokens(db, PROVIDER_ID)?;
        tracing::info!("Google Drive credentials cleared");
        Ok(())
    }

    async fn upload_file(
        &self,
        db: &Database,
        local_path: &Path,
        remote_folder: &str,
        progress_tx: &UnboundedSender<UploadProgress>,
        cancel: &Cancel,
    ) -> Result<FileUploadResult> {
        let token = oauth::ensure_valid_token(CLIENT_ID, CLIENT_SECRET, db, PROVIDER_ID).await?;
        let client = reqwest::Client::new();

        // Ensure remote folder hierarchy exists.
        let folder_id = ensure_folder_hierarchy(&client, &token, remote_folder).await?;

        let file_name = local_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "trace.pftrace".into());

        let file_data = tokio::fs::read(local_path)
            .await
            .context("failed to read trace file")?;
        let total_bytes = file_data.len() as u64;

        // Initiate resumable upload.
        let metadata = serde_json::json!({
            "name": file_name,
            "parents": [folder_id],
        });

        let init_resp = client
            .post(format!("{DRIVE_UPLOAD_URL}?uploadType=resumable"))
            .bearer_auth(&token)
            .header("Content-Type", "application/json; charset=UTF-8")
            .header("X-Upload-Content-Length", total_bytes.to_string())
            .json(&metadata)
            .send()
            .await
            .context("resumable upload init failed")?;

        if !init_resp.status().is_success() {
            let body = init_resp.text().await.unwrap_or_default();
            bail!("resumable upload init failed: {body}");
        }

        let upload_uri = init_resp
            .headers()
            .get("location")
            .context("no Location header in resumable upload response")?
            .to_str()
            .context("invalid Location header")?
            .to_string();

        // Upload in chunks with progress.
        let mut offset: usize = 0;
        while offset < file_data.len() {
            if cancel.is_cancelled() {
                bail!("upload cancelled");
            }

            let end = (offset + CHUNK_SIZE).min(file_data.len());
            let chunk = &file_data[offset..end];
            let content_range = format!(
                "bytes {}-{}/{}",
                offset,
                end - 1,
                total_bytes
            );

            let resp = client
                .put(&upload_uri)
                .header("Content-Range", &content_range)
                .header("Content-Length", chunk.len().to_string())
                .body(chunk.to_vec())
                .send()
                .await
                .context("chunk upload failed")?;

            let status = resp.status().as_u16();
            // 200/201 = final chunk done; 308 = more chunks needed.
            if status != 200 && status != 201 && status != 308 {
                let body = resp.text().await.unwrap_or_default();
                bail!("chunk upload failed (HTTP {status}): {body}");
            }

            offset = end;

            let _ = progress_tx.send(UploadProgress {
                file_name: file_name.clone(),
                bytes_sent: offset as u64,
                total_bytes,
                file_index: 0,
                total_files: 1,
            });

            // If this was the final chunk, try to extract a shareable link.
            if status == 200 || status == 201 {
                let file_id = resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v["id"].as_str().map(String::from));

                let remote_url = file_id.map(|id| {
                    format!("https://drive.google.com/file/d/{id}/view")
                });

                return Ok(FileUploadResult { remote_url });
            }
        }

        Ok(FileUploadResult { remote_url: None })
    }

    async fn folder_url(
        &self,
        db: &Database,
        remote_folder: &str,
    ) -> Result<Option<String>> {
        let token = oauth::ensure_valid_token(CLIENT_ID, CLIENT_SECRET, db, PROVIDER_ID).await?;
        let client = reqwest::Client::new();

        // Look up the deepest folder ID.
        let folder_id = ensure_folder_hierarchy(&client, &token, remote_folder).await?;
        if folder_id == "root" {
            return Ok(None);
        }
        Ok(Some(format!("https://drive.google.com/drive/folders/{folder_id}")))
    }

    fn upload_folder(&self, db: &Database) -> String {
        db.get_setting(&self.folder_settings_key())
            .ok()
            .flatten()
            .unwrap_or_else(|| "perfetto-cli".into())
    }

    fn folder_settings_key(&self) -> String {
        format!("cloud.{PROVIDER_ID}.folder")
    }
}

/// Ensure a folder hierarchy like "perfetto-cli/my-session" exists in Drive.
/// Returns the ID of the deepest folder.
async fn ensure_folder_hierarchy(
    client: &reqwest::Client,
    token: &str,
    path: &str,
) -> Result<String> {
    let mut parent_id = "root".to_string();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        parent_id = find_or_create_folder(client, token, segment, &parent_id).await?;
    }
    Ok(parent_id)
}

/// Find a folder by name under `parent_id`, or create it.
async fn find_or_create_folder(
    client: &reqwest::Client,
    token: &str,
    name: &str,
    parent_id: &str,
) -> Result<String> {
    // Search for existing folder.
    let query = format!(
        "name = '{}' and '{}' in parents and mimeType = 'application/vnd.google-apps.folder' and trashed = false",
        name.replace('\'', "\\'"),
        parent_id,
    );
    let search_resp = client
        .get(DRIVE_FILES_URL)
        .bearer_auth(token)
        .query(&[("q", query.as_str()), ("fields", "files(id)")])
        .send()
        .await
        .context("Drive folder search failed")?;

    let search_body: serde_json::Value = search_resp.json().await?;
    if let Some(id) = search_body["files"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|f| f["id"].as_str())
    {
        return Ok(id.to_string());
    }

    // Create folder.
    let metadata = serde_json::json!({
        "name": name,
        "mimeType": "application/vnd.google-apps.folder",
        "parents": [parent_id],
    });
    let create_resp = client
        .post(DRIVE_FILES_URL)
        .bearer_auth(token)
        .json(&metadata)
        .send()
        .await
        .context("Drive folder creation failed")?;

    if !create_resp.status().is_success() {
        let body = create_resp.text().await.unwrap_or_default();
        bail!("failed to create folder '{name}': {body}");
    }

    let created: serde_json::Value = create_resp.json().await?;
    created["id"]
        .as_str()
        .map(String::from)
        .context("created folder response missing id")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_and_name() {
        let p = GoogleDriveProvider;
        assert_eq!(p.name(), "Google Drive");
        assert_eq!(p.id(), "google_drive");
    }
}
