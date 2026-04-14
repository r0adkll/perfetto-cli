use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;

use crate::adb;

use super::TraceConfig;
use super::textproto;

/// Cooperative cancellation handle. The engine checks `is_cancelled` at
/// natural boundaries and uses `wait` inside `tokio::select!` to break out of
/// sleeps early.
#[derive(Debug, Default)]
pub struct Cancel {
    flag: AtomicBool,
    notify: Notify,
}

impl Cancel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// Resolves as soon as `cancel()` is called. Resolves immediately if the
    /// flag is already set.
    pub async fn wait(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

/// All the context a capture run needs. Built by the caller from a `Session`.
#[derive(Debug, Clone)]
pub struct CaptureRequest {
    #[allow(dead_code)]
    pub session_id: i64,
    pub session_folder: PathBuf,
    pub device_serial: String,
    pub package_name: String,
    pub config: TraceConfig,
    /// User-supplied trace filename stem (no extension). When `Some`, the
    /// pulled trace is named `<stem>.pftrace` instead of the timestamped
    /// default; on collision the engine appends `-2`, `-3`, … to keep the
    /// existing files intact.
    pub custom_filename: Option<String>,
}

/// Events emitted by the capture engine during a run.
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    Log(LogEntry),
    DeviceProcess(u32),
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Ok,
    Warn,
    #[allow(dead_code)]
    Err,
}

#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub trace_path: PathBuf,
    pub duration_ms: u64,
    pub size_bytes: u64,
    pub cancelled: bool,
}

/// Execute a capture end-to-end: optional cold-start pre-hook → spawn
/// `perfetto --background` with the textproto piped via stdin → optional app
/// launch → poll for completion → pull the trace file to the session folder.
///
/// Honors `cancel`: boundaries check `is_cancelled`, the poll sleep is broken
/// by `cancel.wait()`, and on cancel perfetto is SIGTERM'd on-device so it
/// flushes whatever it had.
pub async fn run(
    request: CaptureRequest,
    tx: UnboundedSender<CaptureEvent>,
    cancel: Arc<Cancel>,
) -> Result<CaptureResult> {
    let CaptureRequest {
        session_folder,
        device_serial,
        package_name,
        mut config,
        custom_filename,
        ..
    } = request;

    // Ensure the session's target package is always in `atrace_apps`. Without
    // this, `android.os.Trace.beginSection()` calls inside the app are
    // silently dropped — atrace gates per-app events behind `debug.atrace.app_*`
    // system properties, which only get set when the package is listed. This
    // matches what `record_android_trace -a <pkg>` does.
    if !config.atrace_apps.iter().any(|a| a == &package_name) {
        config.atrace_apps.push(package_name.clone());
        log(
            &tx,
            LogLevel::Info,
            format!("Enabling app-level tracing for {package_name}"),
        );
    }

    if cancel.is_cancelled() {
        bail!("cancelled before start");
    }

    if config.cold_start {
        log(&tx, LogLevel::Info, format!("Force-stopping {package_name}"));
        adb::run(&device_serial, &["shell", "am", "force-stop", &package_name])
            .await
            .context("am force-stop failed")?;
        log(&tx, LogLevel::Ok, "force-stop complete".into());
    } else if config.compose_tracing {
        // Warm path: fire the Compose enable broadcast before perfetto so
        // the app is already emitting Trace events by the time the ring
        // buffer is live. (On cold-start this is deferred until after
        // `am start` — see below — so the broadcast doesn't wake the
        // process prematurely.)
        enable_compose_tracing(&device_serial, &package_name, &tx).await;
    }

    if cancel.is_cancelled() {
        bail!("cancelled before perfetto start");
    }

    let device_path = format!(
        "/data/misc/perfetto-traces/perfetto-cli-{}.pftrace",
        Utc::now().timestamp_millis()
    );

    let textproto_str = textproto::build(&config);
    log(
        &tx,
        LogLevel::Info,
        format!("Starting perfetto on device ({device_path})"),
    );

    let start = Instant::now();
    let pid = spawn_perfetto(&device_serial, &device_path, &textproto_str)
        .await
        .context("failed to start perfetto on device")?;
    log(&tx, LogLevel::Ok, format!("perfetto started (pid {pid})"));
    let _ = tx.send(CaptureEvent::DeviceProcess(pid));

    if config.cold_start && !cancel.is_cancelled() {
        // Tiny delay: perfetto has already backgrounded when we read the PID,
        // but this gives ftrace a moment to warm up before the app launch.
        tokio::time::sleep(Duration::from_millis(300)).await;
        if !cancel.is_cancelled() {
            // If the user pinned a specific activity in the session config,
            // launch it explicitly via `am start -n`. Otherwise fall back to
            // `monkey`, which is fine for apps that only have a single
            // LAUNCHER activity (and the user will pin one in the editor
            // when that isn't true).
            let override_spec = config
                .launch_activity
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match override_spec {
                Some(spec) => {
                    let component = build_component(&package_name, spec);
                    log(&tx, LogLevel::Info, format!("Starting {component}"));
                    adb::run(&device_serial, &["shell", "am", "start", "-n", &component])
                        .await
                        .context("am start failed")?;
                }
                None => {
                    log(&tx, LogLevel::Info, format!("Starting {package_name}"));
                    adb::run(
                        &device_serial,
                        &[
                            "shell",
                            "monkey",
                            "-p",
                            &package_name,
                            "-c",
                            "android.intent.category.LAUNCHER",
                            "1",
                        ],
                    )
                    .await
                    .context("monkey launch failed")?;
                }
            }
            log(&tx, LogLevel::Ok, "app launched".into());

            // Cold path: send the Compose enable broadcast immediately after
            // the app launches so it lands on the freshly-started process
            // instead of waking a dead one before `am start`.
            if config.compose_tracing {
                enable_compose_tracing(&device_serial, &package_name, &tx).await;
            }
        }
    }

    log(
        &tx,
        LogLevel::Info,
        format!("Capturing for {} ms", config.duration_ms),
    );

    let was_cancelled = poll_until_done(&device_serial, pid, &cancel).await?;

    if was_cancelled {
        log(&tx, LogLevel::Warn, "cancel requested — stopping perfetto".into());
        // Best-effort SIGTERM. perfetto should flush the current buffer and exit.
        if let Err(e) = adb::run(
            &device_serial,
            &["shell", "kill", "-TERM", &pid.to_string()],
        )
        .await
        {
            log(&tx, LogLevel::Warn, format!("kill -TERM failed: {e}"));
        }
        // Give it up to 5s to exit cleanly.
        let deadline = Instant::now() + Duration::from_secs(5);
        while perfetto_still_running(&device_serial, pid)
            .await
            .unwrap_or(false)
        {
            if Instant::now() >= deadline {
                log(
                    &tx,
                    LogLevel::Warn,
                    "perfetto did not exit within 5s, continuing anyway".into(),
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        log(&tx, LogLevel::Ok, "perfetto stopped".into());
    }

    let total_ms = start.elapsed().as_millis() as u64;
    log(
        &tx,
        LogLevel::Ok,
        if was_cancelled {
            "capture stopped early — pulling partial trace".into()
        } else {
            "capture complete".into()
        },
    );

    let traces_dir = session_folder.join("traces");
    std::fs::create_dir_all(&traces_dir).context("create traces dir")?;
    // Default name: `YYYY-MM-DD_HH-MM-SS` — readable at a glance, filesystem-
    // safe, still sorts lexicographically in capture order. A user-supplied
    // stem overrides it; collisions get a `-2`, `-3`, … suffix instead of
    // overwriting an existing file.
    let local_path = match custom_filename.as_deref() {
        Some(stem) => unique_trace_path(&traces_dir, stem),
        None => traces_dir.join(format!("{}.pftrace", Utc::now().format("%Y-%m-%d_%H-%M-%S"))),
    };
    log(&tx, LogLevel::Info, "Pulling trace from device".into());
    adb::run(
        &device_serial,
        &[
            "pull",
            &device_path,
            local_path
                .to_str()
                .context("trace path is not valid UTF-8")?,
        ],
    )
    .await
    .context("adb pull failed")?;
    log(&tx, LogLevel::Ok, format!("saved to {}", local_path.display()));

    // Best-effort cleanup — don't fail the capture over it.
    if let Err(e) = adb::run(&device_serial, &["shell", "rm", &device_path]).await {
        log(
            &tx,
            LogLevel::Warn,
            format!("failed to remove device file: {e}"),
        );
    }

    let size_bytes = std::fs::metadata(&local_path)
        .context("stat pulled trace file")?
        .len();

    Ok(CaptureResult {
        trace_path: local_path,
        duration_ms: total_ms,
        size_bytes,
        cancelled: was_cancelled,
    })
}

/// Poll the on-device perfetto process. Returns `true` if the loop exited due
/// to cancellation, `false` if perfetto finished on its own.
async fn poll_until_done(serial: &str, pid: u32, cancel: &Arc<Cancel>) -> Result<bool> {
    let mut poll_interval = tokio::time::interval(Duration::from_secs(1));
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    poll_interval.tick().await; // consume the immediate initial tick

    loop {
        if cancel.is_cancelled() {
            return Ok(true);
        }

        if !perfetto_still_running(serial, pid).await? {
            return Ok(false);
        }

        tokio::select! {
            _ = poll_interval.tick() => {}
            _ = cancel.wait() => {}
        }
    }
}

async fn spawn_perfetto(serial: &str, device_path: &str, textproto_str: &str) -> Result<u32> {
    let mut child = Command::new("adb")
        .arg("-s")
        .arg(serial)
        .args([
            "shell",
            "perfetto",
            "--background",
            "--txt",
            "-c",
            "-",
            "-o",
            device_path,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn adb shell perfetto")?;

    {
        let mut stdin = child.stdin.take().context("perfetto stdin unavailable")?;
        stdin
            .write_all(textproto_str.as_bytes())
            .await
            .context("write textproto to perfetto stdin")?;
        stdin.shutdown().await.ok();
    }

    let output = child
        .wait_with_output()
        .await
        .context("wait for perfetto to background")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("perfetto start failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_pid(&stdout)
        .with_context(|| format!("perfetto did not emit a PID; stdout was: {stdout:?}"))
}

async fn perfetto_still_running(serial: &str, pid: u32) -> Result<bool> {
    let cmd = format!("test -d /proc/{pid} && echo RUN || echo TERM");
    let out = adb::run(serial, &["shell", &cmd]).await?;
    Ok(out.trim() == "RUN")
}

/// Fire the androidx.tracing.perfetto enable broadcast at the app. This
/// flips the `TracingReceiver` inside the target process into "emit Compose
/// events" mode. Failures are soft-logged — the most common reason is that
/// the app simply doesn't ship the `androidx.tracing:tracing-perfetto`
/// library, in which case there's nothing to turn on and the rest of the
/// capture is still valid.
async fn enable_compose_tracing(
    serial: &str,
    package: &str,
    tx: &UnboundedSender<CaptureEvent>,
) {
    let component = format!("{package}/androidx.tracing.perfetto.TracingReceiver");
    log(tx, LogLevel::Info, format!("Enabling Compose tracing in {package}"));
    match adb::run(
        serial,
        &[
            "shell",
            "am",
            "broadcast",
            "-a",
            "androidx.tracing.perfetto.action.ENABLE_TRACING",
            "-n",
            &component,
        ],
    )
    .await
    {
        Ok(_) => log(tx, LogLevel::Ok, "Compose tracing broadcast sent".into()),
        Err(e) => log(
            tx,
            LogLevel::Warn,
            format!(
                "Compose tracing broadcast failed (is androidx.tracing:tracing-perfetto on the app classpath?): {e}"
            ),
        ),
    }
}

/// Normalize a user-provided activity string into an `am start -n` component:
/// - `pkg/class` → pass through
/// - `.MainActivity` → `<package>/.MainActivity`
/// - `com.foo.MainActivity` → `<package>/com.foo.MainActivity`
fn build_component(package: &str, input: &str) -> String {
    if input.contains('/') {
        input.to_string()
    } else {
        format!("{package}/{input}")
    }
}

fn parse_pid(stdout: &str) -> Option<u32> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find_map(|l| l.parse::<u32>().ok())
}

fn log(tx: &UnboundedSender<CaptureEvent>, level: LogLevel, message: String) {
    let _ = tx.send(CaptureEvent::Log(LogEntry { level, message }));
}

/// Resolve `<traces_dir>/<stem>.pftrace`, appending `-2`, `-3`, … until the
/// path is free. Mirrors `Session::unique_folder_path` so user-named captures
/// never silently overwrite an existing trace.
fn unique_trace_path(traces_dir: &std::path::Path, stem: &str) -> PathBuf {
    let first = traces_dir.join(format!("{stem}.pftrace"));
    if !first.exists() {
        return first;
    }
    let mut n: u32 = 2;
    loop {
        let candidate = traces_dir.join(format!("{stem}-{n}.pftrace"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pid_finds_first_number() {
        assert_eq!(parse_pid("12345\n"), Some(12345));
        assert_eq!(parse_pid("starting...\n98765\nready\n"), Some(98765));
        assert_eq!(parse_pid(""), None);
        assert_eq!(parse_pid("no number here\n"), None);
    }

    #[test]
    fn build_component_passes_through_full_component() {
        assert_eq!(
            build_component("com.example", "com.other/.Main"),
            "com.other/.Main"
        );
    }

    #[test]
    fn build_component_prepends_package_for_relative_class() {
        assert_eq!(
            build_component("com.example", ".MainActivity"),
            "com.example/.MainActivity"
        );
    }

    #[test]
    fn build_component_prepends_package_for_absolute_class() {
        assert_eq!(
            build_component("com.example", "com.example.ui.MainActivity"),
            "com.example/com.example.ui.MainActivity"
        );
    }

    #[test]
    fn unique_trace_path_uses_stem_when_free() {
        let dir = std::env::temp_dir().join(format!(
            "perfetto-cli-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = unique_trace_path(&dir, "my-capture");
        assert_eq!(path, dir.join("my-capture.pftrace"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unique_trace_path_appends_suffix_on_collision() {
        let dir = std::env::temp_dir().join(format!(
            "perfetto-cli-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("my-capture.pftrace"), b"").unwrap();
        let path = unique_trace_path(&dir, "my-capture");
        assert_eq!(path, dir.join("my-capture-2.pftrace"));

        std::fs::write(dir.join("my-capture-2.pftrace"), b"").unwrap();
        let path = unique_trace_path(&dir, "my-capture");
        assert_eq!(path, dir.join("my-capture-3.pftrace"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
