use std::collections::BTreeMap;
use std::io::{self, Write};

use kiliax_core::{
    config,
    llm::{ChatRequest, LlmClient, Message, ToolCallDelta},
};
use tokio_stream::StreamExt;

#[derive(Debug, Default)]
struct ToolCallBuf {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn merge_tool_call_delta(buf: &mut ToolCallBuf, delta: ToolCallDelta) {
    if let Some(id) = delta.id {
        buf.id = Some(id);
    }
    if let Some(name) = delta.name {
        buf.name = Some(name);
    }
    if let Some(args) = delta.arguments {
        buf.arguments.push_str(&args);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let loaded = config::load()?;
    let llm = LlmClient::from_config(&loaded.config, None)?;

    eprintln!("Config: {}", loaded.path.display());
    eprintln!(
        "Route: {}/{} ({})",
        llm.route().provider,
        llm.route().model,
        llm.route().base_url
    );

    let prompt = {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.is_empty() {
            "Write a short answer about why streaming is useful.".to_string()
        } else {
            args.join(" ")
        }
    };

    let mut req = ChatRequest::new(vec![Message::User { content: prompt }]);
    req.temperature = Some(0.2);
    req.max_completion_tokens = Some(512);

    let mut stream = llm.chat_stream(req).await?;
    let mut stdout = io::stdout();

    let mut tool_calls: BTreeMap<u32, ToolCallBuf> = BTreeMap::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;

        if let Some(text) = chunk.content_delta {
            write!(stdout, "{text}")?;
            stdout.flush()?;
        }

        for tc in chunk.tool_calls {
            let entry = tool_calls.entry(tc.index).or_default();
            merge_tool_call_delta(entry, tc);
        }
    }

    writeln!(stdout)?;

    if !tool_calls.is_empty() {
        eprintln!("Tool calls:");
        for (idx, tc) in tool_calls {
            eprintln!(
                "- index={idx} id={:?} name={:?} arguments={}",
                tc.id, tc.name, tc.arguments
            );
        }
    }

    Ok(())
}

