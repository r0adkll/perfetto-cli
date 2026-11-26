//! `perfetto-cli query` — run PerfettoSQL against a captured trace without
//! the TUI. Drives the same `TraceProcessor` (pinned, SHA-verified
//! `trace_processor_shell`) as the Analysis screen; `--summary` runs that
//! screen's canned Summary queries.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::{ArgGroup, Args};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

use super::{Ctx, TraceView, resolve_session};
use crate::db::traces::TraceRecord;
use crate::perfetto::capture::Cancel;
use crate::trace_processor::{
    self, Cell, DownloadProgress, LoadProgress, PINNED_VERSION, QueryResult, TraceProcessor,
};
use crate::tui::screens::analysis::summary::{SummaryContext, SummaryKey, escape_sql_literal};
use crate::tui::screens::analysis::worker::is_missing_table;

/// Placeholder substituted with the session's package name (SQL-escaped),
/// matching the TUI query library's templates.
const PACKAGE_PLACEHOLDER: &str = "{{package}}";
/// Human-mode cell width cap; JSON output is never truncated.
const MAX_CELL_CHARS: usize = 80;

#[derive(Args)]
#[command(group(ArgGroup::new("what").required(true).args(["sql", "file", "summary"])))]
pub struct QueryArgs {
    /// Session id, name, or folder slug.
    session: String,
    /// Trace id, name (label or file stem), or file name. Defaults to the
    /// session's most recent trace.
    #[arg(long, value_name = "ID|NAME")]
    trace: Option<String>,
    /// PerfettoSQL to run. `{{package}}` expands to the session's package.
    /// Multiple `;`-separated statements are allowed; the last one's rows
    /// are returned.
    #[arg(long)]
    sql: Option<String>,
    /// Read the PerfettoSQL from a file (`-` for stdin).
    #[arg(long, value_name = "PATH")]
    file: Option<PathBuf>,
    /// Run the Analysis screen's Summary metrics (frames/jank, main-thread
    /// time and top slices, startup, RSS) scoped to the session's package.
    #[arg(long)]
    summary: bool,
}

pub async fn run_query(ctx: &Ctx, args: QueryArgs) -> Result<()> {
    let session = resolve_session(&ctx.db, &args.session)?;
    let session_id = session.id.context("session has no id")?;
    let traces = ctx.db.list_traces(session_id)?;
    let trace = select_trace(&traces, args.trace.as_deref())
        .with_context(|| format!("in session '{}'", session.name))?;
    if !trace.file_path.is_file() {
        bail!("trace file {} is missing", trace.file_path.display());
    }

    let sql = match (&args.sql, &args.file) {
        (Some(sql), _) => Some(sql.clone()),
        (None, Some(path)) => Some(read_sql_file(path)?),
        (None, None) => None,
    }
    .map(|sql| expand_package(&sql, &session.package_name));
    if sql.as_deref().is_some_and(|s| s.trim().is_empty()) {
        bail!("SQL is empty");
    }

    let cancel = Cancel::new();
    let work = async {
        let tp = load_trace(ctx, &trace, cancel.clone()).await?;
        let header = Header {
            session_id,
            session_name: session.name.clone(),
            package: session.package_name.clone(),
            trace: TraceView::from(&trace),
            trace_processor: tp.version().map(str::to_string),
        };
        let out = match &sql {
            Some(sql) => {
                let result = tp.query(sql).await;
                Output::Query(QueryOutput::new(header, result?))
            }
            None => Output::Summary(run_summary(&tp, header).await),
        };
        let _ = tp.shutdown().await;
        Ok::<_, anyhow::Error>(out)
    };
    // Dropping `work` on Ctrl-C drops the TraceProcessor, which kills the
    // subprocess instead of orphaning it.
    let out = tokio::select! {
        out = work => out?,
        _ = tokio::signal::ctrl_c() => {
            cancel.cancel();
            bail!("interrupted");
        }
    };

    match &out {
        Output::Query(q) => ctx.emit(q, || q.print()),
        Output::Summary(s) => ctx.emit(s, || s.print()),
    }
}

/// Pick the trace to analyze. `traces` is newest-first (as returned by
/// `list_traces`), so no selector means the head.
fn select_trace(traces: &[TraceRecord], selector: Option<&str>) -> Result<TraceRecord> {
    let Some(selector) = selector else {
        return traces.first().cloned().context("session has no traces");
    };
    if let Ok(id) = selector.parse::<i64>()
        && let Some(t) = traces.iter().find(|t| t.id == id)
    {
        return Ok(t.clone());
    }
    let matches: Vec<&TraceRecord> = traces
        .iter()
        .filter(|t| {
            t.label.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(selector))
                || t.file_path.file_stem().is_some_and(|f| f == selector)
                || t.file_path.file_name().is_some_and(|f| f == selector)
        })
        .collect();
    match matches.as_slice() {
        [] => bail!("no trace matching '{selector}' (see `perfetto-cli session show`)"),
        [one] => Ok((*one).clone()),
        many => {
            let ids: Vec<String> = many.iter().map(|t| format!("#{}", t.id)).collect();
            bail!(
                "'{selector}' matches multiple traces ({}); pass the id instead",
                ids.join(", ")
            )
        }
    }
}

fn read_sql_file(path: &Path) -> Result<String> {
    if path.as_os_str() == "-" {
        let mut sql = String::new();
        std::io::stdin()
            .read_to_string(&mut sql)
            .context("reading SQL from stdin")?;
        return Ok(sql);
    }
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn expand_package(sql: &str, package: &str) -> String {
    sql.replace(PACKAGE_PLACEHOLDER, &escape_sql_literal(package))
}

/// Install/upgrade the pinned `trace_processor_shell` (reporting download
/// progress on stderr), then spawn it and load the trace. `.tar` in-process
/// bundles load as-is — trace_processor ≥ v58 merges them natively.
async fn load_trace(
    ctx: &Ctx,
    trace: &TraceRecord,
    cancel: std::sync::Arc<Cancel>,
) -> Result<TraceProcessor> {
    let (dl_tx, mut dl_rx) = mpsc::unbounded_channel::<DownloadProgress>();
    let dl_printer = tokio::spawn(async move {
        let mut announced = false;
        while let Some(p) = dl_rx.recv().await {
            if !announced {
                announced = true;
                let size = p
                    .total_bytes
                    .map(|b| format!(" ({} MB)", b / (1024 * 1024)))
                    .unwrap_or_default();
                eprintln!("• Downloading trace_processor_shell {PINNED_VERSION}{size}…");
            }
        }
        announced
    });
    let installed = trace_processor::ensure_binary(&ctx.paths, Some(&dl_tx), cancel.clone()).await;
    drop(dl_tx);
    if dl_printer.await.unwrap_or(false) && installed.is_ok() {
        eprintln!("✓ Installed trace_processor_shell {PINNED_VERSION}");
    }
    installed.context("installing trace_processor_shell")?;

    let size = trace.size_bytes.unwrap_or(0);
    eprintln!(
        "• Loading {} ({} MB)…",
        trace.file_path.display(),
        size / (1024 * 1024)
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<LoadProgress>();
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let tp = TraceProcessor::load(&ctx.paths, &trace.file_path, cancel, Some(&tx)).await;
    drop(tx);
    let _ = drain.await;
    let tp = tp.context("loading trace into trace_processor_shell")?;
    eprintln!(
        "✓ Loaded with trace_processor {}",
        tp.version().unwrap_or(PINNED_VERSION)
    );
    Ok(tp)
}

async fn run_summary(tp: &TraceProcessor, header: Header) -> SummaryOutput {
    let ctx = SummaryContext {
        package_name: header.package.clone(),
    };
    let mut metrics = Vec::new();
    for sq in SummaryKey::all_queries(&ctx) {
        let metric = match tp.query(&sq.sql).await {
            Ok(qr) if sq.multi_row => Metric::Rows {
                columns: qr.columns.clone(),
                rows: json_rows(&qr),
            },
            Ok(qr) => Metric::Value {
                value: qr
                    .rows
                    .first()
                    .and_then(|r| r.cells().first())
                    .map(cell_json)
                    .unwrap_or(Value::Null),
            },
            Err(e) => {
                let msg = format!("{e:#}");
                if is_missing_table(&msg) {
                    Metric::Missing
                } else {
                    Metric::Error { error: msg }
                }
            }
        };
        metrics.push((sq.key.id(), metric));
    }
    SummaryOutput { header, metrics }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

enum Output {
    Query(QueryOutput),
    Summary(SummaryOutput),
}

#[derive(Serialize)]
struct Header {
    session_id: i64,
    session_name: String,
    package: String,
    trace: TraceView,
    /// Version string `trace_processor_shell` reported.
    trace_processor: Option<String>,
}

impl Header {
    fn print(&self) {
        println!(
            "Trace #{} ({}) — session '{}' [{}]",
            self.trace.id, self.trace.path, self.session_name, self.package
        );
    }
}

#[derive(Serialize)]
struct QueryOutput {
    #[serde(flatten)]
    header: Header,
    columns: Vec<String>,
    /// One array per row, values aligned with `columns`.
    rows: Vec<Vec<Value>>,
    row_count: usize,
}

impl QueryOutput {
    fn new(header: Header, qr: QueryResult) -> Self {
        Self {
            header,
            rows: json_rows(&qr),
            row_count: qr.rows.len(),
            columns: qr.columns,
        }
    }

    fn print(&self) {
        self.header.print();
        println!();
        print!("{}", format_table(&self.columns, &self.rows));
        println!(
            "({} row{})",
            self.row_count,
            if self.row_count == 1 { "" } else { "s" }
        );
    }
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Metric {
    /// Single-value metric.
    #[serde(rename = "ok")]
    Value { value: Value },
    /// Multi-row metric.
    #[serde(rename = "ok")]
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    /// The trace lacks the table/module this metric needs (e.g. no frame
    /// timeline on older Android).
    Missing,
    Error { error: String },
}

struct SummaryOutput {
    header: Header,
    metrics: Vec<(&'static str, Metric)>,
}

impl Serialize for SummaryOutput {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct View<'a> {
            #[serde(flatten)]
            header: &'a Header,
            metrics: BTreeMap<&'static str, &'a Metric>,
        }
        View {
            header: &self.header,
            metrics: self.metrics.iter().map(|(k, m)| (*k, m)).collect(),
        }
        .serialize(s)
    }
}

impl SummaryOutput {
    fn print(&self) {
        self.header.print();
        let width = self.metrics.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (name, metric) in &self.metrics {
            match metric {
                Metric::Value { value } => println!("  {name:<width$}  {}", value_text(value)),
                Metric::Missing => println!("  {name:<width$}  — (not in trace)"),
                Metric::Error { error } => println!("  {name:<width$}  error: {error}"),
                Metric::Rows { rows, .. } if rows.is_empty() => {
                    println!("  {name:<width$}  (no rows)")
                }
                Metric::Rows { columns, rows } => {
                    println!("\n  {name}");
                    for line in format_table(columns, rows).lines() {
                        println!("    {line}");
                    }
                    println!();
                }
            }
        }
    }
}

fn json_rows(qr: &QueryResult) -> Vec<Vec<Value>> {
    qr.rows
        .iter()
        .map(|r| r.cells().iter().map(cell_json).collect())
        .collect()
}

/// Ints and floats become JSON numbers (non-finite floats → `null`),
/// blobs base64 strings.
fn cell_json(cell: &Cell) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Int(v) => Value::from(*v),
        Cell::Float(v) => serde_json::Number::from_f64(*v)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Cell::String(s) => Value::String(s.clone()),
        Cell::Blob(b) => Value::String(STANDARD.encode(b)),
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Plain-text table: header, dashed rule, left-aligned cells truncated to
/// [`MAX_CELL_CHARS`].
fn format_table(columns: &[String], rows: &[Vec<Value>]) -> String {
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| r.iter().map(|v| truncate(&value_text(v))).collect())
        .collect();
    let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for row in &cells {
        for (w, c) in widths.iter_mut().zip(row) {
            *w = (*w).max(c.chars().count());
        }
    }
    let line = |items: &mut dyn Iterator<Item = &str>| -> String {
        let padded: Vec<String> = items
            .zip(&widths)
            .map(|(s, w)| format!("{s}{}", " ".repeat(w - s.chars().count())))
            .collect();
        format!("{}\n", padded.join("  ").trim_end())
    };
    let mut out = line(&mut columns.iter().map(String::as_str));
    let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    out.push_str(&line(&mut rule.iter().map(String::as_str)));
    for row in &cells {
        out.push_str(&line(&mut row.iter().map(String::as_str)));
    }
    out
}

fn truncate(s: &str) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= MAX_CELL_CHARS {
        return s;
    }
    let mut out: String = s.chars().take(MAX_CELL_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn trace(id: i64, file: &str, label: Option<&str>) -> TraceRecord {
        TraceRecord {
            id,
            session_id: 1,
            file_path: PathBuf::from("/s/traces").join(file),
            label: label.map(str::to_string),
            duration_ms: None,
            size_bytes: None,
            captured_at: Utc::now(),
            tags: Vec::new(),
            uploads: Default::default(),
        }
    }

    #[test]
    fn select_trace_defaults_to_newest_and_matches_id_label_stem() {
        // Newest-first, as list_traces returns them.
        let traces = vec![
            trace(7, "after-fix.tar", None),
            trace(5, "2026-01-01_10-00-00.pftrace", Some("Baseline")),
        ];
        assert_eq!(select_trace(&traces, None).unwrap().id, 7);
        assert_eq!(select_trace(&traces, Some("5")).unwrap().id, 5);
        assert_eq!(select_trace(&traces, Some("baseline")).unwrap().id, 5);
        assert_eq!(select_trace(&traces, Some("after-fix")).unwrap().id, 7);
        assert_eq!(select_trace(&traces, Some("after-fix.tar")).unwrap().id, 7);
        assert!(select_trace(&traces, Some("missing")).is_err());
        assert!(select_trace(&[], None).is_err());
    }

    #[test]
    fn select_trace_ambiguous_name_errors() {
        let traces = vec![trace(2, "run.tar", None), trace(1, "run.pftrace", None)];
        let err = select_trace(&traces, Some("run")).unwrap_err().to_string();
        assert!(err.contains("#2, #1"), "{err}");
    }

    #[test]
    fn package_placeholder_is_escaped() {
        assert_eq!(
            expand_package("WHERE name = '{{package}}'", "com.o'brien"),
            "WHERE name = 'com.o''brien'"
        );
    }

    #[test]
    fn cells_map_to_json() {
        assert_eq!(cell_json(&Cell::Null), Value::Null);
        assert_eq!(cell_json(&Cell::Int(-3)), Value::from(-3));
        assert_eq!(cell_json(&Cell::Float(1.5)), Value::from(1.5));
        assert_eq!(cell_json(&Cell::Float(f64::NAN)), Value::Null);
        assert_eq!(cell_json(&Cell::String("x".into())), Value::from("x"));
        assert_eq!(cell_json(&Cell::Blob(vec![1, 2, 3])), Value::from("AQID"));
    }

    #[test]
    fn table_aligns_columns() {
        let cols = vec!["name".to_string(), "n".to_string()];
        let rows = vec![
            vec![Value::from("Choreographer#doFrame"), Value::from(12)],
            vec![Value::from("inflate"), Value::Null],
        ];
        assert_eq!(
            format_table(&cols, &rows),
            "name                   n\n\
             ---------------------  ----\n\
             Choreographer#doFrame  12\n\
             inflate                NULL\n"
        );
    }

    #[test]
    fn metric_json_shapes() {
        let v = serde_json::to_value(Metric::Value { value: Value::from(4) }).unwrap();
        assert_eq!(v, serde_json::json!({"status": "ok", "value": 4}));
        let v = serde_json::to_value(Metric::Missing).unwrap();
        assert_eq!(v, serde_json::json!({"status": "missing"}));
        let v = serde_json::to_value(Metric::Rows {
            columns: vec!["a".into()],
            rows: vec![vec![Value::from(1)]],
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"status": "ok", "columns": ["a"], "rows": [[1]]})
        );
    }
}
