//! Bundles the configuration values the transport and tool layers need.
use super::Models;

/// Everything a `TransportRegistry` needs from `config.toml`.
#[derive(Clone, Debug, Default)]
pub struct RuntimeConfig {
    pub models: Models,
    pub default_model: String,
    pub outbound_proxy: Option<String>,
}
