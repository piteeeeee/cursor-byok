//! Wires the parsed configuration into process-level server settings.
use std::{net::SocketAddr, path::PathBuf};

use crate::{Error, Result};

use super::{schema::Config, Paths};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub request_url: String,
    pub api_key: String,
    pub custom_headers: reqwest::header::HeaderMap,
    pub max_output_tokens: Option<u64>,
    pub request_timeout: std::time::Duration,
    pub allowed_body_fields: Option<std::collections::HashSet<String>>,
}

#[derive(Clone)]
pub enum ConsoleSource {
    Directory(PathBuf),
    Proxy(url::Url),
}

/// Everything the process needs at startup, derived from `config.toml`.
#[derive(Clone)]
pub struct Settings {
    pub config: Config,
    pub paths: Paths,
    pub listen_addr: SocketAddr,
    pub console: Option<ConsoleSource>,
}

impl Settings {
    /// Loads `~/.cursor-byok-lite/config.toml`, migrating on first start.
    pub fn load() -> Result<Self> {
        let paths = Paths::managed()?;
        let config = super::load(&paths)?;
        Self::from_config(config, paths)
    }

    /// Binds a parsed configuration to its listen address and database URL.
    pub fn from_config(config: Config, paths: Paths) -> Result<Self> {
        let listen_addr = SocketAddr::from(([127, 0, 0, 1], config.listen_port));
        Ok(Self {
            config,
            paths,
            listen_addr,
            console: console_from_env()?,
        })
    }
}

fn console_from_env() -> Result<Option<ConsoleSource>> {
    let console_dir = std::env::var_os("CURSOR_CONSOLE_DIR").map(PathBuf::from);
    let console_proxy = std::env::var("CURSOR_CONSOLE_PROXY")
        .ok()
        .map(|value| {
            value
                .parse()
                .map_err(|error| Error::Config(format!("invalid CURSOR_CONSOLE_PROXY: {error}")))
        })
        .transpose()?;
    match (console_dir, console_proxy) {
        (Some(_), Some(_)) => Err(Error::Config(
            "CURSOR_CONSOLE_DIR and CURSOR_CONSOLE_PROXY cannot both be set".into(),
        )),
        (Some(directory), None) => Ok(Some(ConsoleSource::Directory(directory))),
        (None, Some(proxy)) => Ok(Some(ConsoleSource::Proxy(proxy))),
        (None, None) => Ok(None),
    }
}
