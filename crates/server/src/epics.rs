use crate::state::AppState;
use epicars::client::Client;
use epicars::dbr::DbrValue;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
/// Bound on each stage of a PV write (client creation, then the CA put). A PV that
/// does not exist will burn this once during the channel search.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Shared Channel Access client used for all PV writes.
///
/// Constructing a `Client` is expensive — measured at ~83 ms, dominated by CA startup,
/// versus ~0.3 ms for a write on an already-built client. Creating one per write made
/// writes *slower* than the old `caput` shell-out (~34 ms), so the client is built once
/// and reused. `write_pv` needs `&mut self`, hence the mutex; writes are operator-driven
/// and already sequential, so serializing them costs nothing.
///
/// `None` means "not yet built, or the last write failed" — a failed write clears it so
/// the next attempt reconnects rather than reusing a dead client.
static WRITE_CLIENT: std::sync::OnceLock<tokio::sync::Mutex<Option<Client>>> =
    std::sync::OnceLock::new();

fn write_client() -> &'static tokio::sync::Mutex<Option<Client>> {
    WRITE_CLIENT.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// Write a scalar PV value over Channel Access using the native `epicars` client.
///
/// Used for `corrA`/`corrB` (zero-WCM), `DQcal`, sweep-timing windows and
/// restore-defaults. An `f64` becomes a `DbrValue::Double`, the same DBR type the old
/// `caput <pv> <value>` shell-out sent.
///
/// The client honours `EPICS_CA_ADDR_LIST`, `EPICS_CA_AUTO_ADDR_LIST` and
/// `EPICS_CA_SERVER_PORT`, which `config::apply_epics_env` sets from `network.yaml`
/// before any client is built — so `VIRTUAL=1` (which uses a non-default server port)
/// still routes writes to the virtual network, exactly as the inherited-env shell-out did.
pub async fn caput(pv_name: &str, value: f64) -> anyhow::Result<()> {
    let mut guard = write_client().lock().await;

    if guard.is_none() {
        let client = tokio::time::timeout(WRITE_TIMEOUT, Client::new())
            .await
            .map_err(|_| anyhow::anyhow!("timeout creating CA client to write {pv_name}"))??;
        *guard = Some(client);
    }
    let client = guard.as_mut().expect("client was just initialized");

    let result = tokio::time::timeout(WRITE_TIMEOUT, client.write_pv(pv_name, value)).await;

    match result {
        Ok(Ok(())) => {
            info!("caput {pv_name} = {value}");
            Ok(())
        }
        // Drop the client on failure so the next write rebuilds it rather than inheriting
        // a dead circuit. `epicars` does not re-export `ClientError`, so we cannot single
        // out a benign "PV not found"; the cost of being conservative is one ~83 ms client
        // rebuild after a failed write, which only happens on an already-exceptional path.
        Ok(Err(e)) => {
            *guard = None;
            Err(anyhow::anyhow!("failed to write {pv_name}: {e}"))
        }
        Err(_) => {
            *guard = None;
            Err(anyhow::anyhow!("timeout writing {pv_name}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end check of the native write path: stand up an in-process `epicars` CA
    /// server exposing one PV, point the client at it via the standard EPICS env vars,
    /// and assert `caput` actually lands the value. No EPICS base / external IOC needed.
    ///
    /// Uses a non-default search port so it cannot collide with a real IOC on the host.
    /// Mutates process env, so it must not run alongside another CA test.
    #[tokio::test]
    async fn caput_writes_scalar_over_channel_access() {
        use epicars::providers::IntercomProvider;
        use epicars::ServerBuilder;

        const PV: &str = "CLARA:TEST:CAPUT";
        const SEARCH_PORT: u16 = 55064;

        let mut provider = IntercomProvider::new();
        let value = provider.add_pv(PV, 0.0f64).expect("add_pv");

        let _server = ServerBuilder::new(provider)
            .search_port(SEARCH_PORT)
            .beacons(false)
            .start()
            .await
            .expect("test CA server should start");

        std::env::set_var("EPICS_CA_ADDR_LIST", "127.0.0.1");
        std::env::set_var("EPICS_CA_AUTO_ADDR_LIST", "NO");
        std::env::set_var("EPICS_CA_SERVER_PORT", SEARCH_PORT.to_string());

        assert_eq!(value.load(), 0.0);
        caput(PV, 42.5).await.expect("caput should succeed");
        assert_eq!(value.load(), 42.5, "value should have been written over CA");
    }

    #[test]
    fn extract_f64_reads_first_scalar_of_each_numeric_variant() {
        assert_eq!(extract_f64(&DbrValue::Double(vec![1.5, 2.5])), Some(1.5));
        assert_eq!(extract_f64(&DbrValue::Float(vec![3.0])), Some(3.0));
        assert_eq!(extract_f64(&DbrValue::Long(vec![-7])), Some(-7.0));
        assert_eq!(extract_f64(&DbrValue::Int(vec![42])), Some(42.0));
        assert_eq!(extract_f64(&DbrValue::Char(vec![5])), Some(5.0));
        assert_eq!(extract_f64(&DbrValue::Enum(9)), Some(9.0));
    }

    #[test]
    fn extract_f64_none_for_empty_or_string() {
        assert_eq!(extract_f64(&DbrValue::Double(vec![])), None);
        assert_eq!(extract_f64(&DbrValue::String(vec!["x".into()])), None);
    }

    #[test]
    fn extract_f64_array_collects_full_waveform() {
        assert_eq!(
            extract_f64_array(&DbrValue::Double(vec![1.0, 2.0, 3.0])),
            Some(vec![1.0, 2.0, 3.0])
        );
        assert_eq!(
            extract_f64_array(&DbrValue::Int(vec![1, 2])),
            Some(vec![1.0, 2.0])
        );
        // Scalar enum yields a single-element vec; strings yield None.
        assert_eq!(extract_f64_array(&DbrValue::Enum(4)), Some(vec![4.0]));
        assert_eq!(extract_f64_array(&DbrValue::String(vec![])), None);
    }
}
