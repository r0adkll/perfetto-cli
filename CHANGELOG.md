# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/r0adkll/perfetto-cli/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/r0adkll/perfetto-cli/compare/v0.2.0...v0.3.0[0.2.0]: https://github.com/r0adkll/perfetto-cli/compare/v0.1.0...v0.2.0[0.1.0]: https://github.com/r0adkll/perfetto-cli/releases/tag/v0.1.0
