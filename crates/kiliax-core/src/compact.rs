use crate::history::sanitize_history_for_next_request;
use crate::llm::{LlmClient, LlmError};
use crate::protocol::{ChatRequest, Message, ToolCall, UserContentPart, UserMessageContent};

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
const COMPACT_TOOL_OUTPUT_MAX_TOKENS: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoCompactTokenSource {
    ProviderUsage,
    Estimate,
}

impl AutoCompactTokenSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderUsage => "provider_usage",
            Self::Estimate => "estimate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoCompactTokenCount {
    pub tokens: usize,
    pub source: AutoCompactTokenSource,
}

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
            Message::User { content, .. } => match content {
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
                            UserContentPart::File { .. } => {}
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

pub fn context_tokens_for_auto_compact(messages: &[Message]) -> AutoCompactTokenCount {
    for msg in messages.iter().rev() {
        if let Message::Assistant {
            usage: Some(usage), ..
        } = msg
        {
            return AutoCompactTokenCount {
                tokens: usage.prompt_tokens as usize,
                source: AutoCompactTokenSource::ProviderUsage,
            };
        }
    }

    AutoCompactTokenCount {
        tokens: estimate_context_tokens(messages),
        source: AutoCompactTokenSource::Estimate,
    }
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
        let Message::User { content, hidden } = msg else {
            continue;
        };
        if *hidden {
            continue;
        }
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
            hidden: false,
        });
    }
    out.push(Message::User {
        content: UserMessageContent::Text(summary_text(summary_suffix)),
        hidden: false,
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
    let mut working = prepare_messages_for_compaction(messages);
    loop {
        let mut req_messages = working.clone();
        req_messages.push(Message::User {
            content: UserMessageContent::Text(SUMMARIZATION_PROMPT.to_string()),
            hidden: false,
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
                if err.is_context_window_exceeded()
                    && drop_oldest_non_preamble_item_preserving_tool_pair(&mut working)
                {
                    continue;
                }
                return Err(err);
            }
        }
    }
}

fn prepare_messages_for_compaction(messages: &[Message]) -> Vec<Message> {
    let mut out = messages.to_vec();
    let _ = sanitize_history_for_next_request(&mut out);
    truncate_tool_outputs_for_compaction(&mut out);
    out
}

fn truncate_tool_outputs_for_compaction(messages: &mut [Message]) {
    for msg in messages {
        let Message::Tool { content, .. } = msg else {
            continue;
        };
        if approx_token_count(content) <= COMPACT_TOOL_OUTPUT_MAX_TOKENS {
            continue;
        }
        *content = truncate_middle_with_token_budget(content, COMPACT_TOOL_OUTPUT_MAX_TOKENS);
    }
}

fn drop_oldest_non_preamble_item_preserving_tool_pair(messages: &mut Vec<Message>) -> bool {
    let idx = messages
        .iter()
        .position(|m| !matches!(m, Message::System { .. } | Message::Developer { .. }));
    let Some(idx) = idx else {
        return false;
    };

    let removed = messages.remove(idx);
    match removed {
        Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
            remove_matching_tool_results_at(messages, idx, &tool_calls);
        }
        Message::Tool { tool_call_id, .. } => {
            remove_matching_assistant_tool_call(messages, &tool_call_id);
        }
        _ => {}
    }
    true
}

fn remove_matching_tool_results_at(
    messages: &mut Vec<Message>,
    start_idx: usize,
    calls: &[ToolCall],
) {
    let expected: std::collections::HashSet<&str> = calls.iter().map(|c| c.id.as_str()).collect();
    if expected.is_empty() {
        return;
    }

    let mut idx = start_idx;
    while idx < messages.len() {
        match &messages[idx] {
            Message::Tool { tool_call_id, .. } if expected.contains(tool_call_id.as_str()) => {
                messages.remove(idx);
            }
            Message::Tool { .. } => {
                idx += 1;
            }
            _ => break,
        }
    }
}

fn remove_matching_assistant_tool_call(messages: &mut Vec<Message>, tool_call_id: &str) {
    let Some(idx) = messages.iter().position(|m| {
        matches!(m, Message::Assistant { tool_calls, .. } if tool_calls.iter().any(|c| c.id == tool_call_id))
    }) else {
        return;
    };
    let removed = messages.remove(idx);
    if let Message::Assistant { tool_calls, .. } = removed {
        remove_matching_tool_results_at(messages, idx, &tool_calls);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{TokenUsage, ToolCall};

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
            matches!(out.last(), Some(Message::User { content: UserMessageContent::Text(t), .. }) if is_summary_message(t))
        );
    }

    fn assistant_with_calls(ids: &[&str]) -> Message {
        Message::Assistant {
            content: None,
            reasoning_content: None,
            tool_calls: ids
                .iter()
                .map(|id| ToolCall {
                    id: (*id).to_string(),
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                })
                .collect(),
            usage: None,
            provider_metadata: None,
        }
    }

    fn assistant_with_usage(prompt_tokens: u32, cached_tokens: Option<u32>) -> Message {
        Message::Assistant {
            content: Some("done".to_string()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: Some(TokenUsage {
                prompt_tokens,
                completion_tokens: 7,
                total_tokens: prompt_tokens + 7,
                cached_tokens,
            }),
            provider_metadata: None,
        }
    }

    fn tool(id: &str, content: &str) -> Message {
        Message::Tool {
            tool_call_id: id.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn auto_compact_tokens_prefer_provider_prompt_tokens() {
        let messages = vec![
            Message::User {
                content: UserMessageContent::Text("short".to_string()),
                hidden: false,
            },
            assistant_with_usage(410_426, Some(410_368)),
        ];

        let count = context_tokens_for_auto_compact(&messages);

        assert_eq!(
            count,
            AutoCompactTokenCount {
                tokens: 410_426,
                source: AutoCompactTokenSource::ProviderUsage,
            }
        );
    }

    #[test]
    fn auto_compact_tokens_use_most_recent_provider_usage() {
        let messages = vec![
            assistant_with_usage(10, None),
            Message::User {
                content: UserMessageContent::Text("next".to_string()),
                hidden: false,
            },
            assistant_with_usage(20, None),
        ];

        let count = context_tokens_for_auto_compact(&messages);

        assert_eq!(count.tokens, 20);
        assert_eq!(count.source, AutoCompactTokenSource::ProviderUsage);
    }

    #[test]
    fn auto_compact_tokens_fall_back_to_estimate_without_usage() {
        let messages = vec![Message::User {
            content: UserMessageContent::Text("12345".to_string()),
            hidden: false,
        }];

        let count = context_tokens_for_auto_compact(&messages);

        assert_eq!(
            count,
            AutoCompactTokenCount {
                tokens: estimate_context_tokens(&messages),
                source: AutoCompactTokenSource::Estimate,
            }
        );
    }

    #[test]
    fn prepare_messages_for_compaction_reorders_tool_results() {
        let messages = vec![
            assistant_with_calls(&["a", "b"]),
            tool("b", "B"),
            tool("a", "A"),
        ];

        let prepared = prepare_messages_for_compaction(&messages);

        assert!(matches!(
            prepared.get(1),
            Some(Message::Tool { tool_call_id, content }) if tool_call_id == "a" && content == "A"
        ));
        assert!(matches!(
            prepared.get(2),
            Some(Message::Tool { tool_call_id, content }) if tool_call_id == "b" && content == "B"
        ));
    }

    #[test]
    fn prepare_messages_for_compaction_inserts_missing_tool_result() {
        let messages = vec![
            assistant_with_calls(&["x"]),
            Message::User {
                content: UserMessageContent::Text("next".to_string()),
                hidden: false,
            },
        ];

        let prepared = prepare_messages_for_compaction(&messages);

        assert!(matches!(
            prepared.get(1),
            Some(Message::Tool { tool_call_id, content })
                if tool_call_id == "x" && content.contains("missing tool response")
        ));
    }

    #[test]
    fn prepare_messages_for_compaction_truncates_long_tool_output() {
        let long = "0123456789".repeat(COMPACT_TOOL_OUTPUT_MAX_TOKENS);
        let messages = vec![assistant_with_calls(&["x"]), tool("x", &long)];

        let prepared = prepare_messages_for_compaction(&messages);

        let Message::Tool { content, .. } = &prepared[1] else {
            panic!("expected tool message");
        };
        assert!(content.len() < long.len());
        assert!(content.contains("tokens truncated"));
    }

    #[test]
    fn drop_oldest_non_preamble_item_removes_tool_call_pair() {
        let mut messages = vec![
            Message::System {
                content: "sys".to_string(),
            },
            assistant_with_calls(&["a", "b"]),
            tool("a", "A"),
            tool("b", "B"),
            Message::User {
                content: UserMessageContent::Text("keep".to_string()),
                hidden: false,
            },
        ];

        assert!(drop_oldest_non_preamble_item_preserving_tool_pair(
            &mut messages
        ));

        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0], Message::System { .. }));
        assert!(matches!(messages[1], Message::User { .. }));
    }

    #[test]
    fn drop_oldest_non_preamble_item_drops_orphan_tool() {
        let mut messages = vec![
            Message::System {
                content: "sys".to_string(),
            },
            tool("orphan", "old"),
            Message::User {
                content: UserMessageContent::Text("keep".to_string()),
                hidden: false,
            },
        ];

        assert!(drop_oldest_non_preamble_item_preserving_tool_pair(
            &mut messages
        ));

        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0], Message::System { .. }));
        assert!(matches!(messages[1], Message::User { .. }));
    }
}
