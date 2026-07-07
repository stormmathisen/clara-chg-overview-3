use shared::messages::{
    ChartSnapshot, ClientMessage, DeviceStatus, DeviceType, Notification, NotificationLevel,
    ServerMessage, Stats,
};
use std::collections::{HashSet, VecDeque};

use crate::controls;
use crate::strip_chart;
use crate::ws_client::WsClient;

/// Y-axis scaling mode for all strip charts
#[derive(Clone, Debug, PartialEq)]
pub enum YAxisScale {
    /// Auto-scale to fit data (default)
    Auto,
    /// Lower bound fixed at 0, upper bound auto-scaled
    ZeroBased,
    /// Manual min and max
    Manual { min: f64, max: f64 },
}

/// Filter state for display
pub struct DisplayFilter {
    pub show_wcm: bool,
    pub show_dq: bool,
    pub show_fcup: bool,
    pub show_ict: bool,
    pub hidden_devices: HashSet<String>,
}

impl Default for DisplayFilter {
    fn default() -> Self {
        Self {
            show_wcm: true,
            show_dq: true,
            show_fcup: true,
            show_ict: true,
            hidden_devices: HashSet::new(),
        }
    }
}

impl DisplayFilter {
    pub fn is_visible(&self, device: &DeviceStatus) -> bool {
        let type_visible = match device.device_type {
            DeviceType::Wcm => self.show_wcm,
            DeviceType::Dq => self.show_dq,
            DeviceType::Fcup => self.show_fcup,
            DeviceType::Ict => self.show_ict,
        };
        type_visible && !self.hidden_devices.contains(&device.name)
    }
}

pub struct ChargeOverviewApp {
    ws: WsClient,
    devices: Vec<DeviceStatus>,
    snapshots: Vec<ChartSnapshot>,
    notifications: VecDeque<(Notification, u64)>,
    buffer_size: usize,
    pub buffer_size_str: String,
    connected: bool,
    pub filter: DisplayFilter,
    pub device_order: Vec<String>,
    pub frozen_stats: Option<Vec<(String, Stats)>>,
    frame_count: u64,
    pub y_scale: YAxisScale,
    pub y_min_str: String,
    pub y_max_str: String,
}

impl ChargeOverviewApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // Derive WebSocket URL from current page location
        let ws_url = get_ws_url();

        let mut ws = WsClient::new();
        ws.connect(&ws_url);

        Self {
            ws,
            devices: Vec::new(),
            snapshots: Vec::new(),
            notifications: VecDeque::new(),
            buffer_size: 1000,
            buffer_size_str: "1000".to_string(),
            connected: false,
            filter: DisplayFilter::default(),
            device_order: Vec::new(),
            frozen_stats: None,
            frame_count: 0,
            y_scale: YAxisScale::Auto,
            y_min_str: String::new(),
            y_max_str: String::new(),
        }
    }

    fn process_messages(&mut self) {
        self.ws.poll();
        self.connected = self.ws.is_connected();

        while let Some(msg) = self.ws.incoming.pop_front() {
            match msg {
                ServerMessage::Init {
                    devices,
                    buffer_size,
                    device_order,
                } => {
                    self.device_order = device_order;
                    self.devices = devices;
                    self.buffer_size = buffer_size;
                }
                ServerMessage::ChartData { snapshots } => {
                    self.snapshots = snapshots;
                    // Update stats in device status from snapshots
                    for snap in &self.snapshots {
                        if let Some(dev) = self.devices.iter_mut().find(|d| d.name == snap.device_name) {
                            dev.stats = snap.stats.clone();
                        }
                    }
                }
                ServerMessage::StateUpdate {
                    device,
                    sensitivity,
                } => {
                    if let Some(dev) = self.devices.iter_mut().find(|d| d.name == device) {
                        dev.current_sensitivity = sensitivity;
                    }
                }
                ServerMessage::BufferSizeChanged { size } => {
                    self.buffer_size = size;
                    self.buffer_size_str = size.to_string();
                }
                ServerMessage::DeviceOrderChanged { order } => {
                    self.device_order = order;
                }
                ServerMessage::Notify(n) => {
                    self.notifications.push_back((n, self.frame_count));
                    if self.notifications.len() > 50 {
                        self.notifications.pop_front();
                    }
                }
            }
        }
    }
}

impl eframe::App for ChargeOverviewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_messages();
        self.frame_count += 1;

        // Auto-dismiss non-error notifications after ~10s (100 frames at 10Hz)
        let fc = self.frame_count;
        self.notifications.retain(|(n, received_frame)| {
            matches!(n.level, NotificationLevel::Error) || fc - received_frame < 100
        });

        let mut out_msgs: Vec<ClientMessage> = Vec::new();

        // Top panel: title + global controls
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui: &mut egui::Ui| {
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.heading("CLARA Charge Overview");
                ui.separator();
                let status_color = if self.connected {
                    egui::Color32::GREEN
                } else {
                    egui::Color32::RED
                };
                ui.colored_label(status_color, if self.connected { "● Connected" } else { "● Disconnected" });
            });
            controls::draw_global_controls(ui, &mut self.buffer_size, &mut self.buffer_size_str, &mut out_msgs, &mut self.frozen_stats, &self.snapshots, &mut self.y_scale, &mut self.y_min_str, &mut self.y_max_str);
        });

        // Bottom panel: notifications
        egui::TopBottomPanel::bottom("notifications").show(ctx, |ui: &mut egui::Ui| {
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.label("Notifications:");
                if let Some((n, _frame)) = self.notifications.back() {
                    let color = match n.level {
                        NotificationLevel::Info => egui::Color32::LIGHT_BLUE,
                        NotificationLevel::Success => egui::Color32::GREEN,
                        NotificationLevel::Warning => egui::Color32::YELLOW,
                        NotificationLevel::Error => egui::Color32::RED,
                    };
                    ui.colored_label(color, &n.message);
                }
            });
        });

        // Left panel: device controls
        egui::SidePanel::left("controls_panel")
            .default_width(280.0)
            .show(ctx, |ui: &mut egui::Ui| {
                egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                    controls::draw_filter_controls(ui, &self.devices, &mut self.filter);
                    ui.separator();
                    let order_before = self.device_order.clone();
                    let names = order_before.clone();
                    let total = names.len();
                    let mut item_rects: Vec<egui::Rect> = Vec::new();

                    let (_, dropped_payload) = ui.dnd_drop_zone::<String, ()>(egui::Frame::default(), |ui| {
                        for (i, name) in names.iter().enumerate() {
                            if let Some(device) = self.devices.iter().find(|d| &d.name == name) {
                                let item_id = egui::Id::new("device_dnd").with(name.as_str());
                                let scope_resp = ui.scope(|ui| {
                                    ui.horizontal(|ui: &mut egui::Ui| {
                                        ui.dnd_drag_source(item_id, name.clone(), |ui| {
                                            ui.label(
                                                egui::RichText::new("⠿")
                                                    .size(16.0)
                                                    .color(egui::Color32::GRAY),
                                            )
                                            .on_hover_text("Drag to reorder");
                                        });
                                        ui.vertical(|ui: &mut egui::Ui| {
                                            controls::draw_device_controls(
                                                ui,
                                                device,
                                                &mut out_msgs,
                                                i,
                                                total,
                                                &mut self.device_order,
                                            );
                                        });
                                    });
                                });
                                item_rects.push(scope_resp.response.rect);
                                ui.add_space(4.0);
                            }
                        }
                    });

                    // Handle drag-and-drop reorder
                    if let Some(source_name) = dropped_payload {
                        if let Some(source_idx) = self.device_order.iter().position(|n| n == source_name.as_str()) {
                            if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                                let mut target_idx = self.device_order.len();
                                for (rect_i, rect) in item_rects.iter().enumerate() {
                                    if pointer_pos.y < rect.center().y {
                                        target_idx = rect_i;
                                        break;
                                    }
                                }
                                if source_idx != target_idx {
                                    let item = self.device_order.remove(source_idx);
                                    let adjusted = if source_idx < target_idx {
                                        (target_idx - 1).min(self.device_order.len())
                                    } else {
                                        target_idx.min(self.device_order.len())
                                    };
                                    self.device_order.insert(adjusted, item);
                                }
                            }
                        }
                    }

                    if self.device_order != order_before {
                        out_msgs.push(ClientMessage::SetDeviceOrder {
                            order: self.device_order.clone(),
                        });
                    }
                });
            });

        // Central panel: strip charts
        egui::CentralPanel::default().show(ctx, |ui: &mut egui::Ui| {
            egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                // Build ordered, filtered snapshots
                let visible_snapshots: Vec<&ChartSnapshot> = self
                    .device_order
                    .iter()
                    .filter_map(|name| {
                        let device = self.devices.iter().find(|d| &d.name == name)?;
                        if !self.filter.is_visible(device) {
                            return None;
                        }
                        self.snapshots.iter().find(|s| &s.device_name == name)
                    })
                    .collect();

                let chart_height = if visible_snapshots.is_empty() {
                    150.0
                } else {
                    let avail = ui.available_height();
                    (avail / visible_snapshots.len() as f32).max(100.0).min(200.0)
                };
                for snapshot in &visible_snapshots {
                    let stats_override = self.frozen_stats.as_ref().and_then(|fs| {
                        fs.iter().find(|(n, _)| n == &snapshot.device_name).map(|(_, s)| s)
                    });
                    strip_chart::draw_strip_chart(ui, snapshot, chart_height, stats_override, &self.y_scale);
                    ui.add_space(4.0);
                }
            });
        });

        // Send any outgoing messages
        for msg in &out_msgs {
            self.ws.send(msg);
        }

        // Request repaint at 10Hz for live updates
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Derive WebSocket URL from the page origin
fn get_ws_url() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let location = web_sys::window()
            .and_then(|w| w.location().href().ok())
            .unwrap_or_else(|| "http://localhost:49195/".to_string());
        let ws_proto = if location.starts_with("https") {
            "wss"
        } else {
            "ws"
        };
        // Extract host from URL
        let host = web_sys::window()
            .and_then(|w| w.location().host().ok())
            .unwrap_or_else(|| "localhost:49195".to_string());
        format!("{ws_proto}://{host}/ws")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        "ws://localhost:49195/ws".to_string()
    }
}
