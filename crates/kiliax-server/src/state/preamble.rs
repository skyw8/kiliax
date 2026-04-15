use std::collections::HashSet;
use std::path::PathBuf;

use kiliax_core::agents::AgentProfile;
use kiliax_core::protocol::Message as CoreMessage;
use kiliax_core::tools::ToolEngine;

pub(super) fn preamble_updates(
    messages: &[CoreMessage],
    new_preamble: Vec<CoreMessage>,
) -> Vec<CoreMessage> {
    const HEADER: &str =
        "Session update: the following system messages override earlier system context.";

    let mut seen: HashSet<String> = messages
        .iter()
        .filter_map(|m| match m {
            CoreMessage::System { content } => Some(content.clone()),
            _ => None,
        })
        .collect();
    let header_seen = seen.contains(HEADER);

    let mut updates: Vec<CoreMessage> = Vec::new();
    for msg in new_preamble {
        let CoreMessage::System { content } = &msg else {
            continue;
        };
        if seen.insert(content.clone()) {
            updates.push(msg);
        }
    }

    if updates.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(updates.len().saturating_add(1));
    if !header_seen {
        out.push(CoreMessage::System {
            content: HEADER.to_string(),
        });
    }
    out.extend(updates);
    out
}

pub(super) fn insert_preamble_updates_before_last_user(
    messages: &mut Vec<CoreMessage>,
    new_preamble: Vec<CoreMessage>,
) {
    let updates = preamble_updates(messages.as_slice(), new_preamble);
    if updates.is_empty() {
        return;
    }
    let idx = messages
        .iter()
        .rposition(|m| matches!(m, CoreMessage::User { .. }))
        .unwrap_or(messages.len());
    messages.splice(idx..idx, updates);
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
