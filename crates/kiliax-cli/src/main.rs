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
    println!("If no config is found, {bin} will write a template and exit.");
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

fn load_or_init_config() -> Result<Option<config::LoadedConfig>> {
    match config::load() {
        Ok(loaded) => Ok(Some(loaded)),
        Err(config::ConfigError::NotFound { paths }) => {
            let path = write_default_config(&paths)?;
            let bin = env!("CARGO_PKG_NAME");
            println!("No `kiliax.yaml` found.");
            println!("Created template config at: {}", path.display());
            println!("Edit it, then rerun `{bin}`.");
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
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

    if args.first().is_some_and(|a| a == "server") {
        match args.get(1).map(String::as_str) {
            Some("start") => {
                let Some(loaded) = load_or_init_config()? else {
                    return Ok(());
                };
                let state =
                    daemon::ensure_running(&workspace_root, &loaded.path, &loaded.config.server)
                        .await?;
                println!("kiliax server: {}:{}", state.host, state.port);
                println!(
                    "kiliax-web: http://{}:{}/?token={}",
                    state.host, state.port, state.token
                );
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
                let Some(loaded) = load_or_init_config()? else {
                    return Ok(());
                };
                let state =
                    daemon::ensure_running(&workspace_root, &loaded.path, &loaded.config.server)
                        .await?;
                println!("kiliax server: {}:{}", state.host, state.port);
                println!(
                    "kiliax-web: http://{}:{}/?token={}",
                    state.host, state.port, state.token
                );
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
