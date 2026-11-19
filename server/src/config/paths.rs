//! Resolves the application directory and performs first-start migration.
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::{Error, Result};

use super::schema::{Config, CONFIG_FILE_NAME};

const CA_DIR_NAME: &str = "ca";

/// Absolute paths the lite build reads and writes.
#[derive(Clone, Debug)]
pub struct Paths {
    pub app_dir: PathBuf,
    pub config_file: PathBuf,
    pub ca_dir: PathBuf,
    pub previous_database: PathBuf,
    pub previous_ca_dir: PathBuf,
}

impl Paths {
    /// Paths rooted at the user's home directory.
    pub fn managed() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
        Ok(Self::at(&home))
    }

    /// Paths rooted at an explicit home directory; tests pass a temp directory.
    pub fn at(home: &Path) -> Self {
        let app_dir = home.join(super::APP_DIR_NAME);
        Self {
            config_file: app_dir.join(CONFIG_FILE_NAME),
            ca_dir: app_dir.join(CA_DIR_NAME),
            previous_database: home.join(".cursor-byok-v3").join("cursor-byok.db"),
            previous_ca_dir: home.join(".cursor-byok-v3").join(CA_DIR_NAME),
            app_dir,
        }
    }
}

/// Creates the application directory with owner-only permissions.
pub fn ensure_app_dir(paths: &Paths) -> Result<()> {
    fs::create_dir_all(&paths.app_dir)?;
    set_dir_mode(&paths.app_dir, 0o700)
}

/// Outcome of the first-start migration, logged once by the caller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Initialization {
    pub exported_models: Option<usize>,
    pub copied_ca: bool,
}

impl Initialization {
    pub fn changed(&self) -> bool {
        self.exported_models.is_some() || self.copied_ca
    }
}

/// Prepares `~/.cursor-byok-lite` for this build.
///
/// The previous build kept models, ports, and the CA in `~/.cursor-byok-v3`.
/// Both are carried over read-only so an upgrade keeps Cursor conversations and
/// the already trusted certificate working without manual steps.
pub fn initialize(paths: &Paths) -> Result<Initialization> {
    ensure_app_dir(paths)?;
    let mut initialization = Initialization::default();
    if !paths.config_file.exists() && paths.previous_database.exists() {
        let exported = super::export_db::export(&paths.previous_database, &paths.config_file, &[])?;
        initialization.exported_models = Some(exported.model_count);
        tracing::info!(
            models = exported.model_count,
            config = %paths.config_file.display(),
            "exported models from the previous database into config.toml"
        );
    }
    if !paths.ca_dir.exists() && paths.previous_ca_dir.is_dir() {
        copy_ca(&paths.previous_ca_dir, &paths.ca_dir)?;
        initialization.copied_ca = true;
        tracing::info!(
            ca = %paths.ca_dir.display(),
            "copied the trusted CA from the previous application directory"
        );
    }
    Ok(initialization)
}

/// Copies the CA directory byte-for-byte so the keychain trust still matches.
fn copy_ca(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    set_dir_mode(destination, 0o700)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let target = destination.join(entry.file_name());
        fs::copy(entry.path(), &target)?;
        set_file_mode(&target, 0o600)?;
    }
    Ok(())
}

/// Loads `config.toml`, running the first-start migration when needed.
pub fn load(paths: &Paths) -> Result<Config> {
    initialize(paths)?;
    if !paths.config_file.exists() {
        return Err(Error::Config(format!(
            "{} does not exist and there is no previous database to export models from",
            paths.config_file.display()
        )));
    }
    Config::load(&paths.config_file)
}

pub(crate) fn set_dir_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

pub(crate) fn set_file_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sql_string(value: &str) -> String {
        value.replace('\'', "''")
    }

    /// Creates the previous build's database with one model and the CA files.
    fn seed_previous(home: &Path) {
        let paths = Paths::at(home);
        std::fs::create_dir_all(paths.previous_database.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&paths.previous_ca_dir).unwrap();
        std::fs::write(paths.previous_ca_dir.join("ca.crt"), b"cert-bytes").unwrap();
        std::fs::write(paths.previous_ca_dir.join("ca.key"), b"key-bytes").unwrap();
        let database = paths.previous_database;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database)
                .create_if_missing(true);
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE model_configs (
                    model_hash TEXT PRIMARY KEY, sort_order INTEGER NOT NULL DEFAULT 0,
                    display_name TEXT NOT NULL, model_type TEXT NOT NULL, base_url TEXT NOT NULL,
                    use_full_url INTEGER NOT NULL DEFAULT 0, api_key TEXT NOT NULL,
                    tooltip_data TEXT NOT NULL, model_id TEXT NOT NULL, reasoning_effort TEXT,
                    openai_endpoint TEXT NOT NULL DEFAULT '',
                    openai_extra_params_enabled INTEGER NOT NULL DEFAULT 0,
                    openai_extra_params_json TEXT NOT NULL DEFAULT '{}',
                    custom_headers_enabled INTEGER NOT NULL DEFAULT 0,
                    custom_headers_json TEXT NOT NULL DEFAULT '{}',
                    anthropic_extra_params_enabled INTEGER NOT NULL DEFAULT 0,
                    anthropic_extra_params_json TEXT NOT NULL DEFAULT '{}',
                    context_window_tokens INTEGER, max_completion_tokens INTEGER,
                    anthropic_max_tokens INTEGER, anthropic_thinking_effort TEXT,
                    thinking_budget_tokens INTEGER, created_at_ms INTEGER NOT NULL DEFAULT 0,
                    updated_at_ms INTEGER NOT NULL DEFAULT 0, group_name TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("CREATE TABLE service_settings (setting_key TEXT PRIMARY KEY, value_json TEXT NOT NULL, updated_at_ms INTEGER NOT NULL DEFAULT 0)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO service_settings(setting_key, value_json) VALUES ('network_ports', '{\"proxy_port\":52148,\"service_port\":51866}')",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(&format!(
                "INSERT INTO model_configs(model_hash, sort_order, display_name, model_type, base_url, api_key, tooltip_data, model_id, openai_endpoint)
                 VALUES ('{}', 1, '{}', 'openai', '{}', '{}', '{}', '{}', '/v1/chat/completions')",
                sql_string("83f1333d63840a0c"),
                sql_string("deepseek-v4.1-flash"),
                sql_string("http://192.168.11.46:7863/v1"),
                sql_string("lan-secret"),
                sql_string("deepseek-v4.1-flash"),
                sql_string("deepseek-v4.1-flash"),
            ))
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        });
    }

    #[test]
    fn first_start_exports_models_and_copies_the_ca_byte_for_byte() {
        let home = tempfile::tempdir().unwrap();
        seed_previous(home.path());
        let paths = Paths::at(home.path());

        let initialization = initialize(&paths).unwrap();
        assert_eq!(initialization.exported_models, Some(1));
        assert!(initialization.copied_ca);

        let config = Config::load(&paths.config_file).unwrap();
        assert_eq!(config.models.all().len(), 1);
        assert_eq!(config.models.all()[0].model_hash, "83f1333d63840a0c");
        assert_eq!(config.listen_port, 51_866);

        let source = std::fs::read(paths.previous_ca_dir.join("ca.crt")).unwrap();
        let copied = std::fs::read(paths.ca_dir.join("ca.crt")).unwrap();
        assert_eq!(source, copied);
        assert_eq!(
            std::fs::read(paths.ca_dir.join("ca.key")).unwrap(),
            b"key-bytes"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&paths.ca_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(paths.ca_dir.join("ca.crt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o600);
        }
    }

    #[test]
    fn an_existing_configuration_is_never_overwritten() {
        let home = tempfile::tempdir().unwrap();
        seed_previous(home.path());
        let paths = Paths::at(home.path());
        initialize(&paths).unwrap();
        std::fs::write(&paths.config_file, "# edited by the user\n").unwrap();

        let initialization = initialize(&paths).unwrap();
        assert_eq!(initialization.exported_models, None);
        assert!(!initialization.copied_ca);
        assert_eq!(
            std::fs::read_to_string(&paths.config_file).unwrap(),
            "# edited by the user\n"
        );
    }

    #[test]
    fn a_missing_previous_database_leaves_the_configuration_absent() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path());

        let initialization = initialize(&paths).unwrap();
        assert!(!initialization.changed());
        let error = load(&paths).unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");
    }
}
