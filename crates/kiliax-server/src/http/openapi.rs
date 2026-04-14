use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;

pub(crate) async fn get_openapi_yaml(
    Extension(openapi): Extension<Arc<utoipa::openapi::OpenApi>>,
) -> impl IntoResponse {
    match openapi.to_yaml() {
        Ok(yaml) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/yaml")],
            yaml,
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to render openapi",
        )
            .into_response(),
    }
}
