use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use kiliax_core::session::SessionId;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::http::headers::idem_key;
use crate::state::ServerState;

pub(in crate::http) fn create_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(create_run)
}

pub(in crate::http) fn get_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_run)
}

pub(in crate::http) fn cancel_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(cancel_run)
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
    let out = state
        .create_run(&id, idem_key(&headers), req.into())
        .await?;
    let out: crate::api::Run = out.into();
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
    let out: crate::api::Run = out.into();
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
    let out: crate::api::Run = out.into();
    Ok(Json(out))
}
