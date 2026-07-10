//! Small shared UI helpers.

/// Format a POSIX timestamp (seconds) as `HH:MM:SS` within the day.
/// Shared by the device-control "Last:" label and the chart time axis.
pub fn hms(secs: f64) -> String {
    let total = (secs as i64).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// Green when `ok`, red otherwise — the app's standard status colour.
pub fn status_color(ok: bool) -> egui::Color32 {
    if ok {
        egui::Color32::GREEN
    } else {
        egui::Color32::RED
    }
}
