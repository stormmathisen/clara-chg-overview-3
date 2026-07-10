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

/// A default value is either a single scalar (used for every sensitivity) or a
/// per-sensitivity array indexed by the current sensitivity.
///
/// The two variants are structurally distinct in YAML (a scalar `5` vs a sequence
/// `[5, 6]`), so `#[serde(untagged)]` resolves them unambiguously — unlike the
/// previous four-variant form where the float/int ordering was fragile. YAML
/// integers deserialize into `Scalar(f64)` / `Array(Vec<f64>)` just fine.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DefaultValue {
    Scalar(f64),
    Array(Vec<f64>),
}

impl DefaultValue {
    /// Get the value for a given sensitivity index as f64.
    /// For scalars, returns the scalar regardless of index.
    /// For arrays, returns the element at that index. Out-of-range falls back to
    /// the last element (or 0.0 if empty); config validation
    /// (`server::config::load_device_configs`) rejects arrays whose length does
    /// not match `sensitivities`, so the fallback is unreachable for valid config.
    pub fn for_sensitivity(&self, index: usize) -> f64 {
        match self {
            DefaultValue::Scalar(v) => *v,
            DefaultValue::Array(arr) => arr
                .get(index)
                .or_else(|| arr.last())
                .copied()
                .unwrap_or(0.0),
        }
    }

    /// If this value is a per-sensitivity array, its length; `None` for scalars.
    pub fn array_len(&self) -> Option<usize> {
        match self {
            DefaultValue::Scalar(_) => None,
            DefaultValue::Array(arr) => Some(arr.len()),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_value_scalar_is_constant_across_sensitivities() {
        let v: DefaultValue = serde_json::from_str("1027").unwrap();
        assert!(matches!(v, DefaultValue::Scalar(_)));
        assert_eq!(v.array_len(), None);
        assert_eq!(v.for_sensitivity(0), 1027.0);
        assert_eq!(v.for_sensitivity(5), 1027.0);
    }

    #[test]
    fn default_value_array_indexes_by_sensitivity() {
        let v: DefaultValue = serde_json::from_str("[0.1, 0.2, 0.3]").unwrap();
        assert!(matches!(v, DefaultValue::Array(_)));
        assert_eq!(v.array_len(), Some(3));
        assert!((v.for_sensitivity(0) - 0.1).abs() < 1e-9);
        assert!((v.for_sensitivity(2) - 0.3).abs() < 1e-9);
        // Out-of-range falls back to the last element.
        assert!((v.for_sensitivity(9) - 0.3).abs() < 1e-9);
    }
}
