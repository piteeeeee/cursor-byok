//! Entry point: take the single-instance lock, then run the menu bar.
//!
//! Two threads, one process. The main thread owns AppKit (the tray icon and its
//! menu); a Tokio runtime owns `App` and every network/disk task. Menu events are
//! forwarded into the event loop, and refresh results are forwarded back out via
//! an `EventLoopProxy`, so no async work ever touches AppKit.

mod actions;
mod resource_limits;
mod tray;

use std::{
    fs::OpenOptions,
    process::ExitCode,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use muda::{MenuEvent, MenuId};
use tao::{
    event::Event,
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder},
    platform::macos::{ActivationPolicy, EventLoopExtMacOS},
};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use actions::{Action, Backend, MenuState};

/// How often the menu's read-only rows are recomputed.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Everything the main thread has to react to.
enum UserEvent {
    /// A menu item was activated.
    Menu(MenuId),
    /// A fresh snapshot is ready to display.
    Snapshot(Box<MenuState>),
    /// The backend reported a problem; show it where the error row goes.
    Failed(String),
    /// Shut the event loop down.
    Quit,
}

fn main() -> ExitCode {
    if let Err(error) = resource_limits::raise_open_file_limit() {
        eprintln!("cursor-byok-lite: cannot raise the open file limit: {error}");
        return ExitCode::FAILURE;
    }
    let Some(_single_instance) = SingleInstance::acquire() else {
        eprintln!("cursor-byok-lite: another instance already owns the menu bar");
        return ExitCode::SUCCESS;
    };
    let runtime = match runtime_with_logging() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("cursor-byok-lite: {error}");
            return ExitCode::FAILURE;
        }
    };
    let shutdown = CancellationToken::new();
    let quitting = Arc::new(AtomicBool::new(false));
    let backend = match Backend::start(runtime, shutdown.clone(), quitting.clone()) {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("cursor-byok-lite: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut event_loop: EventLoop<UserEvent> = EventLoopBuilder::with_user_event().build();
    // A menu bar accessory: no Dock icon, no app switcher entry, no main menu.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let sender = event_loop.create_proxy();

    // muda emits events on the thread that owns the menu; hand them to the loop.
    let menu_sender = sender.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = menu_sender.send_event(UserEvent::Menu(event.id));
    }));

    actions::watch_stop_signals(&backend, sender.clone());

    // The tray is created on the first loop iteration: tray-icon documents that
    // the event loop must already be running, or fullscreen apps hide the icon.
    let mut tray: Option<tray::Tray> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(tao::event::StartCause::Init) => {
                match tray::Tray::install() {
                    Ok(installed) => {
                        tracing::info!("menu bar icon installed");
                        tray = Some(installed);
                        actions::refresh(&backend, &sender);
                    }
                    Err(error) => {
                        eprintln!("cursor-byok-lite: cannot create the menu bar icon: {error}");
                        backend.shutdown.cancel();
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                }
                *control_flow =
                    ControlFlow::WaitUntil(std::time::Instant::now() + REFRESH_INTERVAL);
            }
            Event::NewEvents(tao::event::StartCause::ResumeTimeReached { .. }) => {
                actions::refresh(&backend, &sender);
                *control_flow =
                    ControlFlow::WaitUntil(std::time::Instant::now() + REFRESH_INTERVAL);
            }
            Event::UserEvent(UserEvent::Menu(id)) => {
                let Some(installed) = tray.as_ref() else {
                    return;
                };
                match installed.action_for(&id) {
                    Action::Quit => actions::quit(backend.clone(), sender.clone()),
                    Action::Ignore => {}
                    action => actions::run(action, backend.clone(), sender.clone()),
                }
            }
            Event::UserEvent(UserEvent::Snapshot(state)) => {
                if let Some(installed) = tray.as_mut() {
                    installed.sync_models(&state.catalog, &state.default_model);
                    installed.apply(state.as_ref());
                }
                *control_flow =
                    ControlFlow::WaitUntil(std::time::Instant::now() + REFRESH_INTERVAL);
            }
            Event::UserEvent(UserEvent::Failed(message)) => {
                if let Some(installed) = tray.as_mut() {
                    installed.fail(&message);
                }
            }
            Event::UserEvent(UserEvent::Quit) => {
                backend.shutdown.cancel();
                *control_flow = ControlFlow::Exit;
            }

            _ => {}
        }
    });
}

/// Keeps a second instance from fighting over the same Cursor settings.
struct SingleInstance {
    _file: std::fs::File,
}

impl SingleInstance {
    fn acquire() -> Option<Self> {
        use std::os::unix::io::AsRawFd;

        let directory = cursor_server::config::managed_data_dir().ok()?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("menubar.lock"))
            .ok()?;
        // SAFETY: `file` owns the descriptor for the whole process lifetime.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        (locked == 0).then_some(Self { _file: file })
    }
}

/// Builds the runtime and installs a rolling file logger under the app directory.
fn runtime_with_logging() -> Result<Runtime, String> {
    use tracing_subscriber::prelude::*;

    let directory = cursor_server::config::managed_data_dir()
        .map_err(|error| format!("cannot resolve the application directory: {error}"))?;
    let logs = directory.join("logs");
    std::fs::create_dir_all(&logs)
        .map_err(|error| format!("cannot create {}: {error}", logs.display()))?;
    // Written directly, not buffered through a worker: the event loop exits the
    // process without unwinding, so a buffered line would be lost exactly when a
    // crash report matters most.
    let writer = tracing_appender::rolling::daily(&logs, "menubar.log");
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cursor_byok_lite=info,cursor_server=info".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        )
        .init();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("cursor-byok-lite")
        .build()
        .map_err(|error| format!("cannot start the runtime: {error}"))
}
