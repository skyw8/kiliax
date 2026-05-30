use crate::protocol::{Message, ToolCall};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SanitizeReport {
    pub dropped_empty_assistant: usize,
    pub dropped_orphan_tool: usize,
    pub inserted_missing_tool_result: usize,
}

impl SanitizeReport {
    pub fn changed(&self) -> bool {
        self.dropped_empty_assistant > 0
            || self.dropped_orphan_tool > 0
            || self.inserted_missing_tool_result > 0
    }
}

pub fn assistant_has_wire_output(content: &Option<String>, tool_calls: &[ToolCall]) -> bool {
    content.as_deref().is_some_and(|c| !c.trim().is_empty()) || !tool_calls.is_empty()
}

pub fn assistant_message_has_wire_output(message: &Message) -> bool {
    matches!(message, Message::Assistant { content, tool_calls, .. } if assistant_has_wire_output(content, tool_calls))
}

pub fn assistant_message_is_empty(message: &Message) -> bool {
    matches!(message, Message::Assistant { content, tool_calls, .. } if !assistant_has_wire_output(content, tool_calls))
}

pub fn sanitize_history_for_next_request(messages: &mut Vec<Message>) -> SanitizeReport {
    if messages.iter().all(|m| {
        !assistant_message_is_empty(m)
            && !matches!(m, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty())
            && !matches!(m, Message::Tool { .. })
    }) {
        return SanitizeReport::default();
    }

    let mut report = SanitizeReport::default();
    let mut queue: std::collections::VecDeque<Message> = std::mem::take(messages).into();
    let mut out: Vec<Message> = Vec::with_capacity(queue.len());

    while let Some(msg) = queue.pop_front() {
        match msg {
            Message::Assistant {
                content,
                reasoning_content,
                tool_calls,
                usage,
                provider_metadata,
            } if !tool_calls.is_empty() => {
                let expected_ids: Vec<String> = tool_calls.iter().map(|c| c.id.clone()).collect();
                out.push(Message::Assistant {
                    content,
                    reasoning_content,
                    tool_calls,
                    usage,
                    provider_metadata,
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
                        report.inserted_missing_tool_result += 1;
                        out.push(Message::Tool {
                            tool_call_id: expected_id,
                            content: "error: missing tool response message (repaired)".to_string(),
                        });
                    }
                }

                report.dropped_orphan_tool += remaining.into_iter().flatten().count();
                out.extend(segment_other_msgs);
            }
            Message::Assistant { .. } if assistant_message_is_empty(&msg) => {
                report.dropped_empty_assistant += 1;
            }
            Message::Tool { .. } => {
                report.dropped_orphan_tool += 1;
            }
            other => out.push(other),
        }
    }

    *messages = out;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::UserMessageContent;

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: "t".to_string(),
            arguments: "{}".to_string(),
        }
    }

    #[test]
    fn sanitize_history_reorders_tool_messages() {
        let mut messages = vec![
            Message::User {
                content: UserMessageContent::Text("hi".to_string()),
                hidden: false,
            },
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: vec![tool_call("a"), tool_call("b")],
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
        ];

        let report = sanitize_history_for_next_request(&mut messages);

        assert!(!report.changed());
        assert!(
            matches!(messages.get(2), Some(Message::Tool { tool_call_id, content }) if tool_call_id == "a" && content == "A")
        );
        assert!(
            matches!(messages.get(3), Some(Message::Tool { tool_call_id, content }) if tool_call_id == "b" && content == "B")
        );
    }

    #[test]
    fn sanitize_history_inserts_missing_tool_messages() {
        let mut messages = vec![
            Message::Assistant {
                content: Some("call tools".to_string()),
                reasoning_content: None,
                tool_calls: vec![tool_call("x")],
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

        let report = sanitize_history_for_next_request(&mut messages);

        assert_eq!(report.inserted_missing_tool_result, 1);
        assert!(
            matches!(messages.get(1), Some(Message::Tool { tool_call_id, .. }) if tool_call_id == "x")
        );
    }

    #[test]
    fn sanitize_history_moves_non_tool_messages_after_tool_messages() {
        let mut messages = vec![
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: vec![tool_call("a"), tool_call("b")],
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
        ];

        let report = sanitize_history_for_next_request(&mut messages);

        assert!(!report.changed());
        assert!(
            matches!(messages.get(1), Some(Message::Tool { tool_call_id, .. }) if tool_call_id == "a")
        );
        assert!(
            matches!(messages.get(2), Some(Message::Tool { tool_call_id, .. }) if tool_call_id == "b")
        );
        assert!(matches!(messages.get(3), Some(Message::User { .. })));
    }

    #[test]
    fn sanitize_history_drops_empty_assistant_messages() {
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

        let report = sanitize_history_for_next_request(&mut messages);

        assert_eq!(report.dropped_empty_assistant, 1);
        assert_eq!(messages.len(), 2);
        assert!(matches!(messages.first(), Some(Message::User { .. })));
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
    fn sanitize_history_drops_orphan_tool_messages() {
        let mut messages = vec![
            Message::Tool {
                tool_call_id: "orphan".to_string(),
                content: "unused".to_string(),
            },
            Message::User {
                content: UserMessageContent::Text("hi".to_string()),
                hidden: false,
            },
        ];

        let report = sanitize_history_for_next_request(&mut messages);

        assert_eq!(report.dropped_orphan_tool, 1);
        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0], Message::User { .. }));
    }
}
