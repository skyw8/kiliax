use crate::agents::AgentProfile;
use crate::llm::ToolDefinition;
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

        match self.file_edit_tools {
            FileEditTools::ApplyPatch => !matches!(tool_name, TOOL_WRITE_FILE | TOOL_EDIT_FILE),
            FileEditTools::WriteEdit => tool_name != TOOL_APPLY_PATCH,
        }
    }

    pub fn denial_message(&self, tool_name: &str) -> Option<&'static str> {
        match self.file_edit_tools {
            FileEditTools::ApplyPatch => match tool_name {
                TOOL_WRITE_FILE | TOOL_EDIT_FILE => Some(
                    "tool is disabled for this model; use apply_patch to edit files",
                ),
                _ => None,
            },
            FileEditTools::WriteEdit => match tool_name {
                TOOL_APPLY_PATCH => Some(
                    "tool is disabled for this model; use write_file/edit_file to edit files",
                ),
                _ => None,
            },
        }
    }

    pub fn allows_builtin(&self, tool: BuiltinToolId) -> bool {
        match self.file_edit_tools {
            FileEditTools::ApplyPatch => !matches!(tool, BuiltinToolId::WriteFile | BuiltinToolId::EditFile),
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
        .tool_ids
        .iter()
        .copied()
        .filter(|id| policy.allows_builtin(*id))
        .map(BuiltinToolId::definition)
        .collect();

    out.extend(tools.extra_tool_definitions().await);
    out
}

fn is_gpt5_family(model_id: &str) -> bool {
    let model = model_id.rsplit('/').next().unwrap_or(model_id).trim();
    let model = model.to_ascii_lowercase();
    model.starts_with("gpt-5")
}

