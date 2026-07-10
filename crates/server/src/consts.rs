//! Centralised tunable constants (ports, timeouts, intervals) that were previously
//! scattered across `main.rs`, `ws.rs`, and inline literals in `hardware.rs`.

use std::time::Duration;

/// Default HTTP/WebSocket listen port (overridable via `PORT`).
pub const DEFAULT_PORT: u16 = 49195;

/// Cap on inbound WebSocket message size.
pub const MAX_WS_MESSAGE_SIZE: usize = 64 * 1024;

/// TCP port of the device front-end box (settings push + reachability ping).
pub const FRONT_END_PORT: u16 = 56000;
/// Connect timeout for talking to a front-end box.
pub const FRONT_END_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

// --- Background task cadences ---------------------------------------------------

/// How often persisted state is flushed to disk.
pub const PERSIST_INTERVAL: Duration = Duration::from_secs(30);
/// How often the watchdog checks for stale devices.
pub const WATCHDOG_INTERVAL: Duration = Duration::from_secs(10);
/// A device with no fresh data for this long is marked disconnected.
pub const WATCHDOG_STALE_SECS: f64 = 60.0;
/// How often each device front-end is pinged for reachability.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

// --- Chart broadcast ------------------------------------------------------------

/// Capacity of the broadcast channel fanning chart/notification messages to clients.
pub const BROADCAST_CHANNEL_CAPACITY: usize = 2048;
/// Interval between chart-data broadcasts (10 Hz).
pub const BROADCAST_INTERVAL: Duration = Duration::from_millis(100);
/// Per-client inbound command rate limit (commands per second).
pub const MAX_COMMANDS_PER_SEC: usize = 10;
