use std::collections::{BTreeMap, HashSet};

use tokio_stream::{Stream, StreamExt};

use crate::agents::{AgentKind, AgentProfile};
use async_openai::types::FinishReason;

use crate::llm::{
    ChatRequest, ChatStreamChunk, LlmClient, LlmError, Message, ToolCall, ToolCallDelta, ToolChoice,
};
use crate::tools::{tool_parallelism, ToolEngine, ToolError, ToolParallelism};

#[derive(Debug, thiserror::Error)]
pub enum AgentRuntimeError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error(transparent)]
    Tool(#[from] ToolError),

    #[error("cancelled")]
    Cancelled,

    #[error("max steps exceeded: {max_steps}")]
    MaxSteps { max_steps: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolErrorMode {
    /// Return a tool message with error text (so the model can recover).
    ToolMessage,
    /// Abort the run immediately.
    FailFast,
}

#[derive(Debug, Clone)]
pub struct AgentRuntimeOptions {
    pub max_steps: usize,
    pub tool_choice: ToolChoice,
    pub parallel_tool_calls: Option<bool>,
    pub tool_error_mode: ToolErrorMode,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

impl Default for AgentRuntimeOptions {
    fn default() -> Self {
        Self {
            max_steps: 8,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
            tool_error_mode: ToolErrorMode::ToolMessage,
            temperature: None,
            max_completion_tokens: None,
        }
    }
}

impl AgentRuntimeOptions {
    /// Build runtime options from `kiliax.yaml`.
    ///
    /// Precedence:
    /// 1) `runtime.*` (global defaults)
    /// 2) `agents.<kind>.*` (per-agent overrides)
    pub fn from_config(profile: &AgentProfile, config: &crate::config::Config) -> Self {
        let mut options = Self::default();

        if let Some(max_steps) = config.runtime.max_steps {
            options.max_steps = max_steps;
        }

        let agent_cfg = match profile.kind {
            AgentKind::Plan => &config.agents.plan,
            AgentKind::General => &config.agents.general,
        };
        if let Some(max_steps) = agent_cfg.max_steps {
            options.max_steps = max_steps;
        }

        options
    }
}

#[derive(Debug, Clone)]
pub struct AgentRunOutput {
    pub steps: usize,
    pub messages: Vec<Message>,
    pub finish_reason: Option<FinishReason>,
}

#[derive(Clone)]
pub struct AgentRuntime {
    llm: LlmClient,
    tools: ToolEngine,
}

impl AgentRuntime {
    pub fn new(llm: LlmClient, tools: ToolEngine) -> Self {
        Self { llm, tools }
    }

    pub fn llm(&self) -> &LlmClient {
        &self.llm
    }

    pub fn tools(&self) -> &ToolEngine {
        &self.tools
    }

    pub async fn run(
        &self,
        profile: &AgentProfile,
        mut messages: Vec<Message>,
        options: AgentRuntimeOptions,
    ) -> Result<AgentRunOutput, AgentRuntimeError> {
        let perms = std::sync::Arc::new(profile.permissions.clone());

        for step in 0..options.max_steps {
            let tool_defs = tool_definitions_for(profile, &self.tools).await;
            let mut req = ChatRequest::new(messages.clone());
            req.tools = tool_defs;
            req.tool_choice = options.tool_choice.clone();
            req.parallel_tool_calls = options.parallel_tool_calls;
            req.temperature = options.temperature;
            req.max_completion_tokens = options.max_completion_tokens;

            let resp = self.llm.chat(req).await?;

            let mut assistant = resp.message;
            let tool_calls = match &mut assistant {
                Message::Assistant { tool_calls, .. } => {
                    normalize_tool_call_ids(step, tool_calls);
                    tool_calls.clone()
                }
                _ => Vec::new(),
            };
            messages.push(assistant);

            if tool_calls.is_empty() {
                return Ok(AgentRunOutput {
                    steps: step + 1,
                    messages,
                    finish_reason: resp.finish_reason,
                });
            }

            for group in group_tool_calls(&tool_calls) {
                match group {
                    ToolCallGroup::Exclusive(call) => {
                        match self.tools.execute_to_messages(perms.as_ref(), call).await {
                            Ok(tool_msgs) => messages.extend(tool_msgs),
                            Err(err) => match options.tool_error_mode {
                                ToolErrorMode::FailFast => return Err(err.into()),
                                ToolErrorMode::ToolMessage => {
                                    messages.push(Message::Tool {
                                        tool_call_id: call.id.clone(),
                                        content: format!("error: {err}"),
                                    });
                                }
                            },
                        }
                    }
                    ToolCallGroup::Parallel(calls) => {
                        if calls.len() == 1 {
                            let call = &calls[0];
                            match self.tools.execute_to_messages(perms.as_ref(), call).await {
                                Ok(tool_msgs) => messages.extend(tool_msgs),
                                Err(err) => match options.tool_error_mode {
                                    ToolErrorMode::FailFast => return Err(err.into()),
                                    ToolErrorMode::ToolMessage => {
                                        messages.push(Message::Tool {
                                            tool_call_id: call.id.clone(),
                                            content: format!("error: {err}"),
                                        });
                                    }
                                },
                            }
                            continue;
                        }

                        let mut set = tokio::task::JoinSet::new();
                        let tools = self.tools.clone();

                        for (idx, call) in calls.iter().cloned().enumerate() {
                            let tools = tools.clone();
                            let perms = perms.clone();
                            set.spawn(async move {
                                let res = tools.execute_to_messages(perms.as_ref(), &call).await;
                                (idx, call, res)
                            });
                        }

                        let mut results: Vec<Option<Result<Vec<Message>, ToolError>>> = Vec::new();
                        results.resize_with(calls.len(), || None);
                        while let Some(joined) = set.join_next().await {
                            let (idx, _call, res) = joined.map_err(|e| {
                                ToolError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
                            })?;
                            results[idx] = Some(res);
                        }

                        for (idx, call) in calls.iter().enumerate() {
                            let Some(res) = results.get_mut(idx).and_then(Option::take) else {
                                continue;
                            };

                            match res {
                                Ok(tool_msgs) => messages.extend(tool_msgs),
                                Err(err) => match options.tool_error_mode {
                                    ToolErrorMode::FailFast => return Err(err.into()),
                                    ToolErrorMode::ToolMessage => {
                                        messages.push(Message::Tool {
                                            tool_call_id: call.id.clone(),
                                            content: format!("error: {err}"),
                                        });
                                    }
                                },
                            }
                        }
                    }
                }
            }
        }

        Err(AgentRuntimeError::MaxSteps {
            max_steps: options.max_steps,
        })
    }

    pub async fn run_stream(
        &self,
        profile: &AgentProfile,
        mut messages: Vec<Message>,
        options: AgentRuntimeOptions,
    ) -> Result<
        tokio_stream::wrappers::ReceiverStream<Result<AgentEvent, AgentRuntimeError>>,
        AgentRuntimeError,
    > {
        use tokio_stream::wrappers::ReceiverStream;

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, AgentRuntimeError>>(64);

        let llm = self.llm.clone();
        let tools = self.tools.clone();
        let profile = profile.clone();
        let perms = std::sync::Arc::new(profile.permissions.clone());

        tokio::spawn(async move {
            for step in 0..options.max_steps {
                if tx
                    .send(Ok(AgentEvent::StepStart { step: step + 1 }))
                    .await
                    .is_err()
                {
                    return;
                }

                let mut req = ChatRequest::new(messages.clone());
                req.tools = tool_definitions_for(&profile, &tools).await;
                req.tool_choice = options.tool_choice.clone();
                req.parallel_tool_calls = options.parallel_tool_calls;
                req.temperature = options.temperature;
                req.max_completion_tokens = options.max_completion_tokens;

                let stream = match llm.chat_stream(req).await {
                    Ok(s) => s,
                    Err(err) => {
                        let _ = tx.send(Err(err.into())).await;
                        return;
                    }
                };

                match drive_stream_step(step, stream, &tx).await {
                    Ok(mut step_out) => {
                        if tx.is_closed() {
                            return;
                        }
                        normalize_stream_step_tool_calls(step, &mut step_out);
                        messages.push(step_out.assistant.clone());
                        let _ = tx
                            .send(Ok(AgentEvent::AssistantMessage {
                                message: step_out.assistant.clone(),
                            }))
                            .await;

                        if step_out.tool_calls.is_empty() {
                            let _ = tx.send(Ok(AgentEvent::StepEnd { step: step + 1 })).await;
                            let _ = tx
                                .send(Ok(AgentEvent::Done(AgentRunOutput {
                                    steps: step + 1,
                                    messages,
                                    finish_reason: step_out.finish_reason,
                                })))
                                .await;
                            return;
                        }

                        for group in group_tool_calls(&step_out.tool_calls) {
                            match group {
                                ToolCallGroup::Exclusive(call) => {
                                    if tx.is_closed() {
                                        return;
                                    }
                                    let _ = tx
                                        .send(Ok(AgentEvent::ToolCall { call: call.clone() }))
                                        .await;

                                    match tools.execute_to_messages(perms.as_ref(), call).await {
                                        Ok(tool_msgs) => {
                                            for msg in tool_msgs {
                                                let _ = tx
                                                    .send(Ok(AgentEvent::ToolResult {
                                                        message: msg.clone(),
                                                    }))
                                                    .await;
                                                messages.push(msg);
                                            }
                                        }
                                        Err(err) => match options.tool_error_mode {
                                            ToolErrorMode::FailFast => {
                                                let _ = tx.send(Err(err.into())).await;
                                                return;
                                            }
                                            ToolErrorMode::ToolMessage => {
                                                let tool_msg = Message::Tool {
                                                    tool_call_id: call.id.clone(),
                                                    content: format!("error: {err}"),
                                                };
                                                let _ = tx
                                                    .send(Ok(AgentEvent::ToolResult {
                                                        message: tool_msg.clone(),
                                                    }))
                                                    .await;
                                                messages.push(tool_msg);
                                            }
                                        },
                                    }
                                }
                                ToolCallGroup::Parallel(calls) => {
                                    if tx.is_closed() {
                                        return;
                                    }

                                    for call in calls.iter() {
                                        let _ = tx
                                            .send(Ok(AgentEvent::ToolCall { call: call.clone() }))
                                            .await;
                                    }

                                    if calls.len() == 1 {
                                        let call = &calls[0];
                                        match tools.execute_to_messages(perms.as_ref(), call).await
                                        {
                                            Ok(tool_msgs) => {
                                                for msg in tool_msgs {
                                                    let _ = tx
                                                        .send(Ok(AgentEvent::ToolResult {
                                                            message: msg.clone(),
                                                        }))
                                                        .await;
                                                    messages.push(msg);
                                                }
                                            }
                                            Err(err) => match options.tool_error_mode {
                                                ToolErrorMode::FailFast => {
                                                    let _ = tx.send(Err(err.into())).await;
                                                    return;
                                                }
                                                ToolErrorMode::ToolMessage => {
                                                    let tool_msg = Message::Tool {
                                                        tool_call_id: call.id.clone(),
                                                        content: format!("error: {err}"),
                                                    };
                                                    let _ = tx
                                                        .send(Ok(AgentEvent::ToolResult {
                                                            message: tool_msg.clone(),
                                                        }))
                                                        .await;
                                                    messages.push(tool_msg);
                                                }
                                            },
                                        }
                                        continue;
                                    }

                                    let mut set = tokio::task::JoinSet::new();
                                    let tools = tools.clone();

                                    for (idx, call) in calls.iter().cloned().enumerate() {
                                        let tools = tools.clone();
                                        let perms = perms.clone();
                                        set.spawn(async move {
                                            let res = tools
                                                .execute_to_messages(perms.as_ref(), &call)
                                                .await;
                                            (idx, call, res)
                                        });
                                    }

                                    let mut results: Vec<Option<Vec<Message>>> =
                                        vec![None; calls.len()];
                                    while let Some(joined) = set.join_next().await {
                                        if tx.is_closed() {
                                            return;
                                        }
                                        let (idx, call, res) = match joined {
                                            Ok(v) => v,
                                            Err(err) => {
                                                let _ = tx
                                                    .send(Err(ToolError::Io(std::io::Error::new(
                                                        std::io::ErrorKind::Other,
                                                        err,
                                                    ))
                                                    .into()))
                                                    .await;
                                                return;
                                            }
                                        };

                                        match res {
                                            Ok(tool_msgs) => {
                                                for msg in &tool_msgs {
                                                    let _ = tx
                                                        .send(Ok(AgentEvent::ToolResult {
                                                            message: msg.clone(),
                                                        }))
                                                        .await;
                                                }
                                                results[idx] = Some(tool_msgs);
                                            }
                                            Err(err) => match options.tool_error_mode {
                                                ToolErrorMode::FailFast => {
                                                    let _ = tx.send(Err(err.into())).await;
                                                    return;
                                                }
                                                ToolErrorMode::ToolMessage => {
                                                    let tool_msg = Message::Tool {
                                                        tool_call_id: call.id,
                                                        content: format!("error: {err}"),
                                                    };
                                                    results[idx] = Some(vec![tool_msg.clone()]);
                                                    let _ = tx
                                                        .send(Ok(AgentEvent::ToolResult {
                                                            message: tool_msg,
                                                        }))
                                                        .await;
                                                }
                                            },
                                        }
                                    }

                                    for group in results.into_iter().flatten() {
                                        messages.extend(group);
                                    }
                                }
                            }
                        }

                        let _ = tx.send(Ok(AgentEvent::StepEnd { step: step + 1 })).await;
                    }
                    Err(err) => {
                        let _ = tx.send(Err(err)).await;
                        return;
                    }
                }
            }

            let _ = tx
                .send(Err(AgentRuntimeError::MaxSteps {
                    max_steps: options.max_steps,
                }))
                .await;
        });

        Ok(ReceiverStream::new(rx))
    }
}

async fn tool_definitions_for(
    profile: &AgentProfile,
    tools: &ToolEngine,
) -> Vec<crate::llm::ToolDefinition> {
    let mut tool_defs = profile.tools.clone();
    tool_defs.extend(tools.extra_tool_definitions().await);
    tool_defs
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    StepStart { step: usize },
    StepEnd { step: usize },
    AssistantDelta { delta: String },
    AssistantThinkingDelta { delta: String },
    AssistantMessage { message: Message },
    ToolCall { call: ToolCall },
    ToolResult { message: Message },
    Done(AgentRunOutput),
}

#[derive(Debug, Clone, Copy)]
enum ToolCallGroup<'a> {
    Exclusive(&'a ToolCall),
    Parallel(&'a [ToolCall]),
}

fn group_tool_calls(tool_calls: &[ToolCall]) -> Vec<ToolCallGroup<'_>> {
    let mut out = Vec::new();
    let mut idx = 0usize;

    while idx < tool_calls.len() {
        let call = &tool_calls[idx];
        if tool_parallelism(call.name.as_str()).is_parallel() {
            let start = idx;
            idx += 1;
            while idx < tool_calls.len()
                && tool_parallelism(tool_calls[idx].name.as_str()) == ToolParallelism::Parallel
            {
                idx += 1;
            }
            out.push(ToolCallGroup::Parallel(&tool_calls[start..idx]));
        } else {
            out.push(ToolCallGroup::Exclusive(call));
            idx += 1;
        }
    }

    out
}

#[derive(Debug)]
struct StreamStepOutput {
    assistant: Message,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<FinishReason>,
}

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

fn normalize_tool_call_ids(step: usize, tool_calls: &mut Vec<ToolCall>) {
    let mut used: HashSet<String> = HashSet::with_capacity(tool_calls.len());

    for (idx, call) in tool_calls.iter_mut().enumerate() {
        let trimmed = call.id.trim();
        if trimmed != call.id {
            call.id = trimmed.to_string();
        }

        if call.id.is_empty() || used.contains(&call.id) {
            call.id = format!("call_step{}_{}", step + 1, idx);
        }
        used.insert(call.id.clone());
    }
}

fn normalize_stream_step_tool_calls(step: usize, out: &mut StreamStepOutput) {
    normalize_tool_call_ids(step, &mut out.tool_calls);
    if let Message::Assistant { tool_calls, .. } = &mut out.assistant {
        *tool_calls = out.tool_calls.clone();
    }
}

async fn drive_stream_step(
    step: usize,
    mut stream: impl Stream<Item = Result<ChatStreamChunk, LlmError>> + Unpin,
    tx: &tokio::sync::mpsc::Sender<Result<AgentEvent, AgentRuntimeError>>,
) -> Result<StreamStepOutput, AgentRuntimeError> {
    let mut assistant_content = String::new();
    let mut assistant_reasoning = String::new();
    let mut tool_calls: BTreeMap<u32, ToolCallBuf> = BTreeMap::new();
    let mut finish_reason = None;

    loop {
        let item = tokio::select! {
            _ = tx.closed() => {
                return Err(AgentRuntimeError::Cancelled);
            }
            item = stream.next() => item,
        };
        let Some(item) = item else {
            break;
        };
        let chunk = item?;

        if let Some(delta) = chunk.thinking_delta {
            assistant_reasoning.push_str(&delta);
            if tx
                .send(Ok(AgentEvent::AssistantThinkingDelta { delta }))
                .await
                .is_err()
            {
                return Err(AgentRuntimeError::Cancelled);
            }
        }

        if let Some(delta) = chunk.content_delta {
            assistant_content.push_str(&delta);
            if tx
                .send(Ok(AgentEvent::AssistantDelta { delta }))
                .await
                .is_err()
            {
                return Err(AgentRuntimeError::Cancelled);
            }
        }

        if !chunk.tool_calls.is_empty() {
            for tc in chunk.tool_calls {
                tool_calls.entry(tc.index).or_default();
                if let Some(buf) = tool_calls.get_mut(&tc.index) {
                    merge_tool_call_delta(buf, tc);
                }
            }
        }

        if chunk.finish_reason.is_some() {
            finish_reason = chunk.finish_reason;
        }
    }

    let mut resolved_calls = Vec::new();
    for (idx, buf) in tool_calls {
        let name = buf.name.unwrap_or_else(|| "unknown".to_string());
        let id = buf
            .id
            .unwrap_or_else(|| format!("call_step{}_{}", step + 1, idx));
        resolved_calls.push(ToolCall {
            id,
            name,
            arguments: buf.arguments,
        });
    }

    let assistant = Message::Assistant {
        content: if assistant_content.is_empty() {
            None
        } else {
            Some(assistant_content)
        },
        reasoning_content: if resolved_calls.is_empty() || assistant_reasoning.is_empty() {
            None
        } else {
            Some(assistant_reasoning)
        },
        tool_calls: resolved_calls.clone(),
    };

    Ok(StreamStepOutput {
        assistant,
        tool_calls: resolved_calls,
        finish_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn drive_stream_step_merges_tool_call_deltas() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, AgentRuntimeError>>(16);

        let chunks = vec![
            Ok(ChatStreamChunk {
                id: "chat_1".to_string(),
                created: 0,
                model: "m".to_string(),
                content_delta: Some("Hello ".to_string()),
                thinking_delta: None,
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_string()),
                    name: Some("read".to_string()),
                    arguments: Some("{\"path\":\"README.md\"".to_string()),
                }],
                finish_reason: None,
                usage: None,
            }),
            Ok(ChatStreamChunk {
                id: "chat_1".to_string(),
                created: 0,
                model: "m".to_string(),
                content_delta: Some("world".to_string()),
                thinking_delta: None,
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    arguments: Some("}".to_string()),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
            }),
        ];

        let stream = tokio_stream::iter(chunks);
        let out = drive_stream_step(0, stream, &tx).await.unwrap();
        drop(tx);

        let mut deltas = Vec::new();
        while let Some(event) = rx.recv().await {
            let event = event.unwrap();
            if let AgentEvent::AssistantDelta { delta } = event {
                deltas.push(delta);
            }
        }

        assert_eq!(deltas, vec!["Hello ".to_string(), "world".to_string()]);

        let Message::Assistant {
            content,
            tool_calls,
            ..
        } = out.assistant
        else {
            panic!("expected assistant message");
        };
        assert_eq!(content.unwrap(), "Hello world");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].name, "read");
        assert_eq!(tool_calls[0].arguments, "{\"path\":\"README.md\"}");
        assert_eq!(out.finish_reason, Some(FinishReason::Stop));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drive_stream_step_forwards_thinking_deltas_in_order() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, AgentRuntimeError>>(16);

        let chunks = vec![
            Ok(ChatStreamChunk {
                id: "chat_1".to_string(),
                created: 0,
                model: "m".to_string(),
                content_delta: Some("Hello ".to_string()),
                thinking_delta: Some("t1".to_string()),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
            }),
            Ok(ChatStreamChunk {
                id: "chat_1".to_string(),
                created: 0,
                model: "m".to_string(),
                content_delta: Some("world".to_string()),
                thinking_delta: Some("t2".to_string()),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: None,
            }),
        ];

        let stream = tokio_stream::iter(chunks);
        let out = drive_stream_step(0, stream, &tx).await.unwrap();
        drop(tx);

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            let event = event.unwrap();
            match event {
                AgentEvent::AssistantThinkingDelta { delta } => events.push(("thinking", delta)),
                AgentEvent::AssistantDelta { delta } => events.push(("content", delta)),
                _ => {}
            }
        }

        assert_eq!(
            events,
            vec![
                ("thinking", "t1".to_string()),
                ("content", "Hello ".to_string()),
                ("thinking", "t2".to_string()),
                ("content", "world".to_string()),
            ]
        );

        let Message::Assistant {
            content,
            tool_calls,
            ..
        } = out.assistant
        else {
            panic!("expected assistant message");
        };
        assert_eq!(content.unwrap(), "Hello world");
        assert!(tool_calls.is_empty());
        assert_eq!(out.finish_reason, Some(FinishReason::Stop));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drive_stream_step_generates_tool_call_id_and_unknown_name_when_missing() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, AgentRuntimeError>>(16);

        let chunks = vec![Ok(ChatStreamChunk {
            id: "chat_1".to_string(),
            created: 0,
            model: "m".to_string(),
            content_delta: None,
            thinking_delta: None,
            tool_calls: vec![ToolCallDelta {
                index: 1,
                id: None,
                name: None,
                arguments: Some("{\"x\":1}".to_string()),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: None,
        })];

        let stream = tokio_stream::iter(chunks);
        let out = drive_stream_step(0, stream, &tx).await.unwrap();

        let Message::Assistant {
            content,
            tool_calls,
            ..
        } = out.assistant
        else {
            panic!("expected assistant message");
        };
        assert!(content.is_none());
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_step1_1");
        assert_eq!(tool_calls[0].name, "unknown");
        assert_eq!(tool_calls[0].arguments, "{\"x\":1}");
    }
}
