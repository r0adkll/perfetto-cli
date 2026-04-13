use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use tokio::sync::mpsc::UnboundedSender;

use crate::cloud::{CloudProvider, FileUploadResult, UploadProgress};
use crate::db::Database;
use crate::perfetto::capture::Cancel;

const PROVIDER_ID: &str = "amazon_s3";

/// Upload part size: 5 MiB (S3 minimum for multipart parts).
const PART_SIZE: usize = 5 * 1024 * 1024;

/// Presigned URL validity: 7 days (S3 max).
const PRESIGN_EXPIRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Settings key helper.
fn key(field: &str) -> String {
    format!("cloud.{PROVIDER_ID}.{field}")
}

pub struct AmazonS3Provider;

impl AmazonS3Provider {
    /// Build an S3 client from stored settings.
    async fn build_client(&self, db: &Database) -> Result<(aws_sdk_s3::Client, String)> {
        let region = db
            .get_setting(&key("region"))?
            .context("S3 region not configured — press [r] to set it")?;
        let bucket = db
            .get_setting(&key("bucket"))?
            .context("S3 bucket not configured — press [b] to set it")?;

        let auth_mode = db
            .get_setting(&key("auth_mode"))?
            .unwrap_or_else(|| "keys".into());

        let config = match auth_mode.as_str() {
            "profile" => {
                let profile = db
                    .get_setting(&key("profile_name"))?
                    .unwrap_or_else(|| "default".into());
                aws_config::defaults(BehaviorVersion::latest())
                    .profile_name(&profile)
                    .region(Region::new(region))
                    .load()
                    .await
            }
            _ => {
                // "keys" mode (default).
                let access_key = db
                    .get_setting(&key("access_key_id"))?
                    .context("S3 access key not configured — press [a] to set it")?;
                let secret_key = db
                    .get_setting(&key("secret_access_key"))?
                    .context("S3 secret key not configured — press [s] to set it")?;

                let creds = Credentials::new(
                    &access_key,
                    &secret_key,
                    None,
                    None,
                    "perfetto-cli",
                );

                aws_config::defaults(BehaviorVersion::latest())
                    .credentials_provider(creds)
                    .region(Region::new(region))
                    .load()
                    .await
            }
        };

        let client = aws_sdk_s3::Client::new(&config);
        Ok((client, bucket))
    }
}

#[async_trait]
impl CloudProvider for AmazonS3Provider {
    fn name(&self) -> &str {
        "Amazon S3"
    }

    fn id(&self) -> &str {
        PROVIDER_ID
    }

    async fn is_authenticated(&self, db: &Database) -> bool {
        // Must have bucket + region + credentials configured.
        let has_bucket = db.get_setting(&key("bucket")).ok().flatten().is_some();
        let has_region = db.get_setting(&key("region")).ok().flatten().is_some();
        if !has_bucket || !has_region {
            return false;
        }

        let auth_mode = db
            .get_setting(&key("auth_mode"))
            .ok()
            .flatten()
            .unwrap_or_else(|| "keys".into());

        match auth_mode.as_str() {
            "profile" => true, // Profile is assumed valid if configured.
            _ => {
                db.get_setting(&key("access_key_id")).ok().flatten().is_some()
                    && db.get_setting(&key("secret_access_key")).ok().flatten().is_some()
            }
        }
    }

    async fn authenticate(&self, db: &Database) -> Result<()> {
        let (client, bucket) = self.build_client(db).await?;

        // Validate credentials by calling HeadBucket.
        client
            .head_bucket()
            .bucket(&bucket)
            .send()
            .await
            .context(format!("cannot access bucket '{bucket}' — check credentials, region, and bucket name"))?;

        tracing::info!("Amazon S3 authentication validated (bucket: {bucket})");
        Ok(())
    }

    async fn logout(&self, db: &Database) -> Result<()> {
        for field in &[
            "bucket",
            "region",
            "auth_mode",
            "access_key_id",
            "secret_access_key",
            "profile_name",
        ] {
            let _ = db.delete_setting(&key(field));
        }
        tracing::info!("Amazon S3 credentials cleared");
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
        let (client, bucket) = self.build_client(db).await?;

        let file_name = local_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "trace.pftrace".into());

        let s3_key = format!("{remote_folder}/{file_name}");
        let file_data = tokio::fs::read(local_path)
            .await
            .context("failed to read trace file")?;
        let total_bytes = file_data.len() as u64;

        if file_data.len() <= PART_SIZE {
            // Single PutObject for small files.
            let _ = progress_tx.send(UploadProgress {
                file_name: file_name.clone(),
                bytes_sent: 0,
                total_bytes,
                file_index: 0,
                total_files: 1,
            });

            client
                .put_object()
                .bucket(&bucket)
                .key(&s3_key)
                .body(ByteStream::from(file_data))
                .content_type("application/octet-stream")
                .send()
                .await
                .context("S3 PutObject failed")?;

            let _ = progress_tx.send(UploadProgress {
                file_name: file_name.clone(),
                bytes_sent: total_bytes,
                total_bytes,
                file_index: 0,
                total_files: 1,
            });
        } else {
            // Multipart upload for large files.
            let create = client
                .create_multipart_upload()
                .bucket(&bucket)
                .key(&s3_key)
                .content_type("application/octet-stream")
                .send()
                .await
                .context("S3 CreateMultipartUpload failed")?;

            let upload_id = create
                .upload_id()
                .context("missing upload_id in multipart response")?
                .to_string();

            let mut completed_parts = Vec::new();
            let mut offset: usize = 0;
            let mut part_number: i32 = 1;

            let upload_result = async {
                while offset < file_data.len() {
                    if cancel.is_cancelled() {
                        bail!("upload cancelled");
                    }

                    let end = (offset + PART_SIZE).min(file_data.len());
                    let chunk = file_data[offset..end].to_vec();

                    let part = client
                        .upload_part()
                        .bucket(&bucket)
                        .key(&s3_key)
                        .upload_id(&upload_id)
                        .part_number(part_number)
                        .body(ByteStream::from(chunk))
                        .send()
                        .await
                        .context(format!("S3 UploadPart {part_number} failed"))?;

                    let etag = part.e_tag().unwrap_or_default().to_string();
                    completed_parts.push(
                        CompletedPart::builder()
                            .e_tag(&etag)
                            .part_number(part_number)
                            .build(),
                    );

                    offset = end;
                    part_number += 1;

                    let _ = progress_tx.send(UploadProgress {
                        file_name: file_name.clone(),
                        bytes_sent: offset as u64,
                        total_bytes,
                        file_index: 0,
                        total_files: 1,
                    });
                }
                Ok(())
            }
            .await;

            if let Err(e) = upload_result {
                // Abort the multipart upload on failure.
                let _ = client
                    .abort_multipart_upload()
                    .bucket(&bucket)
                    .key(&s3_key)
                    .upload_id(&upload_id)
                    .send()
                    .await;
                return Err(e);
            }

            let completed = CompletedMultipartUpload::builder()
                .set_parts(Some(completed_parts))
                .build();

            client
                .complete_multipart_upload()
                .bucket(&bucket)
                .key(&s3_key)
                .upload_id(&upload_id)
                .multipart_upload(completed)
                .send()
                .await
                .context("S3 CompleteMultipartUpload failed")?;
        }

        // Generate a presigned GET URL.
        let presign_config = PresigningConfig::expires_in(PRESIGN_EXPIRY)
            .context("invalid presign duration")?;

        let presigned = client
            .get_object()
            .bucket(&bucket)
            .key(&s3_key)
            .presigned(presign_config)
            .await
            .context("failed to generate presigned URL")?;

        Ok(FileUploadResult {
            remote_url: Some(presigned.uri().to_string()),
        })
    }

    async fn folder_url(
        &self,
        db: &Database,
        remote_folder: &str,
    ) -> Result<Option<String>> {
        let bucket = db
            .get_setting(&key("bucket"))?
            .context("bucket not configured")?;
        let region = db
            .get_setting(&key("region"))?
            .context("region not configured")?;

        let prefix = if remote_folder.ends_with('/') {
            remote_folder.to_string()
        } else {
            format!("{remote_folder}/")
        };

        Ok(Some(format!(
            "https://s3.console.aws.amazon.com/s3/buckets/{bucket}?region={region}&prefix={prefix}"
        )))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_and_name() {
        let p = AmazonS3Provider;
        assert_eq!(p.name(), "Amazon S3");
        assert_eq!(p.id(), "amazon_s3");
    }

    #[test]
    fn settings_key_format() {
        assert_eq!(key("bucket"), "cloud.amazon_s3.bucket");
        assert_eq!(key("region"), "cloud.amazon_s3.region");
    }

    #[test]
    fn folder_settings_key() {
        let p = AmazonS3Provider;
        assert_eq!(p.folder_settings_key(), "cloud.amazon_s3.folder");
    }
}
