//! Assembles the server: one value that owns every component and its lifetime.
//!
//! `App::new` builds the object graph; `App::bind` starts accepting connections.
//! Nothing here is tied to a GUI: a menu bar app, a CLI, or an embedding crate
//! drives the same `App`.
use std::{future::IntoFuture, time::Duration};

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    api,
    config::ConfigStore,
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        transport::TransportRegistry,
    },
    local_app::CursorHarness,
    provider::ProviderRouter,
    search::WebCache,
    store::{MemoryStore, UsageStore},
    Result,
};

/// The headless Cursor proxy and its control surface.
#[derive(Clone)]
pub struct App {
    config: ConfigStore,
    usage: UsageStore,
    registry: TransportRegistry,
    harness: CursorHarness,
    router: axum::Router,
}

impl App {
    /// Builds every component from an already loaded configuration.
    pub async fn new(config: ConfigStore) -> Result<Self> {
        let store = MemoryStore::default();
        let usage = UsageStore::open(
            &config.paths().app_dir.join("usage.db"),
            config.retention_days(),
            config.max_rows(),
        )
        .await?;
        let assets = PromptAssets::embedded()?;
        let compiler = PromptCompiler::new(assets);
        let clients = crate::network::NetworkClients::new(config.outbound_proxy());
        let provider = std::sync::Arc::new(ProviderRouter::new(
            usage.clone(),
            config.clone(),
            clients.clone(),
        ));
        let registry = TransportRegistry::with_web_cache(
            store.clone(),
            config.clone(),
            provider,
            compiler,
            WebCache::at(config.paths().app_dir.join("cache/web"))?,
        );
        let harness = CursorHarness::new(&config)?;
        let router = api::router(registry.clone(), clients)?;
        Ok(Self {
            config,
            usage,
            registry,
            harness,
            router,
        })
    }

    /// The live configuration, for reading and switching the default model.
    pub fn config(&self) -> &ConfigStore {
        &self.config
    }

    /// Today's and this month's token totals.
    pub fn usage(&self) -> &UsageStore {
        &self.usage
    }

    /// Enable/disable takeover, and inspect the CA and proxy state.
    pub fn harness(&self) -> &CursorHarness {
        &self.harness
    }

    /// Reserves the service port now, so the caller can learn it before serving.
    pub async fn bind(&self) -> Result<TcpListener> {
        TcpListener::bind(self.config.listen_addr())
            .await
            .map_err(Into::into)
    }

    /// Serves until `shutdown` is cancelled, then disables the Cursor takeover.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let listener = self.bind().await?;
        self.serve_on(listener, shutdown).await
    }

    /// Serves on an already bound listener, so the address is known upfront.
    ///
    /// Takes the `App` by value, since serving owns the router for its lifetime.
    pub async fn serve_on(self, listener: TcpListener, shutdown: CancellationToken) -> Result<()> {
        let address = listener.local_addr()?;
        self.registry.web_cache().set_service_addr(address);
        self.harness.set_backend_addr(address);
        tracing::info!(%address, "cursor server listening");
        let registry = self.registry;
        let harness = self.harness;
        let graceful = shutdown.clone();
        let server = axum::serve(listener, self.router)
            .with_graceful_shutdown(async move {
                graceful.cancelled().await;
            })
            .into_future();
        tokio::pin!(server);

        tokio::select! {
            result = &mut server => {
                if let Err(error) = harness.disable().await {
                    tracing::warn!(%error, "failed to disable Cursor harness after server stop");
                }
                result?
            },
            () = shutdown.cancelled() => {
                if let Err(error) = harness.disable().await {
                    tracing::warn!(%error, "failed to disable Cursor harness during shutdown");
                }
                registry.shutdown().await;
                match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
                    Ok(result) => result?,
                    Err(_) => tracing::warn!("graceful shutdown timed out; forcing server close"),
                }
            }
        }
        Ok(())
    }

    /// Serves until the process receives `SIGINT` or `SIGTERM`.
    pub async fn run(self) -> Result<()> {
        let shutdown = CancellationToken::new();
        let signal_shutdown = shutdown.clone();
        let running = self.serve(shutdown);
        tokio::pin!(running);
        tokio::select! {
            result = &mut running => result,
            () = shutdown_signal() => {
                tracing::info!("shutdown signal received; cancelling active runs");
                signal_shutdown.cancel();
                running.await
            }
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
