mod general;
mod plan;

use serde::{Deserialize, Serialize};

use crate::llm::ToolDefinition;
use crate::tools::Permissions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Plan,
    General,
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
        plan::profile()
    }

    pub fn general() -> Self {
        general::profile()
    }

    /// Map an agent name into a built-in profile.
    ///
    /// Accepts legacy aliases (e.g. "build" -> "general") for backwards compatibility with
    /// persisted sessions and older CLIs.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim() {
            "plan" => Some(Self::plan()),
            "general" | "build" => Some(Self::general()),
            _ => None,
        }
    }
}

