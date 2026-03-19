use serde::{Deserialize, Serialize};

use crate::llm::ToolDefinition;
use crate::tools::{builtin, Permissions, ShellPermissions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Plan,
    Build,
}

#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub kind: AgentKind,
    pub name: &'static str,
    pub developer_prompt: &'static str,
    pub tools: Vec<ToolDefinition>,
    pub permissions: Permissions,
}

impl AgentProfile {
    pub fn plan() -> Self {
        Self {
            kind: AgentKind::Plan,
            name: "plan",
            developer_prompt: PLAN_PROMPT,
            tools: vec![
                builtin::read_tool_definition(),
                builtin::shell_tool_definition(),
            ],
            permissions: plan_permissions(),
        }
    }

    pub fn build() -> Self {
        Self {
            kind: AgentKind::Build,
            name: "build",
            developer_prompt: BUILD_PROMPT,
            tools: vec![
                builtin::read_tool_definition(),
                builtin::write_tool_definition(),
                builtin::shell_tool_definition(),
            ],
            permissions: build_permissions(),
        }
    }
}

fn plan_permissions() -> Permissions {
    Permissions {
        file_read: true,
        file_write: false,
        shell: ShellPermissions::AllowList(vec![
            vec!["ls".into()],
            vec!["cat".into()],
            vec!["rg".into()],
            vec!["find".into()],
            vec!["sed".into()],
            vec!["head".into()],
            vec!["tail".into()],
            vec!["pwd".into()],
            vec!["git".into(), "status".into()],
            vec!["git".into(), "diff".into()],
        ]),
    }
}

fn build_permissions() -> Permissions {
    Permissions {
        file_read: true,
        file_write: true,
        shell: ShellPermissions::AllowAll,
    }
}

const PLAN_PROMPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/plan.md"));
const BUILD_PROMPT: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/build.md"));
