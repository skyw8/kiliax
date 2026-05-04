use async_openai::types::CompletionUsage;
use serde::Deserialize;

use crate::types::{
    ChatResponse, ChatStreamChunk, FinishReason, Message, TokenUsage, ToolCall, ToolCallDelta,
};

use super::{tool_names::to_internal_tool_name, LlmError};

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ByotCreateChatCompletionResponse {
    id: String,
    created: u32,
    model: String,
    #[serde(default)]
    choices: Vec<ByotChatChoice>,
    #[serde(default)]
    usage: Option<CompletionUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ByotChatChoice {
    pub message: ByotChatCompletionMessage,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ByotChatCompletionMessage {
    #[serde(default)]
    pub content: Option<String>,

    #[serde(default, rename = "reasoning_content")]
    pub reasoning_content: Option<String>,

    #[serde(default)]
    pub thinking: Option<String>,

    #[serde(default)]
    pub reasoning: Option<String>,

    #[serde(default)]
    pub tool_calls: Option<Vec<ByotToolCall>>,

    #[serde(default)]
    pub function_call: Option<ByotFunctionCall>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ByotToolCall {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<ByotFunctionCall>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ByotFunctionCall {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

pub(super) fn chat_response_from_byot(
    resp: ByotCreateChatCompletionResponse,
) -> Result<ChatResponse, LlmError> {
    let ByotCreateChatCompletionResponse {
        id,
        created,
        model,
        choices,
        usage,
    } = resp;

    let choice = choices.into_iter().next().ok_or(LlmError::NoChoices)?;
    let ByotChatChoice {
        message,
        finish_reason,
    } = choice;
    let ByotChatCompletionMessage {
        content,
        reasoning_content,
        thinking,
        reasoning,
        tool_calls,
        function_call,
    } = message;

    let tool_calls = if let Some(calls) = tool_calls {
        calls
            .into_iter()
            .map(|c| ToolCall {
                id: c.id.unwrap_or_default(),
                name: c
                    .function
                    .as_ref()
                    .and_then(|f| f.name.clone())
                    .map(|name| to_internal_tool_name(&name).to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                arguments: c
                    .function
                    .as_ref()
                    .and_then(|f| f.arguments.clone())
                    .unwrap_or_default(),
            })
            .collect()
    } else if let Some(call) = function_call {
        vec![ToolCall {
            id: String::new(),
            name: call
                .name
                .map(|name| to_internal_tool_name(&name).to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            arguments: call.arguments.unwrap_or_default(),
        }]
    } else {
        Vec::new()
    };

    let reasoning_content = if tool_calls.is_empty() {
        None
    } else {
        reasoning_content.or(thinking).or(reasoning)
    };

    let usage = usage.as_ref().map(token_usage_from_openai);
    Ok(ChatResponse {
        id,
        created,
        model,
        message: Message::Assistant {
            content,
            reasoning_content,
            tool_calls,
            usage,
            provider_metadata: None,
        },
        finish_reason,
        usage,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ByotCreateChatCompletionStreamResponse {
    id: String,
    created: u32,
    model: String,
    #[serde(default)]
    choices: Vec<ByotChatChoiceStream>,
    #[serde(default)]
    usage: Option<CompletionUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ByotChatChoiceStream {
    #[serde(default)]
    pub delta: ByotChatCompletionStreamDelta,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ByotChatCompletionStreamDelta {
    #[serde(default)]
    pub content: Option<String>,

    #[serde(default, rename = "reasoning_content")]
    pub reasoning_content: Option<String>,

    #[serde(default)]
    pub thinking: Option<String>,

    #[serde(default)]
    pub reasoning: Option<String>,

    #[serde(default)]
    pub tool_calls: Option<Vec<ByotToolCallChunk>>,

    #[serde(default)]
    pub function_call: Option<ByotFunctionCallStream>,
}

#[derive(Debug, Clone, Deserialize)]
struct ByotToolCallChunk {
    pub index: u32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<ByotFunctionCallStream>,
}

#[derive(Debug, Clone, Deserialize)]
struct ByotFunctionCallStream {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

pub(super) fn chat_stream_chunk_from_byot(
    resp: ByotCreateChatCompletionStreamResponse,
) -> ChatStreamChunk {
    let mut content_delta = None;
    let mut thinking_delta = None;
    let mut tool_calls = Vec::new();
    let mut finish_reason = None;

    if let Some(choice) = resp.choices.into_iter().next() {
        content_delta = choice.delta.content;
        thinking_delta = choice
            .delta
            .reasoning_content
            .or(choice.delta.thinking)
            .or(choice.delta.reasoning);
        finish_reason = choice.finish_reason;

        if let Some(calls) = choice.delta.tool_calls {
            tool_calls = calls
                .into_iter()
                .map(|c| ToolCallDelta {
                    index: c.index,
                    id: c.id,
                    name: c
                        .function
                        .as_ref()
                        .and_then(|f| f.name.clone())
                        .map(|name| to_internal_tool_name(&name).to_string()),
                    arguments: c.function.as_ref().and_then(|f| f.arguments.clone()),
                })
                .collect();
        } else if let Some(function_call) = choice.delta.function_call {
            tool_calls = vec![ToolCallDelta {
                index: 0,
                id: None,
                name: function_call
                    .name
                    .map(|name| to_internal_tool_name(&name).to_string()),
                arguments: function_call.arguments,
            }];
        }
    }

    ChatStreamChunk {
        id: resp.id,
        created: resp.created,
        model: resp.model,
        content_delta,
        thinking_delta,
        tool_calls,
        finish_reason,
        usage: resp.usage.as_ref().map(token_usage_from_openai),
        provider_metadata: None,
    }
}

pub(super) fn token_usage_from_openai(usage: &CompletionUsage) -> TokenUsage {
    let cached = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cached_tokens)
        .unwrap_or(0);
    TokenUsage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        cached_tokens: (cached > 0).then_some(cached),
    }
}
