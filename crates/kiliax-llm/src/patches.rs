use async_openai::error::OpenAIError;

use crate::types::Message;
use crate::{ProviderApi, ProviderRoute};

// WHY: Several OpenAI-compatible thinking APIs reject requests unless every assistant tool-call
// message includes `reasoning_content`. Apply the compatibility field broadly for chat-completions
// compatible providers, but avoid official OpenAI chat-completions endpoints because
// `reasoning_content` is not part of the standard OpenAI message schema.
pub(super) fn should_inject_reasoning_content(route: &ProviderRoute) -> bool {
    if route.api != ProviderApi::OpenAiChatCompletions {
        return false;
    }

    let provider = route.provider.to_ascii_lowercase();
    let base_url = route.base_url.to_ascii_lowercase();
    !(provider == "openai" && base_url.contains("api.openai.com"))
}

pub(super) fn inject_prompt_cache_fields(
    body: &mut serde_json::Value,
    prompt_cache_key: Option<&str>,
) {
    let Some(prompt_cache_key) = prompt_cache_key.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };

    let Some(obj) = body.as_object_mut() else {
        return;
    };

    obj.insert(
        "prompt_cache_key".to_string(),
        serde_json::Value::String(prompt_cache_key.to_string()),
    );
}

pub(super) fn inject_reasoning_content_for_tool_calls(
    body: &mut serde_json::Value,
    messages: &[Message],
) {
    // WHY: Some thinking-mode providers require a non-empty `reasoning_content` on *every*
    // assistant message that contains tool calls. Even when we don't have reasoning text, we send
    // a single whitespace (`" "`) to avoid gateways treating `""` as "missing".
    let Some(body_messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return;
    };

    for (idx, msg) in messages.iter().enumerate() {
        let Message::Assistant {
            reasoning_content,
            tool_calls,
            ..
        } = msg
        else {
            continue;
        };

        if tool_calls.is_empty() {
            continue;
        }

        let Some(obj) = body_messages.get_mut(idx).and_then(|v| v.as_object_mut()) else {
            continue;
        };

        let reasoning_ok = match obj.get("reasoning_content") {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::String(s)) => !s.trim().is_empty(),
            Some(_) => true,
        };
        if reasoning_ok {
            continue;
        }

        obj.insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(
                reasoning_content
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(" ")
                    .to_string(),
            ),
        );
    }

    // Safety pass: patch the exact outbound JSON, even if internal and serialized message indices drift.
    // WHY: The provider error references the *serialized* `messages[]` array index; patching the outbound
    // JSON directly prevents hard 400s even if internal message indexing ever diverges.
    for msg in body_messages {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };

        if obj.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }

        let has_tool_calls = obj
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .is_some_and(|calls| !calls.is_empty())
            || obj.get("function_call").is_some();
        if !has_tool_calls {
            continue;
        }

        let reasoning_ok = match obj.get("reasoning_content") {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::String(s)) => !s.trim().is_empty(),
            Some(_) => true,
        };
        if reasoning_ok {
            continue;
        }

        obj.insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(" ".to_string()),
        );
    }
}

pub(super) fn is_reasoning_content_missing_error(err: &OpenAIError) -> bool {
    // WHY: Some providers surface this as a structured API error, others as a stream setup error string.
    let msg = match err {
        OpenAIError::ApiError(api) => api.message.as_str(),
        OpenAIError::StreamError(text) => text.as_str(),
        _ => return false,
    };

    let msg = msg.to_ascii_lowercase();
    msg.contains("reasoning_content")
        && (msg.contains("missing")
            || msg.contains("must be passed back")
            || msg.contains("must be provided")
            || msg.contains("required"))
}
