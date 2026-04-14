//! Thin wrapper that spawns an analysis worker tagged for one side of the
//! diff. The wrap closure bakes in the [`DiffSide`] so the DiffScreen can
//! tell which half each event updates.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::Paths;
use crate::perfetto::capture::Cancel;
use crate::tui::event::{AppEvent, DiffSide};
use crate::tui::screens::analysis::worker::{WorkerRequest, spawn_worker};

pub fn spawn_diff_worker(
    side: DiffSide,
    paths: Paths,
    trace_path: PathBuf,
    cancel: Arc<Cancel>,
    app_tx: UnboundedSender<AppEvent>,
    package_name: String,
) -> UnboundedSender<WorkerRequest> {
    spawn_worker(
        paths,
        trace_path,
        cancel,
        app_tx,
        package_name,
        move |event| AppEvent::Diff { side, event },
    )
}
