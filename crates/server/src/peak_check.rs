//! Peak-window alignment checker.
//!
//! On boot, and whenever a device's `peak_low`/`peak_high` PVs change, verify that
//! the configured window actually brackets the peak in the digitizer signal
//! (positive-going for WCM, negative-going for FCUP). A misaligned window means the
//! charge integration is sampling the wrong part of the waveform, so the flag is
//! pushed to every client and a warning notification names the fix (Sweep Timing).

use std::time::Duration;

use shared::messages::{DeviceType, NotificationLevel, ServerMessage};
use tracing::warn;

use crate::commands::{keys, mean_peak_index};
use crate::epics;
use crate::state::AppState;
use crate::ws::{broadcast_notification, send_message, Broadcaster};

// ponytail: window PVs are polled at 30s rather than live CA subscriptions — good
// enough for "on boot and when the window moves"; subscribe like `persistent_monitor`
// if change latency ever matters.
const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Timeout for reading one window PV.
const CAGET_TIMEOUT: Duration = Duration::from_secs(5);
/// Waveforms averaged to locate the actual peak (~1s at the 10 Hz rep rate).
const CHECK_WAVEFORMS: usize = 10;
/// Bound on collecting those waveforms.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// One device's checkable peak window.
struct Target {
    index: usize,
    name: String,
    digitizer: String,
    low_pv: String,
    high_pv: String,
    /// argmax (WCM, positive-going) vs argmin (FCUP, negative-going).
    find_max: bool,
}

/// Spawn one alignment-check loop per WCM/FCUP device with peak-window PVs.
pub fn spawn_peak_checkers(state: AppState, broadcaster: Broadcaster) {
    tokio::spawn(async move {
        let targets: Vec<Target> = {
            let s = state.read().await;
            s.devices
                .iter()
                .enumerate()
                .filter_map(|(index, d)| {
                    // Only WCM and FCUP have a single beam peak to check; DQ's dark
                    // charge and ICTs have no meaningful digitizer peak window.
                    let find_max = match d.config.device_type {
                        DeviceType::Wcm => true,
                        DeviceType::Fcup => false,
                        _ => return None,
                    };
                    Some(Target {
                        index,
                        name: d.name.clone(),
                        digitizer: d.config.digitizer.clone(),
                        low_pv: d.config.pvs.get(keys::PEAK_LOW)?.clone(),
                        high_pv: d.config.pvs.get(keys::PEAK_HIGH)?.clone(),
                        find_max,
                    })
                })
                .collect()
        };
        for target in targets {
            tokio::spawn(peak_check_loop(state.clone(), broadcaster.clone(), target));
        }
    });
}

/// Poll the window PVs; on the first read (boot) and on any change, locate the actual
/// peak and reconcile the device's `peak_misaligned` flag. Failures just retry on the
/// next poll, so a device that is down produces a warning log, not a task death.
async fn peak_check_loop(state: AppState, broadcaster: Broadcaster, t: Target) {
    let mut last_checked: Option<(f64, f64)> = None;
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    loop {
        interval.tick().await;

        let window = tokio::try_join!(
            epics::caget(&t.low_pv, CAGET_TIMEOUT),
            epics::caget(&t.high_pv, CAGET_TIMEOUT)
        );
        let (low, high) = match window {
            Ok(w) => w,
            Err(e) => {
                warn!("[{}] peak check: {e}", t.name);
                continue;
            }
        };
        if last_checked == Some((low, high)) {
            continue;
        }

        let read_pv = format!("{}-READ", t.digitizer);
        let waveforms =
            match epics::collect_waveforms(&read_pv, CHECK_WAVEFORMS, CHECK_TIMEOUT).await {
                Ok(w) => w,
                Err(e) => {
                    warn!("[{}] peak check: {e}", t.name);
                    continue;
                }
            };
        let Some(peak) = mean_peak_index(&waveforms, t.find_max) else {
            warn!("[{}] peak check: no usable waveform data", t.name);
            continue;
        };
        last_checked = Some((low, high));

        let misaligned = peak < low || peak > high;
        let changed = {
            let mut s = state.write().await;
            match s.devices.get_mut(t.index) {
                Some(d) if d.peak_misaligned != misaligned => {
                    d.peak_misaligned = misaligned;
                    true
                }
                _ => false,
            }
        };
        if changed {
            send_message(
                &broadcaster,
                &ServerMessage::PeakAlignment {
                    device: t.name.clone(),
                    misaligned,
                },
            );
            if misaligned {
                broadcast_notification(
                    &broadcaster,
                    NotificationLevel::Warning,
                    format!(
                        "Peak window misaligned for {}: digitizer peak at sample {peak:.1}, \
                         window [{low:.0}, {high:.0}] — run Sweep Timing",
                        t.name
                    ),
                    Some(t.name.clone()),
                );
            }
        }
    }
}
