mod app;
mod highlight;
mod history;
mod input;
mod markdown;
mod terminal;
mod ui;
mod wrap;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
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
            "plan" | "build" => profile_override = Some(arg.as_str()),
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
    let model_override = resumed
        .as_ref()
        .and_then(|s| s.meta.model_id.as_deref());
    let llm = LlmClient::from_config(&loaded.config, model_override)?;
    let runtime = kiliax_core::runtime::AgentRuntime::new(llm, ToolEngine::new(&workspace_root));

    let profile = match (profile_override, resumed.as_ref().map(|s| s.meta.agent.as_str())) {
        (Some("plan"), _) => AgentProfile::plan(),
        (Some("build"), _) => AgentProfile::build(),
        (None, Some("plan")) => AgentProfile::plan(),
        (None, Some("build")) => AgentProfile::build(),
        _ => AgentProfile::build(),
    };

    let (session, messages) = match resumed {
        Some(session) => {
            let messages = session.messages.clone();
            (session, messages)
        }
        None => {
            let mut builder = PromptBuilder::for_agent(&profile).with_workspace_root(&workspace_root);
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

    let options = AgentRuntimeOptions {
        max_steps: 8,
        ..Default::default()
    };

    let (guard, mut terminal) = terminal::init()?;

    let mut app = App::new(profile, runtime, options, store, session, messages);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(33));
    let mut agent_stream: Option<
        tokio_stream::wrappers::ReceiverStream<
            Result<kiliax_core::runtime::AgentEvent, kiliax_core::runtime::AgentRuntimeError>,
        >,
    > = None;

    loop {
        if app.flush_requested {
            let width = terminal.full_width();
            let lines = app.flush_transcript_to_history(width as usize);
            terminal.queue_history_lines(lines);
            app.flush_requested = false;
        }

        terminal.draw(|frame| ui::draw(frame, &mut app))?;
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
                            if key.modifiers.contains(KeyModifiers::CONTROL)
                                && matches!(key.code, KeyCode::Char('d'))
                            {
                                app.should_quit = true;
                                continue;
                            }
                            if key.code == KeyCode::Esc {
                                app.should_quit = true;
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
                            if key.modifiers.contains(KeyModifiers::CONTROL)
                                && matches!(key.code, KeyCode::Char('d'))
                            {
                                app.should_quit = true;
                                continue;
                            }
                            if key.code == KeyCode::Esc {
                                app.should_quit = true;
                                continue;
                            }
                            if let Some(text) = app.handle_key(key) {
                                app.submit_user_message(text).await?;
                                agent_stream = Some(app.start_run().await?);
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

    if !app.transcript.is_empty() {
        let width = terminal.full_width();
        let lines = app.flush_transcript_to_history(width as usize);
        terminal.queue_history_lines(lines);
        terminal.draw(|_| {})?;
    }

    let session_id = app.session_id().to_string();

    drop(terminal);
    drop(guard);

    execute!(std::io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
    println!("Resume: cargo run -p kiliax-tui -- --resume {session_id}");

    Ok(())
}
