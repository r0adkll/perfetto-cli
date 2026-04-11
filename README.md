# perfetto-cli

A Rust terminal UI for managing Android [Perfetto](https://perfetto.dev) trace
sessions. Organize captures by target app, pick devices from a live adb list,
tune your trace config with presets plus a live textproto preview, run
captures with cold-start support and Ctrl-C cancellation, and open finished
traces in `ui.perfetto.dev` with one key.

## Features

- **Sessions** group captures by a target Android app on a specific device.
  Everything lives on disk in a portable per-session folder plus a SQLite
  index for queryability.
- **Device picker** reads `adb devices -l`, remembers devices and lets you
  nickname them so switching targets is cheap.
- **Config editor** with four presets (Default, App startup, Frame timing,
  CPU scheduling), numeric and multi-value fields, and a side-by-side
  textproto preview that updates live as you edit.
- **Capture engine** ported from Google's `record_android_trace` Python
  script: pipes the textproto config into `perfetto --background --txt -c -`,
  polls `/proc/<pid>` on-device, and pulls the resulting `.pftrace` into the
  session folder.
- **Cold-start flow**: force-stop → start perfetto → `am start` the target
  activity → defer the Jetpack Compose enable broadcast until after launch so
  it doesn't wake the process and ruin the trace window.
- **Ctrl-C / Esc cancellation** during a capture sends `SIGTERM` to perfetto
  on-device, waits for it to flush, and pulls the partial trace so no work is
  thrown away.
- **Jetpack Compose tracing** via the
  `androidx.tracing.perfetto.action.ENABLE_TRACING` broadcast and the
  `track_event` data source.
- **Per-app atrace injection** — the session's `package_name` is auto-added
  to `atrace_apps` so `android.os.Trace.beginSection()` calls from the app
  actually land in the trace (they're no-ops without it).
- **Explicit launch activity override** for apps where `monkey` picks the
  wrong LAUNCHER activity (LeakCanary, multi-launcher modules, etc.).
- **Trace management** — rename (with automatic `.pftrace` suffix handling),
  tag, delete, and filter by tag. Rename input rewrites spaces to dashes for
  filesystem-friendly names.
- **Package name suggestions** in the new-session wizard: recent packages
  from your session history, merged with a live `pm list packages -3` query
  for the currently-highlighted online device. Filter by typing, select with
  Tab + arrow keys.
- **ui.perfetto.dev handoff** — spawns a short-lived `tiny_http` server on
  `127.0.0.1:9001` with the exact CORS headers the Perfetto UI expects,
  serves one trace, then drops the listener so the port is released.
- **Auto-open on completion** (session-level toggle) — successful captures
  open straight into the Perfetto UI.
- **Two-pane session detail** — on terminals ≥ 120 columns, the session
  screen shows the trace list on the left and the generated textproto on
  the right.

## Requirements

- Rust 1.90+ (edition 2024)
- `adb` on `PATH`
- Android device on API 29+ with USB debugging enabled
- A terminal with 256-color / truecolor support (for the accent palette)

## Install

```bash
# run from a checkout
cargo run --release

# or install into ~/.cargo/bin
cargo install --path .
```

## Getting started

1. Launch: `perfetto-cli`
2. Press `d` to open the **Devices** screen, confirm your device shows up as
   `online`, and optionally press `n` to give it a nickname. `Esc` back.
3. Press `n` on the sessions list to open the **New session** wizard.
   - **Name** — any human-readable label.
   - **Package** — start typing and the **Suggestions** panel below will
     filter down recent packages + installed apps from the highlighted
     online device. `Tab` into the list, arrow keys to pick, `Enter` to
     fill.
   - **Device** — pick from the detected online devices.
   - `Tab` to the **Create session** button and press `Enter`.
4. On the session detail screen:
   - `c` — run a capture
   - `o` / `Enter` — open the selected trace in `ui.perfetto.dev`
   - `e` — edit the trace config
   - `r` — rename a trace (extension is handled automatically)
   - `t` — tag a trace (comma-separated)
   - `f` — cycle the tag filter
   - `x` / `Delete` — delete a trace (with confirmation)

## Configuration

Everything perfetto-cli stores lives under `~/.config/perfetto-cli/` on every
platform:

```
~/.config/perfetto-cli/
├── perfetto-cli.db          # SQLite index (sessions, devices, traces, tags)
├── logs/                    # rotating daily logs (never stdout while TUI is up)
└── sessions/
    └── <session-slug>/
        ├── session.json     # self-describing snapshot (portable w/o the DB)
        └── traces/
            └── 2026-04-11_14-30-22.pftrace
```

Session folders are **date-agnostic** — a session can span multiple capture
days without drifting from its creation date.

## Trace configuration

Press `e` on any session to open the config editor. Fields (top to bottom):

| Field | Notes |
|---|---|
| **Preset** | Cycle with `←/→`. Rewrites the other fields to match. |
| **Duration (ms)** | Numeric only. |
| **Buffer (KB)** | Numeric only. |
| **Fill policy** | `ring buffer` / `discard`, cycle with `←/→`. |
| **Cold start** | Toggle with `Space`. Force-stops + restarts the app for a clean startup trace. |
| **Auto-open** | Toggle. Opens in `ui.perfetto.dev` on successful capture. |
| **Compose tracing** | Toggle. Fires the androidx enable broadcast; warm-path fires before perfetto starts, cold-path defers until after `am start`. |
| **Launch activity** | Optional override like `.MainActivity` or `com.example/.MainActivity`. Leave blank to fall back to `monkey`. |
| **Categories** | Comma-separated atrace categories (`sched, freq, gfx, …`). |
| **Ftrace events** | Comma-separated ftrace event paths (`power/cpu_frequency, …`). |
| **Atrace apps** | Extra packages to enable app-level tracing for. The session's own package is added automatically. |

`Ctrl-S` saves, `Esc` cancels. Right-hand panel shows the rendered textproto
live as you edit.

## Text input shortcuts

Every text field in the app runs through one shared helper:

| Shortcut | Effect |
|---|---|
| `Backspace` | delete previous character |
| `Alt-⌫` / `Ctrl-W` | delete previous word (boundaries: whitespace, `-`, `_`) |
| `Cmd-⌫` / `Ctrl-U` | clear the buffer |
| `Enter` | submit / advance |
| `Esc` | cancel |

## Project layout

```
src/
├── adb/                    # async adb wrapper + device parser
├── perfetto/               # config model, presets, textproto builder, capture engine
├── session/                # session struct + filesystem lifecycle
├── db/                     # rusqlite DAOs (devices, sessions, traces, tags)
├── tui/
│   ├── chrome.rs           # shared header + home banner
│   ├── text_input.rs       # shared line-edit helper with edit shortcuts
│   ├── event.rs            # async event bus
│   └── screens/            # one file per screen
├── ui_server.rs            # tiny_http server for ui.perfetto.dev handoff
├── app.rs                  # top-level state machine + routing
└── main.rs
```

## Testing

```bash
cargo test
```

Unit tests cover the `adb devices -l` parser, the textproto builder (including
escape sequences and the `track_event` gate), capture helpers (`parse_pid`,
`build_component`), the slugify function for session folders, and the shared
text-input helper's edit shortcuts.

## Credits

Capture mechanics are a direct port of Google's
[`record_android_trace`](https://github.com/google/perfetto/blob/main/tools/record_android_trace)
Python script. Perfetto itself is Apache 2.0 licensed.
