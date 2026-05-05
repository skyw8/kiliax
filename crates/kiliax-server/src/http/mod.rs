use std::sync::Arc;

mod handlers;
mod headers;
mod mapper;
mod middleware;
mod openapi;
mod web;

use axum::extract::DefaultBodyLimit;
use axum::middleware as axum_middleware;
use axum::routing::get;
use axum::{Extension, Router};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

use crate::state::ServerState;

pub fn build_app(state: Arc<ServerState>) -> Router {
    use utoipa::OpenApi as _;

    let v1 = OpenApiRouter::<Arc<ServerState>>::default()
        .routes(handlers::sessions::collection_routes())
        .routes(handlers::config::config_routes())
        .routes(handlers::config::mcp_routes())
        .routes(handlers::config::providers_routes())
        .routes(handlers::config::runtime_routes())
        .routes(handlers::config::skills_routes())
        .routes(handlers::fs::routes())
        .routes(handlers::skills::global_routes())
        .routes(handlers::sessions::item_routes())
        .routes(handlers::sessions::fork_routes())
        .routes(handlers::sessions::open_workspace_routes())
        .routes(handlers::sessions::settings_routes())
        .routes(handlers::sessions::save_defaults_routes())
        .routes(handlers::sessions::messages_routes())
        .routes(handlers::skills::session_routes())
        .routes(handlers::runs::create_routes())
        .routes(handlers::runs::get_routes())
        .routes(handlers::runs::cancel_routes())
        .routes(handlers::capabilities::routes())
        .routes(handlers::admin::info_routes())
        .routes(handlers::admin::stop_routes())
        .routes(handlers::events::list_routes())
        .routes(handlers::events::stream_sse_routes())
        .routes(handlers::events::stream_ws_routes())
        .route_layer(middleware::http_trace_layer());

    let (v1_router, v1_openapi) = v1.split_for_parts();
    let openapi = crate::openapi::ApiDoc::openapi().nest("/v1", v1_openapi);
    let openapi = Arc::new(openapi);

    let swagger: Router<Arc<ServerState>> = SwaggerUi::new("/docs")
        .url("/v1/openapi.json", (*openapi).clone())
        .into();

    let openapi_yaml: Router<Arc<ServerState>> = Router::new()
        .route("/v1/openapi.yaml", get(openapi::get_openapi_yaml))
        .layer(Extension(openapi.clone()));

    let app: Router<Arc<ServerState>> = Router::new()
        .nest("/v1", v1_router)
        .merge(swagger)
        .merge(openapi_yaml)
        .fallback(web::serve_web);

    app.with_state(state.clone())
        .layer(axum_middleware::from_fn_with_state(
            state,
            middleware::auth_middleware,
        ))
        .layer(DefaultBodyLimit::max(72 * 1024 * 1024))
        .layer(axum_middleware::from_fn(middleware::access_log_middleware))
}
