use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct PortSettings {
    pub proxy_port: u16,
    pub service_port: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct TokenPricingSettings {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: f64,
    pub cache_write_per_million: f64,
}

impl Default for TokenPricingSettings {
    fn default() -> Self {
        Self {
            input_per_million: 5.0,
            output_per_million: 25.0,
            cache_read_per_million: 0.5,
            cache_write_per_million: 6.25,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    #[default]
    Default,
    Custom,
}

impl ProxyMode {
    pub fn is_custom(self) -> bool {
        self == Self::Custom
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DesktopSettings {
    #[serde(default)]
    pub silent_start: bool,
    #[serde(default = "default_true")]
    pub show_dock_icon: bool,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            silent_start: false,
            show_dock_icon: true,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProxySettings {
    pub mode: ProxyMode,
    pub address: String,
    pub auth_enabled: bool,
    pub username: String,
    pub has_password: bool,
}

#[derive(Serialize)]
pub struct StatisticsStorage {
    pub call_count: i64,
}
