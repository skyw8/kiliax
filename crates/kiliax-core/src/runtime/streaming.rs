use std::collections::BTreeMap;

use tokio_stream::{Stream, StreamExt};

use crate::llm::LlmError;
use crate::protocol::{ChatStreamChunk, Message, TokenUsage, ToolCall, ToolCallDelta};

use super::{tool_calls::normalize_tool_call_ids, AgentEvent, AgentRuntimeError};

#[derive(Debug)]
pub(super) struct StreamStepOutput {
    pub(super) assistant: Message,
    pub(super) tool_calls: Vec<ToolCall>,
    pub(super) finish_reason: Option<async_openai::types::FinishReason>,
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

pub(super) fn normalize_stream_step_tool_calls(step: usize, out: &mut StreamStepOutput) {
    normalize_tool_call_ids(step, &mut out.tool_calls);
    if let Message::Assistant { tool_calls, .. } = &mut out.assistant {
        *tool_calls = out.tool_calls.clone();
    }
}

pub(super) async fn drive_stream_step(
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

