use shared::config::DeviceConfig;
use shared::messages::Stats;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::warn;

/// Rolling buffer of (timestamp_secs, value) pairs with fixed capacity.
///
/// Running `sum`/`sum_sq` are maintained on every push/pop so `statistics()` is O(1)
/// for mean and RMSD. Min/max can't be maintained in O(1) under a sliding window, so
/// they are recomputed lazily — but only when the current extremum is evicted, which
/// is uncommon in a monotonic-ish charge signal.
#[derive(Clone, Debug)]
pub struct RollingBuffer {
    data: VecDeque<[f64; 2]>,
    capacity: usize,
    /// Monotonic count of all pushes ever, independent of capacity. Used to detect
    /// "N fresh samples have arrived" even when the buffer is smaller than N.
    total_pushed: u64,
    sum: f64,
    sum_sq: f64,
    /// Cached extrema; valid only when `!extrema_dirty` and the buffer is non-empty.
    min: f64,
    max: f64,
    extrema_dirty: bool,
}

impl RollingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(capacity),
            capacity,
            total_pushed: 0,
            sum: 0.0,
            sum_sq: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            extrema_dirty: false,
        }
    }

    pub fn push(&mut self, timestamp: f64, value: f64) {
        if self.data.len() >= self.capacity {
            self.evict_front();
        }
        self.data.push_back([timestamp, value]);
        self.sum += value;
        self.sum_sq += value * value;
        // Resolve the extrema now (while we hold `&mut self`) so `statistics()` can
        // stay `&self` and simply read the cached values.
        if self.extrema_dirty {
            self.recompute_extrema();
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
        }
        self.total_pushed = self.total_pushed.wrapping_add(1);
    }

    /// Drop the oldest sample, keeping the running accumulators in sync.
    fn evict_front(&mut self) {
        if let Some([_, v]) = self.data.pop_front() {
            self.sum -= v;
            self.sum_sq -= v * v;
            // If we just removed a cached extremum, it must be recomputed.
            if v == self.min || v == self.max {
                self.extrema_dirty = true;
            }
        }
    }

    /// Total number of values ever pushed, regardless of the rolling capacity.
    pub fn total_pushed(&self) -> u64 {
        self.total_pushed
    }

    /// Mean of the most recent `n` values (or all of them if fewer are buffered).
    /// Returns None when the buffer is empty.
    pub fn mean_of_last(&self, n: usize) -> Option<f64> {
        if self.data.is_empty() {
            return None;
        }
        let take = n.min(self.data.len());
        let sum: f64 = self.data.iter().rev().take(take).map(|p| p[1]).sum();
        Some(sum / take as f64)
    }

    pub fn set_capacity(&mut self, new_capacity: usize) {
        self.capacity = new_capacity;
        while self.data.len() > new_capacity {
            self.evict_front();
        }
        if self.extrema_dirty {
            self.recompute_extrema();
        }
    }

    /// Recompute cached min/max from scratch. Called only when the previous extremum
    /// was evicted (`extrema_dirty`).
    fn recompute_extrema(&mut self) {
        self.min = f64::INFINITY;
        self.max = f64::NEG_INFINITY;
        for p in &self.data {
            self.min = self.min.min(p[1]);
            self.max = self.max.max(p[1]);
        }
        self.extrema_dirty = false;
    }

    pub fn statistics(&self) -> Stats {
        if self.data.is_empty() {
            return Stats::default();
        }
        let n = self.data.len() as f64;
        let mean = self.sum / n;
        let rmsd = ((self.sum_sq / n) - (mean * mean)).max(0.0).sqrt();
        // The `&mut` operations (push/set_capacity/clear) always resolve dirty extrema,
        // so the cached values are current here. Fall back defensively just in case.
        let (min, max) = if self.extrema_dirty {
            self.data
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(mn, mx), p| {
                    (mn.min(p[1]), mx.max(p[1]))
                })
        } else {
            (self.min, self.max)
        };
        Stats {
            mean,
            min,
            max,
            rmsd,
        }
    }

    pub fn as_points(&self) -> Vec<[f64; 2]> {
        self.data.iter().copied().collect()
    }

    /// The most recent `n` points (fewer if the buffer is smaller), oldest-first.
    /// Used to build incremental chart deltas.
    pub fn last_points(&self, n: usize) -> Vec<[f64; 2]> {
        let take = n.min(self.data.len());
        self.data
            .iter()
            .skip(self.data.len() - take)
            .copied()
            .collect()
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.sum = 0.0;
        self.sum_sq = 0.0;
        self.min = f64::INFINITY;
        self.max = f64::NEG_INFINITY;
        self.extrema_dirty = false;
    }
}

/// State for a single device
pub struct DeviceState {
    pub config: DeviceConfig,
    pub name: String,
    pub buffer: RollingBuffer,
    pub current_sensitivity: usize,
    pub connected: bool,
    pub fe_alive: bool,
    pub last_data_time: f64,
}

/// Global application state, shared across all tasks
pub struct InnerState {
    pub devices: Vec<DeviceState>,
    pub buffer_size: usize,
    pub device_order: Vec<String>,
    /// Live front-end reset countdown `(remaining_secs, total_secs)`, or `None`. Transient
    /// (not persisted); read into `Init` so newly connected clients show the same countdown.
    pub reset_progress: Option<(u32, u32)>,
    /// Device name -> index into `devices`, so lookups by name are O(1) instead of a
    /// linear scan. `devices` is built once at startup and never reordered (the UI
    /// reorders `device_order`, not `devices`), so this stays valid for the process life.
    name_index: std::collections::HashMap<String, usize>,
}

impl InnerState {
    /// Build state from the device list, deriving the name index. `devices` must not
    /// be reordered afterwards or the index (and EPICS/ping index routing) go stale.
    pub fn new(devices: Vec<DeviceState>, buffer_size: usize, device_order: Vec<String>) -> Self {
        let name_index = devices
            .iter()
            .enumerate()
            .map(|(i, d)| (d.name.clone(), i))
            .collect();
        Self {
            devices,
            buffer_size,
            device_order,
            reset_progress: None,
            name_index,
        }
    }

    /// Index of a device by name, if present.
    pub fn device_index(&self, name: &str) -> Option<usize> {
        self.name_index.get(name).copied()
    }

    /// Shared reference to a device by name.
    pub fn device(&self, name: &str) -> Option<&DeviceState> {
        self.device_index(name).map(|i| &self.devices[i])
    }

    /// Mutable reference to a device by name.
    pub fn device_mut(&mut self, name: &str) -> Option<&mut DeviceState> {
        self.device_index(name).map(|i| &mut self.devices[i])
    }
}

pub type AppState = Arc<RwLock<InnerState>>;

/// Persistent state saved to disk
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct PersistedState {
    pub buffer_size: usize,
    pub sensitivities: std::collections::HashMap<String, usize>,
    #[serde(default)]
    pub device_order: Vec<String>,
}

impl PersistedState {
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(state) => state,
                Err(e) => {
                    let backup = path.with_extension("json.corrupt");
                    let _ = std::fs::rename(path, &backup);
                    warn!(
                        "Corrupt state file (backed up to {}): {e}",
                        backup.display()
                    );
                    Self::default_state()
                }
            },
            Err(_) => Self::default_state(),
        }
    }

    pub fn save(&self, path: &std::path::Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, &json).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    fn default_state() -> Self {
        Self {
            buffer_size: 1000,
            sensitivities: std::collections::HashMap::new(),
            device_order: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference statistics computed by a full scan, to check the incremental ones.
    fn brute_stats(points: &[[f64; 2]]) -> Stats {
        if points.is_empty() {
            return Stats::default();
        }
        let n = points.len() as f64;
        let sum: f64 = points.iter().map(|p| p[1]).sum();
        let sum_sq: f64 = points.iter().map(|p| p[1] * p[1]).sum();
        let mean = sum / n;
        Stats {
            mean,
            min: points.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
            max: points
                .iter()
                .map(|p| p[1])
                .fold(f64::NEG_INFINITY, f64::max),
            rmsd: ((sum_sq / n) - mean * mean).max(0.0).sqrt(),
        }
    }

    #[test]
    fn incremental_stats_match_brute_force_through_eviction() {
        // Sequence chosen so the running max (100) and min (1) get evicted, exercising
        // the lazy extrema recompute path.
        let mut buf = RollingBuffer::new(3);
        let seq = [1.0, 100.0, 50.0, 2.0, 3.0, 100.0, 4.0];
        for (i, v) in seq.iter().enumerate() {
            buf.push(i as f64, *v);
            let got = buf.statistics();
            let expected = brute_stats(&buf.as_points());
            assert!((got.mean - expected.mean).abs() < 1e-9, "mean at step {i}");
            assert!((got.min - expected.min).abs() < 1e-9, "min at step {i}");
            assert!((got.max - expected.max).abs() < 1e-9, "max at step {i}");
            assert!((got.rmsd - expected.rmsd).abs() < 1e-9, "rmsd at step {i}");
        }
    }

    #[test]
    fn last_points_returns_newest_suffix() {
        let mut buf = RollingBuffer::new(10);
        for i in 0..5 {
            buf.push(i as f64, (i * 10) as f64);
        }
        assert_eq!(buf.last_points(2), vec![[3.0, 30.0], [4.0, 40.0]]);
        // Requesting more than buffered yields everything.
        assert_eq!(buf.last_points(99).len(), 5);
    }

    #[test]
    fn clear_resets_running_stats() {
        let mut buf = RollingBuffer::new(10);
        for i in 0..5 {
            buf.push(i as f64, i as f64);
        }
        buf.clear();
        assert_eq!(buf.statistics().mean, 0.0);
        buf.push(0.0, 7.0);
        assert_eq!(buf.statistics().mean, 7.0);
        assert_eq!(buf.statistics().max, 7.0);
    }

    #[test]
    fn rolling_buffer_push_and_capacity() {
        let mut buf = RollingBuffer::new(3);
        buf.push(1.0, 10.0);
        buf.push(2.0, 20.0);
        buf.push(3.0, 30.0);
        assert_eq!(buf.as_points().len(), 3);

        // Exceeding capacity drops oldest
        buf.push(4.0, 40.0);
        assert_eq!(buf.as_points().len(), 3);
        let points = buf.as_points();
        assert_eq!(points[0], [2.0, 20.0]);
        assert_eq!(points[2], [4.0, 40.0]);
    }

    #[test]
    fn rolling_buffer_set_capacity() {
        let mut buf = RollingBuffer::new(5);
        for i in 0..5 {
            buf.push(i as f64, i as f64 * 10.0);
        }
        assert_eq!(buf.as_points().len(), 5);

        buf.set_capacity(2);
        assert_eq!(buf.as_points().len(), 2);
        let points = buf.as_points();
        assert_eq!(points[0], [3.0, 30.0]);
        assert_eq!(points[1], [4.0, 40.0]);
    }

    #[test]
    fn rolling_buffer_mean_of_last_and_total_pushed() {
        let mut buf = RollingBuffer::new(100);
        assert_eq!(buf.mean_of_last(10), None);
        for i in 1..=10 {
            buf.push(i as f64, (i * 10) as f64); // values 10,20,...,100
        }
        assert_eq!(buf.total_pushed(), 10);
        // last 3 values: 80, 90, 100 -> mean 90
        assert!((buf.mean_of_last(3).unwrap() - 90.0).abs() < 1e-10);
        // n larger than buffer averages everything (10..100 -> 55)
        assert!((buf.mean_of_last(1000).unwrap() - 55.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_buffer_total_pushed_counts_beyond_capacity() {
        let mut buf = RollingBuffer::new(3);
        for i in 0..10 {
            buf.push(i as f64, i as f64);
        }
        // Only 3 retained, but the push counter keeps climbing.
        assert_eq!(buf.as_points().len(), 3);
        assert_eq!(buf.total_pushed(), 10);
    }

    #[test]
    fn rolling_buffer_statistics_empty() {
        let buf = RollingBuffer::new(10);
        let stats = buf.statistics();
        assert_eq!(stats.mean, 0.0);
        assert_eq!(stats.min, 0.0);
        assert_eq!(stats.max, 0.0);
        assert_eq!(stats.rmsd, 0.0);
    }

    #[test]
    fn rolling_buffer_statistics_values() {
        let mut buf = RollingBuffer::new(100);
        buf.push(1.0, 10.0);
        buf.push(2.0, 20.0);
        buf.push(3.0, 30.0);
        let stats = buf.statistics();
        assert!((stats.mean - 20.0).abs() < 1e-10);
        assert!((stats.min - 10.0).abs() < 1e-10);
        assert!((stats.max - 30.0).abs() < 1e-10);
        // RMSD = sqrt(((10-20)^2 + (20-20)^2 + (30-20)^2) / 3) = sqrt(200/3) ≈ 8.165
        assert!((stats.rmsd - (200.0_f64 / 3.0).sqrt()).abs() < 1e-6);
    }

    #[test]
    fn rolling_buffer_statistics_constant() {
        let mut buf = RollingBuffer::new(100);
        for i in 0..50 {
            buf.push(i as f64, 42.0);
        }
        let stats = buf.statistics();
        assert!((stats.mean - 42.0).abs() < 1e-10);
        assert!((stats.min - 42.0).abs() < 1e-10);
        assert!((stats.max - 42.0).abs() < 1e-10);
        assert!(stats.rmsd < 1e-10);
    }

    #[test]
    fn persisted_state_save_load_roundtrip() {
        let dir = std::env::temp_dir().join("clara_test_state");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_state.json");

        let state = PersistedState {
            buffer_size: 500,
            sensitivities: [("dev1".to_string(), 2)].into_iter().collect(),
            device_order: vec!["dev1".to_string(), "dev2".to_string()],
        };
        state.save(&path);

        let loaded = PersistedState::load(&path);
        assert_eq!(loaded.buffer_size, 500);
        assert_eq!(loaded.sensitivities.get("dev1"), Some(&2));
        assert_eq!(loaded.device_order, vec!["dev1", "dev2"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persisted_state_load_missing_file() {
        let path = std::path::Path::new("/tmp/clara_test_nonexistent_state.json");
        let loaded = PersistedState::load(path);
        assert_eq!(loaded.buffer_size, 1000); // default
    }

    #[test]
    fn persisted_state_load_corrupt_file() {
        let dir = std::env::temp_dir().join("clara_test_corrupt");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("corrupt_state.json");

        std::fs::write(&path, "this is not valid json!!!").unwrap();
        let loaded = PersistedState::load(&path);
        assert_eq!(loaded.buffer_size, 1000); // default

        // Backup file should exist
        let backup = path.with_extension("json.corrupt");
        assert!(backup.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
