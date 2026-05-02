use async_openai::error::OpenAIError;

use crate::llm::{LlmClient, LlmError};
use crate::protocol::{ChatRequest, Message, UserContentPart, UserMessageContent};

pub const SUMMARIZATION_PROMPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/compact/prompt.md"
));
pub const SUMMARY_PREFIX: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/compact/summary_prefix.md"
));

const APPROX_BYTES_PER_TOKEN: usize = 4;
const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;

pub fn approx_token_count(text: &str) -> usize {
    let len = text.len();
    len.saturating_add(APPROX_BYTES_PER_TOKEN.saturating_sub(1)) / APPROX_BYTES_PER_TOKEN
}

fn approx_bytes_for_tokens(tokens: usize) -> usize {
    tokens.saturating_mul(APPROX_BYTES_PER_TOKEN)
}

fn approx_tokens_from_byte_count(bytes: usize) -> u64 {
    let bytes_u64 = bytes as u64;
    bytes_u64.saturating_add((APPROX_BYTES_PER_TOKEN as u64).saturating_sub(1))
        / (APPROX_BYTES_PER_TOKEN as u64)
}

pub fn estimate_context_tokens(messages: &[Message]) -> usize {
    let mut total = 0usize;
    for msg in messages {
        match msg {
            Message::Developer { content } | Message::System { content } => {
                total = total.saturating_add(approx_token_count(content));
            }
            Message::User { content } => match content {
                UserMessageContent::Text(text) => {
                    total = total.saturating_add(approx_token_count(text));
                }
                UserMessageContent::Parts(parts) => {
                    let mut saw_text = false;
                    for part in parts {
                        match part {
                            UserContentPart::Text { text } => {
                                saw_text = true;
                                total = total.saturating_add(approx_token_count(text));
                            }
                            UserContentPart::Image { .. } => {}
                        }
                    }
                    if !saw_text {
                        total = total.saturating_add(1);
                    }
                }
            },
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if let Some(text) = content.as_ref() {
                    total = total.saturating_add(approx_token_count(text));
                }
                for call in tool_calls {
                    total = total.saturating_add(approx_token_count(&call.name));
                    total = total.saturating_add(approx_token_count(&call.arguments));
                }
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                total = total.saturating_add(approx_token_count(tool_call_id));
                total = total.saturating_add(approx_token_count(content));
            }
        }
    }
    total
}

pub fn is_summary_message(text: &str) -> bool {
    let Some(rest) = text.strip_prefix(SUMMARY_PREFIX) else {
        return false;
    };
    rest.starts_with('\n')
}

pub fn summary_text(summary_suffix: &str) -> String {
    format!("{SUMMARY_PREFIX}\n{summary_suffix}")
}

pub fn collect_real_user_texts(messages: &[Message]) -> Vec<String> {
    let mut out = Vec::new();
    for msg in messages {
        let Message::User { content } = msg else {
            continue;
        };
        let text = user_content_to_text_for_compaction(content);
        if is_summary_message(&text) {
            continue;
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

fn user_content_to_text_for_compaction(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(text) => text.clone(),
        UserMessageContent::Parts(parts) => {
            let mut out = String::new();
            for part in parts {
                if let UserContentPart::Text { text } = part {
                    if !text.trim().is_empty() {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(text);
                    }
                }
            }
            if out.is_empty() {
                "[image]".to_string()
            } else {
                out
            }
        }
    }
}

pub fn build_compacted_user_history(
    user_messages: &[String],
    summary_suffix: &str,
) -> Vec<Message> {
    let mut selected_messages: Vec<String> = Vec::new();
    let mut remaining = COMPACT_USER_MESSAGE_MAX_TOKENS;

    if remaining > 0 {
        for message in user_messages.iter().rev() {
            if remaining == 0 {
                break;
            }
            let tokens = approx_token_count(message);
            if tokens <= remaining {
                selected_messages.push(message.clone());
                remaining = remaining.saturating_sub(tokens);
            } else {
                selected_messages.push(truncate_middle_with_token_budget(message, remaining));
                break;
            }
        }
        selected_messages.reverse();
    }

    let mut out: Vec<Message> = Vec::with_capacity(selected_messages.len().saturating_add(1));
    for message in selected_messages {
        if message.trim().is_empty() {
            continue;
        }
        out.push(Message::User {
            content: UserMessageContent::Text(message),
        });
    }
    out.push(Message::User {
        content: UserMessageContent::Text(summary_text(summary_suffix)),
    });
    out
}

fn truncate_middle_with_token_budget(s: &str, max_tokens: usize) -> String {
    if s.is_empty() {
        return String::new();
    }

    let max_bytes = approx_bytes_for_tokens(max_tokens);
    if max_bytes > 0 && s.len() <= max_bytes {
        return s.to_string();
    }

    let total_tokens = u64::try_from(approx_token_count(s)).unwrap_or(u64::MAX);
    if max_bytes == 0 {
        return format!("…{total_tokens} tokens truncated…");
    }

    let total_bytes = s.len();
    let left_budget = max_bytes / 2;
    let right_budget = max_bytes.saturating_sub(left_budget);
    let tail_start_target = total_bytes.saturating_sub(right_budget);

    let mut prefix_end = 0usize;
    let mut suffix_start = total_bytes;
    let mut suffix_started = false;

    for (idx, ch) in s.char_indices() {
        let char_end = idx + ch.len_utf8();
        if char_end <= left_budget {
            prefix_end = char_end;
            continue;
        }

        if idx >= tail_start_target {
            if !suffix_started {
                suffix_start = idx;
                suffix_started = true;
            }
            continue;
        }
    }

    if suffix_start < prefix_end {
        suffix_start = prefix_end;
    }

    let before = &s[..prefix_end];
    let after = &s[suffix_start..];
    let removed_bytes = total_bytes.saturating_sub(max_bytes);
    let removed_tokens = approx_tokens_from_byte_count(removed_bytes);
    let marker = format!("…{removed_tokens} tokens truncated…");
    format!("{before}{marker}{after}")
}

pub fn find_preamble_cutoff_id(messages: &[Message], message_ids: &[u64]) -> Option<u64> {
    let mut last = None;
    for (msg, id) in messages.iter().zip(message_ids) {
        match msg {
            Message::System { .. } | Message::Developer { .. } => {
                last = Some(*id);
            }
            _ => break,
        }
    }
    last
}

pub async fn run_compaction(llm: &LlmClient, messages: &[Message]) -> Result<String, LlmError> {
    let mut working: Vec<Message> = messages.to_vec();
    loop {
        let mut req_messages = working.clone();
        req_messages.push(Message::User {
            content: UserMessageContent::Text(SUMMARIZATION_PROMPT.to_string()),
        });

        match llm.chat(ChatRequest::new(req_messages)).await {
            Ok(resp) => {
                let summary = match resp.message {
                    Message::Assistant { content, .. } => content.unwrap_or_default(),
                    _ => String::new(),
                };
                return Ok(summary);
            }
            Err(err) => {
                if is_context_window_exceeded_error(&err) && drop_oldest_non_system(&mut working) {
                    continue;
                }
                return Err(err);
            }
        }
    }
}

fn drop_oldest_non_system(messages: &mut Vec<Message>) -> bool {
    let idx = messages
        .iter()
        .position(|m| !matches!(m, Message::System { .. } | Message::Developer { .. }));
    let Some(idx) = idx else {
        return false;
    };
    messages.remove(idx);
    true
}

fn is_context_window_exceeded_error(err: &LlmError) -> bool {
    let LlmError::OpenAI(OpenAIError::ApiError(api_err)) = err else {
        return false;
    };

    let msg = api_err.message.to_ascii_lowercase();
    msg.contains("context")
        && (msg.contains("window")
            || msg.contains("maximum context")
            || msg.contains("max context")
            || msg.contains("context length")
            || msg.contains("too long"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_summary_messages() {
        let summary = summary_text("hello");
        assert!(is_summary_message(&summary));
        assert!(!is_summary_message("hello"));
    }

    #[test]
    fn build_compacted_history_appends_summary_last() {
        let msgs = vec!["one".to_string(), "two".to_string()];
        let out = build_compacted_user_history(&msgs, "sum");
        assert!(
            matches!(out.last(), Some(Message::User { content: UserMessageContent::Text(t) }) if is_summary_message(t))
        );
    }
}
