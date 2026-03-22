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
        let Some(device) = state_read
            .devices
            .iter()
            .find(|d| d.name == device_name)
        else {
            error!("Device {device_name} not found");
            return;
        };

        let sensitivities = &device.config.sensitivities;
        if index >= sensitivities.len() {
            error!("Sensitivity index {index} out of range for {device_name}");
            return;
        }
        let level = sensitivities[index];
        let ip = device.config.ip.clone();

        let corr_a_pv = device.config.pvs.get("corrA").cloned();
        let corr_a_value = device.config.defaults.get("corrA").map(|d| d.for_sensitivity(index));

        // If WCM, also need to set DQcal for the companion :DQ device
        let dq_info = if device.config.device_type == DeviceType::Wcm {
            let dq_name = format!("{device_name}:DQ");
            state_read
                .devices
                .iter()
                .find(|d| d.name == dq_name)
                .map(|dq| {
                    let pv = dq.config.pvs.get("DQcal").cloned();
                    let val = dq.config.defaults.get("DQcal").map(|d| d.for_sensitivity(index));
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
        if let Some(device) = state_write.devices.iter_mut().find(|d| d.name == device_name) {
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

async fn handle_zero_wcm(
    device_name: &str,
    state: &AppState,
    broadcaster: &Broadcaster,
) {
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
        format!("Zeroing {device_name}... collecting samples"),
        Some(device_name.to_string()),
    );

    // Set corrB to 0 first
    if let Err(e) = epics::caput(&corr_b_pv, 0.0).await {
        error!("Failed to zero corrB: {e}");
        return;
    }

    // Collect 100 charge readings from the buffer
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    let mean = {
        let state_read = state.read().await;
        let Some(device) = state_read.devices.iter().find(|d| d.name == device_name) else {
            return;
        };
        device.buffer.statistics().mean
    };

    if let Err(e) = epics::caput(&corr_b_pv, mean).await {
        error!("Failed to set corrB to mean: {e}");
        return;
    }

    broadcast_notification(
        broadcaster,
        NotificationLevel::Success,
        format!("Zeroed {device_name}: offset = {mean:.4}"),
        Some(device_name.to_string()),
    );
}

async fn handle_sweep_timing(
    device_name: &str,
    _state: &AppState,
    broadcaster: &Broadcaster,
) {
    // Sweep timing requires reading digitizer waveforms, which needs waveform PV support.
    // This is a complex operation — placeholder for now.
    broadcast_notification(
        broadcaster,
        NotificationLevel::Warning,
        format!("Sweep timing for {device_name} not yet implemented in Rust version"),
        Some(device_name.to_string()),
    );
}

async fn handle_restore_defaults(
    device_name: &str,
    state: &AppState,
    broadcaster: &Broadcaster,
) {
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

async fn handle_clear_calibration(
    state: &AppState,
    broadcaster: &Broadcaster,
) {
    let devices_info: Vec<(String, String, u8)> = {
        let state_read = state.read().await;
        state_read
            .devices
            .iter()
            .filter(|d| d.config.device_type != DeviceType::Dq)
            .map(|d| {
                let level = d.config.sensitivities
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

async fn handle_set_buffer_size(
    size: usize,
    state: &AppState,
    broadcaster: &Broadcaster,
) {
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

async fn handle_set_device_order(
    order: Vec<String>,
    state: &AppState,
    broadcaster: &Broadcaster,
) {
    {
        let mut state_write = state.write().await;
        state_write.device_order = order.clone();
    }

    let msg = ServerMessage::DeviceOrderChanged { order };
    if let Ok(json) = serde_json::to_string(&msg) {
        let _ = broadcaster.send(json);
    }
}
