use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::extract::{ConnectInfo, MatchedPath, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::response::sse::Event as SseEvent;
use axum::response::{Html, IntoResponse, Response, Sse};
use axum::routing::get;
use axum::{Extension, Json, Router};
use futures_util::stream::{self, StreamExt as _};
use kiliax_core::session::SessionId;
use tokio::sync::broadcast;
use tower::ServiceExt as _;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::Span;
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_swagger_ui::SwaggerUi;

use crate::error::{ApiError, ApiErrorCode};
use crate::state::ServerState;

async fn access_log_middleware(req: axum::extract::Request, next: middleware::Next) -> Response {
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

pub fn build_app(state: Arc<ServerState>) -> Router {
    use utoipa::OpenApi as _;

    let v1 = OpenApiRouter::<Arc<ServerState>>::default()
        .routes(routes!(create_session, list_sessions))
        .routes(routes!(get_config, put_config))
        .routes(routes!(patch_config_mcp))
        .routes(routes!(get_config_providers, patch_config_providers))
        .routes(routes!(get_config_runtime, patch_config_runtime))
        .routes(routes!(get_config_skills, patch_config_skills))
        .routes(routes!(fs_list))
        .routes(routes!(list_global_skills))
        .routes(routes!(get_session, delete_session))
        .routes(routes!(fork_session))
        .routes(routes!(open_workspace))
        .routes(routes!(patch_settings))
        .routes(routes!(save_session_defaults))
        .routes(routes!(get_messages))
        .routes(routes!(list_skills))
        .routes(routes!(create_run))
        .routes(routes!(get_run))
        .routes(routes!(cancel_run))
        .routes(routes!(get_capabilities))
        .routes(routes!(get_admin_info))
        .routes(routes!(stop_server))
        .routes(routes!(list_events))
        .routes(routes!(stream_events_sse))
        .routes(routes!(stream_events_ws))
        .route_layer(http_trace_layer());

    let (v1_router, v1_openapi) = v1.split_for_parts();
    let openapi = crate::openapi::ApiDoc::openapi().nest("/v1", v1_openapi);
    let openapi = Arc::new(openapi);

    let swagger: Router<Arc<ServerState>> = SwaggerUi::new("/docs")
        .url("/v1/openapi.json", (*openapi).clone())
        .into();

    let openapi_yaml: Router<Arc<ServerState>> = Router::new()
        .route("/v1/openapi.yaml", get(get_openapi_yaml))
        .layer(Extension(openapi.clone()));

    let app: Router<Arc<ServerState>> = Router::new()
        .nest("/v1", v1_router)
        .merge(swagger)
        .merge(openapi_yaml)
        .fallback(serve_web);

    app.with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, auth_middleware))
        .layer(middleware::from_fn(access_log_middleware))
}

async fn get_openapi_yaml(
    Extension(openapi): Extension<Arc<utoipa::openapi::OpenApi>>,
) -> impl IntoResponse {
    match openapi.to_yaml() {
        Ok(yaml) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/yaml")],
            yaml,
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to render openapi",
        )
            .into_response(),
    }
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
        .on_response(
            |response: &axum::http::Response<axum::body::Body>,
             latency: std::time::Duration,
             span: &Span| {
                span.record("http.status_code", response.status().as_u16() as u64);
                span.record("http.latency_ms", latency.as_millis() as u64);
            },
        )
}

async fn serve_web(State(state): State<Arc<ServerState>>, req: axum::extract::Request) -> Response {
    if !matches!(req.method(), &Method::GET | &Method::HEAD) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Some(dist_dir) = find_web_dist_dir(&state.workspace_root) {
        let index = dist_dir.join("index.html");

        let path = req.uri().path().to_string();
        let svc = ServeDir::new(dist_dir).fallback(ServeFile::new(index));
        return match svc.oneshot(req).await {
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
        };
    }

    if crate::web::embedded::ENABLED {
        return serve_web_embedded(req.uri().path());
    }

    {
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
    <h2>kiliax-web is not available</h2>
    <p>Build the frontend first (dev mode):</p>
    <pre>cd web
bun install
bun run build</pre>
    <p>Then restart the server and refresh.</p>
  </body>
</html>
"#;
        let mut resp = Html(hint).into_response();
        resp.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
        resp
    }
}

fn serve_web_embedded(path: &str) -> Response {
    let (bytes, served_path) = if path == "/" || path.is_empty() {
        (crate::web::embedded::index_html(), "/index.html")
    } else if let Some(bytes) = crate::web::embedded::get(path) {
        (bytes, path)
    } else {
        (crate::web::embedded::index_html(), "/index.html")
    };

    let cache_control = if served_path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else if served_path.ends_with(".html") {
        "no-cache"
    } else {
        "public, max-age=3600"
    };

    let content_type = content_type_for_path(served_path);
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        .header(axum::http::header::CACHE_CONTROL, cache_control)
        .body(axum::body::Body::from(axum::body::Bytes::from_static(
            bytes,
        )))
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}

fn content_type_for_path(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else if path.ends_with(".webp") {
        "image/webp"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else if path.ends_with(".map") {
        "application/json; charset=utf-8"
    } else {
        "application/octet-stream"
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
    let cookie_token_ok = cookie_token(req.headers()).as_deref() == Some(expected);

    if is_api {
        if !bearer_ok && !cookie_token_ok {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                ApiErrorCode::Unauthorized,
                "unauthorized",
            ));
        }
        return Ok(next.run(req).await);
    }

    // Web UI: use cookie auth, with a one-time `?token=` → Set-Cookie + redirect handshake.
    if cookie_token_ok {
        return Ok(next.run(req).await);
    }

    let query_token_ok = token_from_query(req.uri()).as_deref() == Some(expected);
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
    <p>Start the server with <code>kiliax server start</code> and open the printed URL:</p>
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

#[utoipa::path(
    post,
    path = "/sessions",
    tags = ["Sessions"],
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "Client-provided idempotency key for safe retries.")
    ),
    request_body = Option<crate::api::SessionCreateRequest>,
    responses(
        (status = 201, body = crate::api::Session),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn create_session(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    payload: Option<Json<crate::api::SessionCreateRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let req = payload
        .map(|v| v.0)
        .unwrap_or(crate::api::SessionCreateRequest {
            title: None,
            settings: None,
        });
    let out = state.create_session(idem_key(&headers), req).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

#[utoipa::path(
    get,
    path = "/config",
    tags = ["Config"],
    responses(
        (status = 200, body = crate::api::ConfigResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_config(State(state): State<Arc<ServerState>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config().await?))
}

#[utoipa::path(
    put,
    path = "/config",
    tags = ["Config"],
    request_body = crate::api::ConfigUpdateRequest,
    responses(
        (status = 200, body = crate::api::ConfigResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn put_config(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigUpdateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.update_config(req).await?))
}

#[utoipa::path(
    patch,
    path = "/config/mcp",
    tags = ["Config"],
    request_body = crate::api::ConfigMcpPatchRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_config_mcp(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigMcpPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_mcp(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/config/providers",
    tags = ["Config"],
    responses(
        (status = 200, body = crate::api::ConfigProvidersResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_config_providers(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_providers().await?))
}

#[utoipa::path(
    patch,
    path = "/config/providers",
    tags = ["Config"],
    request_body = crate::api::ConfigProvidersPatchRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_config_providers(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigProvidersPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_providers(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/config/runtime",
    tags = ["Config"],
    responses(
        (status = 200, body = crate::api::ConfigRuntimeResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_config_runtime(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_runtime().await?))
}

#[utoipa::path(
    patch,
    path = "/config/runtime",
    tags = ["Config"],
    request_body = crate::api::ConfigRuntimePatchRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_config_runtime(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigRuntimePatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_runtime(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/config/skills",
    tags = ["Config"],
    responses(
        (status = 200, body = crate::api::ConfigSkillsResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_config_skills(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_skills().await?))
}

#[utoipa::path(
    patch,
    path = "/config/skills",
    tags = ["Config"],
    request_body = crate::api::ConfigSkillsPatchRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_config_skills(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigSkillsPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_skills(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct FsListQuery {
    #[serde(default)]
    path: Option<String>,
}

#[utoipa::path(
    get,
    path = "/fs/list",
    tags = ["FS"],
    params(
        ("path" = Option<String>, Query, description = "Path to list (defaults to workspace root).")
    ),
    responses(
        (status = 200, body = crate::api::FsListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn fs_list(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<FsListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.fs_list(q.path).await?))
}

#[utoipa::path(
    get,
    path = "/sessions",
    tags = ["Sessions"],
    params(
        ("live" = Option<bool>, Query, description = "If true, only return live sessions."),
        ("limit" = Option<usize>, Query, description = "Max number of items to return."),
        ("cursor" = Option<String>, Query, description = "Opaque cursor returned by a previous list call.")
    ),
    responses(
        (status = 200, body = crate::api::SessionListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn list_sessions(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<ListSessionsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let live_only = q.live.unwrap_or(false);
    let limit = q.limit.unwrap_or(50);
    let out = state.list_sessions(live_only, limit, q.cursor).await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}",
    tags = ["Sessions"],
    params(
        ("session_id" = String, Path, description = "Session id.")
    ),
    responses(
        (status = 200, body = crate::api::Session),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.get_session(&id).await?;
    Ok(Json(out))
}

#[utoipa::path(
    delete,
    path = "/sessions/{session_id}",
    tags = ["Sessions"],
    params(
        ("session_id" = String, Path, description = "Session id.")
    ),
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn delete_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    state.delete_session(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/sessions/{session_id}/fork",
    tags = ["Sessions"],
    params(
        ("session_id" = String, Path, description = "Session id.")
    ),
    request_body = crate::api::ForkSessionRequest,
    responses(
        (status = 201, body = crate::api::ForkSessionResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn fork_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(req): Json<crate::api::ForkSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.fork_session(&id, req).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

#[utoipa::path(
    post,
    path = "/sessions/{session_id}/open",
    tags = ["Sessions"],
    params(
        ("session_id" = String, Path, description = "Session id.")
    ),
    request_body = crate::api::OpenWorkspaceRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn open_workspace(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(req): Json<crate::api::OpenWorkspaceRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    state.open_workspace(&id, req.target).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    patch,
    path = "/sessions/{session_id}/settings",
    tags = ["Sessions"],
    params(
        ("session_id" = String, Path, description = "Session id.")
    ),
    request_body = crate::api::SessionSettingsPatch,
    responses(
        (status = 200, body = crate::api::Session),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_settings(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    if body.get("workspace_root").is_some() {
        return Err(ApiError::invalid_argument(
            "workspace_root is immutable (set it when creating the session)",
        ));
    }
    let patch: crate::api::SessionSettingsPatch =
        serde_json::from_value(body).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.patch_session_settings(&id, patch).await?;
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/sessions/{session_id}/settings/save-defaults",
    tags = ["Sessions"],
    params(
        ("session_id" = String, Path, description = "Session id.")
    ),
    request_body = crate::api::SessionSaveDefaultsRequest,
    responses(
        (status = 204),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn save_session_defaults(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(req): Json<crate::api::SessionSaveDefaultsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    state.save_session_defaults(&id, req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct MessagesQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    before: Option<String>,
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/messages",
    tags = ["Sessions"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("limit" = Option<usize>, Query, description = "Max number of items to return."),
        ("before" = Option<String>, Query, description = "Return messages before this message id (exclusive).")
    ),
    responses(
        (status = 200, body = crate::api::MessageListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_messages(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<MessagesQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let limit = q.limit.unwrap_or(50);
    let out = state.get_messages(&id, limit, q.before).await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/skills",
    tags = ["Skills"],
    params(
        ("session_id" = String, Path, description = "Session id.")
    ),
    responses(
        (status = 200, body = crate::api::SkillListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn list_skills(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.list_skills(&id).await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/skills",
    tags = ["Skills"],
    responses(
        (status = 200, body = crate::api::SkillListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn list_global_skills(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.list_global_skills().await?;
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/sessions/{session_id}/runs",
    tags = ["Runs"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("Idempotency-Key" = Option<String>, Header, description = "Client-provided idempotency key for safe retries.")
    ),
    request_body = crate::api::RunCreateRequest,
    responses(
        (status = 201, body = crate::api::Run),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn create_run(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let req: crate::api::RunCreateRequest =
        serde_json::from_value(body).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.create_run(&id, idem_key(&headers), req).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

#[utoipa::path(
    get,
    path = "/runs/{run_id}",
    tags = ["Runs"],
    params(
        ("run_id" = String, Path, description = "Run id.")
    ),
    responses(
        (status = 200, body = crate::api::Run),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_run(
    State(state): State<Arc<ServerState>>,
    Path(run_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.get_run(&run_id).await?;
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/runs/{run_id}/cancel",
    tags = ["Runs"],
    params(
        ("run_id" = String, Path, description = "Run id.")
    ),
    responses(
        (status = 200, body = crate::api::Run),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn cancel_run(
    State(state): State<Arc<ServerState>>,
    Path(run_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.cancel_run(&run_id).await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/capabilities",
    tags = ["Capabilities"],
    responses(
        (status = 200, body = crate::api::Capabilities),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_capabilities(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.get_capabilities().await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/admin/info",
    tags = ["Admin"],
    responses(
        (status = 200, body = crate::api::AdminInfo),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_admin_info(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(crate::api::AdminInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspace_root: state.workspace_root.display().to_string(),
        config_path: state.config_path.display().to_string(),
    }))
}

#[utoipa::path(
    post,
    path = "/admin/stop",
    tags = ["Admin"],
    responses(
        (status = 200, description = "OK"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn stop_server(State(state): State<Arc<ServerState>>) -> Result<impl IntoResponse, ApiError> {
    state.shutdown.notify_waiters();
    Ok(StatusCode::OK)
}

#[derive(serde::Deserialize)]
struct EventsQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    after: Option<u64>,
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/events",
    tags = ["Events"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("limit" = Option<usize>, Query, description = "Max number of items to return."),
        ("after" = Option<u64>, Query, description = "Return events after this event id (exclusive).")
    ),
    responses(
        (status = 200, body = crate::api::EventListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn list_events(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let limit = q.limit.unwrap_or(50);
    let out = state.list_events(&id, limit, q.after).await?;
    Ok(Json(out))
}

#[derive(serde::Deserialize)]
struct StreamQuery {
    #[serde(default)]
    after_event_id: Option<u64>,
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/events/stream",
    tags = ["Events"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("Last-Event-ID" = Option<String>, Header, description = "Resume from last event id (exclusive)."),
        ("after_event_id" = Option<u64>, Query, description = "Start streaming after this event id (exclusive).")
    ),
    responses(
        (status = 200, content_type = "text/event-stream", body = String),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn stream_events_sse(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let after = headers
        .get("Last-Event-ID")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .or(q.after_event_id)
        .unwrap_or(0);

    let live = state.ensure_live(&id).await?;
    let backlog = state.events_backlog_after(&id, after, usize::MAX).await?;
    let rx = live.subscribe_events();
    let shutdown = state.shutdown.clone();

    let backlog_stream = stream::iter(
        backlog
            .into_iter()
            .map(|e| Ok::<_, std::convert::Infallible>(e)),
    );
    let live_stream = stream::unfold(
        LiveSseState {
            rx,
            live,
            session_id: id.to_string(),
            shutdown,
            done: false,
        },
        |mut st| async move {
            if st.done {
                return None;
            }
            tokio::select! {
                _ = st.shutdown.notified() => None,
                received = st.rx.recv() => match received {
                    Ok(ev) => Some((Ok::<_, std::convert::Infallible>(ev), st)),
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        let last_event_id = st.live.last_event_id().await;
                        let ev = events_lagged_event(&st.session_id, missed, last_event_id);
                        st.done = true;
                        Some((Ok::<_, std::convert::Infallible>(ev), st))
                    }
                    Err(_) => None,
                }
            }
        },
    );

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

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/events/ws",
    tags = ["Events"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("after_event_id" = Option<u64>, Query, description = "Start streaming after this event id (exclusive).")
    ),
    responses(
        (status = 101, description = "Switching Protocols"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn stream_events_ws(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<StreamQuery>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let after = q.after_event_id.unwrap_or(0);
    let live = state.ensure_live(&id).await?;
    let backlog = state.events_backlog_after(&id, after, usize::MAX).await?;
    let mut rx = live.subscribe_events();
    let session_id = id.to_string();
    let shutdown = state.shutdown.clone();

    Ok(ws.on_upgrade(move |mut socket| async move {
        for ev in backlog {
            if let Ok(text) = serde_json::to_string(&ev) {
                let _ = socket
                    .send(axum::extract::ws::Message::Text(text.into()))
                    .await;
            }
        }
        loop {
            tokio::select! {
                _ = shutdown.notified() => break,
                received = rx.recv() => match received {
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
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        let last_event_id = live.last_event_id().await;
                        let ev = events_lagged_event(&session_id, missed, last_event_id);
                        if let Ok(text) = serde_json::to_string(&ev) {
                            let _ = socket
                                .send(axum::extract::ws::Message::Text(text.into()))
                                .await;
                        }
                        break;
                    }
                    Err(_) => break,
                }
            };
        }
    }))
}

struct LiveSseState {
    rx: broadcast::Receiver<crate::api::Event>,
    live: Arc<crate::state::LiveSession>,
    session_id: String,
    shutdown: Arc<tokio::sync::Notify>,
    done: bool,
}

fn events_lagged_event(session_id: &str, missed: u64, last_event_id: u64) -> crate::api::Event {
    crate::api::Event {
        event_id: last_event_id,
        ts: now_rfc3339(),
        session_id: session_id.to_string(),
        run_id: None,
        event_type: "events_lagged".to_string(),
        data: serde_json::json!({
            "missed": missed,
            "last_event_id": last_event_id,
        }),
    }
}

fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
