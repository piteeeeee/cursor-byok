//! Bundles the configuration values the transport and tool layers need.
use std::path::PathBuf;

use super::Models;

/// Everything a `TransportRegistry` needs from `config.toml`.
#[derive(Clone, Debug, Default)]
pub struct RuntimeConfig {
    pub models: Models,
    pub default_model: String,
    pub outbound_proxy: Option<String>,
    pub ca_dir: PathBuf,
}

impl RuntimeConfig {
    pub fn new(
        models: Models,
        default_model: String,
        outbound_proxy: Option<String>,
        ca_dir: PathBuf,
    ) -> Self {
        Self {
            models,
            default_model,
            outbound_proxy,
            ca_dir,
        }
    }
}
