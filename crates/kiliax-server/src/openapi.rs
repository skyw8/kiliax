use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{self};
use utoipa::{Modify, OpenApi};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Kiliax Session Control API",
        version = env!("CARGO_PKG_VERSION"),
        description = r#"
Network API for controlling Kiliax sessions.

Notes:
- `kiliax.yaml` provides defaults for *new sessions* only.
- API/CLI can override settings at session-level (persistent) and per-run (ephemeral).
- Streaming output is delivered as events (SSE endpoint is specified here; WebSocket is documented in descriptions).
"#
    ),
    tags(
        (name = "Sessions"),
        (name = "Runs"),
        (name = "Capabilities"),
        (name = "Events"),
        (name = "Config"),
        (name = "FS"),
        (name = "Skills"),
        (name = "Admin")
    ),
    modifiers(&ServerAddon, &SecurityAddon)
)]
pub struct ApiDoc;

struct ServerAddon;

impl Modify for ServerAddon {
    fn modify(&self, openapi: &mut openapi::OpenApi) {
        openapi.servers = Some(vec![openapi::server::ServerBuilder::new()
            .url("http://127.0.0.1:8123")
            .description(Some("Local-only default"))
            .build()]);
    }
}

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(openapi::Components::new);
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("token")
                    .build(),
            ),
        );

        openapi.security = Some(vec![
            SecurityRequirement::new("bearerAuth", std::iter::empty::<String>()),
            SecurityRequirement::default(),
        ]);
    }
}
