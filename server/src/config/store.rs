//! Holds the live configuration the whole process reads.
//!
//! One `config.toml` image, shared by every component through cheap clones. The
//! embedding process can switch the default model or reload the file while runs
//! are in flight: models and the default model are read at request time, so no
//! restart or re-plumbing is needed.
use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use parking_lot::RwLock;

use crate::{model::ModelConfig, Error, Result};

use super::{schema::Config, Models, Paths};

/// The configuration values a running process reads per request.
#[derive(Clone)]
struct Live {
    models: Arc<Models>,
    default_model: Arc<str>,
    outbound_proxy: Option<Arc<str>>,
    listen_addr: SocketAddr,
    proxy_port: u16,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
    retention_days: u32,
    max_rows: u32,
}

impl Live {
    fn from_config(config: Config) -> Self {
        Self {
            models: Arc::new(config.models),
            default_model: Arc::from(config.default_model.as_str()),
            outbound_proxy: config.outbound_proxy.map(Arc::from),
            listen_addr: SocketAddr::from(([127, 0, 0, 1], config.listen_port)),
            proxy_port: config.proxy_port,
            request_timeout: config.request_timeout,
            stream_idle_timeout: config.stream_idle_timeout,
            retention_days: config.retention_days,
            max_rows: config.max_rows,
        }
    }

    /// Values that only take effect when the process restarts.
    fn restart_only_changes(&self, next: &Live) -> Vec<&'static str> {
        let mut changed = Vec::new();
        if self.listen_addr != next.listen_addr {
            changed.push("server.listen_port");
        }
        if self.proxy_port != next.proxy_port {
            changed.push("server.proxy_port");
        }
        if self.request_timeout != next.request_timeout {
            changed.push("server.request_timeout_secs");
        }
        if self.stream_idle_timeout != next.stream_idle_timeout {
            changed.push("server.stream_idle_timeout_secs");
        }
        if self.outbound_proxy != next.outbound_proxy {
            changed.push("server.outbound_proxy");
        }
        if self.retention_days != next.retention_days {
            changed.push("stats.retention_days");
        }
        if self.max_rows != next.max_rows {
            changed.push("stats.max_rows");
        }
        changed
    }
}

/// Where the configuration came from, which decides whether it can be reloaded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `~/.cursor-byok-lite/config.toml`: reload and write-back are available.
    File,
    /// Supplied by the embedding process; nothing is read from or written to disk.
    Memory,
}

#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<Inner>,
}

struct Inner {
    paths: Paths,
    source: Source,
    live: RwLock<Live>,
}

impl ConfigStore {
    /// Loads `~/.cursor-byok-lite/config.toml`, migrating on first start.
    pub fn load() -> Result<Self> {
        Self::at(Paths::managed()?)
    }

    /// Loads the configuration file under explicitly rooted paths.
    pub fn at(paths: Paths) -> Result<Self> {
        let config = super::load(&paths)?;
        Ok(Self::from_config(config, paths, Source::File))
    }

    /// Configuration supplied by the embedding process: no file is read or written.
    pub fn in_memory(paths: Paths, models: Models, default_model: String) -> Result<Self> {
        if !models.all().is_empty() {
            models.resolve_default(&default_model)?;
        }
        let config = Config {
            retention_days: DEFAULT_RETENTION_DAYS,
            max_rows: DEFAULT_MAX_ROWS,
            default_model,
            listen_port: super::DEFAULT_LISTEN_PORT,
            proxy_port: super::DEFAULT_PROXY_PORT,
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
            stream_idle_timeout: Duration::from_secs(DEFAULT_STREAM_IDLE_TIMEOUT_SECS),
            outbound_proxy: None,
            models,
        };
        Ok(Self::from_config(config, paths, Source::Memory))
    }

    fn from_config(config: Config, paths: Paths, source: Source) -> Self {
        Self {
            inner: Arc::new(Inner {
                paths,
                source,
                live: RwLock::new(Live::from_config(config)),
            }),
        }
    }

    /// Application directory, config file, and CA location.
    pub fn paths(&self) -> &Paths {
        &self.inner.paths
    }

    pub fn config_file(&self) -> &Path {
        &self.inner.paths.config_file
    }

    /// The configured models, in `config.toml` order.
    pub fn models(&self) -> Arc<Models> {
        self.inner.live.read().models.clone()
    }

    /// The configured models as a plain list, for menus and catalogs.
    pub fn catalog(&self) -> Vec<ModelConfig> {
        self.models().all().to_vec()
    }

    /// The default model as written in `config.toml`: a hash or a display name.
    pub fn default_model(&self) -> String {
        self.inner.live.read().default_model.to_string()
    }

    pub fn outbound_proxy(&self) -> Option<String> {
        self.inner
            .live
            .read()
            .outbound_proxy
            .as_deref()
            .map(str::to_owned)
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.inner.live.read().listen_addr
    }

    pub fn proxy_port(&self) -> u16 {
        self.inner.live.read().proxy_port
    }

    pub fn request_timeout(&self) -> Duration {
        self.inner.live.read().request_timeout
    }

    pub fn stream_idle_timeout(&self) -> Duration {
        self.inner.live.read().stream_idle_timeout
    }

    pub fn retention_days(&self) -> u32 {
        self.inner.live.read().retention_days
    }

    pub fn max_rows(&self) -> u32 {
        self.inner.live.read().max_rows
    }

    /// Makes `value` the default model for new Cursor runs.
    ///
    /// The model is validated against the configured catalog, persisted as its
    /// canonical `model_hash`, and then applied to the running process. Returns
    /// the hash that is now the default.
    pub fn set_default_model(&self, value: &str) -> Result<String> {
        let model_hash = self.models().resolve_default(value)?.model_hash.clone();
        if self.default_model() == model_hash {
            return Ok(model_hash);
        }
        if self.inner.source == Source::File {
            write_default_model(&self.inner.paths.config_file, &model_hash)?;
        }
        self.inner.live.write().default_model = Arc::from(model_hash.as_str());
        Ok(model_hash)
    }

    /// Re-reads `config.toml` and applies the values that can change in place.
    ///
    /// Ports, timeouts, proxy, and statistics limits are fixed for the lifetime
    /// of the process, so a file that changes them is rejected rather than
    /// silently half-applied.
    pub fn reload(&self) -> Result<()> {
        if self.inner.source == Source::Memory {
            return Err(Error::Config(
                "this configuration was supplied in memory and has no file to reload".into(),
            ));
        }
        let config = Config::load(&self.inner.paths.config_file)?;
        let next = Live::from_config(config);
        let changed = {
            let current = self.inner.live.read();
            current.restart_only_changes(&next)
        };
        if !changed.is_empty() {
            return Err(Error::Config(format!(
                "{} changed; restart the application to apply them",
                changed.join(", ")
            )));
        }
        *self.inner.live.write() = next;
        Ok(())
    }
}

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 60 * 60;
const DEFAULT_STREAM_IDLE_TIMEOUT_SECS: u64 = 30 * 60;
const DEFAULT_RETENTION_DAYS: u32 = 90;
const DEFAULT_MAX_ROWS: u32 = 100_000;

/// Rewrites the top-level `default_model` line, leaving the rest of the file
/// byte-for-byte intact so user comments and model blocks survive.
fn write_default_model(path: &Path, model_hash: &str) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| Error::Config(format!("cannot read {}: {error}", path.display())))?;
    let rewritten = rewrite_default_model(&text, model_hash)?;
    toml::from_str::<toml::Value>(&rewritten).map_err(|error| {
        Error::Config(format!(
            "rewriting default_model would break the file: {error}"
        ))
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| Error::Config("config file has no parent directory".into()))?,
    )?;
    std::io::Write::write_all(&mut temporary, rewritten.as_bytes())?;
    temporary.persist(path).map_err(|error| {
        Error::Config(format!("cannot write {}: {}", path.display(), error.error))
    })?;
    Ok(())
}

fn rewrite_default_model(text: &str, model_hash: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len() + model_hash.len());
    let mut in_table = false;
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        let trimmed = body.trim_start();
        if trimmed.starts_with('[') {
            in_table = true;
        }
        if !replaced && !in_table {
            if let Some(equal) = body.find('=') {
                if body[..equal].trim() == "default_model" {
                    let tail = &body[equal + 1..];
                    out.push_str("default_model = \"");
                    out.push_str(model_hash);
                    out.push('"');
                    if let Some(comment) = trailing_comment(tail) {
                        out.push(' ');
                        out.push_str(comment);
                    }
                    out.push_str(newline);
                    replaced = true;
                    continue;
                }
            }
        }
        out.push_str(body);
        out.push_str(newline);
    }
    if !replaced {
        return Err(Error::Config(
            "config.toml has no top-level default_model line to rewrite".into(),
        ));
    }
    Ok(out)
}

/// Returns the trailing `# ...` comment of a value, ignoring `#` inside quotes.
fn trailing_comment(value: &str) -> Option<&str> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        match character {
            '"' if !escaped => quoted = !quoted,
            '#' if !quoted => return Some(value[index..].trim_end()),
            _ => {}
        }
        escaped = character == '\\' && !escaped;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ensure_app_dir;

    const FILE: &str = "# exported from the previous database
default_model = \"Alpha\"  # switch here
[server]
listen_port = 51866
[[providers]]
name = \"p\"
type = \"openai\"
base_url = \"http://127.0.0.1:1/v1\"
api_key = \"k\"
[[models]]
provider = \"p\"
display_name = \"Alpha\"
model_id = \"alpha\"
[[models]]
provider = \"p\"
display_name = \"Beta\"
model_id = \"beta\"
";

    fn store() -> (tempfile::TempDir, ConfigStore) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::at(home.path());
        ensure_app_dir(&paths).unwrap();
        std::fs::write(&paths.config_file, FILE).unwrap();
        let store = ConfigStore::at(paths).unwrap();
        (home, store)
    }

    #[test]
    fn switching_the_default_model_writes_back_its_hash() {
        let (home, store) = store();
        let beta = store
            .models()
            .resolve_default("Beta")
            .unwrap()
            .model_hash
            .clone();
        assert_eq!(store.set_default_model("Beta").unwrap(), beta);
        assert_eq!(store.default_model(), beta);
        let text =
            std::fs::read_to_string(home.path().join(".cursor-byok-lite/config.toml")).unwrap();
        assert!(
            text.contains(&format!("default_model = \"{beta}\"")),
            "{text}"
        );
        assert!(
            text.contains("# switch here"),
            "the trailing comment survives"
        );
        assert!(
            text.contains("display_name = \"Beta\""),
            "other blocks survive"
        );
    }

    #[test]
    fn an_unknown_model_is_rejected_before_the_file_changes() {
        let (home, store) = store();
        assert!(store.set_default_model("Gamma").is_err());
        let text =
            std::fs::read_to_string(home.path().join(".cursor-byok-lite/config.toml")).unwrap();
        assert_eq!(text, FILE);
        assert_eq!(store.default_model(), "Alpha");
    }

    #[test]
    fn reload_applies_model_changes_and_rejects_restart_only_ones() {
        let (home, store) = store();
        let file = home.path().join(".cursor-byok-lite/config.toml");
        std::fs::write(
            &file,
            FILE.replace("model_id = \"beta\"", "model_id = \"beta-2\""),
        )
        .unwrap();
        store.reload().unwrap();
        assert!(store
            .models()
            .all()
            .iter()
            .any(|model| model.model_id == "beta-2"));

        std::fs::write(
            &file,
            FILE.replace("listen_port = 51866", "listen_port = 51999"),
        )
        .unwrap();
        let error = store.reload().unwrap_err();
        assert!(error.to_string().contains("server.listen_port"), "{error}");
    }

    #[test]
    fn in_memory_configuration_never_touches_a_file() {
        let home = tempfile::tempdir().unwrap();
        let parsed = Config::parse(FILE, &|_| Some("key".into())).unwrap();
        let default = parsed.models.all()[0].model_hash.clone();
        let store =
            ConfigStore::in_memory(Paths::at(home.path()), parsed.models, default.clone()).unwrap();
        let beta = store
            .models()
            .resolve_default("Beta")
            .unwrap()
            .model_hash
            .clone();
        assert_eq!(store.set_default_model("Beta").unwrap(), beta);
        assert_eq!(store.default_model(), beta);
        assert!(store
            .reload()
            .unwrap_err()
            .to_string()
            .contains("in memory"));
        assert!(!home.path().join(".cursor-byok-lite").exists());
    }

    #[test]
    fn rewriting_keeps_comments_but_ignores_nested_keys() {
        let text = "default_model = \"a#b\" # note\n[server]\nlisten_port = 1\n";
        assert_eq!(
            rewrite_default_model(text, "deadbeefdeadbeef").unwrap(),
            "default_model = \"deadbeefdeadbeef\" # note\n[server]\nlisten_port = 1\n"
        );
        assert!(rewrite_default_model("[server]\ndefault_model = \"x\"\n", "h").is_err());
    }
}
