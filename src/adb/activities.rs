use anyhow::{Result, bail};

use super::{DeviceState, list_live_devices, run};

/// Read all declared activities, including those without intent filters.
pub async fn list_activities(serial: Option<&str>, package: &str) -> Result<Vec<String>> {
    if package.is_empty()
        || !package
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
    {
        bail!("Enter a valid package name before choosing an activity");
    }
    let serial = match serial {
        Some(serial) => serial.to_string(),
        None => {
            let devices: Vec<_> = list_live_devices()
                .await?
                .into_iter()
                .filter(|d| d.state == DeviceState::Online)
                .collect();
            match devices.as_slice() {
                [device] => device.serial.clone(),
                [] => bail!("No online device connected"),
                _ => bail!("Multiple devices connected; select a device for this session first"),
            }
        }
    };
    let output = run(
        &serial,
        &["shell", "dumpsys", "package", "--all-components", package],
    )
    .await?;
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("Unable to find package:")
            || line.starts_with("Unknown package:")
            || line.starts_with("Permission Denial:")
            || line.starts_with("Unknown argument:")
        {
            bail!("{line}");
        }
    }
    Ok(parse_activities(&output, package))
}

fn parse_activities(output: &str, package: &str) -> Vec<String> {
    let mut activities = Vec::new();
    let mut section_indent = None;
    for line in output.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        if trimmed == "activities:" {
            section_indent = Some(indent);
            continue;
        }
        let Some(section) = section_indent else {
            continue;
        };
        if trimmed.is_empty() {
            continue;
        }
        if indent <= section {
            section_indent = None;
            continue;
        }
        if let Some((owner, class)) = trimmed.split_once('/')
            && owner == package
            && !class.is_empty()
            && !class.chars().any(char::is_whitespace)
        {
            activities.push(trimmed.to_string());
        }
    }
    activities.sort();
    activities.dedup();
    activities
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_declared_activities_for_target_package() {
        let dump = r#"Activity Resolver Table:
        abcd com.example/.FilteredOnly filter 1234
Packages:
  Package [com.example] (abcd):
    activities:
      com.example/.Main
      com.example/com.library.InternalActivity
      com.example/.Main
      com.other/.Wrong
    services:
      com.example/.SyncService
    receivers:
      com.example/.Receiver
"#;
        assert_eq!(
            parse_activities(dump, "com.example"),
            vec![
                "com.example/.Main",
                "com.example/com.library.InternalActivity",
            ]
        );
        assert!(parse_activities(dump, "missing.package").is_empty());
    }

    #[test]
    fn empty_or_missing_activity_section_has_no_results() {
        assert!(parse_activities("", "com.example").is_empty());
        assert!(
            parse_activities(
                "    activities:\n    services:\n      com.example/.Service",
                "com.example"
            )
            .is_empty()
        );
    }

    #[tokio::test]
    #[ignore = "requires PERFETTO_ADB_SERIAL and PERFETTO_ADB_PACKAGE on a connected device"]
    async fn lists_activities_on_device() {
        let serial = std::env::var("PERFETTO_ADB_SERIAL").expect("set PERFETTO_ADB_SERIAL");
        let package = std::env::var("PERFETTO_ADB_PACKAGE").expect("set PERFETTO_ADB_PACKAGE");
        let activities = list_activities(Some(&serial), &package).await.unwrap();
        assert!(!activities.is_empty(), "choose a package with activities");
        assert!(
            activities
                .iter()
                .all(|activity| activity.starts_with(&format!("{package}/")))
        );
        eprintln!("Found {} activity components on {serial}", activities.len());
    }
}
