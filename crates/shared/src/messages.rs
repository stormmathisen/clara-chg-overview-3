use serde::{Deserialize, Serialize};

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
}
