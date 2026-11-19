use super::preferences::*;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

use futures_util::StreamExt;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{Config, Models},
    local_app::CursorHarness,
    model::{
        ContentPart, ModelConfig, ModelInvocation, ModelRequest, ModelSpec, ProjectedContent,
        ProjectedMessage, PromptSpec, Role,
    },
    provider::{is_valid_response_event, ModelEvent, Provider},
    store::{UsageStore, UsageSummary},
    Error, Result,
};

#[derive(Clone)]
pub struct ControlService {
    store: UsageStore,
    preferences: Arc<Mutex<(DesktopSettings, TokenPricingSettings)>>,
    exports: std::path::PathBuf,
    config: Config,
    cursor_harness: CursorHarness,
    provider: Arc<dyn Provider>,
    model_tests: Arc<Mutex<BTreeMap<String, CancellationToken>>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelConnectivityResult {
    pub duration_ms: u64,
    pub first_valid_response_ms: Option<u64>,
    pub output_tokens: u64,
    pub tokens_per_second: f64,
    pub tokens_estimated: bool,
    pub output: String,
}

impl ControlService {
    pub fn new(
        store: UsageStore,
        provider: Arc<dyn Provider>,
        _clients: crate::network::NetworkClients,
        config: Config,
        ca_dir: std::path::PathBuf,
    ) -> Result<Self> {
        Ok(Self {
            preferences: Arc::default(),
            exports: ca_dir.with_file_name("exports"),
            cursor_harness: CursorHarness::new(
                config.proxy_port,
                config.models.all().len(),
                ca_dir,
            )?,
            store,
            config,
            provider,
            model_tests: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn cursor_harness(&self) -> &CursorHarness {
        &self.cursor_harness
    }

    pub fn models(&self) -> &[ModelConfig] {
        self.config.models.all()
    }

    pub fn models_by_hash(&self) -> Models {
        self.config.models.clone()
    }

    pub fn default_model(&self) -> &str {
        &self.config.default_model
    }

    pub async fn summary(&self) -> Result<UsageSummary> {
        self.store.today_and_month_summary().await
    }
    pub async fn export_csv(&self) -> Result<String> {
        tokio::fs::create_dir_all(&self.exports).await?;
        let path = self
            .exports
            .join(format!("usage-{}.csv", uuid::Uuid::new_v4()));
        self.store.export_csv(&path).await?;
        Ok(path.display().to_string())
    }
    pub async fn test_model(
        &self,
        model_hash: &str,
        test_id: &str,
    ) -> Result<ModelConnectivityResult> {
        let cancellation = CancellationToken::new();
        let cancellation = {
            let mut tests = self
                .model_tests
                .lock()
                .expect("model test registry mutex poisoned");
            tests
                .entry(test_id.to_owned())
                .or_insert_with(|| cancellation.clone())
                .clone()
        };
        let result = self.run_model_test(model_hash, cancellation).await;
        self.model_tests
            .lock()
            .expect("model test registry mutex poisoned")
            .remove(test_id);
        result
    }

    pub fn cancel_model_test(&self, test_id: &str) {
        let cancellation = {
            let mut tests = self
                .model_tests
                .lock()
                .expect("model test registry mutex poisoned");
            tests.entry(test_id.to_owned()).or_default().clone()
        };
        cancellation.cancel();
    }

    async fn run_model_test(
        &self,
        model_hash: &str,
        cancellation: CancellationToken,
    ) -> Result<ModelConnectivityResult> {
        const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
        const TEST_PROMPT: &str = "Output the numbers 1 through 120 separated by a single space. No commas, no newlines, no explanation.";

        let mut model = ModelSpec::new(model_hash);
        let configured = self
            .config
            .models
            .get(model_hash)
            .ok_or_else(|| Error::RunNotFound(format!("model {model_hash}")))?;
        configured.configure(&mut model);
        model.max_output_tokens = Some(configured.max_output_tokens().unwrap_or(65_536));
        let call_id = format!("model-test-{}", uuid::Uuid::new_v4());
        let invocation = ModelInvocation {
            call_id: call_id.clone(),
            run_id: call_id.clone(),
            conversation_id: call_id.clone(),
            provider_call_index: 0,
            request: ModelRequest {
                prompt: PromptSpec {
                    instructions: String::new(),
                    tools: Vec::new(),
                },
                model,
                history: vec![ProjectedMessage {
                    message_id: "connectivity-test".into(),
                    role: Role::User,
                    content: ProjectedContent::Parts(vec![ContentPart::Text {
                        text: TEST_PROMPT.into(),
                    }]),
                }],
            },
        };
        let started = Instant::now();
        let mut first_valid_response_at = None;
        let mut output_tokens = None;
        let mut output = String::new();
        let stream = self.provider.stream(invocation, cancellation.clone());
        let completed = tokio::time::timeout(TEST_TIMEOUT, async {
            futures_util::pin_mut!(stream);
            let mut finished = false;
            while let Some(event) = stream.next().await {
                let event = event?;
                if first_valid_response_at.is_none() && is_valid_response_event(&event) {
                    first_valid_response_at = Some(Instant::now());
                }
                match event {
                    ModelEvent::TextDelta(delta) => {
                        output.push_str(&delta);
                    }
                    ModelEvent::Usage(usage) => {
                        if let Some(tokens) = usage.output_tokens.filter(|tokens| *tokens > 0) {
                            output_tokens = Some(
                                output_tokens.map_or(tokens, |current: u64| current.max(tokens)),
                            );
                        }
                    }
                    ModelEvent::Done(_) => finished = true,
                    _ => {}
                }
            }
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if !finished {
                return Err(Error::Protocol(
                    "provider stream ended without Done during connectivity test".into(),
                ));
            }
            Ok(())
        })
        .await;
        match completed {
            Ok(result) => result?,
            Err(_) => {
                cancellation.cancel();

                return Err(Error::Provider(
                    "model connectivity test timed out after 45 seconds".into(),
                ));
            }
        }
        let elapsed = started.elapsed();
        let output = output.trim().to_string();
        if first_valid_response_at.is_none() {
            return Err(Error::Provider(
                "model connectivity test received no valid response".into(),
            ));
        }
        let tokens_estimated = output_tokens.is_none();
        let output_tokens = output_tokens.unwrap_or_else(|| estimate_output_tokens(&output));
        Ok(ModelConnectivityResult {
            duration_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            first_valid_response_ms: first_valid_response_at.map(|first| {
                first
                    .duration_since(started)
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            }),
            output_tokens,
            tokens_per_second: if elapsed.is_zero() {
                0.0
            } else {
                output_tokens as f64 / elapsed.as_secs_f64()
            },
            tokens_estimated,
            output,
        })
    }

    pub fn ports(&self) -> PortSettings {
        PortSettings {
            proxy_port: self.config.proxy_port,
            service_port: self.config.listen_port,
        }
    }

    pub async fn statistics_storage(&self) -> Result<StatisticsStorage> {
        Ok(StatisticsStorage {
            call_count: self.store.count().await?,
        })
    }

    pub async fn clear_statistics_storage(&self) -> Result<StatisticsStorage> {
        self.store.clear().await?;
        self.statistics_storage().await
    }

    pub fn proxy_settings(&self) -> ProxySettings {
        match self.config.outbound_proxy.as_deref() {
            Some(address) => ProxySettings {
                mode: ProxyMode::Custom,
                address: address.into(),
                auth_enabled: false,
                username: String::new(),
                has_password: false,
            },
            None => ProxySettings::default(),
        }
    }

    pub async fn desktop_settings(&self) -> Result<DesktopSettings> {
        Ok(self.preferences.lock().unwrap().0)
    }

    pub async fn set_desktop_settings(&self, settings: DesktopSettings) -> Result<()> {
        self.preferences.lock().unwrap().0 = settings;
        Ok(())
    }

    pub async fn pricing_settings(&self) -> Result<TokenPricingSettings> {
        Ok(self.preferences.lock().unwrap().1)
    }

    pub async fn set_pricing_settings(
        &self,
        settings: TokenPricingSettings,
    ) -> Result<TokenPricingSettings> {
        self.preferences.lock().unwrap().1 = settings;
        Ok(settings)
    }
}

fn estimate_output_tokens(output: &str) -> u64 {
    let words = output.split_whitespace().count() as u64;
    if words > 0 {
        words
    } else if output.is_empty() {
        0
    } else {
        (output.chars().count() as u64).div_ceil(4)
    }
}
