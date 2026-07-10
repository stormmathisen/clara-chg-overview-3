use shared::chart::PointBuffer;
use shared::messages::{
    ClientMessage, DeviceStatus, DeviceType, Notification, NotificationLevel, ServerMessage, Stats,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use crate::controls;
use crate::strip_chart;
use crate::util::status_color;
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

/// The Y-axis scale plus the raw text of its manual min/max boxes. Bundled so the
/// three travel together instead of being threaded through as separate arguments.
pub struct YAxisState {
    pub scale: YAxisScale,
    pub min_str: String,
    pub max_str: String,
}

impl Default for YAxisState {
    fn default() -> Self {
        Self {
            scale: YAxisScale::Auto,
            min_str: String::new(),
            max_str: String::new(),
        }
    }
}

/// A client-side chart for one device: the shared point buffer (kept in sync via
/// snapshots/deltas) plus the display name and latest stats. Addressed by the device's
/// index in the `Init.devices` list.
pub struct DeviceChart {
    pub name: String,
    pub buffer: PointBuffer,
    pub stats: Stats,
}

impl DeviceChart {
    fn new(name: String) -> Self {
        Self {
            name,
            buffer: PointBuffer::new(),
            stats: Stats::default(),
        }
    }

    fn set_snapshot(&mut self, points: Vec<[f64; 2]>, stats: Stats, cursor: u64, cap: usize) {
        self.buffer.set_snapshot(points, cursor, cap);
        self.stats = stats;
    }

    fn apply_delta(&mut self, new_points: Vec<[f64; 2]>, stats: Stats, cursor: u64, cap: usize) {
        // Ignore stats from deltas that are entirely stale (the buffer ignores the
        // points too); keep the newer stats we already hold.
        if cursor <= self.buffer.cursor() {
            return;
        }
        self.buffer.apply_delta(new_points, cursor, cap);
        self.stats = stats;
    }

    fn set_capacity(&mut self, cap: usize) {
        self.buffer.set_capacity(cap);
    }
}

/// Live-update repaint cadence (matches the server's 10 Hz broadcast).
const REPAINT_INTERVAL: Duration = Duration::from_millis(100);
/// Maximum notifications retained in the history buffer.
const MAX_NOTIFICATIONS: usize = 50;
/// Non-error notifications auto-dismiss after this many frames (~10s at 10 Hz).
const NOTIFICATION_DISMISS_FRAMES: u64 = 100;
/// Buffer size assumed until the server's Init message arrives.
const DEFAULT_BUFFER_SIZE: usize = 1000;
const CONTROLS_PANEL_WIDTH: f32 = 280.0;
const CHART_HEIGHT_EMPTY: f32 = 150.0;
const CHART_HEIGHT_MIN: f32 = 100.0;
const CHART_HEIGHT_MAX: f32 = 200.0;

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
    /// Per-device chart buffers, parallel to `devices` (same index the server uses).
    charts: Vec<DeviceChart>,
    notifications: VecDeque<(Notification, u64)>,
    buffer_size: usize,
    pub buffer_size_str: String,
    connected: bool,
    pub filter: DisplayFilter,
    pub device_order: Vec<String>,
    pub frozen_stats: Option<Vec<(String, Stats)>>,
    frame_count: u64,
    pub y_axis: YAxisState,
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
            charts: Vec::new(),
            notifications: VecDeque::new(),
            buffer_size: DEFAULT_BUFFER_SIZE,
            buffer_size_str: DEFAULT_BUFFER_SIZE.to_string(),
            connected: false,
            filter: DisplayFilter::default(),
            device_order: Vec::new(),
            frozen_stats: None,
            frame_count: 0,
            y_axis: YAxisState::default(),
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
                    // Rebuild chart buffers parallel to the device list; the full
                    // ChartData snapshot that follows Init fills them in.
                    self.charts = devices
                        .iter()
                        .map(|d| DeviceChart::new(d.name.clone()))
                        .collect();
                    self.devices = devices;
                    self.buffer_size = buffer_size;
                }
                ServerMessage::ChartData { snapshots } => {
                    let cap = self.buffer_size;
                    for snap in snapshots {
                        if let Some(chart) = self.charts.get_mut(snap.device) {
                            chart.set_snapshot(snap.points, snap.stats, snap.cursor, cap);
                        }
                    }
                }
                ServerMessage::ChartDelta { updates } => {
                    let cap = self.buffer_size;
                    for upd in updates {
                        if let Some(chart) = self.charts.get_mut(upd.device) {
                            chart.apply_delta(upd.new_points, upd.stats, upd.cursor, cap);
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
                    for chart in &mut self.charts {
                        chart.set_capacity(size);
                    }
                }
                ServerMessage::DeviceOrderChanged { order } => {
                    self.device_order = order;
                }
                ServerMessage::Notify(n) => {
                    self.notifications.push_back((n, self.frame_count));
                    if self.notifications.len() > MAX_NOTIFICATIONS {
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

        // Auto-dismiss non-error notifications after NOTIFICATION_DISMISS_FRAMES frames.
        let fc = self.frame_count;
        self.notifications.retain(|(n, received_frame)| {
            matches!(n.level, NotificationLevel::Error)
                || fc - received_frame < NOTIFICATION_DISMISS_FRAMES
        });

        let mut out_msgs: Vec<ClientMessage> = Vec::new();

        // Top panel: title + global controls
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui: &mut egui::Ui| {
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.heading("CLARA Charge Overview");
                ui.separator();
                ui.colored_label(
                    status_color(self.connected),
                    if self.connected {
                        "● Connected"
                    } else {
                        "● Disconnected"
                    },
                );
            });
            controls::draw_global_controls(
                ui,
                &mut self.buffer_size,
                &mut self.buffer_size_str,
                &mut out_msgs,
                &mut self.frozen_stats,
                &self.charts,
                &mut self.y_axis,
            );
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

        // Devices keyed by name, so the panels below avoid repeated linear scans.
        let device_by_name: HashMap<&str, &DeviceStatus> =
            self.devices.iter().map(|d| (d.name.as_str(), d)).collect();

        // Left panel: device controls
        egui::SidePanel::left("controls_panel")
            .default_width(CONTROLS_PANEL_WIDTH)
            .show(ctx, |ui: &mut egui::Ui| {
                egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                    controls::draw_filter_controls(ui, &self.devices, &mut self.filter);
                    ui.separator();
                    // `reordered` is set by the Up/Dn buttons (returned from
                    // draw_device_controls) and by the drag-and-drop handler below, so we
                    // don't need to clone the order just to diff it afterwards.
                    let mut reordered = false;
                    // A snapshot of the names to iterate while `device_order` is mutably borrowed.
                    let names = self.device_order.clone();
                    let total = names.len();
                    let mut item_rects: Vec<egui::Rect> = Vec::new();

                    let (_, dropped_payload) =
                        ui.dnd_drop_zone::<String, ()>(egui::Frame::default(), |ui| {
                            for (i, name) in names.iter().enumerate() {
                                if let Some(device) = device_by_name.get(name.as_str()) {
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
                                                reordered |= controls::draw_device_controls(
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
                        if let Some(source_idx) = self
                            .device_order
                            .iter()
                            .position(|n| n == source_name.as_str())
                        {
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
                                    reordered = true;
                                }
                            }
                        }
                    }

                    if reordered {
                        out_msgs.push(ClientMessage::SetDeviceOrder {
                            order: self.device_order.clone(),
                        });
                    }
                });
            });

        // Central panel: strip charts
        let chart_by_name: HashMap<&str, &DeviceChart> =
            self.charts.iter().map(|c| (c.name.as_str(), c)).collect();
        egui::CentralPanel::default().show(ctx, |ui: &mut egui::Ui| {
            egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                // Ordered, filtered charts to render.
                let visible_charts: Vec<&DeviceChart> = self
                    .device_order
                    .iter()
                    .filter_map(|name| {
                        let device = device_by_name.get(name.as_str())?;
                        if !self.filter.is_visible(device) {
                            return None;
                        }
                        chart_by_name.get(name.as_str()).copied()
                    })
                    .collect();

                let chart_height = if visible_charts.is_empty() {
                    CHART_HEIGHT_EMPTY
                } else {
                    let avail = ui.available_height();
                    (avail / visible_charts.len() as f32).clamp(CHART_HEIGHT_MIN, CHART_HEIGHT_MAX)
                };
                for chart in &visible_charts {
                    let stats_override = self
                        .frozen_stats
                        .as_ref()
                        .and_then(|fs| fs.iter().find(|(n, _)| n == &chart.name).map(|(_, s)| s));
                    strip_chart::draw_strip_chart(
                        ui,
                        chart,
                        chart_height,
                        stats_override,
                        &self.y_axis.scale,
                    );
                    ui.add_space(4.0);
                }
            });
        });

        // Send any outgoing messages
        for msg in &out_msgs {
            self.ws.send(msg);
        }

        // Request repaint at 10Hz for live updates
        ctx.request_repaint_after(REPAINT_INTERVAL);
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
