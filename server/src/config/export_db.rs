//! Exports models and ports from a previous SQLite database into `config.toml`.
//!
//! The database is opened read-only and immutable: exporting must never touch
//! the file, its WAL, or its mtime, because it belongs to a running build.
use std::{collections::BTreeMap, fmt::Write as _, path::Path};

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
    Row,
};

use crate::{Error, Result};

use super::paths::{set_file_mode, Paths};

const MODEL_COLUMNS: &str = r#"
    model_hash, display_name, group_name, model_type, base_url, use_full_url, api_key,
    tooltip_data, model_id, reasoning_effort, openai_endpoint,
    openai_extra_params_enabled, openai_extra_params_json,
    custom_headers_enabled, custom_headers_json,
    anthropic_extra_params_enabled, anthropic_extra_params_json,
    context_window_tokens, max_completion_tokens, anthropic_max_tokens,
    anthropic_thinking_effort, thinking_budget_tokens
"#;

/// Result of an export, reported by the CLI and the first-start log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportSummary {
    pub model_count: usize,
    pub provider_count: usize,
    pub default_model: String,
    pub service_port: u16,
    pub proxy_port: u16,
}

struct ExportedModel {
    hash: String,
    display_name: String,
    group_name: Option<String>,
    model_type: String,
    base_url: String,
    use_full_url: bool,
    api_key: String,
    tooltip_data: String,
    model_id: String,
    reasoning_effort: Option<String>,
    openai_endpoint: String,
    openai_extra_params_enabled: bool,
    openai_extra_params_json: String,
    custom_headers_enabled: bool,
    custom_headers_json: String,
    anthropic_extra_params_enabled: bool,
    anthropic_extra_params_json: String,
    context_window_tokens: Option<i64>,
    max_completion_tokens: Option<i64>,
    anthropic_max_tokens: Option<i64>,
    anthropic_thinking_effort: Option<String>,
    thinking_budget_tokens: Option<i64>,
}

/// Reads models and ports from `database` and writes `config.toml` to `out`.
pub fn export(database: &Path, out: &Path, default_model: &[String]) -> Result<ExportSummary> {
    if out.exists() {
        return Err(Error::Config(format!(
            "{} already exists; refusing to overwrite",
            out.display()
        )));
    }
    let models = read_models(database)?;
    if models.is_empty() {
        return Err(Error::Config(format!(
            "{} has no model_configs rows to export",
            database.display()
        )));
    }
    let (service_port, proxy_port) = read_ports(database)?;
    let document = render(&models, service_port, proxy_port, default_model)?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, document)?;
    set_file_mode(out, 0o600)?;
    Ok(ExportSummary {
        model_count: models.len(),
        provider_count: provider_keys(&models).len(),
        default_model: default_model_name(&models, default_model),
        service_port,
        proxy_port,
    })
}

/// Exports into the configured application directory, refusing to overwrite.
pub fn export_into(paths: &Paths, default_model: &[String]) -> Result<ExportSummary> {
    super::paths::ensure_app_dir(paths)?;
    export(&paths.previous_database, &paths.config_file, default_model)
}

fn read_models(database: &Path) -> Result<Vec<ExportedModel>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Store(format!("cannot start export runtime: {error}")))?;
    runtime.block_on(read_models_async(database))
}

async fn read_models_async(database: &Path) -> Result<Vec<ExportedModel>> {
    let pool = readonly_pool(database).await?;
    let query =
        format!("SELECT {MODEL_COLUMNS} FROM model_configs ORDER BY sort_order, display_name");
    let rows = sqlx::query(&query).fetch_all(&pool).await?;
    let mut models = Vec::with_capacity(rows.len());
    for row in rows {
        models.push(ExportedModel {
            hash: row.try_get("model_hash")?,
            display_name: row.try_get("display_name")?,
            group_name: row.try_get("group_name")?,
            model_type: row.try_get("model_type")?,
            base_url: row.try_get("base_url")?,
            use_full_url: row.try_get("use_full_url")?,
            api_key: row.try_get("api_key")?,
            tooltip_data: row.try_get("tooltip_data")?,
            model_id: row.try_get("model_id")?,
            reasoning_effort: row.try_get("reasoning_effort")?,
            openai_endpoint: row.try_get("openai_endpoint")?,
            openai_extra_params_enabled: row.try_get("openai_extra_params_enabled")?,
            openai_extra_params_json: row.try_get("openai_extra_params_json")?,
            custom_headers_enabled: row.try_get("custom_headers_enabled")?,
            custom_headers_json: row.try_get("custom_headers_json")?,
            anthropic_extra_params_enabled: row.try_get("anthropic_extra_params_enabled")?,
            anthropic_extra_params_json: row.try_get("anthropic_extra_params_json")?,
            context_window_tokens: row.try_get("context_window_tokens")?,
            max_completion_tokens: row.try_get("max_completion_tokens")?,
            anthropic_max_tokens: row.try_get("anthropic_max_tokens")?,
            anthropic_thinking_effort: row.try_get("anthropic_thinking_effort")?,
            thinking_budget_tokens: row.try_get("thinking_budget_tokens")?,
        });
    }
    pool.close().await;
    Ok(models)
}

fn read_ports(database: &Path) -> Result<(u16, u16)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Store(format!("cannot start export runtime: {error}")))?;
    runtime.block_on(async {
        let pool = readonly_pool(database).await?;
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = 'network_ports'",
        )
        .fetch_optional(&pool)
        .await?;
        pool.close().await;
        let Some(value) = value else {
            return Ok((super::DEFAULT_LISTEN_PORT, super::DEFAULT_PROXY_PORT));
        };
        let ports: PortSettings = serde_json::from_str(&value)
            .map_err(|error| Error::Config(format!("unreadable network_ports row: {error}")))?;
        Ok((ports.service_port, ports.proxy_port))
    })
}

#[derive(serde::Deserialize)]
struct PortSettings {
    proxy_port: u16,
    service_port: u16,
}

async fn readonly_pool(database: &Path) -> Result<sqlx::SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(database)
        .read_only(true)
        .immutable(true)
        .journal_mode(SqliteJournalMode::Off);
    Ok(sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?)
}

fn render(
    models: &[ExportedModel],
    service_port: u16,
    proxy_port: u16,
    default_model: &[String],
) -> Result<String> {
    let providers = provider_keys(models);
    let mut provider_names = BTreeMap::new();
    for (index, key) in providers.iter().enumerate() {
        let base = provider_label(&key.base_url, &key.model_type);
        let mut name = base.clone();
        let mut suffix = 2;
        while provider_names.values().any(|existing| existing == &name) {
            name = format!("{base}-{suffix}");
            suffix += 1;
        }
        provider_names.insert(index, name);
    }

    let mut document = String::new();
    document.push_str("# Cursor BYOK lite configuration.\n");
    document
        .push_str("# Exported from the previous SQLite database; edit and restart to apply.\n\n");
    let default_name = default_model_name(models, default_model);
    let _ = writeln!(document, "default_model = {}", toml_string(&default_name));
    document.push_str("\n[server]\n");
    let _ = writeln!(document, "listen_port = {service_port}");
    let _ = writeln!(document, "proxy_port = {proxy_port}");
    document.push_str("request_timeout_secs = 3600\nstream_idle_timeout_secs = 1800\n");

    for (index, key) in providers.iter().enumerate() {
        let name = provider_names.get(&index).expect("provider name assigned");
        document.push_str("\n[[providers]]\n");
        let _ = writeln!(document, "name = {}", toml_string(name));
        let _ = writeln!(document, "type = {}", toml_string(&key.model_type));
        let _ = writeln!(document, "base_url = {}", toml_string(&key.base_url));
        let _ = writeln!(document, "api_key = {}", toml_string(&key.api_key));
        if let Some(headers) = &key.custom_headers {
            let _ = writeln!(document, "custom_headers = {}", toml_inline(headers)?);
        }
    }

    for model in models {
        let index = provider_index(providers.as_slice(), model);
        let name = provider_names.get(&index).expect("provider name assigned");
        document.push_str("\n[[models]]\n");
        let _ = writeln!(document, "provider = {}", toml_string(name));
        let _ = writeln!(document, "model_hash = {}", toml_string(&model.hash));
        let _ = writeln!(
            document,
            "display_name = {}",
            toml_string(&model.display_name)
        );
        if let Some(group_name) = model
            .group_name
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let _ = writeln!(document, "group_name = {}", toml_string(group_name));
        }
        let _ = writeln!(document, "model_id = {}", toml_string(&model.model_id));
        if model.model_type == "openai" {
            let endpoint = if model.openai_endpoint.is_empty() {
                crate::model::OPENAI_CHAT_ENDPOINT
            } else {
                model.openai_endpoint.as_str()
            };
            let _ = writeln!(document, "endpoint = {}", toml_string(endpoint));
        }
        if model.use_full_url {
            document.push_str("use_full_url = true\n");
        }
        if model.base_url != providers[index].base_url {
            let _ = writeln!(document, "base_url = {}", toml_string(&model.base_url));
        }
        if model.tooltip_data != model.display_name {
            let _ = writeln!(
                document,
                "tooltip_data = {}",
                toml_string(&model.tooltip_data)
            );
        }
        write_optional(
            &mut document,
            "reasoning_effort",
            model.reasoning_effort.as_deref(),
        );
        write_optional(
            &mut document,
            "thinking_effort",
            model.anthropic_thinking_effort.as_deref(),
        );
        write_optional_number(
            &mut document,
            "context_window_tokens",
            model.context_window_tokens,
        );
        write_optional_number(
            &mut document,
            "max_completion_tokens",
            model.max_completion_tokens,
        );
        write_optional_number(
            &mut document,
            "anthropic_max_tokens",
            model.anthropic_max_tokens,
        );
        write_optional_number(
            &mut document,
            "thinking_budget_tokens",
            model.thinking_budget_tokens,
        );
        if model.openai_extra_params_enabled && !is_empty_json(&model.openai_extra_params_json) {
            let _ = writeln!(
                document,
                "openai_extra_params = {}",
                toml_inline(&model.openai_extra_params_json)?
            );
        }
        if model.anthropic_extra_params_enabled
            && !is_empty_json(&model.anthropic_extra_params_json)
        {
            let _ = writeln!(
                document,
                "anthropic_extra_params = {}",
                toml_inline(&model.anthropic_extra_params_json)?
            );
        }
    }
    Ok(document)
}

fn write_optional(document: &mut String, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        let _ = writeln!(document, "{key} = {}", toml_string(value));
    }
}

fn write_optional_number(document: &mut String, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        let _ = writeln!(document, "{key} = {value}");
    }
}

/// Renders a JSON object as a TOML inline table with quoted keys.
///
/// TOML bare keys cannot contain `-`, so header names like `User-Agent` must be
/// quoted; serializing them keeps the exported file parseable.
fn toml_inline(json: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| Error::Config(format!("invalid JSON value in model_configs: {error}")))?;
    let mut rendered = String::new();
    render_inline_value(&value, &mut rendered)?;
    Ok(rendered)
}

fn render_inline_value(value: &serde_json::Value, out: &mut String) -> Result<()> {
    match value {
        serde_json::Value::Object(map) => {
            out.push_str("{ ");
            for (index, (key, value)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&toml_string(key));
                out.push_str(" = ");
                render_inline_value(value, out)?;
            }
            out.push_str(" }");
            Ok(())
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                render_inline_value(item, out)?;
            }
            out.push(']');
            Ok(())
        }
        serde_json::Value::String(text) => {
            out.push_str(&toml_string(text));
            Ok(())
        }
        serde_json::Value::Bool(flag) => {
            out.push_str(if *flag { "true" } else { "false" });
            Ok(())
        }
        serde_json::Value::Number(number) => {
            out.push_str(&number.to_string());
            Ok(())
        }
        serde_json::Value::Null => Err(Error::Config(
            "null cannot be represented in config.toml".into(),
        )),
    }
}

fn is_empty_json(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty() || trimmed == "{}"
}

fn toml_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(escaped, "\\u{:04x}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProviderKey {
    model_type: String,
    base_url: String,
    api_key: String,
    custom_headers: Option<String>,
}

fn provider_keys(models: &[ExportedModel]) -> Vec<ProviderKey> {
    let mut keys = std::collections::BTreeSet::new();
    for model in models {
        keys.insert(ProviderKey {
            model_type: model.model_type.clone(),
            base_url: model.base_url.clone(),
            api_key: model.api_key.clone(),
            custom_headers: Some(model.custom_headers_json.clone()).filter(|_| {
                model.custom_headers_enabled && !is_empty_json(&model.custom_headers_json)
            }),
        });
    }
    keys.into_iter().collect()
}

fn provider_index(providers: &[ProviderKey], model: &ExportedModel) -> usize {
    let key = ProviderKey {
        model_type: model.model_type.clone(),
        base_url: model.base_url.clone(),
        api_key: model.api_key.clone(),
        custom_headers: Some(model.custom_headers_json.clone())
            .filter(|_| model.custom_headers_enabled && !is_empty_json(&model.custom_headers_json)),
    };
    providers
        .iter()
        .position(|candidate| *candidate == key)
        .expect("every model has a provider key")
}

fn provider_label(base_url: &str, _model_type: &str) -> String {
    let host = reqwest::Url::parse(base_url.trim())
        .ok()
        .and_then(|url| {
            url.host_str().map(|host| match url.port() {
                Some(port) => format!("{host}-{port}"),
                None => host.to_string(),
            })
        })
        .unwrap_or_else(|| base_url.trim().to_string());
    let slug = host
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '.' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if slug.is_empty() {
        "provider".into()
    } else {
        slug
    }
}

fn default_model_name(models: &[ExportedModel], preferred: &[String]) -> String {
    for candidate in preferred {
        if let Some(model) = models
            .iter()
            .find(|model| model.hash == *candidate || model.display_name == *candidate)
        {
            return model.display_name.clone();
        }
    }
    models[0].display_name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn sql_string(value: &str) -> String {
        value.replace('\'', "''")
    }

    async fn seed(database: &Path) {
        let options = SqliteConnectOptions::new()
            .filename(database)
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
        let rows = [
            (
                "83f1333d63840a0c",
                4,
                "deepseek-v4.1-flash",
                "openai",
                "http://192.168.11.46:7863/v1",
                "a39b-secret",
                "/v1/chat/completions",
            ),
            (
                "dffde0e2d7478366",
                11,
                "claude-opus-5-5",
                "openai",
                "https://api.ziyouxie.online",
                "sk-c-secret",
                "/v1/responses",
            ),
            (
                "bb2ef95154fa0bde",
                14,
                "claude-sonnet-5-5cc",
                "anthropic",
                "https://api.ziyouxie.online",
                "sk-c-secret",
                "",
            ),
        ];
        for (hash, order, display_name, model_type, base_url, api_key, endpoint) in rows {
            sqlx::query(&format!(
                "INSERT INTO model_configs(model_hash, sort_order, display_name, model_type, base_url, api_key, tooltip_data, model_id, openai_endpoint, custom_headers_enabled, custom_headers_json, reasoning_effort, anthropic_thinking_effort)
                 VALUES ('{}', {}, '{}', '{}', '{}', '{}', '{}', '{}', '{}', 1, '{{\"X-Test\":\"1\"}}', NULL, {})",
                sql_string(hash),
                order,
                sql_string(display_name),
                model_type,
                sql_string(base_url),
                sql_string(api_key),
                sql_string(display_name),
                sql_string(display_name),
                endpoint,
                if model_type == "anthropic" { "'xhigh'" } else { "NULL" },
            ))
            .execute(&pool)
            .await
            .unwrap();
        }
        pool.close().await;
    }

    fn digest(path: &Path) -> (String, std::time::SystemTime) {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(path).unwrap();
        let hash = hex::encode(Sha256::digest(&bytes));
        let modified = std::fs::metadata(path).unwrap().modified().unwrap();
        (hash, modified)
    }

    #[test]
    fn export_round_trips_models_and_keeps_the_source_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("source.db");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(seed(&database));
        let before = digest(&database);

        let out = directory.path().join("config.toml");
        let summary = export(&database, &out, &[]).unwrap();
        assert_eq!(summary.model_count, 3);
        assert_eq!(summary.service_port, 51_866);
        assert_eq!(summary.proxy_port, 52_148);
        assert_eq!(digest(&database), before);

        let text = std::fs::read_to_string(&out).unwrap();
        let config = Config::parse(&text, &|_| Some("env-key".into())).unwrap();
        let models = config.models.all();
        assert_eq!(models.len(), 3);
        let hashes = models
            .iter()
            .map(|model| model.model_hash.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            hashes,
            vec!["83f1333d63840a0c", "dffde0e2d7478366", "bb2ef95154fa0bde"]
        );
        let anthropic = models
            .iter()
            .find(|model| model.model_hash == "bb2ef95154fa0bde")
            .unwrap();
        assert_eq!(
            anthropic.anthropic_thinking_effort.as_deref(),
            Some("xhigh")
        );
        assert!(anthropic.custom_headers_enabled);
        assert_eq!(anthropic.api_key, "sk-c-secret");
        assert_eq!(config.default_model, "deepseek-v4.1-flash");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn export_refuses_to_overwrite_an_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("source.db");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(seed(&database));
        let out = directory.path().join("config.toml");
        std::fs::write(&out, "keep me").unwrap();
        let error = export(&database, &out, &[]).unwrap_err();
        assert!(
            error.to_string().contains("refusing to overwrite"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "keep me");
    }

    #[test]
    fn export_keeps_declared_hashes_even_when_parameters_change() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("source.db");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(seed(&database));
        let out = directory.path().join("config.toml");
        export(&database, &out, &[]).unwrap();

        let text = std::fs::read_to_string(&out).unwrap();
        let rewritten = text.replace("api_key = \"a39b-secret\"", "api_key = \"env:LAN_KEY\"");
        let config = Config::parse(&rewritten, &|_| Some("totally-different".into())).unwrap();
        assert_eq!(config.models.all()[0].model_hash, "83f1333d63840a0c");
    }
}
