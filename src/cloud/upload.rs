use anyhow::{Result, bail};
use tokio::sync::mpsc::UnboundedSender;

use crate::cloud::{self, CloudProvider, UploadProgress, UploadResult};
use crate::db::Database;
use crate::db::traces::TraceRecord;
use crate::perfetto::capture::Cancel;
use crate::session::Session;

/// Upload one or more traces to the cloud provider.
///
/// Returns an `UploadResult` with per-trace URLs and a folder URL.
/// Saves each trace's `remote_url` to the DB as it completes.
pub async fn upload_traces(
    provider: &dyn CloudProvider,
    db: &Database,
    session: &Session,
    traces: &[TraceRecord],
    progress_tx: &UnboundedSender<UploadProgress>,
    cancel: &Cancel,
) -> Result<UploadResult> {
    let remote_folder = cloud::remote_folder_for_session(provider, db, &session.name);
    let total_files = traces.len();
    let mut trace_results = Vec::with_capacity(total_files);

    for (i, trace) in traces.iter().enumerate() {
        if cancel.is_cancelled() {
            bail!("upload cancelled");
        }

        // Wrap the progress sender to inject file_index/total_files.
        let (inner_tx, mut inner_rx) = tokio::sync::mpsc::unbounded_channel::<UploadProgress>();

        let outer_tx = progress_tx.clone();
        let forward = tokio::spawn(async move {
            while let Some(mut p) = inner_rx.recv().await {
                p.file_index = i;
                p.total_files = total_files;
                let _ = outer_tx.send(p);
            }
        });

        let file_result = provider
            .upload_file(db, &trace.file_path, &remote_folder, &inner_tx, cancel)
            .await?;

        drop(inner_tx);
        let _ = forward.await;

        // Persist the remote URL to the database, keyed by provider name.
        if let Some(ref url) = file_result.remote_url {
            if let Err(e) = db.set_trace_upload(trace.id, provider.name(), url) {
                tracing::warn!(trace_id = trace.id, ?e, "failed to save upload url");
            }
        }

        trace_results.push((trace.id, file_result.remote_url));
    }

    // Get the folder URL (only worth fetching if we uploaded something).
    let folder_url = if !trace_results.is_empty() {
        provider.folder_url(db, &remote_folder).await.ok().flatten()
    } else {
        None
    };

    Ok(UploadResult {
        traces: trace_results,
        folder_url,
    })
}
