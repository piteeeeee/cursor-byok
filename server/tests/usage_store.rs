//! Exercises the application's provider recorder against a local SSE upstream.
use cursor_server::{
    config::{Config, Paths},
    App, Settings,
};

#[tokio::test]
async fn twenty_local_provider_calls_are_exported_without_payloads() {
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
    let app = App::new(Settings::from_config(config, paths).unwrap())
        .await
        .unwrap();
    for i in 0..20 {
        app.control()
            .test_model(&hash, &format!("test-{i}"))
            .await
            .unwrap();
    }
    assert_eq!(app.control().summary().await.unwrap().today_tokens, 200);
    let csv = std::fs::read_to_string(app.control().export_csv().await.unwrap()).unwrap();
    drop(app);
    upstream.abort();
    let _ = upstream.await;
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
