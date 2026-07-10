use egui_plot::{AxisHints, Line, Plot, PlotPoints};
use shared::messages::{ChartSnapshot, Stats};

use crate::app::YAxisScale;

/// Draw a strip chart for a single device
pub fn draw_strip_chart(
    ui: &mut egui::Ui,
    snapshot: &ChartSnapshot,
    height: f32,
    stats_override: Option<&Stats>,
    y_scale: &YAxisScale,
) {
    let stats = stats_override.unwrap_or(&snapshot.stats);
    let frozen = stats_override.is_some();

    // Stats header
    ui.horizontal(|ui: &mut egui::Ui| {
        ui.label(
            egui::RichText::new(&snapshot.device_name)
                .strong()
                .size(14.0),
        );
        if frozen {
            ui.colored_label(egui::Color32::YELLOW, "❄ FROZEN");
        }
        ui.separator();
        stats_label(ui, stats);
    });

    // Plot
    let points: PlotPoints = snapshot.points.iter().map(|p| [p[0], p[1]]).collect();

    let line = Line::new(points).color(egui::Color32::LIGHT_BLUE);

    let x_axes = vec![AxisHints::new_x().formatter(format_timestamp)];
    let y_axes = vec![AxisHints::new_y().label("Charge (pC)")];

    let mut plot = Plot::new(&snapshot.device_name)
        .height(height)
        .custom_x_axes(x_axes)
        .custom_y_axes(y_axes)
        .show_axes([true, true])
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false);

    match y_scale {
        YAxisScale::Auto => {}
        YAxisScale::ZeroBased => {
            plot = plot.include_y(0.0);
        }
        YAxisScale::Manual { min, max } => {
            plot = plot
                .include_y(*min)
                .include_y(*max)
                .auto_bounds(egui::Vec2b::new(true, false));
        }
    }

    plot.show(ui, |plot_ui| {
        plot_ui.line(line);
    });
}

fn format_timestamp(mark: egui_plot::GridMark, _range: &std::ops::RangeInclusive<f64>) -> String {
    let secs = mark.value as i64;
    let remainder = mark.value - secs as f64;
    let millis = (remainder * 1000.0) as u32;

    // Convert POSIX timestamp to HH:MM:SS
    let total_secs = secs.rem_euclid(86400);
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if millis > 0 {
        format!("{h:02}:{m:02}:{s:02}.{millis:03}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}

fn stats_label(ui: &mut egui::Ui, stats: &Stats) {
    let text = format!(
        "Mean: {:.4} pC  Min: {:.4} pC  Max: {:.4} pC  RMSD: {:.4} pC",
        stats.mean, stats.min, stats.max, stats.rmsd
    );
    ui.label(egui::RichText::new(text).size(11.0).monospace());
}
