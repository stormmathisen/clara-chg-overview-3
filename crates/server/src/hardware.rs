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

/// Build settings for a given sensitivity level (0–5 maps to FB0–FB5)
pub fn settings_for_sensitivity(level: u8) -> FrontEndSettings {
    let integrator = format!("FB{level}");
    FrontEndSettings {
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
        integrator,
        ..Default::default()
    }
}

/// Build settings for clearing calibration at a given sensitivity
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

    let addr = format!("{ip}:56000");
    info!("Connecting to front-end at {addr}");

    let mut stream = tokio::time::timeout(
        std::time::Duration::from_millis(500),
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
