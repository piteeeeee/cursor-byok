//! Everything the menu can do.
//!
//! Each action is a thin call into `cursor_server` plus, where a file must open,
//! `open(1)`. Nothing here holds state of its own: `App` owns the server, the
//! `ConfigStore` owns the settings, and the harness owns the takeover. Snapshots
//! are built inside the runtime and rendered by the event loop.

use std::{
    path::Path,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use cursor_server::{
    error::{Error, Result as ServerResult},
    local_app::CaState,
    App, ConfigStore,
};
use tao::event_loop::EventLoopProxy;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::UserEvent;

/// The Tokio side of the process: the server plus the handles to stop it.
pub struct Backend {
    runtime: Runtime,
    /// Cloning a handle outlives a `&self` borrow, so background tasks take this.
    tasks: tokio::runtime::Handle,
    pub app: Arc<App>,
    pub shutdown: CancellationToken,
    pub quitting: Arc<AtomicBool>,
}

/// What the menu displays. Built on the runtime, rendered on the main thread.
#[derive(Default)]
pub struct MenuState {
    pub status: String,
    pub model: String,
    pub usage: String,
    /// Present only when a call actually failed; the row is hidden otherwise.
    pub error: Option<String>,
    pub taking_over: bool,
    pub default_model: String,
    /// `(model_hash, display_name)` in `config.toml` order.
    pub catalog: Vec<(String, String)>,
}

/// A menu click, resolved from its item id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleTakeover,
    SelectModel(String),
    ReloadConfig,
    ExportCsv,
    OpenConfig,
    OpenLogs,
    Quit,
    Ignore,
}

impl Backend {
    /// Builds the server on `runtime`, then serves it in the background.
    ///
    /// `serve_on` consumes an `App`, so the serving copy is a clone of the same
    /// object graph: menu actions keep reading the very components serving uses.
    pub fn start(
        runtime: Runtime,
        shutdown: CancellationToken,
        quitting: Arc<AtomicBool>,
    ) -> ServerResult<Arc<Self>> {
        let app = runtime.block_on(async {
            let app = App::new(ConfigStore::load()?).await?;
            // A stale proxy setting from a previous run never points at this process.
            app.harness().cleanup_stale_settings().await?;
            Ok::<App, Error>(app)
        })?;
        let tasks = runtime.handle().clone();
        let backend = Arc::new(Self {
            runtime,
            tasks,
            app: Arc::new(app),
            shutdown,
            quitting,
        });
        backend.serve_in_background()?;
        Ok(backend)
    }

    fn serve_in_background(&self) -> ServerResult<()> {
        let listener = self.runtime.block_on(self.app.bind())?;
        let address = listener.local_addr()?;
        // Known before serving starts, so the harness can be applied right away.
        self.app.harness().set_backend_addr(address);
        let serving = self.app.as_ref().clone();
        let shutdown = self.shutdown.clone();
        self.tasks.spawn(async move {
            if let Err(error) = serving.serve_on(listener, shutdown).await {
                tracing::error!(%error, "cursor server stopped");
            }
        });
        // Apply the saved pause choice once. A first run without a trusted CA is
        // normal: the menu then offers to take over after the CA is installed.
        if let Err(error) = self.runtime.block_on(self.app.harness().status()) {
            tracing::warn!(%error, "cursor takeover was not restored");
        }
        Ok(())
    }
}

/// Recomputes the read-only rows and hands them to the event loop.
pub fn refresh(backend: &Arc<Backend>, sender: &EventLoopProxy<UserEvent>) {
    let sender = sender.clone();
    let task = backend.clone();
    backend.tasks.spawn(async move {
        let state = snapshot(&task).await;
        let _ = sender.send_event(UserEvent::Snapshot(Box::new(state)));
    });
}

/// Runs one menu action, then asks the loop for a fresh snapshot.
pub fn run(action: Action, backend: Arc<Backend>, sender: EventLoopProxy<UserEvent>) {
    let task = backend.clone();
    backend.tasks.spawn(async move {
        match perform(&action, &task).await {
            Ok(()) => {
                let state = snapshot(&task).await;
                let _ = sender.send_event(UserEvent::Snapshot(Box::new(state)));
            }
            Err(error) => {
                tracing::warn!(%error, ?action, "menu action failed");
                let _ = sender.send_event(UserEvent::Failed(error.to_string()));
            }
        }
    });
}

/// Restores Cursor's settings when the process is asked to stop from outside.
pub fn watch_stop_signals(backend: &Arc<Backend>, sender: EventLoopProxy<UserEvent>) {
    let task = backend.clone();
    backend.tasks.spawn(async move {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    tracing::warn!(%error, "cannot watch SIGTERM");
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT"),
            _ = terminate.recv() => tracing::info!("received SIGTERM"),
        }
        quit(task, sender);
    });
}

/// Stops the server, restores Cursor's settings, then asks the loop to exit.
pub fn quit(backend: Arc<Backend>, sender: EventLoopProxy<UserEvent>) {
    if backend.quitting.swap(true, Ordering::AcqRel) {
        return;
    }
    backend.shutdown.cancel();
    let task = backend.clone();
    backend.tasks.spawn(async move {
        if let Err(error) = task.app.harness().disable().await {
            tracing::warn!(%error, "could not restore Cursor settings while quitting");
        }
        let _ = sender.send_event(UserEvent::Quit);
    });
}

async fn perform(action: &Action, backend: &Backend) -> ServerResult<()> {
    let config = backend.app.config();
    let harness = backend.app.harness();
    match action {
        Action::ToggleTakeover => {
            let status = harness.report().await?;
            if !status.is_taking_over() && matches!(status.ca, CaState::Missing) {
                // Generates the CA files; trusting it in the keychain stays a
                // user step, and the menu reports it when it is still missing.
                harness.initialize_ca().await?;
            }
            harness.set_enabled(!status.is_taking_over()).await?;
        }
        Action::SelectModel(hash) => {
            config.set_default_model(hash)?;
        }
        Action::ReloadConfig => config.reload()?,
        Action::ExportCsv => {
            let directory = config.paths().app_dir.join("exports");
            tokio::fs::create_dir_all(&directory).await?;
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default();
            let path = directory.join(format!("usage-{stamp}.csv"));
            backend.app.usage().export_csv(&path).await?;
            reveal(&path)?;
        }
        Action::OpenConfig => reveal(config.config_file())?,
        Action::OpenLogs => reveal(&config.paths().app_dir.join("logs"))?,
        Action::Quit | Action::Ignore => {}
    }
    Ok(())
}

/// Builds the menu's current text from the live state.
async fn snapshot(backend: &Backend) -> MenuState {
    let app = &backend.app;
    let config = app.config();
    let catalog: Vec<(String, String)> = config
        .catalog()
        .into_iter()
        .map(|model| (model.model_hash, model.display_name))
        .collect();
    let default_model = config.default_model();
    let display_name = catalog
        .iter()
        .find(|(hash, _)| *hash == default_model)
        .map(|(_, name)| name.clone())
        .unwrap_or_else(|| default_model.clone());

    // Read-only: the saved choice was applied once at startup, and a periodic
    // repaint must never rewrite Cursor's settings.
    let (taking_over, status) = match app.harness().report().await {
        Ok(report) if report.is_taking_over() => (
            true,
            format!(
                "状态: 接管中 ({} / {})",
                config.listen_addr().port(),
                config.proxy_port()
            ),
        ),
        Ok(_) => (false, "状态: 已暂停".to_owned()),
        // A missing CA is the normal first-run state, not a failure.
        Err(error) => (false, format!("状态: 待初始化 CA ({error})")),
    };
    let usage = match app.usage().today_and_month_summary().await {
        Ok(summary) => format!(
            "今日 Token: {} | 本月: {}",
            summary.today_tokens, summary.month_tokens
        ),
        Err(error) => format!("今日 Token: — | 本月: — ({error})"),
    };
    let error = match app.usage().latest_error().await {
        Ok(Some(message)) => Some(format!("最近错误: {}", truncate(&message, 48))),
        Ok(None) => None,
        Err(error) => Some(format!("最近错误: 读取失败 ({error})")),
    };
    MenuState {
        status,
        model: format!("默认模型: {display_name}"),
        usage,
        error,
        taking_over,
        default_model,
        catalog,
    }
}

/// Opens a path in the Finder, revealing it when it is a file.
fn reveal(path: &Path) -> ServerResult<()> {
    let mut command = Command::new("open");
    if path.is_file() {
        command.arg("-R");
    }
    let status = command
        .arg(path)
        .status()
        .map_err(|error| Error::Store(format!("cannot run open: {error}")))?;
    if !status.success() {
        return Err(Error::Store(format!("open exited with {status}")));
    }
    Ok(())
}

fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_owned(),
    }
}
