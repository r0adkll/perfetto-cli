//! Parser for Android Macrobenchmark `-benchmarkData.json` result files.
//!
//! The AGP schema varies across versions (new metric shapes appear, optional
//! fields come and go) so we parse with `serde_json::Value` for the metric
//! bodies and extract what we recognize. Unknown metric blocks are surfaced
//! with empty `runs` rather than failing the whole import.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// One Macrobenchmark `@Test` method's results — the unit we turn into a
/// session. A single JSON file may contain many of these.
#[derive(Debug, Clone)]
pub struct Benchmark {
    pub class_name: String,
    pub method_name: String,
    pub metrics: Vec<Metric>,
}

/// A single summarized metric row (frame duration, startup ms, etc.).
#[derive(Debug, Clone)]
pub struct Metric {
    pub name: String,
    pub minimum: f64,
    pub median: f64,
    pub maximum: f64,
    pub runs: Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct Root {
    #[serde(default)]
    benchmarks: Vec<BenchmarkEntry>,
}

#[derive(Debug, Deserialize)]
struct BenchmarkEntry {
    /// The method name (e.g. `"startupHot"`).
    name: String,
    #[serde(rename = "className")]
    class_name: String,
    #[serde(default)]
    metrics: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct MetricEntry {
    #[serde(default)]
    minimum: Option<f64>,
    #[serde(default)]
    median: Option<f64>,
    #[serde(default)]
    maximum: Option<f64>,
    #[serde(default)]
    runs: Vec<f64>,
}

pub fn parse(path: &Path) -> Result<Vec<Benchmark>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let root: Root = serde_json::from_str(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(root
        .benchmarks
        .into_iter()
        .map(|b| Benchmark {
            class_name: b.class_name,
            method_name: b.name,
            metrics: b
                .metrics
                .into_iter()
                .map(|(name, value)| {
                    let parsed: MetricEntry =
                        serde_json::from_value(value).unwrap_or(MetricEntry {
                            minimum: None,
                            median: None,
                            maximum: None,
                            runs: Vec::new(),
                        });
                    Metric {
                        name,
                        minimum: parsed.minimum.unwrap_or(0.0),
                        median: parsed.median.unwrap_or(0.0),
                        maximum: parsed.maximum.unwrap_or(0.0),
                        runs: parsed.runs,
                    }
                })
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
  "context": { "build": {} },
  "benchmarks": [
    {
      "name": "startupHot",
      "params": {},
      "className": "com.example.StartupBenchmark",
      "totalRunTimeNs": 12345,
      "metrics": {
        "timeToInitialDisplayMs": {
          "minimum": 123.4,
          "maximum": 456.7,
          "median": 234.5,
          "runs": [123.4, 234.5, 456.7]
        },
        "frameDurationCpuMs": {
          "P50": 8.1,
          "P95": 15.0
        }
      }
    }
  ]
}"#;

    #[test]
    fn parses_benchmark_and_metrics() {
        let tmp = tempfile();
        std::fs::write(&tmp, SAMPLE).unwrap();
        let bms = parse(&tmp).unwrap();
        assert_eq!(bms.len(), 1);
        let b = &bms[0];
        assert_eq!(b.class_name, "com.example.StartupBenchmark");
        assert_eq!(b.method_name, "startupHot");
        assert_eq!(b.metrics.len(), 2);
        let ttid = b
            .metrics
            .iter()
            .find(|m| m.name == "timeToInitialDisplayMs")
            .unwrap();
        assert!((ttid.minimum - 123.4).abs() < f64::EPSILON);
        assert!((ttid.maximum - 456.7).abs() < f64::EPSILON);
        assert_eq!(ttid.runs.len(), 3);
        // Unknown shape (no minimum/median/maximum/runs keys) degrades to
        // zeros with empty runs rather than failing the import.
        let unknown = b
            .metrics
            .iter()
            .find(|m| m.name == "frameDurationCpuMs")
            .unwrap();
        assert_eq!(unknown.runs.len(), 0);
    }

    fn tempfile() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "perfetto-cli-test-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }
}
