use async_openai::types::CompletionUsage;
use serde::Deserialize;

use crate::types::{
    ChatResponse, ChatStreamChunk, FinishReason, Message, TokenUsage, ToolCall, ToolCallDelta,
};

use super::{tool_names::to_internal_tool_name, LlmError};

#[derive(Debug, Deserialize)]
pub(super) struct ByotCreateChatCompletionResponse {
    id: String,
    created: u32,
    model: String,
    #[serde(default)]
    choices: Vec<ByotChatChoice>,
    #[serde(default)]
    usage: Option<CompletionUsage>,
}

#[derive(Debug, Deserialize)]
struct ByotChatChoice {
    message: ByotChatCompletionMessage,
    #[serde(default)]
    finish_reason: Option<FinishReason>,
}

#[derive(Debug, Default, Deserialize)]
struct ByotChatCompletionMessage {
    #[serde(default)]
    content: Option<String>,

    #[serde(default, rename = "reasoning_content")]
    reasoning_content: Option<String>,

    #[serde(default)]
    thinking: Option<String>,

    #[serde(default)]
    reasoning: Option<String>,

    #[serde(default)]
    tool_calls: Option<Vec<ByotToolCall>>,

    #[serde(default)]
    function_call: Option<ByotFunctionCall>,
}

#[derive(Debug, Default, Deserialize)]
struct ByotToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ByotFunctionCall>,
}

#[derive(Debug, Default, Deserialize)]
struct ByotFunctionCall {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

impl ByotFunctionCall {
    fn into_tool_call(self, id: String) -> ToolCall {
        ToolCall {
            id,
            name: self
                .name
                .map(|name| to_internal_tool_name(&name).to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            arguments: self.arguments.unwrap_or_default(),
        }
    }

    fn into_delta(self, index: u32, id: Option<String>) -> ToolCallDelta {
        ToolCallDelta {
            index,
            id,
            name: self
                .name
                .map(|name| to_internal_tool_name(&name).to_string()),
            arguments: self.arguments,
        }
    }
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
            .map(|call| {
                call.function
                    .unwrap_or_default()
                    .into_tool_call(call.id.unwrap_or_default())
            })
            .collect()
    } else {
        function_call
            .map(|call| vec![call.into_tool_call(String::new())])
            .unwrap_or_default()
    };

    let reasoning_content = reasoning_content.or(thinking).or(reasoning);

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

#[derive(Debug, Deserialize)]
pub(super) struct ByotCreateChatCompletionStreamResponse {
    id: String,
    created: u32,
    model: String,
    #[serde(default)]
    choices: Vec<ByotChatChoiceStream>,
    #[serde(default)]
    usage: Option<CompletionUsage>,
}

#[derive(Debug, Deserialize)]
struct ByotChatChoiceStream {
    #[serde(default)]
    delta: ByotChatCompletionStreamDelta,
    #[serde(default)]
    finish_reason: Option<FinishReason>,
}

#[derive(Debug, Default, Deserialize)]
struct ByotChatCompletionStreamDelta {
    #[serde(default)]
    content: Option<String>,

    #[serde(default, rename = "reasoning_content")]
    reasoning_content: Option<String>,

    #[serde(default)]
    thinking: Option<String>,

    #[serde(default)]
    reasoning: Option<String>,

    #[serde(default)]
    tool_calls: Option<Vec<ByotToolCallChunk>>,

    #[serde(default)]
    function_call: Option<ByotFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct ByotToolCallChunk {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ByotFunctionCall>,
}

pub(super) fn chat_stream_chunk_from_byot(
    resp: ByotCreateChatCompletionStreamResponse,
) -> ChatStreamChunk {
    let (content_delta, thinking_delta, tool_calls, finish_reason) =
        if let Some(choice) = resp.choices.into_iter().next() {
            let delta = choice.delta;
            let tool_calls = if let Some(calls) = delta.tool_calls {
                calls
                    .into_iter()
                    .map(|call| {
                        call.function
                            .unwrap_or_default()
                            .into_delta(call.index, call.id)
                    })
                    .collect()
            } else {
                delta
                    .function_call
                    .map(|call| vec![call.into_delta(0, None)])
                    .unwrap_or_default()
            };

            (
                delta.content,
                delta
                    .reasoning_content
                    .or(delta.thinking)
                    .or(delta.reasoning),
                tool_calls,
                choice.finish_reason,
            )
        } else {
            (None, None, Vec::new(), None)
        };

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
