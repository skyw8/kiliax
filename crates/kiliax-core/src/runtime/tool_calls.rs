use crate::protocol::{Message, ToolCall};
use crate::tools::{tool_parallelism, ToolParallelism};

#[derive(Debug, Clone, Copy)]
pub(super) enum ToolCallGroup<'a> {
    Exclusive(&'a ToolCall),
    Parallel(&'a [ToolCall]),
}

pub(super) fn group_tool_calls(tool_calls: &[ToolCall]) -> Vec<ToolCallGroup<'_>> {
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

pub(super) fn sanitize_tool_call_history(messages: &mut Vec<Message>) {
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

pub(super) fn normalize_tool_call_ids(step: usize, tool_calls: &mut Vec<ToolCall>) {
    let mut used: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(tool_calls.len());

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
