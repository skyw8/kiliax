mod api;
mod error;
mod state;

#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
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
use tower_http::services::{ServeDir, ServeFile};
use tracing::Level;

use crate::error::{ApiError, ApiErrorCode};
use crate::state::ServerState;

pub(crate) fn build_app(state: Arc<ServerState>) -> Router {
    let api = Router::new()
        .route("/sessions", post(create_session).get(list_sessions))
        .route("/config", get(get_config).put(put_config))
        .route("/sessions/{session_id}", get(get_session))
        .route("/sessions/{session_id}/resume", post(resume_session))
        .route("/sessions/{session_id}/settings", patch(patch_settings))
        .route("/sessions/{session_id}/messages", get(get_messages))
        .route("/sessions/{session_id}/skills", get(list_skills))
        .route("/sessions/{session_id}/runs", post(create_run))
        .route("/runs/{run_id}", get(get_run))
        .route("/runs/{run_id}/cancel", post(cancel_run))
        .route("/capabilities", get(get_capabilities))
        .route("/admin/stop", post(stop_server))
        .route("/sessions/{session_id}/events", get(list_events))
        .route("/sessions/{session_id}/events/stream", get(stream_events_sse))
        .route("/sessions/{session_id}/events/ws", get(stream_events_ws))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    Router::new()
        .nest("/v1", api)
        .fallback(serve_web)
        .with_state(state)
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

    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

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
        .filter(|v| !v.is_empty());

    let state = Arc::new(ServerState::new(
        workspace_root.clone(),
        config_path.clone(),
        loaded.config.clone(),
        token,
    ).await?);

    let shutdown = state.shutdown.clone();
    let app = build_app(state);

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    tracing::info!("kiliax-server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
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

    let dist_dir = state.workspace_root.join("web").join("dist");
    let index = dist_dir.join("index.html");
    if !index.is_file() {
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
npm install
npm run build</pre>
    <p>Then refresh this page.</p>
  </body>
</html>
"#;
        return Html(hint).into_response();
    }

    let svc = ServeDir::new(dist_dir).fallback(ServeFile::new(index));
    match svc.oneshot(req).await {
        Ok(resp) => resp.map(axum::body::Body::new).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn print_help() {
    println!("kiliax-server");
    println!("  --host <ip>             (default: 127.0.0.1)");
    println!("  --port <port>           (default: 8123)");
    println!("  --workspace-root <dir>  (default: cwd)");
    println!("  --config <path>         (default: auto खोज kiliax.yaml)");
    println!("  --token <token>         (optional bearer auth)");
}

async fn auth_middleware(
    State(state): State<Arc<ServerState>>,
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> Result<Response, ApiError> {
    let Some(expected) = state.token.as_deref() else {
        return Ok(next.run(req).await);
    };
    let auth = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bearer = auth.strip_prefix("Bearer ").unwrap_or("").trim();
    let query_token_ok = token_from_query(req.uri())
        .as_deref()
        .is_some_and(|t| t == expected);

    if bearer != expected && !query_token_ok {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            ApiErrorCode::Unauthorized,
            "unauthorized",
        ));
    }
    Ok(next.run(req).await)
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

async fn resume_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.resume_session(&id).await?;
    Ok(Json(out))
}

async fn patch_settings(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Json(patch): Json<api::SessionSettingsPatch>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
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

async fn create_run(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(req): Json<api::RunCreateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let id = SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
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
