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

