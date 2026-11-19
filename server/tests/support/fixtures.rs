//! Provides isolated stores, model catalogs, and canonical message fixtures for tests.
#![allow(dead_code)]

use std::path::PathBuf;

use cursor_server::{
    config::RuntimeConfig,
    model::{CanonicalMessage, ModelConfig, ModelConfigInput, Origin, Role},
    store::MemoryStore,
};

pub async fn temp_store() -> (tempfile::TempDir, MemoryStore) {
    let directory = tempfile::tempdir().unwrap();
    (directory, MemoryStore::default())
}

/// Builds a configured model outside the database; step 3 removed model rows.
pub fn model(input: ModelConfigInput) -> ModelConfig {
    ModelConfig::from_input(input, None).expect("test model configuration")
}

/// Runtime configuration carrying `models`, with the first one as default.
pub fn runtime(models: Vec<ModelConfig>) -> RuntimeConfig {
    let default_model = models
        .first()
        .map(|model| model.model_hash.clone())
        .unwrap_or_default();
    RuntimeConfig::new(
        cursor_server::config::Models::new(models).expect("test models"),
        default_model,
        None,
        PathBuf::from("test-ca"),
    )
}

/// Runtime configuration with no models, for tests that never route a model.
pub fn empty_runtime() -> RuntimeConfig {
    runtime(Vec::new())
}

pub fn user(id: &str, text: &str) -> CanonicalMessage {
    CanonicalMessage::text(id, Role::User, Origin::User, text)
}
