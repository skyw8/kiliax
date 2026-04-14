use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;

use crate::state::ServerState;

#[derive(Debug, Clone, Default)]
pub struct ServerRunOptions {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub workspace_root: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub token: Option<String>,
}

pub async fn run_server(opts: ServerRunOptions) -> anyhow::Result<()> {
    let workspace_root = opts
        .workspace_root
        .unwrap_or(std::env::current_dir().context("current_dir")?);

    let loaded = if let Some(path) = opts.config_path.clone() {
        kiliax_core::config::load_from_path(path)?
    } else {
        kiliax_core::config::load()?
    };
    let config_path = opts.config_path.unwrap_or(loaded.path.clone());

    let host = opts
        .host
        .or_else(|| loaded.config.server.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = opts.port.or(loaded.config.server.port).unwrap_or(8123);
    let token = opts.token.or_else(|| loaded.config.server.token.clone());
    let token = token
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "kiliax server requires a token (set server.token in kiliax.yaml or pass --token)"
            )
        })?;

    let _otel = kiliax_otel::init(
        &loaded.config,
        "kiliax-server",
        env!("CARGO_PKG_VERSION"),
        kiliax_otel::LocalLogs::Stdout,
    )?;

    let state = Arc::new(
        ServerState::new(
            workspace_root.clone(),
            config_path.clone(),
            loaded.config.clone(),
            Some(token),
        )
        .await?,
    );

    let shutdown = state.shutdown.clone();
    let app = crate::build_app(state);

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    tracing::info!("kiliax server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.notified().await;
    })
    .await?;

    Ok(())
}
