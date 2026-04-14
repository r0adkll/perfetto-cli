//! Curated library of out-of-box PerfettoSQL queries.
//!
//! Entries are compile-time constants and ship with the binary. The REPL
//! exposes them via `Alt+I` ("insert from library"); users pick one, the
//! SQL drops into the editor with `{{package}}` substituted for the
//! current session's package name, and they can review / tweak before
//! saving with `Alt+S`.
//!
//! Adding / removing entries is a code change — no runtime config.
//! Queries target Perfetto v54's stdlib (our pinned `trace_processor_shell`
//! version). Entries that depend on optional capture sources (Compose
//! track_event, frame timeline, startup events) degrade to empty
//! results on traces that don't have them; the REPL surfaces the raw
//! error inline as it does for any user-authored query.

/// One entry in the query library.
pub struct LibraryEntry {
    /// Default name used to pre-fill the SaveAs prompt. Stable across
    /// releases — users may have saved queries under this name and we
    /// don't want to break their dashboards on upgrade.
    pub name: &'static str,
    /// One-sentence description surfaced in the picker. Keep under
    /// ~90 chars so it fits on a single line in typical terminals.
    pub description: &'static str,
    /// SQL template. May contain `{{package}}` which the REPL replaces
    /// with the session's `package_name` at load time.
    pub sql: &'static str,
}

/// Substitute the `{{package}}` placeholder for an actual package name.
/// Queries that don't use the placeholder pass through verbatim.
pub fn render_sql(entry: &LibraryEntry, package_name: &str) -> String {
    entry.sql.replace("{{package}}", package_name)
}

/// The curated set. Keep this list tight — scrolling is fine for ~15
/// entries; beyond that the picker will want a filter.
pub const LIBRARY: &[LibraryEntry] = &[
    LibraryEntry {
        name: "startup-monitor-contention",
        description:
            "Startups blocked on Java monitor (lock) contention — which app launches waited longest for locks",
        sql: "INCLUDE PERFETTO MODULE android.startup.startups;\n\
              INCLUDE PERFETTO MODULE android.monitor_contention;\n\
              \n\
              SELECT\n\
                launches.startup_id,\n\
                launches.package,\n\
                launches.startup_type,\n\
                launches.dur / 1e6 AS total_launch_ms,\n\
                SUM(monitor_contention.dur) / 1e6 AS total_monitor_ms\n\
              FROM android_startups AS launches\n\
              JOIN android_monitor_contention AS monitor_contention\n\
                ON monitor_contention.ts BETWEEN launches.ts AND launches.ts + launches.dur\n\
              GROUP BY launches.startup_id\n\
              ORDER BY total_monitor_ms DESC\n\
              LIMIT 10",
    },
    LibraryEntry {
        name: "frame-jank-reasons",
        description:
            "Jank-type breakdown for {{package}}'s frames that missed their deadline (on_time_finish = 0)",
        sql: "SELECT jank_type, COUNT(*) AS frames\n\
              FROM actual_frame_timeline_slice aft\n\
              JOIN process p ON aft.upid = p.upid\n\
              WHERE p.name = '{{package}}' AND aft.on_time_finish = 0\n\
              GROUP BY jank_type\n\
              ORDER BY frames DESC",
    },
    LibraryEntry {
        name: "slow-binder-transactions",
        description:
            "Top 10 slowest binder IPC transactions across the trace with source thread/process",
        sql: "SELECT\n\
                slice.name,\n\
                slice.dur / 1e6 AS dur_ms,\n\
                thread.name AS thread_name,\n\
                process.name AS process_name\n\
              FROM slice\n\
              JOIN thread_track ON slice.track_id = thread_track.id\n\
              JOIN thread USING(utid)\n\
              JOIN process USING(upid)\n\
              WHERE slice.name LIKE 'binder transaction%'\n\
              ORDER BY slice.dur DESC\n\
              LIMIT 10",
    },
    LibraryEntry {
        name: "cpu-by-process",
        description: "Total on-CPU time per process — quick way to see who dominated the CPU",
        sql: "SELECT\n\
                process.name AS process_name,\n\
                SUM(thread_state.dur) / 1e6 AS cpu_ms\n\
              FROM thread_state\n\
              JOIN thread USING(utid)\n\
              JOIN process USING(upid)\n\
              WHERE thread_state.state = 'Running'\n\
              GROUP BY process.name\n\
              ORDER BY cpu_ms DESC\n\
              LIMIT 15",
    },
    LibraryEntry {
        name: "longest-slices-any-thread",
        description: "Top 10 single slices by duration across every thread — catches system-wide anomalies",
        sql: "SELECT\n\
                slice.name,\n\
                slice.dur / 1e6 AS dur_ms,\n\
                thread.name AS thread_name,\n\
                process.name AS process_name\n\
              FROM slice\n\
              JOIN thread_track ON slice.track_id = thread_track.id\n\
              JOIN thread USING(utid)\n\
              JOIN process USING(upid)\n\
              WHERE slice.name IS NOT NULL AND slice.dur > 0\n\
              ORDER BY slice.dur DESC\n\
              LIMIT 10",
    },
    LibraryEntry {
        name: "gc-events-on-main",
        description: "ART garbage-collection slices on {{package}}'s main thread, ranked by duration",
        sql: "SELECT\n\
                slice.name,\n\
                slice.dur / 1e6 AS dur_ms,\n\
                slice.ts\n\
              FROM slice\n\
              JOIN thread_track ON slice.track_id = thread_track.id\n\
              JOIN thread USING(utid)\n\
              JOIN process USING(upid)\n\
              WHERE process.name = '{{package}}'\n\
                AND thread.is_main_thread = 1\n\
                AND (slice.name LIKE '%GC%' OR slice.name LIKE '%gc%' OR slice.name LIKE '%collector%')\n\
              ORDER BY slice.dur DESC\n\
              LIMIT 10",
    },
    LibraryEntry {
        name: "recompositions-by-function",
        description:
            "Compose recomposition counts per function (requires track_event capture)",
        sql: "SELECT slice.name, COUNT(*) AS recompositions\n\
              FROM slice\n\
              WHERE slice.name LIKE '%recompose%'\n\
                 OR slice.name LIKE '%Recompose%'\n\
                 OR slice.category = 'androidx.compose.ui.node'\n\
              GROUP BY slice.name\n\
              ORDER BY recompositions DESC\n\
              LIMIT 10",
    },
    LibraryEntry {
        name: "thread-state-breakdown",
        description:
            "{{package}}'s main thread time split by state (Running / Sleeping / Runnable / UninterruptibleSleep / …)",
        sql: "SELECT\n\
                thread_state.state,\n\
                SUM(thread_state.dur) / 1e6 AS ms\n\
              FROM thread_state\n\
              JOIN thread USING(utid)\n\
              JOIN process USING(upid)\n\
              WHERE process.name = '{{package}}' AND thread.is_main_thread = 1\n\
              GROUP BY thread_state.state\n\
              ORDER BY ms DESC",
    },
    LibraryEntry {
        name: "io-blocked-threads",
        description:
            "Threads that spent the most time in uninterruptible sleep (kernel I/O waits, disk pressure)",
        sql: "SELECT\n\
                thread.name AS thread_name,\n\
                process.name AS process_name,\n\
                SUM(thread_state.dur) / 1e6 AS io_ms\n\
              FROM thread_state\n\
              JOIN thread USING(utid)\n\
              JOIN process USING(upid)\n\
              WHERE thread_state.state = 'D'\n\
              GROUP BY thread.utid\n\
              ORDER BY io_ms DESC\n\
              LIMIT 10",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn library_is_non_empty() {
        assert!(!LIBRARY.is_empty(), "library must ship with entries");
    }

    #[test]
    fn library_names_are_unique_and_sensible() {
        let mut seen = HashSet::new();
        for entry in LIBRARY {
            assert!(!entry.name.is_empty(), "entry has empty name");
            assert!(
                !entry.sql.trim().is_empty(),
                "entry {} has empty sql",
                entry.name
            );
            assert!(
                !entry.description.trim().is_empty(),
                "entry {} has empty description",
                entry.name
            );
            assert!(
                seen.insert(entry.name),
                "entry name {} duplicated",
                entry.name
            );
        }
    }

    #[test]
    fn render_sql_substitutes_package_placeholder() {
        let entry = LibraryEntry {
            name: "test",
            description: "t",
            sql: "SELECT * FROM process WHERE name = '{{package}}'",
        };
        let rendered = render_sql(&entry, "com.example.app");
        assert_eq!(
            rendered,
            "SELECT * FROM process WHERE name = 'com.example.app'"
        );
    }

    #[test]
    fn render_sql_passes_through_universal_queries() {
        let entry = LibraryEntry {
            name: "test",
            description: "t",
            sql: "SELECT COUNT(*) FROM slice",
        };
        let rendered = render_sql(&entry, "ignored");
        assert_eq!(rendered, "SELECT COUNT(*) FROM slice");
    }

    #[test]
    fn render_sql_substitutes_every_occurrence() {
        let entry = LibraryEntry {
            name: "test",
            description: "t",
            sql: "SELECT '{{package}}' AS a, '{{package}}' AS b",
        };
        let rendered = render_sql(&entry, "com.x");
        assert_eq!(rendered, "SELECT 'com.x' AS a, 'com.x' AS b");
    }
}
