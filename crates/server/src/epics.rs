use crate::state::AppState;
use epicars::client::Client;
use epicars::dbr::DbrValue;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Message from EPICS subscription task to the state manager
pub struct EpicsUpdate {
    pub device_index: usize,
    pub timestamp: f64,
    pub value: f64,
}

/// Manages EPICS CA client connections with persistent reconnection.
pub struct EpicsManager {
    _update_tx: mpsc::UnboundedSender<EpicsUpdate>,
}

impl EpicsManager {
    /// Start the EPICS manager. Spawns a persistent subscription task for each
    /// device's charge PV that automatically reconnects on failure.
    pub async fn start(
        state: &AppState,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<EpicsUpdate>)> {
        let (update_tx, update_rx) = mpsc::unbounded_channel();

        let state_read = state.read().await;
        let mut subscriptions: Vec<(usize, String)> = Vec::new();

        for (i, device) in state_read.devices.iter().enumerate() {
            if let Some(charge_pv) = device.config.pvs.get("charge") {
                subscriptions.push((i, charge_pv.clone()));
            }
        }
        drop(state_read);

        // Spawn one persistent task per PV — each manages its own client + reconnect
        for (device_index, pv_name) in subscriptions {
            let tx = update_tx.clone();
            let app_state = state.clone();
            tokio::spawn(persistent_monitor(device_index, pv_name, tx, app_state));
        }

        Ok((
            Self {
                _update_tx: update_tx,
            },
            update_rx,
        ))
    }
}

/// Long-lived task that maintains a subscription to a single PV.
/// On any failure (client creation, subscription, or monitor recv) it marks the
/// device as disconnected and retries with exponential backoff.
async fn persistent_monitor(
    device_index: usize,
    pv_name: String,
    tx: mpsc::UnboundedSender<EpicsUpdate>,
    state: AppState,
) {
    let mut retry_delay = INITIAL_RETRY_DELAY;
    // Stagger initial connection attempts to avoid thundering herd
    let jitter = Duration::from_millis((device_index as u64 * 200) % 1000);
    tokio::time::sleep(jitter).await;

    loop {
        info!("[{pv_name}] Connecting to EPICS...");

        match run_monitor(device_index, &pv_name, &tx).await {
            Ok(()) => {
                // Monitor ended cleanly (channel closed) — still retry
                warn!("[{pv_name}] Monitor ended, will reconnect");
            }
            Err(e) => {
                error!("[{pv_name}] Monitor failed: {e}");
            }
        }

        // Mark device as disconnected
        {
            let mut s = state.write().await;
            if let Some(device) = s.devices.get_mut(device_index) {
                device.connected = false;
            }
        }

        warn!("[{pv_name}] Retrying in {}s...", retry_delay.as_secs());
        tokio::time::sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
    }
}

/// Create a client, subscribe, and drain updates until the monitor ends or errors.
/// Returns Ok(()) when the monitor stream ends, Err on any failure.
async fn run_monitor(
    device_index: usize,
    pv_name: &str,
    tx: &mpsc::UnboundedSender<EpicsUpdate>,
) -> anyhow::Result<()> {
    let mut client = tokio::time::timeout(Duration::from_secs(5), Client::new())
        .await
        .map_err(|_| anyhow::anyhow!("timeout connecting to CA repeater"))??;
    let (mut monitor, _token) =
        tokio::time::timeout(Duration::from_secs(5), client.subscribe(pv_name))
            .await
            .map_err(|_| anyhow::anyhow!("timeout subscribing to {pv_name}"))??;
    info!("[{pv_name}] Subscribed successfully");

    loop {
        match monitor.recv().await {
            Ok(dbr) => {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                if let Some(value) = extract_f64(dbr.value()) {
                    if tx
                        .send(EpicsUpdate {
                            device_index,
                            timestamp,
                            value,
                        })
                        .is_err()
                    {
                        // Receiver dropped — app shutting down
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                anyhow::bail!("monitor recv error: {e}");
            }
        }
    }
}

/// Extract a single f64 from a DbrValue
fn extract_f64(value: &DbrValue) -> Option<f64> {
    match value {
        DbrValue::Double(v) => v.first().copied(),
        DbrValue::Float(v) => v.first().map(|f| *f as f64),
        DbrValue::Long(v) => v.first().map(|i| *i as f64),
        DbrValue::Int(v) => v.first().map(|i| *i as f64),
        DbrValue::Char(v) => v.first().map(|i| *i as f64),
        DbrValue::Enum(v) => Some(*v as f64),
        DbrValue::String(_) => None,
    }
}

/// Extract a full array of f64 from a DbrValue (for waveform PVs).
/// Scalar variants yield a single-element vec; strings yield None.
fn extract_f64_array(value: &DbrValue) -> Option<Vec<f64>> {
    match value {
        DbrValue::Double(v) => Some(v.clone()),
        DbrValue::Float(v) => Some(v.iter().map(|f| *f as f64).collect()),
        DbrValue::Long(v) => Some(v.iter().map(|i| *i as f64).collect()),
        DbrValue::Int(v) => Some(v.iter().map(|i| *i as f64).collect()),
        DbrValue::Char(v) => Some(v.iter().map(|i| *i as f64).collect()),
        DbrValue::Enum(v) => Some(vec![*v as f64]),
        DbrValue::String(_) => None,
    }
}

/// Subscribe to a waveform PV and collect `count` successive array updates.
/// Used by sweep timing to gather digitizer waveforms. The whole operation is
/// bounded by `timeout`; a partial or empty result is an error.
pub async fn collect_waveforms(
    pv_name: &str,
    count: usize,
    timeout: Duration,
) -> anyhow::Result<Vec<Vec<f64>>> {
    let collect = async {
        let mut client = Client::new().await?;
        let (mut monitor, _token) = client.subscribe(pv_name).await?;
        let mut waveforms: Vec<Vec<f64>> = Vec::with_capacity(count);
        while waveforms.len() < count {
            let dbr = monitor
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("monitor recv error for {pv_name}: {e}"))?;
            if let Some(arr) = extract_f64_array(dbr.value()) {
                waveforms.push(arr);
            }
        }
        Ok::<_, anyhow::Error>(waveforms)
    };

    tokio::time::timeout(timeout, collect)
        .await
        .map_err(|_| anyhow::anyhow!("timed out collecting {count} waveforms from {pv_name}"))?
}

/// Write a PV value. Currently shells out to caput since epicars may not support writes.
pub async fn caput(pv_name: &str, value: f64) -> anyhow::Result<()> {
    let output = tokio::process::Command::new("caput")
        .arg(pv_name)
        .arg(value.to_string())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("caput failed for {pv_name}: {stderr}");
    }
    Ok(())
}
