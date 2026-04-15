//! Background worker for the analysis screen.
//!
//! The screen never touches a [`TraceProcessor`] directly. Instead it sends
//! [`WorkerRequest`]s over an mpsc channel; a single `tokio::spawn`ed worker
//! owns the client, runs queries sequentially, and replies by emitting
//! [`AnalysisEvent`]s on the app-wide event bus.
//!
//! Shutdown is Drop-driven: when the screen is replaced its `worker_tx` is
//! dropped, the worker's `rx.recv()` returns `None`, and the task runs
//! `TraceProcessor::shutdown` before returning. `kill_on_drop(true)` on the
//! underlying subprocess is the ultimate safety net.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedSender};

use crate::config::Paths;
use crate::perfetto::capture::Cancel;
use crate::trace_processor::{
    Cell, LoadProgress, QueryResult, Row, TraceProcessor,
};
use crate::tui::event::AppEvent;

use super::summary::{SummaryContext, SummaryKey, SummaryQuery};

/// Commands the analysis screen sends to its worker.
pub enum WorkerRequest {
    /// Fire every canned summary query and every user-saved custom query.
    /// The worker emits one [`AnalysisEvent::SummaryCell`] /
    /// [`AnalysisEvent::SummaryRows`] per canned key, and one
    /// [`AnalysisEvent::CustomResult`] per custom query.
    RunSummary { custom_queries: Vec<CustomQuery> },
    /// REPL submission. `id` lets the screen pair results to their original
    /// SQL when multiple queries are in flight (we only allow one at a time
    /// today but the id keeps the protocol future-proof).
    RunQuery { id: u64, sql: String },
    /// Enumerate the loaded trace's tables for the REPL's completion
    /// popup. Fires exactly once after load; the result flows back as a
    /// single [`AnalysisEvent::SchemaLoaded`] regardless of success —
    /// query failure yields an empty list so the REPL falls back to the
    /// curated static candidates silently.
    LoadSchema,
}

/// A user-saved PerfettoSQL query fed to the worker alongside the canned
/// summary queries. Read from the DB by the screen each time a
/// `RunSummary` is dispatched; the worker itself stays DB-free.
#[derive(Debug, Clone)]
pub struct CustomQuery {
    pub name: String,
    pub sql: String,
}

/// Events emitted by the worker back to the app event bus.
pub enum AnalysisEvent {
    LoadProgress(LoadProgress),
    LoadReady {
        version: Option<String>,
    },
    LoadFailed(String),
    /// Single-cell result for a summary metric.
    SummaryCell {
        key: SummaryKey,
        result: SummaryCellOutcome,
    },
    /// Multi-row result for summary metrics like "top slices".
    SummaryRows {
        key: SummaryKey,
        result: SummaryRowsOutcome,
    },
    /// REPL query completed.
    QueryResult {
        id: u64,
        sql: String,
        result: Result<QueryResult, String>,
    },
    /// One user-saved query completed as part of a Summary refresh. No
    /// soft-fail downgrade — the user wrote the SQL, so raw error text
    /// is more useful feedback than a `—`.
    CustomResult {
        name: String,
        result: Result<QueryResult, String>,
    },
    /// Table names discovered in the loaded trace, feeding the REPL's
    /// completion popup. Always emitted in response to a
    /// [`WorkerRequest::LoadSchema`] — an empty vec on query failure
    /// rather than an error, since losing schema completion is purely
    /// a cosmetic downgrade.
    SchemaLoaded { tables: Vec<String> },
    /// Per-table column names discovered via `PRAGMA table_info`. Fired
    /// once after [`AnalysisEvent::SchemaLoaded`] with the aggregated
    /// map — per-table streaming would add event volume without helping
    /// the UX, since the REPL rebuilds the merged candidate pool once
    /// per snapshot anyway. Tables whose PRAGMA failed are omitted.
    ColumnsLoaded {
        by_table: std::collections::HashMap<String, Vec<String>>,
    },
}

/// Outcome of a single-cell summary query. `MissingTable` lets the UI render
/// `—` without treating it as an error.
pub enum SummaryCellOutcome {
    Ok(Cell),
    MissingTable,
    Error(String),
}

pub enum SummaryRowsOutcome {
    Ok(Vec<Row>),
    MissingTable,
    Error(String),
}

/// Spawn the worker task. Returns the sender the screen uses to enqueue work.
///
/// `package_name` scopes queries that need it (e.g. main-thread hotspots) —
/// it's threaded into a [`SummaryContext`] used when building queries in the
/// `RunSummary` arm.
///
/// `wrap` chooses which `AppEvent` variant carries each `AnalysisEvent`.
/// The Analysis screen passes `|ev| AppEvent::Analysis(ev)`; the Diff screen
/// passes a closure that tags the event with a `DiffSide`. Keeping the
/// worker itself agnostic of event routing lets us reuse the same machinery
/// for any screen that wants a trace_processor pipeline.
pub fn spawn_worker<F>(
    paths: Paths,
    trace_path: PathBuf,
    cancel: Arc<Cancel>,
    app_tx: UnboundedSender<AppEvent>,
    package_name: String,
    wrap: F,
) -> UnboundedSender<WorkerRequest>
where
    F: Fn(AnalysisEvent) -> AppEvent + Send + Sync + 'static,
{
    let (req_tx, mut req_rx) = mpsc::unbounded_channel::<WorkerRequest>();
    let wrap = Arc::new(wrap);

    tokio::spawn(async move {
        // ── Phase 1: forward load progress ────────────────────────────────
        let (prog_tx, mut prog_rx) = mpsc::unbounded_channel::<LoadProgress>();
        let app_for_progress = app_tx.clone();
        let wrap_for_progress = wrap.clone();
        let progress_pump = tokio::spawn(async move {
            while let Some(p) = prog_rx.recv().await {
                if app_for_progress
                    .send(wrap_for_progress(AnalysisEvent::LoadProgress(p)))
                    .is_err()
                {
                    break;
                }
            }
        });

        let tp = match TraceProcessor::load(
            &paths,
            &trace_path,
            cancel.clone(),
            Some(&prog_tx),
        )
        .await
        {
            Ok(tp) => tp,
            Err(e) => {
                let _ = app_tx
                    .send(wrap(AnalysisEvent::LoadFailed(format!("{e:#}"))));
                drop(prog_tx);
                let _ = progress_pump.await;
                return;
            }
        };
        drop(prog_tx);
        let _ = progress_pump.await;

        let _ = app_tx.send(wrap(AnalysisEvent::LoadReady {
            version: tp.version().map(|v| v.to_string()),
        }));

        // ── Phase 2: serve requests ──────────────────────────────────────
        let summary_ctx = SummaryContext { package_name };
        while let Some(req) = req_rx.recv().await {
            if cancel.is_cancelled() {
                break;
            }
            match req {
                WorkerRequest::RunSummary { custom_queries } => {
                    for sq in SummaryKey::all_queries(&summary_ctx) {
                        let ev = run_summary_item(&tp, sq).await;
                        if app_tx.send(wrap(ev)).is_err() {
                            break;
                        }
                    }
                    for cq in custom_queries {
                        let result = tp.query(&cq.sql).await.map_err(|e| format!("{e:#}"));
                        if app_tx
                            .send(wrap(AnalysisEvent::CustomResult {
                                name: cq.name,
                                result,
                            }))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                WorkerRequest::RunQuery { id, sql } => {
                    let result = tp.query(&sql).await.map_err(|e| format!("{e:#}"));
                    let _ = app_tx.send(wrap(AnalysisEvent::QueryResult {
                        id,
                        sql,
                        result,
                    }));
                }
                WorkerRequest::LoadSchema => {
                    let tables = load_schema_tables(&tp).await;
                    let tables_for_columns = tables.clone();
                    if app_tx
                        .send(wrap(AnalysisEvent::SchemaLoaded { tables }))
                        .is_err()
                    {
                        break;
                    }
                    // Immediately follow up with per-table column loads.
                    // Serialised through this worker (trace_processor is
                    // single-threaded anyway); a pathological trace with
                    // many hundreds of tables would delay subsequent user
                    // requests by ~seconds, so cap the scan.
                    const MAX_TABLES_FOR_COLUMNS: usize = 256;
                    let by_table = load_all_columns(
                        &tp,
                        tables_for_columns
                            .iter()
                            .take(MAX_TABLES_FOR_COLUMNS),
                    )
                    .await;
                    let _ = app_tx.send(wrap(AnalysisEvent::ColumnsLoaded { by_table }));
                }
            }
        }

        // ── Phase 3: shutdown ────────────────────────────────────────────
        let _ = tp.shutdown().await;
    });

    req_tx
}

/// Run one summary query and wrap it in an [`AnalysisEvent`] variant matched
/// to the expected shape (single cell vs. multi-row).
async fn run_summary_item(tp: &TraceProcessor, sq: SummaryQuery) -> AnalysisEvent {
    let result = tp.query(&sq.sql).await;
    if sq.multi_row {
        let outcome = match result {
            Ok(qr) => SummaryRowsOutcome::Ok(qr.rows),
            Err(e) => classify_rows_error(e),
        };
        AnalysisEvent::SummaryRows {
            key: sq.key,
            result: outcome,
        }
    } else {
        let outcome = match result {
            Ok(qr) => match qr.rows.into_iter().next() {
                Some(row) => {
                    let cell = row.cells().first().cloned().unwrap_or(Cell::Null);
                    SummaryCellOutcome::Ok(cell)
                }
                None => SummaryCellOutcome::Ok(Cell::Null),
            },
            Err(e) => classify_cell_error(e),
        };
        AnalysisEvent::SummaryCell {
            key: sq.key,
            result: outcome,
        }
    }
}

fn classify_cell_error(e: anyhow::Error) -> SummaryCellOutcome {
    let msg = format!("{e:#}");
    if is_missing_table(&msg) {
        SummaryCellOutcome::MissingTable
    } else {
        SummaryCellOutcome::Error(msg)
    }
}

fn classify_rows_error(e: anyhow::Error) -> SummaryRowsOutcome {
    let msg = format!("{e:#}");
    if is_missing_table(&msg) {
        SummaryRowsOutcome::MissingTable
    } else {
        SummaryRowsOutcome::Error(msg)
    }
}

/// Enumerate user-queryable tables / views in the loaded trace.
///
/// Perfetto's `perfetto_tables` view lists internal implementation
/// entries prefixed `__intrinsic_…` — users don't write those; they
/// query the unprefixed wrappers that exist as views. `sqlite_master`
/// is the authoritative source for both tables and views and the
/// natural filter point: skip SQLite's own metadata (`sqlite_*`) and
/// anything double-underscore-prefixed (Perfetto internals and
/// stdlib-private scratch tables).
///
/// Failure yields an empty vec — schema browsing / completion is a
/// nicety, not a feature-gate, so we swallow errors rather than
/// surfacing them.
async fn load_schema_tables(tp: &TraceProcessor) -> Vec<String> {
    const SQL: &str = "SELECT name FROM sqlite_master \
         WHERE type IN ('table','view') \
           AND substr(name, 1, 2) != '__' \
           AND name NOT LIKE 'sqlite_%' \
         ORDER BY name";
    let Ok(qr) = tp.query(SQL).await else {
        return Vec::new();
    };
    qr.rows
        .into_iter()
        .filter_map(|r| r.cells().first().and_then(|c| c.as_str_opt()).map(|s| s.to_string()))
        .collect()
}

/// Fetch column lists for every table via `PRAGMA table_info`. Tables
/// whose PRAGMA errors (views on stdlib that don't accept the pragma,
/// etc.) are silently skipped. Identifiers are double-quoted with `"`
/// escaped as `""` to make the PRAGMA safe across any table name
/// trace_processor might return.
async fn load_all_columns<'a, I>(
    tp: &TraceProcessor,
    tables: I,
) -> std::collections::HashMap<String, Vec<String>>
where
    I: IntoIterator<Item = &'a String>,
{
    let mut out = std::collections::HashMap::new();
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let sql = format!("PRAGMA table_info(\"{}\")", quoted);
        let Ok(qr) = tp.query(&sql).await else {
            continue;
        };
        // `PRAGMA table_info` columns: (cid, name, type, notnull, dflt_value, pk).
        // We want column index 1 (name).
        let cols: Vec<String> = qr
            .rows
            .iter()
            .filter_map(|r| r.cells().get(1).and_then(|c| c.as_str_opt()).map(str::to_string))
            .collect();
        if !cols.is_empty() {
            out.insert(table.clone(), cols);
        }
    }
    out
}

/// Detect the trace_processor errors we want to downgrade to a soft
/// "metric unavailable" state: missing SQLite tables and missing stdlib
/// modules (both common on older traces).
pub(crate) fn is_missing_table(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("no such table")
        || lower.contains("no such column")
        || lower.contains("unknown module")
        || lower.contains("no such module")
        || lower.contains("failed to find module")
        || lower.contains("could not find module")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_table_detects_common_phrasings() {
        assert!(is_missing_table("error: no such table: actual_frame_timeline_slice"));
        assert!(is_missing_table(
            "query failed: No such module: android.startup.startups"
        ));
        assert!(is_missing_table(
            "Failed to find module 'android.startup.startups'"
        ));
        assert!(!is_missing_table("syntax error near SELECT"));
    }
}
