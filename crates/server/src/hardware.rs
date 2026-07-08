use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tracing::info;

/// Front-end hardware settings matching the JSON protocol.
/// Sent over TCP to device front-ends on port 56000.
#[derive(Clone, Debug, Serialize)]
pub struct FrontEndSettings {
    pub calibration: Calibration,
    pub io: InputOutput,
    pub integrator: String,
    pub power: Power,
    pub meta: Meta,
}

#[derive(Clone, Debug, Serialize)]
pub struct Calibration {
    pub reference: String,
    pub level: u16,
    pub trigger: u16,
    pub offset: u16,
}

#[derive(Clone, Debug, Serialize)]
pub struct InputOutput {
    pub input: String,
    pub output: String,
    pub reference: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Power {
    pub positive: bool,
    pub negative: bool,
    pub integrator: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Meta {
    pub last_changed: [u64; 2],
    pub device_name: String,
    pub device_location: String,
}

impl Default for FrontEndSettings {
    fn default() -> Self {
        Self {
            calibration: Calibration {
                reference: "REF2048mV".to_string(),
                level: 128,
                trigger: 1,
                offset: 1,
            },
            io: InputOutput {
                input: "EXT".to_string(),
                output: "TERM".to_string(),
                reference: "REF500mV".to_string(),
            },
            integrator: "FB0".to_string(),
            power: Power {
                positive: true,
                negative: true,
                integrator: true,
            },
            meta: Meta {
                last_changed: [0, 0],
                device_name: String::new(),
                device_location: String::new(),
            },
        }
    }
}

/// Build settings for a given sensitivity level (0–5 maps to FB0–FB5).
/// Only the integrator differs from the default (normal-operation) settings.
pub fn settings_for_sensitivity(level: u8) -> FrontEndSettings {
    FrontEndSettings {
        integrator: format!("FB{level}"),
        ..Default::default()
    }
}

/// Build settings for clearing (exiting) calibration mode at a given sensitivity.
///
/// The front-end box's calibration state is controlled by `io.input`:
/// `"CAL"` = calibration mode on (internal reference), `"EXT"` = off (external beam
/// signal, i.e. normal operation). Clearing calibration therefore just forces `EXT`.
/// This matches the reference implementation, which only ever exits calibration mode.
/// Note `settings_for_sensitivity` already defaults `io.input` to `EXT`, so for a
/// device not currently in calibration mode this is equivalent to a normal push.
pub fn settings_for_clear_calibration(level: u8) -> FrontEndSettings {
    let mut settings = settings_for_sensitivity(level);
    settings.io.input = "EXT".to_string();
    settings
}

/// Send settings to a device front-end via TCP
pub async fn send_settings(ip: &str, settings: &FrontEndSettings) -> anyhow::Result<()> {
    if ip.is_empty() {
        anyhow::bail!("No IP address configured for device");
    }

    let addr = format!("{ip}:{}", crate::consts::FRONT_END_PORT);
    info!("Connecting to front-end at {addr}");

    let mut stream = tokio::time::timeout(
        crate::consts::FRONT_END_CONNECT_TIMEOUT,
        TcpStream::connect(&addr),
    )
    .await??;

    let json = serde_json::to_string(settings)?;
    stream.write_all(json.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;

    info!("Settings sent to {addr}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_for_sensitivity_only_changes_integrator() {
        let s = settings_for_sensitivity(4);
        assert_eq!(s.integrator, "FB4");
        // Everything else matches normal-operation defaults.
        assert_eq!(s.io.input, "EXT");
        assert_eq!(s.calibration.reference, "REF2048mV");
    }

    #[test]
    fn clear_calibration_forces_external_input() {
        let s = settings_for_clear_calibration(3);
        assert_eq!(s.integrator, "FB3");
        assert_eq!(s.io.input, "EXT");
    }

    #[test]
    fn settings_serialize_to_expected_json_shape() {
        let json = serde_json::to_value(settings_for_sensitivity(0)).unwrap();
        assert_eq!(json["integrator"], "FB0");
        assert_eq!(json["io"]["input"], "EXT");
        assert_eq!(json["calibration"]["level"], 128);
        assert_eq!(json["power"]["integrator"], true);
    }
}
