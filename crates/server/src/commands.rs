use std::sync::Arc;

use shared::messages::{ClientMessage, DeviceType, NotificationLevel, ServerMessage};
use tracing::error;

use crate::audit::AuditLog;
use crate::epics;
use crate::hardware;
use crate::state::AppState;
use crate::ws::{broadcast_chart_reset, broadcast_notification, send_message, Broadcaster};

/// PV / default map keys used across device configs. Centralised so the vocabulary
/// lives in one place instead of being repeated as bare string literals.
pub mod keys {
    pub const CHARGE: &str = "charge";
    pub const CORR_A: &str = "corrA";
    pub const CORR_B: &str = "corrB";
    pub const DQ_CAL: &str = "DQcal";
    pub const PEAK_LOW: &str = "peak_low";
    pub const PEAK_HIGH: &str = "peak_high";
    pub const BASE_LOW: &str = "base_low";
    pub const BASE_HIGH: &str = "base_high";
    /// EVR output enable PV whose trigger feeds a device's front-end box.
    pub const RESET_TRIGGER: &str = "reset_trigger";

    /// The four sweep-timing window keys, in the order they are written.
    pub const WINDOW_KEYS: [&str; 4] = [PEAK_LOW, PEAK_HIGH, BASE_LOW, BASE_HIGH];

    /// Name of the companion dark-charge (`:DQ`) device for a WCM device.
    pub fn dq_companion(wcm_name: &str) -> String {
        format!("{wcm_name}:DQ")
    }
}

/// A command failure surfaced to the operator. The dispatcher logs it and pushes an
/// error notification to all clients, so individual handlers just `return Err(..)`
/// instead of repeating the log + notify + return triad at every failure site.
struct CommandError {
    message: String,
    device: Option<String>,
}

impl CommandError {
    fn for_device(message: impl Into<String>, device: &str) -> Self {
        Self {
            message: message.into(),
            device: Some(device.to_string()),
        }
    }
}

/// Log an error and broadcast it as a notification. Used both by the dispatcher for
/// returned `CommandError`s and inline by best-effort handlers that keep going after
/// a per-device failure.
fn notify_error(broadcaster: &Broadcaster, message: String, device: Option<String>) {
    error!("{message}");
    broadcast_notification(broadcaster, NotificationLevel::Error, message, device);
}

/// Handle a command from a client
pub async fn handle_command(
    msg: ClientMessage,
    state: &AppState,
    broadcaster: &Broadcaster,
    audit: &Arc<AuditLog>,
) {
    audit.log_command("ws-client", &format!("{msg:?}"));

    let result = match msg {
        ClientMessage::SetSensitivity { device, index } => {
            handle_set_sensitivity(&device, index, state, broadcaster).await
        }
        ClientMessage::ZeroWCM { device } => handle_zero_wcm(&device, state, broadcaster).await,
        ClientMessage::SweepTiming { device } => {
            handle_sweep_timing(&device, state, broadcaster).await
        }
        ClientMessage::RestoreDefaults { device } => {
            handle_restore_defaults(&device, state, broadcaster).await
        }
        ClientMessage::ClearCalibration => handle_clear_calibration(state, broadcaster).await,
        ClientMessage::ResetFrontEnds => {
            spawn_reset(state.clone(), broadcaster.clone());
            Ok(())
        }
        ClientMessage::SetBufferSize { size } => {
            handle_set_buffer_size(size, state, broadcaster).await;
            Ok(())
        }
        ClientMessage::SetDeviceOrder { order } => {
            handle_set_device_order(order, state, broadcaster).await;
            Ok(())
        }
        ClientMessage::ClearBuffer { device } => {
            handle_clear_buffer(device, state, broadcaster).await;
            Ok(())
        }
    };

    if let Err(e) = result {
        notify_error(broadcaster, e.message, e.device);
    }
}

async fn handle_set_sensitivity(
    device_name: &str,
    index: usize,
    state: &AppState,
    broadcaster: &Broadcaster,
) -> Result<(), CommandError> {
    let (ip, sensitivity_level, corr_a_pv, corr_a_value, dq_info) = {
        let state_read = state.read().await;
        let Some(device) = state_read.device(device_name) else {
            return Err(CommandError::for_device(
                format!("Device {device_name} not found"),
                device_name,
            ));
        };

        // ICTs have no front-end box and no sensitivities to select.
        if device.config.device_type == DeviceType::Ict {
            return Err(CommandError::for_device(
                format!("SetSensitivity not applicable to ICT device {device_name}"),
                device_name,
            ));
        }

        let sensitivities = &device.config.sensitivities;
        if index >= sensitivities.len() {
            return Err(CommandError::for_device(
                format!("Sensitivity index {index} out of range for {device_name}"),
                device_name,
            ));
        }
        let level = sensitivities[index];
        let ip = device.config.ip.clone();

        let corr_a_pv = device.config.pvs.get(keys::CORR_A).cloned();
        let corr_a_value = device
            .config
            .defaults
            .get(keys::CORR_A)
            .map(|d| d.for_sensitivity(index));

        // If WCM, also need to set DQcal for the companion :DQ device
        let dq_info = if device.config.device_type == DeviceType::Wcm {
            state_read
                .device(&keys::dq_companion(device_name))
                .map(|dq| {
                    let pv = dq.config.pvs.get(keys::DQ_CAL).cloned();
                    let val = dq
                        .config
                        .defaults
                        .get(keys::DQ_CAL)
                        .map(|d| d.for_sensitivity(index));
                    (pv, val)
                })
        } else {
            None
        };

        (ip, level, corr_a_pv, corr_a_value, dq_info)
    };

    // Push the integrator to the front-end box over its HTTP API.
    if let Err(e) = hardware::set_sensitivity(&ip, sensitivity_level).await {
        return Err(CommandError::for_device(
            format!("Failed to set sensitivity for {device_name}: {e}"),
            device_name,
        ));
    }

    // Record the new sensitivity before the best-effort caputs below, so that when the
    // front-end echoes this change back over its `/events` SSE stream the index already
    // matches and `fe_events` treats it as our own write, not an external change.
    {
        let mut state_write = state.write().await;
        if let Some(device) = state_write.device_mut(device_name) {
            device.current_sensitivity = index;
        }
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

    send_message(
        broadcaster,
        &ServerMessage::StateUpdate {
            device: device_name.to_string(),
            sensitivity: index,
        },
    );

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        format!("Sensitivity set to {sensitivity_level} for {device_name}"),
        Some(device_name.to_string()),
    );
    Ok(())
}

/// Number of fresh charge readings averaged to compute the WCM zero offset.
const ZERO_SAMPLE_COUNT: u64 = 100;
/// Timeout for collecting those readings. At the current 10 Hz rep rate,
/// ZERO_SAMPLE_COUNT (100) readings take ~10s, so allow headroom.
const ZERO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Poll interval while waiting for fresh readings.
const ZERO_POLL: std::time::Duration = std::time::Duration::from_millis(100);

async fn handle_zero_wcm(
    device_name: &str,
    state: &AppState,
    broadcaster: &Broadcaster,
) -> Result<(), CommandError> {
    let corr_b_pv = {
        let state_read = state.read().await;
        let Some(device) = state_read.device(device_name) else {
            return Err(CommandError::for_device(
                format!("Device {device_name} not found"),
                device_name,
            ));
        };
        device.config.pvs.get(keys::CORR_B).cloned()
    };

    let Some(corr_b_pv) = corr_b_pv else {
        return Err(CommandError::for_device(
            format!("No corrB PV for {device_name}"),
            device_name,
        ));
    };

    broadcast_notification(
        broadcaster,
        NotificationLevel::Info,
        format!("Zeroing {device_name}... collecting {ZERO_SAMPLE_COUNT} samples"),
        Some(device_name.to_string()),
    );

    // Set corrB to 0 first so that fresh charge readings reflect a zero offset.
    if let Err(e) = epics::caput(&corr_b_pv, 0.0).await {
        return Err(CommandError::for_device(
            format!("Failed to zero {device_name}: {e}"),
            device_name,
        ));
    }

    // Snapshot the push counter, then wait for ZERO_SAMPLE_COUNT fresh readings to
    // arrive (robust to the user-configurable buffer size), bounded by ZERO_TIMEOUT.
    let Some(baseline) = sample_push_count(state, device_name).await else {
        return Ok(()); // device vanished
    };
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(ZERO_POLL).await;
        let Some(now) = sample_push_count(state, device_name).await else {
            return Ok(()); // device vanished
        };
        if now.wrapping_sub(baseline) >= ZERO_SAMPLE_COUNT {
            break;
        }
        if start.elapsed() > ZERO_TIMEOUT {
            return Err(CommandError::for_device(
                format!("Zeroing {device_name}: timed out waiting for charge readings"),
                device_name,
            ));
        }
    }

    let mean = {
        let state_read = state.read().await;
        let Some(device) = state_read.device(device_name) else {
            return Ok(());
        };
        device.buffer.mean_of_last(ZERO_SAMPLE_COUNT as usize)
    };
    let Some(mean) = mean else {
        return Err(CommandError::for_device(
            format!("Zeroing {device_name}: no charge readings available"),
            device_name,
        ));
    };

    if let Err(e) = epics::caput(&corr_b_pv, mean).await {
        return Err(CommandError::for_device(
            format!("Failed to set offset for {device_name}: {e}"),
            device_name,
        ));
    }

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        format!("Zeroed {device_name}: offset = {mean:.4}"),
        Some(device_name.to_string()),
    );
    Ok(())
}

/// Clear the rolling data buffer for one device (`Some(name)`) or all devices (`None`).
/// A clear can't be expressed as an append delta, so a full chart snapshot is
/// broadcast afterwards to reset every client's buffers.
async fn handle_clear_buffer(device: Option<String>, state: &AppState, broadcaster: &Broadcaster) {
    {
        let mut state_write = state.write().await;
        match device {
            Some(name) => {
                if let Some(d) = state_write.device_mut(&name) {
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
    broadcast_chart_reset(state, broadcaster).await;
}

/// Read a device's monotonic buffer push counter, or None if the device is gone.
async fn sample_push_count(state: &AppState, device_name: &str) -> Option<u64> {
    let state_read = state.read().await;
    state_read
        .device(device_name)
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
        (keys::PEAK_LOW, anchor as i64),
        (keys::PEAK_HIGH, (peak + peak_offset as f64) as i64),
    ];
    if *device_type == DeviceType::Wcm {
        out.push((
            keys::BASE_LOW,
            (anchor - (d.peak_low - d.base_low) as f64) as i64,
        ));
        out.push((
            keys::BASE_HIGH,
            (anchor - (d.peak_high - d.base_high) as f64) as i64,
        ));
    }
    out
}

async fn handle_sweep_timing(
    device_name: &str,
    state: &AppState,
    broadcaster: &Broadcaster,
) -> Result<(), CommandError> {
    // Gather config while holding the read lock briefly.
    let cfg = {
        let state_read = state.read().await;
        let Some(device) = state_read.device(device_name) else {
            return Err(CommandError::for_device(
                format!("Device {device_name} not found"),
                device_name,
            ));
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
        let (Some(peak_low), Some(peak_high)) = (def(keys::PEAK_LOW), def(keys::PEAK_HIGH)) else {
            return Err(CommandError::for_device(
                format!("Sweep timing: {device_name} has no peak-window defaults configured"),
                device_name,
            ));
        };

        let mut window_pvs = std::collections::HashMap::new();
        for key in keys::WINDOW_KEYS {
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
                base_low: def(keys::BASE_LOW).unwrap_or(0.0) as i64,
                base_high: def(keys::BASE_HIGH).unwrap_or(0.0) as i64,
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
    let waveforms = epics::collect_waveforms(&read_pv, SWEEP_WAVEFORM_COUNT, SWEEP_TIMEOUT)
        .await
        .map_err(|e| {
            CommandError::for_device(format!("Sweep timing for {device_name}: {e}"), device_name)
        })?;

    // WCM/DQ pulses are positive-going (argmax); FCUP is negative-going (argmin).
    // NOTE: the Python reference had a broken `dq` branch (it collapsed each waveform
    // to a scalar before searching); here DQ uses the same peak-window logic as WCM.
    // This deviation should be confirmed against real hardware.
    let find_max = cfg.device_type != DeviceType::Fcup;
    let Some(peak) = mean_peak_index(&waveforms, find_max) else {
        return Err(CommandError::for_device(
            format!("Sweep timing for {device_name}: no usable waveform data"),
            device_name,
        ));
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
    Ok(())
}

/// Best-effort: attempts every write and notifies per failure, but still reports
/// overall success (matching the legacy behaviour), so it returns `Ok`.
async fn handle_restore_defaults(
    device_name: &str,
    state: &AppState,
    broadcaster: &Broadcaster,
) -> Result<(), CommandError> {
    let pvs_and_defaults = {
        let state_read = state.read().await;
        let Some(device) = state_read.device(device_name) else {
            return Err(CommandError::for_device(
                format!("Device {device_name} not found"),
                device_name,
            ));
        };
        let sensitivity_index = device.current_sensitivity;

        let mut result: Vec<(String, f64)> = Vec::new();
        for (key, pv_name) in &device.config.pvs {
            if key == keys::CHARGE {
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
            notify_error(
                broadcaster,
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
    Ok(())
}

/// Push each device's currently selected sensitivity to its front-end box, best-effort:
/// a box that fails gets an error notification and the rest still get pushed.
///
/// `:DQ` devices are skipped because they share their WCM's physical box (same IP), and
/// ICTs because they have no box at all. `what` names the operation in error messages.
///
/// Each box is set to `FB{level}` with `io.input = "EXT"` — normal operation, calibration
/// mode off (`hardware::clear_calibration`). That is what both callers want: a
/// clear-calibration is exactly "put every box back into normal operation at its selected
/// sensitivity", and so is the resend after a front-end reset.
async fn push_all_front_ends(state: &AppState, broadcaster: &Broadcaster, what: &str) {
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
        if let Err(e) = hardware::clear_calibration(ip, *level).await {
            notify_error(
                broadcaster,
                format!("Failed to {what} for {name}: {e}"),
                Some(name.clone()),
            );
        }
    }
}

/// Best-effort across all non-DQ devices; see `handle_restore_defaults`.
async fn handle_clear_calibration(
    state: &AppState,
    broadcaster: &Broadcaster,
) -> Result<(), CommandError> {
    push_all_front_ends(state, broadcaster, "clear calibration").await;

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        "Cleared calibration mode for all devices".to_string(),
        None,
    );
    Ok(())
}

/// Set while a front-end reset is running. A second reset started mid-wait would flip the
/// trigger back on early and leave the PICs half-rebooted, so only one runs at a time.
static RESET_IN_PROGRESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Run a front-end reset in the background.
///
/// Every other command is awaited inline in the per-client WebSocket receive loop; a reset
/// takes over a minute, which would stall that operator's other commands for its duration.
/// So this one is spawned. Progress and results reach every client over the broadcaster,
/// exactly as they would from the inline path.
fn spawn_reset(state: AppState, broadcaster: Broadcaster) {
    use std::sync::atomic::Ordering;

    if RESET_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        broadcast_notification(
            &broadcaster,
            NotificationLevel::Warning,
            "A front-end reset is already running".to_string(),
            None,
        );
        return;
    }

    tokio::spawn(async move {
        if let Err(e) = reset_front_ends(&state, &broadcaster).await {
            notify_error(&broadcaster, e.message, e.device);
        }
        RESET_IN_PROGRESS.store(false, Ordering::SeqCst);
    });
}

/// Cut the front-end trigger, wait for the PICs to reboot, restore it, then re-apply every
/// device's sensitivity — the boxes come back up defaulted to FB4, so without the resend the
/// readings would be silently wrong.
///
/// The boxes ignore settings pushes while their trigger is off, so the resend must happen
/// strictly after the trigger is back on.
async fn reset_front_ends(state: &AppState, broadcaster: &Broadcaster) -> Result<(), CommandError> {
    // Devices may share an EVR output (today they all do), so toggle each distinct PV once.
    let trigger_pvs: std::collections::BTreeSet<String> = {
        let state_read = state.read().await;
        state_read
            .devices
            .iter()
            .filter_map(|d| d.config.pvs.get(keys::RESET_TRIGGER).cloned())
            .collect()
    };
    if trigger_pvs.is_empty() {
        return Err(CommandError {
            message: format!("No '{}' PV configured for any device", keys::RESET_TRIGGER),
            device: None,
        });
    }

    let total_secs = crate::consts::RESET_WAIT.as_secs() as u32;
    broadcast_notification(
        broadcaster,
        NotificationLevel::Info,
        format!("Resetting front ends: trigger off for {total_secs}s..."),
        None,
    );

    // Cut the trigger. If one PV won't take, restore the ones that did rather than leaving
    // the machine with its triggers half off.
    let mut zeroed: Vec<&String> = Vec::new();
    for pv in &trigger_pvs {
        if let Err(e) = epics::caput(pv, 0.0).await {
            for done in zeroed {
                let _ = epics::caput(done, 1.0).await;
            }
            return Err(CommandError {
                message: format!("Failed to disable trigger {pv}: {e} — reset aborted"),
                device: None,
            });
        }
        zeroed.push(pv);
    }

    for remaining in (1..=total_secs).rev() {
        // Store in shared state too, so a client connecting mid-reset gets the same
        // countdown in its Init instead of the plain button.
        state.write().await.reset_progress = Some((remaining, total_secs));
        send_message(
            broadcaster,
            &ServerMessage::ResetProgress {
                remaining_secs: remaining,
                total_secs,
            },
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // Restore the trigger. This one has to land: a PV stuck at 0 means no beam trigger, so
    // retry once and shout if it still fails — but keep going, the other PVs and the
    // sensitivity resend are still worth doing.
    for pv in &trigger_pvs {
        if let Err(e) = epics::caput(pv, 1.0).await {
            if let Err(e2) = epics::caput(pv, 1.0).await {
                notify_error(
                    broadcaster,
                    format!("FAILED TO RE-ENABLE TRIGGER {pv}: {e}; retry: {e2}"),
                    None,
                );
            }
        }
    }

    state.write().await.reset_progress = None;
    send_message(
        broadcaster,
        &ServerMessage::ResetProgress {
            remaining_secs: 0,
            total_secs,
        },
    );

    push_all_front_ends(state, broadcaster, "re-apply sensitivity").await;

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        "Front ends reset and sensitivities re-applied".to_string(),
        None,
    );
    Ok(())
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

    send_message(broadcaster, &ServerMessage::BufferSizeChanged { size });
    // Resizing may drop points, so reset every client's buffers to match.
    broadcast_chart_reset(state, broadcaster).await;
}

async fn handle_set_device_order(order: Vec<String>, state: &AppState, broadcaster: &Broadcaster) {
    {
        let mut state_write = state.write().await;
        state_write.device_order = order.clone();
    }

    send_message(broadcaster, &ServerMessage::DeviceOrderChanged { order });
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
