# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-04-14

### Added
- `perfetto-cli clear` subcommand — wipes the local SQLite database and sessions directory after a typed `yes` confirmation (`-y`/`--yes` skips the prompt); themes and logs are preserved
- `perfetto-cli import <dir> [--name <prefix>]` subcommand — imports an Android Macrobenchmark output directory (the `connected_android_test_additional_output/.../<device>/` folder) as one read-only session per `@Test` method, copying the `-benchmarkData.json` and every matching iteration trace into the session folder
- Session detail right pane shows a benchmark metrics summary (per-metric min/median/max + run count) for imported sessions, replacing the textproto preview that doesn't apply to opaque imported configs
- Sessions list shows a dim `[imported]` tag next to imported sessions
- Three new `sessions` columns (`is_imported`, `benchmark_json_path`, `import_source_dir`) with an additive migration so existing databases upgrade in place
- **Local trace analysis via PerfettoSQL** — press `[a]` on any trace in session detail to open the Analysis screen, which spawns a pinned-version `trace_processor_shell` (downloaded + SHA-256 verified on first use, cached under `~/.config/perfetto-cli/bin/`), parses the trace, and runs queries locally
- **Summary tab** on the Analysis screen — diagnostic dashboard with context strip (package · device · captured-at · duration), three health tiles (jank rate, frame times p50/p95, main-thread busy %), conditional startup card, memory-over-time sparkline with inline min/peak callouts, main-thread hotspots, a "Custom metrics" section driven by user-saved queries, and a data-sources ribbon
- **SQL tab — metric authoring surface** — three stacked panes (saved metrics list · result · multi-line editor). All actions via visible `Alt+` chords: `Alt+Enter` run, `Alt+S` save/update (prompts for name when new, updates in place when editing an existing metric), `Alt+L` load highlighted metric into editor, `Alt+N` new, `Alt+R` rename highlighted, `Alt+D` delete highlighted (with `[y]`/`[n]` confirm), `Alt+Up`/`Alt+Down` cycle highlight. Plain Enter inserts a newline; `Ctrl+U` / `Esc` clear. Result scroll on `Shift+Up/Down` / `PageUp/PageDown`. Ctrl+Enter also submits on terminals that forward the modifier
- **Bracketed paste support** in all text inputs — multi-line pastes now land atomically in textareas instead of arriving as a stream of synthetic keystrokes; single-line inputs collapse newlines to spaces
- **Per-app saved metrics** — each app's saved SQL queries live in a new `saved_queries` table keyed on `(package_name, name)` and auto-run on every Summary refresh. Builds a dashboard tailored to each app's instrumentation; metrics persist across every session for the same package
- **Out-of-box query library (`Alt+I`)** — REPL ships with ~9 curated PerfettoSQL queries (startup blocked on monitor contention, jank reasons, slow binder transactions, CPU per process, longest slices, GC events, Compose recompositions, main-thread state breakdown, I/O-blocked threads). Pick one with Enter; `{{package}}` is substituted for the session's package and the SaveAs prompt pre-fills with the entry's default name on `Alt+S`
- **Shape-aware Custom metrics rendering** on the Summary tab — saved queries now render in their natural shape: 1×1 results as bordered tiles (packed 3 per row), multi-row results as inline tables (up to 4 visible rows + `+N more rows` footer), empty results and errors as compact status cards. Replaces the previous one-line teaser that truncated tabular results. The Custom section now claims all remaining Summary area after the fixed regions (context / health / min-hotspots / ribbon / optional startup & memory). Press `[c]` on the Summary tab to toggle compact mode — tables collapse to a one-line "N rows" tile so a dense dashboard shrinks to a scannable overview on demand
- **Named capture** — press `[C]` (Shift+C) on session detail to prompt for a trace filename before capturing; `[c]` still runs a quick capture with the default `YYYY-MM-DD_HH-MM-SS.pftrace` name. Spaces in the prompt collapse to `-`, an empty submit falls back to the timestamp default, and on filename collision the engine appends `-2`, `-3`, … so existing traces are never overwritten
- **SQL keyword highlighting** in the Analysis REPL editor — PerfettoSQL keywords render in the theme's accent-secondary bold so queries are readable at a glance. Re-colors automatically on theme change
- **Scope-aware completion popup** (`Ctrl+Space`, or `Alt+/` as a chord-friendly fallback) on the Analysis REPL — filters PerfettoSQL keywords, aggregates, and tables by prefix; discovered tables from the loaded trace appear after the curated set; column names become available once `FROM <table>` (or `JOIN <table>`) puts a table in scope. Dotted prefix `s.` restricts completions to the alias's columns. Empty prefix shows the full pool in curated order so the popup doubles as a browser. `Tab`/`Enter` accept, `Up`/`Down` navigate, `Esc` dismisses
- **Schema browser panel** on the Analysis REPL (`Alt+B` to focus) — right-side tree listing the loaded trace's tables (sorted, `__intrinsic_*` / `sqlite_*` filtered out) with expandable per-table column lists fetched via `PRAGMA table_info`. Arrow keys / Enter expand and collapse. `Alt+I` inserts the highlighted name at the editor cursor. Type letters to filter (substring match, case-insensitive; tables with a matching column auto-expand). Tables referenced in the current FROM-scope render in accent-secondary so users can see at a glance which branches the query already touches. Panel shows on terminals ≥ 140 cols

### Changed
- Analysis screen tab switching via `Tab`/`Shift-Tab`; digit shortcuts `1`/`2` only active when the SQL editor doesn't have focus, so SQL content can contain those characters; `Ctrl+Q`/`Ctrl+C` exit and `Ctrl+O` opens in `ui.perfetto.dev` when text input is focused

### Removed
- **Two-trace diff screen** (pre-release) — single-sample comparison was too noisy and the canned metric set too generic to describe any specific app. Replaced by per-app saved queries (see Added). The `[space]` / `[D]` keybindings on session detail are gone
- **Peak RSS health tile** (pre-release) — redundant with the memory section's inline `peak` callout, which shows the same value scoped to the target app. Health row shrinks from four tiles to three
- **`:save <name>` colon command** (pre-release) — replaced by the explicit `Alt+S` chord which prompts for a name inline when saving a new metric or updates in place when editing an existing one. Discoverability and management (rename, delete) both improve
- **REPL Up/Down history recall** (pre-release) — superseded by the saved-metrics list (named, persistent, better)

### Fixed
- Saving a session config in the editor now returns to the session detail screen instead of the session list
- Capture screen timer no longer overshoots the configured duration during the trace-pull/flush phase — the status strip caps `elapsed` at `target`, while the completion log line still reports the full wall-clock time

## [0.4.1] - 2026-04-13

### Changed
- Removed global device selection — the header device indicator and dedicated device picker screen are gone; device selection now lives exclusively in the per-session new-session wizard
- New session device list now shows colored status badges with labels (`● online`, `○ offline`, `⚠ unauth`, `· remembered`) with aligned columns
- CPU info in the device details pane now shows core count and max clock speed (e.g. `arm64-v8a • 8 cores • 2.84 GHz`)

### Fixed
- Startup commands: strip blank or default-value `nameOrPath` args before sending to ui.perfetto.dev so the UI falls back to its own defaults instead of receiving empty strings
- Trace rename now moves the physical `.pftrace` file on disk and updates the DB path, instead of only changing the display label

### Added
- Logging in the browser handoff path — serialized startup commands JSON and final URL are now written to the trace log for debugging

## [0.4.0] - 2026-04-13

### Added
- Cloud upload support — upload individual traces or entire sessions to Google Drive
- Extensible cloud provider system via `CloudProvider` trait for future storage backends (S3, Dropbox, etc.)
- OAuth2 authentication with PKCE for Google Drive — browser-based consent flow with local redirect listener
- Resumable uploads with 5 MB chunking, real-time progress in the TUI footer, and `Esc`/`Ctrl-C` cancellation
- Automatic token refresh — re-authenticates silently when the access token expires
- Per-trace upload tracking — uploaded traces show `[Google Drive]` indicator in the trace list
- `[u]` upload selected trace / `[U]` upload all session traces from the session detail screen
- `[s]` share — copies the uploaded trace's shareable link to the clipboard (visible only for uploaded traces)
- Cloud providers management screen (`[p]` from sessions list) — login/logout, set default provider, configure upload folder path
- Configurable upload folder root per provider (defaults to `perfetto-cli/<session-name>/`)
- Upload links persisted as JSON in the `remote_url` column, supporting multiple providers per trace
- `build.rs` reads Google OAuth credentials from `.env` (local dev) or environment variables (CI) at compile time
- Google OAuth env vars (`PERFETTO_GOOGLE_CLIENT_ID`, `PERFETTO_GOOGLE_CLIENT_SECRET`) injected into release workflow
- Open session directory in OS file browser via `[d]` on the session detail screen
- **Amazon S3 cloud provider** — upload traces to S3 with access key or AWS CLI profile auth, multipart upload with progress, and 7-day presigned shareable URLs
- S3 configuration in TUI: `[b]` bucket, `[r]` region, `[a]` access key, `[s]` secret key, `[p]` AWS profile — shown when S3 provider is selected
- Provider picker for upload — when multiple providers are configured, `[u]`/`[U]` shows an inline horizontal picker (◀/▶) before the confirm step; skipped when only one provider exists
- Provider picker for share — when a trace has been uploaded to multiple providers, `[s]` lets you choose which link to copy
- Upload and share flows now use the chosen provider instead of always using the default

### Changed
- Provider list in cloud providers screen uses aligned columns for name, status, default marker, and folder
- Upload confirm prompt, progress, and success messages dynamically show the chosen provider name instead of hardcoded "Google Drive"

## [0.3.1] - 2026-04-12

### Changed
- Expanded Suggestions, Config, and Startup Commands panels in the new session wizard (6→8, 5→7, 5→7 rows) for better visibility

## [0.3.0] - 2026-04-12

### Added
- Runtime theme switching via [opaline](https://github.com/hyperb1iss/opaline) theme engine — 39 built-in themes plus custom user themes from `~/.config/perfetto-cli/themes/`
- Default "Perfetto" theme using Google's official dark-mode brand colors from ui.perfetto.dev
- Theme picker screen (`t` from sessions list) with searchable two-pane layout, live preview on navigation, and color swatches
- Theme selection persisted across sessions via `settings` table
- Global config management screen (`g` from sessions list) — create, edit, duplicate, delete reusable trace configs
- Config import via `Ctrl-I` on the config list — full-screen multiline text editor (ratatui-textarea) for pasting/typing raw textproto, saved with a user-provided name
- Config export via `Ctrl-E` on both the config list and config editor — copies the generated textproto to the system clipboard
- Config selection step in the new session wizard — pick from saved configs or "Default" when creating a session
- Split-pane new session wizard — form fields on the left (65%), device list + device info on the right (35%)
- Device info panel in the new session wizard showing hardware specs for the selected device
- Package suggestions refresh when switching between devices in the wizard
- Auto-dismissing status messages (3-second timeout via `theme::Status`) across all screens
- `custom_textproto` field on `TraceConfig` for imported configs — `textproto::build` returns it verbatim
- Config editor hides structured probe sections for imported custom configs, showing only session-level behavioral toggles
- Perfetto UI startup commands support — 14-command catalog (tracks, debug tracks, workspaces, queries, annotations)
- Command set management screen (`s` from sessions list) — create, edit, delete reusable named sets
- Command set editor with two-pane layout: command list + catalog picker, inline arg editing, `Shift-J/K` reorder
- Command set selection step in the new session wizard — pick "None" or a saved set
- Startup commands passed to ui.perfetto.dev via `&startupCommands=` URL parameter on trace open
- Scrollable textproto preview on session detail screen (`[`/`]` keys)
- Startup commands preview box on session detail screen (below textproto, auto-sized, only when commands are set)

### Changed
- `EditorContext` enum tracks whether the config editor is editing a session config, a saved config, or creating a new one — save routes to the correct DB table
- Config editor footer shows `[Ctrl-E] export` hint
- Status messages no longer block the command hints — they auto-expire instead of requiring a keypress to dismiss

## [0.2.0] - 2026-04-12

### Changed
- Config editor rewritten to mirror [ui.perfetto.dev](https://ui.perfetto.dev/#!/record) recorder UI sections
- Probe groups restructured: CPU, GPU, Power, Memory, Android Apps, Advanced — each with independently toggled sub-options and inline poll intervals
- Atrace categories are now the single source of truth (23 defaults from perfetto's Appendix A)
- Buffer default bumped from 32 MB to 64 MB to match perfetto UI
- `linux.sys_stats` fields merged into a single data source block when multiple probes need them
- Process/thread association auto-enabled as a dependency for CPU scheduling, LMK, and high-freq memory probes
- `ftrace/print` auto-added when any atrace category is enabled
- README rewritten with concise emoji feature list
- Device picker now shows a two-pane detail view with hardware specs, Android version, perfetto version, and installed apps
- Active device displayed in the app header (right-aligned inside the chrome box)

### Added
- Active device auto-selected on startup from last-used (persisted) or first known device
- `settings` key-value table in SQLite for persisting app-level state
- `query_device_info` adb helper fetches manufacturer, OS version/codename, CPU, RAM, storage, perfetto version, and installed packages
- Dynamic terminal tab/window title updates per screen
- CPU coarse usage polling probe (`linux.sys_stats` with `stat_period_ms`)
- CPU frequency/idle polling probe (`cpufreq_period_ms`)
- GPU work period probe (`power/gpu_work_period`)
- Battery drain & power rails probe (`android.power` data source)
- Board voltages probe (regulator/clock ftrace events)
- Kernel meminfo polling probe with configurable interval
- High-frequency memory events probe (mm_event, rss_stat, ion/dmabuf)
- Logcat probe with selectable log buffers (crash, default, events, kernel, system)
- Per-process stats with configurable poll interval
- Advanced ftrace settings (kernel symbol resolution, generic event filtering)
- Reference doc at `docs/perfetto-recorder-config-reference.md`

### Removed
- Built-in presets (Default, App Startup, Frame Timing, CPU Scheduling) — one session = one config
- Custom saveable presets and the `presets` DB table
- Master `enabled` toggle on probe groups — each sub-option is independent

## [0.1.0] - 2026-04-11

Initial release.

### Added
- 📦 Session management — group captures by target app + device with portable on-disk folders
- 📱 Device picker with `adb devices -l`, nicknames, and persistent memory
- ⚙️ Trace config editor with four built-in presets and a live textproto preview
- 🎬 Capture engine ported from Google's `record_android_trace`
- 🚀 Cold-start support — force-stop, perfetto, `am start`, deferred Compose broadcast
- ⏹️ Ctrl-C / Esc cancellation with SIGTERM + partial-trace pull
- 🎨 Jetpack Compose tracing via `ENABLE_TRACING` broadcast + `track_event` data source
- 🔍 Auto-injection of session package into `atrace_apps`
- 🎯 Launch activity override for LeakCanary and similar conflicts
- 🏷️ Trace rename (with `.pftrace` extension handling), tag, delete, filter by tag
- 💡 Package name suggestions from session history + live `pm list packages -3`
- 🌐 ui.perfetto.dev handoff via short-lived `tiny_http` server on `:9001` with CORS
- 📊 Auto-open traces in browser on capture completion (session-level toggle)
- 📐 Two-pane session detail showing textproto preview on wide terminals (≥120 cols)
- ⌨️ Shared text input helper with word-delete (`Alt-⌫`/`Ctrl-W`) and clear (`Ctrl-U`)
- 📁 Date-agnostic session folders with collision-safe naming
- 🕐 ISO 8601 trace filenames (`YYYY-MM-DD_HH-MM-SS.pftrace`)
- 🏠 Welcome banner with ASCII logo on empty sessions list
- 🔧 Release pipeline via cargo-dist with shell installer + Homebrew tap

[Unreleased]: https://github.com/r0adkll/perfetto-cli/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/r0adkll/perfetto-cli/compare/v0.4.1...v0.5.0[0.4.1]: https://github.com/r0adkll/perfetto-cli/compare/v0.4.0...v0.4.1[0.4.0]: https://github.com/r0adkll/perfetto-cli/compare/v0.3.1...v0.4.0[0.3.1]: https://github.com/r0adkll/perfetto-cli/compare/v0.3.0...v0.3.1[0.3.0]: https://github.com/r0adkll/perfetto-cli/compare/v0.2.0...v0.3.0[0.2.0]: https://github.com/r0adkll/perfetto-cli/compare/v0.1.0...v0.2.0[0.1.0]: https://github.com/r0adkll/perfetto-cli/releases/tag/v0.1.0
