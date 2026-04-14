use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use tower::ServiceExt as _;
use tower_http::services::{ServeDir, ServeFile};

use crate::state::ServerState;

pub(crate) async fn serve_web(
    State(state): State<Arc<ServerState>>,
    req: axum::extract::Request,
) -> Response {
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
