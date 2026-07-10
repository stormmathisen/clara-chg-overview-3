//! Small shared UI helpers.

use shared::messages::NotificationLevel;

/// Glyphs used in the UI, kept together because they are all subject to one
/// non-obvious constraint.
///
/// egui's default proportional family is `Ubuntu-Light → NotoEmoji → emoji-icon-font`
/// — it does **not** include Hack. A codepoint missing from all three renders as a
/// tofu box, silently, with no compile or runtime error. Check any new glyph against
/// `epaint::text::Fonts::has_glyphs(&FontId::new(14.0, FontFamily::Proportional), g)`
/// before using it here.
///
/// Two near-misses worth remembering: `●` U+25CF and `▲`/`▼` U+25B2/U+25BC live only
/// in Hack, so they are monospace-only; `⠿` U+283F is in none of the bundled fonts.
pub mod glyph {
    /// Filled circle for connection indicators. U+25CF is *not* usable here.
    pub const STATUS_DOT: &str = "⏺";
    /// Drag-to-reorder affordance. U+283F is *not* usable here.
    pub const DRAG_HANDLE: &str = "☰";
    /// Expand the notification history upwards.
    pub const CHEVRON_UP: &str = "⏶";
    /// Collapse the notification history.
    pub const CHEVRON_DOWN: &str = "⏷";
}

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

/// Severity colour for a notification. Shared by the notification bar and the
/// history list so a message keeps the same colour in both.
pub fn notification_color(level: &NotificationLevel) -> egui::Color32 {
    match level {
        NotificationLevel::Info => egui::Color32::LIGHT_BLUE,
        NotificationLevel::Success => egui::Color32::GREEN,
        NotificationLevel::Warning => egui::Color32::YELLOW,
        NotificationLevel::Error => egui::Color32::RED,
    }
}
