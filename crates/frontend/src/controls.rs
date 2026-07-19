use shared::messages::{ClientMessage, DeviceStatus, DeviceType, Stats};

use crate::app::{BufferState, DeviceChart, DisplayFilter, YAxisScale, YAxisState};
use crate::util::{glyph, hms, status_color};

/// A coloured status dot with a context-dependent hover explanation.
fn status_dot(ui: &mut egui::Ui, prefix: &str, ok: bool, tip_ok: &str, tip_bad: &str) {
    ui.colored_label(status_color(ok), format!("{prefix}{}", glyph::STATUS_DOT))
        .on_hover_text(if ok { tip_ok } else { tip_bad });
}

/// A labelled button that pushes a lazily-built message when clicked.
fn action_button(
    ui: &mut egui::Ui,
    label: &str,
    tip: &str,
    out_msgs: &mut Vec<ClientMessage>,
    make_msg: impl FnOnce() -> ClientMessage,
) {
    if ui.button(label).on_hover_text(tip).clicked() {
        out_msgs.push(make_msg());
    }
}

/// Draw filter controls for device types and individual devices
pub fn draw_filter_controls(
    ui: &mut egui::Ui,
    devices: &[DeviceStatus],
    filter: &mut DisplayFilter,
) {
    ui.label(egui::RichText::new("Filters").strong().size(13.0));

    ui.horizontal(|ui: &mut egui::Ui| {
        ui.checkbox(&mut filter.show_wcm, "WCM");
        ui.checkbox(&mut filter.show_dq, "DQ");
        ui.checkbox(&mut filter.show_fcup, "FCUP");
        ui.checkbox(&mut filter.show_ict, "ICT");
    });

    egui::CollapsingHeader::new("Individual Devices")
        .default_open(false)
        .show(ui, |ui: &mut egui::Ui| {
            for device in devices {
                let mut visible = !filter.hidden_devices.contains(&device.name);
                if ui.checkbox(&mut visible, &device.name).changed() {
                    if visible {
                        filter.hidden_devices.remove(&device.name);
                    } else {
                        filter.hidden_devices.insert(device.name.clone());
                    }
                }
            }
        });
}

/// Draw controls for a single device. Returns `true` if the user reordered devices
/// (via the Up/Dn buttons), so the caller can broadcast the new order.
pub fn draw_device_controls(
    ui: &mut egui::Ui,
    device: &DeviceStatus,
    out_msgs: &mut Vec<ClientMessage>,
    index: usize,
    total: usize,
    device_order: &mut [String],
) -> bool {
    let mut reordered = false;
    ui.group(|ui: &mut egui::Ui| {
        ui.horizontal(|ui: &mut egui::Ui| {
            status_dot(
                ui,
                "E",
                device.connected,
                "EPICS Channel Access: receiving data",
                "EPICS Channel Access: no data",
            );
            // ICTs have no front-end box, so there is no reachability to show.
            if device.device_type != DeviceType::Ict {
                status_dot(
                    ui,
                    "FE",
                    device.fe_alive,
                    "Front-end hardware box: reachable",
                    "Front-end hardware box: unreachable",
                );
            }
            ui.label(egui::RichText::new(&device.name).strong().size(13.0));
            ui.label(format!("({:?})", device.device_type));
            if device.last_data_time > 0.0 {
                ui.label(
                    egui::RichText::new(format!("Last: {}", hms(device.last_data_time)))
                        .size(10.0)
                        .weak(),
                );
            }

            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui: &mut egui::Ui| {
                    if index + 1 < total
                        && ui
                            .small_button("Dn")
                            .on_hover_text("Move device down in display order")
                            .clicked()
                    {
                        device_order.swap(index, index + 1);
                        reordered = true;
                    }
                    if index > 0
                        && ui
                            .small_button("Up")
                            .on_hover_text("Move device up in display order")
                            .clicked()
                    {
                        device_order.swap(index, index - 1);
                        reordered = true;
                    }
                },
            );
        });

        // Sensitivity selector (only for devices with sensitivities). When the sensitivity
        // was changed outside this program the calibration factors may be stale, so we
        // highlight the whole row with an orange band + solid selected button; re-clicking
        // the selected level re-applies it (and its config calibration factors), clearing it.
        if !device.sensitivities.is_empty() {
            let mismatch = device.calibration_mismatch;
            let tooltip = "Sensitivity was changed outside this program — its calibration \
                           factors may no longer match the config. Click the selected level \
                           to re-apply them and clear this warning.";
            // Transparent frame normally (no layout shift); an orange band when mismatched.
            let frame = if mismatch {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(120, 60, 0))
                    .inner_margin(egui::Margin::symmetric(6, 3))
                    .corner_radius(4u8)
            } else {
                egui::Frame::NONE
            };
            frame.show(ui, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    let label = egui::RichText::new("Sensitivity:");
                    let resp = ui.label(if mismatch {
                        label.color(egui::Color32::ORANGE).strong()
                    } else {
                        label
                    });
                    if mismatch {
                        resp.on_hover_text(tooltip);
                    }
                    for (i, sens) in device.sensitivities.iter().enumerate() {
                        let selected = i == device.current_sensitivity;
                        // Loud solid-orange button for the active level while mismatched.
                        let resp = if mismatch && selected {
                            ui.add(
                                egui::Button::new(
                                    egui::RichText::new(format!("FB{sens}"))
                                        .color(egui::Color32::BLACK)
                                        .strong(),
                                )
                                .fill(egui::Color32::ORANGE),
                            )
                        } else {
                            ui.selectable_label(selected, format!("FB{sens}"))
                        };
                        let resp = if mismatch {
                            resp.on_hover_text(tooltip)
                        } else {
                            resp
                        };
                        // No `!selected` guard: clicking the current level re-applies it.
                        if resp.clicked() {
                            out_msgs.push(ClientMessage::SetSensitivity {
                                device: device.name.clone(),
                                index: i,
                            });
                        }
                    }
                });
            });
        }

        ui.horizontal(|ui: &mut egui::Ui| {
            // Device-specific buttons (ICTs only get Restore Defaults)
            if device.device_type == DeviceType::Wcm {
                action_button(
                    ui,
                    "Zero WCM",
                    "Zero the WCM offset (corrB). Beam must be OFF but RF must be ON.",
                    out_msgs,
                    || ClientMessage::ZeroWCM {
                        device: device.name.clone(),
                    },
                );
            }

            // Sweep timing needs a digitizer peak window: not applicable to DQ or ICT.
            if device.device_type != DeviceType::Dq && device.device_type != DeviceType::Ict {
                action_button(
                    ui,
                    "Sweep Timing",
                    "Sweep timing window to find optimal peak. Beam must be ON the device.",
                    out_msgs,
                    || ClientMessage::SweepTiming {
                        device: device.name.clone(),
                    },
                );
            }

            action_button(
                ui,
                "Restore Defaults",
                &restore_defaults_tooltip(device),
                out_msgs,
                || ClientMessage::RestoreDefaults {
                    device: device.name.clone(),
                },
            );

            action_button(
                ui,
                "Clear",
                "Empty this device's rolling data buffer",
                out_msgs,
                || ClientMessage::ClearBuffer {
                    device: Some(device.name.clone()),
                },
            );
        });
    });
    reordered
}

/// Build the "Restore Defaults" hover tooltip listing each PV's default value.
fn restore_defaults_tooltip(device: &DeviceStatus) -> String {
    if device.defaults.is_empty() {
        return "Restore all PV defaults for this device".to_string();
    }
    let mut lines: Vec<String> = device
        .defaults
        .iter()
        .filter(|(k, _)| k.as_str() != "charge")
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    lines.sort();
    format!("Restore defaults:\n{}", lines.join("\n"))
}

/// Draw global controls
#[allow(clippy::too_many_arguments)] // a UI draw fn: every arg is one widget's state
pub fn draw_global_controls(
    ui: &mut egui::Ui,
    buffer: &mut BufferState,
    out_msgs: &mut Vec<ClientMessage>,
    frozen_stats: &mut Option<Vec<(String, Stats)>>,
    charts: &[DeviceChart],
    y_axis: &mut YAxisState,
    reset_progress: Option<(u32, u32)>,
    auto_gain: bool,
) {
    let YAxisState {
        scale: y_scale,
        min_str: y_min_str,
        max_str: y_max_str,
    } = y_axis;
    ui.horizontal(|ui: &mut egui::Ui| {
        if ui.button("Clear Calibration (All)").clicked() {
            out_msgs.push(ClientMessage::ClearCalibration);
        }
        // While a reset runs the button becomes its own progress bar, so there is nothing
        // left to click twice.
        match reset_progress {
            Some((remaining, total)) => {
                let done = (total.saturating_sub(remaining)) as f32 / total.max(1) as f32;
                ui.add(
                    egui::ProgressBar::new(done)
                        .desired_width(220.0)
                        .text(format!("Resetting front ends... {remaining}s")),
                );
            }
            None => {
                if ui
                    .button("Reset Front Ends")
                    .on_hover_text(
                        "Cut the front-end trigger for 65s to reboot the PICs, \
                         then re-apply every device's sensitivity",
                    )
                    .clicked()
                {
                    out_msgs.push(ClientMessage::ResetFrontEnds);
                }
            }
        }
        if ui
            .button("Clear Data (All)")
            .on_hover_text("Empty the rolling data buffers for all devices")
            .clicked()
        {
            out_msgs.push(ClientMessage::ClearBuffer { device: None });
        }
        ui.separator();
        ui.label("Buffer:");
        let response = ui.add(egui::TextEdit::singleline(&mut buffer.input).desired_width(60.0));
        if response.lost_focus() {
            if let Ok(new_size) = buffer.input.parse::<usize>() {
                if new_size != buffer.size {
                    buffer.size = new_size;
                    out_msgs.push(ClientMessage::SetBufferSize { size: new_size });
                }
            } else {
                // Reset to current value on invalid input
                buffer.input = buffer.size.to_string();
            }
        }
        ui.separator();
        // Server-side setting shared by all clients, so the checkbox edits a local
        // copy and the real state arrives back via AutoGainChanged.
        let mut auto_gain_ui = auto_gain;
        if ui
            .checkbox(&mut auto_gain_ui, "Auto gain")
            .on_hover_text(
                "Automatically switch a saturating FCUP/WCM to a less sensitive level \
                 when its rolling average exceeds the saturation limit",
            )
            .changed()
        {
            out_msgs.push(ClientMessage::SetAutoGain {
                enabled: auto_gain_ui,
            });
        }
        ui.separator();
        let is_frozen = frozen_stats.is_some();
        if ui
            .button(if is_frozen {
                "Unfreeze Stats"
            } else {
                "Freeze Stats"
            })
            .on_hover_text(if is_frozen {
                "Resume live statistics updates"
            } else {
                "Snapshot current statistics for recording"
            })
            .clicked()
        {
            if is_frozen {
                *frozen_stats = None;
            } else {
                *frozen_stats = Some(
                    charts
                        .iter()
                        .map(|c| (c.name.clone(), c.stats.clone()))
                        .collect(),
                );
            }
        }
    });

    // Y-axis scale controls
    ui.horizontal(|ui: &mut egui::Ui| {
        ui.label("Y Axis:");

        let current_label = match y_scale {
            YAxisScale::Auto => "Auto",
            YAxisScale::ZeroBased => "Zero-based",
            YAxisScale::Manual { .. } => "Manual",
        };

        egui::ComboBox::from_id_salt("y_axis_scale")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(matches!(y_scale, YAxisScale::Auto), "Auto")
                    .clicked()
                {
                    *y_scale = YAxisScale::Auto;
                }
                if ui
                    .selectable_label(matches!(y_scale, YAxisScale::ZeroBased), "Zero-based")
                    .clicked()
                {
                    *y_scale = YAxisScale::ZeroBased;
                }
                if ui
                    .selectable_label(matches!(y_scale, YAxisScale::Manual { .. }), "Manual")
                    .clicked()
                    && !matches!(y_scale, YAxisScale::Manual { .. })
                {
                    *y_scale = YAxisScale::Manual {
                        min: 0.0,
                        max: 100.0,
                    };
                    *y_min_str = "0".to_string();
                    *y_max_str = "100".to_string();
                }
            });

        if let YAxisScale::Manual { min, max } = y_scale {
            ui.label("Min:");
            let min_resp = ui.add(egui::TextEdit::singleline(y_min_str).desired_width(50.0));
            ui.label("Max:");
            let max_resp = ui.add(egui::TextEdit::singleline(y_max_str).desired_width(50.0));

            if min_resp.lost_focus() || max_resp.lost_focus() {
                if let (Ok(new_min), Ok(new_max)) =
                    (y_min_str.parse::<f64>(), y_max_str.parse::<f64>())
                {
                    if new_min < new_max {
                        *min = new_min;
                        *max = new_max;
                    }
                }
            }
        }
    });
}
