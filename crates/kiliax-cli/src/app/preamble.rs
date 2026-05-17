use std::path::PathBuf;

use kiliax_core::agents::AgentProfile;
use kiliax_core::protocol::Message;

pub(super) fn replace_preamble(
    messages: &mut Vec<Message>,
    message_ids: &mut Vec<u64>,
    last_seq: &mut u64,
    new_preamble: Vec<Message>,
) {
    let end = messages
        .iter()
        .position(|m| !matches!(m, Message::System { .. }))
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
    project_prompt: Option<String>,
    tools: &kiliax_core::tools::ToolEngine,
    skills_config: &kiliax_core::config::SkillsConfig,
) -> Vec<Message> {
    let mut builder = kiliax_core::prompt::PromptBuilder::for_agent(profile)
        .with_tools({
            kiliax_core::tools::policy::tool_definitions_for_agent(profile, tools, model_id).await
        })
        .with_model_id(model_id.to_string())
        .with_workspace_root(workspace_root)
        .with_project_prompt(project_prompt);
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
