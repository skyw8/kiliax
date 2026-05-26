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
