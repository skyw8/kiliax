mod general;
mod plan;

use serde::{Deserialize, Serialize};

use crate::tools::builtin::BuiltinToolId;
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
    pub tool_ids: Vec<BuiltinToolId>,
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
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim() {
            "plan" => Some(Self::plan()),
            "general" => Some(Self::general()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_trims_whitespace() {
        let profile = AgentProfile::from_name("  general  ").unwrap();
        assert_eq!(profile.kind, AgentKind::General);
    }

    #[test]
    fn from_name_recognizes_plan() {
        let profile = AgentProfile::from_name("plan").unwrap();
        assert_eq!(profile.kind, AgentKind::Plan);
    }

    #[test]
    fn from_name_rejects_legacy_build_alias() {
        assert!(AgentProfile::from_name("build").is_none());
    }
}
