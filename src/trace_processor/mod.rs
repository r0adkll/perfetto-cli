//! Local SQL analysis of `.pftrace` files via Perfetto's `trace_processor_shell`.
//!
//! Spawns `trace_processor_shell -D --http-port <port>` as a subprocess and
//! communicates over its protobuf-over-HTTP interface. See
//! <https://perfetto.dev/docs/analysis/trace-processor> for the upstream API
//! that this module wraps.
//!
//! Typical use:
//!
//! ```no_run
//! # async fn run(paths: &crate::config::Paths, trace_path: &std::path::Path) -> anyhow::Result<()> {
//! use std::sync::Arc;
//! use crate::trace_processor::{TraceProcessor};
//! use crate::perfetto::capture::Cancel;
//!
//! let cancel = Cancel::new();
//! let tp = TraceProcessor::load(paths, trace_path, cancel, None).await?;
//! let result = tp.query("SELECT COUNT(*) AS n FROM slice").await?;
//! let n = result.rows[0].get("n")?.as_int()?;
//! println!("slice count: {n}");
//! tp.shutdown().await?;
//! # Ok(())
//! # }
//! ```

// The module is a foundation layer — the TUI integration that consumes it
// arrives in a follow-up PR. Until then, the API surface is intentionally
// kept compile-reachable without live call sites.
#![allow(dead_code, unused_imports)]

pub mod binary;
mod client;
mod http;
mod proto;
mod query;

pub use binary::{DownloadProgress, PINNED_VERSION, detect_version, ensure_binary};
pub use client::{LoadProgress, TraceProcessor};
pub use query::{Cell, QueryResult, Row};
