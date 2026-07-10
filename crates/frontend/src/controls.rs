use shared::messages::{ChartSnapshot, ClientMessage, DeviceStatus, DeviceType, Stats};

use crate::app::{DisplayFilter, YAxisScale};

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

/// Draw controls for a single device
pub fn draw_device_controls(
    ui: &mut egui::Ui,
    device: &DeviceStatus,
    out_msgs: &mut Vec<ClientMessage>,
    index: usize,
    total: usize,
    device_order: &mut Vec<String>,
) {
    ui.group(|ui: &mut egui::Ui| {
        ui.horizontal(|ui: &mut egui::Ui| {
            let epics_color = if device.connected {
                egui::Color32::GREEN
            } else {
                egui::Color32::RED
            };
            ui.colored_label(epics_color, "E●")
                .on_hover_text(if device.connected {
                    "EPICS Channel Access: receiving data"
                } else {
                    "EPICS Channel Access: no data"
                });
            if device.device_type != DeviceType::Ict {
                let fe_color = if device.fe_alive {
                    egui::Color32::GREEN
                } else {
                    egui::Color32::RED
                };
                ui.colored_label(fe_color, "FE●")
                    .on_hover_text(if device.fe_alive {
                        "Front-end hardware box: reachable"
                    } else {
                        "Front-end hardware box: unreachable"
                    });
            }
            ui.label(egui::RichText::new(&device.name).strong().size(13.0));
            ui.label(format!("({:?})", device.device_type));
            if device.last_data_time > 0.0 {
                let secs = device.last_data_time as i64;
                let total = secs.rem_euclid(86400);
                let h = total / 3600;
                let m = (total % 3600) / 60;
                let s = total % 60;
                ui.label(
                    egui::RichText::new(format!("Last: {h:02}:{m:02}:{s:02}"))
                        .size(10.0)
                        .weak(),
                );
            }

            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui: &mut egui::Ui| {
                    if index + 1 < total {
                        if ui
                            .small_button("Dn")
                            .on_hover_text("Move device down in display order")
                            .clicked()
                        {
                            device_order.swap(index, index + 1);
                        }
                    }
                    if index > 0 {
                        if ui
                            .small_button("Up")
                            .on_hover_text("Move device up in display order")
                            .clicked()
                        {
                            device_order.swap(index, index - 1);
                        }
                    }
                },
            );
        });

        // Sensitivity selector (only for devices with sensitivities)
        if !device.sensitivities.is_empty() {
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.label("Sensitivity:");
                for (i, sens) in device.sensitivities.iter().enumerate() {
                    let selected = i == device.current_sensitivity;
                    let label = format!("FB{sens}");
                    if ui.selectable_label(selected, &label).clicked() && !selected {
                        out_msgs.push(ClientMessage::SetSensitivity {
                            device: device.name.clone(),
                            index: i,
                        });
                    }
                }
            });
        }

        ui.horizontal(|ui: &mut egui::Ui| {
            // Device-specific buttons (ICTs only get Restore Defaults)
            if device.device_type == DeviceType::Wcm {
                if ui
                    .button("Zero WCM")
                    .on_hover_text(
                        "Zero the WCM offset (corrB). Beam must be OFF but RF must be ON.",
                    )
                    .clicked()
                {
                    out_msgs.push(ClientMessage::ZeroWCM {
                        device: device.name.clone(),
                    });
                }
            }

            if device.device_type != DeviceType::Dq && device.device_type != DeviceType::Ict {
                if ui
                    .button("Sweep Timing")
                    .on_hover_text(
                        "Sweep timing window to find optimal peak. Beam must be ON the device.",
                    )
                    .clicked()
                {
                    out_msgs.push(ClientMessage::SweepTiming {
                        device: device.name.clone(),
                    });
                }
            }

            // Build defaults tooltip
            let defaults_tip = if device.defaults.is_empty() {
                "Restore all PV defaults for this device".to_string()
            } else {
                let mut lines: Vec<String> = device
                    .defaults
                    .iter()
                    .filter(|(k, _)| k.as_str() != "charge")
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect();
                lines.sort();
                format!("Restore defaults:\n{}", lines.join("\n"))
            };
            if ui
                .button("Restore Defaults")
                .on_hover_text(defaults_tip)
                .clicked()
            {
                out_msgs.push(ClientMessage::RestoreDefaults {
                    device: device.name.clone(),
                });
            }

            if ui
                .button("Clear")
                .on_hover_text("Empty this device's rolling data buffer")
                .clicked()
            {
                out_msgs.push(ClientMessage::ClearBuffer {
                    device: Some(device.name.clone()),
                });
            }
        });
    });
}

/// Draw global controls
pub fn draw_global_controls(
    ui: &mut egui::Ui,
    buffer_size: &mut usize,
    buffer_size_str: &mut String,
    out_msgs: &mut Vec<ClientMessage>,
    frozen_stats: &mut Option<Vec<(String, Stats)>>,
    snapshots: &[ChartSnapshot],
    y_scale: &mut YAxisScale,
    y_min_str: &mut String,
    y_max_str: &mut String,
) {
    ui.horizontal(|ui: &mut egui::Ui| {
        if ui.button("Clear Calibration (All)").clicked() {
            out_msgs.push(ClientMessage::ClearCalibration);
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
        let response = ui.add(egui::TextEdit::singleline(buffer_size_str).desired_width(60.0));
        if response.lost_focus() {
            if let Ok(new_size) = buffer_size_str.parse::<usize>() {
                if new_size != *buffer_size {
                    *buffer_size = new_size;
                    out_msgs.push(ClientMessage::SetBufferSize { size: new_size });
                }
            } else {
                // Reset to current value on invalid input
                *buffer_size_str = buffer_size.to_string();
            }
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
                    snapshots
                        .iter()
                        .map(|s| (s.device_name.clone(), s.stats.clone()))
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
                {
                    if !matches!(y_scale, YAxisScale::Manual { .. }) {
                        *y_scale = YAxisScale::Manual {
                            min: 0.0,
                            max: 100.0,
                        };
                        *y_min_str = "0".to_string();
                        *y_max_str = "100".to_string();
                    }
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
