//! Walk a Macrobenchmark output directory and pair each benchmark method
//! (from the `-benchmarkData.json` files) to its per-iteration trace files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::benchmark_json::{self, Benchmark};

/// One benchmark method's discovered inputs, ready to hand to the importer.
#[derive(Debug, Clone)]
pub struct DiscoveredBenchmark {
    /// The JSON file the summary came from — copied into the session folder.
    pub json_path: PathBuf,
    pub benchmark: Benchmark,
    /// Matched iteration traces, sorted by filename (which orders them by
    /// iteration number).
    pub traces: Vec<PathBuf>,
}

/// Non-recursive scan of `dir`: pick up every `*-benchmarkData.json`, parse it,
/// and for each `Benchmark` inside match `<className>_<methodName>_iter*.(perfetto-trace|pftrace)`.
///
/// Benchmarks without any matching traces are still returned — callers log a
/// warning and skip them.
pub fn discover(dir: &Path) -> Result<Vec<DiscoveredBenchmark>> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();

    let json_files: Vec<&PathBuf> = entries
        .iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("-benchmarkData.json"))
        })
        .collect();

    let trace_files: Vec<&PathBuf> = entries
        .iter()
        .filter(|p| is_trace_file(p))
        .collect();

    let mut out = Vec::new();
    for json in json_files {
        let parsed = benchmark_json::parse(json)?;
        for bench in parsed {
            // Macrobenchmark names trace files with the SHORT class name
            // (e.g. `StartupBenchmarks_startupHot_iter000.perfetto-trace`),
            // while the JSON's `className` is the fully-qualified name
            // (e.g. `app.example.StartupBenchmarks`). Try short first, fall
            // back to FQCN for older AGP versions / edge cases.
            let short = bench
                .class_name
                .rsplit('.')
                .next()
                .unwrap_or(&bench.class_name);
            let prefixes = [
                format!("{}_{}_iter", short, bench.method_name),
                format!("{}_{}_iter", bench.class_name, bench.method_name),
            ];
            let mut matching: Vec<PathBuf> = trace_files
                .iter()
                .filter(|t| {
                    t.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| {
                            prefixes.iter().any(|p| n.starts_with(p))
                        })
                })
                .map(|p| (*p).clone())
                .collect();
            matching.sort();
            out.push(DiscoveredBenchmark {
                json_path: json.clone(),
                benchmark: bench,
                traces: matching,
            });
        }
    }
    Ok(out)
}

fn is_trace_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "perfetto-trace" || e == "pftrace")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_json_with_matching_iter_traces() {
        let tmp = tempdir();
        std::fs::write(
            tmp.join("com.example.StartupBenchmark-benchmarkData.json"),
            r#"{"benchmarks":[
                {"name":"startupHot","className":"com.example.StartupBenchmark","metrics":{}},
                {"name":"startupCold","className":"com.example.StartupBenchmark","metrics":{}}
            ]}"#,
        )
        .unwrap();
        // Real-world Macrobenchmark output: trace files use the SHORT class
        // name and append a capture timestamp after `iterNNN`.
        std::fs::write(
            tmp.join("StartupBenchmark_startupHot_iter000_2026-04-14-03-53-39.perfetto-trace"),
            b"",
        )
        .unwrap();
        std::fs::write(
            tmp.join("StartupBenchmark_startupHot_iter001_2026-04-14-03-54-39.perfetto-trace"),
            b"",
        )
        .unwrap();
        std::fs::write(
            tmp.join("StartupBenchmark_startupCold_iter000.pftrace"),
            b"",
        )
        .unwrap();
        // A stray file that must NOT be paired.
        std::fs::write(tmp.join("unrelated.txt"), b"").unwrap();

        let results = discover(&tmp).unwrap();
        assert_eq!(results.len(), 2);

        let hot = results
            .iter()
            .find(|r| r.benchmark.method_name == "startupHot")
            .unwrap();
        assert_eq!(hot.traces.len(), 2);

        let cold = results
            .iter()
            .find(|r| r.benchmark.method_name == "startupCold")
            .unwrap();
        assert_eq!(cold.traces.len(), 1);
        assert!(cold.traces[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(".pftrace"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "perfetto-cli-discover-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
