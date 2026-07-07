use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Statistics for a device's rolling buffer
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Stats {
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub rmsd: f64,
}

/// A snapshot of chart data for one device
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartSnapshot {
    pub device_name: String,
    pub points: Vec<[f64; 2]>, // [timestamp_secs, value]
    pub stats: Stats,
}

/// Status of a single device
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub name: String,
    pub device_type: DeviceType,
    pub current_sensitivity: usize,
    pub sensitivities: Vec<u8>,
    pub stats: Stats,
    pub connected: bool,
    #[serde(default)]
    pub fe_alive: bool,
    #[serde(default)]
    pub last_data_time: f64,
    #[serde(default)]
    pub defaults: HashMap<String, f64>,
}

/// Basic device info sent on init
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub device_type: DeviceType,
    pub sensitivities: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Wcm,
    Dq,
    Fcup,
    Ict,
}

/// A notification to display in the UI
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notification {
    pub level: NotificationLevel,
    pub message: String,
    pub device: Option<String>,
    pub timestamp: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Messages from server to client via WebSocket
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Sent on initial connection — full state
    Init {
        devices: Vec<DeviceStatus>,
        buffer_size: usize,
        device_order: Vec<String>,
    },
    /// Periodic chart data update (all devices)
    ChartData {
        snapshots: Vec<ChartSnapshot>,
    },
    /// A single state change broadcast to all clients
    StateUpdate {
        device: String,
        sensitivity: usize,
    },
    /// Buffer size changed
    BufferSizeChanged {
        size: usize,
    },
    /// Device order changed
    DeviceOrderChanged {
        order: Vec<String>,
    },
    /// Notification for the UI
    Notify(Notification),
}

/// Messages from client to server via WebSocket
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    SetSensitivity { device: String, index: usize },
    ZeroWCM { device: String },
    SweepTiming { device: String },
    RestoreDefaults { device: String },
    ClearCalibration,
    SetBufferSize { size: usize },
    SetDeviceOrder { order: Vec<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_message_init_roundtrip() {
        let msg = ServerMessage::Init {
            devices: vec![DeviceStatus {
                name: "TEST-DEV".to_string(),
                device_type: DeviceType::Wcm,
                current_sensitivity: 0,
                sensitivities: vec![3, 4],
                stats: Stats::default(),
                connected: true,
                fe_alive: true,
                last_data_time: 1234567890.0,
                defaults: HashMap::new(),
            }],
            buffer_size: 1000,
            device_order: vec!["TEST-DEV".to_string()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        if let ServerMessage::Init { devices, buffer_size, device_order } = decoded {
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0].name, "TEST-DEV");
            assert_eq!(devices[0].last_data_time, 1234567890.0);
            assert_eq!(buffer_size, 1000);
            assert_eq!(device_order, vec!["TEST-DEV"]);
        } else {
            panic!("Expected Init message");
        }
    }

    #[test]
    fn client_message_set_sensitivity_roundtrip() {
        let msg = ClientMessage::SetSensitivity {
            device: "DEV-1".to_string(),
            index: 2,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        if let ClientMessage::SetSensitivity { device, index } = decoded {
            assert_eq!(device, "DEV-1");
            assert_eq!(index, 2);
        } else {
            panic!("Expected SetSensitivity");
        }
    }

    #[test]
    fn device_type_equality() {
        assert_eq!(DeviceType::Wcm, DeviceType::Wcm);
        assert_ne!(DeviceType::Wcm, DeviceType::Dq);
        assert_ne!(DeviceType::Dq, DeviceType::Fcup);
        assert_ne!(DeviceType::Fcup, DeviceType::Ict);
        assert_eq!(DeviceType::Ict, DeviceType::Ict);
    }

    #[test]
    fn notification_serialization() {
        let notif = Notification {
            level: NotificationLevel::Error,
            message: "test error".to_string(),
            device: Some("DEV".to_string()),
            timestamp: 1000.0,
        };
        let json = serde_json::to_string(&notif).unwrap();
        let decoded: Notification = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.message, "test error");
        assert_eq!(decoded.timestamp, 1000.0);
    }

    #[test]
    fn last_data_time_defaults_to_zero() {
        // Test backward compatibility: JSON without last_data_time should default to 0
        let json = r#"{"name":"DEV","device_type":"wcm","current_sensitivity":0,"sensitivities":[3],"stats":{"mean":0.0,"min":0.0,"max":0.0,"rmsd":0.0},"connected":false}"#;
        let status: DeviceStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.last_data_time, 0.0);
    }
}
