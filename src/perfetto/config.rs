use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::commands::StartupCommand;

/// Every known atrace category: (tag, human-readable description).
/// Sourced from `adb shell atrace --list_categories` on a recent device.
pub const ATRACE_CATEGORIES: &[(&str, &str)] = &[
    ("gfx", "Graphics"),
    ("input", "Input"),
    ("view", "View System"),
    ("webview", "WebView"),
    ("wm", "Window Manager"),
    ("am", "Activity Manager"),
    ("sm", "Sync Manager"),
    ("audio", "Audio"),
    ("video", "Video"),
    ("camera", "Camera"),
    ("hal", "Hardware Modules"),
    ("res", "Resource Loading"),
    ("dalvik", "Dalvik VM"),
    ("rs", "RenderScript"),
    ("bionic", "Bionic C Library"),
    ("power", "Power Management"),
    ("pm", "Package Manager"),
    ("ss", "System Server"),
    ("database", "Database"),
    ("network", "Network"),
    ("adb", "ADB"),
    ("vibrator", "Vibrator"),
    ("aidl", "AIDL calls"),
    ("nnapi", "NNAPI"),
    ("rro", "Runtime Resource Overlay"),
    ("sched", "CPU Scheduling"),
    ("irq", "IRQ Events"),
    ("freq", "CPU Frequency"),
    ("idle", "CPU Idle"),
    ("disk", "Disk I/O"),
    ("sync", "Synchronization"),
    ("memreclaim", "Kernel Memory Reclaim"),
    ("binder_driver", "Binder Kernel driver"),
    ("binder_lock", "Binder global lock trace"),
    ("memory", "Memory"),
    ("thermal", "Thermal event"),
    ("workq", "Kernel Workqueues"),
];

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

// ---------------------------------------------------------------------------
// Serde default helpers
// ---------------------------------------------------------------------------

fn yes() -> bool {
    true
}

fn default_fill_policy() -> FillPolicy {
    FillPolicy::RingBuffer
}

fn default_duration_ms() -> u32 {
    10_000
}

fn default_buffer_size_kb() -> u32 {
    65_536
}

fn default_poll_ms() -> u32 {
    1000
}

// ---------------------------------------------------------------------------
// Top-level trace config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceConfig {
    // --- recording settings ---
    #[serde(default = "default_duration_ms")]
    pub duration_ms: u32,
    #[serde(default = "default_buffer_size_kb")]
    pub buffer_size_kb: u32,
    #[serde(default = "default_fill_policy")]
    pub fill_policy: FillPolicy,

    // --- behavioral toggles ---
    #[serde(default)]
    pub cold_start: bool,
    #[serde(default = "yes")]
    pub auto_open: bool,
    #[serde(default = "yes")]
    pub compose_tracing: bool,
    #[serde(default)]
    pub launch_activity: Option<String>,

    // --- atrace categories (THE source of truth) ---
    #[serde(default = "default_atrace_categories")]
    pub atrace_categories: BTreeSet<String>,

    // --- additional atrace_apps package names ---
    #[serde(default)]
    pub atrace_apps: Vec<String>,

    // --- startup commands (UI automation, not part of the textproto) ---
    #[serde(default)]
    pub startup_commands: Vec<StartupCommand>,

    // --- raw textproto override (for imported configs) ---
    // When set, `textproto::build` returns this verbatim instead of
    // generating from the structured fields. Used for imported configs
    // that can't be round-tripped through the structured model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_textproto: Option<String>,

    // --- probe groups ---
    #[serde(default)]
    pub cpu: CpuProbe,
    #[serde(default)]
    pub gpu: GpuProbe,
    #[serde(default)]
    pub power: PowerProbe,
    #[serde(default)]
    pub memory: MemoryProbe,
    #[serde(default)]
    pub android: AndroidProbe,
    #[serde(default)]
    pub advanced: AdvancedProbe,

    // --- legacy fields (read for migration, never re-serialized) ---
    #[serde(default, skip_serializing)]
    pub categories: Vec<String>,
    #[serde(default, skip_serializing)]
    pub ftrace_events: Vec<String>,
    #[serde(default, skip_serializing)]
    pub android_apps: LegacyAndroidAppsProbe,
}

/// The 23 atrace categories marked `isDefault` in Appendix A.
pub fn default_atrace_categories() -> BTreeSet<String> {
    [
        "aidl",
        "am",
        "binder_driver",
        "camera",
        "dalvik",
        "disk",
        "freq",
        "gfx",
        "hal",
        "idle",
        "input",
        "memory",
        "memreclaim",
        "network",
        "power",
        "res",
        "sched",
        "ss",
        "sync",
        "thermal",
        "view",
        "webview",
        "wm",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            duration_ms: 10_000,
            buffer_size_kb: 65_536,
            fill_policy: FillPolicy::RingBuffer,
            cold_start: false,
            auto_open: true,
            compose_tracing: true,
            launch_activity: None,
            atrace_categories: default_atrace_categories(),
            atrace_apps: Vec::new(),
            startup_commands: Vec::new(),
            custom_textproto: None,
            cpu: CpuProbe::default(),
            gpu: GpuProbe::default(),
            power: PowerProbe::default(),
            memory: MemoryProbe::default(),
            android: AndroidProbe::default(),
            advanced: AdvancedProbe::default(),
            categories: Vec::new(),
            ftrace_events: Vec::new(),
            android_apps: LegacyAndroidAppsProbe::default(),
        }
    }
}

impl TraceConfig {
    /// Migrate legacy configs into the current model. Called after
    /// deserialization. Safe to call multiple times.
    ///
    /// Old probe fields that no longer exist (e.g. `enabled` bools on probes,
    /// removed probe structs like `rendering`, `logging`, `process_stats`) are
    /// silently ignored by serde's default-for-missing-fields behavior.
    pub fn migrate_legacy(&mut self) {
        // --- migrate flat category/ftrace fields ---
        for cat in self.categories.drain(..) {
            self.atrace_categories.insert(cat);
        }
        for ev in self.ftrace_events.drain(..) {
            if !self.advanced.extra_ftrace_events.contains(&ev) {
                self.advanced.extra_ftrace_events.push(ev);
            }
        }

        // --- migrate legacy android_apps probe booleans ---
        let legacy = std::mem::take(&mut self.android_apps);
        if legacy.activity_manager {
            self.atrace_categories.insert("am".into());
        }
        if legacy.window_manager {
            self.atrace_categories.insert("wm".into());
        }
        if legacy.dalvik {
            self.atrace_categories.insert("dalvik".into());
        }
        if legacy.binder {
            self.atrace_categories.insert("binder_driver".into());
            self.atrace_categories.insert("binder_lock".into());
        }
        for app in legacy.atrace_apps {
            if !self.atrace_apps.contains(&app) {
                self.atrace_apps.push(app);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Probe groups
// ---------------------------------------------------------------------------

/// CPU-related probes: coarse usage polling, scheduling details, frequency/idle
/// tracking, and syscall tracing. Each sub-option is independently toggled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuProbe {
    #[serde(default)]
    pub coarse_usage: bool,
    #[serde(default = "default_poll_ms")]
    pub coarse_poll_ms: u32,
    #[serde(default)]
    pub scheduling: bool,
    #[serde(default)]
    pub freq_idle: bool,
    #[serde(default = "default_poll_ms")]
    pub freq_poll_ms: u32,
    #[serde(default)]
    pub syscalls: bool,
}

impl Default for CpuProbe {
    fn default() -> Self {
        Self {
            coarse_usage: false,
            coarse_poll_ms: 1000,
            scheduling: false,
            freq_idle: false,
            freq_poll_ms: 1000,
            syscalls: false,
        }
    }
}

/// GPU-related probes: frequency events, memory tracking, and work period.
/// Each sub-option is independently toggled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuProbe {
    #[serde(default)]
    pub frequency: bool,
    #[serde(default)]
    pub memory: bool,
    #[serde(default)]
    pub work_period: bool,
}

impl Default for GpuProbe {
    fn default() -> Self {
        Self {
            frequency: false,
            memory: false,
            work_period: false,
        }
    }
}

/// Power-related probes: battery drain polling and board voltage/frequency
/// ftrace events. Each sub-option is independently toggled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerProbe {
    #[serde(default)]
    pub battery_drain: bool,
    #[serde(default = "default_poll_ms")]
    pub battery_poll_ms: u32,
    #[serde(default)]
    pub board_voltages: bool,
}

impl Default for PowerProbe {
    fn default() -> Self {
        Self {
            battery_drain: false,
            battery_poll_ms: 1000,
            board_voltages: false,
        }
    }
}

/// Memory-related probes: kernel meminfo polling, high-frequency memory ftrace
/// events, low-memory killer tracking, and per-process stats polling.
/// `per_process_stats` defaults to true because many other probes depend on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryProbe {
    #[serde(default)]
    pub kernel_meminfo: bool,
    #[serde(default = "default_poll_ms")]
    pub meminfo_poll_ms: u32,
    #[serde(default)]
    pub high_freq_events: bool,
    #[serde(default)]
    pub low_memory_killer: bool,
    #[serde(default = "yes")]
    pub per_process_stats: bool,
    #[serde(default = "default_poll_ms")]
    pub process_poll_ms: u32,
}

impl Default for MemoryProbe {
    fn default() -> Self {
        Self {
            kernel_meminfo: false,
            meminfo_poll_ms: 1000,
            high_freq_events: false,
            low_memory_killer: false,
            per_process_stats: true,
            process_poll_ms: 1000,
        }
    }
}

/// Android-specific probes: frame timeline, logcat buffers.
/// Each sub-option is independently toggled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AndroidProbe {
    #[serde(default = "yes")]
    pub frame_timeline: bool,
    #[serde(default)]
    pub logcat: bool,
    #[serde(default = "yes")]
    pub log_crash: bool,
    #[serde(default = "yes")]
    pub log_default: bool,
    #[serde(default)]
    pub log_events: bool,
    #[serde(default)]
    pub log_kernel: bool,
    #[serde(default)]
    pub log_system: bool,
}

impl Default for AndroidProbe {
    fn default() -> Self {
        Self {
            frame_timeline: true,
            logcat: false,
            log_crash: true,
            log_default: true,
            log_events: false,
            log_kernel: false,
            log_system: false,
        }
    }
}

/// Advanced ftrace settings: kernel symbol resolution, generic event filtering,
/// and extra ftrace events. Considered active when any sub-option differs from
/// its default value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvancedProbe {
    #[serde(default = "yes")]
    pub symbolize_ksyms: bool,
    #[serde(default = "yes")]
    pub disable_generic_events: bool,
    #[serde(default)]
    pub extra_ftrace_events: Vec<String>,
}

impl Default for AdvancedProbe {
    fn default() -> Self {
        Self {
            symbolize_ksyms: true,
            disable_generic_events: true,
            extra_ftrace_events: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy structs (deserialization only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LegacyAndroidAppsProbe {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub activity_manager: bool,
    #[serde(default)]
    pub window_manager: bool,
    #[serde(default)]
    pub dalvik: bool,
    #[serde(default)]
    pub binder: bool,
    #[serde(default)]
    pub atrace_apps: Vec<String>,
}
