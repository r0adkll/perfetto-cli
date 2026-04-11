#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: DeviceState,
    pub model: Option<String>,
    pub product: Option<String>,
    pub transport_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    Online,
    Offline,
    Unauthorized,
    Other(String),
}

impl DeviceState {
    #[allow(dead_code)]
    pub fn label(&self) -> &str {
        match self {
            DeviceState::Online => "online",
            DeviceState::Offline => "offline",
            DeviceState::Unauthorized => "unauthorized",
            DeviceState::Other(s) => s.as_str(),
        }
    }
}

pub fn parse_devices(input: &str) -> Vec<Device> {
    input
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !l.starts_with("List of devices"))
        .filter(|l| !l.starts_with('*'))
        .filter_map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Option<Device> {
    let line = line.trim();
    let mut parts = line.split_whitespace();
    let serial = parts.next()?.to_string();
    let state = match parts.next()? {
        "device" => DeviceState::Online,
        "offline" => DeviceState::Offline,
        "unauthorized" => DeviceState::Unauthorized,
        other => DeviceState::Other(other.to_string()),
    };

    let mut model = None;
    let mut product = None;
    let mut transport_id = None;
    for kv in parts {
        if let Some((k, v)) = kv.split_once(':') {
            match k {
                "model" => model = Some(v.to_string()),
                "product" => product = Some(v.to_string()),
                "transport_id" => transport_id = Some(v.to_string()),
                _ => {}
            }
        }
    }

    Some(Device {
        serial,
        state,
        model,
        product,
        transport_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_and_emulator() {
        let raw = "List of devices attached\n\
                   abcdef1234      device usb:1-2 product:coral model:Pixel_4 device:coral transport_id:1\n\
                   emulator-5554   device product:sdk_gphone model:sdk_gphone device:generic transport_id:2\n\
                   \n";
        let devices = parse_devices(raw);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "abcdef1234");
        assert_eq!(devices[0].state, DeviceState::Online);
        assert_eq!(devices[0].model.as_deref(), Some("Pixel_4"));
        assert_eq!(devices[0].transport_id.as_deref(), Some("1"));
        assert_eq!(devices[1].serial, "emulator-5554");
    }

    #[test]
    fn parses_states() {
        let raw = "List of devices attached\n\
                   unauth_serial  unauthorized\n\
                   offline_serial offline\n";
        let devices = parse_devices(raw);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].state, DeviceState::Unauthorized);
        assert_eq!(devices[1].state, DeviceState::Offline);
    }

    #[test]
    fn skips_daemon_startup_noise() {
        let raw = "* daemon not running; starting now at tcp:5037\n\
                   * daemon started successfully\n\
                   List of devices attached\n\
                   abcdef device product:coral model:Pixel_4 device:coral transport_id:1\n";
        let devices = parse_devices(raw);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].serial, "abcdef");
    }

    #[test]
    fn empty_when_no_devices() {
        let raw = "List of devices attached\n\n";
        assert!(parse_devices(raw).is_empty());
    }
}
