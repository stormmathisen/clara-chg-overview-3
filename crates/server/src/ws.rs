use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use shared::messages::{
    ChartSnapshot, ClientMessage, DeviceDelta, DeviceStatus, Notification, NotificationLevel,
    ServerMessage,
};
use tokio::sync::broadcast;
use tracing::{info, warn};

use std::sync::Arc;

use crate::audit::AuditLog;
use crate::commands;
use crate::state::{AppState, InnerState};

use crate::consts::{BROADCAST_CHANNEL_CAPACITY, BROADCAST_INTERVAL, MAX_COMMANDS_PER_SEC};

/// Broadcaster for server messages to all connected clients. Payloads are `Arc<str>`
/// so the broadcast channel hands every client a shared reference instead of cloning
/// the JSON per subscriber.
pub type Broadcaster = broadcast::Sender<Arc<str>>;

/// Create a new broadcaster
pub fn new_broadcaster() -> Broadcaster {
    let (tx, _) = broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
    tx
}

/// Serialize a `ServerMessage` once and broadcast it to all connected clients.
/// Send errors (no subscribers) and the practically-impossible serialize error are
/// ignored — these message types always serialize.
pub fn send_message(broadcaster: &Broadcaster, msg: &ServerMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        let _ = broadcaster.send(Arc::from(json));
    }
}

/// Build a full chart snapshot (every device's whole buffer). Sent per-client on
/// connect and broadcast as a reset after a buffer clear/resize.
pub fn build_chart_snapshot(state: &InnerState) -> ServerMessage {
    let snapshots: Vec<ChartSnapshot> = state
        .devices
        .iter()
        .enumerate()
        .map(|(i, d)| ChartSnapshot {
            device: i,
            points: d.buffer.as_points(),
            stats: d.buffer.statistics(),
            cursor: d.buffer.total_pushed(),
        })
        .collect();
    ServerMessage::ChartData { snapshots }
}

/// Broadcast a full chart snapshot to all clients — used after a structural change
/// (buffer clear / resize) that a per-device append delta can't express.
pub async fn broadcast_chart_reset(state: &AppState, broadcaster: &Broadcaster) {
    let msg = {
        let s = state.read().await;
        build_chart_snapshot(&s)
    };
    send_message(broadcaster, &msg);
}

/// Build full init message from current state
pub fn build_init_message(state: &InnerState) -> ServerMessage {
    let devices: Vec<DeviceStatus> = state
        .devices
        .iter()
        .map(|d| {
            let defaults: std::collections::HashMap<String, f64> = d
                .config
                .defaults
                .iter()
                .map(|(k, v)| (k.clone(), v.for_sensitivity(d.current_sensitivity)))
                .collect();
            DeviceStatus {
                name: d.name.clone(),
                device_type: d.config.device_type.clone(),
                current_sensitivity: d.current_sensitivity,
                sensitivities: d.config.sensitivities.clone(),
                stats: d.buffer.statistics(),
                connected: d.connected,
                fe_alive: d.fe_alive,
                last_data_time: d.last_data_time,
                defaults,
            }
        })
        .collect();
    ServerMessage::Init {
        devices,
        buffer_size: state.buffer_size,
        device_order: state.device_order.clone(),
        reset_progress: state.reset_progress,
    }
}

/// Spawn the periodic chart broadcast task (10 Hz). It sends incremental
/// `ChartDelta`s — only the points pushed since the previous tick — instead of the
/// whole buffer, cutting steady-state bandwidth by roughly the buffer size.
pub fn spawn_chart_broadcaster(state: AppState, broadcaster: Broadcaster) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(BROADCAST_INTERVAL);
        // Per-device cursor (total_pushed) already reflected in what clients have.
        let mut prev_cursors: Vec<u64> = Vec::new();
        loop {
            interval.tick().await;

            // Building/serializing point payloads is only worth it with subscribers,
            // but we still advance the cursors either way so a late-joining client
            // doesn't trigger a giant catch-up delta (it gets a full snapshot instead).
            let have_receivers = broadcaster.receiver_count() > 0;

            let updates = {
                let s = state.read().await;
                if prev_cursors.len() != s.devices.len() {
                    prev_cursors = vec![0; s.devices.len()];
                }
                let mut updates: Vec<DeviceDelta> = Vec::new();
                for (i, d) in s.devices.iter().enumerate() {
                    let current = d.buffer.total_pushed();
                    if current == prev_cursors[i] {
                        continue;
                    }
                    if have_receivers {
                        let new_count = current.saturating_sub(prev_cursors[i]) as usize;
                        updates.push(DeviceDelta {
                            device: i,
                            new_points: d.buffer.last_points(new_count),
                            stats: d.buffer.statistics(),
                            cursor: current,
                        });
                    }
                    prev_cursors[i] = current;
                }
                updates
            };

            if have_receivers && !updates.is_empty() {
                send_message(&broadcaster, &ServerMessage::ChartDelta { updates });
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

    // Subscribe BEFORE snapshotting so no delta produced during the handshake is
    // lost: any delta covering points already in the snapshot is de-duplicated by
    // the client via its per-device cursor.
    let mut broadcast_rx = broadcaster.subscribe();

    // Send the metadata (Init) and a full chart snapshot up front, so the client has
    // the existing buffers before it starts applying deltas.
    {
        let state_read = state.read().await;
        let init_msg = build_init_message(&state_read);
        let snapshot_msg = build_chart_snapshot(&state_read);
        drop(state_read);
        for msg in [init_msg, snapshot_msg] {
            let Ok(json) = serde_json::to_string(&msg) else {
                return;
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                return;
            }
        }
    }

    // Spawn a task to forward broadcast messages to this client
    let mut send_task = tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(msg) => {
                    if ws_tx
                        .send(Message::Text(msg.as_ref().into()))
                        .await
                        .is_err()
                    {
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
    send_message(broadcaster, &ServerMessage::Notify(notification));
}
