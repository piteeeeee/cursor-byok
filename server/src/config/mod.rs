//! Owns `~/.cursor-byok-lite/config.toml`: parsing, first-start migration, and
//! the read-only export from the previous SQLite database.
mod export_db;
mod models;
mod paths;
mod runtime;
mod schema;
mod settings;

pub use export_db::{export, export_into, ExportSummary};
pub use models::Models;
pub use paths::{ensure_app_dir, initialize, load, Paths};
pub use runtime::RuntimeConfig;
pub use schema::Config;
pub use settings::{ConsoleSource, ProviderConfig, ProviderKind, Settings};

use crate::Result;
use std::path::PathBuf;
pub const APP_DIR_NAME: &str = ".cursor-byok-lite";
pub const DEFAULT_LISTEN_PORT: u16 = 51_866;
pub const DEFAULT_PROXY_PORT: u16 = 52_148;
pub fn managed_data_dir() -> Result<PathBuf> {
    let paths = Paths::managed()?;
    ensure_app_dir(&paths)?;
    Ok(paths.app_dir)
}
