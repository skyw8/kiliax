use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware;
use axum::response::Response;

use crate::error::{ApiError, ApiErrorCode};
use crate::state::ServerState;

pub(crate) async fn access_log_middleware(
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

pub(crate) async fn auth_middleware(
    State(state): State<Arc<ServerState>>,
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> Result<Response, ApiError> {
    let Some(expected) = state.token.as_deref() else {
        return Ok(next.run(req).await);
    };

    let path = req.uri().path();
    let is_api = path == "/v1" || path.starts_with("/v1/");
    let is_events_ws = path.starts_with("/v1/sessions/") && path.ends_with("/events/ws");
    let is_openapi_spec = path == "/v1/openapi.json" || path == "/v1/openapi.yaml";

    if !is_api || is_events_ws || is_openapi_spec {
        return Ok(next.run(req).await);
    }

    let bearer = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    if bearer != Some(expected) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            ApiErrorCode::Unauthorized,
            "unauthorized",
        ));
    }
    Ok(next.run(req).await)
}

pub(crate) fn strip_token_query(uri: &axum::http::Uri) -> String {
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

pub(crate) async fn security_headers_middleware(
    req: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .filter(|v| valid_csp_host(v))
        .map(str::to_string);
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    let connect_src = host
        .as_deref()
        .map(|v| format!("'self' ws://{v} wss://{v}"))
        .unwrap_or_else(|| "'self'".to_string());
    let csp = format!(
        "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; font-src 'self' data:; connect-src {connect_src}; \
         object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
    );
    if let Ok(value) = HeaderValue::from_str(&csp) {
        headers.insert(axum::http::header::CONTENT_SECURITY_POLICY, value);
    }
    resp
}

fn valid_csp_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}
