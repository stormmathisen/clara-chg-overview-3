use shared::config::{DeviceConfig, NetworkConfig};
use std::collections::HashMap;
use std::path::Path;

/// Load device configs from YAML file
pub fn load_device_configs(path: &Path) -> anyhow::Result<HashMap<String, DeviceConfig>> {
    let contents = std::fs::read_to_string(path)?;
    let configs: HashMap<String, DeviceConfig> = serde_yaml::from_str(&contents)?;
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
