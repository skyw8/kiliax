use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::state::ServerState;

pub(in crate::http) fn routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_capabilities)
}

#[utoipa::path(
    get,
    path = "/capabilities",
    tags = ["Capabilities"],
    responses(
        (status = 200, body = crate::api::Capabilities),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_capabilities(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    let out = state.get_capabilities().await?;
    Ok(Json(out))
}
