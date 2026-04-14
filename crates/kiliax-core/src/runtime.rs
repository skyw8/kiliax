use std::collections::{BTreeMap, HashSet};

use tokio_stream::{Stream, StreamExt};
use tracing::Instrument;

use crate::agents::{AgentKind, AgentProfile};
use async_openai::types::FinishReason;

use crate::llm::{LlmClient, LlmError};
use crate::protocol::{
    ChatRequest, ChatStreamChunk, Message, TokenUsage, ToolCall, ToolCallDelta, ToolChoice,
    ToolDefinition,
};
use crate::telemetry;
use crate::tools::{policy, tool_parallelism, ToolEngine, ToolError, ToolParallelism};

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
            max_steps: 1024,
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
        let model_id = self.llm.route().model_id();
        let tool_policy = policy::ToolPolicy::for_model_id(&model_id);
        let perms = std::sync::Arc::new(profile.permissions.clone());

        for step in 0..options.max_steps {
            sanitize_tool_call_history(&mut messages);
            let tool_defs = tool_definitions_for(profile, &self.tools, &model_id).await;
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
                        if !tool_policy.allows_tool_name(call.name.as_str()) {
                            let reason = tool_policy
                                .denial_message(call.name.as_str())
                                .unwrap_or("tool not available for this model");
                            let err = ToolError::PermissionDenied(format!(
                                "{}: {reason}",
                                call.name.as_str()
                            ));
                            match options.tool_error_mode {
                                ToolErrorMode::FailFast => return Err(err.into()),
                                ToolErrorMode::ToolMessage => {
                                    messages.push(Message::Tool {
                                        tool_call_id: call.id.clone(),
                                        content: format!("error: {err}"),
                                    });
                                }
                            }
                            continue;
                        }
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
                            if !tool_policy.allows_tool_name(call.name.as_str()) {
                                let reason = tool_policy
                                    .denial_message(call.name.as_str())
                                    .unwrap_or("tool not available for this model");
                                let err = ToolError::PermissionDenied(format!(
                                    "{}: {reason}",
                                    call.name.as_str()
                                ));
                                match options.tool_error_mode {
                                    ToolErrorMode::FailFast => return Err(err.into()),
                                    ToolErrorMode::ToolMessage => {
                                        messages.push(Message::Tool {
                                            tool_call_id: call.id.clone(),
                                            content: format!("error: {err}"),
                                        });
                                    }
                                }
                                continue;
                            }
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

                        let mut results: Vec<Option<Result<Vec<Message>, ToolError>>> = Vec::new();
                        results.resize_with(calls.len(), || None);

                        for (idx, call) in calls.iter().cloned().enumerate() {
                            if !tool_policy.allows_tool_name(call.name.as_str()) {
                                let reason = tool_policy
                                    .denial_message(call.name.as_str())
                                    .unwrap_or("tool not available for this model");
                                results[idx] = Some(Err(ToolError::PermissionDenied(format!(
                                    "{}: {reason}",
                                    call.name.as_str()
                                ))));
                                continue;
                            }

                            let tools = tools.clone();
                            let perms = perms.clone();
                            let parent_span = tracing::Span::current();
                            set.spawn(
                                async move {
                                    let res =
                                        tools.execute_to_messages(perms.as_ref(), &call).await;
                                    (idx, call, res)
                                }
                                .instrument(parent_span),
                            );
                        }

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

        enum LoopControl {
            Continue,
            Return,
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, AgentRuntimeError>>(64);

        let llm = self.llm.clone();
        let tools = self.tools.clone();
        let profile = profile.clone();
        let perms = std::sync::Arc::new(profile.permissions.clone());
        let model_id = llm.route().model_id();
        let tool_policy = policy::ToolPolicy::for_model_id(&model_id);

        let started = std::time::Instant::now();
        let run_span = tracing::info_span!(
            "kiliax.agent.run",
            agent = %profile.name,
            max_steps = options.max_steps as u64,
        );
        telemetry::spans::set_attribute(&run_span, "langfuse.observation.type", "agent");

        tokio::spawn(
            async move {
                for step in 0..options.max_steps {
                    let step_no = step + 1;
                    let step_span = tracing::info_span!(
                        "kiliax.agent.step",
                        agent = %profile.name,
                        step = step_no as u64,
                    );
                    telemetry::spans::set_attribute(
                        &step_span,
                        "langfuse.observation.type",
                        "chain",
                    );

                    let control: LoopControl = async {
                        sanitize_tool_call_history(&mut messages);
                        if tx
                            .send(Ok(AgentEvent::StepStart { step: step_no }))
                            .await
                            .is_err()
                        {
                            return LoopControl::Return;
                        }

                        let mut req = ChatRequest::new(messages.clone());
            req.tools = tool_definitions_for(&profile, &tools, &model_id).await;
                        req.tool_choice = options.tool_choice.clone();
                        req.parallel_tool_calls = options.parallel_tool_calls;
                        req.temperature = options.temperature;
                        req.max_completion_tokens = options.max_completion_tokens;

                        let stream = match llm.chat_stream(req).await {
                            Ok(s) => s,
                            Err(err) => {
                                telemetry::metrics::record_run_finished(
                                    profile.name,
                                    "error",
                                    step_no as u64,
                                    started.elapsed(),
                                );
                                let _ = tx.send(Err(err.into())).await;
                                return LoopControl::Return;
                            }
                        };

                        match drive_stream_step(step, stream, &tx).await {
                            Ok(mut step_out) => {
                                if tx.is_closed() {
                                    return LoopControl::Return;
                                }
                                normalize_stream_step_tool_calls(step, &mut step_out);
                                messages.push(step_out.assistant.clone());
                                if telemetry::capture_enabled() {
                                    if let Ok(json) = serde_json::to_string(&step_out.assistant) {
                                        let captured = telemetry::capture_text(&json);
                                        tracing::info!(
                                            target: "kiliax_core::telemetry",
                                            event = "llm.response",
                                            llm_stream = true,
                                            step = step_no as u64,
                                            response_len = captured.len as u64,
                                            response_truncated = captured.truncated,
                                            response_sha256 = %captured.sha256.as_deref().unwrap_or(""),
                                            response = %captured.as_str(),
                                        );
                                    }
                                }
                                let _ = tx
                                    .send(Ok(AgentEvent::AssistantMessage {
                                        message: step_out.assistant.clone(),
                                    }))
                                    .await;

                                if step_out.tool_calls.is_empty() {
                                    let _ = tx
                                        .send(Ok(AgentEvent::StepEnd { step: step_no }))
                                        .await;
                                    let _ = tx
                                        .send(Ok(AgentEvent::Done(AgentRunOutput {
                                            steps: step_no,
                                            messages: std::mem::take(&mut messages),
                                            finish_reason: step_out.finish_reason,
                                        })))
                                        .await;
                                    telemetry::metrics::record_run_finished(
                                        profile.name,
                                        "done",
                                        step_no as u64,
                                        started.elapsed(),
                                    );
                                    return LoopControl::Return;
                                }

                                for group in group_tool_calls(&step_out.tool_calls) {
                                    match group {
                                        ToolCallGroup::Exclusive(call) => {
                                            if tx.is_closed() {
                                                return LoopControl::Return;
                                            }
                                            let _ = tx
                                                .send(Ok(AgentEvent::ToolCall {
                                                    call: call.clone(),
                                                }))
                                                .await;

                                            if !tool_policy.allows_tool_name(call.name.as_str()) {
                                                let reason = tool_policy
                                                    .denial_message(call.name.as_str())
                                                    .unwrap_or("tool not available for this model");
                                                let err = ToolError::PermissionDenied(format!(
                                                    "{}: {reason}",
                                                    call.name.as_str()
                                                ));
                                                match options.tool_error_mode {
                                                    ToolErrorMode::FailFast => {
                                                        let _ = tx.send(Err(err.into())).await;
                                                        return LoopControl::Return;
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
                                                        continue;
                                                    }
                                                }
                                            }

                                            match tools
                                                .execute_to_messages(perms.as_ref(), call)
                                                .await
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
                                                        return LoopControl::Return;
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
                                                return LoopControl::Return;
                                            }

                                            for call in calls.iter() {
                                                let _ = tx
                                                    .send(Ok(AgentEvent::ToolCall {
                                                        call: call.clone(),
                                                    }))
                                                    .await;
                                            }

                                            if calls.len() == 1 {
                                                let call = &calls[0];
                                                if !tool_policy
                                                    .allows_tool_name(call.name.as_str())
                                                {
                                                    let reason = tool_policy
                                                        .denial_message(call.name.as_str())
                                                        .unwrap_or(
                                                            "tool not available for this model",
                                                        );
                                                    let err = ToolError::PermissionDenied(
                                                        format!(
                                                            "{}: {reason}",
                                                            call.name.as_str()
                                                        ),
                                                    );
                                                    match options.tool_error_mode {
                                                        ToolErrorMode::FailFast => {
                                                            let _ = tx
                                                                .send(Err(err.into()))
                                                                .await;
                                                            return LoopControl::Return;
                                                        }
                                                        ToolErrorMode::ToolMessage => {
                                                            let tool_msg = Message::Tool {
                                                                tool_call_id: call.id.clone(),
                                                                content: format!("error: {err}"),
                                                            };
                                                            let _ = tx
                                                                .send(Ok(
                                                                    AgentEvent::ToolResult {
                                                                        message: tool_msg.clone(),
                                                                    },
                                                                ))
                                                                .await;
                                                            messages.push(tool_msg);
                                                            continue;
                                                        }
                                                    }
                                                }
                                                match tools
                                                    .execute_to_messages(perms.as_ref(), call)
                                                    .await
                                                {
                                                    Ok(tool_msgs) => {
                                                        for msg in tool_msgs {
                                                            let _ = tx
                                                                .send(Ok(
                                                                    AgentEvent::ToolResult {
                                                                        message: msg.clone(),
                                                                    },
                                                                ))
                                                                .await;
                                                            messages.push(msg);
                                                        }
                                                    }
                                                    Err(err) => match options.tool_error_mode {
                                                        ToolErrorMode::FailFast => {
                                                            let _ =
                                                                tx.send(Err(err.into())).await;
                                                            return LoopControl::Return;
                                                        }
                                                        ToolErrorMode::ToolMessage => {
                                                            let tool_msg = Message::Tool {
                                                                tool_call_id: call.id.clone(),
                                                                content: format!("error: {err}"),
                                                            };
                                                            let _ = tx
                                                                .send(Ok(
                                                                    AgentEvent::ToolResult {
                                                                        message: tool_msg.clone(),
                                                                    },
                                                                ))
                                                                .await;
                                                            messages.push(tool_msg);
                                                        }
                                                    },
                                                }
                                                continue;
                                            }

                                            let mut set = tokio::task::JoinSet::new();
                                            let tools = tools.clone();

                                            let mut results: Vec<Option<Vec<Message>>> =
                                                vec![None; calls.len()];

                                            for (idx, call) in calls.iter().cloned().enumerate() {
                                                if !tool_policy.allows_tool_name(call.name.as_str())
                                                {
                                                    let reason = tool_policy
                                                        .denial_message(call.name.as_str())
                                                        .unwrap_or("tool not available for this model");
                                                    let err = ToolError::PermissionDenied(format!(
                                                        "{}: {reason}",
                                                        call.name.as_str()
                                                    ));
                                                    match options.tool_error_mode {
                                                        ToolErrorMode::FailFast => {
                                                            let _ =
                                                                tx.send(Err(err.into())).await;
                                                            return LoopControl::Return;
                                                        }
                                                        ToolErrorMode::ToolMessage => {
                                                            let tool_msg = Message::Tool {
                                                                tool_call_id: call.id.clone(),
                                                                content: format!("error: {err}"),
                                                            };
                                                            results[idx] =
                                                                Some(vec![tool_msg.clone()]);
                                                            let _ = tx
                                                                .send(Ok(
                                                                    AgentEvent::ToolResult {
                                                                        message: tool_msg,
                                                                    },
                                                                ))
                                                                .await;
                                                            continue;
                                                        }
                                                    }
                                                }

                                                let tools = tools.clone();
                                                let perms = perms.clone();
                                                let parent_span = tracing::Span::current();
                                                set.spawn(
                                                    async move {
                                                        let res = tools
                                                            .execute_to_messages(
                                                                perms.as_ref(),
                                                                &call,
                                                            )
                                                            .await;
                                                        (idx, call, res)
                                                    }
                                                    .instrument(parent_span),
                                                );
                                            }

                                            while let Some(joined) = set.join_next().await {
                                                if tx.is_closed() {
                                                    return LoopControl::Return;
                                                }
                                                let (idx, call, res) = match joined {
                                                    Ok(v) => v,
                                                    Err(err) => {
                                                        let _ = tx
                                                            .send(Err(ToolError::Io(
                                                                std::io::Error::new(
                                                                    std::io::ErrorKind::Other,
                                                                    err,
                                                                ),
                                                            )
                                                            .into()))
                                                            .await;
                                                        return LoopControl::Return;
                                                    }
                                                };

                                                match res {
                                                    Ok(tool_msgs) => {
                                                        for msg in &tool_msgs {
                                                            let _ = tx
                                                                .send(Ok(
                                                                    AgentEvent::ToolResult {
                                                                        message: msg.clone(),
                                                                    },
                                                                ))
                                                                .await;
                                                        }
                                                        results[idx] = Some(tool_msgs);
                                                    }
                                                    Err(err) => match options.tool_error_mode {
                                                        ToolErrorMode::FailFast => {
                                                            let _ =
                                                                tx.send(Err(err.into())).await;
                                                            return LoopControl::Return;
                                                        }
                                                        ToolErrorMode::ToolMessage => {
                                                            let tool_msg = Message::Tool {
                                                                tool_call_id: call.id,
                                                                content: format!("error: {err}"),
                                                            };
                                                            results[idx] =
                                                                Some(vec![tool_msg.clone()]);
                                                            let _ = tx
                                                                .send(Ok(
                                                                    AgentEvent::ToolResult {
                                                                        message: tool_msg,
                                                                    },
                                                                ))
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

                                let _ = tx.send(Ok(AgentEvent::StepEnd { step: step_no })).await;
                            }
                            Err(err) => {
                                telemetry::metrics::record_run_finished(
                                    profile.name,
                                    "error",
                                    step_no as u64,
                                    started.elapsed(),
                                );
                                let _ = tx.send(Err(err)).await;
                                return LoopControl::Return;
                            }
                        }

                        LoopControl::Continue
                    }
                    .instrument(step_span)
                    .await;

                    match control {
                        LoopControl::Continue => {}
                        LoopControl::Return => return,
                    }
                }

                telemetry::metrics::record_run_finished(
                    profile.name,
                    "max_steps",
                    options.max_steps as u64,
                    started.elapsed(),
                );
                let _ = tx
                    .send(Err(AgentRuntimeError::MaxSteps {
                        max_steps: options.max_steps,
                    }))
                    .await;
            }
            .instrument(run_span),
        );

        Ok(ReceiverStream::new(rx))
    }
}

async fn tool_definitions_for(
    profile: &AgentProfile,
    tools: &ToolEngine,
    model_id: &str,
) -> Vec<ToolDefinition> {
    policy::tool_definitions_for_agent(profile, tools, model_id).await
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

fn sanitize_tool_call_history(messages: &mut Vec<Message>) {
    if messages.iter().all(|m| {
        !matches!(m, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty())
            && !matches!(m, Message::Tool { .. })
    }) {
        return;
    }

    let mut queue: std::collections::VecDeque<Message> = std::mem::take(messages).into();
    let mut out: Vec<Message> = Vec::with_capacity(queue.len());

    while let Some(msg) = queue.pop_front() {
        match msg {
            Message::Assistant {
                content,
                reasoning_content,
                tool_calls,
                usage,
            } if !tool_calls.is_empty() => {
                let expected_ids: Vec<String> = tool_calls.iter().map(|c| c.id.clone()).collect();
                out.push(Message::Assistant {
                    content,
                    reasoning_content,
                    tool_calls,
                    usage,
                });

                let mut segment_tool_msgs: Vec<Message> = Vec::new();
                let mut segment_other_msgs: Vec<Message> = Vec::new();
                while !matches!(queue.front(), Some(Message::Assistant { .. }) | None) {
                    let next = queue.pop_front().expect("front checked");
                    match next {
                        Message::Tool { .. } => segment_tool_msgs.push(next),
                        other => segment_other_msgs.push(other),
                    }
                }

                let mut remaining: Vec<Option<Message>> =
                    segment_tool_msgs.into_iter().map(Some).collect();
                for expected_id in expected_ids {
                    let mut picked: Option<Message> = None;
                    for slot in remaining.iter_mut() {
                        let Some(Message::Tool { tool_call_id, .. }) = slot.as_ref() else {
                            continue;
                        };
                        if tool_call_id == &expected_id {
                            picked = slot.take();
                            break;
                        }
                    }

                    if let Some(msg) = picked {
                        out.push(msg);
                    } else {
                        out.push(Message::Tool {
                            tool_call_id: expected_id,
                            content: "error: missing tool response message (repaired)".to_string(),
                        });
                    }
                }

                out.extend(segment_other_msgs);
            }
            Message::Tool { .. } => {}
            other => out.push(other),
        }
    }

    *messages = out;
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
    let mut last_usage = None;
    let mut assistant_body_started = false;

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

        let ChatStreamChunk {
            content_delta,
            thinking_delta,
            tool_calls: tool_call_deltas,
            finish_reason: chunk_finish_reason,
            usage,
            ..
        } = chunk;

        if let Some(delta) = thinking_delta.filter(|_| !assistant_body_started) {
            assistant_reasoning.push_str(&delta);
            if tx
                .send(Ok(AgentEvent::AssistantThinkingDelta { delta }))
                .await
                .is_err()
            {
                return Err(AgentRuntimeError::Cancelled);
            }
        }

        if let Some(delta) = content_delta {
            assistant_body_started = true;
            assistant_content.push_str(&delta);
            if tx
                .send(Ok(AgentEvent::AssistantDelta { delta }))
                .await
                .is_err()
            {
                return Err(AgentRuntimeError::Cancelled);
            }
        }

        for tc in tool_call_deltas {
            tool_calls.entry(tc.index).or_default();
            if let Some(buf) = tool_calls.get_mut(&tc.index) {
                merge_tool_call_delta(buf, tc);
            }
        }

        if chunk_finish_reason.is_some() {
            finish_reason = chunk_finish_reason;
        }

        if let Some(usage) = usage {
            last_usage = Some(usage);
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
        usage: last_usage.as_ref().map(TokenUsage::from_completion_usage),
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
    use crate::protocol::UserMessageContent;

    #[test]
    fn sanitize_tool_call_history_reorders_tool_messages() {
        let mut messages = vec![
            Message::User {
                content: UserMessageContent::Text("hi".to_string()),
            },
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: vec![
                    ToolCall {
                        id: "a".to_string(),
                        name: "t".to_string(),
                        arguments: "{}".to_string(),
                    },
                    ToolCall {
                        id: "b".to_string(),
                        name: "t".to_string(),
                        arguments: "{}".to_string(),
                    },
                ],
                usage: None,
            },
            Message::Tool {
                tool_call_id: "b".to_string(),
                content: "B".to_string(),
            },
            Message::Tool {
                tool_call_id: "a".to_string(),
                content: "A".to_string(),
            },
            Message::Assistant {
                content: Some("done".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            },
        ];

        sanitize_tool_call_history(&mut messages);

        assert!(matches!(
            messages.get(2),
            Some(Message::Tool {
                tool_call_id,
                content,
            }) if tool_call_id == "a" && content == "A"
        ));
        assert!(matches!(
            messages.get(3),
            Some(Message::Tool {
                tool_call_id,
                content,
            }) if tool_call_id == "b" && content == "B"
        ));
    }

    #[test]
    fn sanitize_tool_call_history_inserts_missing_tool_messages() {
        let mut messages = vec![
            Message::Assistant {
                content: Some("call tools".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "x".to_string(),
                    name: "t".to_string(),
                    arguments: "{}".to_string(),
                }],
                usage: None,
            },
            Message::Assistant {
                content: Some("next".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            },
        ];

        sanitize_tool_call_history(&mut messages);

        assert!(matches!(
            messages.get(1),
            Some(Message::Tool { tool_call_id, .. }) if tool_call_id == "x"
        ));
    }

    #[test]
    fn sanitize_tool_call_history_moves_non_tool_messages_after_tool_messages() {
        let mut messages = vec![
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: vec![
                    ToolCall {
                        id: "a".to_string(),
                        name: "t".to_string(),
                        arguments: "{}".to_string(),
                    },
                    ToolCall {
                        id: "b".to_string(),
                        name: "t".to_string(),
                        arguments: "{}".to_string(),
                    },
                ],
                usage: None,
            },
            Message::Tool {
                tool_call_id: "a".to_string(),
                content: "A".to_string(),
            },
            Message::User {
                content: UserMessageContent::Text("[img]".to_string()),
            },
            Message::Tool {
                tool_call_id: "b".to_string(),
                content: "B".to_string(),
            },
            Message::Assistant {
                content: Some("done".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            },
        ];

        sanitize_tool_call_history(&mut messages);

        assert!(matches!(
            messages.get(1),
            Some(Message::Tool { tool_call_id, .. }) if tool_call_id == "a"
        ));
        assert!(matches!(
            messages.get(2),
            Some(Message::Tool { tool_call_id, .. }) if tool_call_id == "b"
        ));
        assert!(matches!(messages.get(3), Some(Message::User { .. })));
    }

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
    async fn drive_stream_step_attaches_usage_from_final_chunk() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, AgentRuntimeError>>(16);

        let usage =
            serde_json::from_value::<async_openai::types::CompletionUsage>(serde_json::json!({
                "prompt_tokens": 19,
                "completion_tokens": 21,
                "total_tokens": 40,
                "prompt_tokens_details": { "cached_tokens": 10 }
            }))
            .unwrap();

        let chunks = vec![
            Ok(ChatStreamChunk {
                id: "chat_1".to_string(),
                created: 0,
                model: "m".to_string(),
                content_delta: Some("hello".to_string()),
                thinking_delta: None,
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
            }),
            Ok(ChatStreamChunk {
                id: "chat_1".to_string(),
                created: 0,
                model: "m".to_string(),
                content_delta: None,
                thinking_delta: None,
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: Some(usage),
            }),
        ];

        let stream = tokio_stream::iter(chunks);
        let out = drive_stream_step(0, stream, &tx).await.unwrap();

        let Message::Assistant { usage, .. } = out.assistant else {
            panic!("expected assistant message");
        };
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 19);
        assert_eq!(usage.completion_tokens, 21);
        assert_eq!(usage.total_tokens, 40);
        assert_eq!(usage.cached_tokens, Some(10));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drive_stream_step_stops_forwarding_thinking_after_body_starts() {
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

    #[test]
    fn group_tool_calls_treats_apply_patch_as_barrier() {
        let tool_calls = vec![
            ToolCall {
                id: "read".to_string(),
                name: crate::tools::builtin::TOOL_READ_FILE.to_string(),
                arguments: "{}".to_string(),
            },
            ToolCall {
                id: "patch".to_string(),
                name: crate::tools::builtin::TOOL_APPLY_PATCH.to_string(),
                arguments: "{}".to_string(),
            },
            ToolCall {
                id: "grep".to_string(),
                name: crate::tools::builtin::TOOL_GREP_FILES.to_string(),
                arguments: "{}".to_string(),
            },
        ];

        let groups = group_tool_calls(&tool_calls);

        assert!(matches!(groups[0], ToolCallGroup::Parallel(calls) if calls.len() == 1));
        assert!(matches!(
            groups[1],
            ToolCallGroup::Exclusive(call) if call.name == crate::tools::builtin::TOOL_APPLY_PATCH
        ));
        assert!(matches!(groups[2], ToolCallGroup::Parallel(calls) if calls.len() == 1));
    }
}
