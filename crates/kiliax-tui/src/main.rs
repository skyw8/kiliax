mod app;
mod clipboard_paste;
mod custom_terminal;
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

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode};
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

#[tokio::main]
async fn main() -> Result<()> {
    let workspace_root = std::env::current_dir()?;
    let store = FileSessionStore::project(&workspace_root);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut profile_override: Option<&str> = None;
    let mut resume_id: Option<SessionId> = None;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "plan" | "general" | "build" => profile_override = Some(arg.as_str()),
            "--resume" => {
                let Some(id) = iter.next() else {
                    anyhow::bail!("--resume expects a session id");
                };
                resume_id = Some(SessionId::parse(id)?);
            }
            _ => {}
        }
    }

    let mut resumed: Option<kiliax_core::session::SessionState> = None;
    if let Some(id) = resume_id.as_ref() {
        resumed = Some(store.load(id).await?);
    }

    let loaded = config::load()?;
    let model_override = resumed.as_ref().and_then(|s| s.meta.model_id.as_deref());
    let llm = LlmClient::from_config(&loaded.config, model_override)?;
    let runtime = kiliax_core::runtime::AgentRuntime::new(
        llm,
        ToolEngine::new(&workspace_root, loaded.config.clone()),
    );

    let profile = profile_override
        .and_then(AgentProfile::from_name)
        .or_else(|| {
            resumed
                .as_ref()
                .and_then(|s| AgentProfile::from_name(&s.meta.agent))
        })
        .unwrap_or_else(AgentProfile::general);

    let (session, messages) = match resumed {
        Some(session) => {
            let messages = session.messages.clone();
            (session, messages)
        }
        None => {
            let mut builder = PromptBuilder::for_agent(&profile)
                .with_tools({
                    let mut tools = profile.tools.clone();
                    tools.extend(runtime.tools().extra_tool_definitions().await);
                    tools
                })
                .with_model_id(runtime.llm().route().model_id())
                .with_workspace_root(&workspace_root);
            if let Ok(skills) = tools::skills::discover_skills(&workspace_root) {
                builder = builder.add_skills(skills);
            }
            let messages = builder.build();
            let session = store
                .create(
                    profile.name.to_string(),
                    Some(runtime.llm().route().model_id()),
                    Some(loaded.path.display().to_string()),
                    Some(workspace_root.display().to_string()),
                    messages.clone(),
                )
                .await?;
            (session, messages)
        }
    };

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

    let session_id = app.session_id().to_string();

    // Clear the viewport so the shell output isn't interleaved with the composer UI.
    terminal.draw(1, |frame| {
        frame.render_widget(ratatui::widgets::Clear, frame.area())
    })?;

    drop(terminal);
    drop(guard);

    // Clear the full screen after restoring terminal modes to avoid leaving stale UI behind.
    execute!(std::io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
    println!("Resume: cargo run -p kiliax-tui -- --resume {session_id}");

    Ok(())
}
