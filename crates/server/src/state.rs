use shared::config::DeviceConfig;
use shared::messages::Stats;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Rolling buffer of (timestamp_secs, value) pairs with fixed capacity
#[derive(Clone, Debug)]
pub struct RollingBuffer {
    data: VecDeque<[f64; 2]>,
    capacity: usize,
}

impl RollingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, timestamp: f64, value: f64) {
        if self.data.len() >= self.capacity {
            self.data.pop_front();
        }
        self.data.push_back([timestamp, value]);
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
        let values: Vec<f64> = self.data.iter().map(|p| p[1]).collect();
        let n = values.len() as f64;
        let mean = values.iter().sum::<f64>() / n;
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let rmsd = (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
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

    pub fn len(&self) -> usize {
        self.data.len()
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
}

/// Global application state, shared across all tasks
pub struct InnerState {
    pub devices: Vec<DeviceState>,
    pub buffer_size: usize,
}

pub type AppState = Arc<RwLock<InnerState>>;

/// Persistent state saved to disk
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct PersistedState {
    pub buffer_size: usize,
    pub sensitivities: std::collections::HashMap<String, usize>,
}

impl PersistedState {
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Self {
                buffer_size: 1000,
                sensitivities: std::collections::HashMap::new(),
            })
    }

    pub fn save(&self, path: &std::path::Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}
