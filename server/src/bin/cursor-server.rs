//! Runs the headless server without a menu bar, for development and containers.
use cursor_server::{error::Result, App, ConfigStore};
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cursor_server=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    App::new(ConfigStore::load()?).await?.run().await
}
