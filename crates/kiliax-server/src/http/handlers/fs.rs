use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::state::ServerState;

pub(in crate::http) fn routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(fs_pick)
}

#[utoipa::path(
    post,
    path = "/fs/pick",
    tags = ["FS"],
    request_body = crate::api::FsPickRequest,
    responses(
        (status = 200, body = crate::api::FsPickResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn fs_pick(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::FsPickRequest>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.fs_pick(req).await?))
}
