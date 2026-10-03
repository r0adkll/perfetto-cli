//! In-process traces from `androidx.tracing:tracing-wire` (Tracing 2.0).
//!
//! The library ships a shell-only `ConnectedProfilerTracingReceiver` that
//! profilers drive with three broadcasts:
//!
//! - `START` clears stale trace files and enables tracing. The enabled bit is
//!   persisted as a component-enabled state, so it survives `am force-stop`
//!   and is read again when the next process initializes — that's what makes
//!   cold-start capture work.
//! - `FLUSH_TRACES_GET_PATH` flushes the driver and copies every
//!   `*.perfetto-trace` file into the app's external media dir, returning
//!   that path as the broadcast's result data.
//! - `STOP` disables tracing again.
//!
//! The pulled files are bundled with the system trace into a TAR, which
//! Trace Processor (v58+) and ui.perfetto.dev open as one merged timeline.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedSender;

use crate::adb;

use super::capture::{CaptureEvent, LogLevel, log};

const RECEIVER: &str = "androidx.tracing.profiler.ConnectedProfilerTracingReceiver";
const ACTION_START: &str = "androidx.tracing.profiler.action.START";
const ACTION_FLUSH: &str = "androidx.tracing.profiler.action.FLUSH_TRACES_GET_PATH";
const ACTION_STOP: &str = "androidx.tracing.profiler.action.STOP";

const RESULT_SUCCESS: i32 = 1;
const RESULT_FLUSH_COMPLETED: i32 = 2;
const RESULT_DELAYED_TRACE_DRIVER: i32 = -2;

const TRACE_EXT: &str = ".perfetto-trace";

/// Enable in-process tracing in `package`. Returns `false` (after logging a
/// warning) when the app doesn't ship `tracing-wire` — the capture carries on
/// as a plain system trace.
pub async fn start(serial: &str, package: &str, tx: &UnboundedSender<CaptureEvent>) -> bool {
    log(
        tx,
        LogLevel::Info,
        format!("Enabling in-process tracing in {package}"),
    );
    match broadcast(serial, package, ACTION_START).await {
        Ok((RESULT_SUCCESS, _)) => {
            log(tx, LogLevel::Ok, "in-process tracing enabled".into());
            true
        }
        Ok((code, _)) => {
            log(
                tx,
                LogLevel::Warn,
                format!(
                    "in-process tracing not enabled (result={code}) — is androidx.tracing:tracing-wire on the app classpath?"
                ),
            );
            false
        }
        Err(e) => {
            log(
                tx,
                LogLevel::Warn,
                format!("in-process tracing broadcast failed: {e}"),
            );
            false
        }
    }
}

/// Device-local timestamp in the library's file-name format
/// (`yyyy-MM-dd-HH-mm-ss`). Used as a lower bound to drop trace files left
/// behind by a process that predates a cold start.
pub async fn device_timestamp(serial: &str) -> Option<String> {
    adb::run(serial, &["shell", "date", "+%Y-%m-%d-%H-%M-%S"])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Flush the app's in-process traces, pull them into `staging_dir`, and
/// disable tracing again. Every failure is soft — the system trace is still
/// valid on its own — so this returns whatever it managed to pull.
pub async fn collect(
    serial: &str,
    package: &str,
    staging_dir: &Path,
    not_before: Option<&str>,
    tx: &UnboundedSender<CaptureEvent>,
) -> Vec<PathBuf> {
    let pulled = flush_and_pull(serial, package, staging_dir, not_before, tx)
        .await
        .unwrap_or_else(|e| {
            log(
                tx,
                LogLevel::Warn,
                format!("in-process trace collection failed: {e:#}"),
            );
            Vec::new()
        });
    if let Err(e) = broadcast(serial, package, ACTION_STOP).await {
        log(
            tx,
            LogLevel::Warn,
            format!("failed to disable in-process tracing: {e}"),
        );
    }
    pulled
}

async fn flush_and_pull(
    serial: &str,
    package: &str,
    staging_dir: &Path,
    not_before: Option<&str>,
    tx: &UnboundedSender<CaptureEvent>,
) -> Result<Vec<PathBuf>> {
    log(tx, LogLevel::Info, "Flushing in-process traces".into());
    let (code, data) = broadcast(serial, package, ACTION_FLUSH).await?;
    let device_dir = match (code, data) {
        (RESULT_FLUSH_COMPLETED, Some(dir)) => dir,
        (RESULT_DELAYED_TRACE_DRIVER, Some(dir)) => {
            log(
                tx,
                LogLevel::Warn,
                "trace driver wasn't initialized at app startup — in-process trace may be empty"
                    .into(),
            );
            dir
        }
        (code, _) => anyhow::bail!("flush returned result={code}"),
    };

    let listing = adb::run(serial, &["shell", "ls", "-1", &device_dir])
        .await
        .context("list in-process traces")?;
    let names = select_trace_files(&listing, not_before);
    if names.is_empty() {
        log(
            tx,
            LogLevel::Warn,
            "app produced no in-process traces".into(),
        );
        return Ok(Vec::new());
    }

    std::fs::create_dir_all(staging_dir).context("create staging dir")?;
    let mut pulled = Vec::new();
    for name in names {
        let remote = format!("{device_dir}/{name}");
        let local = staging_dir.join(&name);
        let local_str = local.to_str().context("staging path is not valid UTF-8")?;
        adb::run(serial, &["pull", &remote, local_str])
            .await
            .with_context(|| format!("adb pull {remote}"))?;
        let _ = adb::run(serial, &["shell", "rm", "-f", &remote]).await;
        // The driver writes process/thread descriptors even when every
        // category is off, so an empty file is just noise.
        if std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0) > 0 {
            pulled.push(local);
        }
    }
    log(
        tx,
        LogLevel::Ok,
        format!("pulled {} in-process trace file(s)", pulled.len()),
    );
    Ok(pulled)
}

/// Explicit-component broadcast (implicit ones don't reach manifest
/// receivers on O+). Returns the parsed `(result, data)`.
async fn broadcast(serial: &str, package: &str, action: &str) -> Result<(i32, Option<String>)> {
    let component = format!("{package}/{RECEIVER}");
    let out = adb::run(
        serial,
        &["shell", "am", "broadcast", "-a", action, "-n", &component],
    )
    .await?;
    parse_broadcast_result(&out)
        .with_context(|| format!("unexpected `am broadcast` output: {}", out.trim()))
}

/// Parse `Broadcast completed: result=2, data="/storage/…"`.
fn parse_broadcast_result(stdout: &str) -> Option<(i32, Option<String>)> {
    let line = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("Broadcast completed:"))?;
    let rest = line.trim().strip_prefix("result=")?;
    let (code, tail) = match rest.split_once(',') {
        Some((code, tail)) => (code, Some(tail)),
        None => (rest, None),
    };
    let code = code.trim().parse().ok()?;
    let data = tail
        .and_then(|t| t.trim().strip_prefix("data=\""))
        .and_then(|t| t.split_once('"'))
        .map(|(d, _)| d.to_string());
    Some((code, data))
}

/// Trace files from an `ls -1` listing. Files are named
/// `perfetto-<yyyy-MM-dd-HH-mm-ss>-<n>.perfetto-trace`, so `not_before`
/// (same format) compares lexicographically.
fn select_trace_files(listing: &str, not_before: Option<&str>) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|n| n.ends_with(TRACE_EXT))
        .filter(|n| match (not_before, file_timestamp(n)) {
            (Some(floor), Some(ts)) => ts >= floor,
            _ => true,
        })
        .map(str::to_string)
        .collect()
}

fn file_timestamp(name: &str) -> Option<&str> {
    let ts = name.strip_prefix("perfetto-")?.get(..19)?;
    ts.bytes()
        .all(|b| b.is_ascii_digit() || b == b'-')
        .then_some(ts)
}

/// Write `system_trace` plus every in-process trace into a TAR at `out`.
pub fn bundle(system_trace: &Path, in_process: &[PathBuf], out: &Path) -> Result<()> {
    let file = std::fs::File::create(out).with_context(|| format!("create {}", out.display()))?;
    let mut builder = tar::Builder::new(file);
    builder
        .append_path_with_name(system_trace, "system.pftrace")
        .context("add system trace to bundle")?;
    for path in in_process {
        let name = path
            .file_name()
            .context("in-process trace has no filename")?;
        builder
            .append_path_with_name(path, name)
            .with_context(|| format!("add {} to bundle", path.display()))?;
    }
    builder.into_inner()?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flush_result_with_data() {
        let out = "Broadcasting: Intent { act=androidx.tracing.profiler.action.FLUSH_TRACES_GET_PATH flg=0x400000 cmp=com.example/androidx.tracing.profiler.ConnectedProfilerTracingReceiver }\n\
                   Broadcast completed: result=2, data=\"/storage/emulated/0/Android/media/com.example/perfetto_traces\"\n";
        assert_eq!(
            parse_broadcast_result(out),
            Some((
                2,
                Some("/storage/emulated/0/Android/media/com.example/perfetto_traces".into())
            ))
        );
    }

    #[test]
    fn parses_result_without_data() {
        assert_eq!(
            parse_broadcast_result("Broadcasting: Intent {}\nBroadcast completed: result=1\n"),
            Some((1, None))
        );
        assert_eq!(
            parse_broadcast_result("Broadcast completed: result=-2, data=\"/x\""),
            Some((-2, Some("/x".into())))
        );
    }

    #[test]
    fn missing_completion_line_is_none() {
        assert_eq!(parse_broadcast_result("Broadcasting: Intent {}\n"), None);
        assert_eq!(parse_broadcast_result(""), None);
    }

    #[test]
    fn selects_only_trace_files() {
        let listing = "perfetto-2026-10-03-10-00-00-0.perfetto-trace\nnotes.txt\n\
                       perfetto-2026-10-03-10-00-00-1.perfetto-trace\n";
        assert_eq!(
            select_trace_files(listing, None),
            vec![
                "perfetto-2026-10-03-10-00-00-0.perfetto-trace",
                "perfetto-2026-10-03-10-00-00-1.perfetto-trace",
            ]
        );
    }

    #[test]
    fn drops_files_older_than_floor() {
        let listing = "perfetto-2026-10-03-09-59-59-0.perfetto-trace\n\
                       perfetto-2026-10-03-10-00-00-0.perfetto-trace\n\
                       perfetto-2026-10-03-10-00-05-0.perfetto-trace\n\
                       custom-name-0.perfetto-trace\n";
        assert_eq!(
            select_trace_files(listing, Some("2026-10-03-10-00-00")),
            vec![
                "perfetto-2026-10-03-10-00-00-0.perfetto-trace",
                "perfetto-2026-10-03-10-00-05-0.perfetto-trace",
                // Unparseable names are kept rather than silently dropped.
                "custom-name-0.perfetto-trace",
            ]
        );
    }

    #[test]
    fn bundle_writes_tar_with_all_traces() {
        let dir = std::env::temp_dir().join(format!(
            "perfetto-cli-bundle-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let system = dir.join("sys.pftrace");
        let app = dir.join("perfetto-2026-10-03-10-00-00-0.perfetto-trace");
        std::fs::write(&system, b"system").unwrap();
        std::fs::write(&app, b"app").unwrap();
        let out = dir.join("bundle.tar");

        bundle(&system, std::slice::from_ref(&app), &out).unwrap();

        let mut archive = tar::Archive::new(std::fs::File::open(&out).unwrap());
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "system.pftrace",
                "perfetto-2026-10-03-10-00-00-0.perfetto-trace",
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
