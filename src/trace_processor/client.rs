//! `TraceProcessor` — spawns `trace_processor_shell -D --http-port N` and
//! drives the parse/query lifecycle over the legacy HTTP endpoints.
//!
//! Each instance owns one subprocess bound to one loaded trace. Drop it (or
//! call [`TraceProcessor::shutdown`]) to stop the subprocess.

use std::collections::VecDeque;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, AsyncReadExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Paths;
use crate::perfetto::capture::Cancel;
use crate::trace_processor::binary;
use crate::trace_processor::http;
use crate::trace_processor::proto::{
    AppendTraceDataResult, QueryArgs, QueryResult as ProtoQueryResult, StatusResult,
};
use crate::trace_processor::query::{self, QueryResult};

const SPAWN_ATTEMPTS: usize = 3;
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const READINESS_TIMEOUT: Duration = Duration::from_secs(10);
const PARSE_CHUNK_BYTES: usize = 32 * 1024 * 1024;
const STDERR_TAIL_LINES: usize = 64;

/// Progress events emitted while ingesting a trace into `trace_processor_shell`.
#[derive(Debug, Clone)]
pub enum LoadProgress {
    /// Bytes streamed to `/parse` so far out of the file's total size.
    Parse { bytes_so_far: u64, total_bytes: u64 },
    /// The subprocess acknowledged end-of-stream.
    Finalized,
}

/// A running `trace_processor_shell -D` subprocess with a loaded trace.
pub struct TraceProcessor {
    child: Option<Child>,
    http: reqwest::Client,
    base_url: String,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    binary_path: PathBuf,
    version: Option<String>,
}

impl TraceProcessor {
    /// Ensure the binary exists, spawn the daemon, load the trace, and return
    /// a ready-to-query handle.
    pub async fn load(
        paths: &Paths,
        trace_path: &Path,
        cancel: Arc<Cancel>,
        progress: Option<&UnboundedSender<LoadProgress>>,
    ) -> Result<Self> {
        let binary_path = binary::ensure_binary(paths, None, cancel.clone()).await?;
        let mut tp = Self::spawn(&binary_path, cancel.clone()).await?;
        tp.parse_trace(trace_path, cancel.clone(), progress).await?;
        tp.notify_eof().await?;
        if let Some(tx) = progress {
            let _ = tx.send(LoadProgress::Finalized);
        }
        Ok(tp)
    }

    /// Spawn the subprocess and poll `/status` until it's accepting requests.
    /// Retries up to [`SPAWN_ATTEMPTS`] times to cover the port-binding race.
    async fn spawn(binary: &Path, cancel: Arc<Cancel>) -> Result<Self> {
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=SPAWN_ATTEMPTS {
            match Self::spawn_once(binary, cancel.clone()).await {
                Ok(tp) => return Ok(tp),
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "trace_processor spawn attempt failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("trace_processor spawn failed")))
    }

    async fn spawn_once(binary: &Path, cancel: Arc<Cancel>) -> Result<Self> {
        let port = pick_free_port()?;
        tracing::info!(port, "spawning trace_processor_shell -D");

        let mut child = Command::new(binary)
            .arg("-D")
            .arg("--http-port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning {}", binary.display()))?;

        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        drain_stdout(child.stdout.take().expect("piped stdout"));
        drain_stderr(
            child.stderr.take().expect("piped stderr"),
            stderr_tail.clone(),
        );

        let base_url = format!("http://127.0.0.1:{port}");
        let http_client = reqwest::Client::builder()
            .user_agent(concat!("perfetto-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building reqwest client")?;

        // Readiness loop.
        let version = match wait_ready(&http_client, &base_url, cancel.clone()).await {
            Ok(v) => v,
            Err(e) => {
                // Kill the child before bailing so we don't leak it.
                let _ = child.start_kill();
                let tail = snapshot_tail(&stderr_tail);
                return Err(e.context(format!(
                    "trace_processor_shell never became ready (stderr tail: {tail})"
                )));
            }
        };

        Ok(Self {
            child: Some(child),
            http: http_client,
            base_url,
            stderr_tail,
            binary_path: binary.to_path_buf(),
            version,
        })
    }

    async fn parse_trace(
        &mut self,
        trace_path: &Path,
        cancel: Arc<Cancel>,
        progress: Option<&UnboundedSender<LoadProgress>>,
    ) -> Result<()> {
        let mut file = tokio::fs::File::open(trace_path)
            .await
            .with_context(|| format!("opening {}", trace_path.display()))?;
        let total_bytes = file
            .metadata()
            .await
            .map(|m| m.len())
            .with_context(|| format!("stat {}", trace_path.display()))?;
        let mut buf = vec![0u8; PARSE_CHUNK_BYTES];
        let mut bytes_so_far: u64 = 0;
        let parse_url = format!("{}/parse", self.base_url);

        loop {
            if cancel.is_cancelled() {
                bail!("trace load cancelled");
            }
            let n = tokio::select! {
                _ = cancel.wait() => bail!("trace load cancelled"),
                r = file.read(&mut buf) => r.context("reading trace file")?,
            };
            if n == 0 {
                break;
            }
            let result: AppendTraceDataResult = self.with_crash_context(
                http::post_bytes(&self.http, &parse_url, buf[..n].to_vec()).await,
            )?;
            if let Some(err) = result.error.as_deref().filter(|e| !e.is_empty()) {
                bail!("trace_processor /parse reported: {err}");
            }
            bytes_so_far += n as u64;
            if let Some(tx) = progress {
                let _ = tx.send(LoadProgress::Parse {
                    bytes_so_far,
                    total_bytes,
                });
            }
        }
        Ok(())
    }

    async fn notify_eof(&self) -> Result<()> {
        let url = format!("{}/notify_eof", self.base_url);
        self.with_crash_context(http::get_ok(&self.http, &url).await)
    }

    /// Execute one SQL statement and return the decoded rows.
    pub async fn query(&self, sql: &str) -> Result<QueryResult> {
        let url = format!("{}/query", self.base_url);
        let req = QueryArgs {
            sql_query: Some(sql.to_string()),
            tag: None,
        };
        let raw: ProtoQueryResult =
            self.with_crash_context(http::post_proto(&self.http, &url, &req).await)?;
        if let Some(err) = raw.error.as_deref().filter(|e| !e.is_empty()) {
            bail!("query failed: {err}");
        }
        query::decode(raw)
    }

    /// Version string reported by `/status` when we first connected.
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// Absolute path to the `trace_processor_shell` binary this instance drives.
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    /// Gracefully stop the subprocess. Safe to call at most once.
    pub async fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner().await
    }

    async fn shutdown_inner(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
        Ok(())
    }

    /// Attach a stderr snapshot to errors so crashes aren't opaque.
    fn with_crash_context<T>(&self, result: Result<T>) -> Result<T> {
        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                let tail = snapshot_tail(&self.stderr_tail);
                if tail.is_empty() {
                    Err(e)
                } else {
                    Err(e.context(format!("trace_processor stderr tail: {tail}")))
                }
            }
        }
    }
}

impl Drop for TraceProcessor {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // kill_on_drop already handles the await; start_kill is best-effort.
            let _ = child.start_kill();
        }
    }
}

/// Bind `127.0.0.1:0` to let the OS assign a free ephemeral port, then drop
/// the listener before handing the number to the child. There's a tiny race
/// where another process could grab the port between drop and child spawn —
/// the caller retries to cover it.
fn pick_free_port() -> Result<u16> {
    let listener =
        TcpListener::bind("127.0.0.1:0").context("binding ephemeral port for trace_processor")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

async fn wait_ready(
    client: &reqwest::Client,
    base_url: &str,
    cancel: Arc<Cancel>,
) -> Result<Option<String>> {
    let status_url = format!("{base_url}/status");
    let deadline = tokio::time::Instant::now() + READINESS_TIMEOUT;
    loop {
        if cancel.is_cancelled() {
            bail!("readiness wait cancelled");
        }
        match http::get_proto::<StatusResult>(client, &status_url).await {
            Ok(status) => {
                return Ok(status.human_readable_version);
            }
            Err(e) => {
                tracing::trace!(error = %e, "waiting for trace_processor /status");
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("no response from /status after {:?}", READINESS_TIMEOUT);
        }
        tokio::select! {
            _ = cancel.wait() => bail!("readiness wait cancelled"),
            _ = tokio::time::sleep(READINESS_POLL_INTERVAL) => {}
        }
    }
}

fn drain_stdout(stdout: tokio::process::ChildStdout) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "trace_processor.stdout", "{line}");
        }
    });
}

fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            tracing::debug!(target: "trace_processor.stderr", "{line}");
            if let Ok(mut q) = tail.lock() {
                if q.len() == STDERR_TAIL_LINES {
                    q.pop_front();
                }
                q.push_back(line);
            }
        }
    });
}

fn snapshot_tail(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    match tail.lock() {
        Ok(q) => q.iter().cloned().collect::<Vec<_>>().join(" | "),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod smoke {
    //! End-to-end smoke test. Ignored by default because it downloads
    //! `trace_processor_shell` (~100 MB) and needs a real trace on disk.
    //!
    //! Run with:
    //!   PERFETTO_SMOKE_TRACE=/abs/path/trace.pftrace cargo test --release \
    //!     trace_processor::client::smoke -- --ignored --nocapture

    use super::*;
    use crate::config::Paths;

    #[tokio::test]
    #[ignore]
    async fn load_and_query_real_trace() {
        let trace_path = match std::env::var("PERFETTO_SMOKE_TRACE") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                eprintln!("set PERFETTO_SMOKE_TRACE=/path/to/trace.pftrace to run");
                return;
            }
        };
        let paths = Paths::resolve().expect("paths");
        paths.ensure().expect("ensure");

        let cancel = Cancel::new();
        let tp = TraceProcessor::load(&paths, &trace_path, cancel, None)
            .await
            .expect("load trace");

        println!("version: {:?}", tp.version());

        let result = tp
            .query("SELECT COUNT(*) AS n FROM slice")
            .await
            .expect("query");
        assert_eq!(result.columns, vec!["n"]);
        assert_eq!(result.rows.len(), 1);
        let n = result.rows[0].get("n").unwrap().as_int().unwrap();
        println!("slice count: {n}");
        assert!(n >= 0);

        let sample = tp
            .query(
                "SELECT ts, dur, name FROM slice WHERE name IS NOT NULL ORDER BY dur DESC LIMIT 5",
            )
            .await
            .expect("sample query");
        for row in &sample.rows {
            println!(
                "ts={} dur={} name={:?}",
                row.get("ts").unwrap().as_int().unwrap(),
                row.get("dur").unwrap().as_int().unwrap(),
                row.get("name").unwrap().as_str_opt().unwrap_or("<null>"),
            );
        }

        // Exercise every summary query the TUI fires, using the package
        // name from PERFETTO_SMOKE_PACKAGE (falls back to a made-up string
        // — missing-process queries will return empty results, which is
        // the soft-fail path we want to verify compiles and runs).
        use crate::tui::screens::analysis::summary::{SummaryContext, SummaryKey};
        let package_name =
            std::env::var("PERFETTO_SMOKE_PACKAGE").unwrap_or_else(|_| "com.example.nope".into());
        let ctx = SummaryContext {
            package_name: package_name.clone(),
        };
        for sq in SummaryKey::all_queries(&ctx) {
            let res = tp.query(&sq.sql).await;
            match res {
                Ok(qr) => {
                    println!(
                        "summary {:?}: ok, {} rows, {} cols",
                        sq.key,
                        qr.rows.len(),
                        qr.columns.len()
                    );
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let soft = crate::tui::screens::analysis::is_missing_table(&msg);
                    println!("summary {:?}: err (soft={soft}): {msg}", sq.key);
                    assert!(
                        soft,
                        "unexpected hard error on summary query {:?}: {msg}",
                        sq.key
                    );
                }
            }
        }

        // Bonus: validate that every library query the REPL surfaces at
        // `Alt+I` parses and runs cleanly against this trace. We accept
        // empty results (trace may lack the underlying data source) and
        // raw errors (e.g. missing stdlib module on older traces) —
        // only a hard panic would be a regression.
        use crate::tui::screens::analysis::library::{LIBRARY, render_sql};
        println!("\n--- library queries against this trace ---");
        for entry in LIBRARY {
            let sql = render_sql(entry, &package_name);
            match tp.query(&sql).await {
                Ok(qr) => println!(
                    "library {}: ok, {} rows, {} cols",
                    entry.name,
                    qr.rows.len(),
                    qr.columns.len()
                ),
                Err(e) => println!("library {}: err: {e:#}", entry.name),
            }
        }

        tp.shutdown().await.expect("shutdown");
    }
}

