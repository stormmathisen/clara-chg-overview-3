use shared::config::DeviceConfig;
use shared::messages::Stats;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::warn;

/// Rolling buffer of (timestamp_secs, value) pairs with fixed capacity
#[derive(Clone, Debug)]
pub struct RollingBuffer {
    data: VecDeque<[f64; 2]>,
    capacity: usize,
    /// Monotonic count of all pushes ever, independent of capacity. Used to detect
    /// "N fresh samples have arrived" even when the buffer is smaller than N.
    total_pushed: u64,
}

impl RollingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(capacity),
            capacity,
            total_pushed: 0,
        }
    }

    pub fn push(&mut self, timestamp: f64, value: f64) {
        if self.data.len() >= self.capacity {
            self.data.pop_front();
        }
        self.data.push_back([timestamp, value]);
        self.total_pushed = self.total_pushed.wrapping_add(1);
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
            self.data.pop_front();
        }
    }

    pub fn statistics(&self) -> Stats {
        if self.data.is_empty() {
            return Stats::default();
        }
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        let n = self.data.len() as f64;

        for p in &self.data {
            let v = p[1];
            sum += v;
            sum_sq += v * v;
            if v < min { min = v; }
            if v > max { max = v; }
        }

        let mean = sum / n;
        let rmsd = ((sum_sq / n) - (mean * mean)).max(0.0).sqrt();
        Stats { mean, min, max, rmsd }
    }

    pub fn as_points(&self) -> Vec<[f64; 2]> {
        self.data.iter().copied().collect()
    }

    pub fn clear(&mut self) {
        self.data.clear();
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
