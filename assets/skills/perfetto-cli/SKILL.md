---
name: perfetto-cli
description: Perfetto traces of Android apps via the `perfetto-cli` command. Use to capture a trace (cold start, jank, a single run), analyze it with PerfettoSQL (main-thread slices, janky frames), organize captures into sessions, upload/share traces, or open one in ui.perfetto.dev with startup commands.
---

<!-- Installed by perfetto-cli v{{version}}. Re-run `perfetto-cli skills install` after upgrading. -->

`perfetto-cli` records Android Perfetto traces over adb and files each one into a **session**: one app package, one device, one saved trace config, and the traces captured with it. `perfetto-cli <command> --help` is the source of truth for flags.

## Output contract

Pass `--json` on every call. stdout then holds exactly one JSON document; progress and capture logs go to stderr. Non-zero exit means failure, with the reason on stderr. Address a session by id, name (case-insensitive), or folder slug.

## Capture loop

1. **Device.** `perfetto-cli devices --json`. Done when exactly one device is `online`, or you have chosen a serial to pass as `--device`.
2. **Session.** Reuse one per scenario under test (e.g. "Campfire cold start"), not one per run:
   `perfetto-cli session create --name "<scenario>" --package <pkg> --if-not-exists --json`
   Add `--cold` for startup work, `--duration <secs>`, `--launch-activity <.Activity>` when the app has several launcher activities, and `--commands <set>` (repeatable) to attach saved UI startup commands (`perfetto-cli command-sets --json` lists them). Done when the JSON shows the intended `package`, `cold_start`, and `duration_ms`.
3. **Capture.** `perfetto-cli capture "<scenario>" --name <run-label> --tag <tag> --json`
   It blocks for the trace duration plus ~10–20s of setup and pull, so give the shell call a timeout of at least `duration + 60s`. `--cold`/`--warm`/`--duration` here apply to this run only. Done when the JSON has `"cancelled": false` and a `trace.path`; report `trace.id` and `trace.path` back.
4. **Share** (only when asked). Append `--upload` to the capture, or run `perfetto-cli upload "<scenario>" --trace <id> --json` afterwards. Done when every uploaded trace has a `url`.

Label runs so they compare cleanly: `--name before-fix` / `--name after-fix`, and `--tag` for the variable under test.

## Analysis loop

After a capture, read the trace yourself with `query`. It runs PerfettoSQL through a pinned `trace_processor_shell` that perfetto-cli downloads, verifies, and runs itself (the first call takes a few extra seconds); never download or run a trace processor yourself.

`perfetto-cli query "<scenario>" [--trace <id|name>] --sql "<PerfettoSQL>" --json`

- `--trace` takes the `trace.id` from `capture` (or the trace's name / file stem); without it, the session's newest trace is used.
- `--file <path>` reads SQL from a file, or from stdin with `--file -`. Statements may be `;`-separated (e.g. `INCLUDE PERFETTO MODULE ...;` first); only the last statement's rows come back.
- `{{package}}` in the SQL expands to the session's package name.
- Output: `{"session_id", "session_name", "package", "trace": {...}, "trace_processor": "<version>", "columns": [...], "rows": [[...], ...], "row_count": N}`. Each row is an array aligned with `columns`; ints and floats are numbers, NULL is `null`, blobs are base64 strings. Timestamps and durations are nanoseconds. A SQL error exits non-zero with the trace processor's message on stderr.
- `--summary` (instead of `--sql`) returns `metrics`, keyed by name (`jank_frame_count`, `total_frame_count`, `frame_dur_percentiles`, `main_thread_running_ns`, `main_thread_total_ns`, `main_thread_top_slices`, `startup`, `rss_over_time`, `trace_duration_ns`, ...). Each metric has `"status": "ok"` plus `value` or `columns`/`rows`, `"status": "missing"` when the trace lacks that data, or `"status": "error"` with `error`. Summary metrics match the app by process name only; if they come back zero or NULL, use the queries below.

Find the app's processes with `t.name = substr('{{package}}', -15)` as well as `p.name = '{{package}}'`, as both queries below do. The process name is missing from some traces (when the ring buffer wraps, it overwrites the process list recorded at the start), and Linux truncates the main thread's name to the last 15 characters of the package.

Longest main-thread slices, grouped by name:

```sql
SELECT s.name, COUNT(*) AS n, SUM(s.dur) / 1e6 AS total_ms, MAX(s.dur) / 1e6 AS max_ms
FROM slice s
JOIN thread_track tt ON s.track_id = tt.id
JOIN thread t USING (utid)
LEFT JOIN process p USING (upid)
WHERE t.is_main_thread = 1
  AND (p.name = '{{package}}' OR t.name = substr('{{package}}', -15))
GROUP BY s.name ORDER BY total_ms DESC LIMIT 20
```

Janky frames. A frame is janky when `on_time_finish = 0` (it missed its deadline). Don't count `jank_type != 'None'`, which flags nearly every frame:

```sql
SELECT COUNT(*) AS frames, SUM(aft.on_time_finish = 0) AS janky, MAX(aft.dur) / 1e6 AS worst_ms
FROM actual_frame_timeline_slice aft
JOIN thread t ON t.upid = aft.upid AND t.is_main_thread = 1
LEFT JOIN process p ON p.upid = aft.upid
WHERE p.name = '{{package}}' OR t.name = substr('{{package}}', -15)
```

To list the janky frames themselves, select `aft.ts, aft.dur, aft.jank_type`, add `AND aft.on_time_finish = 0`, and order by `aft.dur DESC`. Done when you can name the slices or frames behind the regression, with numbers from the trace.

## Gotchas

- **Uploads may need a human.** Credentials live in the TUI's cloud settings. If upload fails on authentication, stop and ask the user to set up the provider by running `perfetto-cli` with no arguments.
- **`open` needs a human at a browser.** `perfetto-cli open <trace-id> --json` launches ui.perfetto.dev, applies the session's startup commands, and waits (default 60s) for the browser to fetch the trace. Use it only when the user wants to look at the trace now.
- **Startup command sets merge in `command-sets` list order**, whatever order you pass `--commands` in. Session JSON reports the matched sets under `command_sets`.
- **Session settings persist.** `session update <session> ...` changes package, device, commands, or capture settings for every later capture; prefer per-capture overrides for one-off experiments.
- **In-process traces.** `--in-process` (on `session create`/`update` or `capture`) also collects the app's `androidx.tracing:tracing-wire` traces and saves a `.tar` bundle that Perfetto opens as one merged timeline. The app must depend on `tracing-wire`; if it doesn't, the capture logs a warning and saves a plain `.pftrace`. Session JSON reports it as `in_process_tracing`.
- **Imported Macrobenchmark sessions are read-only**; capture into a regular session instead.
- **Interactive commands.** Running `perfetto-cli` with no subcommand launches a full-screen TUI, and `perfetto-cli clear` deletes every session; run either only when the user asks.
