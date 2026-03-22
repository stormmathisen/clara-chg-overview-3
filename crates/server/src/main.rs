mod audit;
mod commands;
mod config;
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
use state::{AppState, DeviceState, InnerState, PersistedState, RollingBuffer};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::services::ServeDir;
use tracing::info;

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

    let persisted = PersistedState::load(&state_path);
    let buffer_size = persisted.buffer_size;

    let mut devices: Vec<DeviceState> = device_configs
        .into_iter()
        .map(|(name, cfg)| {
            let sensitivity = persisted.sensitivities.get(&name).copied().unwrap_or(0);
            DeviceState {
                config: cfg,
                name,
                buffer: RollingBuffer::new(buffer_size),
                current_sensitivity: sensitivity,
                connected: false,
            }
        })
        .collect();
    devices.sort_by(|a, b| a.name.cmp(&b.name));

    let app_state: AppState = Arc::new(RwLock::new(InnerState {
        devices,
        buffer_size,
    }));

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
            }
        }
    });

    let broadcaster = ws::new_broadcaster();
    ws::spawn_chart_broadcaster(app_state.clone(), broadcaster.clone());

    // Periodic state persistence (every 30s)
    let state_for_persist = app_state.clone();
    let state_path_clone = state_path.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let s = state_for_persist.read().await;
            let p = PersistedState {
                buffer_size: s.buffer_size,
                sensitivities: s.devices.iter().map(|d| (d.name.clone(), d.current_sensitivity)).collect(),
            };
            drop(s);
            p.save(&state_path_clone);
        }
    });

    let audit_path = PathBuf::from(
        std::env::var("AUDIT_LOG").unwrap_or_else(|_| "audit.log".into()),
    );
    let audit = Arc::new(audit::AuditLog::open(&audit_path)?);

    let server_state = ServerState {
        app: app_state,
        broadcaster,
        audit,
    };

    let frontend_dir =
        std::env::var("FRONTEND_DIR").unwrap_or_else(|_| "frontend_dist".into());

    let router = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new(&frontend_dir).append_index_html_on_directories(true))
        .with_state(server_state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(49195);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!("Listening on http://0.0.0.0:{port}");

    axum::serve(listener, router).await?;
    Ok(())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws::handle_ws(socket, state.app, state.broadcaster, state.audit))
}
