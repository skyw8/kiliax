mod api;
mod error;
mod state;

#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::extract::{ConnectInfo, MatchedPath, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Html;
use axum::middleware;
use axum::response::sse::Event as SseEvent;
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures_util::stream::{self, StreamExt as _};
use kiliax_core::session::SessionId;
use tokio_stream::wrappers::BroadcastStream;
use tokio::sync::broadcast;
use tower::ServiceExt as _;
use tower_http::trace::TraceLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::Span;

use crate::error::{ApiError, ApiErrorCode};
use crate::state::ServerState;

async fn access_log_middleware(
    req: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let method = req.method().clone();
    let version = req.version();
    let uri = req.uri().clone();
    let remote = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);

    let started = std::time::Instant::now();
    let resp = next.run(req).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let status = resp.status().as_u16();

    let target = strip_token_query(&uri);
    let version = http_version(version);

    if let Some(remote) = remote {
        tracing::info!(
            target: "kiliax_server::access",
            "{remote} - \"{method} {target} {version}\" {status} {latency_ms}ms"
        );
    } else {
        tracing::info!(
            target: "kiliax_server::access",
            "- - \"{method} {target} {version}\" {status} {latency_ms}ms"
        );
    }

    resp
}

fn http_version(version: axum::http::Version) -> &'static str {
    match version {
        axum::http::Version::HTTP_09 => "HTTP/0.9",
        axum::http::Version::HTTP_10 => "HTTP/1.0",
        axum::http::Version::HTTP_11 => "HTTP/1.1",
        axum::http::Version::HTTP_2 => "HTTP/2.0",
        axum::http::Version::HTTP_3 => "HTTP/3.0",
        _ => "HTTP/?",
    }
}

pub(crate) fn build_app(state: Arc<ServerState>) -> Router {
    let api = Router::new()
        .route("/sessions", post(create_session).get(list_sessions))
        .route("/config", get(get_config).put(put_config))
        .route("/config/mcp", patch(patch_config_mcp))
        .route(
            "/config/providers",
            get(get_config_providers).patch(patch_config_providers),
        )
        .route(
            "/config/runtime",
            get(get_config_runtime).patch(patch_config_runtime),
        )
        .route("/config/skills", get(get_config_skills).patch(patch_config_skills))
        .route("/fs/list", get(fs_list))
        .route("/skills", get(list_global_skills))
        .route("/sessions/{session_id}", get(get_session).delete(delete_session))
        .route("/sessions/{session_id}/fork", post(fork_session))
        .route("/sessions/{session_id}/open", post(open_workspace))
        .route("/sessions/{session_id}/settings", patch(patch_settings))
        .route("/sessions/{session_id}/messages", get(get_messages))
        .route("/sessions/{session_id}/skills", get(list_skills))
        .route("/sessions/{session_id}/runs", post(create_run))
        .route("/runs/{run_id}", get(get_run))
        .route("/runs/{run_id}/cancel", post(cancel_run))
        .route("/capabilities", get(get_capabilities))
        .route("/admin/info", get(get_admin_info))
        .route("/admin/stop", post(stop_server))
        .route("/sessions/{session_id}/events", get(list_events))
        .route("/sessions/{session_id}/events/stream", get(stream_events_sse))
        .route("/sessions/{session_id}/events/ws", get(stream_events_ws))
        .route_layer(http_trace_layer());

    Router::new()
        .nest("/v1", api)
        .fallback(serve_web)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, auth_middleware))
        .layer(middleware::from_fn(access_log_middleware))
}

fn http_trace_layer() -> TraceLayer<
    tower_http::trace::HttpMakeClassifier,
    impl tower_http::trace::MakeSpan<axum::body::Body> + Clone,
    (),
    impl tower_http::trace::OnResponse<axum::body::Body> + Clone,
> {
    TraceLayer::new_for_http()
        .make_span_with(|request: &axum::http::Request<axum::body::Body>| {
            let route = request
                .extensions()
                .get::<MatchedPath>()
                .map(|p| p.as_str())
                .unwrap_or_else(|| request.uri().path());
            let user_agent = request
                .headers()
                .get(axum::http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let target = strip_token_query(request.uri());

            let span = tracing::info_span!(
                "http.request",
                otel.kind = "server",
                http.method = %request.method(),
                http.route = %route,
                http.target = %target,
                http.user_agent = %user_agent,
                http.status_code = tracing::field::Empty,
                http.latency_ms = tracing::field::Empty,
            );
            let _ = kiliax_otel::set_parent_from_http_headers(&span, request.headers());
            span
        })
        .on_request(())
        .on_response(|response: &axum::http::Response<axum::body::Body>, latency: std::time::Duration, span: &Span| {
            span.record("http.status_code", response.status().as_u16() as u64);
            span.record("http.latency_ms", latency.as_millis() as u64);
        })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut workspace_root: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut token: Option<String> = None;

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--host" => {
                if let Some(v) = iter.next() {
                    host = Some(v.to_string());
                }
            }
            "--port" => {
                if let Some(v) = iter.next() {
                    port = v.parse().ok();
                }
            }
            "--workspace-root" => {
                if let Some(v) = iter.next() {
                    workspace_root = Some(PathBuf::from(v));
                }
            }
            "--config" => {
                if let Some(v) = iter.next() {
                    config_path = Some(PathBuf::from(v));
                }
            }
            "--token" => {
                if let Some(v) = iter.next() {
                    token = Some(v.to_string());
                }
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            _ => {}
        }
    }

    let workspace_root = workspace_root.unwrap_or(std::env::current_dir()?);
    let loaded = if let Some(path) = config_path.clone() {
        kiliax_core::config::load_from_path(path)?
    } else {
        kiliax_core::config::load()?
    };
    let config_path = config_path.unwrap_or(loaded.path.clone());

    let host = host
        .or_else(|| loaded.config.server.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = port.or(loaded.config.server.port).unwrap_or(8123);
    let token = token.or_else(|| loaded.config.server.token.clone());
    let token = token
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("kiliax-server requires a token (set server.token in kiliax.yaml or pass --token)"))?;

    let _otel = kiliax_otel::init(
        &loaded.config,
        "kiliax-server",
        env!("CARGO_PKG_VERSION"),
        kiliax_otel::LocalLogs::Stdout,
    )?;

    let state = Arc::new(ServerState::new(
        workspace_root.clone(),
        config_path.clone(),
        loaded.config.clone(),
        Some(token),
    ).await?);

    let shutdown = state.shutdown.clone();
    let app = build_app(state);

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    tracing::info!("kiliax-server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
        })
        .await?;
    Ok(())
}

async fn serve_web(
    State(state): State<Arc<ServerState>>,
    req: axum::extract::Request,
) -> Response {
    if !matches!(req.method(), &Method::GET | &Method::HEAD) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Some(dist_dir) = find_web_dist_dir(&state.workspace_root) else {
        let hint = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>kiliax-web</title>
    <style>
      body { font-family: ui-sans-serif, system-ui, sans-serif; padding: 24px; background: #fff; color: #111; }
      code, pre { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
      pre { background: #f6f6f6; padding: 12px; border-radius: 8px; overflow: auto; }
    </style>
  </head>
  <body>
    <h2>kiliax-web is not built</h2>
    <p>Build the frontend first:</p>
    <pre>cd web
bun install
bun run build</pre>
    <p>Then refresh this page.</p>
  </body>
</html>
"#;
        let mut resp = Html(hint).into_response();
        resp.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
        return resp;
    };

    let index = dist_dir.join("index.html");

    let path = req.uri().path().to_string();
    let svc = ServeDir::new(dist_dir).fallback(ServeFile::new(index));
    match svc.oneshot(req).await {
        Ok(resp) => {
            let mut resp = resp.map(axum::body::Body::new).into_response();

            if path.starts_with("/assets/") {
                resp.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                );
            } else if resp
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/html"))
            {
                resp.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    HeaderValue::from_static("no-cache"),
                );
            }

            resp
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn find_web_dist_dir(workspace_root: &FsPath) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.push(workspace_root.join("web").join("dist"));

    for ancestor in workspace_root.ancestors().take(5).skip(1) {
        candidates.push(ancestor.join("web").join("dist"));
    }

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("web").join("dist"));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for ancestor in dir.ancestors().take(8) {
                candidates.push(ancestor.join("web").join("dist"));
            }
        }
    }

    for dir in candidates {
        if dir.join("index.html").is_file() {
            return Some(dir);
        }
    }

    None
}

fn print_help() {
    println!("kiliax-server");
    println!("  --host <ip>             (default: 127.0.0.1)");
    println!("  --port <port>           (default: 8123)");
    println!("  --workspace-root <dir>  (default: cwd)");
    println!("  --config <path>         (default: auto-detect kiliax.yaml)");
    println!("  --token <token>         (required bearer/web auth)");
}

async fn auth_middleware(
    State(state): State<Arc<ServerState>>,
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> Result<Response, ApiError> {
    let Some(expected) = state.token.as_deref() else {
        return Ok(next.run(req).await);
    };

    let path = req.uri().path();
    let is_api = path == "/v1" || path.starts_with("/v1/");

    let auth = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bearer = auth.strip_prefix("Bearer ").unwrap_or("").trim();
    let bearer_ok = bearer == expected;
    let query_token_ok = token_from_query(req.uri()).as_deref() == Some(expected);
    let cookie_token_ok = cookie_token(req.headers()).as_deref() == Some(expected);

    if is_api {
        if !bearer_ok && !query_token_ok && !cookie_token_ok {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                ApiErrorCode::Unauthorized,
                "unauthorized",
            ));
        }
        let mut resp = next.run(req).await;
        if query_token_ok && !cookie_token_ok {
            if let Ok(v) = build_auth_cookie(expected).parse() {
                resp.headers_mut().insert(axum::http::header::SET_COOKIE, v);
            }
        }
        return Ok(resp);
    }

    // Web UI: use cookie auth, with a one-time `?token=` → Set-Cookie + redirect handshake.
    if cookie_token_ok {
        return Ok(next.run(req).await);
    }

    if query_token_ok && matches!(req.method(), &Method::GET | &Method::HEAD) {
        let dest = strip_token_query(req.uri());
        let resp = Response::builder()
            .status(StatusCode::FOUND)
            .header(axum::http::header::LOCATION, dest)
            .header(axum::http::header::SET_COOKIE, build_auth_cookie(expected))
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::FOUND.into_response());
        return Ok(resp);
    }

    let unauthorized = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>kiliax-web</title>
    <style>
      body { font-family: ui-sans-serif, system-ui, sans-serif; padding: 24px; background: #fff; color: #111; }
      code, pre { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
      pre { background: #f6f6f6; padding: 12px; border-radius: 8px; overflow: auto; }
    </style>
  </head>
  <body>
    <h2>Unauthorized</h2>
    <p>This UI requires a token.</p>
    <p>Start the server with <code>kiliax serve start</code> and open the printed URL:</p>
    <pre>http://127.0.0.1:8123/?token=...</pre>
    <p>After the first visit, your browser will store a cookie and you can use <code>/</code>.</p>
  </body>
</html>
"#;
    Ok((StatusCode::UNAUTHORIZED, Html(unauthorized)).into_response())
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        let (k, v) = part.split_once('=')?;
        if k.trim() == "kiliax_token" {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn build_auth_cookie(token: &str) -> String {
    // Intentionally kept simple: same-origin, local UI.
    format!("kiliax_token={token}; Path=/; HttpOnly; SameSite=Strict")
}

fn strip_token_query(uri: &axum::http::Uri) -> String {
    let path = uri.path();
    let Some(query) = uri.query() else {
        return path.to_string();
    };

    let mut kept: Vec<&str> = Vec::new();
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let k = part.split_once('=').map(|(k, _)| k).unwrap_or(part);
        if k == "token" {
            continue;
        }
        kept.push(part);
    }

    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

fn token_from_query(uri: &axum::http::Uri) -> Option<String> {
    let query = uri.query()?;
    for part in query.split('&') {
        let (k, v) = part.split_once('=')?;
        if k == "token" {
            return percent_decode(v);
        }
    }
    None
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = from_hex(bytes[i + 1])?;
                let lo = from_hex(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn idem_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[derive(serde::Deserialize)]
struct ListSessionsQuery {
    #[serde(default)]
    live: Option<bool>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

async fn create_session(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    payload: Option<Json<api::SessionCreateRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let req = payload.map(|v| v.0).unwrap_or(api::SessionCreateRequest {
        title: None,
        settings: None,
    });
    let out = state.create_session(idem_key(&headers), req).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

async fn get_config(State(state): State<Arc<ServerState>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config().await?))
}

async fn put_config(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<api::ConfigUpdateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.update_config(req).await?))
}

async fn patch_config_mcp(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<api::ConfigMcpPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_mcp(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_config_providers(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_providers().await?))
}

async fn patch_config_providers(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<api::ConfigProvidersPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_providers(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_config_runtime(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_runtime().await?))
}

async fn patch_config_runtime(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<api::ConfigRuntimePatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_runtime(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_config_skills(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_skills().await?))
}

async fn patch_config_skills(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<api::ConfigSkillsPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_skills(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct FsListQuery {
    #[serde(default)]
    path: Option<String>,
}

async fn fs_list(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<FsListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.fs_list(q.path).await?))
}

async fn list_sessions(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<ListSessionsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let live_only = q.live.unwrap_or(false);
    let limit = q.limit.unwrap_or(50);
    let out = state.list_sessions(live_only, limit, q.cursor).await?;
    Ok(Json(out))
}

async fn get_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.get_session(&id).await?;
    Ok(Json(out))
}

async fn delete_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    state.delete_session(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn fork_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(req): Json<api::ForkSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.fork_session(&id, req).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

async fn open_workspace(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(req): Json<api::OpenWorkspaceRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    state.open_workspace(&id, req.target).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn patch_settings(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    if body.get("workspace_root").is_some() {
        return Err(ApiError::invalid_argument(
            "workspace_root is immutable (set it when creating the session)",
        ));
    }
    let patch: api::SessionSettingsPatch =
        serde_json::from_value(body).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.patch_session_settings(&id, patch).await?;
    Ok(Json(out))
}

#[derive(serde::Deserialize)]
struct MessagesQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    before: Option<String>,
}

async fn get_messages(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<MessagesQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let limit = q.limit.unwrap_or(50);
    let out = state.get_messages(&id, limit, q.before).await?;
    Ok(Json(out))
}

async fn list_skills(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.list_skills(&id).await?;
    Ok(Json(out))
}

async fn list_global_skills(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.list_global_skills().await?;
    Ok(Json(out))
}

async fn create_run(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let req: api::RunCreateRequest =
        serde_json::from_value(body).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.create_run(&id, idem_key(&headers), req).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

async fn get_run(
    State(state): State<Arc<ServerState>>,
    Path(run_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.get_run(&run_id).await?;
    Ok(Json(out))
}

async fn cancel_run(
    State(state): State<Arc<ServerState>>,
    Path(run_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.cancel_run(&run_id).await?;
    Ok(Json(out))
}

async fn get_capabilities(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.get_capabilities().await?;
    Ok(Json(out))
}

async fn get_admin_info(State(state): State<Arc<ServerState>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(api::AdminInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspace_root: state.workspace_root.display().to_string(),
        config_path: state.config_path.display().to_string(),
    }))
}

async fn stop_server(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    state.shutdown.notify_waiters();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::process::exit(0);
    });
    Ok(StatusCode::OK)
}

#[derive(serde::Deserialize)]
struct EventsQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    after: Option<u64>,
}

async fn list_events(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let limit = q.limit.unwrap_or(50);
    let out = state.list_events(&id, limit, q.after).await?;
    Ok(Json(out))
}

#[derive(serde::Deserialize)]
struct StreamQuery {
    #[serde(default)]
    after_event_id: Option<u64>,
}

async fn stream_events_sse(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let after = headers
        .get("Last-Event-ID")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .or(q.after_event_id)
        .unwrap_or(0);

    let backlog = state.events_backlog_after(&id, after, usize::MAX).await?;
    let rx = state.live_events_stream(&id).await?;

    let backlog_stream = stream::iter(backlog.into_iter().map(|e| Ok::<_, std::convert::Infallible>(e)));
    let live_stream = BroadcastStream::new(rx).filter_map(|item| async move {
        match item {
            Ok(ev) => Some(Ok::<_, std::convert::Infallible>(ev)),
            Err(_) => None,
        }
    });

    let out = backlog_stream.chain(live_stream).map(|item| {
        let ev = item.unwrap();
        let json = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
        Ok::<SseEvent, std::convert::Infallible>(
            SseEvent::default()
                .id(ev.event_id.to_string())
                .event(ev.event_type.clone())
                .data(json),
        )
    });

    Ok(Sse::new(out).keep_alive(axum::response::sse::KeepAlive::default()))
}

async fn stream_events_ws(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<StreamQuery>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let after = q.after_event_id.unwrap_or(0);
    let backlog = state.events_backlog_after(&id, after, usize::MAX).await?;
    let mut rx = state.live_events_stream(&id).await?;

    Ok(ws.on_upgrade(move |mut socket| async move {
        for ev in backlog {
            if let Ok(text) = serde_json::to_string(&ev) {
                let _ = socket
                    .send(axum::extract::ws::Message::Text(text.into()))
                    .await;
            }
        }
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Ok(text) = serde_json::to_string(&ev) {
                        if socket
                            .send(axum::extract::ws::Message::Text(text.into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    }))
}
