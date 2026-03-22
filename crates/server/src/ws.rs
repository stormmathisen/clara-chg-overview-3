use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use shared::messages::{
    ChartSnapshot, ClientMessage, DeviceStatus, Notification, NotificationLevel, ServerMessage,
};
use tokio::sync::broadcast;
use tracing::{info, warn};

use std::sync::Arc;

use crate::audit::AuditLog;
use crate::commands;
use crate::state::{AppState, InnerState};

const BROADCAST_CHANNEL_CAPACITY: usize = 2048;
const BROADCAST_INTERVAL_MS: u64 = 100;
const MAX_COMMANDS_PER_SEC: usize = 10;

/// Broadcaster for server messages to all connected clients
pub type Broadcaster = broadcast::Sender<String>;

/// Create a new broadcaster
pub fn new_broadcaster() -> Broadcaster {
    let (tx, _) = broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
    tx
}

/// Build chart data snapshots from current state
pub fn build_chart_data(state: &InnerState) -> ServerMessage {
    let snapshots: Vec<ChartSnapshot> = state
        .devices
        .iter()
        .map(|d| ChartSnapshot {
            device_name: d.name.clone(),
            points: d.buffer.as_points(),
            stats: d.buffer.statistics(),
        })
        .collect();
    ServerMessage::ChartData { snapshots }
}

/// Build full init message from current state
pub fn build_init_message(state: &InnerState) -> ServerMessage {
    let devices: Vec<DeviceStatus> = state
        .devices
        .iter()
        .map(|d| DeviceStatus {
            name: d.name.clone(),
            device_type: d.config.device_type.clone(),
            current_sensitivity: d.current_sensitivity,
            sensitivities: d.config.sensitivities.clone(),
            stats: d.buffer.statistics(),
            connected: d.connected,
            last_data_time: d.last_data_time,
        })
        .collect();
    ServerMessage::Init {
        devices,
        buffer_size: state.buffer_size,
        device_order: state.device_order.clone(),
    }
}

/// Spawn the periodic chart data broadcast task (10 Hz)
pub fn spawn_chart_broadcaster(state: AppState, broadcaster: Broadcaster) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(BROADCAST_INTERVAL_MS));
        loop {
            interval.tick().await;
            let state_read = state.read().await;
            let msg = build_chart_data(&state_read);
            drop(state_read);
            if let Ok(json) = serde_json::to_string(&msg) {
                // Ignore send errors — means no subscribers
                let _ = broadcaster.send(json);
            }
        }
    });
}

/// Handle a single WebSocket connection
pub async fn handle_ws(
    socket: WebSocket,
    state: AppState,
    broadcaster: Broadcaster,
    audit: Arc<AuditLog>,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    audit.log_connect("ws-client");

    // Send init message
    {
        let state_read = state.read().await;
        let init_msg = build_init_message(&state_read);
        if let Ok(json) = serde_json::to_string(&init_msg) {
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                return;
            }
        }
    }

    // Subscribe to broadcast channel
    let mut broadcast_rx = broadcaster.subscribe();

    // Spawn a task to forward broadcast messages to this client
    let mut send_task = tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(msg) => {
                    if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Client lagged behind by {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Process incoming messages from client
    let state_clone = state.clone();
    let broadcaster_clone = broadcaster.clone();
    let audit_clone = audit.clone();
    let mut recv_task = tokio::spawn(async move {
        let mut command_times: std::collections::VecDeque<std::time::Instant> =
            std::collections::VecDeque::new();
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(text) = msg {
                // Rate limit: max 10 commands per second
                let now = std::time::Instant::now();
                command_times
                    .retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(1));
                if command_times.len() >= MAX_COMMANDS_PER_SEC {
                    warn!("Client rate limited — dropping command");
                    continue;
                }
                command_times.push_back(now);

                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(client_msg) => {
                        commands::handle_command(
                            client_msg,
                            &state_clone,
                            &broadcaster_clone,
                            &audit_clone,
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!("Invalid client message: {e}");
                    }
                }
            }
        }
    });

    // Wait for either task to finish
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    audit.log_disconnect("ws-client");
    info!("WebSocket client disconnected");
}

/// Send a notification to all clients via the broadcaster
pub fn broadcast_notification(
    broadcaster: &Broadcaster,
    level: NotificationLevel,
    message: String,
    device: Option<String>,
) {
    let notification = Notification {
        level,
        message,
        device,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64(),
    };
    let msg = ServerMessage::Notify(notification);
    if let Ok(json) = serde_json::to_string(&msg) {
        let _ = broadcaster.send(json);
    }
}
