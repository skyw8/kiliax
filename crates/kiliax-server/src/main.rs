use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use kiliax_server::state::ServerState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut workspace_root: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut token: Option<String> = None;

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--host" => {
                if let Some(v) = iter.next() {
                    host = Some(v.to_string());
                }
            }
            "--port" => {
                if let Some(v) = iter.next() {
                    port = v.parse().ok();
                }
            }
            "--workspace-root" => {
                if let Some(v) = iter.next() {
                    workspace_root = Some(PathBuf::from(v));
                }
            }
            "--config" => {
                if let Some(v) = iter.next() {
                    config_path = Some(PathBuf::from(v));
                }
            }
            "--token" => {
                if let Some(v) = iter.next() {
                    token = Some(v.to_string());
                }
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            _ => {}
        }
    }

    let workspace_root = workspace_root.unwrap_or(std::env::current_dir()?);
    let loaded = if let Some(path) = config_path.clone() {
        kiliax_core::config::load_from_path(path)?
    } else {
        kiliax_core::config::load()?
    };
    let config_path = config_path.unwrap_or(loaded.path.clone());

    let host = host
        .or_else(|| loaded.config.server.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = port.or(loaded.config.server.port).unwrap_or(8123);
    let token = token.or_else(|| loaded.config.server.token.clone());
    let token = token
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "kiliax-server requires a token (set server.token in kiliax.yaml or pass --token)"
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
    let app = kiliax_server::build_app(state);

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    tracing::info!("kiliax-server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
        })
        .await?;
    Ok(())
}

fn print_help() {
    println!("kiliax-server");
    println!("  --host <ip>             (default: 127.0.0.1)");
    println!("  --port <port>           (default: 8123)");
    println!("  --workspace-root <dir>  (default: cwd)");
    println!("  --config <path>         (default: auto-detect kiliax.yaml)");
    println!("  --token <token>         (required bearer/web auth)");
}

