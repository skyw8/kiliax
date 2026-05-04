use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt as _;
use kiliax_core::config::{Config, ProviderApi, ProviderConfig};
use kiliax_core::protocol::{Message, TokenUsage, UserMessageContent};
use kiliax_core::session::FileSessionStore;
use kiliax_core::session::SessionId;
use tempfile::TempDir;
use tower::ServiceExt as _;

use crate::state::ServerState;

fn test_config() -> Config {
    let mut cfg = Config {
        default_model: Some("test/test-model".to_string()),
        ..Default::default()
    };
    cfg.providers.insert(
        "test".to_string(),
        ProviderConfig {
            api: ProviderApi::OpenAiChatCompletions,
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: None,
            models: vec!["test-model".to_string(), "new-model".to_string()],
        },
    );
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

fn req_empty_with_headers(method: Method, uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    builder.body(Body::empty()).expect("request")
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
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
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
async fn web_requires_auth_and_sets_cookie() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, Some("secret".to_string())).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/"))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("Unauthorized"), "body: {body}");

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/?token=secret"))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FOUND);
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        set_cookie.contains("kiliax_token=secret"),
        "set-cookie missing token: {set_cookie}"
    );
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(location, "/");

    let req = Request::builder()
        .method(Method::GET)
        .uri("/")
        .header(header::COOKIE, "kiliax_token=secret")
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("kiliax-web"), "body: {body}");
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

    let app = build_test_app(&dir, Some("secret".to_string())).await;

    let cookie = ("Cookie", "kiliax_token=secret");

    let resp = app
        .clone()
        .oneshot(req_empty_with_headers(Method::GET, "/", &[cookie]))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("INDEX_OK"), "body: {body}");

    let resp = app
        .clone()
        .oneshot(req_empty_with_headers(
            Method::GET,
            "/some/deep/link",
            &[cookie],
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("INDEX_OK"), "body: {body}");

    let resp = app
        .clone()
        .oneshot(req_empty_with_headers(
            Method::GET,
            "/assets/hello.txt",
            &[cookie],
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "HELLO_ASSET");
}

#[tokio::test]
async fn openapi_and_docs_are_served() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, Some("secret".to_string())).await;

    let auth = ("Authorization", "Bearer secret");

    let resp = app
        .clone()
        .oneshot(req_empty_with_headers(
            Method::GET,
            "/v1/openapi.json",
            &[auth],
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("openapi").and_then(|v| v.as_str()), Some("3.1.0"));
    assert!(
        body.get("paths")
            .and_then(|p| p.get("/v1/sessions"))
            .is_some(),
        "paths missing /v1/sessions: {body}"
    );

    let resp = app
        .clone()
        .oneshot(req_empty_with_headers(
            Method::GET,
            "/v1/openapi.yaml",
            &[auth],
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("openapi: 3.1.0"), "body: {body}");
    assert!(body.contains("/v1/sessions"), "body: {body}");

    // /docs requires cookie auth (web UI flow).
    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/docs?token=secret"))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FOUND);
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        set_cookie.contains("kiliax_token=secret"),
        "set-cookie missing token: {set_cookie}"
    );

    let resp = app
        .clone()
        .oneshot(req_empty_with_headers(
            Method::GET,
            "/docs/",
            &[("Cookie", "kiliax_token=secret")],
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_text(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.contains("Swagger UI") || body.contains("swagger-ui"),
        "body: {body}"
    );
}

#[tokio::test]
async fn messages_include_usage_when_present() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let store = FileSessionStore::project(dir.path());
    let mut session = store
        .create(
            "general",
            Some("test/test-model".to_string()),
            None,
            Some(dir.path().to_string_lossy().to_string()),
            Vec::new(),
            vec![Message::User {
                content: UserMessageContent::Text("hello".to_string()),
            }],
        )
        .await
        .expect("create session");

    store
        .record_message(
            &mut session,
            Message::Assistant {
                content: Some("hi".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    prompt_tokens: 19,
                    completion_tokens: 21,
                    total_tokens: 40,
                    cached_tokens: Some(10),
                }),
                provider_metadata: None,
            },
        )
        .await
        .expect("record message");

    let uri = format!("/v1/sessions/{}/messages?limit=10", session.id());
    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, &uri))
        .await
        .unwrap();
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let items = body
        .get("items")
        .and_then(|v| v.as_array())
        .expect("items array");
    let assistant = items
        .iter()
        .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        .expect("assistant message");
    let usage = assistant.get("usage").expect("usage");

    assert_eq!(
        usage.get("prompt_tokens").and_then(|v| v.as_u64()),
        Some(19)
    );
    assert_eq!(
        usage.get("completion_tokens").and_then(|v| v.as_u64()),
        Some(21)
    );
    assert_eq!(usage.get("total_tokens").and_then(|v| v.as_u64()), Some(40));
    assert_eq!(
        usage.get("cached_tokens").and_then(|v| v.as_u64()),
        Some(10)
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

    let store = FileSessionStore::project(dir.path());
    let id = SessionId::parse(&session_id).expect("session id");
    let session = store.load(&id).await.expect("load session");
    assert_eq!(
        session.meta.model_id.as_deref(),
        Some("test/test-model"),
        "meta should persist model_id"
    );
}

#[tokio::test]
async fn fork_session_inherits_prompt_cache_key() {
    let dir = TempDir::new().expect("tempdir");
    let workspace_root = dir.path().to_path_buf();
    let config_path = workspace_root.join("kiliax.yaml");
    let state =
        ServerState::new_for_tests(workspace_root.clone(), config_path, test_config(), None)
            .await
            .expect("new_for_tests");

    let store = state.store.clone();
    let mut source = store
        .create(
            "general",
            Some("test/test-model".to_string()),
            None,
            Some(workspace_root.display().to_string()),
            Vec::new(),
            vec![Message::User {
                content: UserMessageContent::Text("hello".to_string()),
            }],
        )
        .await
        .expect("create source session");

    let expected = source.meta.prompt_cache_key.clone();
    store
        .record_finish(&mut source, Some("test".to_string()))
        .await
        .expect("record finish");
    store
        .record_message(
            &mut source,
            Message::Assistant {
                content: Some("hi".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
                provider_metadata: None,
            },
        )
        .await
        .expect("record assistant");
    let assistant_message_id = source.meta.last_seq.to_string();

    let out = state
        .fork_session(
            source.id(),
            crate::api::ForkSessionRequest {
                message_id: Some(assistant_message_id.clone()),
            }
            .into(),
        )
        .await
        .expect("fork_session");

    let forked_id = SessionId::parse(&out.session.summary.id).expect("forked id");
    let forked = store.load(&forked_id).await.expect("load forked");
    assert_eq!(forked.meta.prompt_cache_key, expected);

    // Ensure the original key didn't change.
    source = store.load(source.id()).await.expect("reload source");
    assert_eq!(source.meta.prompt_cache_key, expected);
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
    let id1 = body1
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

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
    let id2 = body2
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    assert_eq!(id1, id2);
}

#[tokio::test]
async fn delete_session_removes_session() {
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

    let meta_path = dir
        .path()
        .join(".kiliax")
        .join("sessions")
        .join(&session_id)
        .join("meta.json");
    assert!(meta_path.is_file(), "expected meta.json to exist");

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::DELETE,
            &format!("/v1/sessions/{session_id}"),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::GET,
            &format!("/v1/sessions/{session_id}"),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("items")
            .and_then(|v| v.as_array())
            .map(|v| v.len()),
        Some(0),
        "unexpected sessions: {body}"
    );
}

#[tokio::test]
async fn list_sessions_shows_persisted_after_restart() {
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
    assert_eq!(
        body.get("items").and_then(|v| v.as_array()).unwrap().len(),
        0
    );

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
        .find(|it| it.get("id").and_then(|v| v.as_str()) == Some(&session_id));
    let found = found.expect("session listed");
    assert_eq!(
        found
            .get("status")
            .and_then(|s| s.get("run_state"))
            .and_then(|v| v.as_str()),
        Some("idle")
    );
    assert!(
        found
            .get("status")
            .and_then(|s| s.get("session_state"))
            .is_none(),
        "session_state should not be part of the public API"
    );
}

#[tokio::test]
async fn create_run_auto_resumes_session_into_live_only_list() {
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
    assert_eq!(
        body.get("items").and_then(|v| v.as_array()).unwrap().len(),
        0
    );

    let resp = app2
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{session_id}/runs"),
            serde_json::json!({ "input": { "type": "text", "text": "hello" } }),
        ))
        .await
        .expect("oneshot");
    let (status, _body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);

    let resp = app2
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/sessions?live=true"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    assert!(
        items
            .iter()
            .any(|it| it.get("id").and_then(|v| v.as_str()) == Some(&session_id)),
        "expected session to be loaded into live list after create_run"
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

    // new state => not loaded (not live)
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
async fn events_stream_auto_loads_session() {
    let dir = TempDir::new().expect("tempdir");
    let workspace_root = dir.path().to_path_buf();

    let store = FileSessionStore::project(&workspace_root);
    let state = store
        .create(
            "general",
            Some("test/test-model".to_string()),
            None,
            Some(workspace_root.display().to_string()),
            Vec::new(),
            vec![Message::User {
                content: UserMessageContent::Text("u1".to_string()),
            }],
        )
        .await
        .expect("create");

    let session_id = state.id().to_string();
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::GET,
            &format!("/v1/sessions/{session_id}/events/stream?after_event_id=0"),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn edit_user_message_truncates_and_enqueues_from_user_message_run() {
    let dir = TempDir::new().expect("tempdir");
    let workspace_root = dir.path().to_path_buf();

    let store = FileSessionStore::project(&workspace_root);
    let mut state = store
        .create(
            "general",
            Some("test/test-model".to_string()),
            None,
            Some(workspace_root.display().to_string()),
            Vec::new(),
            vec![Message::User {
                content: UserMessageContent::Text("u1".to_string()),
            }],
        )
        .await
        .expect("create");
    store
        .record_message(
            &mut state,
            Message::Assistant {
                content: Some("a1".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
                provider_metadata: None,
            },
        )
        .await
        .expect("assistant1");
    store
        .record_message(
            &mut state,
            Message::User {
                content: UserMessageContent::Text("u2".to_string()),
            },
        )
        .await
        .expect("user2");
    let user2_id = state.meta.last_seq;
    store
        .record_message(
            &mut state,
            Message::Assistant {
                content: Some("a2".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
                provider_metadata: None,
            },
        )
        .await
        .expect("assistant2");
    let assistant2_id = state.meta.last_seq;

    let session_id = state.id().to_string();
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{session_id}/runs"),
            serde_json::json!({
                "input": {
                    "type": "edit_user_message",
                    "user_message_id": user2_id,
                    "content": "u2 edited",
                }
            }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body.get("input")
            .and_then(|i| i.get("type"))
            .and_then(|v| v.as_str()),
        Some("edit_user_message")
    );
    assert_eq!(
        body.get("input")
            .and_then(|i| i.get("user_message_id"))
            .and_then(|v| v.as_u64()),
        Some(user2_id)
    );
    assert_eq!(
        body.get("input")
            .and_then(|i| i.get("content"))
            .and_then(|v| v.as_str()),
        Some("u2 edited")
    );

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::GET,
            &format!("/v1/sessions/{session_id}/messages?limit=50"),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[2]
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<u64>().ok()),
        Some(user2_id)
    );
    assert_eq!(
        items[2].get("content").and_then(|v| v.as_str()),
        Some("u2 edited")
    );
    assert!(
        !items.iter().any(|m| {
            m.get("id")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<u64>().ok())
                == Some(assistant2_id)
        }),
        "assistant2 should be truncated"
    );
}

#[tokio::test]
async fn regenerate_assistant_truncates_and_enqueues_from_user_message_run() {
    let dir = TempDir::new().expect("tempdir");
    let workspace_root = dir.path().to_path_buf();

    let store = FileSessionStore::project(&workspace_root);
    let mut state = store
        .create(
            "general",
            Some("test/test-model".to_string()),
            None,
            Some(workspace_root.display().to_string()),
            Vec::new(),
            vec![Message::User {
                content: UserMessageContent::Text("u1".to_string()),
            }],
        )
        .await
        .expect("create");
    store
        .record_message(
            &mut state,
            Message::Assistant {
                content: Some("a1".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
                provider_metadata: None,
            },
        )
        .await
        .expect("assistant1");
    store
        .record_message(
            &mut state,
            Message::User {
                content: UserMessageContent::Text("u2".to_string()),
            },
        )
        .await
        .expect("user2");
    let user2_id = state.meta.last_seq;
    store
        .record_message(
            &mut state,
            Message::Assistant {
                content: Some("a2".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
                provider_metadata: None,
            },
        )
        .await
        .expect("assistant2");
    let assistant2_id = state.meta.last_seq;

    let session_id = state.id().to_string();
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{session_id}/runs"),
            serde_json::json!({
                "input": {
                    "type": "regenerate_after_user_message",
                    "user_message_id": user2_id,
                }
            }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body.get("input")
            .and_then(|i| i.get("type"))
            .and_then(|v| v.as_str()),
        Some("regenerate_after_user_message")
    );
    assert_eq!(
        body.get("input")
            .and_then(|i| i.get("user_message_id"))
            .and_then(|v| v.as_u64()),
        Some(user2_id)
    );

    let resp = app
        .clone()
        .oneshot(req_empty(
            Method::GET,
            &format!("/v1/sessions/{session_id}/messages?limit=50"),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[2]
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<u64>().ok()),
        Some(user2_id)
    );
    assert!(
        !items.iter().any(|m| {
            m.get("id")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<u64>().ok())
                == Some(assistant2_id)
        }),
        "assistant2 should be truncated"
    );
}

#[tokio::test]
async fn history_mutation_endpoints_return_conflict_when_session_busy() {
    let dir = TempDir::new().expect("tempdir");
    let workspace_root = dir.path().to_path_buf();

    let store = FileSessionStore::project(&workspace_root);
    let mut state = store
        .create(
            "general",
            Some("test/test-model".to_string()),
            None,
            Some(workspace_root.display().to_string()),
            Vec::new(),
            vec![Message::User {
                content: UserMessageContent::Text("u1".to_string()),
            }],
        )
        .await
        .expect("create");
    store
        .record_message(
            &mut state,
            Message::User {
                content: UserMessageContent::Text("u2".to_string()),
            },
        )
        .await
        .expect("user2");
    let user2_id = state.meta.last_seq;

    let session_id = state.id().to_string();
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{session_id}/runs"),
            serde_json::json!({ "input": { "type": "text", "text": "hi" } }),
        ))
        .await
        .expect("oneshot");
    let (status, _body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{session_id}/runs"),
            serde_json::json!({
                "input": {
                    "type": "edit_user_message",
                    "user_message_id": user2_id,
                    "content": "u2 edited",
                }
            }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_error_code(&body, "conflict");
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
    assert_eq!(
        body.get("state").and_then(|v| v.as_str()),
        Some("cancelled")
    );

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
        items
            .iter()
            .any(|ev| ev.get("type").and_then(|v| v.as_str()) == Some("run_cancelled")),
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

    let store = FileSessionStore::project(dir.path());
    let id = SessionId::parse(&session_id).expect("session id");
    let session = store.load(&id).await.expect("load session");
    assert_eq!(
        session.meta.agent.as_str(),
        "plan",
        "meta should persist agent"
    );

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
async fn patch_settings_model_stays_session_local() {
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
            serde_json::json!({ "model_id": "test/new-model" }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("model_id"))
            .and_then(|v| v.as_str()),
        Some("test/new-model")
    );

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/config/providers"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("default_model").and_then(|v| v.as_str()),
        Some("test/test-model")
    );

    let resp = app
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("model_id"))
            .and_then(|v| v.as_str()),
        Some("test/test-model")
    );
}

#[tokio::test]
async fn save_session_defaults_updates_model_and_mcp_defaults() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let yaml = r#"
default_model: test/test-model
providers:
  test:
    base_url: http://127.0.0.1:1
    models:
      - test-model
      - new-model
mcp:
  servers:
    - name: demo
      enable: false
      command: true
      args: []
"#;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PUT,
            "/v1/config",
            serde_json::json!({ "yaml": yaml }),
        ))
        .await
        .expect("oneshot");
    let (status, _body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);

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
            serde_json::json!({
                "agent": "plan",
                "model_id": "test/new-model",
                "mcp": { "servers": [{ "id": "demo", "enable": true }] }
            }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("model_id"))
            .and_then(|v| v.as_str()),
        Some("test/new-model")
    );
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("mcp"))
            .and_then(|m| m.get("servers"))
            .and_then(|servers| servers.as_array())
            .and_then(|servers| servers.first())
            .and_then(|server| server.get("enable"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::POST,
            &format!("/v1/sessions/{session_id}/settings/save-defaults"),
            serde_json::json!({ "model": true, "agent": true, "mcp": true }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/config"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("config")
            .and_then(|cfg| cfg.get("default_model"))
            .and_then(|v| v.as_str()),
        Some("test/new-model")
    );
    assert_eq!(
        body.get("config")
            .and_then(|cfg| cfg.get("default_agent"))
            .and_then(|v| v.as_str()),
        Some("plan")
    );
    assert_eq!(
        body.get("config")
            .and_then(|cfg| cfg.get("mcp"))
            .and_then(|mcp| mcp.get("servers"))
            .and_then(|servers| servers.as_array())
            .and_then(|servers| servers.first())
            .and_then(|server| server.get("enable"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    let resp = app
        .clone()
        .oneshot(req_empty(Method::POST, "/v1/sessions"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("agent"))
            .and_then(|v| v.as_str()),
        Some("plan")
    );
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("model_id"))
            .and_then(|v| v.as_str()),
        Some("test/new-model")
    );
    assert_eq!(
        body.get("settings")
            .and_then(|s| s.get("mcp"))
            .and_then(|m| m.get("servers"))
            .and_then(|servers| servers.as_array())
            .and_then(|servers| servers.first())
            .and_then(|server| server.get("enable"))
            .and_then(|v| v.as_bool()),
        Some(true)
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
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_error_code(&body, "unauthorized");

    // Query token is only used for web handshake.
    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/?token=secret"))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FOUND);
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        set_cookie.contains("kiliax_token=secret"),
        "set-cookie missing token: {set_cookie}"
    );

    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/capabilities")
        .header(header::COOKIE, "kiliax_token=secret")
        .body(Body::empty())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("oneshot");
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

    let ws_path = std::path::Path::new(ws);
    let allowed_kiliax_dir = dir.path().join(".kiliax");
    assert!(
        ws_path.starts_with(&allowed_kiliax_dir),
        "workspace_root not under test .kiliax dir: {ws}"
    );
    assert!(
        ws_path.starts_with(allowed_kiliax_dir.join("workspace")),
        "workspace_root not under test .kiliax/workspace: {ws}"
    );

    let dir_name = ws_path.file_name().and_then(|v| v.to_str()).unwrap_or("");
    assert!(
        dir_name.starts_with("tmp_"),
        "workspace_root dir must start with tmp_: {ws}"
    );
    let id = dir_name.strip_prefix("tmp_").unwrap_or("");
    assert!(
        SessionId::parse(id).is_ok(),
        "workspace_root suffix must be a valid SessionId: {ws}"
    );
    assert!(
        id.len() >= 9
            && id.chars().take(8).all(|c| c.is_ascii_digit())
            && id.chars().nth(8) == Some('T'),
        "workspace_root suffix must include YYYYMMDDT prefix: {ws}"
    );

    let updated_at = body
        .get("updated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");
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

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let workspace_root = dir
        .path()
        .join(format!("kiliax_test_workspace_{ts}_{}", std::process::id()));
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
        items
            .iter()
            .any(|it| it.get("id").and_then(|v| v.as_str()) == Some("demo_skill")),
        "missing demo_skill: {items:?}"
    );

    let _ = tokio::fs::remove_dir_all(&workspace_root).await;
}

#[tokio::test]
async fn list_global_skills_returns_workspace_skills() {
    let dir = TempDir::new().expect("tempdir");
    let skill_dir = dir.path().join("skills").join("demo_skill");
    tokio::fs::create_dir_all(&skill_dir)
        .await
        .expect("create skill dir");
    tokio::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: Demo Skill\ndescription: Hello\n---\n# Demo\n",
    )
    .await
    .expect("write SKILL.md");

    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/skills"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.get("items").and_then(|v| v.as_array()).unwrap();
    assert!(
        items
            .iter()
            .any(|it| it.get("id").and_then(|v| v.as_str()) == Some("demo_skill")),
        "missing demo_skill: {items:?}"
    );
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
        .oneshot(req_empty(
            Method::GET,
            &format!("/v1/sessions/{session_id}"),
        ))
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
async fn put_config_falls_back_and_persists_default_model() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let new_yaml = r#"
default_model: missing/model
providers:
  test:
    base_url: http://127.0.0.1:1
    models:
      - test-model
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
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("config")
            .and_then(|cfg| cfg.get("default_model"))
            .and_then(|v| v.as_str()),
        Some("test/test-model")
    );
    assert!(
        body.get("yaml")
            .and_then(|v| v.as_str())
            .is_some_and(|yaml| yaml.contains("default_model: test/test-model")),
        "normalized yaml not returned: {body}"
    );

    let saved = tokio::fs::read_to_string(dir.path().join("kiliax.yaml"))
        .await
        .expect("read saved config");
    assert!(saved.contains("default_model: test/test-model"));
}

#[tokio::test]
async fn patch_config_mcp_updates_enable_flag() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let yaml = r#"
default_model: test/test-model
providers:
  test:
    base_url: http://127.0.0.1:1
    models:
      - test-model
mcp:
  servers:
    - name: demo
      enable: false
      command: true
      args: []
"#;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PUT,
            "/v1/config",
            serde_json::json!({ "yaml": yaml }),
        ))
        .await
        .expect("oneshot");
    let (status, _body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PATCH,
            "/v1/config/mcp",
            serde_json::json!({ "servers": [{ "id": "demo", "enable": true }] }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/config"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("config")
            .and_then(|c| c.get("mcp"))
            .and_then(|m| m.get("servers"))
            .and_then(|s| s.as_array())
            .and_then(|s| s.first())
            .and_then(|s| s.get("enable"))
            .and_then(|v| v.as_bool()),
        Some(true),
        "config mcp not updated: {body}"
    );
}

#[tokio::test]
async fn patch_config_providers_sets_api_key_without_echo() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PATCH,
            "/v1/config/providers",
            serde_json::json!({
                "upsert": [{ "id": "test", "api_key": "sk-test" }]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/config/providers"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);

    let providers = body
        .get("providers")
        .and_then(|v| v.as_array())
        .expect("providers array");
    let test_provider = providers
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some("test"))
        .expect("missing provider test");
    assert_eq!(
        test_provider.get("api").and_then(|v| v.as_str()),
        Some("openai_chat_completions")
    );
    assert_eq!(
        test_provider.get("api_key_set").and_then(|v| v.as_bool()),
        Some(true),
        "expected api_key_set: true, got: {body}"
    );
    assert!(
        test_provider.get("api_key").is_none(),
        "api_key must not be returned: {body}"
    );
}

#[tokio::test]
async fn patch_config_providers_accepts_legacy_kind_alias() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PATCH,
            "/v1/config/providers",
            serde_json::json!({
                "upsert": [{
                    "id": "anthropic",
                    "kind": "anthropic",
                    "base_url": "https://api.anthropic.com/v1",
                    "models": ["claude-test"]
                }]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/config/providers"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let providers = body
        .get("providers")
        .and_then(|v| v.as_array())
        .expect("providers array");
    let provider = providers
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some("anthropic"))
        .expect("missing provider anthropic");
    assert_eq!(
        provider.get("api").and_then(|v| v.as_str()),
        Some("anthropic_messages")
    );
}

#[tokio::test]
async fn patch_config_providers_accepts_openai_responses_api() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PATCH,
            "/v1/config/providers",
            serde_json::json!({
                "upsert": [{
                    "id": "openai",
                    "api": "openai_responses",
                    "base_url": "https://api.openai.com/v1",
                    "models": ["gpt-4.1-mini"]
                }]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/config/providers"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    let providers = body
        .get("providers")
        .and_then(|v| v.as_array())
        .expect("providers array");
    let provider = providers
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some("openai"))
        .expect("missing provider openai");
    assert_eq!(
        provider.get("api").and_then(|v| v.as_str()),
        Some("openai_responses")
    );
}

#[tokio::test]
async fn patch_config_runtime_updates_max_steps() {
    let dir = TempDir::new().expect("tempdir");
    let app = build_test_app(&dir, None).await;

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PATCH,
            "/v1/config/runtime",
            serde_json::json!({
                "runtime_max_steps": 8,
                "agents_plan_max_steps": 16,
                "agents_general_max_steps": null
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(req_empty(Method::GET, "/v1/config/runtime"))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("runtime_max_steps").and_then(|v| v.as_u64()),
        Some(8)
    );
    assert_eq!(
        body.get("agents_plan_max_steps").and_then(|v| v.as_u64()),
        Some(16)
    );
    assert_eq!(
        body.get("agents_general_max_steps")
            .and_then(|v| v.as_u64()),
        None
    );

    let resp = app
        .clone()
        .oneshot(req_json(
            Method::PATCH,
            "/v1/config/runtime",
            serde_json::json!({ "runtime_max_steps": 0 }),
        ))
        .await
        .expect("oneshot");
    let (status, body) = read_json(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_error_code(&body, "invalid_argument");
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
    tokio::fs::write(&meta_path, meta_text)
        .await
        .expect("meta write");

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
    tokio::fs::write(&meta_path, meta_text)
        .await
        .expect("meta write");

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
