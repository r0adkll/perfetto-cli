pub mod device;

use std::collections::HashMap;

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

// ---------------------------------------------------------------------------
// Device detail info
// ---------------------------------------------------------------------------

/// Detailed hardware / software info fetched from a connected device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub serial: String,
    pub device_name: Option<String>,
    pub manufacturer: Option<String>,
    pub android_version: Option<String>,
    pub sdk_version: Option<String>,
    pub cpu_abi: Option<String>,
    pub ram_bytes: Option<u64>,
    pub storage_total_bytes: Option<u64>,
    pub storage_used_bytes: Option<u64>,
    pub perfetto_version: Option<String>,
    pub packages: Vec<String>,
}

impl DeviceInfo {
    /// e.g. "14 (API 34, Upside Down Cake)"
    pub fn android_display(&self) -> String {
        let ver = self.android_version.as_deref().unwrap_or("?");
        let sdk = self.sdk_version.as_deref().unwrap_or("?");
        match self.sdk_version.as_deref().and_then(android_codename) {
            Some(name) => format!("{ver} (API {sdk}, {name})"),
            None => format!("{ver} (API {sdk})"),
        }
    }

    pub fn ram_display(&self) -> Option<String> {
        self.ram_bytes.map(format_bytes)
    }

    pub fn storage_display(&self) -> Option<String> {
        self.storage_total_bytes.map(|total| {
            let total_str = format_bytes(total);
            match self.storage_used_bytes {
                Some(used) => format!("{total_str} ({} used)", format_bytes(used)),
                None => total_str,
            }
        })
    }
}

/// Fetch detailed info from an online device via multiple adb shell commands.
pub async fn query_device_info(serial: &str) -> Result<DeviceInfo> {
    // Batch all system properties in one call.
    let props_raw = run(serial, &["shell", "getprop"]).await.unwrap_or_default();
    let props = parse_getprop(&props_raw);

    let device_name = props.get("ro.product.model").cloned();
    let manufacturer = props.get("ro.product.manufacturer").cloned();
    let android_version = props.get("ro.build.version.release").cloned();
    let sdk_version = props.get("ro.build.version.sdk").cloned();
    let cpu_abi = props.get("ro.product.cpu.abi").cloned();

    let ram_bytes = run(serial, &["shell", "cat", "/proc/meminfo"])
        .await
        .ok()
        .and_then(|s| parse_memtotal(&s));

    let (storage_total_bytes, storage_used_bytes) = run(serial, &["shell", "df", "/data"])
        .await
        .ok()
        .map(|s| parse_df(&s))
        .unwrap_or((None, None));

    let perfetto_version = run(serial, &["shell", "perfetto", "--version"])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let packages = list_installed_packages(serial).await.unwrap_or_default();

    Ok(DeviceInfo {
        serial: serial.to_string(),
        device_name,
        manufacturer,
        android_version,
        sdk_version,
        cpu_abi,
        ram_bytes,
        storage_total_bytes,
        storage_used_bytes,
        perfetto_version,
        packages,
    })
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_getprop(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('[') {
            if let Some((key, rest)) = rest.split_once("]: [") {
                if let Some(value) = rest.strip_suffix(']') {
                    if !value.is_empty() {
                        map.insert(key.to_string(), value.to_string());
                    }
                }
            }
        }
    }
    map
}

fn parse_memtotal(raw: &str) -> Option<u64> {
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb = rest.trim().split_whitespace().next()?.parse::<u64>().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

fn parse_df(raw: &str) -> (Option<u64>, Option<u64>) {
    for line in raw.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 {
            let total = parts[1].parse::<u64>().ok().map(|kb| kb * 1024);
            let used = parts[2].parse::<u64>().ok().map(|kb| kb * 1024);
            return (total, used);
        }
    }
    (None, None)
}

fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    }
}

fn android_codename(sdk: &str) -> Option<&'static str> {
    match sdk {
        "36" => Some("Baklava"),
        "35" => Some("Vanilla Ice Cream"),
        "34" => Some("Upside Down Cake"),
        "33" => Some("Tiramisu"),
        "32" | "31" => Some("Snow Cone"),
        "30" => Some("Red Velvet Cake"),
        "29" => Some("Quince Tart"),
        "28" => Some("Pie"),
        "27" | "26" => Some("Oreo"),
        "25" | "24" => Some("Nougat"),
        "23" => Some("Marshmallow"),
        "22" | "21" => Some("Lollipop"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_getprop() {
        let raw = "\
[ro.product.model]: [Pixel 7 Pro]
[ro.product.manufacturer]: [Google]
[ro.build.version.release]: [14]
[ro.build.version.sdk]: [34]
[ro.product.cpu.abi]: [arm64-v8a]
[persist.sys.timezone]: []
";
        let props = parse_getprop(raw);
        assert_eq!(props.get("ro.product.model").unwrap(), "Pixel 7 Pro");
        assert_eq!(props.get("ro.product.manufacturer").unwrap(), "Google");
        assert_eq!(props.get("ro.build.version.sdk").unwrap(), "34");
        // empty values are skipped
        assert!(!props.contains_key("persist.sys.timezone"));
    }

    #[test]
    fn parses_memtotal() {
        let raw = "MemTotal:        8005632 kB\nMemFree:          123456 kB\n";
        assert_eq!(parse_memtotal(raw), Some(8005632 * 1024));
    }

    #[test]
    fn parses_memtotal_missing() {
        assert_eq!(parse_memtotal("nothing here"), None);
    }

    #[test]
    fn parses_df() {
        let raw = "Filesystem     1K-blocks     Used Available Use% Mounted on\n\
                   /dev/block/dm-9 119267328 52436480  66830848  44% /data\n";
        let (total, used) = parse_df(raw);
        assert_eq!(total, Some(119267328 * 1024));
        assert_eq!(used, Some(52436480 * 1024));
    }

    #[test]
    fn parses_df_empty() {
        assert_eq!(parse_df(""), (None, None));
    }

    #[test]
    fn format_bytes_gb() {
        assert_eq!(format_bytes(8 * 1024 * 1024 * 1024), "8.0 GB");
    }

    #[test]
    fn format_bytes_mb() {
        assert_eq!(format_bytes(512 * 1024 * 1024), "512 MB");
    }

    #[test]
    fn codename_lookup() {
        assert_eq!(android_codename("34"), Some("Upside Down Cake"));
        assert_eq!(android_codename("31"), Some("Snow Cone"));
        assert_eq!(android_codename("19"), None);
    }

    #[test]
    fn device_info_android_display() {
        let info = DeviceInfo {
            serial: String::new(),
            device_name: None,
            manufacturer: None,
            android_version: Some("14".into()),
            sdk_version: Some("34".into()),
            cpu_abi: None,
            ram_bytes: None,
            storage_total_bytes: None,
            storage_used_bytes: None,
            perfetto_version: None,
            packages: vec![],
        };
        assert_eq!(info.android_display(), "14 (API 34, Upside Down Cake)");
    }

    #[test]
    fn device_info_storage_display() {
        let info = DeviceInfo {
            serial: String::new(),
            device_name: None,
            manufacturer: None,
            android_version: None,
            sdk_version: None,
            cpu_abi: None,
            ram_bytes: None,
            storage_total_bytes: Some(119267328 * 1024),
            storage_used_bytes: Some(52436480 * 1024),
            perfetto_version: None,
            packages: vec![],
        };
        assert_eq!(info.storage_display().unwrap(), "113.7 GB (50.0 GB used)");
    }
}
