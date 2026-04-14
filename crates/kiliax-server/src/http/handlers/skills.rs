use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use kiliax_core::session::SessionId;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::state::ServerState;

pub(in crate::http) fn session_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(list_skills)
}

pub(in crate::http) fn global_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(list_global_skills)
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
