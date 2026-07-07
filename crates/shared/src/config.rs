use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::messages::DeviceType;

/// Configuration for a single charge device, parsed from YAML
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceConfig {
    #[serde(rename = "type")]
    pub device_type: DeviceType,
    pub digitizer: String,
    pub ip: String,
    #[serde(default)]
    pub sensitivities: Vec<u8>,
    pub pvs: HashMap<String, String>,
    pub defaults: HashMap<String, DefaultValue>,
}

/// A default value can be a scalar (f64), an integer, or a per-sensitivity array
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DefaultValue {
    Float(f64),
    Int(i64),
    FloatArray(Vec<f64>),
    IntArray(Vec<i64>),
}

impl DefaultValue {
    /// Get the value for a given sensitivity index as f64.
    /// For scalars, returns the scalar regardless of index.
    /// For arrays, returns the element at that index (or the last element if out of bounds).
    pub fn for_sensitivity(&self, index: usize) -> f64 {
        match self {
            DefaultValue::Float(v) => *v,
            DefaultValue::Int(v) => *v as f64,
            DefaultValue::FloatArray(arr) => {
                arr.get(index).or_else(|| arr.last()).copied().unwrap_or(0.0)
            }
            DefaultValue::IntArray(arr) => {
                arr.get(index)
                    .or_else(|| arr.last())
                    .copied()
                    .unwrap_or(0) as f64
            }
        }
    }
}

/// Network configuration for EPICS CA
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(rename = "PHYSICAL")]
    pub physical: HashMap<String, String>,
    #[serde(rename = "VIRTUAL")]
    pub virtual_: HashMap<String, String>,
    #[serde(rename = "CATAP_PATH")]
    pub catap_path: Option<String>,
}
