---
name: perfetto-cli
description: Perfetto traces of Android apps via the `perfetto-cli` command. Use to capture a trace (cold start, jank, a single run), organize captures into sessions, upload/share traces, or open one in ui.perfetto.dev with startup commands.
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

## Gotchas

- **Uploads may need a human.** Credentials live in the TUI's cloud settings. If upload fails on authentication, stop and ask the user to set up the provider by running `perfetto-cli` with no arguments.
- **`open` needs a human at a browser.** `perfetto-cli open <trace-id> --json` launches ui.perfetto.dev, applies the session's startup commands, and waits (default 60s) for the browser to fetch the trace. Use it only when the user wants to look at the trace now.
- **Startup command sets merge in `command-sets` list order**, whatever order you pass `--commands` in. Session JSON reports the matched sets under `command_sets`.
- **Session settings persist.** `session update <session> ...` changes package, device, commands, or capture settings for every later capture; prefer per-capture overrides for one-off experiments.
- **Imported Macrobenchmark sessions are read-only**; capture into a regular session instead.
- **Interactive commands.** Running `perfetto-cli` with no subcommand launches a full-screen TUI, and `perfetto-cli clear` deletes every session; run either only when the user asks.
