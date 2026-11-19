//! Assembles server dependencies and starts the application services.
use std::{future::IntoFuture, net::SocketAddr, time::Duration};

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    api,
    config::{ConsoleSource, Settings},
    control,
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        transport::TransportRegistry,
    },
    local_app::CursorHarness,
    provider::ProviderRouter,
    search::WebCache,
    store::Store,
    Result,
};

pub struct App {
    settings: Settings,
    router: axum::Router,
    registry: TransportRegistry,
    harness: CursorHarness,
    store: Store,
}

impl App {
    pub async fn new(settings: Settings) -> Result<Self> {
        let store = Store::connect(&settings.database_url).await?;
        let assets = PromptAssets::embedded()?;
        let compiler = PromptCompiler::new(assets);
        let clients = crate::network::NetworkClients::new(settings.config.outbound_proxy.clone());
        let provider = std::sync::Arc::new(ProviderRouter::new(
            store.clone(),
            settings.config.models.clone(),
            clients.clone(),
            settings.config.request_timeout,
            settings.config.stream_idle_timeout,
        ));
        let registry = TransportRegistry::with_web_cache(
            store.clone(),
            crate::config::RuntimeConfig::new(
                settings.config.models.clone(),
                settings.config.default_model.clone(),
                settings.config.outbound_proxy.clone(),
                settings.paths.ca_dir.clone(),
            ),
            provider.clone(),
            compiler,
            WebCache::managed()?,
        );
        let control = control::ControlService::new(
            store.clone(),
            provider,
            clients.clone(),
            settings.config.clone(),
            settings.paths.ca_dir.clone(),
        )?;
        let harness = control.cursor_harness().clone();
        let mut router = api::router(registry.clone(), clients)?;
        router = match &settings.console {
            Some(ConsoleSource::Directory(directory)) => {
                router.merge(control::web_router(control.clone(), directory))
            }
            Some(ConsoleSource::Proxy(target)) => {
                router.merge(control::proxy_web_router(control.clone(), target.clone()))
            }
            None => router.merge(control::api_router(control.clone())),
        };
        Ok(Self {
            router,
            registry,
            harness,
            store,
            settings,
        })
    }

    pub fn merge_router(mut self, router: axum::Router) -> Self {
        self.router = self.router.merge(router);
        self
    }

    pub async fn bind(&self) -> Result<TcpListener> {
        bind_service_listener(self.settings.listen_addr).await
    }

    pub fn harness(&self) -> CursorHarness {
        self.harness.clone()
    }

    pub fn store(&self) -> Store {
        self.store.clone()
    }

    pub async fn serve(self) -> Result<()> {
        let listener = self.bind().await?;
        let shutdown = CancellationToken::new();
        let signal_shutdown = shutdown.clone();
        let running = self.serve_on(listener, shutdown);
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
}

async fn bind_service_listener(requested: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(requested).await.map_err(Into::into)
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
