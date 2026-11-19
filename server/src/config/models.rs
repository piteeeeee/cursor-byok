//! Holds the configured models: the single source for the model catalog.
use std::collections::HashSet;

use crate::{model::ModelConfig, Error, Result};

/// Models in configuration order, indexed by the hash Cursor routes with.
#[derive(Clone, Debug, Default)]
pub struct Models {
    ordered: Vec<ModelConfig>,
    hashes: HashSet<String>,
}

impl Models {
    pub fn new(models: Vec<ModelConfig>) -> Result<Self> {
        let mut hashes = HashSet::with_capacity(models.len());
        for model in &models {
            if model.model_hash.trim().is_empty() {
                return Err(Error::Config("model hash cannot be empty".into()));
            }
            if !hashes.insert(model.model_hash.clone()) {
                return Err(Error::Config(format!(
                    "duplicate model_hash: {} (set model_hash explicitly or rename the model)",
                    model.model_hash
                )));
            }
        }
        Ok(Self {
            ordered: models,
            hashes,
        })
    }

    pub fn all(&self) -> &[ModelConfig] {
        &self.ordered
    }

    pub fn get(&self, model_hash: &str) -> Option<&ModelConfig> {
        self.hashes
            .contains(model_hash)
            .then(|| {
                self.ordered
                    .iter()
                    .find(|model| model.model_hash == model_hash)
            })
            .flatten()
    }

    pub fn contains(&self, model_hash: &str) -> bool {
        self.hashes.contains(model_hash)
    }

    /// Resolves `default_model`, which is either a `model_hash` or a display name.
    pub fn resolve_default(&self, value: &str) -> Result<&ModelConfig> {
        let value = value.trim();
        if let Some(model) = self.get(value) {
            return Ok(model);
        }
        if let Some(model) = self
            .ordered
            .iter()
            .find(|model| model.display_name == value)
        {
            return Ok(model);
        }
        Err(Error::Config(format!(
            "default_model {value} does not match any [[models]] entry by model_hash or display_name"
        )))
    }

    pub fn default(&self) -> &ModelConfig {
        &self.ordered[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(hash: &str, display_name: &str) -> ModelConfig {
        let mut model = ModelConfig {
            model_hash: hash.into(),
            sort_order: 0,
            display_name: display_name.into(),
            group_name: None,
            model_type: crate::model::ModelType::OpenAi,
            base_url: "https://example.com/v1".into(),
            use_full_url: false,
            api_key: "key".into(),
            tooltip_data: display_name.into(),
            model_id: display_name.into(),
            reasoning_effort: None,
            openai_endpoint: crate::model::OPENAI_CHAT_ENDPOINT.into(),
            openai_extra_params_enabled: false,
            openai_extra_params: serde_json::json!({}),
            custom_headers_enabled: false,
            custom_headers: serde_json::json!({}),
            anthropic_extra_params_enabled: false,
            anthropic_extra_params: serde_json::json!({}),
            context_window_tokens: None,
            max_completion_tokens: None,
            anthropic_max_tokens: None,
            anthropic_thinking_effort: None,
            thinking_budget_tokens: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        model.model_hash = hash.into();
        model
    }

    #[test]
    fn resolves_default_by_hash_or_display_name() {
        let models = Models::new(vec![model("aaaa", "Alpha"), model("bbbb", "Beta")]).unwrap();
        assert_eq!(models.resolve_default("bbbb").unwrap().display_name, "Beta");
        assert_eq!(models.resolve_default("Alpha").unwrap().model_hash, "aaaa");
        assert!(models.resolve_default("missing").is_err());
    }

    #[test]
    fn rejects_duplicate_hashes() {
        let error = Models::new(vec![model("same", "One"), model("same", "Two")]).unwrap_err();
        assert!(
            error.to_string().contains("duplicate model_hash"),
            "{error}"
        );
    }
}
