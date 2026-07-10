mod audit;
mod commands;
mod config;
mod consts;
mod epics;
mod hardware;
mod state;
mod ws;

use axum::{
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use consts::{
    DEFAULT_PORT, FRONT_END_CONNECT_TIMEOUT, FRONT_END_PORT, MAX_WS_MESSAGE_SIZE, PERSIST_INTERVAL,
    PING_INTERVAL, WATCHDOG_INTERVAL, WATCHDOG_STALE_SECS,
};
use state::{AppState, DeviceState, InnerState, PersistedState, RollingBuffer};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::services::ServeDir;
use tracing::{info, warn};

#[derive(Clone)]
struct ServerState {
    app: AppState,
    broadcaster: ws::Broadcaster,
    audit: Arc<audit::AuditLog>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "server=info,tower_http=info".into()),
        )
        .init();

    let config_path = PathBuf::from(
        std::env::var("CHARGE_CONFIG").unwrap_or_else(|_| "config/charge_devices.yaml".into()),
    );
    let network_path = PathBuf::from(
        std::env::var("NETWORK_CONFIG").unwrap_or_else(|_| "config/network.yaml".into()),
    );
    let state_path = PathBuf::from("state.json");
    let virtual_mode = std::env::var("VIRTUAL").map(|v| v == "1").unwrap_or(false);

    let device_configs = config::load_device_configs(&config_path)?;
    let network_config = config::load_network_config(&network_path)?;
    info!("Loaded {} device configs", device_configs.len());

    config::apply_epics_env(&network_config, virtual_mode);

    // PV writes shell out to the external `caput` binary (see epics::caput), so surface a
    // missing EPICS base at startup rather than at the first write. The Docker image builds
    // EPICS from source; a local checkout needs `caput` on PATH.
    match tokio::process::Command::new("caput")
        .arg("-h")
        .output()
        .await
    {
        Ok(output) if output.status.success() => info!("caput found on PATH"),
        _ => warn!("caput not found on PATH — EPICS PV writes will fail (logged, non-fatal)"),
    }

    let persisted = PersistedState::load(&state_path);
    let buffer_size = persisted.buffer_size;

    let mut devices: Vec<DeviceState> = device_configs
        .into_iter()
        .map(|(name, cfg)| {
            let max_index = cfg.sensitivities.len().saturating_sub(1);
            let sensitivity = persisted
                .sensitivities
                .get(&name)
                .copied()
                .unwrap_or(max_index);
            DeviceState {
                config: cfg,
                name,
                buffer: RollingBuffer::new(buffer_size),
                current_sensitivity: sensitivity,
                connected: false,
                fe_alive: false,
                last_data_time: 0.0,
            }
        })
        .collect();
    devices.sort_by(|a, b| a.name.cmp(&b.name));

    let device_order = reconcile_device_order(&persisted.device_order, &devices);

    let app_state: AppState = Arc::new(RwLock::new(InnerState::new(
        devices,
        buffer_size,
        device_order,
    )));

    // Start EPICS subscriptions
    let (_epics, mut epics_rx) = epics::EpicsManager::start(&app_state).await?;

    // Drain EPICS updates into state
    let state_for_epics = app_state.clone();
    tokio::spawn(async move {
        while let Some(update) = epics_rx.recv().await {
            let mut s = state_for_epics.write().await;
            if let Some(device) = s.devices.get_mut(update.device_index) {
                device.buffer.push(update.timestamp, update.value);
                device.connected = true;
                device.last_data_time = update.timestamp;
            }
        }
    });

    let broadcaster = ws::new_broadcaster();
    ws::spawn_chart_broadcaster(app_state.clone(), broadcaster.clone());

    // Watchdog: mark devices with no data for 60s as disconnected
    let state_for_watchdog = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(WATCHDOG_INTERVAL);
        loop {
            interval.tick().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let mut s = state_for_watchdog.write().await;
            for device in &mut s.devices {
                if device.connected
                    && device.last_data_time > 0.0
                    && (now - device.last_data_time) > WATCHDOG_STALE_SECS
                {
                    warn!("[{}] No data for 60s, marking disconnected", device.name);
                    device.connected = false;
                }
            }
        }
    });

    // Periodic front-end ping: TCP connect to each device front-end box.
    let state_for_ping = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PING_INTERVAL);
        loop {
            interval.tick().await;
            // Collect (index, ip) pairs while holding the lock briefly
            let targets: Vec<(usize, String)> = {
                let s = state_for_ping.read().await;
                s.devices
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| !d.config.ip.is_empty())
                    .map(|(i, d)| (i, d.config.ip.clone()))
                    .collect()
            };
            // Ping all devices concurrently
            let mut handles = Vec::new();
            for (idx, ip) in targets {
                handles.push(tokio::spawn(async move {
                    let addr = format!("{ip}:{FRONT_END_PORT}");
                    let alive = tokio::time::timeout(
                        FRONT_END_CONNECT_TIMEOUT,
                        tokio::net::TcpStream::connect(&addr),
                    )
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);
                    (idx, alive)
                }));
            }
            let mut results = Vec::new();
            for h in handles {
                if let Ok(r) = h.await {
                    results.push(r);
                }
            }
            // Update state with results
            let mut s = state_for_ping.write().await;
            for (idx, alive) in results {
                if let Some(device) = s.devices.get_mut(idx) {
                    device.fe_alive = alive;
                }
            }
        }
    });

    // Periodic state persistence (every 30s)
    let state_for_persist = app_state.clone();
    let state_path_clone = state_path.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PERSIST_INTERVAL);
        loop {
            interval.tick().await;
            let s = state_for_persist.read().await;
            let p = PersistedState {
                buffer_size: s.buffer_size,
                sensitivities: s
                    .devices
                    .iter()
                    .map(|d| (d.name.clone(), d.current_sensitivity))
                    .collect(),
                device_order: s.device_order.clone(),
            };
            drop(s);
            p.save(&state_path_clone);
        }
    });

    let audit_path =
        PathBuf::from(std::env::var("AUDIT_LOG").unwrap_or_else(|_| "audit.log".into()));
    let audit = Arc::new(audit::AuditLog::open(&audit_path)?);

    let server_state = ServerState {
        app: app_state,
        broadcaster,
        audit,
    };

    let frontend_dir = std::env::var("FRONTEND_DIR").unwrap_or_else(|_| "frontend_dist".into());

    let router = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new(&frontend_dir).append_index_html_on_directories(true))
        .with_state(server_state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!("Listening on http://0.0.0.0:{port}");

    axum::serve(listener, router).await?;
    Ok(())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<ServerState>) -> impl IntoResponse {
    ws.max_message_size(MAX_WS_MESSAGE_SIZE)
        .on_upgrade(move |socket| ws::handle_ws(socket, state.app, state.broadcaster, state.audit))
}

/// Reconcile the persisted display order against the devices actually configured.
///
/// The UI only renders devices present in `device_order`, so a `state.json` written
/// before a device was added to the config would silently hide it forever. Keep the
/// persisted relative order for devices that still exist, drop names that no longer
/// do, and append any newly-configured devices (already name-sorted) at the end.
fn reconcile_device_order(persisted: &[String], devices: &[DeviceState]) -> Vec<String> {
    let known: std::collections::HashSet<&str> = devices.iter().map(|d| d.name.as_str()).collect();
    let mut order: Vec<String> = persisted
        .iter()
        .filter(|name| known.contains(name.as_str()))
        .cloned()
        .collect();
    let listed: std::collections::HashSet<&str> = order.iter().map(|s| s.as_str()).collect();
    let missing: Vec<String> = devices
        .iter()
        .filter(|d| !listed.contains(d.name.as_str()))
        .map(|d| d.name.clone())
        .collect();
    if !missing.is_empty() {
        info!(
            "Adding {} newly-configured device(s) to display order",
            missing.len()
        );
    }
    order.extend(missing);
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::config::DeviceConfig;
    use shared::messages::DeviceType;

    fn device(name: &str) -> DeviceState {
        DeviceState {
            config: DeviceConfig {
                device_type: DeviceType::Ict,
                digitizer: String::new(),
                ip: String::new(),
                sensitivities: Vec::new(),
                pvs: std::collections::HashMap::new(),
                defaults: std::collections::HashMap::new(),
            },
            name: name.to_string(),
            buffer: RollingBuffer::new(10),
            current_sensitivity: 0,
            connected: false,
            fe_alive: false,
            last_data_time: 0.0,
        }
    }

    #[test]
    fn empty_persisted_order_uses_all_devices() {
        let devices = vec![device("a"), device("b")];
        assert_eq!(reconcile_device_order(&[], &devices), vec!["a", "b"]);
    }

    #[test]
    fn newly_configured_devices_are_appended() {
        // `state.json` predates the ICT devices being added to the config.
        let devices = vec![device("a"), device("b"), device("ict-1")];
        let persisted = vec!["b".to_string(), "a".to_string()];
        // Persisted order preserved; the new device shows up rather than vanishing.
        assert_eq!(
            reconcile_device_order(&persisted, &devices),
            vec!["b", "a", "ict-1"]
        );
    }

    #[test]
    fn removed_devices_are_dropped() {
        let devices = vec![device("a")];
        let persisted = vec!["a".to_string(), "gone".to_string()];
        assert_eq!(reconcile_device_order(&persisted, &devices), vec!["a"]);
    }
}
