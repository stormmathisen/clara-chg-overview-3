//! Faraday-cup calibration sweep.
//!
//! For each selected sensitivity the front end is put in calibration mode with a fixed
//! cal reference, and the DAC level is stepped from 0 to 255. Each step reads digitizer
//! waveforms and measures the pulse as baseline (mean of the pulse-free start of the
//! trace) → minimum (the cal pulse is negative-going). The sweep for a range stops at
//! the first saturated level. The injected charge is `Vref · level / 255 · 66 pF`; the
//! levels go to the UI, which fits corrA/corrB with `shared::calibration`.
//!
//! Sweeps on different devices run in parallel: each has its own front end, digitizer
//! subscription and task. The box's settings are read first and put back afterwards,
//! whether the sweep finished, failed or was cancelled.
//!
//! Method and thresholds follow the 2026-09-29 FCUP calibration check
//! (`docs/reports/fcup-calibration-check-2026-09-29.md`).

use std::time::Duration;

use shared::messages::{
    CalibrationLevel, CalibrationPoint, DeviceType, NotificationLevel, ServerMessage,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, info};

use crate::commands::keys;
use crate::state::AppState;
use crate::ws::{broadcast_notification, send_message, Broadcaster};
use crate::{epics, hardware};
use shared::calibration;

/// Cal-injection capacitance the `corrA` constants are defined against:
/// `Q [pC] = Vref [V] · level / 255 · 66 pF`.
const CAL_CAPACITANCE_PF: f64 = 66.0;
/// Upper bound on the operator's shots-per-level, so a typo can't hold a box in CAL for
/// hours.
const MAX_SHOTS: u8 = 50;
/// Leading samples averaged for the baseline (pulses land at ~1700–1850 of 2048).
const BASELINE_SAMPLES: usize = 500;
/// |baseline → min| above this many counts counts as a cal pulse. The noise minimum
/// reaches ~1–3k counts, the smallest useful pulse is well above.
const PULSE_COUNTS: f64 = 5000.0;
const ADC_MAX: f64 = 65535.0;
/// Within this many counts of either ADC rail = railed.
const RAIL_MARGIN: f64 = 500.0;
/// At least `FLAT_SAMPLES` samples within `FLAT_COUNTS` of the minimum = a clipped
/// (saturated) integrator; an unsaturated pulse has a rounded bottom.
const FLAT_COUNTS: f64 = 30.0;
const FLAT_SAMPLES: usize = 5;
/// Fewest pulsed levels needed for a fit.
const MIN_FIT_POINTS: usize = 3;
/// Wait for one digitizer shot (10 Hz).
const SHOT_TIMEOUT: Duration = Duration::from_secs(2);
/// After restoring the box, wait this long before handing the device back to fe_events
/// and auto gain, so the restore's SSE echo and ~1 s of normal charge readings are in.
const RELEASE_DELAY: Duration = Duration::from_secs(2);

/// Cal reference (name, volts) for a sensitivity: each keeps as many DAC levels as
/// possible below saturation.
// ponytail: fixed table from the 2026-09-29 check; move to config if boxes differ.
fn reference_for(fb: u8) -> (&'static str, f64) {
    match fb {
        0..=2 => ("REF500mV", 0.5),
        3 => ("REF2048mV", 2.048),
        _ => ("REF4096mV", 4.096),
    }
}

/// One shot's pulse amplitude (min − baseline, negative for a pulse) and whether it
/// saturated. `None` for a trace too short to analyse.
fn analyse(wf: &[f64]) -> Option<(f64, bool)> {
    let n = BASELINE_SAMPLES.min(wf.len() / 4);
    if n == 0 {
        return None;
    }
    let base = wf[..n].iter().sum::<f64>() / n as f64;
    let min = wf.iter().copied().fold(f64::INFINITY, f64::min);
    let max = wf.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let amplitude = min - base;
    let rail = min < RAIL_MARGIN || max > ADC_MAX - RAIL_MARGIN;
    // Only judge traces with a pulse: on noise the flat test trips trivially.
    let flat = wf.iter().filter(|&&v| v - min <= FLAT_COUNTS).count() >= FLAT_SAMPLES;
    Some((amplitude, -amplitude > PULSE_COUNTS && (rail || flat)))
}

async fn next_shot(rx: &mut UnboundedReceiver<Vec<f64>>) -> Result<Vec<f64>, String> {
    match tokio::time::timeout(SHOT_TIMEOUT, rx.recv()).await {
        Ok(Some(wf)) => Ok(wf),
        Ok(None) => Err("digitizer stream closed".to_string()),
        Err(_) => Err("no digitizer shot within 2 s".to_string()),
    }
}

/// Mean amplitude, its sample σ, and "majority saturated" over the next `shots` shots.
/// Shots already queued are dropped and one more is skipped, so every shot measured was
/// taken after the preceding settings write landed.
async fn measure(
    rx: &mut UnboundedReceiver<Vec<f64>>,
    shots: usize,
) -> Result<(f64, f64, bool), String> {
    while rx.try_recv().is_ok() {}
    next_shot(rx).await?;
    let mut amps = Vec::with_capacity(shots);
    let mut saturated = 0;
    for _ in 0..shots {
        let (amp, sat) = analyse(&next_shot(rx).await?).ok_or("digitizer trace too short")?;
        amps.push(amp);
        saturated += sat as usize;
    }
    let (mean, sigma) = mean_sigma(&amps);
    Ok((mean, sigma, saturated * 2 > shots))
}

/// Mean and sample standard deviation (σ = 0 for fewer than two values).
fn mean_sigma(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let mean = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    (mean, var.sqrt())
}

/// Level-count progress across all ranges of one sweep; a range that saturates early
/// drops its unmeasured levels from `total`.
struct Progress {
    done: usize,
    total: usize,
    start: std::time::Instant,
}

impl Progress {
    fn send(&self, broadcaster: &Broadcaster, device: &str) {
        let fraction = self.done as f32 / self.total.max(1) as f32;
        let per_level = self.start.elapsed().as_secs_f32() / self.done.max(1) as f32;
        send_message(
            broadcaster,
            &ServerMessage::CalibrationProgress {
                device: device.to_string(),
                fraction,
                remaining_secs: per_level * (self.total - self.done) as f32,
            },
        );
    }
}

/// Everything one range needs besides its FB level.
struct Sweep<'a> {
    device: &'a str,
    ip: &'a str,
    step: u8,
    shots: u8,
    state: &'a AppState,
    broadcaster: &'a Broadcaster,
}

impl Sweep<'_> {
    async fn cancelled(&self) -> bool {
        let s = self.state.read().await;
        s.device(self.device).is_some_and(|d| d.cancel_calibration)
    }

    /// Sweep one sensitivity, recording every measured level into `levels` (kept on error,
    /// for the plot). Returns the first saturated level, if any.
    async fn range(
        &self,
        fb: u8,
        rx: &mut UnboundedReceiver<Vec<f64>>,
        levels: &mut Vec<CalibrationLevel>,
        progress: &mut Progress,
    ) -> Result<Option<u8>, String> {
        let (reference, vref) = reference_for(fb);
        let pc_per_level = vref * CAL_CAPACITANCE_PF / 255.0;
        let mut saturated_at = None;
        for level in (0..=255u8).step_by(self.step.max(1).into()) {
            if self.cancelled().await {
                return Err("cancelled".into());
            }
            hardware::set_calibration(self.ip, fb, reference, level)
                .await
                .map_err(|e| format!("front end: {e}"))?;
            let shots = self.shots.clamp(1, MAX_SHOTS).into();
            let (mean, sigma, saturated) = measure(rx, shots).await?;
            levels.push(CalibrationLevel {
                level,
                charge: pc_per_level * level as f64,
                mean,
                sigma,
                used: !saturated && -mean > PULSE_COUNTS,
            });
            progress.done += 1;
            progress.send(self.broadcaster, self.device);
            if saturated {
                saturated_at = Some(level);
                break;
            }
        }
        // ponytail: drop the last level before saturation in case it was already
        // compressing; add a linearity check if fits still disagree with the report.
        if saturated_at.is_some() {
            if let Some(l) = levels.iter_mut().rev().find(|l| l.used) {
                l.used = false;
            }
        }
        let used = levels.iter().filter(|l| l.used).count();
        if used < MIN_FIT_POINTS {
            return Err(format!(
                "no cal pulse: only {used} levels above {PULSE_COUNTS} counts"
            ));
        }
        info!(
            "Calibration {} FB{fb} {reference}: origin {:?}, offset {:?}",
            self.device,
            calibration::fit_origin(levels),
            calibration::fit_offset(levels)
        );
        Ok(saturated_at)
    }
}

/// Run a calibration sweep in the background (tens of seconds per sensitivity, far too
/// long to block the client's WebSocket loop). The device is flagged `calibrating` for
/// the duration, and its front end is always put back the way it was found.
pub fn spawn_sweep(
    device: String,
    indices: Vec<usize>,
    step: u8,
    shots: u8,
    state: AppState,
    broadcaster: Broadcaster,
) {
    tokio::spawn(async move {
        let fail = |msg: String| {
            error!("{msg}");
            broadcast_notification(
                &broadcaster,
                NotificationLevel::Error,
                msg,
                Some(device.clone()),
            );
        };
        // (ip, waveform PV, [(fb, corrA, corrB in use)]), and claim the device.
        let (ip, pv, ranges) = {
            let mut s = state.write().await;
            let Some(d) = s.device_mut(&device) else {
                return fail(format!("Device {device} not found"));
            };
            if d.config.device_type != DeviceType::Fcup {
                return fail(format!(
                    "Calibration sweep is only for Faraday cups, not {device}"
                ));
            }
            if d.calibrating {
                return fail(format!(
                    "A calibration sweep is already running on {device}"
                ));
            }
            let ranges: Vec<(u8, Option<f64>, Option<f64>)> = indices
                .iter()
                .filter_map(|&i| {
                    Some((
                        *d.config.sensitivities.get(i)?,
                        d.default_value(keys::CORR_A, i),
                        d.default_value(keys::CORR_B, i),
                    ))
                })
                .collect();
            if ranges.is_empty() {
                return fail(format!(
                    "Calibration sweep: no valid sensitivities for {device}"
                ));
            }
            d.calibrating = true;
            d.cancel_calibration = false;
            (
                d.config.ip.clone(),
                format!("{}-READ", d.config.digitizer),
                ranges,
            )
        };
        let release = || async {
            tokio::time::sleep(RELEASE_DELAY).await;
            if let Some(d) = state.write().await.device_mut(&device) {
                d.calibrating = false;
                d.cancel_calibration = false;
            }
        };

        // Snapshot the box before touching it. Without a snapshot there is nothing safe
        // to restore to, so don't start.
        let saved = match hardware::read_settings(&ip).await {
            Ok(saved) => saved,
            Err(e) => {
                fail(format!(
                    "Calibration sweep for {device}: could not read front-end settings, \
                     not starting: {e}"
                ));
                return release().await;
            }
        };

        let sweep = Sweep {
            device: &device,
            ip: &ip,
            step,
            shots,
            state: &state,
            broadcaster: &broadcaster,
        };
        let n_levels = (0..=255u8).step_by(step.max(1).into()).count();
        let mut progress = Progress {
            done: 0,
            total: n_levels * ranges.len(),
            start: std::time::Instant::now(),
        };
        let mut results = Vec::new();
        let mut cancelled = false;
        match epics::waveform_stream(&pv).await {
            Err(e) => fail(format!("Calibration sweep for {device}: {e}")),
            Ok(mut rx) => {
                for (fb, corr_a_old, corr_b_old) in ranges {
                    let (reference, _) = reference_for(fb);
                    broadcast_notification(
                        &broadcaster,
                        NotificationLevel::Info,
                        format!("Calibrating {device} FB{fb} ({reference}, level 0–255)…"),
                        Some(device.clone()),
                    );
                    let mut levels = Vec::new();
                    let r = sweep.range(fb, &mut rx, &mut levels, &mut progress).await;
                    // Levels this range didn't measure (saturation, error) leave the total.
                    progress.total -= n_levels - levels.len();
                    progress.send(&broadcaster, &device);
                    let (saturated_at, error) = match r {
                        Ok(sat) => (sat, None),
                        Err(e) => (None, Some(e)),
                    };
                    cancelled = error.as_deref() == Some("cancelled");
                    results.push(CalibrationPoint {
                        sensitivity: fb,
                        reference: reference.to_string(),
                        corr_a_old,
                        corr_b_old,
                        saturated_at,
                        error,
                        levels,
                    });
                    if cancelled {
                        break;
                    }
                }
            }
        }

        // Put the box back: the exact snapshot on an HTTP box, else (legacy, no read-back)
        // normal operation at the operator's selected sensitivity.
        let restored = match &saved {
            Some(saved) => hardware::restore_settings(&ip, saved).await,
            None => {
                let level = {
                    let s = state.read().await;
                    s.device(&device)
                        .and_then(|d| d.config.sensitivities.get(d.current_sensitivity).copied())
                };
                match level {
                    Some(level) => hardware::clear_calibration(&ip, level).await,
                    None => Err(anyhow::anyhow!("no selected sensitivity")),
                }
            }
        };
        if let Err(e) = restored {
            fail(format!(
                "{device} MAY STILL BE IN CALIBRATION MODE — restore failed: {e}"
            ));
        }
        release().await;

        let ok = results.iter().filter(|r| r.error.is_none()).count();
        let (level, msg) = if cancelled {
            (
                NotificationLevel::Warning,
                format!("Calibration sweep for {device} cancelled; front end restored"),
            )
        } else {
            (
                if ok == results.len() {
                    NotificationLevel::Success
                } else {
                    NotificationLevel::Warning
                },
                format!(
                    "Calibration sweep for {device} done: {ok}/{} ranges measured",
                    results.len()
                ),
            )
        };
        broadcast_notification(&broadcaster, level, msg, Some(device.clone()));
        send_message(
            &broadcaster,
            &ServerMessage::CalibrationResult { device, results },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2048-sample trace at baseline 54000 with a negative pulse of `depth` counts,
    /// rounded (parabolic) or clipped flat at the bottom.
    fn trace(depth: f64, clipped: bool) -> Vec<f64> {
        (0..2048)
            .map(|i| {
                let x = (i as f64 - 1750.0) / 20.0;
                let shape = (1.0 - x * x).max(0.0);
                let shape = if clipped {
                    (shape * 2.0).min(1.0)
                } else {
                    shape
                };
                54000.0 - depth * shape
            })
            .collect()
    }

    #[test]
    fn analyse_measures_baseline_to_min() {
        let (amp, sat) = analyse(&trace(18000.0, false)).unwrap();
        assert!((amp + 18000.0).abs() < 1.0, "{amp}");
        assert!(!sat);
    }

    #[test]
    fn analyse_flags_clipped_and_railed_pulses_only() {
        assert!(analyse(&trace(18000.0, true)).unwrap().1, "flat bottom");
        assert!(
            analyse(&trace(53900.0, false)).unwrap().1,
            "near the 0 rail"
        );
        // A flat noise-free trace has no pulse, so it is never "saturated".
        assert!(!analyse(&vec![54000.0; 2048]).unwrap().1);
        assert!(analyse(&[1.0, 2.0]).is_none());
    }

    #[test]
    fn mean_sigma_is_sample_std() {
        assert_eq!(mean_sigma(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]).0, 5.0);
        let (_, s) = mean_sigma(&[1.0, 3.0]);
        assert!((s - 2f64.sqrt()).abs() < 1e-12);
        assert_eq!(mean_sigma(&[7.0]), (7.0, 0.0));
    }
}
