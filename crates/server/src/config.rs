use shared::config::{DeviceConfig, NetworkConfig};
use shared::messages::DeviceType;
use std::collections::HashMap;
use std::path::Path;

/// Load device configs from YAML file
pub fn load_device_configs(path: &Path) -> anyhow::Result<HashMap<String, DeviceConfig>> {
    let contents = std::fs::read_to_string(path)?;
    let configs: HashMap<String, DeviceConfig> = serde_yaml::from_str(&contents)?;

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
    }

    Ok(configs)
}

/// Load network config from YAML file
pub fn load_network_config(path: &Path) -> anyhow::Result<NetworkConfig> {
    let contents = std::fs::read_to_string(path)?;
    let config: NetworkConfig = serde_yaml::from_str(&contents)?;
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
}
