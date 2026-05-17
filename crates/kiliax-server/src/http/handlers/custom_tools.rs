use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use kiliax_core::session::SessionId;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::state::ServerState;

pub(in crate::http) fn session_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(list_custom_tools)
}

pub(in crate::http) fn global_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(list_global_custom_tools)
}

#[utoipa::path(
    get,
    path = "/sessions/{session_id}/custom-tools",
    tags = ["Custom Tools"],
    params(
        ("session_id" = String, Path, description = "Session id.")
    ),
    responses(
        (status = 200, body = crate::api::CustomToolListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn list_custom_tools(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id =
        SessionId::parse(&session_id).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
    let out = state.list_custom_tools(&id).await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/custom-tools",
    tags = ["Custom Tools"],
    responses(
        (status = 200, body = crate::api::CustomToolListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn list_global_custom_tools(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.list_global_custom_tools().await?;
    Ok(Json(out))
}
