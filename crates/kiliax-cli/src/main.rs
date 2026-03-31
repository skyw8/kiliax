mod app;
mod clipboard_paste;
mod custom_terminal;
mod daemon;
mod header;
mod highlight;
mod history;
mod input;
mod markdown;
mod mcp_picker;
mod model_picker;
mod slash_command;
mod style;
mod terminal;
mod ui;
mod wrap;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use crossterm::{cursor::MoveTo, execute, terminal::Clear, terminal::ClearType};
use futures_util::StreamExt;
use kiliax_core::{
    agents::AgentProfile,
    config,
    llm::LlmClient,
    prompt::PromptBuilder,
    runtime::AgentRuntimeOptions,
    session::{FileSessionStore, SessionId},
    tools::{self, ToolEngine},
};

use crate::app::App;
use crate::app::{AppAction, SubmitDisposition};

const EXAMPLE_CONFIG_YAML: &str = include_str!("../../../kiliax.example.yaml");

fn tui_local_logs() -> kiliax_otel::LocalLogs {
    if let Ok(v) = std::env::var("KILIAX_TUI_LOG_PATH") {
        let path = v.trim();
        if !path.is_empty() {
            return kiliax_otel::LocalLogs::File {
                path: std::path::PathBuf::from(path),
            };
        }
    }

    if let Some(home) = dirs::home_dir() {
        return kiliax_otel::LocalLogs::File {
            path: home.join(".kiliax").join("tui.log"),
        };
    }

    kiliax_otel::LocalLogs::None
}

fn print_help() {
    let bin = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");

    println!("{bin} {version}");
    println!();
    println!("Usage:");
    println!("  {bin} [plan|general] [--resume <SESSION_ID>]");
    println!("  {bin} serve start");
    println!("  {bin} serve stop");
    println!("  {bin} serve restart");
    println!();
    println!("Options:");
    println!("  --resume <SESSION_ID>    Resume a session");
    println!("  -h, --help               Show help");
    println!("  -V, --version            Show version");
    println!();
    println!("Config search order:");
    println!("  1) ~/.kiliax/kiliax.yaml");
    println!();
    println!("If no config is found, {bin} will write a template and exit.");
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

    if args.first().is_some_and(|a| a == "serve") {
        match args.get(1).map(String::as_str) {
            Some("start") => {
                let Some(loaded) = load_or_init_config()? else {
                    return Ok(());
                };
                let state = daemon::ensure_running(
                    &workspace_root,
                    &loaded.path,
                    &loaded.config.server,
                )
                .await?;
                println!("kiliax-server: {}:{}", state.host, state.port);
                println!(
                    "kiliax-web: http://{}:{}/?token={}",
                    state.host, state.port, state.token
                );
            }
            Some("stop") => {
                match daemon::stop().await? {
                    daemon::StopOutcome::NotRunning => {
                        println!("kiliax-server not running (no ~/.kiliax/server.json)");
                    }
                    daemon::StopOutcome::Stopped => {
                        println!("kiliax-server stopped");
                    }
                    daemon::StopOutcome::NotReachable => {
                        println!("kiliax-server not reachable (removed stale ~/.kiliax/server.json)");
                    }
                }
            }
            Some("restart") => {
                let _ = daemon::stop().await?;
                let Some(loaded) = load_or_init_config()? else {
                    return Ok(());
                };
                let state = daemon::ensure_running(
                    &workspace_root,
                    &loaded.path,
                    &loaded.config.server,
                )
                .await?;
                println!("kiliax-server: {}:{}", state.host, state.port);
                println!(
                    "kiliax-web: http://{}:{}/?token={}",
                    state.host, state.port, state.token
                );
            }
            _ => print_help(),
        }
        return Ok(());
    }

    let mut profile_override: Option<&str> = None;
    let mut resume_id: Option<SessionId> = None;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "plan" | "general" => profile_override = Some(arg.as_str()),
            "--resume" => {
                let Some(id) = iter.next() else {
                    anyhow::bail!("--resume expects a session id");
                };
                resume_id = Some(SessionId::parse(id)?);
            }
            other if other.starts_with('-') => anyhow::bail!("unknown option: {other}"),
            other => anyhow::bail!("unknown command: {other}"),
        }
    }

    let Some(loaded) = load_or_init_config()? else {
        return Ok(());
    };

    let _otel = kiliax_otel::init(
        &loaded.config,
        "kiliax-cli",
        env!("CARGO_PKG_VERSION"),
        tui_local_logs(),
    )?;
    tracing::info!(
        event = "tui.start",
        version = env!("CARGO_PKG_VERSION"),
        cwd = %workspace_root.display(),
        config_path = %loaded.path.display(),
        resume = %resume_id.as_ref().map(|v| v.to_string()).unwrap_or_default(),
    );
    let store = FileSessionStore::global()
        .context("failed to determine home directory for sessions (expected ~/.kiliax/sessions)")?;

    let mut resumed: Option<kiliax_core::session::SessionState> = None;
    if let Some(id) = resume_id.as_ref() {
        resumed = Some(store.load(id).await?);
    }

    let tool_engine = ToolEngine::new(&workspace_root, loaded.config.clone());

    let profile = profile_override
        .and_then(AgentProfile::from_name)
        .or_else(|| {
            resumed
                .as_ref()
                .and_then(|s| AgentProfile::from_name(&s.meta.agent))
        })
        .unwrap_or_else(AgentProfile::general);

    let (session, messages, llm) = match resumed {
        Some(session) => {
            let messages = session.messages.clone();
            let llm = LlmClient::from_config(&loaded.config, session.meta.model_id.as_deref())?
                .with_prompt_cache_key(session.meta.prompt_cache_key.clone());
            (session, messages, llm)
        }
        None => {
            let llm = LlmClient::from_config(&loaded.config, None)?;
            let model_id = llm.route().model_id();
            let mut builder = PromptBuilder::for_agent(&profile)
                .with_tools({
                    kiliax_core::tools::policy::tool_definitions_for_agent(
                        &profile,
                        &tool_engine,
                        &model_id,
                    )
                    .await
                })
                .with_model_id(model_id.clone())
                .with_workspace_root(&workspace_root);
            if let Ok(skills) = tools::skills::discover_skills(&workspace_root) {
                builder = builder.add_skills(skills);
            }
            let messages = builder.build();
            let session = store
                .create(
                    profile.name.to_string(),
                    Some(model_id.clone()),
                    Some(loaded.path.display().to_string()),
                    Some(workspace_root.display().to_string()),
                    Vec::new(),
                    messages.clone(),
                )
                .await?;
            let llm = llm.with_prompt_cache_key(session.meta.prompt_cache_key.clone());
            (session, messages, llm)
        }
    };

    let runtime = kiliax_core::runtime::AgentRuntime::new(llm, tool_engine);

    let extra_workspace_roots: Vec<std::path::PathBuf> = session
        .meta
        .extra_workspace_roots
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    runtime
        .tools()
        .set_extra_workspace_roots(extra_workspace_roots.clone())
        .map_err(|e| anyhow::anyhow!(e))?;

    let options = AgentRuntimeOptions::from_config(&profile, &loaded.config);

    let (guard, mut terminal) = terminal::init()?;
    let composer_style = style::composer_background_style();

    let mut app = App::new(
        profile,
        runtime,
        options,
        store,
        session,
        messages,
        workspace_root.clone(),
        extra_workspace_roots,
        loaded.path.clone(),
        loaded.config.clone(),
    );
    terminal.queue_history_lines(header::startup_lines(
        env!("CARGO_PKG_VERSION"),
        app.model_id(),
        &workspace_root,
    ));
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(33));
    let mut agent_stream: Option<
        tokio_stream::wrappers::ReceiverStream<
            Result<kiliax_core::runtime::AgentEvent, kiliax_core::runtime::AgentRuntimeError>,
        >,
    > = None;

    loop {
        if app.should_quit {
            break;
        }

        if agent_stream.is_none()
            && !app.running
            && app.model_picker().is_none()
            && app.mcp_picker().is_none()
        {
            while let Some(queued) = app.pop_next_queued_submission() {
                match app.handle_queued_submission(queued).await? {
                    SubmitDisposition::Handled => {
                        if app.model_picker().is_some() || app.mcp_picker().is_some() {
                            break;
                        }
                    }
                    SubmitDisposition::StartRun => {
                        agent_stream = Some(app.start_run().await?);
                        break;
                    }
                }
            }
        }

        let history_lines = app.drain_history_lines();
        if !history_lines.is_empty() {
            terminal.queue_history_lines(history_lines);
        }

        let width = terminal.screen_size()?.width;
        app.set_screen_width(width);
        let viewport_height = ui::desired_viewport_height(&app, width);
        terminal.draw(viewport_height, |frame| {
            ui::draw(frame, &mut app, composer_style)
        })?;
        if app.should_quit {
            break;
        }

        if agent_stream.is_some() {
            tokio::select! {
                _ = tick.tick() => {}
                maybe_event = events.next() => {
                    let Some(event) = maybe_event else {
                        break;
                    };

                    match event? {
                        Event::Key(key) => {
                            if matches!(key.kind, KeyEventKind::Release) {
                                continue;
                            }
                            if key.code == KeyCode::Esc {
                                app.interrupt_run();
                                agent_stream = None;
                                continue;
                            }
                            let _ = app.handle_key(key);
                        }
                        Event::Paste(text) => app.handle_paste(&text),
                        Event::Resize(_, _) => {}
                        _ => {}
                    }
                }
                maybe_item = async {
                    match agent_stream.as_mut() {
                        Some(stream) => stream.next().await,
                        None => None,
                    }
                } => {
                    let Some(item) = maybe_item else {
                        agent_stream = None;
                        app.running = false;
                        continue;
                    };

                    match item {
                        Ok(event) => {
                            let done = matches!(event, kiliax_core::runtime::AgentEvent::Done(_));
                            app.handle_agent_event(event).await?;
                            if done {
                                agent_stream = None;
                            }
                        }
                        Err(err) => {
                            app.handle_agent_error(err).await?;
                            agent_stream = None;
                        }
                    }
                }
            }
        } else {
            tokio::select! {
                _ = tick.tick() => {}
                maybe_event = events.next() => {
                    let Some(event) = maybe_event else {
                        break;
                    };

                    match event? {
                        Event::Key(key) => {
                            if matches!(key.kind, KeyEventKind::Release) {
                                continue;
                            }
                            match app.handle_key(key) {
                                AppAction::None => {}
                                AppAction::Submitted(text) => {
                                    match app.handle_submit(text).await? {
                                        SubmitDisposition::Handled => {}
                                        SubmitDisposition::StartRun => {
                                            agent_stream = Some(app.start_run().await?);
                                        }
                                    }
                                }
                                AppAction::ModelPicked(model_id) => {
                                    app.apply_model_selection(model_id).await?;
                                }
                                AppAction::McpToggled { server, enable } => {
                                    app.apply_mcp_toggle(server, enable).await?;
                                }
                            }
                        }
                        Event::Paste(text) => app.handle_paste(&text),
                        Event::Resize(_, _) => {}
                        _ => {}
                    }
                }
            }
        }
    }

    let history_lines = app.drain_history_lines();
    if !history_lines.is_empty() {
        terminal.queue_history_lines(history_lines);
        let width = terminal.screen_size()?.width;
        app.set_screen_width(width);
        let viewport_height = ui::desired_viewport_height(&app, width);
        terminal.draw(viewport_height, |frame| {
            ui::draw(frame, &mut app, composer_style)
        })?;
    }

    // Clear the viewport so the shell output isn't interleaved with the composer UI.
    terminal.draw(1, |frame| {
        frame.render_widget(ratatui::widgets::Clear, frame.area())
    })?;

    drop(terminal);
    drop(guard);

    // Clear the full screen after restoring terminal modes to avoid leaving stale UI behind.
    execute!(std::io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
    app.cleanup_empty_session().await;

    Ok(())
}
