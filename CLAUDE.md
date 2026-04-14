# AGENTS.md

Notes for AI agents (and humans) working on this repo. Keep it short, keep it
accurate, update it when conventions shift.

## Project

`perfetto-cli` is a Rust terminal UI for managing Android Perfetto trace
sessions. Built on `ratatui` + `crossterm` + `tokio`. Capture mechanics are a
direct port of Google's `record_android_trace` Python script. All state lives
under `~/.config/perfetto-cli/`:

- `perfetto-cli.db` — SQLite index (sessions, devices, traces, tags).
- `sessions/<slug>/session.json` + `traces/*.pftrace` — portable per-session
  folders that survive DB loss.
- `logs/` — rotating daily `tracing` logs. **Never write to stdout while the
  TUI is up**; that's what `tracing-appender` is for.

## Layout

```
src/
├── adb/              # async adb wrapper (list_live_devices, run, list_installed_packages)
├── perfetto/
│   ├── config.rs     # TraceConfig struct + FillPolicy enum
│   ├── presets.rs    # Preset enum with 4 variants
│   ├── textproto.rs  # TraceConfig → perfetto textproto string
│   └── capture.rs    # capture::run + Cancel primitive
├── session/          # Session struct, slug/unique_folder_path, ensure_filesystem
├── db/
│   ├── mod.rs        # Database handle (Arc<Mutex<Connection>>), migrate
│   ├── schema.sql    # idempotent CREATE TABLE IF NOT EXISTS
│   ├── devices.rs    # upsert_device_seen, list_known_devices, set/get nickname
│   ├── sessions.rs   # create/list/delete/update_config + list_recent_packages
│   └── traces.rs     # create/list/rename/delete + set_trace_tags
├── tui/
│   ├── chrome.rs     # app_header() + home_banner() + HEADER_HEIGHT const
│   ├── text_input.rs # shared line-edit helper (see "Conventions")
│   ├── event.rs      # AppEvent enum + EventBus (key + tick + async results)
│   ├── theme.rs      # color palette constants
│   └── screens/      # one file per screen, each owns its own state
├── ui_server.rs      # tiny_http server for ui.perfetto.dev handoff
├── app.rs            # top-level state machine, Screen enum, key routing
└── main.rs           # entry + tracing subscriber + db init
```

## Commands

```bash
cargo build                 # sanity check (debug)
cargo test                  # unit tests (parsers + helpers, all in #[cfg(test)] mods)
cargo run                   # launch the TUI
dist plan                   # dry-run what a release would publish
dist build --artifacts=local --target <triple>   # build one release target locally
```

**Worktrees need a `.env` copy.** `build.rs` panics if `PERFETTO_GOOGLE_CLIENT_ID`
and `PERFETTO_GOOGLE_CLIENT_SECRET` aren't reachable, and `.env` is gitignored —
so a fresh worktree won't build until you copy it over:

```bash
cp ../../.env .
```

## Conventions

- **Every text field goes through `tui::text_input::apply()` (or
  `apply_filtered` for numeric fields).** Don't inline `KeyCode::Char`/
  `Backspace` handling — the shared helper supplies `Alt-⌫`/`Ctrl-W`
  (word delete), `Cmd-⌫`/`Ctrl-U` (clear), and word boundaries that treat
  whitespace + `-` + `_` as separators. Screens interpret `TextAction::Submit`
  / `Cancel` per their own semantics.
- **Screens return `…Action` enums from `on_key`; the `App` router handles
  navigation.** Don't mutate `self.screen` from inside a screen. When an
  action carries state the app needs, pull values into locals before
  re-borrowing `self.screen` to avoid NLL conflicts (see `app.rs` for the
  `Screen::ConfigEditor` / `Screen::Capture` patterns).
- **Async work (adb queries, captures) runs via `tokio::spawn` and pushes
  results into the event bus as `AppEvent::*Loaded` / `AppEvent::Capture*`
  variants.** The main loop receives and routes them to whichever screen is
  currently active.
- **DB access is synchronous** via `Database::lock()` → `MutexGuard`. Keep
  lock scopes small. Each table's DAO lives in its own module under `db/`
  as an `impl Database` block.
- **The `Cancel` primitive in `perfetto::capture` is the cancellation pattern
  of record.** `AtomicBool` + `tokio::sync::Notify`; check `is_cancelled()`
  at phase boundaries, use `cancel.wait()` inside `tokio::select!` to break
  out of sleeps early.
- **`chrome::app_header(subtitle)` is the only way to render the top bar.**
  Don't hand-roll headers. Its `HEADER_HEIGHT` constant (5 rows) is what
  every screen's layout constraint should use.
- **Layout constraints are field-specific** — `Constraint::Length(3)` is
  still used for text-field rows in the wizard and status rows in the
  capture screen, only the header row uses `HEADER_HEIGHT`. Don't global-
  replace.

## Key flows

### Cold-start capture

1. `am force-stop <pkg>`
2. `perfetto --background --txt -c - -o /data/misc/perfetto-traces/…` with
   textproto piped via stdin → parse PID from stdout
3. Short (300ms) warmup sleep so ftrace is ready
4. Launch: `am start -n <override>` if `config.launch_activity` is set, else
   `monkey -p <pkg> -c android.intent.category.LAUNCHER 1`
5. **Defer** the `androidx.tracing.perfetto.action.ENABLE_TRACING` broadcast
   until *after* `am start` so it lands on the freshly-running process
   instead of waking a dead one
6. Poll `/proc/<pid>` every 1s, breaking on cancel
7. On cancel: `adb shell kill -TERM <pid>`, wait up to 5s, pull anyway
8. `adb pull → <session>/traces/<ISO-timestamp>.pftrace`
9. Register trace in DB, optionally auto-open in `ui.perfetto.dev`

### Warm capture

Same flow minus the `force-stop` / `am start`. Compose enable broadcast fires
*before* perfetto spawns so the app is already emitting Trace events by the
time the ring buffer goes live.

### ui.perfetto.dev handoff

`UiServer` binds `127.0.0.1:9001` via `tiny_http`, serves one successful GET
of the registered trace filename, then drops the listener and the thread
exits. `App::open_trace` reaps dead servers via `is_alive()` + `join()`
before rebinding for the next trace. The server **must** return 404 for
`/status` and anything other than the exact trace filename — the perfetto UI
probes `/status` to detect a trace_processor RPC server, and a 200 there
triggers a version handshake that fails.

## Non-obvious behaviors

- **`capture::run` injects `session.package_name` into `config.atrace_apps`**
  before building the textproto. Without this, `android.os.Trace.beginSection`
  calls from the app are no-ops because `debug.atrace.app_*` system
  properties aren't set for the target package.
- **`track_event` data source is gated on `config.compose_tracing`** in the
  textproto builder. Compose events won't make it to the buffer without it,
  even if the enable broadcast succeeds.
- **`monkey` picks LAUNCHER activities non-deterministically** when multiple
  exist (e.g., LeakCanary). The workaround is `config.launch_activity` — the
  user sets it to `.MainActivity` or a full `pkg/class` component. An earlier
  attempt at auto-resolving via `cmd package resolve-activity --brief` was
  tried and reverted; `monkey` is the default fallback.
- **Trace rename** strips/appends `.pftrace` so the user never types or sees
  the extension while editing. Spaces in rename input are rewritten to `-`
  so filenames stay shell-friendly. Tag editing is untouched (tags allow
  spaces).
- **Session folder names are date-agnostic** — `Session::unique_folder_path`
  returns `<slug>`, `<slug>-2`, `<slug>-3`, … on collision. Sessions can
  span multiple capture days without the folder drifting.
- **Trace filenames use `YYYY-MM-DD_HH-MM-SS.pftrace`** (local `_` separator,
  no timezone suffix) — readable, filesystem-safe, lex-sorts in capture order.
- **Session detail renders a two-pane layout at terminal widths ≥ 120 cols**,
  with the session/traces list on the left and the live textproto preview
  on the right. Below 120 it falls back to single-column.
- **Package suggestions in the new-session wizard** merge `list_recent_packages`
  from the DB with a live `pm list packages -3` query for the highlighted
  online device. The suggestions panel is a focus target in the Tab cycle
  (`Name → Package → Suggestions → Device → Submit`).

## Testing

Unit tests live in `#[cfg(test)]` modules at the bottom of each source file
(no integration test dir). Coverage today:

- `adb::device::tests` — `adb devices -l` parser fixtures (USB + emulator,
  unauthorized/offline, daemon startup noise, empty list).
- `perfetto::textproto::tests` — renders defaults, ftrace/apps,
  `track_event` gate, escape sequences.
- `perfetto::capture::tests` — `parse_pid`, `build_component`.
- `session::tests` — `slugify` edge cases.
- `tui::text_input::tests` — every edit shortcut + word-boundary cases for
  whitespace, `-`, and `_`.

When adding new parser/helper logic, add tests in the same file. UI
rendering is not tested.

## Release

Tag-driven via cargo-dist. See `dist-workspace.toml` and
`.github/workflows/release.yml`. Details in `README.md` → Releasing.

Quick reference:

```bash
git tag vX.Y.Z && git push origin vX.Y.Z
```

Matrix targets: macOS (x86_64 + aarch64), Linux (x86_64 + aarch64, gnu),
Windows (x86_64 msvc). Installers: `shell` + `homebrew` (tap:
`r0adkll/homebrew-tap`, needs `HOMEBREW_TAP_TOKEN` secret).

## Communication preferences

When acting as an agent in this repo:

- **Terse updates.** State what changed and what's next. No trailing summary
  paragraphs, no "in conclusion."
- **Recommend one option, don't list three.** When asked a design question,
  pick the option you'd actually ship and say why. Alternatives go as a
  short aside, not a menu.
- **Revert without resistance** when asked. The user has clarified their
  intent; don't re-argue the prior decision.
- **Don't over-engineer.** If `monkey` works for 95% of cases, don't build
  an auto-resolver that handles the 5%. Ship the user override instead.
- **Match the task scope.** Bug fixes don't get surrounding refactors.
  One-shot operations don't get helpers. Three similar lines > premature
  abstraction.
- **Respect linter/user edits mid-session.** If a file was modified outside
  your last write, read it fresh before editing. Don't clobber.
