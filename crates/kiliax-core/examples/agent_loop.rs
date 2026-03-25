use std::io::{self, Write};

use kiliax_core::{
    agents::AgentProfile,
    config,
    llm::{LlmClient, Message, UserMessageContent},
    prompt::PromptBuilder,
    runtime::{AgentEvent, AgentRuntime, AgentRuntimeOptions},
    session::{FileSessionStore, SessionId},
    tools::{self, ToolEngine},
};
use tokio_stream::StreamExt;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut resume_id: Option<SessionId> = None;

    let mut idx = 0;
    let profile = match args.get(0).map(|s| s.as_str()) {
        Some("general") | Some("build") => {
            idx = 1;
            AgentProfile::general()
        }
        Some("plan") => {
            idx = 1;
            AgentProfile::plan()
        }
        _ => AgentProfile::plan(),
    };

    while idx < args.len() {
        if args[idx] == "--resume" {
            let Some(id) = args.get(idx + 1) else {
                return Err("--resume requires a session id".into());
            };
            resume_id = Some(SessionId::parse(id)?);
            idx += 2;
            continue;
        }
        break;
    }
    let prompt_args = args[idx..].to_vec();

    let prompt = if prompt_args.is_empty() && resume_id.is_none() {
        "Use the read_file tool to read README.md, then summarize how to run examples.".to_string()
    } else {
        prompt_args.join(" ")
    };

    let loaded = config::load()?;
    let llm = LlmClient::from_config(&loaded.config, None)?;
    let workspace_root = std::env::current_dir()?;

    eprintln!("Config: {}", loaded.path.display());
    eprintln!(
        "Route: {}/{} ({})",
        llm.route().provider,
        llm.route().model,
        llm.route().base_url
    );
    eprintln!("Agent: {}", profile.name);

    let store = FileSessionStore::project(&workspace_root);

    let tools_engine = ToolEngine::new(&workspace_root, loaded.config.clone());
    let runtime = AgentRuntime::new(llm, tools_engine);

    let (messages, mut session) = match resume_id {
        Some(id) => {
            let mut s = store.load(&id).await?;
            if !prompt.trim().is_empty() {
                store
                    .record_message(
                        &mut s,
                        Message::User {
                            content: UserMessageContent::Text(prompt.clone()),
                        },
                    )
                    .await?;
            }
            (s.messages.clone(), s)
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
            let msgs = builder.push_user(prompt).build();

            let s = store
                .create(
                    profile.name.to_string(),
                    Some(runtime.llm().route().model_id()),
                    Some(loaded.path.display().to_string()),
                    Some(workspace_root.display().to_string()),
                    msgs.clone(),
                )
                .await?;
            eprintln!("Session: {}", s.meta.id);
            (msgs, s)
        }
    };

    let options = AgentRuntimeOptions::from_config(&profile, &loaded.config);

    let mut stream = runtime.run_stream(&profile, messages, options).await?;
    let mut stdout = io::stdout();

    while let Some(item) = stream.next().await {
        let event = match item {
            Ok(event) => event,
            Err(err) => {
                let _ = store.record_error(&mut session, err.to_string()).await;
                return Err(err.into());
            }
        };

        match event {
            AgentEvent::AssistantDelta { delta } => {
                write!(stdout, "{delta}")?;
                stdout.flush()?;
            }
            AgentEvent::AssistantMessage { message } => {
                store.record_message(&mut session, message).await?;
            }
            AgentEvent::ToolCall { call } => {
                eprintln!("\n[tool call] {} {}", call.name, call.arguments);
            }
            AgentEvent::ToolResult { message } => {
                store.record_message(&mut session, message.clone()).await?;
                if let kiliax_core::llm::Message::Tool {
                    tool_call_id,
                    content,
                } = message
                {
                    eprintln!("[tool result] {tool_call_id}\n{content}");
                }
            }
            AgentEvent::Done(out) => {
                store
                    .record_finish(
                        &mut session,
                        out.finish_reason.as_ref().map(|r| format!("{r:?}")),
                    )
                    .await?;
                writeln!(stdout)?;
                eprintln!(
                    "Done in {} steps. finish_reason={:?}",
                    out.steps, out.finish_reason
                );
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
