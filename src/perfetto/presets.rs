use super::config::{FillPolicy, TraceConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Default,
    AppStartup,
    FrameTiming,
    CpuScheduling,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Default,
        Preset::AppStartup,
        Preset::FrameTiming,
        Preset::CpuScheduling,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Preset::Default => "Default",
            Preset::AppStartup => "App startup (cold)",
            Preset::FrameTiming => "Frame timing / jank",
            Preset::CpuScheduling => "CPU scheduling",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Preset::Default => "General-purpose 10s capture with common categories",
            Preset::AppStartup => "Cold-start capture: force-stop, trace, restart the app",
            Preset::FrameTiming => "Frame-level events tuned for jank analysis",
            Preset::CpuScheduling => "CPU scheduling and frequency detail",
        }
    }

    pub fn config(&self) -> TraceConfig {
        match self {
            Preset::Default => TraceConfig::default(),
            Preset::AppStartup => TraceConfig {
                duration_ms: 8_000,
                buffer_size_kb: 32 * 1024,
                fill_policy: FillPolicy::Discard,
                categories: vec![
                    "am".into(),
                    "wm".into(),
                    "sched".into(),
                    "binder_driver".into(),
                    "view".into(),
                    "gfx".into(),
                    "input".into(),
                    "dalvik".into(),
                ],
                ftrace_events: Vec::new(),
                atrace_apps: Vec::new(),
                cold_start: true,
                auto_open: true,
                launch_activity: None,
                compose_tracing: true,
            },
            Preset::FrameTiming => TraceConfig {
                duration_ms: 10_000,
                buffer_size_kb: 32 * 1024,
                fill_policy: FillPolicy::RingBuffer,
                categories: vec![
                    "gfx".into(),
                    "view".into(),
                    "input".into(),
                    "hal".into(),
                    "sched".into(),
                    "freq".into(),
                    "power".into(),
                ],
                ftrace_events: Vec::new(),
                atrace_apps: Vec::new(),
                cold_start: false,
                auto_open: true,
                launch_activity: None,
                compose_tracing: true,
            },
            Preset::CpuScheduling => TraceConfig {
                duration_ms: 15_000,
                buffer_size_kb: 64 * 1024,
                fill_policy: FillPolicy::RingBuffer,
                categories: vec![
                    "sched".into(),
                    "freq".into(),
                    "idle".into(),
                    "power".into(),
                ],
                ftrace_events: vec![
                    "sched/sched_switch".into(),
                    "sched/sched_wakeup".into(),
                    "power/cpu_frequency".into(),
                    "power/cpu_idle".into(),
                ],
                atrace_apps: Vec::new(),
                cold_start: false,
                auto_open: true,
                launch_activity: None,
                compose_tracing: true,
            },
        }
    }

    pub fn cycle_forward(self) -> Self {
        let idx = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn cycle_back(self) -> Self {
        let idx = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}
