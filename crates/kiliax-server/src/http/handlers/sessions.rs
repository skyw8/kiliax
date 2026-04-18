use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use kiliax_core::session::SessionId;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::http::headers::idem_key;
use crate::state::ServerState;

pub(in crate::http) fn collection_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(create_session, list_sessions)
}

pub(in crate::http) fn item_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_session, delete_session)
}

pub(in crate::http) fn fork_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(fork_session)
}

pub(in crate::http) fn open_workspace_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(open_workspace)
}

pub(in crate::http) fn settings_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(patch_settings)
}

pub(in crate::http) fn save_defaults_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(save_session_defaults)
}

pub(in crate::http) fn messages_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_messages)
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
    let out = state.create_session(idem_key(&headers), req.into()).await?;
    let out: crate::api::Session = out.into();
    Ok((StatusCode::CREATED, Json(out)))
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
    let out: crate::api::SessionListResponse = out.into();
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
    let out: crate::api::Session = out.into();
    Ok(Json(out))
}

#[derive(serde::Deserialize)]
struct DeleteSessionQuery {
    #[serde(default)]
    delete_workspace_root: Option<bool>,
}

#[utoipa::path(
    delete,
    path = "/sessions/{session_id}",
    tags = ["Sessions"],
    params(
        ("session_id" = String, Path, description = "Session id."),
        ("delete_workspace_root" = Option<bool>, Query, description = "If true, also delete the tmp workspace directory when no sessions remain.")
    ),
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn delete_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
    Query(q): Query<DeleteSessionQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    state
        .delete_session(&id, q.delete_workspace_root.unwrap_or(false))
        .await?;
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
    let out = state.fork_session(&id, req.into()).await?;
    let out: crate::api::ForkSessionResponse = out.into();
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
    let out = state.patch_session_settings(&id, patch.into()).await?;
    let out: crate::api::Session = out.into();
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
    state.save_session_defaults(&id, req.into()).await?;
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
    let out: crate::api::MessageListResponse = out.into();
    Ok(Json(out))
}
