//! Passive listener for the front-end boxes' Server-Sent-Events stream.
//!
//! Each box streams its full `Settings` JSON on `GET /events` **every time any setting
//! changes, from any client** — including its own local web UI and physical front-panel.
//! We watch the `integrator` field ("FB{n}") so that a sensitivity changed *outside* this
//! program is reflected in our state and surfaced to operators.
//!
//! De-duplicating our own writes is implicit: `handle_set_sensitivity` updates
//! `current_sensitivity` right after the HTTP POST, so by the time the device echoes the
//! change back over SSE the index already matches and we stay quiet. Only a change we
//! didn't make produces a mismatch — and a notification. The device sends no snapshot on
//! connect (it only forwards live changes), so every event received is a genuine change.

use crate::consts::{FRONT_END_PORT, INITIAL_RETRY_DELAY, MAX_RETRY_DELAY};
use crate::state::AppState;
use crate::ws::{broadcast_notification, send_message, Broadcaster};
use futures::StreamExt;
use shared::messages::{NotificationLevel, ServerMessage};
use tracing::{error, info, warn};

/// Spawn one long-lived SSE listener per device that has a front-end box (non-empty IP).
pub fn spawn_fe_event_listeners(state: AppState, broadcaster: Broadcaster) {
    tokio::spawn(async move {
        let targets: Vec<(usize, String)> = {
            let s = state.read().await;
            s.devices
                .iter()
                .enumerate()
                .filter(|(_, d)| !d.config.ip.is_empty())
                .map(|(i, d)| (i, d.config.ip.clone()))
                .collect()
        };
        for (idx, ip) in targets {
            tokio::spawn(listen(idx, ip, state.clone(), broadcaster.clone()));
        }
    });
}

/// Maintain the `/events` subscription for one box, reconnecting with exponential backoff.
async fn listen(device_index: usize, ip: String, state: AppState, broadcaster: Broadcaster) {
    let url = format!("http://{ip}:{FRONT_END_PORT}/events");
    let mut retry_delay = INITIAL_RETRY_DELAY;
    loop {
        match consume(&url, device_index, &state, &broadcaster).await {
            Ok(()) => warn!("[{ip}] events stream ended, will reconnect"),
            Err(e) => error!("[{ip}] events stream failed: {e}"),
        }
        tokio::time::sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
    }
}

/// Connect and drain SSE frames until the stream ends or errors. Returns Ok when the
/// stream closes cleanly, Err on any connection/transport failure.
async fn consume(
    url: &str,
    device_index: usize,
    state: &AppState,
    broadcaster: &Broadcaster,
) -> anyhow::Result<()> {
    // No request timeout — this is a long-lived stream that is idle between changes.
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    info!("[{url}] subscribed to front-end events");
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    while let Some(chunk) = stream.next().await {
        buf.push_str(&String::from_utf8_lossy(&chunk?));
        // SSE frames are separated by a blank line. Drain every complete frame in the buffer.
        while let Some(end) = buf.find("\n\n") {
            let frame: String = buf.drain(..end + 2).collect();
            if let Some(json) = sse_data(&frame) {
                handle_settings(&json, device_index, state, broadcaster).await;
            }
        }
    }
    Ok(())
}

/// Extract and concatenate the `data:` payload lines of one SSE frame (per the spec,
/// multiple `data:` lines join with `\n`). Returns None for comment/heartbeat frames.
fn sse_data(frame: &str) -> Option<String> {
    let data: Vec<&str> = frame
        .lines()
        .filter_map(|l| {
            l.strip_prefix("data:")
                .map(|v| v.strip_prefix(' ').unwrap_or(v))
        })
        .collect();
    (!data.is_empty()).then(|| data.join("\n"))
}

/// Parse a `Settings` payload, map its integrator to a sensitivity index, and if it
/// differs from what we believe is set, reconcile state and notify operators.
async fn handle_settings(
    json: &str,
    device_index: usize,
    state: &AppState,
    broadcaster: &Broadcaster,
) {
    let Some(fb) = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| integrator_level(v.get("integrator")?.as_str()?))
    else {
        return;
    };

    let (name, changed) = {
        let mut s = state.write().await;
        let Some(device) = s.devices.get_mut(device_index) else {
            return;
        };
        // Map the FB level back to an index in this device's sensitivities array.
        let Some(index) = device.config.sensitivities.iter().position(|&v| v == fb) else {
            warn!(
                "[{}] front-end reports FB{fb} not in its sensitivities",
                device.name
            );
            return;
        };
        let changed = device.current_sensitivity != index;
        if changed {
            device.current_sensitivity = index;
        }
        (device.name.clone(), changed.then_some(index))
    };

    if let Some(index) = changed {
        info!("[{name}] sensitivity changed externally to FB{fb} (index {index})");
        send_message(
            broadcaster,
            &ServerMessage::StateUpdate {
                device: name.clone(),
                sensitivity: index,
            },
        );
        broadcast_notification(
            broadcaster,
            NotificationLevel::Warning,
            format!("Sensitivity changed externally to FB{fb} for {name}"),
            Some(name),
        );
    }
}

/// Parse the number `n` out of an integrator enum string `"FB{n}"`.
fn integrator_level(s: &str) -> Option<u8> {
    s.strip_prefix("FB").and_then(|n| n.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integrator_level() {
        assert_eq!(integrator_level("FB3"), Some(3));
        assert_eq!(integrator_level("FB0"), Some(0));
        assert_eq!(integrator_level("EXT"), None);
        assert_eq!(integrator_level("FB"), None);
    }

    #[test]
    fn extracts_sse_data_payload() {
        assert_eq!(
            sse_data("data: {\"a\":1}\n\n").as_deref(),
            Some("{\"a\":1}")
        );
        // Multi-line data joins with newline; comment/heartbeat frames yield nothing.
        assert_eq!(sse_data("data: a\ndata: b\n\n").as_deref(), Some("a\nb"));
        assert_eq!(sse_data(": keep-alive\n\n"), None);
    }
}
