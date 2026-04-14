use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, MatchedPath, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware;
use axum::response::{Html, IntoResponse, Response};
use tower_http::trace::TraceLayer;
use tracing::Span;

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

pub(crate) fn http_trace_layer() -> TraceLayer<
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
