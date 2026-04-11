pub mod device;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

pub use device::{Device, DeviceState};

/// Run `adb devices -l` and parse the live device list.
pub async fn list_live_devices() -> Result<Vec<Device>> {
    let output = Command::new("adb")
        .args(["devices", "-l"])
        .output()
        .await
        .context("failed to spawn `adb` — is it on PATH?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("adb devices -l failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(device::parse_devices(&stdout))
}

/// List third-party (non-system) packages installed on the given device.
/// Used to populate the new-session wizard's package suggestions without
/// drowning the list in the hundreds of system packages.
pub async fn list_installed_packages(serial: &str) -> Result<Vec<String>> {
    let out = run(serial, &["shell", "pm", "list", "packages", "-3"]).await?;
    let mut pkgs: Vec<String> = out
        .lines()
        .filter_map(|l| l.trim().strip_prefix("package:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    pkgs.sort();
    pkgs.dedup();
    Ok(pkgs)
}

/// Run `adb -s <serial> <args...>` and return captured stdout. Errors out on
/// non-zero exit with the device's stderr attached.
pub async fn run(serial: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("adb")
        .arg("-s")
        .arg(serial)
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to spawn `adb {args:?}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("adb {:?} failed: {}", args, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
