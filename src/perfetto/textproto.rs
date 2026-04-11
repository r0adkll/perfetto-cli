use super::config::TraceConfig;

/// Render a `TraceConfig` as a perfetto textproto config string, suitable for
/// piping into `perfetto --txt -c -` on device.
pub fn build(config: &TraceConfig) -> String {
    let mut out = String::new();

    out.push_str("buffers: {\n");
    out.push_str(&format!("    size_kb: {}\n", config.buffer_size_kb));
    out.push_str(&format!(
        "    fill_policy: {}\n",
        config.fill_policy.textproto()
    ));
    out.push_str("}\n");

    out.push_str("data_sources: {\n");
    out.push_str("    config {\n");
    out.push_str("        name: \"linux.ftrace\"\n");
    out.push_str("        ftrace_config {\n");
    for event in &config.ftrace_events {
        out.push_str(&format!("            ftrace_events: {}\n", quote(event)));
    }
    for cat in &config.categories {
        out.push_str(&format!("            atrace_categories: {}\n", quote(cat)));
    }
    for app in &config.atrace_apps {
        out.push_str(&format!("            atrace_apps: {}\n", quote(app)));
    }
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");

    out.push_str("data_sources: {\n");
    out.push_str("    config {\n");
    out.push_str("        name: \"linux.process_stats\"\n");
    out.push_str("        process_stats_config {\n");
    out.push_str("            scan_all_processes_on_start: true\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");

    if config.compose_tracing {
        // androidx.tracing.perfetto emits via the TrackEvent data source —
        // without this block the ENABLE_TRACING broadcast is accepted but
        // the events never make it into the ring buffer.
        out.push_str("data_sources: {\n");
        out.push_str("    config {\n");
        out.push_str("        name: \"track_event\"\n");
        out.push_str("    }\n");
        out.push_str("}\n");
    }

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
    use crate::perfetto::config::FillPolicy;

    #[test]
    fn renders_default_config() {
        let cfg = TraceConfig::default();
        let txt = build(&cfg);
        assert!(txt.contains("size_kb: 32768"));
        assert!(txt.contains("fill_policy: RING_BUFFER"));
        assert!(txt.contains("atrace_categories: \"sched\""));
        assert!(txt.contains("duration_ms: 10000"));
        assert!(txt.contains("linux.process_stats"));
    }

    #[test]
    fn renders_ftrace_and_apps() {
        let cfg = TraceConfig {
            duration_ms: 5_000,
            buffer_size_kb: 16 * 1024,
            fill_policy: FillPolicy::Discard,
            categories: vec!["sched".into()],
            ftrace_events: vec!["sched/sched_switch".into()],
            atrace_apps: vec!["com.example.app".into()],
            cold_start: false,
            auto_open: true,
            launch_activity: None,
            compose_tracing: true,
        };
        let txt = build(&cfg);
        assert!(txt.contains("fill_policy: DISCARD"));
        assert!(txt.contains("ftrace_events: \"sched/sched_switch\""));
        assert!(txt.contains("atrace_apps: \"com.example.app\""));
        assert!(txt.contains("duration_ms: 5000"));
    }

    #[test]
    fn track_event_data_source_gated_on_compose_tracing() {
        let mut cfg = TraceConfig::default();
        cfg.compose_tracing = true;
        let on = build(&cfg);
        assert!(on.contains("name: \"track_event\""));

        cfg.compose_tracing = false;
        let off = build(&cfg);
        assert!(!off.contains("name: \"track_event\""));
    }

    #[test]
    fn escapes_quotes_in_values() {
        let cfg = TraceConfig {
            categories: vec!["a\"b\\c".into()],
            ..TraceConfig::default()
        };
        let txt = build(&cfg);
        assert!(txt.contains(r#"atrace_categories: "a\"b\\c""#));
    }
}
