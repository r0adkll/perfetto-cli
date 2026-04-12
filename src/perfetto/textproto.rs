use std::collections::BTreeSet;

use super::config::TraceConfig;

/// Render a `TraceConfig` as a perfetto textproto config string.
///
/// Follows the structure documented in `docs/perfetto-recorder-config-reference.md`.
/// Atrace categories come from `config.atrace_categories`. Probe groups
/// contribute ftrace events, `linux.sys_stats` fields, and standalone data
/// source blocks. Everything merges via `BTreeSet` for dedup.
pub fn build(config: &TraceConfig) -> String {
    // Imported configs store verbatim textproto that can't be round-tripped
    // through the structured model. Return it as-is.
    if let Some(custom) = &config.custom_textproto {
        return custom.clone();
    }

    let mut ftrace_evts: BTreeSet<&str> = BTreeSet::new();
    let mut sys_stats_fields: Vec<String> = Vec::new();
    let mut extra_blocks: Vec<String> = Vec::new();
    let mut needs_proc_assoc = false;

    // -----------------------------------------------------------------------
    // §2 CPU
    // -----------------------------------------------------------------------

    // §2.1 Coarse CPU usage counter
    if config.cpu.coarse_usage {
        sys_stats_fields.push(format!(
            "      stat_period_ms: {}\n      stat_counters: STAT_CPU_TIMES\n      stat_counters: STAT_FORK_COUNT",
            config.cpu.coarse_poll_ms
        ));
        needs_proc_assoc = true;
    }

    // §2.2 Scheduling details
    if config.cpu.scheduling {
        ftrace_evts.insert("sched/sched_switch");
        ftrace_evts.insert("power/suspend_resume");
        ftrace_evts.insert("sched/sched_blocked_reason");
        ftrace_evts.insert("sched/sched_wakeup");
        ftrace_evts.insert("sched/sched_wakeup_new");
        ftrace_evts.insert("sched/sched_waking");
        ftrace_evts.insert("sched/sched_process_exit");
        ftrace_evts.insert("sched/sched_process_free");
        ftrace_evts.insert("task/task_newtask");
        ftrace_evts.insert("task/task_rename");
        needs_proc_assoc = true;
    }

    // §2.3 CPU frequency and idle states
    if config.cpu.freq_idle {
        sys_stats_fields.push(format!(
            "      cpufreq_period_ms: {}",
            config.cpu.freq_poll_ms
        ));
        ftrace_evts.insert("power/cpu_frequency");
        ftrace_evts.insert("power/cpu_idle");
        ftrace_evts.insert("power/suspend_resume");
    }

    // §2.4 Syscalls
    if config.cpu.syscalls {
        ftrace_evts.insert("raw_syscalls/sys_enter");
        ftrace_evts.insert("raw_syscalls/sys_exit");
    }

    // -----------------------------------------------------------------------
    // §3 GPU
    // -----------------------------------------------------------------------

    // §3.1 GPU frequency
    if config.gpu.frequency {
        ftrace_evts.insert("power/gpu_frequency");
    }

    // §3.2 GPU memory
    if config.gpu.memory {
        ftrace_evts.insert("gpu_mem/gpu_mem_total");
        extra_blocks.push(
            "data_sources: {\n  config {\n    name: \"android.gpu.memory\"\n  }\n}\n".into(),
        );
    }

    // §3.3 GPU work period
    if config.gpu.work_period {
        ftrace_evts.insert("power/gpu_work_period");
    }

    // -----------------------------------------------------------------------
    // §4 Power
    // -----------------------------------------------------------------------

    // §4.1 Battery drain & power rails
    if config.power.battery_drain {
        extra_blocks.push(format!(
            "data_sources: {{\n  config {{\n    name: \"android.power\"\n\
             android_power_config {{\n      battery_poll_ms: {}\n\
                   collect_power_rails: true\n\
                   battery_counters: BATTERY_COUNTER_CAPACITY_PERCENT\n\
                   battery_counters: BATTERY_COUNTER_CHARGE\n\
                   battery_counters: BATTERY_COUNTER_CURRENT\n\
                 }}\n  }}\n}}\n",
            config.power.battery_poll_ms
        ));
    }

    // §4.2 Board voltages & frequencies
    if config.power.board_voltages {
        ftrace_evts.insert("regulator/regulator_set_voltage");
        ftrace_evts.insert("regulator/regulator_set_voltage_complete");
        ftrace_evts.insert("power/clock_enable");
        ftrace_evts.insert("power/clock_disable");
        ftrace_evts.insert("power/clock_set_rate");
        ftrace_evts.insert("power/suspend_resume");
    }

    // -----------------------------------------------------------------------
    // §5 Memory
    // -----------------------------------------------------------------------

    // §5.3 Kernel meminfo
    if config.memory.kernel_meminfo {
        sys_stats_fields.push(format!(
            "      meminfo_period_ms: {}",
            config.memory.meminfo_poll_ms
        ));
    }

    // §5.5 High-frequency memory events
    if config.memory.high_freq_events {
        ftrace_evts.insert("mm_event/mm_event_record");
        ftrace_evts.insert("kmem/rss_stat");
        ftrace_evts.insert("ion/ion_stat");
        ftrace_evts.insert("dmabuf_heap/dma_heap_stat");
        ftrace_evts.insert("kmem/ion_heap_grow");
        ftrace_evts.insert("kmem/ion_heap_shrink");
        needs_proc_assoc = true;
    }

    // §5.6 Low memory killer
    if config.memory.low_memory_killer {
        ftrace_evts.insert("lowmemorykiller/lowmemory_kill");
        ftrace_evts.insert("oom/oom_score_adj_update");
        needs_proc_assoc = true;
    }

    // -----------------------------------------------------------------------
    // §6 Android Apps & Svcs
    // -----------------------------------------------------------------------

    // §6.1 Atrace — ftrace/print always added when any category is enabled
    if !config.atrace_categories.is_empty() {
        ftrace_evts.insert("ftrace/print");
    }

    // §6.2 Logcat
    if config.android.logcat {
        let mut log_ids = Vec::new();
        if config.android.log_crash {
            log_ids.push("LID_CRASH");
        }
        if config.android.log_default {
            log_ids.push("LID_DEFAULT");
        }
        if config.android.log_events {
            log_ids.push("LID_EVENTS");
        }
        if config.android.log_kernel {
            log_ids.push("LID_KERNEL");
        }
        if config.android.log_system {
            log_ids.push("LID_SYSTEM");
        }
        if !log_ids.is_empty() {
            let mut block =
                "data_sources: {\n  config {\n    name: \"android.log\"\n    android_log_config {\n"
                    .to_string();
            for id in &log_ids {
                block.push_str(&format!("      log_ids: {id}\n"));
            }
            block.push_str("    }\n  }\n}\n");
            extra_blocks.push(block);
        }
    }

    // §6.3 Frame timeline
    if config.android.frame_timeline {
        extra_blocks.push(
            "data_sources: {\n  config {\n    name: \"android.surfaceflinger.frametimeline\"\n  }\n}\n"
                .into(),
        );
    }

    // -----------------------------------------------------------------------
    // §11 Advanced
    // -----------------------------------------------------------------------

    for ev in &config.advanced.extra_ftrace_events {
        ftrace_evts.insert(ev.as_str());
    }

    // -----------------------------------------------------------------------
    // Atrace apps
    // -----------------------------------------------------------------------
    let mut atrace_apps: BTreeSet<&str> = BTreeSet::new();
    for app in &config.atrace_apps {
        atrace_apps.insert(app.as_str());
    }

    // -----------------------------------------------------------------------
    // Assemble
    // -----------------------------------------------------------------------
    let mut out = String::new();

    // Buffer
    out.push_str("buffers: {\n");
    out.push_str(&format!("  size_kb: {}\n", config.buffer_size_kb));
    out.push_str(&format!(
        "  fill_policy: {}\n",
        config.fill_policy.textproto()
    ));
    out.push_str("}\n");

    // linux.sys_stats (merged from CPU coarse, CPU freq, meminfo)
    if !sys_stats_fields.is_empty() {
        out.push_str("data_sources: {\n  config {\n    name: \"linux.sys_stats\"\n    sys_stats_config {\n");
        for field in &sys_stats_fields {
            out.push_str(field);
            out.push('\n');
        }
        out.push_str("    }\n  }\n}\n");
    }

    // linux.ftrace (shared)
    if !config.atrace_categories.is_empty()
        || !ftrace_evts.is_empty()
        || !atrace_apps.is_empty()
    {
        out.push_str("data_sources: {\n  config {\n    name: \"linux.ftrace\"\n    ftrace_config {\n");
        for ev in &ftrace_evts {
            out.push_str(&format!("      ftrace_events: {}\n", quote(ev)));
        }
        for cat in &config.atrace_categories {
            out.push_str(&format!("      atrace_categories: {}\n", quote(cat)));
        }
        for app in &atrace_apps {
            out.push_str(&format!("      atrace_apps: {}\n", quote(app)));
        }
        if config.advanced.symbolize_ksyms {
            out.push_str("      symbolize_ksyms: true\n");
        }
        if config.advanced.disable_generic_events {
            out.push_str("      disable_generic_events: true\n");
        }
        out.push_str("    }\n  }\n}\n");
    }

    // §5.7 / §11.2 linux.process_stats (auto-dependency or explicit)
    if config.memory.per_process_stats || needs_proc_assoc {
        out.push_str("data_sources: {\n  config {\n    name: \"linux.process_stats\"\n    process_stats_config {\n");
        out.push_str("      scan_all_processes_on_start: true\n");
        if config.memory.per_process_stats && config.memory.process_poll_ms > 0 {
            out.push_str(&format!(
                "      proc_stats_poll_ms: {}\n",
                config.memory.process_poll_ms
            ));
        }
        out.push_str("    }\n  }\n}\n");
    }

    // Additional data-source blocks from probes
    for block in &extra_blocks {
        out.push_str(block);
    }

    // §8 Compose / Perfetto SDK track_event
    if config.compose_tracing {
        out.push_str(
            "data_sources: {\n  config {\n    name: \"track_event\"\n  }\n}\n",
        );
    }

    // Duration
    out.push_str(&format!("duration_ms: {}\n", config.duration_ms));

    out
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_atrace_and_process_stats() {
        let cfg = TraceConfig::default();
        let txt = build(&cfg);
        // Default atrace categories
        assert!(txt.contains("atrace_categories: \"sched\""));
        assert!(txt.contains("atrace_categories: \"gfx\""));
        assert!(txt.contains("atrace_categories: \"am\""));
        // ftrace/print auto-added when atrace categories present
        assert!(txt.contains("ftrace_events: \"ftrace/print\""));
        // per_process_stats default on
        assert!(txt.contains("linux.process_stats"));
        // frame_timeline default on
        assert!(txt.contains("android.surfaceflinger.frametimeline"));
        // compose tracing default on
        assert!(txt.contains("name: \"track_event\""));
        assert!(txt.contains("duration_ms: 10000"));
        assert!(txt.contains("size_kb: 65536"));
    }

    #[test]
    fn cpu_scheduling_emits_ftrace_events() {
        let mut cfg = TraceConfig::default();
        cfg.cpu.scheduling = true;
        let txt = build(&cfg);
        assert!(txt.contains("sched/sched_switch"));
        assert!(txt.contains("sched/sched_wakeup"));
        assert!(txt.contains("task/task_newtask"));
    }

    #[test]
    fn cpu_coarse_usage_emits_sys_stats() {
        let mut cfg = TraceConfig::default();
        cfg.cpu.coarse_usage = true;
        cfg.cpu.coarse_poll_ms = 500;
        let txt = build(&cfg);
        assert!(txt.contains("linux.sys_stats"));
        assert!(txt.contains("stat_period_ms: 500"));
        assert!(txt.contains("STAT_CPU_TIMES"));
    }

    #[test]
    fn cpu_freq_idle_emits_ftrace_and_sys_stats() {
        let mut cfg = TraceConfig::default();
        cfg.cpu.freq_idle = true;
        let txt = build(&cfg);
        assert!(txt.contains("power/cpu_frequency"));
        assert!(txt.contains("power/cpu_idle"));
        assert!(txt.contains("cpufreq_period_ms: 1000"));
    }

    #[test]
    fn gpu_probes_emit_correctly() {
        let mut cfg = TraceConfig::default();
        cfg.gpu.frequency = true;
        cfg.gpu.memory = true;
        cfg.gpu.work_period = true;
        let txt = build(&cfg);
        assert!(txt.contains("power/gpu_frequency"));
        assert!(txt.contains("android.gpu.memory"));
        assert!(txt.contains("gpu_mem/gpu_mem_total"));
        assert!(txt.contains("power/gpu_work_period"));
    }

    #[test]
    fn power_battery_drain_emits_data_source() {
        let mut cfg = TraceConfig::default();
        cfg.power.battery_drain = true;
        cfg.power.battery_poll_ms = 2000;
        let txt = build(&cfg);
        assert!(txt.contains("android.power"));
        assert!(txt.contains("battery_poll_ms: 2000"));
        assert!(txt.contains("collect_power_rails: true"));
    }

    #[test]
    fn power_board_voltages_emits_ftrace() {
        let mut cfg = TraceConfig::default();
        cfg.power.board_voltages = true;
        let txt = build(&cfg);
        assert!(txt.contains("regulator/regulator_set_voltage"));
        assert!(txt.contains("power/clock_enable"));
    }

    #[test]
    fn memory_kernel_meminfo_emits_sys_stats() {
        let mut cfg = TraceConfig::default();
        cfg.memory.kernel_meminfo = true;
        cfg.memory.meminfo_poll_ms = 250;
        let txt = build(&cfg);
        assert!(txt.contains("meminfo_period_ms: 250"));
    }

    #[test]
    fn memory_high_freq_emits_ftrace() {
        let mut cfg = TraceConfig::default();
        cfg.memory.high_freq_events = true;
        let txt = build(&cfg);
        assert!(txt.contains("mm_event/mm_event_record"));
        assert!(txt.contains("kmem/rss_stat"));
    }

    #[test]
    fn memory_lmk_emits_ftrace_and_proc_assoc() {
        let mut cfg = TraceConfig::default();
        cfg.memory.per_process_stats = false; // turn off default
        cfg.memory.low_memory_killer = true;
        let txt = build(&cfg);
        assert!(txt.contains("lowmemorykiller/lowmemory_kill"));
        assert!(txt.contains("oom/oom_score_adj_update"));
        // proc_assoc auto-dependency
        assert!(txt.contains("linux.process_stats"));
    }

    #[test]
    fn android_logcat_emits_log_data_source() {
        let mut cfg = TraceConfig::default();
        cfg.android.logcat = true;
        let txt = build(&cfg);
        assert!(txt.contains("android.log"));
        assert!(txt.contains("LID_CRASH"));
        assert!(txt.contains("LID_DEFAULT"));
    }

    #[test]
    fn compose_tracing_gate() {
        let mut cfg = TraceConfig::default();
        cfg.compose_tracing = true;
        assert!(build(&cfg).contains("track_event"));
        cfg.compose_tracing = false;
        assert!(!build(&cfg).contains("track_event"));
    }

    #[test]
    fn empty_categories_no_ftrace_block() {
        let mut cfg = TraceConfig::default();
        cfg.atrace_categories.clear();
        cfg.memory.per_process_stats = false;
        cfg.android.frame_timeline = false;
        cfg.compose_tracing = false;
        let txt = build(&cfg);
        assert!(!txt.contains("linux.ftrace"));
    }

    #[test]
    fn sys_stats_merges_multiple_probes() {
        let mut cfg = TraceConfig::default();
        cfg.cpu.coarse_usage = true;
        cfg.cpu.freq_idle = true;
        cfg.memory.kernel_meminfo = true;
        let txt = build(&cfg);
        // Should have ONE linux.sys_stats block with all three fields
        let count = txt.matches("linux.sys_stats").count();
        assert_eq!(count, 1);
        assert!(txt.contains("stat_period_ms"));
        assert!(txt.contains("cpufreq_period_ms"));
        assert!(txt.contains("meminfo_period_ms"));
    }

    #[test]
    fn escapes_quotes_in_values() {
        let mut cfg = TraceConfig::default();
        cfg.atrace_categories.insert("a\"b\\c".into());
        let txt = build(&cfg);
        assert!(txt.contains(r#"atrace_categories: "a\"b\\c""#));
    }
}
