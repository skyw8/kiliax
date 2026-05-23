use crate::agents::{AgentProfile, AgentToolset};
use crate::protocol::ToolDefinition;
use crate::tools::builtin::{BuiltinToolId, TOOL_APPLY_PATCH, TOOL_EDIT_FILE, TOOL_WRITE_FILE};
use crate::tools::ToolEngine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileEditTools {
    ApplyPatch,
    WriteEdit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolPolicy {
    file_edit_tools: FileEditTools,
}

impl ToolPolicy {
    pub fn for_model_id(model_id: &str) -> Self {
        if is_gpt5_family(model_id) {
            Self {
                file_edit_tools: FileEditTools::ApplyPatch,
            }
        } else {
            Self {
                file_edit_tools: FileEditTools::WriteEdit,
            }
        }
    }

    pub fn allows_tool_name(&self, tool_name: &str) -> bool {
        if tool_name.starts_with("mcp__") {
            return true;
        }
        if tool_name.starts_with("custom__") {
            return true;
        }

        match self.file_edit_tools {
            FileEditTools::ApplyPatch => !matches!(tool_name, TOOL_WRITE_FILE | TOOL_EDIT_FILE),
            FileEditTools::WriteEdit => tool_name != TOOL_APPLY_PATCH,
        }
    }

    pub fn denial_message(&self, tool_name: &str) -> Option<&'static str> {
        match self.file_edit_tools {
            FileEditTools::ApplyPatch => match tool_name {
                TOOL_WRITE_FILE | TOOL_EDIT_FILE => {
                    Some("tool is disabled for this model; use apply_patch to edit files")
                }
                _ => None,
            },
            FileEditTools::WriteEdit => match tool_name {
                TOOL_APPLY_PATCH => {
                    Some("tool is disabled for this model; use write_file/edit_file to edit files")
                }
                _ => None,
            },
        }
    }

    pub fn allows_builtin(&self, tool: BuiltinToolId) -> bool {
        match self.file_edit_tools {
            FileEditTools::ApplyPatch => {
                !matches!(tool, BuiltinToolId::WriteFile | BuiltinToolId::EditFile)
            }
            FileEditTools::WriteEdit => tool != BuiltinToolId::ApplyPatch,
        }
    }
}

pub async fn tool_definitions_for_agent(
    profile: &AgentProfile,
    tools: &ToolEngine,
    model_id: &str,
) -> Vec<ToolDefinition> {
    let policy = ToolPolicy::for_model_id(model_id);
    let mut out: Vec<ToolDefinition> = profile
        .tools
        .builtin
        .iter()
        .copied()
        .filter(|id| policy.allows_builtin(*id))
        .map(BuiltinToolId::definition)
        .collect();

    if profile.tools.toolsets.contains(&AgentToolset::MultiAgent) && tools.multi_agent_available() {
        out.extend(crate::tools::builtin::multi_agents::tool_definitions());
    }

    out.extend(
        tools
            .extra_tool_definitions()
            .await
            .into_iter()
            .filter(|def| allows_extra_tool(profile, &def.name)),
    );
    out
}

fn allows_extra_tool(profile: &AgentProfile, tool_name: &str) -> bool {
    if let Some((server, _tool)) = crate::tools::mcp::McpHub::parse_exposed_tool_name(tool_name) {
        return profile.tools.mcp.allows(server);
    }
    if let Some(name) = crate::tools::custom::parse_exposed_tool_name(tool_name) {
        return profile.tools.custom.allows(name);
    }
    false
}

fn is_gpt5_family(model_id: &str) -> bool {
    let model = model_id.rsplit('/').next().unwrap_or(model_id).trim();
    let model = model.to_ascii_lowercase();
    model.starts_with("gpt-5")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::agents::{AgentKind, AgentProfile, AgentSource, AgentToolFilter, ToolAllow};
    use crate::config::AgentRuntimeConfig;
    use crate::tools::{Permissions, ShellPermissions};

    use super::allows_extra_tool;

    fn profile(custom: ToolAllow, mcp: ToolAllow) -> AgentProfile {
        AgentProfile {
            kind: AgentKind::Custom,
            source: AgentSource::Custom,
            name: "custom".to_string(),
            display_name: None,
            description: None,
            developer_prompt: "prompt".to_string(),
            tools: AgentToolFilter::custom(Vec::new(), BTreeSet::new(), mcp, custom),
            permissions: Permissions {
                file_read: true,
                file_write: false,
                shell: ShellPermissions::DenyAll,
            },
            runtime: Option::<AgentRuntimeConfig>::None,
        }
    }

    #[test]
    fn custom_agent_filters_custom_tools() {
        let allowed = ToolAllow::Only(BTreeSet::from(["alert_ubuntu".to_string()]));
        let profile = profile(allowed, ToolAllow::None);
        assert!(allows_extra_tool(&profile, "custom__alert_ubuntu"));
        assert!(!allows_extra_tool(&profile, "custom__repo_stats"));
    }

    #[test]
    fn custom_agent_filters_mcp_servers() {
        let allowed = ToolAllow::Only(BTreeSet::from(["github".to_string()]));
        let profile = profile(ToolAllow::None, allowed);
        assert!(allows_extra_tool(&profile, "mcp__github__create_issue"));
        assert!(!allows_extra_tool(&profile, "mcp__figma__inspect"));
    }
}
