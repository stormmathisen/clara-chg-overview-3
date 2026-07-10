use std::sync::Arc;

use shared::messages::{ClientMessage, DeviceType, NotificationLevel, ServerMessage};
use tracing::error;

use crate::audit::AuditLog;
use crate::epics;
use crate::hardware;
use crate::state::AppState;
use crate::ws::{broadcast_notification, Broadcaster};

/// Handle a command from a client
pub async fn handle_command(
    msg: ClientMessage,
    state: &AppState,
    broadcaster: &Broadcaster,
    audit: &Arc<AuditLog>,
) {
    audit.log_command("ws-client", &format!("{msg:?}"));

    match msg {
        ClientMessage::SetSensitivity { device, index } => {
            handle_set_sensitivity(&device, index, state, broadcaster).await;
        }
        ClientMessage::ZeroWCM { device } => {
            handle_zero_wcm(&device, state, broadcaster).await;
        }
        ClientMessage::SweepTiming { device } => {
            handle_sweep_timing(&device, state, broadcaster).await;
        }
        ClientMessage::RestoreDefaults { device } => {
            handle_restore_defaults(&device, state, broadcaster).await;
        }
        ClientMessage::ClearCalibration => {
            handle_clear_calibration(state, broadcaster).await;
        }
        ClientMessage::SetBufferSize { size } => {
            handle_set_buffer_size(size, state, broadcaster).await;
        }
        ClientMessage::SetDeviceOrder { order } => {
            handle_set_device_order(order, state, broadcaster).await;
        }
        ClientMessage::ClearBuffer { device } => {
            handle_clear_buffer(device, state).await;
        }
    }
}

async fn handle_set_sensitivity(
    device_name: &str,
    index: usize,
    state: &AppState,
    broadcaster: &Broadcaster,
) {
    let (ip, sensitivity_level, corr_a_pv, corr_a_value, dq_info) = {
        let state_read = state.read().await;
        let Some(device) = state_read.devices.iter().find(|d| d.name == device_name) else {
            error!("Device {device_name} not found");
            return;
        };

        if device.config.device_type == DeviceType::Ict {
            error!("SetSensitivity not applicable to ICT device {device_name}");
            return;
        }

        let sensitivities = &device.config.sensitivities;
        if index >= sensitivities.len() {
            error!("Sensitivity index {index} out of range for {device_name}");
            return;
        }
        let level = sensitivities[index];
        let ip = device.config.ip.clone();

        let corr_a_pv = device.config.pvs.get("corrA").cloned();
        let corr_a_value = device
            .config
            .defaults
            .get("corrA")
            .map(|d| d.for_sensitivity(index));

        // If WCM, also need to set DQcal for the companion :DQ device
        let dq_info = if device.config.device_type == DeviceType::Wcm {
            let dq_name = format!("{device_name}:DQ");
            state_read
                .devices
                .iter()
                .find(|d| d.name == dq_name)
                .map(|dq| {
                    let pv = dq.config.pvs.get("DQcal").cloned();
                    let val = dq
                        .config
                        .defaults
                        .get("DQcal")
                        .map(|d| d.for_sensitivity(index));
                    (pv, val)
                })
        } else {
            None
        };

        (ip, level, corr_a_pv, corr_a_value, dq_info)
    };

    // Send TCP settings to hardware
    let settings = hardware::settings_for_sensitivity(sensitivity_level);
    if let Err(e) = hardware::send_settings(&ip, &settings).await {
        error!("Failed to send settings to {device_name}: {e}");
        broadcast_notification(
            broadcaster,
            NotificationLevel::Error,
            format!("Failed to set sensitivity for {device_name}: {e}"),
            Some(device_name.to_string()),
        );
        return;
    }

    // Set corrA via EPICS
    if let (Some(pv), Some(val)) = (corr_a_pv, corr_a_value) {
        if let Err(e) = epics::caput(&pv, val).await {
            error!("Failed to caput corrA for {device_name}: {e}");
        }
    }

    // Set DQcal if WCM
    if let Some((Some(pv), Some(val))) = dq_info {
        if let Err(e) = epics::caput(&pv, val).await {
            error!("Failed to caput DQcal: {e}");
        }
    }

    // Update state and broadcast
    {
        let mut state_write = state.write().await;
        if let Some(device) = state_write
            .devices
            .iter_mut()
            .find(|d| d.name == device_name)
        {
            device.current_sensitivity = index;
        }
    }

    let update = ServerMessage::StateUpdate {
        device: device_name.to_string(),
        sensitivity: index,
    };
    if let Ok(json) = serde_json::to_string(&update) {
        let _ = broadcaster.send(json);
    }

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        format!("Sensitivity set to {sensitivity_level} for {device_name}"),
        Some(device_name.to_string()),
    );
}

/// Number of fresh charge readings averaged to compute the WCM zero offset.
const ZERO_SAMPLE_COUNT: u64 = 100;
/// Timeout for collecting those readings. At the current 10 Hz rep rate,
/// ZERO_SAMPLE_COUNT (100) readings take ~10s, so allow headroom.
const ZERO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Poll interval while waiting for fresh readings.
const ZERO_POLL: std::time::Duration = std::time::Duration::from_millis(100);

async fn handle_zero_wcm(device_name: &str, state: &AppState, broadcaster: &Broadcaster) {
    let corr_b_pv = {
        let state_read = state.read().await;
        let Some(device) = state_read.devices.iter().find(|d| d.name == device_name) else {
            error!("Device {device_name} not found");
            return;
        };
        device.config.pvs.get("corrB").cloned()
    };

    let Some(corr_b_pv) = corr_b_pv else {
        broadcast_notification(
            broadcaster,
            NotificationLevel::Error,
            format!("No corrB PV for {device_name}"),
            Some(device_name.to_string()),
        );
        return;
    };

    broadcast_notification(
        broadcaster,
        NotificationLevel::Info,
        format!("Zeroing {device_name}... collecting {ZERO_SAMPLE_COUNT} samples"),
        Some(device_name.to_string()),
    );

    // Set corrB to 0 first so that fresh charge readings reflect a zero offset.
    if let Err(e) = epics::caput(&corr_b_pv, 0.0).await {
        error!("Failed to zero corrB for {device_name}: {e}");
        broadcast_notification(
            broadcaster,
            NotificationLevel::Error,
            format!("Failed to zero {device_name}: {e}"),
            Some(device_name.to_string()),
        );
        return;
    }

    // Snapshot the push counter, then wait for ZERO_SAMPLE_COUNT fresh readings to
    // arrive (robust to the user-configurable buffer size), bounded by ZERO_TIMEOUT.
    let baseline = match sample_push_count(state, device_name).await {
        Some(n) => n,
        None => return,
    };
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(ZERO_POLL).await;
        let Some(now) = sample_push_count(state, device_name).await else {
            return; // device vanished
        };
        if now.wrapping_sub(baseline) >= ZERO_SAMPLE_COUNT {
            break;
        }
        if start.elapsed() > ZERO_TIMEOUT {
            broadcast_notification(
                broadcaster,
                NotificationLevel::Error,
                format!("Zeroing {device_name}: timed out waiting for charge readings"),
                Some(device_name.to_string()),
            );
            return;
        }
    }

    let mean = {
        let state_read = state.read().await;
        let Some(device) = state_read.devices.iter().find(|d| d.name == device_name) else {
            return;
        };
        device.buffer.mean_of_last(ZERO_SAMPLE_COUNT as usize)
    };
    let Some(mean) = mean else {
        broadcast_notification(
            broadcaster,
            NotificationLevel::Error,
            format!("Zeroing {device_name}: no charge readings available"),
            Some(device_name.to_string()),
        );
        return;
    };

    if let Err(e) = epics::caput(&corr_b_pv, mean).await {
        error!("Failed to set corrB to mean for {device_name}: {e}");
        broadcast_notification(
            broadcaster,
            NotificationLevel::Error,
            format!("Failed to set offset for {device_name}: {e}"),
            Some(device_name.to_string()),
        );
        return;
    }

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        format!("Zeroed {device_name}: offset = {mean:.4}"),
        Some(device_name.to_string()),
    );
}

/// Clear the rolling data buffer for one device (`Some(name)`) or all devices (`None`).
/// No dedicated broadcast is needed: the emptied charts appear on the next periodic
/// ChartData tick.
async fn handle_clear_buffer(device: Option<String>, state: &AppState) {
    let mut state_write = state.write().await;
    match device {
        Some(name) => {
            if let Some(d) = state_write.devices.iter_mut().find(|d| d.name == name) {
                d.buffer.clear();
            } else {
                error!("ClearBuffer: device {name} not found");
            }
        }
        None => {
            for d in &mut state_write.devices {
                d.buffer.clear();
            }
        }
    }
}

/// Read a device's monotonic buffer push counter, or None if the device is gone.
async fn sample_push_count(state: &AppState, device_name: &str) -> Option<u64> {
    let state_read = state.read().await;
    state_read
        .devices
        .iter()
        .find(|d| d.name == device_name)
        .map(|d| d.buffer.total_pushed())
}

/// Number of digitizer waveforms averaged when locating the charge peak.
const SWEEP_WAVEFORM_COUNT: usize = 100;
/// Timeout for collecting the sweep waveforms. At the current 10 Hz rep rate,
/// SWEEP_WAVEFORM_COUNT (100) waveforms take ~10s, so allow headroom.
const SWEEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Default sample-window bounds for a device at the current sensitivity.
struct WindowDefaults {
    peak_low: i64,
    peak_high: i64,
    base_low: i64,
    base_high: i64,
}

/// Everything needed to run a sweep, extracted from device config while holding the lock.
struct SweepConfig {
    digitizer: String,
    device_type: DeviceType,
    /// Window-key ("peak_low"/"peak_high"/"base_low"/"base_high") -> PV name.
    window_pvs: std::collections::HashMap<&'static str, String>,
    defaults: WindowDefaults,
}

/// Mean peak-sample index across a set of waveforms. `find_max` selects argmax
/// (WCM/DQ, positive-going) vs argmin (FCUP, negative-going). Empty waveforms are
/// skipped; returns None if there is no usable data.
fn mean_peak_index(waveforms: &[Vec<f64>], find_max: bool) -> Option<f64> {
    let mut sum = 0.0;
    let mut n = 0usize;
    for wf in waveforms {
        let Some((idx, _)) = wf.iter().copied().enumerate().reduce(|best, cur| {
            let better = if find_max {
                cur.1 > best.1
            } else {
                cur.1 < best.1
            };
            if better {
                cur
            } else {
                best
            }
        }) else {
            continue;
        };
        sum += idx as f64;
        n += 1;
    }
    (n > 0).then(|| sum / n as f64)
}

/// Compute the integer window bounds to write, given the mean peak index.
/// Faithful port of `sweep_timing` from the Python reference: the peak window is
/// centred on the peak; for WCM the base window is offset ahead of the peak window
/// by the same gap it has by default. FCUP/DQ set the peak window only.
fn compute_windows(
    peak: f64,
    device_type: &DeviceType,
    d: &WindowDefaults,
) -> Vec<(&'static str, i64)> {
    let peak_offset = (d.peak_high - d.peak_low) / 2;
    let anchor = peak - peak_offset as f64; // float peak_low, reused for the base window
    let mut out = vec![
        ("peak_low", anchor as i64),
        ("peak_high", (peak + peak_offset as f64) as i64),
    ];
    if *device_type == DeviceType::Wcm {
        out.push((
            "base_low",
            (anchor - (d.peak_low - d.base_low) as f64) as i64,
        ));
        out.push((
            "base_high",
            (anchor - (d.peak_high - d.base_high) as f64) as i64,
        ));
    }
    out
}

async fn handle_sweep_timing(device_name: &str, state: &AppState, broadcaster: &Broadcaster) {
    // Gather config while holding the read lock briefly.
    let cfg = {
        let state_read = state.read().await;
        let Some(device) = state_read.devices.iter().find(|d| d.name == device_name) else {
            error!("Device {device_name} not found");
            return;
        };
        let idx = device.current_sensitivity;
        let def = |k: &str| {
            device
                .config
                .defaults
                .get(k)
                .map(|v| v.for_sensitivity(idx))
        };

        // peak_low/peak_high are required for every device type.
        let (Some(peak_low), Some(peak_high)) = (def("peak_low"), def("peak_high")) else {
            broadcast_notification(
                broadcaster,
                NotificationLevel::Error,
                format!("Sweep timing: {device_name} has no peak-window defaults configured"),
                Some(device_name.to_string()),
            );
            return;
        };

        let mut window_pvs = std::collections::HashMap::new();
        for key in ["peak_low", "peak_high", "base_low", "base_high"] {
            if let Some(pv) = device.config.pvs.get(key) {
                window_pvs.insert(key, pv.clone());
            }
        }

        SweepConfig {
            digitizer: device.config.digitizer.clone(),
            device_type: device.config.device_type.clone(),
            window_pvs,
            defaults: WindowDefaults {
                peak_low: peak_low as i64,
                peak_high: peak_high as i64,
                base_low: def("base_low").unwrap_or(0.0) as i64,
                base_high: def("base_high").unwrap_or(0.0) as i64,
            },
        }
    };

    broadcast_notification(
        broadcaster,
        NotificationLevel::Info,
        format!("Sweeping timing for {device_name}... collecting waveforms"),
        Some(device_name.to_string()),
    );

    // Collect digitizer waveforms from the "-READ" PV.
    let read_pv = format!("{}-READ", cfg.digitizer);
    let waveforms =
        match epics::collect_waveforms(&read_pv, SWEEP_WAVEFORM_COUNT, SWEEP_TIMEOUT).await {
            Ok(w) => w,
            Err(e) => {
                error!("Sweep timing waveform collection failed for {device_name}: {e}");
                broadcast_notification(
                    broadcaster,
                    NotificationLevel::Error,
                    format!("Sweep timing for {device_name}: {e}"),
                    Some(device_name.to_string()),
                );
                return;
            }
        };

    // WCM/DQ pulses are positive-going (argmax); FCUP is negative-going (argmin).
    // NOTE: the Python reference had a broken `dq` branch (it collapsed each waveform
    // to a scalar before searching); here DQ uses the same peak-window logic as WCM.
    // This deviation should be confirmed against real hardware.
    let find_max = cfg.device_type != DeviceType::Fcup;
    let Some(peak) = mean_peak_index(&waveforms, find_max) else {
        broadcast_notification(
            broadcaster,
            NotificationLevel::Error,
            format!("Sweep timing for {device_name}: no usable waveform data"),
            Some(device_name.to_string()),
        );
        return;
    };

    for (key, value) in compute_windows(peak, &cfg.device_type, &cfg.defaults) {
        if let Some(pv_name) = cfg.window_pvs.get(key) {
            if let Err(e) = epics::caput(pv_name, value as f64).await {
                error!("Failed to set {key} ({pv_name}) for {device_name}: {e}");
            }
        }
    }

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        format!("Sweep timing for {device_name}: peak at sample {peak:.1}, windows updated"),
        Some(device_name.to_string()),
    );
}

async fn handle_restore_defaults(device_name: &str, state: &AppState, broadcaster: &Broadcaster) {
    let pvs_and_defaults = {
        let state_read = state.read().await;
        let Some(device) = state_read.devices.iter().find(|d| d.name == device_name) else {
            error!("Device {device_name} not found");
            return;
        };
        let sensitivity_index = device.current_sensitivity;

        let mut result: Vec<(String, f64)> = Vec::new();
        for (key, pv_name) in &device.config.pvs {
            if key == "charge" {
                continue;
            }
            if let Some(default) = device.config.defaults.get(key) {
                let val = default.for_sensitivity(sensitivity_index);
                result.push((pv_name.clone(), val));
            }
        }
        result
    };

    for (pv_name, value) in &pvs_and_defaults {
        if let Err(e) = epics::caput(pv_name, *value).await {
            error!("Failed to restore {pv_name}: {e}");
            broadcast_notification(
                broadcaster,
                NotificationLevel::Error,
                format!("Failed to restore {pv_name}: {e}"),
                Some(device_name.to_string()),
            );
        }
    }

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        format!("Restored defaults for {device_name}"),
        Some(device_name.to_string()),
    );
}

async fn handle_clear_calibration(state: &AppState, broadcaster: &Broadcaster) {
    let devices_info: Vec<(String, String, u8)> = {
        let state_read = state.read().await;
        state_read
            .devices
            .iter()
            .filter(|d| {
                d.config.device_type != DeviceType::Dq && d.config.device_type != DeviceType::Ict
            })
            .map(|d| {
                let level = d
                    .config
                    .sensitivities
                    .get(d.current_sensitivity)
                    .copied()
                    .unwrap_or(3);
                (d.name.clone(), d.config.ip.clone(), level)
            })
            .collect()
    };

    for (name, ip, level) in &devices_info {
        let settings = hardware::settings_for_clear_calibration(*level);
        if let Err(e) = hardware::send_settings(ip, &settings).await {
            error!("Failed to clear calibration for {name}: {e}");
            broadcast_notification(
                broadcaster,
                NotificationLevel::Error,
                format!("Failed to clear calibration for {name}: {e}"),
                Some(name.clone()),
            );
        }
    }

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        "Cleared calibration mode for all devices".to_string(),
        None,
    );
}

async fn handle_set_buffer_size(size: usize, state: &AppState, broadcaster: &Broadcaster) {
    let size = size.clamp(10, 10000);
    {
        let mut state_write = state.write().await;
        state_write.buffer_size = size;
        for device in &mut state_write.devices {
            device.buffer.set_capacity(size);
        }
    }

    let msg = ServerMessage::BufferSizeChanged { size };
    if let Ok(json) = serde_json::to_string(&msg) {
        let _ = broadcaster.send(json);
    }
}

async fn handle_set_device_order(order: Vec<String>, state: &AppState, broadcaster: &Broadcaster) {
    {
        let mut state_write = state.write().await;
        state_write.device_order = order.clone();
    }

    let msg = ServerMessage::DeviceOrderChanged { order };
    if let Ok(json) = serde_json::to_string(&msg) {
        let _ = broadcaster.send(json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_peak_index_argmax() {
        let waveforms = vec![vec![0.0, 5.0, 2.0], vec![1.0, 1.0, 9.0]];
        // argmax indices: 1 and 2 -> mean 1.5
        assert_eq!(mean_peak_index(&waveforms, true), Some(1.5));
    }

    #[test]
    fn mean_peak_index_argmin() {
        let waveforms = vec![vec![0.0, 5.0, 2.0], vec![3.0, -1.0, 2.0]];
        // argmin indices: 0 and 1 -> mean 0.5
        assert_eq!(mean_peak_index(&waveforms, false), Some(0.5));
    }

    #[test]
    fn mean_peak_index_skips_empty_and_handles_none() {
        let empty: Vec<Vec<f64>> = vec![vec![]];
        assert_eq!(mean_peak_index(&empty, true), None);
        assert_eq!(mean_peak_index(&[], true), None);
        // First waveform empty, second usable (argmax idx 0) -> mean 0.0
        let mixed = vec![vec![], vec![7.0, 1.0]];
        assert_eq!(mean_peak_index(&mixed, true), Some(0.0));
    }

    #[test]
    fn compute_windows_wcm_sets_peak_and_base() {
        // WCM defaults from config: peak_low=1035, peak_high=1037, base_low=1025, base_high=1027
        let d = WindowDefaults {
            peak_low: 1035,
            peak_high: 1037,
            base_low: 1025,
            base_high: 1027,
        };
        let windows = compute_windows(1040.0, &DeviceType::Wcm, &d);
        // peak_offset = (1037-1035)/2 = 1; anchor = 1039
        assert_eq!(
            windows,
            vec![
                ("peak_low", 1039),
                ("peak_high", 1041),
                // base window sits 10 samples (peak_low_def - base_low_def) ahead of the peak window
                ("base_low", 1029),
                ("base_high", 1029),
            ]
        );
    }

    #[test]
    fn compute_windows_fcup_peak_only() {
        // FCUP defaults: peak_low=1040, peak_high=1046
        let d = WindowDefaults {
            peak_low: 1040,
            peak_high: 1046,
            base_low: 200,
            base_high: 800,
        };
        let windows = compute_windows(1000.0, &DeviceType::Fcup, &d);
        // peak_offset = (1046-1040)/2 = 3
        assert_eq!(windows, vec![("peak_low", 997), ("peak_high", 1003)]);
    }
}
