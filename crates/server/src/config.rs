use shared::config::{DeviceConfig, NetworkConfig};
use shared::messages::DeviceType;
use std::collections::HashMap;
use std::path::Path;

/// Load device configs from YAML file
pub fn load_device_configs(path: &Path) -> anyhow::Result<HashMap<String, DeviceConfig>> {
    let contents = std::fs::read_to_string(path)?;
    let configs: HashMap<String, DeviceConfig> = serde_norway::from_str(&contents)?;

    // Validate each device config
    for (name, config) in &configs {
        if config.device_type != DeviceType::Ict {
            anyhow::ensure!(
                !config.sensitivities.is_empty(),
                "Device {name}: sensitivities array must not be empty"
            );
        }
        anyhow::ensure!(
            config.pvs.contains_key("charge"),
            "Device {name}: missing required 'charge' PV"
        );
        // Saturation limits are indexed by the current sensitivity, so a non-empty
        // array must match `sensitivities` (empty disables saturation checking).
        if !config.saturation_charges.is_empty() {
            anyhow::ensure!(
                config.saturation_charges.len() == config.sensitivities.len(),
                "Device {name}: saturation_charges has {} values but there are {} sensitivities",
                config.saturation_charges.len(),
                config.sensitivities.len()
            );
        }
        // Per-sensitivity default arrays are indexed by the current sensitivity, so
        // their length must match `sensitivities`. A mismatch would otherwise be
        // masked at runtime by `DefaultValue::for_sensitivity`'s last-element fallback.
        let n_sens = config.sensitivities.len();
        for (key, value) in &config.defaults {
            if let Some(len) = value.array_len() {
                // Devices without sensitivities (ICT) have nothing to index an array by.
                anyhow::ensure!(
                    n_sens > 0,
                    "Device {name}: default '{key}' is a per-sensitivity array, but the \
                     device has no sensitivities (use a scalar default)"
                );
                anyhow::ensure!(
                    len == n_sens,
                    "Device {name}: default '{key}' has {len} values but there are \
                     {n_sens} sensitivities (per-sensitivity arrays must match)"
                );
            }
        }
    }

    Ok(configs)
}

/// Load network config from YAML file
pub fn load_network_config(path: &Path) -> anyhow::Result<NetworkConfig> {
    let contents = std::fs::read_to_string(path)?;
    let config: NetworkConfig = serde_norway::from_str(&contents)?;
    Ok(config)
}

/// Apply EPICS environment variables from network config
pub fn apply_epics_env(network: &NetworkConfig, virtual_mode: bool) {
    let vars = if virtual_mode {
        &network.virtual_
    } else {
        &network.physical
    };
    for (key, value) in vars {
        std::env::set_var(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_yaml(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_config.yaml");
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn valid_config_loads() {
        let yaml = r#"
test-device:
  type: wcm
  digitizer: "DIG01"
  ip: "192.168.1.1"
  sensitivities: [3, 4]
  pvs:
    charge: "TEST:CHARGE"
    corrA: "TEST:CORRA"
  defaults:
    corrA: 1.0
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let configs = load_device_configs(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert!(configs.contains_key("test-device"));
    }

    #[test]
    fn empty_sensitivities_rejected() {
        let yaml = r#"
test-device:
  type: wcm
  digitizer: "DIG01"
  ip: "192.168.1.1"
  sensitivities: []
  pvs:
    charge: "TEST:CHARGE"
  defaults: {}
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let result = load_device_configs(&path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("sensitivities"));
    }

    #[test]
    fn saturation_charges_length_mismatch_rejected() {
        let yaml = r#"
test-device:
  type: fcup
  digitizer: "DIG01"
  ip: "192.168.1.1"
  sensitivities: [0, 1, 2]
  saturation_charges: [10, 20]
  pvs:
    charge: "TEST:CHARGE"
  defaults: {}
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let err = load_device_configs(&path).unwrap_err().to_string();
        assert!(err.contains("saturation_charges"), "{err}");
    }

    #[test]
    fn missing_charge_pv_rejected() {
        let yaml = r#"
test-device:
  type: wcm
  digitizer: "DIG01"
  ip: "192.168.1.1"
  sensitivities: [3]
  pvs:
    corrA: "TEST:CORRA"
  defaults: {}
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let result = load_device_configs(&path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("charge"));
    }

    #[test]
    fn ict_config_loads_without_sensitivities() {
        let yaml = r#"
test-ict:
  type: ict
  digitizer: "DIG01"
  ip: ""
  pvs:
    charge: "TEST:CHARGE"
    HoldDelay: "TEST:HOLDDELAY"
  defaults:
    charge: 0.0
    HoldDelay: 0
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let configs = load_device_configs(&path).unwrap();
        assert_eq!(configs.len(), 1);
        let cfg = configs.get("test-ict").unwrap();
        assert!(cfg.sensitivities.is_empty());
    }

    #[test]
    fn ict_with_per_sensitivity_array_rejected() {
        // An ICT has no sensitivities, so an array default cannot be indexed.
        let yaml = r#"
test-ict:
  type: ict
  digitizer: "DIG01"
  ip: ""
  pvs:
    charge: "TEST:CHARGE"
    HoldDelay: "TEST:HOLDDELAY"
  defaults:
    HoldDelay: [1, 2]
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let err = load_device_configs(&path).unwrap_err().to_string();
        assert!(err.contains("HoldDelay"), "{err}");
        assert!(err.contains("no sensitivities"), "{err}");
    }

    #[test]
    fn per_sensitivity_array_matching_length_loads() {
        // Two sensitivities, a scalar default and a matching 2-element array default.
        let yaml = r#"
test-device:
  type: wcm
  digitizer: "DIG01"
  ip: "192.168.1.1"
  sensitivities: [3, 4]
  pvs:
    charge: "TEST:CHARGE"
    corrA: "TEST:CORRA"
  defaults:
    base_low: 1025
    corrA: [0.089, 0.273]
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let configs = load_device_configs(&path).unwrap();
        let dev = &configs["test-device"];
        // Array indexes by sensitivity; scalar is constant.
        assert!((dev.defaults["corrA"].for_sensitivity(1) - 0.273).abs() < 1e-9);
        assert!((dev.defaults["base_low"].for_sensitivity(1) - 1025.0).abs() < 1e-9);
    }

    #[test]
    fn per_sensitivity_array_length_mismatch_rejected() {
        // Three sensitivities but only two calibration values — this is exactly the
        // silent-fallback bug the validation is meant to catch.
        let yaml = r#"
test-device:
  type: wcm
  digitizer: "DIG01"
  ip: "192.168.1.1"
  sensitivities: [3, 4, 5]
  pvs:
    charge: "TEST:CHARGE"
    corrA: "TEST:CORRA"
  defaults:
    corrA: [0.089, 0.273]
"#;
        let (_dir, path) = write_temp_yaml(yaml);
        let result = load_device_configs(&path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("corrA"));
        assert!(err_msg.contains("sensitivities"));
    }
}
