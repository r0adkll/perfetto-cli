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
    pub cpu_cores: Option<u32>,
    pub cpu_max_freq_mhz: Option<u32>,
    pub ram_bytes: Option<u64>,
    pub perfetto_version: Option<String>,
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

    /// e.g. "arm64-v8a • 8 cores @ 2.84 GHz"
    pub fn cpu_display(&self) -> Option<String> {
        let abi = self.cpu_abi.as_deref()?;
        let mut parts = vec![abi.to_string()];
        if let Some(cores) = self.cpu_cores {
            parts.push(format!("{cores} cores"));
        }
        if let Some(freq) = self.cpu_max_freq_mhz {
            if freq >= 1000 {
                parts.push(format!("{:.2} GHz", freq as f64 / 1000.0));
            } else {
                parts.push(format!("{freq} MHz"));
            }
        }
        Some(parts.join(" • "))
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

    let cpu_cores = run(serial, &["shell", "nproc"])
        .await
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());

    // Read max frequency across all cores (highest policy max).
    let cpu_max_freq_mhz = run(
        serial,
        &["shell", "cat", "/sys/devices/system/cpu/cpu*/cpufreq/cpuinfo_max_freq"],
    )
    .await
    .ok()
    .and_then(|s| parse_max_cpu_freq(&s));

    let ram_bytes = run(serial, &["shell", "cat", "/proc/meminfo"])
        .await
        .ok()
        .and_then(|s| parse_memtotal(&s));

    let perfetto_version = run(serial, &["shell", "perfetto", "--version"])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Ok(DeviceInfo {
        serial: serial.to_string(),
        device_name,
        manufacturer,
        android_version,
        sdk_version,
        cpu_abi,
        cpu_cores,
        cpu_max_freq_mhz,
        ram_bytes,
        perfetto_version,
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

/// Parse the highest value from `cpuinfo_max_freq` output (one KHz value per
/// line, one per CPU core). Returns the max converted to MHz.
fn parse_max_cpu_freq(raw: &str) -> Option<u32> {
    raw.lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .max()
        .map(|khz| (khz / 1000) as u32)
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
            cpu_cores: None,
            cpu_max_freq_mhz: None,
            ram_bytes: None,
            perfetto_version: None,
        };
        assert_eq!(info.android_display(), "14 (API 34, Upside Down Cake)");
    }

    #[test]
    fn cpu_display_full() {
        let info = DeviceInfo {
            serial: String::new(),
            device_name: None,
            manufacturer: None,
            android_version: None,
            sdk_version: None,
            cpu_abi: Some("arm64-v8a".into()),
            cpu_cores: Some(8),
            cpu_max_freq_mhz: Some(2840),
            ram_bytes: None,
            perfetto_version: None,
        };
        assert_eq!(info.cpu_display().unwrap(), "arm64-v8a • 8 cores • 2.84 GHz");
    }

    #[test]
    fn cpu_display_abi_only() {
        let info = DeviceInfo {
            serial: String::new(),
            device_name: None,
            manufacturer: None,
            android_version: None,
            sdk_version: None,
            cpu_abi: Some("x86_64".into()),
            cpu_cores: None,
            cpu_max_freq_mhz: None,
            ram_bytes: None,
            perfetto_version: None,
        };
        assert_eq!(info.cpu_display().unwrap(), "x86_64");
    }

    #[test]
    fn parse_max_cpu_freq_picks_highest() {
        let raw = "1804800\n2841600\n2841600\n1804800\n";
        assert_eq!(parse_max_cpu_freq(raw), Some(2841));
    }
}
