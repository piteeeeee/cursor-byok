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

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Application directory of the lite build: `~/.cursor-byok-lite`.
pub const APP_DIR_NAME: &str = ".cursor-byok-lite";
/// Application directory of the previous build: `~/.cursor-byok-v3`.
pub const PREVIOUS_APP_DIR_NAME: &str = ".cursor-byok-v3";
/// Default ports used when `[server]` is omitted.
pub const DEFAULT_LISTEN_PORT: u16 = 51_866;
pub const DEFAULT_PROXY_PORT: u16 = 52_148;

const DATABASE_FILE_NAME: &str = "cursor-byok.db";

/// Database of the previous build; step 4 replaces the storage layer.
pub fn previous_database_path(home: &Path) -> PathBuf {
    home.join(PREVIOUS_APP_DIR_NAME).join(DATABASE_FILE_NAME)
}

/// Directory holding the SQLite database and logs.
///
/// Step 3 deliberately keeps `~/.cursor-byok-v3` so an upgrade reuses the
/// existing database; the storage layer moves in a later step.
pub fn managed_data_dir() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
    let directory = home.join(PREVIOUS_APP_DIR_NAME);
    std::fs::create_dir_all(&directory)?;
    paths::set_dir_mode(&directory, 0o700)?;
    Ok(directory)
}

/// Path of the v0.0.49 YAML configuration imported by the legacy importer.
pub fn v0049_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
    Ok(home.join(".cursor-local-assistant-v2").join("config.yaml"))
}
