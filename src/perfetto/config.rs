use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillPolicy {
    #[serde(rename = "ring_buffer")]
    RingBuffer,
    #[serde(rename = "discard")]
    Discard,
}

impl FillPolicy {
    pub fn textproto(&self) -> &'static str {
        match self {
            FillPolicy::RingBuffer => "RING_BUFFER",
            FillPolicy::Discard => "DISCARD",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            FillPolicy::RingBuffer => "ring buffer",
            FillPolicy::Discard => "discard",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            FillPolicy::RingBuffer => FillPolicy::Discard,
            FillPolicy::Discard => FillPolicy::RingBuffer,
        }
    }
}

/// Structured trace configuration. Milestone 4 fleshes this out with enough
/// knobs to drive the `record_android_trace` workflow; milestone 5 turns it
/// into textproto and sends it to the device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceConfig {
    pub duration_ms: u32,
    pub buffer_size_kb: u32,
    #[serde(default = "default_fill_policy")]
    pub fill_policy: FillPolicy,
    /// Short-form atrace categories (e.g. `sched`, `freq`, `gfx`).
    pub categories: Vec<String>,
    /// Fully-qualified ftrace events (e.g. `power/cpu_frequency`).
    pub ftrace_events: Vec<String>,
    /// Package names whose atrace tags should be collected.
    pub atrace_apps: Vec<String>,
    /// When true, the capture flow will `am force-stop` the target package
    /// before starting, and `am start` the launch activity shortly after.
    #[serde(default)]
    pub cold_start: bool,
    /// When true, successful captures automatically open in ui.perfetto.dev
    /// as soon as they finish. Defaults to `true` so the common workflow
    /// (capture → immediately inspect) is one key press.
    #[serde(default = "default_auto_open")]
    pub auto_open: bool,
    /// Explicit activity to launch for cold-start traces. Use this when
    /// `cmd package resolve-activity` picks the wrong activity (e.g.
    /// LeakCanary's `LeakActivity`).
    ///
    /// Accepted forms: `.MainActivity`, `com.example.MainActivity`, or a
    /// full `com.example/.MainActivity` component string. When unset, the
    /// capture engine falls back to `monkey`.
    #[serde(default)]
    pub launch_activity: Option<String>,
    /// When true, send the `androidx.tracing.perfetto.action.ENABLE_TRACING`
    /// broadcast so the target app emits Jetpack Compose recomposition /
    /// composition events into the trace. For cold-start captures the
    /// broadcast is deferred until directly after `am start` so it doesn't
    /// spawn the app process prematurely and ruin the cold-start window.
    #[serde(default = "default_compose_tracing")]
    pub compose_tracing: bool,
}

fn default_auto_open() -> bool {
    true
}

fn default_compose_tracing() -> bool {
    true
}

fn default_fill_policy() -> FillPolicy {
    FillPolicy::RingBuffer
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            duration_ms: 10_000,
            buffer_size_kb: 32 * 1024,
            fill_policy: FillPolicy::RingBuffer,
            categories: vec![
                "sched".into(),
                "freq".into(),
                "idle".into(),
                "am".into(),
                "wm".into(),
                "gfx".into(),
                "view".into(),
            ],
            ftrace_events: Vec::new(),
            atrace_apps: Vec::new(),
            cold_start: false,
            auto_open: true,
            launch_activity: None,
            compose_tracing: true,
        }
    }
}
