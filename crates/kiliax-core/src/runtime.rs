use tracing::Instrument;

use crate::agents::{AgentKind, AgentProfile};

use crate::history::{assistant_message_is_empty, sanitize_history_for_next_request};
use crate::llm::{llm_retry_decision, LlmClient, LlmError, LlmRetryKind, LlmRetryMode};
use crate::protocol::{
    ChatRequest, FinishReason, Message, ReasoningEffort, ToolCall, ToolChoice, ToolDefinition,
};
use crate::telemetry;
use crate::tools::{policy, ToolEngine, ToolError};

mod streaming;
pub(crate) mod tool_calls;

use streaming::{drive_stream_step, normalize_stream_step_tool_calls};
use tool_calls::{group_tool_calls, normalize_tool_call_ids, ToolCallGroup};

#[derive(Debug, thiserror::Error)]
pub enum AgentRuntimeError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error(transparent)]
    LlmBeforeOutput(LlmError),

    #[error(transparent)]
    Tool(#[from] ToolError),

    #[error("cancelled")]
    Cancelled,

    #[error("max steps exceeded: {max_steps}")]
    MaxSteps { max_steps: usize },

    #[error("empty assistant message from model at step {step}")]
    EmptyAssistantMessage { step: usize },
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
    pub reasoning_effort: Option<ReasoningEffort>,
    pub auto_compact_token_limit: Option<usize>,
    pub retry_mode: LlmRetryMode,
    pub cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
}

impl Default for AgentRuntimeOptions {
    fn default() -> Self {
        Self {
            max_steps: 1024,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
            tool_error_mode: ToolErrorMode::ToolMessage,
            temperature: None,
            reasoning_effort: None,
            auto_compact_token_limit: None,
            retry_mode: LlmRetryMode::Run,
            cancel_rx: None,
        }
    }
}

impl AgentRuntimeOptions {
    /// Build runtime options from `kiliax.yaml`.
    ///
    /// Uses `default_model` for model-specific options.
    pub fn from_config(profile: &AgentProfile, config: &crate::config::Config) -> Self {
        let model_id = config.default_model.as_deref();
        Self::from_config_for_model(profile, config, model_id)
    }

    /// Build runtime options from `kiliax.yaml` for a concrete model.
    ///
    /// Precedence:
    /// 1) `runtime.*` (global defaults)
    /// 2) `agents.<kind>.*` (per-agent overrides)
    /// 3) `providers.<provider>.models[].auto_compact_token_limit` (per-model override)
    pub fn from_config_for_model(
        profile: &AgentProfile,
        config: &crate::config::Config,
        model_id: Option<&str>,
    ) -> Self {
        let mut options = Self::default();

        if let Some(max_steps) = config.runtime.max_steps {
            options.max_steps = max_steps;
        }
        options.auto_compact_token_limit = config.runtime.auto_compact_token_limit;

        let custom_cfg;
        let agent_cfg = match profile.kind {
            AgentKind::Plan => Some(&config.agents.plan),
            AgentKind::General => Some(&config.agents.general),
            AgentKind::Custom => {
                custom_cfg = profile.runtime.clone();
                custom_cfg.as_ref()
            }
        };
        if let Some(max_steps) = agent_cfg.and_then(|cfg| cfg.max_steps) {
            options.max_steps = max_steps;
        }
        if let Some(auto_compact_token_limit) =
            agent_cfg.and_then(|cfg| cfg.auto_compact_token_limit)
        {
            options.auto_compact_token_limit = Some(auto_compact_token_limit);
        }
        if let Some(auto_compact_token_limit) =
            model_id.and_then(|model_id| config.model_auto_compact_token_limit(model_id))
        {
            options.auto_compact_token_limit = Some(auto_compact_token_limit);
        }
        if let Some(temperature) = model_id.and_then(|model_id| config.model_temperature(model_id))
        {
            options.temperature = Some(temperature);
        }
        if let Some(reasoning_effort) =
            model_id.and_then(|model_id| config.model_reasoning_effort(model_id))
        {
            options.reasoning_effort = Some(reasoning_effort);
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
            let sanitize_report = sanitize_history_for_next_request(&mut messages);
            if sanitize_report.changed() {
                tracing::warn!(
                    event = "history.sanitized",
                    dropped_empty_assistant = sanitize_report.dropped_empty_assistant,
                    dropped_orphan_tool = sanitize_report.dropped_orphan_tool,
                    inserted_missing_tool_result = sanitize_report.inserted_missing_tool_result,
                );
            }
            let tool_defs = tool_definitions_for(profile, &self.tools, &model_id).await;
            let mut req = ChatRequest::new(messages.clone());
            req.tools = tool_defs;
            req.tool_choice = options.tool_choice.clone();
            req.parallel_tool_calls = options.parallel_tool_calls;
            req.temperature = options.temperature;
            req.reasoning_effort = options.reasoning_effort;

            let resp = self.llm.chat(req).await?;

            let mut assistant = resp.message;
            let tool_calls = match &mut assistant {
                Message::Assistant { tool_calls, .. } => {
                    normalize_tool_call_ids(step, tool_calls);
                    tool_calls.clone()
                }
                _ => Vec::new(),
            };
            if assistant_message_is_empty(&assistant) {
                return Err(AgentRuntimeError::EmptyAssistantMessage { step: step + 1 });
            }
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
                            let (idx, _call, res) =
                                joined.map_err(|e| ToolError::Io(std::io::Error::other(e)))?;
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
                        let sanitize_report = sanitize_history_for_next_request(&mut messages);
                        if sanitize_report.changed() {
                            tracing::warn!(
                                event = "history.sanitized",
                                dropped_empty_assistant = sanitize_report.dropped_empty_assistant,
                                dropped_orphan_tool = sanitize_report.dropped_orphan_tool,
                                inserted_missing_tool_result = sanitize_report.inserted_missing_tool_result,
                            );
                        }
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
                        req.reasoning_effort = options.reasoning_effort;

                        let step_result = drive_step_with_retry(
                            &llm,
                            req,
                            step,
                            options.retry_mode,
                            options.cancel_rx.clone(),
                            &tx,
                        )
                        .await;

                        match step_result {
                            Ok(mut step_out) => {
                                if tx.is_closed() {
                                    return LoopControl::Return;
                                }
                                normalize_stream_step_tool_calls(step, &mut step_out);
                                if assistant_message_is_empty(&step_out.assistant) {
                                    telemetry::metrics::record_run_finished(
                                        &profile.name,
                                        "empty_assistant_message",
                                        step_no as u64,
                                        started.elapsed(),
                                    );
                                    let _ = tx
                                        .send(Err(AgentRuntimeError::EmptyAssistantMessage {
                                            step: step_no,
                                        }))
                                        .await;
                                    return LoopControl::Return;
                                }
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
                                        &profile.name,
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
                                                                std::io::Error::other(err),
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
                                    &profile.name,
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
                    &profile.name,
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
    LlmRetry(LlmRetryEvent),
    AssistantDelta { delta: String },
    AssistantThinkingDelta { delta: String },
    AssistantMessage { message: Message },
    ToolCall { call: ToolCall },
    ToolResult { message: Message },
    Done(AgentRunOutput),
}

#[derive(Debug, Clone)]
pub struct LlmRetryEvent {
    pub kind: LlmRetryKind,
    pub attempt: u32,
    pub max_attempts: Option<u32>,
    pub delay_ms: u64,
    pub message: String,
}

async fn drive_step_with_retry(
    llm: &LlmClient,
    req: ChatRequest,
    step: usize,
    mode: LlmRetryMode,
    mut cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    tx: &tokio::sync::mpsc::Sender<Result<AgentEvent, AgentRuntimeError>>,
) -> Result<streaming::StreamStepOutput, AgentRuntimeError> {
    let mut attempt = 1_u32;
    loop {
        let stream = match llm.chat_stream(req.clone()).await {
            Ok(stream) => stream,
            Err(err) => {
                let decision = llm_retry_decision(&err, mode, attempt);
                retry_or_return(err, decision, attempt, cancel_rx.as_mut(), tx).await?;
                attempt = attempt.saturating_add(1);
                continue;
            }
        };

        match drive_stream_step(step, stream, tx).await {
            Ok(out) => return Ok(out),
            Err(AgentRuntimeError::LlmBeforeOutput(err)) => {
                let decision = llm_retry_decision(&err, mode, attempt);
                retry_or_return(err, decision, attempt, cancel_rx.as_mut(), tx).await?;
                attempt = attempt.saturating_add(1);
            }
            Err(err) => return Err(err),
        }
    }
}

async fn retry_or_return(
    err: LlmError,
    decision: crate::llm::LlmRetryDecision,
    attempt: u32,
    cancel_rx: Option<&mut tokio::sync::watch::Receiver<bool>>,
    tx: &tokio::sync::mpsc::Sender<Result<AgentEvent, AgentRuntimeError>>,
) -> Result<(), AgentRuntimeError> {
    if !decision.retryable {
        return Err(err.into());
    }
    let event = AgentEvent::LlmRetry(LlmRetryEvent {
        kind: decision.kind,
        attempt,
        max_attempts: decision.max_attempts,
        delay_ms: decision.delay.as_millis().min(u128::from(u64::MAX)) as u64,
        message: decision.message,
    });
    if tx.send(Ok(event)).await.is_err() {
        return Err(err.into());
    }

    if let Some(rx) = cancel_rx {
        tokio::select! {
            _ = tokio::time::sleep(decision.delay) => {}
            changed = rx.changed() => {
                if changed.is_ok() && *rx.borrow() {
                    return Err(AgentRuntimeError::Cancelled);
                }
            }
        }
    } else {
        tokio::time::sleep(decision.delay).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::sanitize_history_for_next_request as sanitize_tool_call_history;
    use crate::protocol::UserMessageContent;
    use crate::protocol::{ChatStreamChunk, ProviderMessageMetadata, ToolCallDelta};

    #[test]
    fn sanitize_tool_call_history_reorders_tool_messages() {
        let mut messages = vec![
            Message::User {
                content: UserMessageContent::Text("hi".to_string()),
                hidden: false,
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
                provider_metadata: None,
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
                provider_metadata: None,
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
                provider_metadata: None,
            },
            Message::Assistant {
                content: Some("next".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
                provider_metadata: None,
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
                provider_metadata: None,
            },
            Message::Tool {
                tool_call_id: "a".to_string(),
                content: "A".to_string(),
            },
            Message::User {
                content: UserMessageContent::Text("[img]".to_string()),
                hidden: false,
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
                provider_metadata: None,
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

    #[test]
    fn sanitize_tool_call_history_drops_empty_assistant_messages() {
        let mut messages = vec![
            Message::User {
                content: UserMessageContent::Text("hi".to_string()),
                hidden: false,
            },
            Message::Assistant {
                content: None,
                reasoning_content: Some("thinking only".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                provider_metadata: None,
            },
            Message::Assistant {
                content: Some("done".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
                provider_metadata: None,
            },
        ];

        sanitize_tool_call_history(&mut messages);

        assert_eq!(messages.len(), 2);
        assert!(matches!(messages.get(0), Some(Message::User { .. })));
        assert!(matches!(
            messages.get(1),
            Some(Message::Assistant {
                content: Some(content),
                tool_calls,
                ..
            }) if content == "done" && tool_calls.is_empty()
        ));
    }

    #[test]
    fn runtime_options_read_auto_compact_limit_for_model_with_fallback() {
        let config = crate::config::load_from_str(
            r#"
providers:
  test:
    base_url: http://localhost
    models:
      - small
      - id: large
        auto_compact_token_limit: 2000
        temperature: 0.2
        reasoning_effort: low
runtime:
  auto_compact_token_limit: 1000
"#,
        )
        .unwrap();
        let profile = crate::agents::AgentProfile::general();

        let small =
            AgentRuntimeOptions::from_config_for_model(&profile, &config, Some("test/small"));
        let large =
            AgentRuntimeOptions::from_config_for_model(&profile, &config, Some("test/large"));

        assert_eq!(small.auto_compact_token_limit, Some(1000));
        assert_eq!(large.auto_compact_token_limit, Some(2000));
        assert_eq!(large.temperature, Some(0.2));
        assert_eq!(large.reasoning_effort, Some(ReasoningEffort::Low));
    }

    #[test]
    fn runtime_options_model_auto_compact_limit_overrides_agent_limit() {
        let config = crate::config::load_from_str(
            r#"
providers:
  test:
    base_url: http://localhost
    models:
      - id: m
        auto_compact_token_limit: 4000
runtime:
  auto_compact_token_limit: 1000
agents:
  general:
    auto_compact_token_limit: 3000
"#,
        )
        .unwrap();
        let profile = crate::agents::AgentProfile::general();

        let options = AgentRuntimeOptions::from_config_for_model(&profile, &config, Some("test/m"));

        assert_eq!(options.auto_compact_token_limit, Some(4000));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drive_stream_step_keeps_length_finish_reason() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, AgentRuntimeError>>(16);

        let chunks = vec![Ok(ChatStreamChunk {
            id: "chat_1".to_string(),
            created: 0,
            model: "m".to_string(),
            content_delta: Some("partial".to_string()),
            thinking_delta: None,
            tool_calls: Vec::new(),
            finish_reason: Some(FinishReason::Length),
            usage: None,
            provider_metadata: None,
        })];

        let stream = tokio_stream::iter(chunks);
        let out = drive_stream_step(0, stream, &tx).await.unwrap();

        assert_eq!(out.finish_reason, Some(FinishReason::Length));
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
                provider_metadata: None,
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
                provider_metadata: None,
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

        let usage = crate::protocol::TokenUsage {
            prompt_tokens: 19,
            completion_tokens: 21,
            total_tokens: 40,
            cached_tokens: Some(10),
        };

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
                provider_metadata: None,
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
                provider_metadata: None,
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
    async fn drive_stream_step_attaches_provider_metadata_from_final_chunk() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, AgentRuntimeError>>(16);
        let metadata = ProviderMessageMetadata::OpenAiResponses {
            output: vec![serde_json::json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "read",
                "arguments": "{}"
            })],
        };

        let chunks = vec![Ok(ChatStreamChunk {
            id: "resp_1".to_string(),
            created: 0,
            model: "m".to_string(),
            content_delta: None,
            thinking_delta: None,
            tool_calls: vec![ToolCallDelta {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("read".to_string()),
                arguments: Some("{}".to_string()),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            provider_metadata: Some(metadata.clone()),
        })];

        let stream = tokio_stream::iter(chunks);
        let out = drive_stream_step(0, stream, &tx).await.unwrap();

        let Message::Assistant {
            provider_metadata, ..
        } = out.assistant
        else {
            panic!("expected assistant message");
        };
        assert_eq!(provider_metadata, Some(metadata));
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
                provider_metadata: None,
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
                provider_metadata: None,
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
            provider_metadata: None,
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
