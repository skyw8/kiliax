mod daemon;
mod server_run_args;

use anyhow::{Context, Result};
use kiliax_core::{config, session::FileSessionStore, session::SessionId};

const EXAMPLE_CONFIG_YAML: &str = include_str!("../../../kiliax.example.yaml");

fn print_help() {
    let bin = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");

    println!("{bin} {version}");
    println!();
    println!("Usage:");
    println!("  {bin}");
    println!("  {bin} server start");
    println!("  {bin} server run [OPTIONS]");
    println!("  {bin} server stop");
    println!("  {bin} server restart");
    println!("  {bin} mcp serve [--transport stdio|http] [--base-url URL] [--token TOKEN]");
    println!("  {bin} mcp skill install [--path DIR] [--force]");
    println!("  {bin} goal get --session <SESSION_ID>");
    println!("  {bin} goal set --session <SESSION_ID> <OBJECTIVE...>");
    println!("  {bin} goal clear --session <SESSION_ID>");
    println!();
    println!("Options:");
    println!("  -h, --help               Show help");
    println!("  -V, --version            Show version");
    println!();
    println!("Config search order:");
    println!("  1) ~/.kiliax/kiliax.yaml");
    println!();
    println!("If no config is found, {bin} will write a template and continue.");
}

async fn handle_goal_command(args: &[String]) -> Result<()> {
    let store = FileSessionStore::global()
        .context("failed to determine home directory for sessions (expected ~/.kiliax/sessions)")?;
    let Some(action) = args.first().map(String::as_str) else {
        anyhow::bail!("goal expects one of: get, set, clear");
    };
    let mut session_id: Option<SessionId> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut iter = args[1..].iter();
    while let Some(arg) = iter.next() {
        if arg == "--session" {
            let Some(id) = iter.next() else {
                anyhow::bail!("--session expects a session id");
            };
            session_id = Some(SessionId::parse(id)?);
        } else {
            rest.push(arg.clone());
        }
    }
    let Some(session_id) = session_id else {
        anyhow::bail!("--session <SESSION_ID> is required");
    };
    let mut session = store.load(&session_id).await?;
    match action {
        "get" => {
            println!("{}", serde_json::to_string_pretty(&session.meta.goal)?);
        }
        "set" => {
            let objective = rest.join(" ");
            if objective.trim().is_empty() {
                anyhow::bail!("goal set expects an objective");
            }
            let goal = store.set_goal(&mut session, objective).await?;
            store.checkpoint(&mut session).await?;
            println!("{}", serde_json::to_string_pretty(&goal)?);
        }
        "clear" => {
            store.clear_goal(&mut session).await?;
            store.checkpoint(&mut session).await?;
            println!("goal cleared");
        }
        other => anyhow::bail!("unknown goal command: {other}"),
    }
    Ok(())
}

fn print_version() {
    let bin = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");
    println!("{bin} {version}");
}

fn write_default_config(paths: &[std::path::PathBuf]) -> Result<std::path::PathBuf> {
    let Some(target) = (if paths.len() >= 3 {
        paths.last()
    } else {
        paths.first()
    }) else {
        anyhow::bail!("no config candidate paths available");
    };

    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create config dir: {}", dir.display()))?;
    }

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .with_context(|| format!("failed to create config file: {}", target.display()))?;
    f.write_all(EXAMPLE_CONFIG_YAML.as_bytes())
        .with_context(|| format!("failed to write config file: {}", target.display()))?;
    Ok(target.clone())
}

fn load_or_init_config() -> Result<config::LoadedConfig> {
    match config::load() {
        Ok(loaded) => Ok(loaded),
        Err(config::ConfigError::NotFound { paths }) => {
            let path = write_default_config(&paths)?;
            // Silent first-run experience: create a template config and continue.
            Ok(config::load_from_path(path)?)
        }
        Err(err) => Err(err.into()),
    }
}

fn is_wsl() -> bool {
    if std::env::consts::OS != "linux" {
        return false;
    }
    std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some()
}

fn try_open_browser(url: &str) {
    use std::process::{Command, Stdio};

    let spawn = |program: &str, args: &[&str]| -> std::io::Result<()> {
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
    };

    let _ = if is_wsl() {
        // Best-effort: open in the Windows default browser.
        spawn("wslview", &[url]).or_else(|_| spawn("cmd.exe", &["/c", "start", "", url]))
    } else if std::env::consts::OS == "windows" {
        spawn("cmd.exe", &["/c", "start", "", url])
    } else if std::env::consts::OS == "macos" {
        spawn("open", &[url])
    } else {
        spawn("xdg-open", &[url])
    };
}

async fn ensure_server_and_open(workspace_root: &std::path::Path) -> Result<()> {
    let loaded = load_or_init_config()?;
    let state = daemon::ensure_running(workspace_root, &loaded.path, &loaded.config.server).await?;
    let url = format!(
        "http://{}:{}/?token={}",
        state.host, state.port, state.token
    );
    println!("{url}");
    try_open_browser(&url);
    Ok(())
}

async fn ensure_server(workspace_root: &std::path::Path) -> Result<daemon::DaemonState> {
    let loaded = load_or_init_config()?;
    daemon::ensure_running(workspace_root, &loaded.path, &loaded.config.server).await
}

struct McpServeArgs {
    transport: McpTransport,
    base_url: Option<String>,
    token: Option<String>,
    host: String,
    port: u16,
    path: String,
    mcp_token: Option<String>,
    allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpTransport {
    Stdio,
    Http,
}

fn parse_mcp_serve_args(args: &[String]) -> Result<McpServeArgs> {
    let mut transport = McpTransport::Stdio;
    let mut base_url = std::env::var("KILIAX_BASE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let mut token = std::env::var("KILIAX_TOKEN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let mut host = std::env::var("KILIAX_MCP_HOST")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let mut port = std::env::var("KILIAX_MCP_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(8124);
    let mut path = std::env::var("KILIAX_MCP_PATH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/mcp".to_string());
    let mut mcp_token = std::env::var("KILIAX_MCP_TOKEN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let mut allowed_origins = Vec::new();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--transport" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--transport expects stdio or http");
                };
                transport = match value.as_str() {
                    "stdio" => McpTransport::Stdio,
                    "http" => McpTransport::Http,
                    other => anyhow::bail!("unsupported MCP transport: {other}"),
                };
            }
            "--base-url" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--base-url expects a URL");
                };
                let value = value.trim();
                if value.is_empty() {
                    anyhow::bail!("--base-url must not be empty");
                }
                base_url = Some(value.to_string());
            }
            "--token" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--token expects a token");
                };
                token = Some(value.trim().to_string());
            }
            "--host" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--host expects a bind host");
                };
                host = value.trim().to_string();
                if host.is_empty() {
                    anyhow::bail!("--host must not be empty");
                }
            }
            "--port" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--port expects a port");
                };
                port = value
                    .parse::<u16>()
                    .with_context(|| format!("invalid --port value: {value}"))?;
            }
            "--path" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--path expects an endpoint path");
                };
                path = value.trim().to_string();
            }
            "--mcp-token" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--mcp-token expects a token");
                };
                mcp_token = Some(value.trim().to_string());
            }
            "--allow-origin" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--allow-origin expects an origin");
                };
                let value = value.trim();
                if !value.is_empty() {
                    allowed_origins.push(value.to_string());
                }
            }
            other => anyhow::bail!("unknown mcp serve option: {other}"),
        }
    }

    if transport == McpTransport::Http && !is_loopback_bind_host(&host) && mcp_token.is_none() {
        anyhow::bail!("HTTP MCP on non-loopback host requires --mcp-token or KILIAX_MCP_TOKEN");
    }

    Ok(McpServeArgs {
        transport,
        base_url,
        token,
        host,
        port,
        path,
        mcp_token,
        allowed_origins,
    })
}

fn is_loopback_bind_host(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

struct McpSkillInstallArgs {
    path: std::path::PathBuf,
    force: bool,
}

fn parse_mcp_skill_install_args(args: &[String]) -> Result<McpSkillInstallArgs> {
    let mut path: Option<std::path::PathBuf> = None;
    let mut force = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--path" => {
                let Some(value) = iter.next() else {
                    anyhow::bail!("--path expects a directory");
                };
                path = Some(std::path::PathBuf::from(value));
            }
            "--force" => force = true,
            other => anyhow::bail!("unknown mcp skill install option: {other}"),
        }
    }
    let path = path.unwrap_or_else(default_kiliax_skills_root);
    Ok(McpSkillInstallArgs { path, force })
}

fn default_kiliax_skills_root() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".kiliax")
        .join("skills")
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        print_version();
        return Ok(());
    }

    let workspace_root = std::env::current_dir()?;

    if args.is_empty() {
        return ensure_server_and_open(&workspace_root).await;
    }

    if args.first().is_some_and(|a| a == "server") {
        match args.get(1).map(String::as_str) {
            Some("start") => {
                ensure_server_and_open(&workspace_root).await?;
            }
            Some("run") => {
                if args.iter().any(|a| a == "-h" || a == "--help") {
                    server_run_args::print_run_help();
                    return Ok(());
                }
                let opts = server_run_args::parse_run_args(&args[2..]);
                kiliax_server::runner::run_server(opts).await?;
            }
            Some("stop") => match daemon::stop().await? {
                daemon::StopOutcome::NotRunning => {
                    println!("kiliax server not running (no ~/.kiliax/server.json)");
                }
                daemon::StopOutcome::Stopped => {
                    println!("kiliax server stopped");
                }
                daemon::StopOutcome::NotReachable => {
                    println!("kiliax server not reachable (removed stale ~/.kiliax/server.json)");
                }
            },
            Some("restart") => {
                let _ = daemon::stop().await?;
                ensure_server_and_open(&workspace_root).await?;
            }
            _ => print_help(),
        }
        return Ok(());
    }

    if args.first().is_some_and(|a| a == "mcp") {
        match args.get(1).map(String::as_str) {
            Some("serve") => {
                let serve_args = parse_mcp_serve_args(&args[2..])?;
                let (base_url, token) = if let Some(base_url) = serve_args.base_url {
                    (base_url, serve_args.token)
                } else {
                    let state = ensure_server(&workspace_root).await?;
                    (
                        format!("http://{}:{}", state.host, state.port),
                        Some(state.token),
                    )
                };
                let upstream = kiliax_mcp::McpServerOptions { base_url, token };
                match serve_args.transport {
                    McpTransport::Stdio => {
                        kiliax_mcp::serve_stdio(upstream).await?;
                    }
                    McpTransport::Http => {
                        kiliax_mcp::serve_http(kiliax_mcp::HttpServerOptions {
                            upstream,
                            host: serve_args.host,
                            port: serve_args.port,
                            path: serve_args.path,
                            auth_token: serve_args.mcp_token,
                            allowed_origins: serve_args.allowed_origins,
                        })
                        .await?;
                    }
                }
            }
            Some("skill") => match args.get(2).map(String::as_str) {
                Some("install") => {
                    let install_args = parse_mcp_skill_install_args(&args[3..])?;
                    let dir = kiliax_mcp::install_call_kiliax_skill(
                        &install_args.path,
                        install_args.force,
                    )?;
                    println!("{}", dir.display());
                }
                _ => print_help(),
            },
            _ => print_help(),
        }
        return Ok(());
    }

    if args.first().is_some_and(|a| a == "goal") {
        handle_goal_command(&args[1..]).await?;
        return Ok(());
    }

    if let Some(command) = args.first() {
        anyhow::bail!("unknown command: {command}");
    }

    print_help();
    Ok(())
}
