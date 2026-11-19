//! Parses `~/.cursor-byok-lite/config.toml` into the runtime configuration.
use std::{collections::HashSet, env, str::FromStr, time::Duration};

use serde::Deserialize;

use crate::{
    model::{ModelConfig, ModelConfigInput, ModelType},
    Error, Result,
};

use super::models::Models;

/// File name of the TOML configuration inside the application directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

const API_KEY_ENV_PREFIX: &str = "env:";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    default_model: String,
    #[serde(default)]
    server: ServerSection,
    #[serde(default)]
    stats: StatsSection,
    #[serde(default)]
    providers: Vec<ProviderEntry>,
    #[serde(default)]
    models: Vec<ModelEntry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerSection {
    #[serde(default = "default_listen_port")]
    listen_port: u16,
    #[serde(default = "default_proxy_port")]
    proxy_port: u16,
    #[serde(default = "default_request_timeout")]
    request_timeout_secs: u64,
    #[serde(default = "default_stream_idle_timeout")]
    stream_idle_timeout_secs: u64,
    #[serde(default)]
    outbound_proxy: Option<String>,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen_port: default_listen_port(),
            proxy_port: default_proxy_port(),
            request_timeout_secs: default_request_timeout(),
            stream_idle_timeout_secs: default_stream_idle_timeout(),
            outbound_proxy: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatsSection {
    #[serde(default)]
    retention_days: u32,
    #[serde(default = "default_stats_max_rows")]
    max_rows: u32,
}

impl Default for StatsSection {
    fn default() -> Self {
        Self {
            retention_days: 0,
            max_rows: default_stats_max_rows(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderEntry {
    name: String,
    #[serde(rename = "type")]
    provider_type: ModelType,
    #[serde(default)]
    base_url: String,
    api_key: String,
    #[serde(default)]
    custom_headers: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelEntry {
    provider: String,
    display_name: String,
    #[serde(default)]
    group_name: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    model_id: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    model_hash: Option<String>,
    #[serde(default)]
    use_full_url: bool,
    #[serde(default)]
    tooltip_data: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    thinking_effort: Option<String>,
    #[serde(default)]
    openai_extra_params: Option<serde_json::Value>,
    #[serde(default)]
    anthropic_extra_params: Option<serde_json::Value>,
    #[serde(default)]
    context_window_tokens: Option<u64>,
    #[serde(default)]
    max_completion_tokens: Option<u64>,
    #[serde(default)]
    anthropic_max_tokens: Option<u64>,
    #[serde(default)]
    thinking_budget_tokens: Option<u64>,
}

fn default_listen_port() -> u16 {
    super::DEFAULT_LISTEN_PORT
}

fn default_proxy_port() -> u16 {
    super::DEFAULT_PROXY_PORT
}

fn default_request_timeout() -> u64 {
    60 * 60
}

fn default_stream_idle_timeout() -> u64 {
    30 * 60
}

fn default_stats_max_rows() -> u32 {
    100_000
}

/// Parsed configuration, shared by the server for its whole lifetime.
#[derive(Clone, Debug)]
pub struct Config {
    pub default_model: String,
    pub listen_port: u16,
    pub proxy_port: u16,
    pub request_timeout: Duration,
    pub stream_idle_timeout: Duration,
    pub outbound_proxy: Option<String>,
    pub models: Models,
}

impl Config {
    /// Reads configuration from `path`, resolving `env:` credentials.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| Error::Config(format!("cannot read {}: {error}", path.display())))?;
        Self::parse(&text, &|name| env::var(name).ok())
    }

    /// Parses configuration text; `resolve` supplies `env:VAR` credentials.
    pub fn parse(text: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let file: FileConfig = toml::from_str(text)
            .map_err(|error| Error::Config(format!("invalid config.toml: {error}")))?;
        let providers = file
            .providers
            .iter()
            .map(|entry| Provider::from_entry(entry, resolve))
            .collect::<Result<Vec<_>>>()?;
        let providers = Providers::new(providers)?;
        let mut models = Vec::with_capacity(file.models.len());
        for (index, entry) in file.models.iter().enumerate() {
            let sort_order = i64::try_from(index + 1).unwrap_or(i64::MAX);
            models.push(providers.model(entry, sort_order)?);
        }
        if models.is_empty() {
            return Err(Error::Config(
                "config.toml must define at least one [[models]] entry".into(),
            ));
        }
        let models = Models::new(models)?;
        models.resolve_default(&file.default_model)?;
        if file.server.listen_port == file.server.proxy_port {
            return Err(Error::Config(
                "server.listen_port and server.proxy_port must differ".into(),
            ));
        }
        let stats = file.stats;
        if stats.retention_days == 0 && stats.max_rows == 0 {
            return Err(Error::Config(
                "stats.retention_days and stats.max_rows cannot both be zero".into(),
            ));
        }
        Ok(Self {
            default_model: file.default_model,
            listen_port: file.server.listen_port,
            proxy_port: file.server.proxy_port,
            request_timeout: Duration::from_secs(file.server.request_timeout_secs),
            stream_idle_timeout: Duration::from_secs(file.server.stream_idle_timeout_secs),
            outbound_proxy: normalize_outbound_proxy(file.server.outbound_proxy)?,
            models,
        })
    }
}

impl FromStr for Config {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text, &|name| env::var(name).ok())
    }
}

/// Resolves `[[models]]` blocks against their `[[providers]]` entries.
#[derive(Clone, Debug)]
struct Providers(Vec<Provider>);

#[derive(Clone, Debug)]
struct Provider {
    name: String,
    provider_type: ModelType,
    base_url: String,
    api_key: String,
    custom_headers: serde_json::Value,
}

impl Provider {
    fn from_entry(entry: &ProviderEntry, resolve: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let name = entry.name.trim();
        if name.is_empty() {
            return Err(Error::Config("provider name cannot be empty".into()));
        }
        Ok(Self {
            name: name.into(),
            provider_type: entry.provider_type,
            base_url: entry.base_url.trim().into(),
            api_key: resolve_api_key(&entry.api_key, resolve)?,
            custom_headers: serde_json::Value::Object(entry.custom_headers.clone()),
        })
    }
}

impl Providers {
    fn new(providers: Vec<Provider>) -> Result<Self> {
        let mut names = HashSet::with_capacity(providers.len());
        for provider in &providers {
            if !names.insert(provider.name.as_str()) {
                return Err(Error::Config(format!(
                    "duplicate provider name: {}",
                    provider.name
                )));
            }
        }
        Ok(Self(providers))
    }

    fn get(&self, name: &str) -> Result<&Provider> {
        self.0
            .iter()
            .find(|provider| provider.name == name)
            .ok_or_else(|| Error::Config(format!("unknown provider: {name}")))
    }

    fn model(&self, entry: &ModelEntry, sort_order: i64) -> Result<ModelConfig> {
        let provider = self.get(entry.provider.trim())?;
        let base_url = entry
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&provider.base_url);
        let openai = provider.provider_type == ModelType::OpenAi;
        let openai_endpoint = match entry.endpoint.as_deref().map(str::trim) {
            Some(endpoint) if !endpoint.is_empty() => endpoint.to_string(),
            _ if openai => crate::model::OPENAI_CHAT_ENDPOINT.to_string(),
            _ => String::new(),
        };
        let tooltip_data = entry
            .tooltip_data
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(entry.display_name.trim())
            .to_string();
        let group_name = entry
            .group_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| Some(provider.name.clone()));
        let input = ModelConfigInput {
            sort_order,
            display_name: entry.display_name.trim().into(),
            group_name,
            model_type: provider.provider_type,
            base_url: base_url.into(),
            use_full_url: entry.use_full_url,
            api_key: provider.api_key.clone(),
            tooltip_data,
            model_id: entry.model_id.trim().into(),
            reasoning_effort: entry.reasoning_effort.clone(),
            openai_endpoint,
            openai_extra_params_enabled: openai && entry.openai_extra_params.is_some(),
            openai_extra_params: entry
                .openai_extra_params
                .clone()
                .unwrap_or_else(empty_object),
            custom_headers_enabled: provider
                .custom_headers
                .as_object()
                .is_some_and(|headers| !headers.is_empty()),
            custom_headers: provider.custom_headers.clone(),
            anthropic_extra_params_enabled: !openai && entry.anthropic_extra_params.is_some(),
            anthropic_extra_params: entry
                .anthropic_extra_params
                .clone()
                .unwrap_or_else(empty_object),
            context_window_tokens: entry.context_window_tokens,
            max_completion_tokens: entry.max_completion_tokens,
            anthropic_max_tokens: entry.anthropic_max_tokens,
            anthropic_thinking_effort: entry.thinking_effort.clone(),
            thinking_budget_tokens: entry.thinking_budget_tokens,
        };
        ModelConfig::from_input(input, entry.model_hash.as_deref())
    }
}

fn resolve_api_key(value: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::Config("provider api_key cannot be empty".into()));
    }
    let Some(variable) = trimmed.strip_prefix(API_KEY_ENV_PREFIX) else {
        return Ok(trimmed.into());
    };
    let variable = variable.trim();
    if variable.is_empty() {
        return Err(Error::Config(
            "provider api_key cannot be \"env:\" without a variable name".into(),
        ));
    }
    resolve(variable)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::Config(format!(
                "api_key references environment variable {variable}, which is not set"
            ))
        })
}

fn normalize_outbound_proxy(proxy: Option<String>) -> Result<Option<String>> {
    let Some(proxy) = proxy.map(|value| value.trim().to_string()) else {
        return Ok(None);
    };
    if proxy.is_empty() {
        return Ok(None);
    }
    let url = url::Url::parse(&proxy)
        .map_err(|error| Error::Config(format!("invalid server.outbound_proxy: {error}")))?;
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") {
        return Err(Error::Config(
            "server.outbound_proxy must use http, https, socks5, or socks5h".into(),
        ));
    }
    reqwest::Proxy::all(&proxy)?;
    Ok(Some(proxy))
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
default_model = "deepseek-v4.1-flash"

[server]
listen_port = 51867
proxy_port = 52149
request_timeout_secs = 120
stream_idle_timeout_secs = 60
outbound_proxy = "http://127.0.0.1:7890"

[stats]
retention_days = 30
max_rows = 1000

[[providers]]
name = "lan-7863"
type = "openai"
base_url = "http://192.168.11.46:7863/v1"
api_key = "lan-plaintext"
custom_headers = { "User-Agent" = "cursor-byok" }

[[providers]]
name = "ziyouxie"
type = "openai"
base_url = "https://api.ziyouxie.online"
api_key = "env:ZIYOUXIE_API_KEY"

[[models]]
provider = "lan-7863"
display_name = "deepseek-v4.1-flash"
model_id = "deepseek-v4.1-flash"
endpoint = "/v1/chat/completions"
model_hash = "83f1333d63840a0c"
reasoning_effort = "medium"
context_window_tokens = 128000
max_completion_tokens = 16384

[[models]]
provider = "ziyouxie"
display_name = "gpt-6-astra"
model_id = "gpt-6-astra"
endpoint = "/v1/responses"
"#;

    fn resolve(name: &str) -> Option<String> {
        (name == "ZIYOUXIE_API_KEY").then(|| "resolved-secret".into())
    }

    #[test]
    fn parses_the_documented_configuration() {
        let config = Config::parse(EXAMPLE, &resolve).unwrap();
        assert_eq!(config.default_model, "deepseek-v4.1-flash");
        assert_eq!(config.listen_port, 51_867);
        assert_eq!(config.proxy_port, 52_149);
        assert_eq!(config.request_timeout, Duration::from_secs(120));
        assert_eq!(config.stream_idle_timeout, Duration::from_secs(60));
        assert_eq!(
            config.outbound_proxy.as_deref(),
            Some("http://127.0.0.1:7890")
        );

        let models = config.models.all();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model_hash, "83f1333d63840a0c");
        assert_eq!(models[0].api_key, "lan-plaintext");
        assert_eq!(models[0].group_name.as_deref(), Some("lan-7863"));
        assert_eq!(models[0].sort_order, 1);
        assert_eq!(models[0].reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(models[0].context_window_tokens, Some(128_000));
        assert_eq!(models[0].max_completion_tokens, Some(16_384));
        assert!(models[0].custom_headers_enabled);
        assert_eq!(
            models[0].request_url().unwrap(),
            "http://192.168.11.46:7863/v1/chat/completions"
        );

        let generated = &models[1];
        assert_eq!(generated.api_key, "resolved-secret");
        assert_eq!(generated.sort_order, 2);
        assert_eq!(generated.model_hash.len(), 16);
        assert_eq!(
            generated.request_url().unwrap(),
            "https://api.ziyouxie.online/v1/responses"
        );
    }

    #[test]
    fn generated_hashes_match_the_legacy_algorithm() {
        let config = Config::parse(EXAMPLE, &resolve).unwrap();
        let generated = config.models.all()[1].clone();
        let legacy = crate::model::model_hash(&generated.to_input()).unwrap();
        assert_eq!(generated.model_hash, legacy);
    }

    #[test]
    fn rejects_unset_env_references() {
        let error = Config::parse(EXAMPLE, &|_| None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("ZIYOUXIE_API_KEY, which is not set"),
            "{error}"
        );
    }

    #[test]
    fn rejects_unknown_reasoning_effort() {
        let error = Config::parse(&EXAMPLE.replace("medium", "turbo"), &resolve).unwrap_err();
        assert!(
            error.to_string().contains("unsupported reasoning effort"),
            "{error}"
        );
    }

    #[test]
    fn rejects_missing_required_fields() {
        let error = Config::parse("[[models]]\nprovider = \"none\"\n", &resolve).unwrap_err();
        assert!(error.to_string().contains("missing field"), "{error}");

        let error = Config::parse(
            "[[providers]]\nname = \"p\"\ntype = \"openai\"\napi_key = \"k\"\n[[models]]\nprovider = \"p\"\ndisplay_name = \"m\"\nmodel_id = \"m\"\n",
            &resolve,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("missing field `default_model`"),
            "{error}"
        );
    }

    #[test]
    fn rejects_unknown_provider_references() {
        let error = Config::parse(
            &EXAMPLE.replace("provider = \"ziyouxie\"", "provider = \"missing\""),
            &resolve,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown provider"), "{error}");
    }

    #[test]
    fn rejects_an_unknown_default_model() {
        let error = Config::parse(
            &EXAMPLE.replace(
                "default_model = \"deepseek-v4.1-flash\"",
                "default_model = \"nope\"",
            ),
            &resolve,
        )
        .unwrap_err();
        assert!(error.to_string().contains("default_model nope"), "{error}");
    }

    #[test]
    fn defaults_apply_when_sections_are_omitted() {
        let config = Config::parse(
            "default_model = \"m\"\n[[providers]]\nname = \"p\"\ntype = \"openai\"\nbase_url = \"https://example.com\"\napi_key = \"k\"\n[[models]]\nprovider = \"p\"\ndisplay_name = \"m\"\nmodel_id = \"m\"\n",
            &resolve,
        )
        .unwrap();
        assert_eq!(config.listen_port, 51_866);
        assert_eq!(config.proxy_port, 52_148);
        assert_eq!(config.request_timeout, Duration::from_secs(3_600));
        assert_eq!(config.stream_idle_timeout, Duration::from_secs(1_800));
        assert_eq!(config.outbound_proxy, None);
        assert_eq!(
            config.models.all()[0].openai_endpoint,
            "/v1/chat/completions"
        );
    }
}
