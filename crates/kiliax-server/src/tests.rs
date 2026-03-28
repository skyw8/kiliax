use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt as _;
use kiliax_core::config::{Config, ProviderConfig};
use kiliax_core::session::SessionId;
use tempfile::TempDir;
use tower::ServiceExt as _;

use crate::state::ServerState;

fn test_config() -> Config {
    let mut cfg = Config::default();
    cfg.default_model = Some("test/test-model".to_string());
    cfg.providers.insert(
        "test".to_string(),
        ProviderConfig {
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: None,
            models: vec!["test-model".to_string()],
        },
    );
    cfg.mcp.servers = Vec::new();
    cfg
}

async fn build_test_app(dir: &TempDir, token: Option<String>) -> axum::Router {
    let workspace_root = dir.path().to_path_buf();
    let config_path = workspace_root.join("kiliax.yaml");
    let state = Arc::new(
        ServerState::new_for_tests(workspace_root, config_path, test_config(), token)
            .await
            .expect("new_for_tests"),
    );
    crate::build_app(state)
}

fn req_empty(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

fn req_json(method: Method, uri: &str, json: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json.to_string()))
        .expect("request")
}

fn req_json_with_headers(
    method: Method,
    uri: &str,
    json: serde_json::Value,
    headers: &[(&str, &str)],
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    builder.body(Body::from(json.to_string())).expect("request")
}

async fn read_json(resp: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::Value::Null);
    (status, value)
}

async fn read_text(resp: axum::response::Response) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, text)
}

fn assert_error_code(body: &serde_json::Value, code: &str) {
    assert_eq!(
        body.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|v| v.as_str()),
        Some(code),
        "unexpected error body: {body}"
    );
}

#[tokio::test]
async fn web_serves_hint_when_dist_missing() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/"))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("kiliax-web is not built"), "body: {body}");
}

#[tokio::test]
async fn web_serves_dist_and_spa_fallback() {
    let dir = TempDir::new().expect("tempdir");
    let dist_dir = dir.path().join("web").join("dist");
    tokio::fs::create_dir_all(dist_dir.join("assets"))
        .await
        .expect("mkdir");
    tokio::fs::write(dist_dir.join("index.html"), "<html>INDEX_OK</html>")
        .await
        .expect("write index");
    tokio::fs::write(dist_dir.join("assets").join("hello.txt"), "HELLO_ASSET")
        .await
        .expect("write asset");

    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/"))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("INDEX_OK"), "body: {body}");

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/some/deep/link"))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("INDEX_OK"), "body: {body}");

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/assets/hello.txt"))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "HELLO_ASSET");
}

#[tokio::test]
async fn create_session_accepts_empty_body_and_persists_settings() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);

    let session_id = body
        .get("id")
        .and_then(|v| v.as_str())
        .expect("session id")
        .to_string();

    let settings_path = dir
        .path()
        .join(".kiliax")
        .join("sessions")
        .join(&session_id)
        .join("settings.json");
    let settings_text = tokio::fs::read_to_string(&settings_path)
        .await
        .expect("settings.json");
    let parsed: serde_json::Value = serde_json::from_str(&settings_text).expect("settings json");
    assert_eq!(parsed.get("model_id").and_then(|v| v.as_str()), Some("test/test-model"));
}

#[tokio::test]
async fn idempotency_key_makes_create_session_idempotent() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let headers = [("Idempotency-Key", "abc")];

    let resp1 = app
        .clone()
        .oneshot(req_json_with_headers(
            Method::POST,
            "/v1/sessions",
            serde_json::json!({}),
            &headers,
        ))
        .await
        .expect("oneshot");
    let (status1, body1) = read_json(resp1).await;
    assert_eq!(status1, StatusCode::CREATED);
    let id1 = body1.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let resp2 = app
        .clone()
        .oneshot(req_json_with_headers(
            Method::POST,
            "/v1/sessions",
            serde_json::json!({}),
            &headers,
        ))
        .await
        .expect("oneshot");
    let (status2, body2) = read_json(resp2).await;
    assert_eq!(status2, StatusCode::CREATED);
    let id2 = body2.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    assert_eq!(id1, id2);
}

#[tokio::test]
async fn list_sessions_shows_archived_after_restart() {
    let dir = TempDir::new().expect("tempdir");
    let app1 = build_test_app(&dir, None).await;

    let resp = app1
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (_status, body) = read_json(resp).await;
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let app2 = build_test_app(&dir, None).await;

    let resp = app2
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/sessions?live=true"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("items").and_then(|v| v.as_array()).unwrap().len(), 0);

    let resp = app2
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);

    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    let found = items.iter().find(|it| it.get("id").and_then(|v| v.as_str()) == Some(&session_id));
    let found = found.expect("session listed");
    assert_eq!(
        found.get("status")
            .and_then(|s| s.get("session_state"))
            .and_then(|v| v.as_str()),
        Some("archived")
    );
}

#[tokio::test]
async fn resume_session_makes_it_live() {
    let dir = TempDir::new().expect("tempdir");
    let app1 = build_test_app(&dir, None).await;

    let resp = app1
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (_status, body) = read_json(resp).await;
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let app2 = build_test_app(&dir, None).await;

    let resp = app2
        .clone()
        .oneshot(req_empty(
            Method::POST,
            &format!("/v1/sessions/{session_id}/resume"),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("status")
            .and_then(|s| s.get("session_state"))
            .and_then(|v| v.as_str()),
        Some("live")
    );
}

#[tokio::test]
async fn create_run_auto_resume_false_requires_live_session() {
    let dir = TempDir::new().expect("tempdir");
    let app1 = build_test_app(&dir, None).await;

    let resp = app1
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (_status, body) = read_json(resp).await;
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    // new state => archived
    let app2 = build_test_app(&dir, None).await;

    let resp = app2
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{session_id}/runs"),
            serde_json::json!({
                "input": { "type": "text", "text": "hello" },
                "auto_resume": false
            }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_error_code(&body, "session_not_live");
}

#[tokio::test]
async fn create_run_nonexistent_session_returns_404() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let missing = SessionId::new().to_string();
    let resp = app
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{missing}/runs"),
            serde_json::json!({
                "input": { "type": "text", "text": "hello" },
                "auto_resume": false
            }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_error_code(&body, "not_found");
}

#[tokio::test]
async fn cancel_queued_run_marks_cancelled_and_emits_event() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (_status, body) = read_json(resp).await;
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{session_id}/runs"),
            serde_json::json!({ "input": { "type": "text", "text": "hi" } }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);
    let run_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();
    assert_eq!(body.get("state").and_then(|v| v.as_str()), Some("queued"));

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::POST,
            &format!("/v1/runs/{run_id}/cancel"),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("state").and_then(|v| v.as_str()), Some("cancelled"));

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::GET,
            &format!("/v1/sessions/{session_id}/events?limit=50"),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    assert!(
        items.iter().any(|ev| ev.get("type").and_then(|v| v.as_str()) == Some("run_cancelled")),
        "events missing run_cancelled: {items:?}"
    );
}

#[tokio::test]
async fn patch_settings_persists_and_emits_event() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (_status, body) = read_json(resp).await;
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PATCH,
            &format!("/v1/sessions/{session_id}/settings"),
            serde_json::json!({ "agent": "plan" }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("agent"))
            .and_then(|v| v.as_str()),
        Some("plan")
    );

    let settings_path = dir
        .path()
        .join(".kiliax")
        .join("sessions")
        .join(&session_id)
        .join("settings.json");
    let settings_text = tokio::fs::read_to_string(&settings_path)
        .await
        .expect("settings.json");
    let parsed: serde_json::Value = serde_json::from_str(&settings_text).expect("settings json");
    assert_eq!(parsed.get("agent").and_then(|v| v.as_str()), Some("plan"));

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::GET,
            &format!("/v1/sessions/{session_id}/events?limit=50"),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    assert!(
        items.iter().any(|ev| {
            ev.get("type").and_then(|v| v.as_str()) == Some("session_settings_changed")
        }),
        "events missing session_settings_changed: {items:?}"
    );
}

#[tokio::test]
async fn auth_middleware_enforces_bearer_token() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, Some("secret".to_string())).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/capabilities"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_error_code(&body, "unauthorized");

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/capabilities?token=secret"))
        .await
        .expect("oneshot");
    let (status, _body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/capabilities")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let (status, _body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
}

fn allowed_home_kiliax_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .expect("home_dir")
        .join(".kiliax")
}

fn unique_allowed_workspace_root(prefix: &str) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    allowed_home_kiliax_dir().join(format!("{prefix}_{ts}_{}", std::process::id()))
}

#[tokio::test]
async fn create_session_sets_workspace_root_and_updated_at() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);

    let ws = body
        .get("settings")
        .and_then(|s| s.get("workspace_root"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(!ws.trim().is_empty(), "workspace_root missing: {body}");
    assert!(
        std::path::Path::new(ws).starts_with(&allowed_home_kiliax_dir()),
        "workspace_root not under ~/.kiliax: {ws}"
    );

    let updated_at = body.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
    assert!(!updated_at.trim().is_empty(), "updated_at missing: {body}");
}

#[tokio::test]
async fn patch_settings_rejects_workspace_root_outside_kiliax_home() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (_status, body) = read_json(resp).await;
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PATCH,
            &format!("/v1/sessions/{session_id}/settings"),
            serde_json::json!({ "workspace_root": "/tmp" }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error_code(&body, "invalid_argument");
}

#[tokio::test]
async fn list_skills_returns_workspace_skills() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let workspace_root = unique_allowed_workspace_root("kiliax_test_workspace");
    let skill_dir = workspace_root.join("skills").join("demo_skill");
    tokio::fs::create_dir_all(&skill_dir)
        .await
        .expect("create skill dir");
    tokio::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: Demo Skill\ndescription: Hello\n---\n# Demo\n",
    )
    .await
    .expect("write SKILL.md");

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::POST,
            "/v1/sessions",
            serde_json::json!({
                "settings": {
                    "workspace_root": workspace_root.display().to_string(),
                }
            }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::GET,
            &format!("/v1/sessions/{session_id}/skills"),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    assert!(
        items.iter().any(|it| it.get("id").and_then(|v| v.as_str()) == Some("demo_skill")),
        "missing demo_skill: {items:?}"
    );

    let _ = tokio::fs::remove_dir_all(&workspace_root).await;
}

#[tokio::test]
async fn put_config_updates_live_session_model_when_removed() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (_status, body) = read_json(resp).await;
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let new_yaml = r#"
default_model: test/new-model
providers:
  test:
    base_url: http://127.0.0.1:1
    models:
      - new-model
mcp:
  servers: []
"#;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PUT,
            "/v1/config",
            serde_json::json!({ "yaml": new_yaml }),
        ))
        .await
        .expect("oneshot");
    let (status, _body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, &format!("/v1/sessions/{session_id}")))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("model_id"))
            .and_then(|v| v.as_str()),
        Some("test/new-model"),
        "session settings not normalized after config update: {body}"
    );
}

#[tokio::test]
async fn list_sessions_last_outcome_reflects_meta_finish_or_error() {
    let dir = TempDir::new().expect("tempdir");
    let app1 = build_test_app(&dir, None).await;

    let resp = app1
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (_status, body) = read_json(resp).await;
    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    let meta_path = dir
        .path()
        .join(".kiliax")
        .join("sessions")
        .join(&session_id)
        .join("meta.json");
    let meta_text = tokio::fs::read_to_string(&meta_path).await.expect("meta");
    let mut meta: serde_json::Value = serde_json::from_str(&meta_text).expect("meta json");
    meta["last_finish_reason"] = serde_json::Value::String("done".to_string());
    let meta_text = serde_json::to_string_pretty(&meta).expect("meta serialize");
    tokio::fs::write(&meta_path, meta_text).await.expect("meta write");

    let app2 = build_test_app(&dir, None).await;
    let resp = app2
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    let found = items
        .iter()
        .find(|it| it.get("id").and_then(|v| v.as_str()) == Some(&session_id))
        .expect("session");
    assert_eq!(
        found.get("last_outcome").and_then(|v| v.as_str()),
        Some("done")
    );

    // Now mark as error and ensure it takes precedence.
    let meta_text = tokio::fs::read_to_string(&meta_path).await.expect("meta");
    let mut meta: serde_json::Value = serde_json::from_str(&meta_text).expect("meta json");
    meta["last_error"] = serde_json::Value::String("boom".to_string());
    let meta_text = serde_json::to_string_pretty(&meta).expect("meta serialize");
    tokio::fs::write(&meta_path, meta_text).await.expect("meta write");

    let app3 = build_test_app(&dir, None).await;
    let resp = app3
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    let found = items
        .iter()
        .find(|it| it.get("id").and_then(|v| v.as_str()) == Some(&session_id))
        .expect("session");
    assert_eq!(
        found.get("last_outcome").and_then(|v| v.as_str()),
        Some("error")
    );
}
