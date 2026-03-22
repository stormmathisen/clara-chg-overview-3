use shared::messages::{
    ChartSnapshot, ClientMessage, DeviceStatus, DeviceType, Notification, ServerMessage, Stats,
};
use std::collections::{HashSet, VecDeque};

use crate::controls;
use crate::strip_chart;
use crate::ws_client::WsClient;

/// Filter state for display
pub struct DisplayFilter {
    pub show_wcm: bool,
    pub show_dq: bool,
    pub show_fcup: bool,
    pub hidden_devices: HashSet<String>,
}

impl Default for DisplayFilter {
    fn default() -> Self {
        Self {
            show_wcm: true,
            show_dq: true,
            show_fcup: true,
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
        };
        type_visible && !self.hidden_devices.contains(&device.name)
    }
}

pub struct ChargeOverviewApp {
    ws: WsClient,
    devices: Vec<DeviceStatus>,
    snapshots: Vec<ChartSnapshot>,
    notifications: VecDeque<Notification>,
    buffer_size: usize,
    pub buffer_size_str: String,
    connected: bool,
    pub filter: DisplayFilter,
    pub device_order: Vec<String>,
    pub frozen_stats: Option<Vec<(String, Stats)>>,
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
                    self.notifications.push_back(n);
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
            controls::draw_global_controls(ui, &mut self.buffer_size, &mut self.buffer_size_str, &mut out_msgs, &mut self.frozen_stats, &self.snapshots);
        });

        // Bottom panel: notifications
        egui::TopBottomPanel::bottom("notifications").show(ctx, |ui: &mut egui::Ui| {
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.label("Notifications:");
                if let Some(n) = self.notifications.back() {
                    let color = match n.level {
                        shared::messages::NotificationLevel::Info => egui::Color32::LIGHT_BLUE,
                        shared::messages::NotificationLevel::Success => egui::Color32::GREEN,
                        shared::messages::NotificationLevel::Warning => egui::Color32::YELLOW,
                        shared::messages::NotificationLevel::Error => egui::Color32::RED,
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
                    for i in 0..self.device_order.len() {
                        let name = &self.device_order[i];
                        if let Some(device) = self.devices.iter().find(|d| &d.name == name) {
                            controls::draw_device_controls(
                                ui,
                                device,
                                &mut out_msgs,
                                i,
                                self.device_order.len(),
                                &mut self.device_order,
                            );
                            ui.add_space(4.0);
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
                    strip_chart::draw_strip_chart(ui, snapshot, chart_height, stats_override);
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
