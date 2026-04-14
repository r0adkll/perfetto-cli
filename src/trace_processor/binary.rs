//! Locate or install `trace_processor_shell` under `~/.config/perfetto-cli/bin/`.
//!
//! Binaries are pinned to a specific Perfetto release (see `PINNED_VERSION`).
//! Each supported platform has a known URL and SHA-256 that we verify after
//! downloading so a corrupted or swapped upload can't be silently installed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Paths;
use crate::perfetto::capture::Cancel;

/// Pinned Perfetto release we install `trace_processor_shell` from. Bump this
/// alongside the URL/SHA table below when updating.
pub const PINNED_VERSION: &str = "v54.0";

/// Per-platform download metadata.
struct PlatformArtifact {
    url: &'static str,
    sha256: &'static str,
}

/// Resolve the download artifact for the host platform, or `None` if
/// unsupported. Architectures map to the slugs Perfetto publishes under
/// `perfetto-luci-artifacts/<version>/`.
fn host_artifact() -> Option<PlatformArtifact> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => Some(PlatformArtifact {
            url: "https://commondatastorage.googleapis.com/perfetto-luci-artifacts/v54.0/mac-amd64/trace_processor_shell",
            sha256: "a15360712875344d8bb8e4c461cd7ce9ec250f71a76f89e6ae327c5185eb4855",
        }),
        ("macos", "aarch64") => Some(PlatformArtifact {
            url: "https://commondatastorage.googleapis.com/perfetto-luci-artifacts/v54.0/mac-arm64/trace_processor_shell",
            sha256: "23638faac4ca695e86039a01fade05ff4a38ffa89672afc7a4e4077318603507",
        }),
        ("linux", "x86_64") => Some(PlatformArtifact {
            url: "https://commondatastorage.googleapis.com/perfetto-luci-artifacts/v54.0/linux-amd64/trace_processor_shell",
            sha256: "a7aa1f738bbe2926a70f0829d00837f5720be8cafe26de78f962094fa24a3da4",
        }),
        ("linux", "aarch64") => Some(PlatformArtifact {
            url: "https://commondatastorage.googleapis.com/perfetto-luci-artifacts/v54.0/linux-arm64/trace_processor_shell",
            sha256: "53af6216259df603115f1eefa94f034eef9c29cf851df15302ad29160334ca81",
        }),
        ("windows", "x86_64") => Some(PlatformArtifact {
            url: "https://commondatastorage.googleapis.com/perfetto-luci-artifacts/v54.0/windows-amd64/trace_processor_shell.exe",
            sha256: "7138e6f97c562fa063e1ceab1a0221c1c211328a304060aa8899363b07c7e2ab",
        }),
        _ => None,
    }
}

/// Progress report emitted while streaming the binary download.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub bytes_so_far: u64,
    pub total_bytes: Option<u64>,
}

/// Ensure `trace_processor_shell` exists and is executable. Returns the
/// absolute path to the installed binary.
///
/// - If the file already exists at the expected path, returns immediately.
///   (No re-hashing — we trust our own install directory. Users who suspect
///   corruption can delete the file and call this again.)
/// - Otherwise downloads the pinned artifact for the host platform, verifies
///   its SHA-256 against the vendored constant, `chmod 0o755`'s it (Unix),
///   and renames it into place atomically.
/// - Emits `DownloadProgress` events between chunks if `progress` is `Some`.
/// - Aborts cleanly on cancellation, leaving only the `.partial` file behind
///   (which will be overwritten on the next attempt).
pub async fn ensure_binary(
    paths: &Paths,
    progress: Option<&UnboundedSender<DownloadProgress>>,
    cancel: Arc<Cancel>,
) -> Result<PathBuf> {
    let dest = paths.trace_processor_binary();

    if is_executable(&dest) {
        return Ok(dest);
    }

    let artifact = host_artifact().ok_or_else(|| {
        anyhow!(
            "no prebuilt trace_processor_shell for this platform ({}/{})",
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    })?;

    std::fs::create_dir_all(paths.bin_dir())
        .with_context(|| format!("failed to create {}", paths.bin_dir().display()))?;

    let partial = paths.bin_dir().join(format!(
        "{}.partial",
        dest.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("trace_processor_shell")
    ));

    download_with_verify(artifact, &partial, progress, cancel.clone()).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&partial)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&partial, perms)
            .with_context(|| format!("chmod 755 {}", partial.display()))?;
    }

    std::fs::rename(&partial, &dest).with_context(|| {
        format!(
            "rename {} -> {}",
            partial.display(),
            dest.display()
        )
    })?;

    Ok(dest)
}

/// Run the installed binary with `--version` to read the human-readable
/// version string back. Used by callers that want to persist the value (e.g.
/// into the `settings` table).
pub async fn detect_version(binary: &Path) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(binary)
            .arg("--version")
            .output(),
    )
    .await
    .context("trace_processor_shell --version timed out")?
    .with_context(|| format!("failed to spawn {} --version", binary.display()))?;

    if !output.status.success() {
        bail!(
            "{} --version exited {}: {}",
            binary.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(m) => m.is_file() && (m.permissions().mode() & 0o111) != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
    }
}

async fn download_with_verify(
    artifact: PlatformArtifact,
    dest: &Path,
    progress: Option<&UnboundedSender<DownloadProgress>>,
    cancel: Arc<Cancel>,
) -> Result<()> {
    tracing::info!(url = artifact.url, "downloading trace_processor_shell");

    let client = reqwest::Client::builder()
        .user_agent(format!(
            "perfetto-cli/{} ({}; {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
        ))
        .build()
        .context("building reqwest client")?;

    // `tokio::select!` on `cancel.wait()` cancels the whole send operation if
    // the user bails before the server responds.
    let resp = tokio::select! {
        _ = cancel.wait() => bail!("download cancelled"),
        r = client.get(artifact.url).send() => r.context("sending GET")?,
    };
    let resp = resp.error_for_status().context("non-success from GCS")?;
    let total_bytes = resp.content_length();

    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("create {}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut bytes_so_far: u64 = 0;
    let mut stream = resp.bytes_stream();

    loop {
        let next = tokio::select! {
            _ = cancel.wait() => bail!("download cancelled"),
            n = stream.next() => n,
        };
        let chunk = match next {
            Some(res) => res.context("reading response body")?,
            None => break,
        };
        hasher.update(&chunk);
        file.write_all(&chunk).await.context("writing chunk")?;
        bytes_so_far += chunk.len() as u64;
        if let Some(tx) = progress {
            let _ = tx.send(DownloadProgress {
                bytes_so_far,
                total_bytes,
            });
        }
    }

    file.flush().await.ok();
    drop(file);

    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(artifact.sha256) {
        // Leave `.partial` on disk so a developer can inspect / diff it.
        bail!(
            "SHA-256 mismatch for {}: expected {}, got {}",
            artifact.url,
            artifact.sha256,
            actual
        );
    }

    Ok(())
}

// Small local hex encoder to avoid pulling in a dedicated crate for ~12 bytes
// of formatting.
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        let bytes = bytes.as_ref();
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(nibble(b >> 4));
            out.push(nibble(b & 0x0f));
        }
        out
    }

    fn nibble(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            10..=15 => (b'a' + n - 10) as char,
            _ => unreachable!(),
        }
    }
}
