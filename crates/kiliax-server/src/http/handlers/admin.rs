use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::state::ServerState;

pub(in crate::http) fn info_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_admin_info)
}

pub(in crate::http) fn stop_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(stop_server)
}

#[utoipa::path(
    get,
    path = "/admin/info",
    tags = ["Admin"],
    responses(
        (status = 200, body = crate::api::AdminInfo),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_admin_info(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(crate::api::AdminInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspace_root: state.workspace_root.display().to_string(),
        config_path: state.config_path.display().to_string(),
    }))
}

#[utoipa::path(
    post,
    path = "/admin/stop",
    tags = ["Admin"],
    responses(
        (status = 200, description = "OK"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn stop_server(State(state): State<Arc<ServerState>>) -> Result<impl IntoResponse, ApiError> {
    state.shutdown.notify_waiters();
    Ok(StatusCode::OK)
}
