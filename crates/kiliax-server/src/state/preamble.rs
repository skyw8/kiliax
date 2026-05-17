use std::path::PathBuf;

use kiliax_core::agents::AgentProfile;
use kiliax_core::protocol::Message as CoreMessage;
use kiliax_core::tools::ToolEngine;

pub(super) fn replace_preamble(messages: &mut Vec<CoreMessage>, new_preamble: Vec<CoreMessage>) {
    let end = messages
        .iter()
        .position(|m| !matches!(m, CoreMessage::System { .. }))
        .unwrap_or(messages.len());
    messages.splice(0..end, new_preamble);
}

pub(super) fn replace_preamble_with_ids(
    messages: &mut Vec<CoreMessage>,
    message_ids: &mut Vec<u64>,
    last_seq: &mut u64,
    new_preamble: Vec<CoreMessage>,
) {
    let end = messages
        .iter()
        .position(|m| !matches!(m, CoreMessage::System { .. }))
        .unwrap_or(messages.len());
    let new_len = new_preamble.len();

    messages.splice(0..end, new_preamble);

    let mut replacement_ids = Vec::with_capacity(new_len);
    for idx in 0..new_len {
        if idx < end && idx < message_ids.len() {
            replacement_ids.push(message_ids[idx]);
        } else {
            *last_seq += 1;
            replacement_ids.push(*last_seq);
        }
    }
    message_ids.splice(0..end.min(message_ids.len()), replacement_ids);
}

pub(super) async fn build_preamble(
    profile: &AgentProfile,
    model_id: &str,
    workspace_root: &PathBuf,
    tools: &ToolEngine,
    skills_config: &kiliax_core::config::SkillsConfig,
) -> Vec<CoreMessage> {
    let mut builder = kiliax_core::prompt::PromptBuilder::for_agent(profile)
        .with_tools({
            kiliax_core::tools::policy::tool_definitions_for_agent(profile, tools, model_id).await
        })
        .with_model_id(model_id.to_string())
        .with_workspace_root(workspace_root);
    let discovered = kiliax_core::tools::skills::discover_skills(workspace_root);
    let filtered = discovered.items.into_iter().filter(|s| {
        skills_config
            .overrides
            .get(&s.id)
            .copied()
            .unwrap_or(skills_config.default_enable)
    });
    builder = builder.add_skills(filtered);
    builder.build()
}

#[cfg(test)]
mod tests {
    use kiliax_core::protocol::{Message, UserMessageContent};

    use super::replace_preamble_with_ids;

    #[test]
    fn replace_preamble_removes_old_system_messages_and_keeps_ids_aligned() {
        let mut messages = vec![
            Message::System {
                content: "old plan prompt".to_string(),
            },
            Message::System {
                content: "old tools".to_string(),
            },
            Message::User {
                content: UserMessageContent::Text("hello".to_string()),
                hidden: false,
            },
        ];
        let mut ids = vec![1, 2, 3];
        let mut last_seq = 3;

        replace_preamble_with_ids(
            &mut messages,
            &mut ids,
            &mut last_seq,
            vec![Message::System {
                content: "new reviewer prompt".to_string(),
            }],
        );

        assert_eq!(messages.len(), ids.len());
        assert_eq!(ids, vec![1, 3]);
        assert!(matches!(&messages[0], Message::System { content } if content == "new reviewer prompt"));
        assert!(matches!(&messages[1], Message::User { .. }));
        assert_eq!(last_seq, 3);
    }
}
