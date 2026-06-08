use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use kiliax_core::compact::{context_tokens_for_auto_compact, estimate_context_tokens};
use kiliax_core::history::sanitize_history_for_next_request;
use kiliax_core::prompt::PromptBuilder;
use kiliax_core::protocol::{Message, ToolCall, ToolDefinition, UserMessageContent};
use kiliax_core::session::FileSessionStore;
use tempfile::TempDir;

fn user_message(text: impl Into<String>) -> Message {
    Message::User {
        content: UserMessageContent::text(text),
        hidden: false,
    }
}

fn assistant_message(text: impl Into<String>) -> Message {
    Message::Assistant {
        content: Some(text.into()),
        reasoning_content: None,
        tool_calls: Vec::new(),
        usage: None,
        provider_metadata: None,
    }
}

fn assistant_tool_call(id: impl Into<String>) -> Message {
    let id = id.into();
    Message::Assistant {
        content: None,
        reasoning_content: Some("thinking".to_string()),
        tool_calls: vec![ToolCall {
            id,
            name: "read_file".to_string(),
            arguments: r#"{"filePath":"src/main.rs"}"#.to_string(),
        }],
        usage: None,
        provider_metadata: None,
    }
}

fn tool_result(id: impl Into<String>) -> Message {
    Message::Tool {
        tool_call_id: id.into(),
        content: "tool output\n".repeat(32),
    }
}

fn large_history(pairs: usize) -> Vec<Message> {
    let mut messages = Vec::with_capacity(pairs * 2);
    for i in 0..pairs {
        messages.push(user_message(format!(
            "user message {i}: {}",
            "context ".repeat(64)
        )));
        messages.push(assistant_message(format!(
            "assistant message {i}: {}",
            "answer ".repeat(64)
        )));
    }
    messages
}

fn tool_history(calls: usize) -> Vec<Message> {
    let mut messages = Vec::with_capacity(calls * 3);
    for i in 0..calls {
        let id = format!("call_{i}");
        messages.push(assistant_tool_call(id.clone()));
        messages.push(user_message(format!("interleaved user message {i}")));
        messages.push(tool_result(id));
    }
    messages
}

fn prompt_build(c: &mut Criterion) {
    let messages = large_history(250);
    let tools = vec![
        ToolDefinition {
            name: "read_file".to_string(),
            description: Some("Read a file".to_string()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": { "filePath": { "type": "string" } },
                "required": ["filePath"]
            })),
            strict: Some(true),
        },
        ToolDefinition {
            name: "shell_command".to_string(),
            description: Some("Run a shell command".to_string()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            })),
            strict: Some(true),
        },
    ];

    c.bench_function("prompt_build_large_history", |b| {
        b.iter(|| {
            let out = PromptBuilder::new()
                .with_model_id("test/test-model")
                .with_agent_prompt("agent prompt")
                .with_project_prompt(Some("project prompt".to_string()))
                .include_environment_prompt(false)
                .with_tools(tools.clone())
                .extend_messages(messages.clone())
                .build();
            black_box(out);
        });
    });
}

fn history_sanitize(c: &mut Criterion) {
    let messages = tool_history(200);
    c.bench_function("sanitize_tool_history", |b| {
        b.iter_batched(
            || messages.clone(),
            |mut messages| {
                let report = sanitize_history_for_next_request(&mut messages);
                black_box((messages, report));
            },
            BatchSize::SmallInput,
        );
    });
}

fn compact_token_estimates(c: &mut Criterion) {
    let messages = large_history(500);
    c.bench_function("estimate_context_tokens_large_history", |b| {
        b.iter(|| black_box(estimate_context_tokens(black_box(&messages))));
    });
    c.bench_function("auto_compact_tokens_large_history", |b| {
        b.iter(|| black_box(context_tokens_for_auto_compact(black_box(&messages))));
    });
}

fn session_message_paging(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let dir = TempDir::new().expect("tempdir");
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let mut state = runtime
        .block_on(store.create(
            "general",
            Some("test/test-model".to_string()),
            None,
            Some(dir.path().display().to_string()),
            Vec::new(),
            Vec::new(),
        ))
        .expect("create session");

    runtime.block_on(async {
        for i in 0..1000 {
            store
                .record_message(
                    &mut state,
                    user_message(format!("paged message {i}: {}", "body ".repeat(24))),
                )
                .await
                .expect("record message");
        }
    });
    let id = state.id().clone();

    c.bench_function("session_read_message_page_50_from_1000", |b| {
        b.to_async(&runtime).iter(|| async {
            let page = store
                .read_message_page(black_box(&id), black_box(50), None)
                .await
                .expect("read page");
            black_box(page);
        });
    });
}

criterion_group!(
    benches,
    prompt_build,
    history_sanitize,
    compact_token_estimates,
    session_message_paging
);
criterion_main!(benches);
