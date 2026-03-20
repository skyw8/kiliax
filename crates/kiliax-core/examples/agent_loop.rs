use std::io::{self, Write};

use kiliax_core::{
    agents::AgentProfile,
    config,
    llm::LlmClient,
    prompt::PromptBuilder,
    runtime::{AgentEvent, AgentRuntime, AgentRuntimeOptions},
    tools::{self, ToolEngine},
};
use tokio_stream::StreamExt;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (profile, prompt_args) = match args.first().map(|s| s.as_str()) {
        Some("build") => (AgentProfile::build(), args[1..].to_vec()),
        Some("plan") => (AgentProfile::plan(), args[1..].to_vec()),
        _ => (AgentProfile::plan(), args),
    };

    let prompt = if prompt_args.is_empty() {
        "Use the read tool to read README.md, then summarize how to run examples."
            .to_string()
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

    let tools_engine = ToolEngine::new(&workspace_root);
    let runtime = AgentRuntime::new(llm, tools_engine);

    let mut builder = PromptBuilder::for_agent(&profile).with_workspace_root(&workspace_root);
    if let Ok(skills) = tools::skills::discover_skills(&workspace_root) {
        builder = builder.add_skills(skills);
    }
    let messages = builder.push_user(prompt).build();

    let options = AgentRuntimeOptions {
        max_steps: 8,
        ..Default::default()
    };

    let mut stream = runtime.run_stream(&profile, messages, options).await?;
    let mut stdout = io::stdout();

    while let Some(event) = stream.next().await {
        match event? {
            AgentEvent::AssistantDelta { delta } => {
                write!(stdout, "{delta}")?;
                stdout.flush()?;
            }
            AgentEvent::ToolCall { call } => {
                eprintln!("\n[tool call] {} {}", call.name, call.arguments);
            }
            AgentEvent::ToolResult { message } => {
                if let kiliax_core::llm::Message::Tool {
                    tool_call_id,
                    content,
                } = message
                {
                    eprintln!("[tool result] {tool_call_id}\n{content}");
                }
            }
            AgentEvent::Done(out) => {
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

