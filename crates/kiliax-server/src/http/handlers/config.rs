use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use utoipa_axum::router::UtoipaMethodRouter;

use crate::error::ApiError;
use crate::state::ServerState;

pub(in crate::http) fn config_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_config, put_config)
}

pub(in crate::http) fn mcp_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(patch_config_mcp)
}

pub(in crate::http) fn providers_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_config_providers, patch_config_providers)
}

pub(in crate::http) fn runtime_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_config_runtime, patch_config_runtime)
}

pub(in crate::http) fn skills_routes() -> UtoipaMethodRouter<Arc<ServerState>> {
    utoipa_axum::routes!(get_config_skills, patch_config_skills)
}

#[utoipa::path(
    get,
    path = "/config",
    tags = ["Config"],
    responses(
        (status = 200, body = crate::api::ConfigResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_config(State(state): State<Arc<ServerState>>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config().await?))
}

#[utoipa::path(
    put,
    path = "/config",
    tags = ["Config"],
    request_body = crate::api::ConfigUpdateRequest,
    responses(
        (status = 200, body = crate::api::ConfigResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn put_config(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigUpdateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.update_config(req).await?))
}

#[utoipa::path(
    patch,
    path = "/config/mcp",
    tags = ["Config"],
    request_body = crate::api::ConfigMcpPatchRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_config_mcp(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigMcpPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_mcp(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/config/providers",
    tags = ["Config"],
    responses(
        (status = 200, body = crate::api::ConfigProvidersResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_config_providers(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_providers().await?))
}

#[utoipa::path(
    patch,
    path = "/config/providers",
    tags = ["Config"],
    request_body = crate::api::ConfigProvidersPatchRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_config_providers(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigProvidersPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_providers(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/config/runtime",
    tags = ["Config"],
    responses(
        (status = 200, body = crate::api::ConfigRuntimeResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_config_runtime(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_runtime().await?))
}

#[utoipa::path(
    patch,
    path = "/config/runtime",
    tags = ["Config"],
    request_body = crate::api::ConfigRuntimePatchRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_config_runtime(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigRuntimePatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_runtime(req).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/config/skills",
    tags = ["Config"],
    responses(
        (status = 200, body = crate::api::ConfigSkillsResponse),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn get_config_skills(
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.get_config_skills().await?))
}

#[utoipa::path(
    patch,
    path = "/config/skills",
    tags = ["Config"],
    request_body = crate::api::ConfigSkillsPatchRequest,
    responses(
        (status = 204, description = "No Content"),
        (status = "default", body = crate::error::ApiErrorResponse)
    )
)]
async fn patch_config_skills(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<crate::api::ConfigSkillsPatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state.patch_config_skills(req).await?;
    Ok(StatusCode::NO_CONTENT)
}
