//! Import Macrobenchmark output directories as perfetto-cli sessions.
//!
//! One session per `@Test` method (see [`discovery`]). Each session owns the
//! copied benchmarkData JSON plus every matching iteration trace. The session's
//! `TraceConfig` carries a placeholder `custom_textproto` so the config editor
//! opens in read-only mode — we don't try to reconstruct the original textproto
//! from the trace in v1.

pub mod benchmark_json;
pub mod discovery;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::config::Paths;
use crate::db::Database;
use crate::perfetto::TraceConfig;
use crate::session::Session;

pub use benchmark_json::Benchmark;
pub use discovery::DiscoveredBenchmark;

const IMPORTED_TEXTPROTO_PLACEHOLDER: &str = "\
# Imported from Android Macrobenchmark output.
#
# The original TraceConfig is embedded inside each .perfetto-trace file in
# this session. perfetto-cli does not decode it, so this config cannot be
# edited here — change recording parameters in your Macrobenchmark Gradle
# setup and re-import instead.
";

/// Result of a single import run.
pub struct ImportOutcome {
    pub session_id: i64,
    pub session_name: String,
    pub folder_path: PathBuf,
    pub trace_count: usize,
}

/// Import every benchmark in `dir` as its own session. Returns one
/// [`ImportOutcome`] per created session. Benchmarks with zero matching traces
/// are skipped (and logged).
pub fn import_directory(
    db: &Database,
    paths: &Paths,
    dir: &Path,
    name_prefix: Option<&str>,
) -> Result<Vec<ImportOutcome>> {
    let discovered = discovery::discover(dir)
        .with_context(|| format!("scan {}", dir.display()))?;
    if discovered.is_empty() {
        anyhow::bail!(
            "no macrobenchmark results found in {} (looked for *-benchmarkData.json)",
            dir.display()
        );
    }

    let mut out = Vec::new();
    for d in discovered {
        if d.traces.is_empty() {
            tracing::warn!(
                class = d.benchmark.class_name,
                method = d.benchmark.method_name,
                "no iteration traces matched, skipping",
            );
            continue;
        }
        let outcome = import_one(db, paths, dir, &d, name_prefix)?;
        out.push(outcome);
    }
    Ok(out)
}

fn import_one(
    db: &Database,
    paths: &Paths,
    source_dir: &Path,
    d: &DiscoveredBenchmark,
    name_prefix: Option<&str>,
) -> Result<ImportOutcome> {
    let short_class = d
        .benchmark
        .class_name
        .rsplit('.')
        .next()
        .unwrap_or(&d.benchmark.class_name);
    let base = format!("{}.{}", short_class, d.benchmark.method_name);
    let name = match name_prefix {
        Some(p) if !p.is_empty() => format!("{p} · {base}"),
        _ => base,
    };

    let folder_path = Session::unique_folder_path(&paths.sessions_dir(), &name);
    let mut config = TraceConfig::default();
    config.custom_textproto = Some(IMPORTED_TEXTPROTO_PLACEHOLDER.to_string());

    // Copy the benchmarkData JSON into the session folder so the session is
    // self-contained and survives DB loss.
    let benchmark_json_dest = folder_path.join("benchmarkData.json");

    let session = Session {
        id: None,
        name: name.clone(),
        package_name: String::new(),
        device_serial: None,
        config,
        folder_path: folder_path.clone(),
        created_at: Utc::now(),
        notes: None,
        is_imported: true,
        benchmark_json_path: Some(benchmark_json_dest.clone()),
        import_source_dir: Some(source_dir.to_path_buf()),
    };

    session
        .ensure_filesystem()
        .with_context(|| format!("create session folder {}", folder_path.display()))?;

    std::fs::copy(&d.json_path, &benchmark_json_dest)
        .with_context(|| format!("copy {} → {}", d.json_path.display(), benchmark_json_dest.display()))?;

    let traces_dir = session.traces_dir();
    let mut copied: Vec<(PathBuf, u64, Option<String>)> = Vec::new();
    for trace in &d.traces {
        let file_name = trace
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("trace.perfetto-trace")
            .to_string();
        let dest = traces_dir.join(&file_name);
        std::fs::copy(trace, &dest)
            .with_context(|| format!("copy {} → {}", trace.display(), dest.display()))?;
        let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        let label = iter_label_from(&file_name);
        copied.push((dest, size, label));
    }

    let session_id = db
        .create_session(&session)
        .with_context(|| format!("insert session {name}"))?;

    for (path, size, label) in &copied {
        db.create_trace(session_id, path, label.as_deref(), None, Some(*size))
            .with_context(|| format!("insert trace {}", path.display()))?;
    }

    Ok(ImportOutcome {
        session_id,
        session_name: name,
        folder_path,
        trace_count: copied.len(),
    })
}

/// Extract `iter000` from `<Class>_<method>_iter000.perfetto-trace` (or
/// `<...>_iter000_<timestamp>.perfetto-trace` — Macrobenchmark appends a
/// capture timestamp on real devices) for use as the trace label. Returns
/// `None` if the filename doesn't match the expected shape — the DB column
/// is nullable.
fn iter_label_from(filename: &str) -> Option<String> {
    let stem = filename.rsplit_once('.').map(|(s, _)| s).unwrap_or(filename);
    let (_, after) = stem.rsplit_once("_iter")?;
    // Stop at the next `_` so a trailing `_<timestamp>` doesn't pollute the
    // label.
    let n = after.split_once('_').map(|(n, _)| n).unwrap_or(after);
    Some(format!("iter{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iter_label_extraction() {
        assert_eq!(
            iter_label_from("com.example.Foo_startupHot_iter003.perfetto-trace"),
            Some("iter003".into()),
        );
        assert_eq!(
            iter_label_from("com.example.Foo_startupHot_iter000.pftrace"),
            Some("iter000".into()),
        );
        assert_eq!(
            iter_label_from(
                "StartupBenchmarks_startupHot_iter000_2026-04-14-03-53-39.perfetto-trace",
            ),
            Some("iter000".into()),
        );
        assert_eq!(iter_label_from("nothing.txt"), None);
    }

    #[test]
    fn import_directory_creates_session_per_method() {
        // Build a synthetic macrobenchmark output dir.
        let src = tempdir("perfetto-cli-import-src");
        std::fs::write(
            src.join("com.example.StartupBenchmark-benchmarkData.json"),
            r#"{"benchmarks":[
                {"name":"startupHot","className":"com.example.StartupBenchmark","metrics":{
                    "timeToInitialDisplayMs":{"minimum":100.0,"median":120.0,"maximum":150.0,"runs":[100.0,120.0,150.0]}
                }},
                {"name":"startupCold","className":"com.example.StartupBenchmark","metrics":{}}
            ]}"#,
        )
        .unwrap();
        std::fs::write(
            src.join("com.example.StartupBenchmark_startupHot_iter000.perfetto-trace"),
            b"TRACE_A",
        )
        .unwrap();
        std::fs::write(
            src.join("com.example.StartupBenchmark_startupHot_iter001.perfetto-trace"),
            b"TRACE_B",
        )
        .unwrap();
        std::fs::write(
            src.join("com.example.StartupBenchmark_startupCold_iter000.pftrace"),
            b"TRACE_C",
        )
        .unwrap();

        // Isolated paths/db in a tempdir.
        let home = tempdir("perfetto-cli-import-home");
        let paths = Paths {
            config_dir: home.clone(),
        };
        paths.ensure().unwrap();
        let db = Database::open(&paths.db_file()).unwrap();
        db.migrate().unwrap();

        let outcomes = import_directory(&db, &paths, &src, Some("Run1")).unwrap();
        assert_eq!(outcomes.len(), 2);

        // Both sessions should have been created and have their traces copied.
        let sessions = db.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        for s in &sessions {
            assert!(s.is_imported);
            assert!(s.benchmark_json_path.is_some());
            assert!(s.benchmark_json_path.as_ref().unwrap().exists());
            assert_eq!(s.import_source_dir.as_deref(), Some(src.as_path()));
            assert!(s.name.starts_with("Run1 · StartupBenchmark."));
            let traces = db.list_traces(s.id.unwrap()).unwrap();
            assert!(!traces.is_empty());
            for t in traces {
                assert!(t.file_path.exists());
                assert!(t.label.as_deref().unwrap_or("").starts_with("iter"));
            }
        }

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    fn tempdir(prefix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
