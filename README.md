# 📊 perfetto-cli

A terminal UI for capturing and managing [Perfetto](https://perfetto.dev) traces sessions on Android.

![](.github/art/demo.gif)

## Features

| |  |  |
|---|---|---|
| 📱 | **Device picker** | Live `adb devices` with nicknames and memory |
| 📦 | **Session management** | Group captures by app + device, stored in portable folders |
| ⚙️ | **Config editor** | Grouped probe toggles mirroring [ui.perfetto.dev](https://ui.perfetto.dev/#!/record) with a live textproto preview |
| 📋 | **Global configs** | Save, duplicate, and reuse trace configurations across sessions; import raw textproto via paste, export to clipboard |
| 🎬 | **Capture engine** | Ported from Google's `record_android_trace`, with `Ctrl-C` cancellation and partial-trace pull |
| 🚀 | **Cold-start support** | Force-stop, trace, launch, with deferred Compose broadcast |
| 🎨 | **Compose tracing** | `track_event` data source + `ENABLE_TRACING` broadcast |
| 🏷️ | **Trace management** | Rename, tag, delete, filter by tag |
| 🌐 | **ui.perfetto.dev handoff** | One-key open via a short-lived local HTTP server with optional startup commands |
| 🧩 | **Startup commands** | Build reusable command sets from a 14-command catalog and pass them to the Perfetto UI on open |
| ☁️ | **Cloud upload** | Upload traces to Google Drive or Amazon S3 with progress, cancellation, and shareable links |
| 🔀 | **Multi-provider picker** | Choose which cloud provider to upload to or share from when multiple are configured |
| 🎨 | **Theming** | 39 built-in themes via a searchable picker, plus custom themes in `~/.config/perfetto-cli/themes/` |

## Requirements

- `adb` on `PATH`
- Android device on API 29+ with USB debugging enabled

## Install

**Shell installer (macOS / Linux):**

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/r0adkll/perfetto-cli/releases/latest/download/perfetto-cli-installer.sh \
  | sh
```

**Homebrew:**

```bash
brew install r0adkll/tap/perfetto-cli
```

**Windows:** grab `perfetto-cli-x86_64-pc-windows-msvc.zip` from the [latest release](https://github.com/r0adkll/perfetto-cli/releases).

**From source:**

```bash
cargo install --path .
```

## Quick start

```
perfetto-cli
```

1. `d` — confirm your device is online, optionally nickname it
2. `n` — create a session (name, package with suggestions, device)
3. `e` — edit the trace config (probe toggles, atrace categories, poll intervals)
4. `c` — capture a trace
5. `o` — open it in ui.perfetto.dev

## Config editor

Press `e` on any session. The editor mirrors the [perfetto recorder UI](https://ui.perfetto.dev/#!/record) sections:

| Section | What it controls |
|---|---|
| **Recording** | Duration, buffer size, fill policy, cold start, auto-open, Compose tracing, launch activity |
| **CPU** | Coarse usage polling, scheduling details, frequency/idle, syscalls |
| **GPU** | Frequency, memory, work period |
| **Power** | Battery drain + power rails, board voltages |
| **Memory** | Kernel meminfo, high-freq events, LMK, per-process stats |
| **Android Apps** | 38 atrace categories (23 default), logcat buffers, frame timeline, atrace apps |
| **Advanced** | Kernel symbol resolution, generic events, extra ftrace events |

Each probe group expands to show sub-options with descriptions. Toggles that have a poll interval reveal a number field when enabled. The right panel shows the generated textproto updating live.

`Ctrl-S` saves. `Esc` cancels. `Space` toggles. `Enter` expands groups or starts editing fields. `←` collapses from inside a group.

## Theming

Press `t` from the home screen to open the theme picker. Search and preview any of the 39 built-in themes provided by [opaline](https://github.com/r0adkll/opaline), or create your own.

Custom themes are `.toml` files dropped into `~/.config/perfetto-cli/themes/`. A theme defines a color palette, semantic tokens, and named styles. See the bundled [`perfetto.toml`](assets/perfetto.toml) for the full format, or refer to the [opaline theme spec](https://github.com/r0adkll/opaline#theme-format).

Your selected theme persists across sessions.

## Text input shortcuts

| Shortcut | Effect |
|---|---|
| `Backspace` | Delete character |
| `Alt-⌫` / `Ctrl-W` | Delete word (`-` and `_` are boundaries) |
| `Cmd-⌫` / `Ctrl-U` | Clear buffer |

## Data storage

```
~/.config/perfetto-cli/
├── perfetto-cli.db        # SQLite (sessions, devices, traces, tags)
├── logs/                  # Rotating daily logs
└── sessions/
    └── <slug>/
        ├── session.json   # Portable session snapshot
        └── traces/
            └── 2026-04-12_09-15-30.pftrace
```

## Project layout

```
src/
├── adb/          # Async adb wrapper + device parser
├── perfetto/     # Config model, textproto builder, capture engine
├── session/      # Session struct + filesystem lifecycle
├── db/           # SQLite DAOs (devices, sessions, traces, tags)
├── tui/          # ratatui screens, chrome, text input, event bus
├── ui_server.rs  # tiny_http for ui.perfetto.dev handoff
├── app.rs        # Screen router + state machine
└── main.rs       # Entry point
```

## Testing

```bash
cargo test   # 54 tests
```

## Releasing

Tag-driven via [cargo-dist](https://github.com/axodotdev/cargo-dist). Use the release script:

```bash
./scripts/release.sh patch   # 0.1.0 → 0.1.1
./scripts/release.sh minor   # 0.1.0 → 0.2.0
./scripts/release.sh major   # 0.1.0 → 1.0.0
./scripts/release.sh 2.0.0   # explicit version
```

The script bumps `Cargo.toml`, updates `CHANGELOG.md` (moves `[Unreleased]` into the new version, updates comparison links), commits, tags, and pushes. cargo-dist then builds for macOS (Intel + ARM), Linux (x86_64 + aarch64), and Windows, publishing archives, checksums, a shell installer, and a Homebrew formula to [`r0adkll/homebrew-tap`](https://github.com/r0adkll/homebrew-tap).

## Credits

Capture engine ported from Google's [`record_android_trace`](https://github.com/google/perfetto/blob/main/tools/record_android_trace). Perfetto is Apache 2.0 licensed.

## License

[MIT](LICENSE)
