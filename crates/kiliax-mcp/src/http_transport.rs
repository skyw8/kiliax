use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;

use crate::{error_response, handle_message, IncomingOutcome, KiliaxHttpClient, McpServerOptions};

#[derive(Debug, Clone)]
pub struct HttpServerOptions {
    pub upstream: McpServerOptions,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub auth_token: Option<String>,
    pub allowed_origins: Vec<String>,
}

pub async fn serve_http(options: HttpServerOptions) -> Result<()> {
    let path = normalize_path(&options.path)?;
    let listener = tokio::net::TcpListener::bind((options.host.as_str(), options.port))
        .await
        .with_context(|| {
            format!(
                "failed to bind MCP HTTP server at {}:{}",
                options.host, options.port
            )
        })?;
    let local_addr = listener.local_addr().context("failed to read local addr")?;
    let state = Arc::new(HttpState {
        client: KiliaxHttpClient::new(options.upstream)?,
        auth_token: options.auth_token.filter(|v| !v.trim().is_empty()),
        allowed_origins: options.allowed_origins,
    });

    let app = Router::new()
        .route(&path, post(post_mcp).get(get_mcp).delete(delete_mcp))
        .with_state(state);

    eprintln!("kiliax MCP HTTP listening on http://{local_addr}{path}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug)]
struct HttpState {
    client: KiliaxHttpClient,
    auth_token: Option<String>,
    allowed_origins: Vec<String>,
}

async fn post_mcp(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(resp) = request_validation_error(&state, &headers, true) {
        return resp;
    }

    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(err) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                error_response(Value::Null, -32700, err.to_string()),
            )
        }
    };

    match handle_message(&state.client, value).await {
        IncomingOutcome::Accepted => StatusCode::ACCEPTED.into_response(),
        IncomingOutcome::Response(response) => json_response(StatusCode::OK, response),
    }
}

async fn get_mcp(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Response {
    if let Some(resp) = request_validation_error(&state, &headers, false) {
        return resp;
    }
    method_not_allowed()
}

async fn delete_mcp(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Response {
    if let Some(resp) = request_validation_error(&state, &headers, false) {
        return resp;
    }
    method_not_allowed()
}

fn request_validation_error(
    state: &HttpState,
    headers: &HeaderMap,
    validate_accept: bool,
) -> Option<Response> {
    if let Some(token) = state.auth_token.as_deref() {
        let ok = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| value == format!("Bearer {token}"));
        if !ok {
            return Some(json_error(
                StatusCode::UNAUTHORIZED,
                -32001,
                "missing or invalid bearer token",
            ));
        }
    }

    if let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = headers.get(HOST).and_then(|v| v.to_str().ok());
        if !origin_allowed(origin, host, &state.allowed_origins) {
            return Some(json_error(
                StatusCode::FORBIDDEN,
                -32002,
                "origin is not allowed",
            ));
        }
    }

    if validate_accept {
        let accept = headers.get(ACCEPT).and_then(|v| v.to_str().ok());
        if !accept.is_some_and(|value| {
            accepts(value, "application/json") && accepts(value, "text/event-stream")
        }) {
            return Some(json_error(
                StatusCode::NOT_ACCEPTABLE,
                -32003,
                "Accept must include application/json and text/event-stream",
            ));
        }
    }

    None
}

fn origin_allowed(origin: &str, host: Option<&str>, allowed_origins: &[String]) -> bool {
    if allowed_origins.iter().any(|allowed| allowed == origin) {
        return true;
    }
    let origin = origin.to_ascii_lowercase();
    if origin.starts_with("http://localhost")
        || origin.starts_with("https://localhost")
        || origin.starts_with("http://127.0.0.1")
        || origin.starts_with("https://127.0.0.1")
        || origin.starts_with("http://[::1]")
        || origin.starts_with("https://[::1]")
    {
        return true;
    }
    let Some(host) = host.map(|v| v.to_ascii_lowercase()) else {
        return false;
    };
    origin == format!("http://{host}") || origin == format!("https://{host}")
}

fn accepts(header: &str, mime: &str) -> bool {
    header
        .split(',')
        .map(|part| part.split(';').next().unwrap_or("").trim())
        .any(|part| part == mime || part == "*/*")
}

fn method_not_allowed() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert("allow", HeaderValue::from_static("POST"));
    response
}

fn json_error(status: StatusCode, code: i64, message: &str) -> Response {
    json_response(
        status,
        error_response(Value::Null, code, message.to_string()),
    )
}

fn json_response<T: serde::Serialize>(status: StatusCode, value: T) -> Response {
    let mut response = (status, Json(value)).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn normalize_path(path: &str) -> Result<String> {
    let path = path.trim();
    if path.is_empty() {
        anyhow::bail!("MCP HTTP path must not be empty");
    }
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    if path.contains('?') || path.contains('#') {
        anyhow::bail!("MCP HTTP path must not contain query or fragment");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_validation_rejects_nonlocal_origins_by_default() {
        assert!(origin_allowed("http://localhost:3000", None, &[]));
        assert!(origin_allowed("http://127.0.0.1:3000", None, &[]));
        assert!(!origin_allowed("https://evil.example", None, &[]));
        assert!(origin_allowed(
            "https://trusted.example",
            None,
            &["https://trusted.example".to_string()]
        ));
    }

    #[test]
    fn accept_validation_matches_mcp_http_requirement() {
        assert!(accepts(
            "application/json, text/event-stream",
            "application/json"
        ));
        assert!(accepts(
            "application/json, text/event-stream",
            "text/event-stream"
        ));
        assert!(!accepts("application/json", "text/event-stream"));
    }

    #[test]
    fn request_validation_checks_auth_origin_and_accept_headers() {
        let state = HttpState {
            client: KiliaxHttpClient::new(McpServerOptions {
                base_url: "http://127.0.0.1:1".to_string(),
                token: None,
            })
            .unwrap(),
            auth_token: Some("secret".to_string()),
            allowed_origins: vec!["https://trusted.example".to_string()],
        };

        let headers = HeaderMap::new();
        let err = request_validation_error(&state, &headers, true).unwrap();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        headers.insert(ORIGIN, HeaderValue::from_static("https://trusted.example"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let err = request_validation_error(&state, &headers, true).unwrap();
        assert_eq!(err.status(), StatusCode::NOT_ACCEPTABLE);

        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        assert!(request_validation_error(&state, &headers, true).is_none());
    }
}
