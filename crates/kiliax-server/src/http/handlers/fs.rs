use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::state::ServerState;

pub(in crate::http) fn routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(fs_list)
}

#[derive(serde::Deserialize)]
struct FsListQuery {
    #[serde(default)]
    path: Option<String>,
}

#[utoipa::path(
    get,
    path = "/fs/list",
    tags = ["FS"],
    params(
        ("path" = Option<String>, Query, description = "Path to list (defaults to workspace root).")
    ),
    responses(
        (status = 200, body = crate::api::FsListResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn fs_list(
    State(state): State<Arc<ServerState>>,
    Query(q): Query<FsListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.fs_list(q.path).await?))
}
