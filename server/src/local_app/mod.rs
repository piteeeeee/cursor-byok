//! Exposes the local desktop application integration.
mod account;
mod ca;
mod process;
mod proxy;
mod settings;
mod state;

use std::{net::SocketAddr, sync::Arc};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{config::ConfigStore, Error, Result};

use self::{ca::CaManager, proxy::ProxyRuntime};

pub(crate) fn proxy_host_allowed(host: &str) -> bool {
    proxy::is_cursor_host(host)
}

pub(crate) fn request_uses_local_cursor_token(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(account::is_local_cursor_authorization)
}

#[cfg(test)]
pub(crate) fn local_cursor_authorization() -> String {
    format!("Bearer {}", account::local_token().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaState {
    Missing,
    Untrusted,
    Ready,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationState {
    Disabled,
    Enabled,
    Degraded,
}

/// Whether the Cursor proxy is currently taking over, and on which ports.
#[derive(Clone, Debug, Serialize)]
pub struct CursorHarnessStatus {
    pub ca: CaState,
    pub configured_models: usize,
    pub integration: IntegrationState,
    pub settings_applied: bool,
    /// `Some` while the MITM proxy is listening for Cursor traffic.
    pub proxy_url: Option<String>,
    /// The local service port the proxy forwards Cursor calls to.
    pub service_port: Option<u16>,
    /// The port Cursor is pointed at; also the port to report in the menu.
    pub proxy_port: Option<u16>,
    pub ca_install_command: Option<String>,
}

impl CursorHarnessStatus {
    /// Whether the proxy is running and Cursor is configured to use it.
    pub fn is_taking_over(&self) -> bool {
        self.integration == IntegrationState::Enabled
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct SetEnabled {
    pub enabled: bool,
}

/// Enables and disables the Cursor takeover: writes `settings.json`, runs the
/// MITM proxy, and restores everything on the way out.
#[derive(Clone)]
pub struct CursorHarness {
    inner: Arc<Inner>,
}

struct Inner {
    state_path: std::path::PathBuf,
    proxy_port: u16,
    configured_models: usize,
    ca: CaManager,
    ca_initialization: Mutex<()>,
    backend_addr: RwLock<Option<SocketAddr>>,
    proxy: Mutex<ProxyRuntime>,
}

impl CursorHarness {
    /// Builds a harness for the paths and ports in `config`.
    pub fn new(config: &ConfigStore) -> Result<Self> {
        let ca_dir = config.paths().ca_dir.clone();
        Ok(Self {
            inner: Arc::new(Inner {
                state_path: ca_dir.with_file_name("state.json"),
                proxy_port: config.proxy_port(),
                configured_models: config.models().all().len(),
                ca: CaManager::managed(ca_dir)?,
                ca_initialization: Mutex::new(()),
                backend_addr: RwLock::new(None),
                proxy: Mutex::new(ProxyRuntime::default()),
            }),
        })
    }

    /// Tells the harness where the local service listens, so the proxy can forward to it.
    pub fn set_backend_addr(&self, addr: SocketAddr) {
        *self.inner.backend_addr.write() = Some(addr);
    }

    pub fn service_port(&self) -> Option<u16> {
        self.inner.backend_addr.read().map(|addr| addr.port())
    }

    pub async fn proxy_port(&self) -> Option<u16> {
        self.inner.proxy.lock().await.port()
    }

    /// Removes proxy keys this build left behind in Cursor's `settings.json`.
    pub async fn cleanup_stale_settings(&self) -> Result<()> {
        settings::clear_stale_managed_settings()
    }

    /// Applies the saved choice: takes over again unless the user paused it.
    pub async fn status(&self) -> Result<CursorHarnessStatus> {
        self.status_with_ca(self.inner.ca.state()?, true).await
    }

    /// Reports the current state without enabling or rewriting anything.
    pub async fn report(&self) -> Result<CursorHarnessStatus> {
        self.status_with_ca(self.inner.ca.state()?, false).await
    }

    async fn status_with_ca(
        &self,
        ca: CaState,
        apply_saved_choice: bool,
    ) -> Result<CursorHarnessStatus> {
        let configured_models = self.inner.configured_models;
        if apply_saved_choice
            && !state::paused(&self.inner.state_path)?
            && matches!(ca, CaState::Ready)
            && self.inner.backend_addr.read().is_some()
        {
            self.enable().await?;
        }
        let proxy = self.inner.proxy.lock().await;
        let proxy_url = proxy.url();
        let settings_applied = proxy_url
            .as_deref()
            .map(settings::settings_match)
            .transpose()?
            .unwrap_or(false);
        let integration = match (proxy.running(), settings_applied) {
            (false, false) => IntegrationState::Disabled,
            (true, true) => IntegrationState::Enabled,
            _ => IntegrationState::Degraded,
        };
        Ok(CursorHarnessStatus {
            ca,
            configured_models,
            integration,
            settings_applied,
            proxy_url,
            service_port: self.service_port(),
            proxy_port: proxy.port(),
            ca_install_command: self.inner.ca.install_command(),
        })
    }

    /// Creates the CA when it is missing, so the first enable can succeed.
    pub async fn initialize_ca(&self) -> Result<CursorHarnessStatus> {
        let _initialization = self.inner.ca_initialization.lock().await;
        let manager = self.inner.ca.clone();
        tokio::task::spawn_blocking(move || manager.initialize_local())
            .await
            .map_err(|error| Error::Store(format!("CA initialization task failed: {error}")))??;
        self.status().await
    }

    /// Turns takeover on or off and remembers the user's explicit choice.
    pub async fn set_enabled(&self, enabled: bool) -> Result<CursorHarnessStatus> {
        state::set_paused(&self.inner.state_path, !enabled)?;
        if enabled {
            self.enable().await?;
        } else {
            self.disable().await?;
        }
        self.status().await
    }

    /// Points Cursor at the local proxy and starts listening for its traffic.
    pub async fn enable(&self) -> Result<()> {
        if !matches!(self.inner.ca.state()?, CaState::Ready) {
            return Err(Error::Config(
                "initialize and trust the CA before enabling Cursor".into(),
            ));
        }
        let backend_addr = self
            .inner
            .backend_addr
            .read()
            .ok_or_else(|| Error::Config("the local service is not listening yet".into()))?;
        let mut proxy = self.inner.proxy.lock().await;
        let settings_applied = proxy
            .url()
            .as_deref()
            .map(settings::settings_match)
            .transpose()?
            .unwrap_or(false);
        if !settings_applied {
            // Terminating Cursor only makes the freshly written http.proxy take effect
            // sooner; it is optional, so a failed probe or kill must not block takeover.
            if let Err(error) = process::terminate_cursor().await {
                tracing::warn!(%error, "could not terminate Cursor before applying proxy settings");
            }
        }
        if proxy.running() {
            if let Some(url) = proxy.url() {
                apply_cursor_configuration(&url).await?;
            }
            return Ok(());
        }
        let ca = self.inner.ca.load()?;
        let requested_port = self.inner.proxy_port;
        let (url, _actual_port) = proxy.start(backend_addr, ca, requested_port).await?;
        if let Err(error) = apply_cursor_configuration(&url).await {
            proxy.stop().await;
            return Err(error);
        }
        Ok(())
    }

    /// Restores Cursor's `settings.json` and stops the proxy.
    pub async fn disable(&self) -> Result<()> {
        settings::clear_proxy_settings()?;
        self.inner.proxy.lock().await.stop().await;
        Ok(())
    }
}

async fn apply_cursor_configuration(proxy_url: &str) -> Result<()> {
    account::inject_if_missing().await?;
    settings::write_proxy_settings(proxy_url)
}

#[cfg(test)]
mod pause_tests {
    use super::*;
    use crate::config::{Config, ConfigStore, Paths};

    fn config_store(home: &std::path::Path) -> ConfigStore {
        let paths = Paths::at(home);
        crate::config::ensure_app_dir(&paths).unwrap();
        let config = Config::parse(
            r#"default_model = "Alpha"
[server]
listen_port = 0
proxy_port = 1
[[providers]]
name = "p"
type = "openai"
base_url = "http://127.0.0.1:1/v1"
api_key = "k"
[[models]]
provider = "p"
display_name = "Alpha"
model_id = "alpha"
"#,
            &|_| Some("key".into()),
        )
        .unwrap();
        ConfigStore::in_memory(paths, config.models, config.default_model).unwrap()
    }

    #[tokio::test]
    async fn explicit_pause_survives_restart_even_when_ca_and_backend_are_ready() {
        let home = tempfile::tempdir().unwrap();
        let config = config_store(home.path());
        let path = home.path().join(".cursor-byok-lite/state.json");
        let first = CursorHarness::new(&config).unwrap();
        assert!(!state::paused(&path).unwrap());
        // Persist the same choice set_enabled(false) makes without touching Cursor settings.
        state::set_paused(&first.inner.state_path, true).unwrap();
        drop(first);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"paused":true}"#
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let restarted = CursorHarness::new(&config).unwrap();
        restarted.set_backend_addr("127.0.0.1:0".parse().unwrap());
        // No real trusted CA or process commands are used: entering enable() would fail.
        let status = restarted
            .status_with_ca(CaState::Ready, true)
            .await
            .unwrap();
        assert!(matches!(status.integration, IntegrationState::Disabled));
        assert_eq!(status.service_port, Some(0));
        assert!(matches!(
            restarted.status().await.unwrap().integration,
            IntegrationState::Disabled
        ));
        assert!(restarted.proxy_port().await.is_none());
    }
}
