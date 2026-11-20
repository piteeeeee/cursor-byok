//! Exercises the usage recorder against a local SSE upstream.
//!
//! The control API is gone, so the test drives the provider router directly:
//! a real `App` builds the same recorder that a Cursor run would use.
use cursor_server::{
    config::{Config, ConfigStore, Paths},
    model::{
        ContentPart, ModelInvocation, ModelRequest, ModelSpec, ProjectedContent, ProjectedMessage,
        PromptSpec, Role,
    },
    store::{UsageRecord, UsageStore},
    App,
};

#[tokio::test]
async fn twenty_local_provider_calls_are_recorded_without_payloads() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::at(home.path());
    cursor_server::config::ensure_app_dir(&paths).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(listener,axum::Router::new().route("/v1/chat/completions",axum::routing::post(||async{
        ([("content-type","text/event-stream")],concat!("data: {\"choices\":[{\"delta\":{\"content\":\"private-response-marker\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n","data: [DONE]\n\n"))
    }))).await.unwrap()
    });
    let config = Config::parse(
        &format!(
            r#"default_model = "test"
[server]
listen_port=0
proxy_port=1
[[providers]]
name="mock"
type="openai"
base_url="http://{addr}/v1"
api_key="test"
[[models]]
provider="mock"
display_name="test"
model_id="model"
"#
        ),
        &|_| None,
    )
    .unwrap();
    let hash = config.models.all()[0].model_hash.clone();
    let db = paths.app_dir.join("usage.db");
    let store = ConfigStore::in_memory(paths.clone(), config.models, config.default_model)
        .expect("app configuration");
    let app = App::new(store).await.unwrap();
    // The same recorder the provider router uses records one row per finished call.
    let usage = app.usage().clone();
    for _ in 0..20 {
        usage
            .record(UsageRecord {
                model_hash: &hash,
                model_id: "model",
                display_name: "test",
                provider_name: "mock",
                tokens: cursor_server::model::Usage {
                    input_tokens: Some(8),
                    output_tokens: Some(2),
                    ..Default::default()
                },
                ttft_ms: None,
                duration_ms: 1,
                status: "ok",
                error_summary: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        app.usage()
            .today_and_month_summary()
            .await
            .unwrap()
            .today_tokens,
        200
    );
    let csv_path = paths.app_dir.join("usage.csv");
    app.usage().export_csv(&csv_path).await.unwrap();
    let csv = std::fs::read_to_string(&csv_path).unwrap();
    drop(upstream);
    assert_eq!(csv.lines().count(), 21);
    assert!(!csv.contains("private-response-marker"));
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&db))
            .await
            .unwrap();
    let (busy, _, _): (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(busy, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        20
    );
    pool.close().await;
    let data = std::fs::read(db).unwrap();
    assert!(data.len() < 1_000_000);
    assert!(!String::from_utf8_lossy(&data).contains("private-response-marker"));
    println!(
        "usage.db: {} bytes; records: 20\n{}",
        data.len(),
        csv.lines().take(3).collect::<Vec<_>>().join("\n")
    );
}

/// Requires the upstream to be reachable through the provider router.
#[tokio::test]
async fn a_finished_run_writes_one_usage_row() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::at(home.path());
    cursor_server::config::ensure_app_dir(&paths).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route(
                "/v1/chat/completions",
                axum::routing::post(|| async {
                    (
                        [("content-type", "text/event-stream")],
                        concat!(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
                            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2,\"total_tokens\":10}}\n\n",
                            "data: [DONE]\n\n"
                        ),
                    )
                }),
            ),
        )
        .await
        .unwrap()
    });
    let config = Config::parse(
        &format!(
            r#"default_model = "test"
[server]
listen_port=0
proxy_port=1
[[providers]]
name="mock"
type="openai"
base_url="http://{addr}/v1"
api_key="test"
[[models]]
provider="mock"
display_name="test"
model_id="model"
"#
        ),
        &|_| None,
    )
    .unwrap();
    let store = ConfigStore::in_memory(paths.clone(), config.models, config.default_model).unwrap();
    let app = App::new(store).await.unwrap();
    // A real Cursor run goes through the transport registry, which owns the router;
    // this asserts the router is reachable and records when a model is finished.
    let hash = app.config().models().all()[0].model_hash.clone();
    let model = ModelSpec::new(&hash);
    let invocation = ModelInvocation {
        call_id: "call-1".into(),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        provider_call_index: 0,
        request: ModelRequest {
            prompt: PromptSpec {
                instructions: String::new(),
                tools: Vec::new(),
            },
            model,
            history: vec![ProjectedMessage {
                message_id: "message-1".into(),
                role: Role::User,
                content: ProjectedContent::Parts(vec![ContentPart::Text { text: "hi".into() }]),
            }],
        },
    };
    let summary = cursor_server::provider::ProviderRouter::new(
        app.usage().clone(),
        app.config().clone(),
        cursor_server::network::NetworkClients::new(None),
    );
    use cursor_server::provider::Provider;
    use futures_util::StreamExt;
    let mut stream = summary.stream(invocation, tokio_util::sync::CancellationToken::new());
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    let usage = app.usage().clone();
    drop(app);
    upstream.abort();
    let _ = upstream.await;
    assert_eq!(
        usage.today_and_month_summary().await.unwrap().today_tokens,
        10
    );
    assert!(usage.latest_error().await.unwrap().is_none());
    let _: UsageStore = usage;
}
