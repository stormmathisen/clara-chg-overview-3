//! The FCUP calibration-sweep window: pick cups, sensitivities, step and shots; watch
//! progress; inspect each fit; apply the result or go back to the config.

use std::collections::{BTreeMap, BTreeSet};

use shared::calibration::{fit_offset, fit_origin, prefer_offset, Fit};
use shared::messages::{
    CalibrationPoint, CalibrationValue, ClientMessage, DeviceStatus, DeviceType,
};

/// Kept while the window is closed, so results survive reopening.
pub struct CalibrationDialog {
    pub open: bool,
    /// DAC levels between measurements.
    step: u8,
    /// Shots averaged per level.
    shots: u8,
    /// FCUPs ticked for calibration.
    devices: BTreeSet<String>,
    /// FB levels NOT ticked (so every level starts ticked).
    skipped: BTreeSet<u8>,
    /// FCUPs with a sweep in flight -> (fraction done, estimated seconds left).
    running: BTreeMap<String, (f32, f32)>,
    results: BTreeMap<String, Vec<CalibrationPoint>>,
    /// Operator's fit-model choice per (FCUP, FB): true = with offset. Unset ranges use
    /// `prefer_offset`.
    offset: BTreeMap<(String, u8), bool>,
    /// (FCUP, FB level) whose fit is plotted.
    plotted: Option<(String, u8)>,
}

impl Default for CalibrationDialog {
    fn default() -> Self {
        Self {
            open: false,
            step: 20,
            shots: 25,
            devices: BTreeSet::new(),
            skipped: BTreeSet::new(),
            running: BTreeMap::new(),
            results: BTreeMap::new(),
            offset: BTreeMap::new(),
            plotted: None,
        }
    }
}

impl CalibrationDialog {
    pub fn on_progress(&mut self, device: String, fraction: f32, remaining_secs: f32) {
        self.running.insert(device, (fraction, remaining_secs));
    }

    pub fn on_result(&mut self, device: String, results: Vec<CalibrationPoint>) {
        self.running.remove(&device);
        self.offset.retain(|(d, _), _| *d != device);
        // Show the first fit straight away if nothing is plotted yet.
        if self.plotted.is_none() {
            self.plotted = results.first().map(|r| (device.clone(), r.sensitivity));
        }
        self.results.insert(device, results);
    }

    /// The fit in use for a range and whether it is the offset model.
    fn fit(&self, device: &str, r: &CalibrationPoint) -> (Option<Fit>, bool) {
        let (origin, offset) = (fit_origin(&r.levels), fit_offset(&r.levels));
        let with_offset = self
            .offset
            .get(&(device.to_string(), r.sensitivity))
            .copied()
            .unwrap_or_else(|| prefer_offset(origin, offset));
        (if with_offset { offset } else { origin }, with_offset)
    }

    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        devices: &[DeviceStatus],
        out_msgs: &mut Vec<ClientMessage>,
    ) {
        let fcups: Vec<&DeviceStatus> = devices
            .iter()
            .filter(|d| d.device_type == DeviceType::Fcup)
            .collect();
        let mut open = self.open;
        egui::Window::new("Calibration sweep")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui: &mut egui::Ui| {
                ui.label(
                    "Puts each ticked Faraday cup's front end in calibration mode and sweeps \
                     the cal level 0–255 at each ticked sensitivity, measuring the waveform's \
                     baseline → negative peak. The box's settings are restored afterwards \
                     (also on cancel). Charge readings from a cup are cal pulses while it runs.",
                );
                ui.add_space(4.0);
                self.draw_setup(ui, &fcups, out_msgs);
                self.draw_results(ui, &fcups, out_msgs);
            });
        // Closing hides the window; running sweeps carry on server-side.
        self.open = open;
    }

    fn draw_setup(
        &mut self,
        ui: &mut egui::Ui,
        fcups: &[&DeviceStatus],
        out_msgs: &mut Vec<ClientMessage>,
    ) {
        ui.label("Faraday cups:").on_hover_text(
            "Cups to sweep. Several can run at once; each has its own front end and digitizer.",
        );
        for d in fcups {
            ui.horizontal(|ui: &mut egui::Ui| {
                let mut ticked = self.devices.contains(&d.name);
                let running = self.running.get(&d.name).copied();
                if ui
                    .add_enabled(running.is_none(), egui::Checkbox::new(&mut ticked, &d.name))
                    .on_hover_text(
                        "HTTP front ends are restored to their exact previous settings; older \
                         (legacy) boxes can't be read back and return to normal operation at \
                         the selected sensitivity.",
                    )
                    .on_disabled_hover_text("Sweep running")
                    .changed()
                {
                    if ticked {
                        self.devices.insert(d.name.clone());
                    } else {
                        self.devices.remove(&d.name);
                    }
                }
                if let Some((fraction, remaining)) = running {
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(220.0)
                            .text(format!(
                                "{:.0} % · ~{remaining:.0} s left",
                                fraction * 100.0
                            )),
                    )
                    .on_hover_text(
                        "Levels measured out of those planned; a range that saturates early \
                         drops its remaining levels. Time left is from the pace so far.",
                    );
                    if ui
                        .button("Cancel")
                        .on_hover_text("Stop and restore the front end's settings")
                        .clicked()
                    {
                        out_msgs.push(ClientMessage::CancelCalibration {
                            device: d.name.clone(),
                        });
                    }
                } else if !d.calibration_overrides.is_empty() {
                    let fbs: Vec<String> = d
                        .calibration_overrides
                        .iter()
                        .map(|v| format!("FB{}", v.sensitivity))
                        .collect();
                    ui.weak(format!("applied calibration: {}", fbs.join(", ")))
                        .on_hover_text(
                            "These ranges use sweep constants instead of the config. \
                             Restore default calibration below reverts them.",
                        );
                }
            });
        }

        let levels: BTreeSet<u8> = fcups
            .iter()
            .flat_map(|d| d.sensitivities.iter().copied())
            .collect();
        ui.horizontal(|ui: &mut egui::Ui| {
            ui.label("Sensitivities:");
            for fb in levels {
                let mut ticked = !self.skipped.contains(&fb);
                // Mirrors the server's reference table (calibration::reference_for).
                let (reference, vref) = match fb {
                    0..=2 => ("REF500mV", 0.5),
                    3 => ("REF2048mV", 2.048),
                    _ => ("REF4096mV", 4.096),
                };
                if ui
                    .checkbox(&mut ticked, format!("FB{fb}"))
                    .on_hover_text(format!(
                        "Sweep FB{fb} with the {reference} cal reference \
                         (level 0–255 → 0–{:.0} pC at 66 pF)",
                        vref * 66.0
                    ))
                    .changed()
                {
                    if ticked {
                        self.skipped.remove(&fb);
                    } else {
                        self.skipped.insert(fb);
                    }
                }
            }
        });

        ui.horizontal(|ui: &mut egui::Ui| {
            let step_tip = "Cal DAC levels between measurements (0, step, 2·step, … 255). \
                            Smaller = more points in the fit, longer sweep.";
            ui.label("Level step:").on_hover_text(step_tip);
            ui.add(egui::DragValue::new(&mut self.step).range(1..=64))
                .on_hover_text(step_tip);
            let shots_tip = "Digitizer shots (10 Hz) averaged per level; their spread gives \
                             the ±1σ error bars. One shot after each change is always skipped.";
            ui.label("Shots per level:").on_hover_text(shots_tip);
            ui.add(egui::DragValue::new(&mut self.shots).range(1..=50))
                .on_hover_text(shots_tip);
            ui.weak(format!(
                "≈ {:.0} s per sensitivity (less if it saturates)",
                self.secs_per_range()
            ))
            .on_hover_text(
                "Estimate. A range stops at its first saturated level, so the sensitive \
                 ranges usually finish early.",
            );
        });

        // Ticked, idle cups with the ticked levels as indices into their own list.
        let jobs: Vec<(String, Vec<usize>)> = fcups
            .iter()
            .filter(|d| self.devices.contains(&d.name) && !self.running.contains_key(&d.name))
            .map(|d| {
                let idx = (0..d.sensitivities.len())
                    .filter(|&i| !self.skipped.contains(&d.sensitivities[i]))
                    .collect::<Vec<_>>();
                (d.name.clone(), idx)
            })
            .filter(|(_, idx)| !idx.is_empty())
            .collect();
        ui.horizontal(|ui: &mut egui::Ui| {
            if ui
                .add_enabled(!jobs.is_empty(), egui::Button::new("Start sweep"))
                .on_hover_text(
                    "Start a sweep on every ticked idle cup. Charge readings from those cups \
                     are cal pulses until it finishes.",
                )
                .on_disabled_hover_text("Tick at least one idle cup and one sensitivity")
                .clicked()
            {
                for (device, sensitivities) in jobs {
                    let estimate = self.secs_per_range() * sensitivities.len() as f32;
                    self.running.insert(device.clone(), (0.0, estimate));
                    out_msgs.push(ClientMessage::CalibrationSweep {
                        device,
                        sensitivities,
                        step: self.step,
                        shots: self.shots,
                    });
                }
            }
            if self.running.len() > 1
                && ui
                    .button("Cancel all")
                    .on_hover_text("Stop every running sweep and restore the front ends")
                    .clicked()
            {
                for device in self.running.keys() {
                    out_msgs.push(ClientMessage::CancelCalibration {
                        device: device.clone(),
                    });
                }
            }
        });
    }

    /// One skipped + `shots` measured shots at 10 Hz, plus ~0.15 s of writes, per level.
    fn secs_per_range(&self) -> f32 {
        let n_levels = 255 / self.step as usize + 1;
        n_levels as f32 * ((self.shots as f32 + 1.0) * 0.1 + 0.15)
    }

    fn draw_results(
        &mut self,
        ui: &mut egui::Ui,
        fcups: &[&DeviceStatus],
        out_msgs: &mut Vec<ClientMessage>,
    ) {
        // Cups with results or an applied calibration get Apply / Restore buttons.
        let listed: Vec<&&DeviceStatus> = fcups
            .iter()
            .filter(|d| self.results.contains_key(&d.name) || !d.calibration_overrides.is_empty())
            .collect();
        if listed.is_empty() {
            return;
        }
        ui.separator();
        let mut toggles = Vec::new();
        egui::Grid::new("calibration_results")
            .striped(true)
            .show(ui, |ui: &mut egui::Ui| {
                for (h, tip) in [
                    ("FCUP", "Click a cup to plot that range's fit"),
                    (
                        "Range",
                        "Integrator feedback (sensitivity); FB0 is the most sensitive",
                    ),
                    (
                        "Ref",
                        "Cal reference: injected Q = Vref · level / 255 · 66 pF",
                    ),
                    (
                        "Offset",
                        "Ticked: fit Q = corrA·A − corrB. Unticked: fit through zero \
                         (corrB = 0). Defaults to the offset fit where it at least halves \
                         the RMS error (typically FB0/FB1); through zero is better where the \
                         response curves at high charge (FB3/FB4).",
                    ),
                    (
                        "corrA now",
                        "corrA in use when the sweep ran (applied calibration, else config)",
                    ),
                    (
                        "corrA new",
                        "Fitted corrA, pC per ADC count (select to copy)",
                    ),
                    ("Change", "corrA new relative to corrA now"),
                    (
                        "corrB new",
                        "Fitted corrB in pC, subtracted by the IOC (0 for the through-zero fit)",
                    ),
                    (
                        "RMS error",
                        "RMS of (fitted − nominal) charge over the levels used in the fit",
                    ),
                    (
                        "Saturated at",
                        "First cal level where the waveform clipped or hit an ADC rail; the \
                         sweep stopped there and the level before it is left out of the fit",
                    ),
                ] {
                    ui.strong(h).on_hover_text(tip);
                }
                ui.end_row();
                for (device, results) in &self.results {
                    for r in results {
                        let key = (device.clone(), r.sensitivity);
                        let shown = self.plotted.as_ref() == Some(&key);
                        if ui
                            .selectable_label(shown, device)
                            .on_hover_text("Click to plot this fit")
                            .clicked()
                        {
                            self.plotted = Some(key.clone());
                        }
                        ui.label(format!("FB{}", r.sensitivity));
                        ui.label(&r.reference);
                        let (fit, mut with_offset) = self.fit(device, r);
                        if ui
                            .checkbox(&mut with_offset, "")
                            .on_hover_text(
                                "Fit with an offset (writes corrB) instead of through zero",
                            )
                            .changed()
                        {
                            toggles.push((key, with_offset));
                        }
                        ui.label(r.corr_a_old.map_or("—".into(), |v| format!("{v:.4e}")));
                        match (fit, &r.error) {
                            (Some(f), None) => {
                                // Selectable so values can be copied.
                                ui.add(
                                    egui::Label::new(format!("{:.4e}", f.corr_a)).selectable(true),
                                );
                                ui.label(r.corr_a_old.map_or("—".into(), |old| {
                                    format!("{:+.1} %", (f.corr_a - old) / old * 100.0)
                                }));
                                ui.add(
                                    egui::Label::new(format!("{:+.3} pC", f.corr_b))
                                        .selectable(true),
                                );
                                ui.label(format!("{:.3} pC", f.rms_pc));
                                ui.label(
                                    r.saturated_at
                                        .map_or("not reached".into(), |l| l.to_string()),
                                );
                            }
                            (_, err) => {
                                ui.colored_label(
                                    egui::Color32::LIGHT_RED,
                                    err.as_deref().unwrap_or("no fit"),
                                );
                            }
                        }
                        ui.end_row();
                    }
                }
            });
        for (key, on) in toggles {
            self.offset.insert(key, on);
        }

        ui.add_space(4.0);
        for d in listed {
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.label(&d.name);
                let values: Vec<CalibrationValue> = self
                    .results
                    .get(&d.name)
                    .into_iter()
                    .flatten()
                    .filter(|r| r.error.is_none())
                    .filter_map(|r| {
                        let f = self.fit(&d.name, r).0?;
                        Some(CalibrationValue {
                            sensitivity: r.sensitivity,
                            corr_a: f.corr_a,
                            corr_b: f.corr_b,
                        })
                    })
                    .collect();
                let tip = values
                    .iter()
                    .map(|v| {
                        format!(
                            "FB{}: corrA {:.4e}, corrB {:+.3} pC",
                            v.sensitivity, v.corr_a, v.corr_b
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if ui
                    .add_enabled(!values.is_empty(), egui::Button::new("Apply"))
                    .on_disabled_hover_text("No fitted ranges for this cup")
                    .on_hover_text(format!(
                        "Use these instead of the config (persisted):\n{tip}"
                    ))
                    .clicked()
                {
                    out_msgs.push(ClientMessage::ApplyCalibration {
                        device: d.name.clone(),
                        values,
                    });
                }
                if ui
                    .add_enabled(
                        !d.calibration_overrides.is_empty(),
                        egui::Button::new("Restore default calibration"),
                    )
                    .on_disabled_hover_text("Already using the config's calibration")
                    .on_hover_text(
                        "Drop applied calibrations and go back to the config's corrA/corrB",
                    )
                    .clicked()
                {
                    out_msgs.push(ClientMessage::RestoreDefaultCalibration {
                        device: d.name.clone(),
                    });
                }
            });
        }

        let plotted = self.plotted.as_ref().and_then(|(dev, fb)| {
            let r = self
                .results
                .get(dev)?
                .iter()
                .find(|r| r.sensitivity == *fb)?;
            Some((dev, r))
        });
        if let Some((dev, r)) = plotted {
            ui.separator();
            draw_plot(ui, dev, r, self.fit(dev, r).0);
        }
    }
}

/// Pulse amplitude against nominal injected charge for one range: measured levels with
/// ±1σ error bars (grey = not used in the fit), the chosen fit and the constants in use.
fn draw_plot(ui: &mut egui::Ui, device: &str, r: &CalibrationPoint, fit: Option<Fit>) {
    use egui_plot::{Legend, Line, LineStyle, Plot, PlotPoints, Points};
    let used = egui::Color32::LIGHT_BLUE;
    let unused = egui::Color32::GRAY;
    let q_max = r.levels.iter().map(|l| l.charge).fold(0.0, f64::max);
    ui.label(format!(
        "{device} FB{} ({}): amplitude vs nominal charge, ±1σ",
        r.sensitivity, r.reference
    ))
    .on_hover_text(
        "Blue: levels used in the fit; grey: not used (no pulse, saturated, or the level \
         before saturation). Bars are ±1σ over the shots. Green: the selected fit. Dashed \
         orange: the calibration in use when the sweep ran. Scroll to zoom, drag to pan, \
         double-click to reset.",
    );
    // Amplitudes are negative-going; plot magnitudes so the lines rise left to right.
    // Q = corrA·A − corrB  ⇒  |A| = −(Q + corrB) / corrA.
    let line = |corr_a: f64, corr_b: f64| {
        let amp = |q: f64| -(q + corr_b) / corr_a;
        PlotPoints::from(vec![[0.0, amp(0.0)], [q_max, amp(q_max)]])
    };
    Plot::new("calibration_fit")
        .height(300.0)
        .width(640.0)
        .legend(Legend::default())
        .x_axis_label("Nominal charge (pC)")
        .y_axis_label("|Amplitude| (counts)")
        .show(ui, |plot_ui| {
            for (in_fit, color, name) in [(true, used, "Used in fit"), (false, unused, "Not used")]
            {
                let pts: Vec<[f64; 2]> = r
                    .levels
                    .iter()
                    .filter(|l| l.used == in_fit)
                    .map(|l| [l.charge, -l.mean])
                    .collect();
                plot_ui.points(Points::new(pts).radius(2.5_f32).color(color).name(name));
            }
            for l in r.levels.iter().filter(|l| l.sigma > 0.0) {
                let color = if l.used { used } else { unused };
                let bar = vec![[l.charge, -l.mean - l.sigma], [l.charge, -l.mean + l.sigma]];
                plot_ui.line(Line::new(PlotPoints::from(bar)).color(color));
            }
            if let Some(f) = fit {
                plot_ui.line(
                    Line::new(line(f.corr_a, f.corr_b))
                        .color(egui::Color32::GREEN)
                        .name(format!(
                            "Fit: corrA {:.4e}, corrB {:+.3} pC",
                            f.corr_a, f.corr_b
                        )),
                );
            }
            if let Some(a) = r.corr_a_old {
                let b = r.corr_b_old.unwrap_or(0.0);
                plot_ui.line(
                    Line::new(line(a, b))
                        .color(egui::Color32::ORANGE)
                        .style(LineStyle::dashed_loose())
                        .name(format!("In use: corrA {a:.4e}, corrB {b:+.3} pC")),
                );
            }
        });
}
