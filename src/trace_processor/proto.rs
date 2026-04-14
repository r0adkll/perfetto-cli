//! Prost-generated types from `proto/trace_processor.proto`.
//!
//! The build script (`build.rs`) invokes `prost-build` and writes the module to
//! `$OUT_DIR/perfetto.protos.rs`. Only the legacy HTTP endpoints' messages are
//! included — see the vendored proto file's header comment for the subset.

include!(concat!(env!("OUT_DIR"), "/perfetto.protos.rs"));
