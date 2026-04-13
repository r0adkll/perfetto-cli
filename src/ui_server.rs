use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::perfetto::commands::{self, StartupCommand};

/// Short-lived local HTTP server that hands a `.pftrace` over to ui.perfetto.dev.
///
/// Binds `127.0.0.1:9001`, answers CORS preflight + one GET of the loaded
/// trace, then tears itself down so the listener socket is released. The
/// `App` keeps at most one instance and reaps dead ones before opening the
/// next trace.
pub struct UiServer {
    current: Arc<Mutex<Option<PathBuf>>>,
    alive: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

const BIND_ADDR: &str = "127.0.0.1:9001";
const UI_ORIGIN: &str = "https://ui.perfetto.dev";
const POLL_INTERVAL: Duration = Duration::from_millis(200);

impl UiServer {
    pub fn start() -> Result<Self> {
        let server = Server::http(BIND_ADDR)
            .map_err(|e| anyhow!("failed to bind {BIND_ADDR}: {e}"))?;
        let current: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let alive = Arc::new(AtomicBool::new(true));

        let current_for_thread = current.clone();
        let alive_for_thread = alive.clone();
        let handle = std::thread::Builder::new()
            .name("perfetto-cli-ui-server".into())
            .spawn(move || {
                while alive_for_thread.load(Ordering::Relaxed) {
                    match server.recv_timeout(POLL_INTERVAL) {
                        Ok(Some(request)) => {
                            if handle_request(request, &current_for_thread) {
                                // Trace delivered — flag ourselves dead and
                                // fall out of the loop. Dropping `server`
                                // at the end of the closure releases :9001.
                                alive_for_thread.store(false, Ordering::Relaxed);
                            }
                        }
                        Ok(None) => {} // idle tick, re-check the alive flag
                        Err(e) => {
                            tracing::warn!(error = %e, "ui_server recv failed");
                            break;
                        }
                    }
                }
                drop(server);
                tracing::info!("ui_server shut down, socket released");
            })
            .context("failed to spawn ui server thread")?;

        Ok(Self {
            current,
            alive,
            handle: Some(handle),
        })
    }

    /// Register the trace file to serve and return the ui.perfetto.dev URL
    /// that loads it. If `startup_commands` is non-empty, they're serialized
    /// to JSON and appended as the `startupCommands` URL parameter so the
    /// Perfetto UI executes them when the trace loads.
    pub fn serve(
        &self,
        trace_path: &Path,
        startup_commands: &[StartupCommand],
    ) -> Result<String> {
        let canonical = trace_path
            .canonicalize()
            .unwrap_or_else(|_| trace_path.to_path_buf());
        *self.current.lock().expect("ui_server mutex poisoned") = Some(canonical.clone());

        let filename = canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "trace.pftrace".into());

        let commands_param = if startup_commands.is_empty() {
            tracing::debug!("no startup commands to attach");
            String::new()
        } else {
            let json = commands::serialize_commands(startup_commands);
            tracing::info!(
                count = startup_commands.len(),
                json = %json,
                "attaching startup commands to handoff URL"
            );
            format!(
                "&startupCommands={}",
                urlencoding::encode(&json)
            )
        };

        let url = format!(
            "{UI_ORIGIN}/#!/?url=http://127.0.0.1:9001/{filename}&referrer=perfetto-cli{commands_param}"
        );

        tracing::info!(
            trace = %canonical.display(),
            %url,
            "opening trace in browser"
        );
        webbrowser::open(&url).context("failed to launch browser")?;
        Ok(url)
    }

    /// `true` while the server thread is still running. Flips to `false`
    /// after a trace has been successfully transferred (and before the
    /// thread fully exits), so callers can detect a completed session
    /// quickly without blocking.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Block until the server thread has exited and the socket is released.
    /// Safe to call after `is_alive()` returns `false` — it'll return
    /// immediately once the thread finishes its drop.
    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

}

impl Drop for UiServer {
    fn drop(&mut self) {
        // Make sure the thread exits if the handle wasn't joined explicitly.
        self.alive.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Returns `true` if the request was a successful GET of the loaded trace
/// (i.e., it's time to shut the server down).
fn handle_request(request: tiny_http::Request, current: &Arc<Mutex<Option<PathBuf>>>) -> bool {
    let method = request.method().clone();
    let url = request.url().to_string();
    tracing::debug!(?method, url, "ui_server request");

    match method {
        Method::Options => {
            let _ = request.respond(with_cors(Response::empty(StatusCode(200))));
            false
        }
        Method::Get => {
            let current_path = current.lock().expect("ui_server mutex poisoned").clone();
            let Some(trace_path) = current_path else {
                let _ = request.respond(with_cors(
                    Response::from_string("no trace loaded").with_status_code(404),
                ));
                return false;
            };
            let Some(expected_name) = trace_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
            else {
                let _ = request.respond(with_cors(
                    Response::from_string("invalid trace path").with_status_code(500),
                ));
                return false;
            };

            let req_path = url
                .split('?')
                .next()
                .unwrap_or("")
                .trim_start_matches('/');
            if req_path != expected_name {
                let _ = request.respond(with_cors(
                    Response::from_string("not found").with_status_code(404),
                ));
                return false;
            }

            match std::fs::File::open(&trace_path) {
                Ok(f) => {
                    let response = with_cors(Response::from_file(f));
                    match request.respond(response) {
                        Ok(()) => {
                            // Clear the slot too, just in case recv_timeout
                            // wakes with a stray request before the loop
                            // sees the `alive` flag.
                            *current.lock().expect("ui_server mutex poisoned") = None;
                            tracing::info!(
                                path = %trace_path.display(),
                                "ui_server served trace; shutting down"
                            );
                            true
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %trace_path.display(),
                                error = %e,
                                "ui_server respond failed mid-stream; keeping server alive"
                            );
                            false
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %trace_path.display(), error = %e, "ui_server failed to open trace");
                    let _ = request.respond(with_cors(
                        Response::from_string(format!("failed to open trace: {e}"))
                            .with_status_code(500),
                    ));
                    false
                }
            }
        }
        _ => {
            let _ = request.respond(with_cors(Response::empty(StatusCode(404))));
            false
        }
    }
}

fn with_cors<R: std::io::Read>(response: Response<R>) -> Response<R> {
    response
        .with_header(header("Access-Control-Allow-Origin", UI_ORIGIN))
        .with_header(header("Access-Control-Allow-Methods", "GET, POST, OPTIONS"))
        .with_header(header(
            "Access-Control-Allow-Headers",
            "Content-Type, Cache-Control",
        ))
        .with_header(header(
            "Access-Control-Expose-Headers",
            "Content-Length, Content-Range",
        ))
        .with_header(header("Cache-Control", "no-cache"))
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("hardcoded header literals are valid")
}
