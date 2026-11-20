//! Headless Cursor-BYOK server: one in-process library an embedding app drives.
//!
//! These four types are the whole integration surface. The host process owns the
//! Tokio runtime and the lifetime; this crate owns the Cursor protocol, the
//! conversation state, provider routing, and usage accounting.
pub mod api;
pub mod app;
pub mod config;
pub mod cursor;
pub mod error;
pub mod local_app;
pub mod model;
pub mod network;
pub mod provider;
pub mod run;
pub mod search;
pub mod store;
pub(crate) mod timing;

// Internal paths (`crate::Error`, `crate::Result`) stay available to modules
// without widening the public API.
pub(crate) use error::{Error, Result};

pub use app::App;
pub use config::ConfigStore;
pub use local_app::CursorHarness;
pub use store::UsageStore;
