use shared::messages::{ChartSnapshot, ClientMessage, DeviceStatus, DeviceType, Stats};

use crate::app::DisplayFilter;

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
            let connected_color = if device.connected {
                egui::Color32::GREEN
            } else {
                egui::Color32::RED
            };
            ui.colored_label(connected_color, "●");
            ui.label(
                egui::RichText::new(&device.name)
                    .strong()
                    .size(13.0),
            );
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

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
                if index + 1 < total {
                    if ui.small_button("Dn").clicked() {
                        device_order.swap(index, index + 1);
                    }
                }
                if index > 0 {
                    if ui.small_button("Up").clicked() {
                        device_order.swap(index, index - 1);
                    }
                }
            });
        });

        // Sensitivity selector
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

        ui.horizontal(|ui: &mut egui::Ui| {
            // Device-specific buttons
            if device.device_type == DeviceType::Wcm {
                if ui.button("Zero WCM").clicked() {
                    out_msgs.push(ClientMessage::ZeroWCM {
                        device: device.name.clone(),
                    });
                }
            }

            if ui.button("Sweep Timing").clicked() {
                out_msgs.push(ClientMessage::SweepTiming {
                    device: device.name.clone(),
                });
            }

            if ui.button("Restore Defaults").clicked() {
                out_msgs.push(ClientMessage::RestoreDefaults {
                    device: device.name.clone(),
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
) {
    ui.horizontal(|ui: &mut egui::Ui| {
        if ui.button("Clear Calibration (All)").clicked() {
            out_msgs.push(ClientMessage::ClearCalibration);
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
        if ui.button(if is_frozen { "Unfreeze Stats" } else { "Freeze Stats" }).clicked() {
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
}
