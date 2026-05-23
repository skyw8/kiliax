mod custom;
mod explore;
mod general;
mod master;
mod plan;

pub use custom::CustomAgentDiscoveryError;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::AgentRuntimeConfig;
use crate::tools::builtin::BuiltinToolId;
use crate::tools::Permissions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Plan,
    General,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSource {
    Builtin,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAllow {
    All,
    None,
    Only(BTreeSet<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentToolset {
    Goal,
    MultiAgent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolFilter {
    pub builtin: Vec<BuiltinToolId>,
    pub toolsets: BTreeSet<AgentToolset>,
    pub mcp: ToolAllow,
    pub custom: ToolAllow,
}

#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub kind: AgentKind,
    pub source: AgentSource,
    pub name: String,
    pub subagent: bool,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub developer_prompt: String,
    pub tools: AgentToolFilter,
    pub permissions: Permissions,
    pub runtime: Option<AgentRuntimeConfig>,
}

impl AgentProfile {
    pub fn plan() -> Self {
        plan::profile()
    }

    pub fn explore() -> Self {
        explore::profile()
    }

    pub fn general() -> Self {
        general::profile()
    }

    pub fn master() -> Self {
        master::profile()
    }

    /// Map an agent name into a built-in or discovered custom profile.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        if let Some(profile) = custom::discover_custom_agents()
            .items
            .into_iter()
            .find(|profile| profile.name == name)
        {
            return Some(profile);
        }

        match name {
            "explore" => Some(Self::explore()),
            "plan" => Some(Self::plan()),
            "general" => Some(Self::general()),
            "master" => Some(Self::master()),
            _ => None,
        }
    }

    pub fn list_names() -> Vec<String> {
        Self::list_names_with_errors().0
    }

    pub fn list_names_with_errors() -> (Vec<String>, Vec<CustomAgentDiscoveryError>) {
        let mut out = vec![
            "explore".to_string(),
            "general".to_string(),
            "plan".to_string(),
            "master".to_string(),
        ];
        let discovered = custom::discover_custom_agents();
        out.extend(discovered.items.into_iter().map(|profile| profile.name));
        out.sort();
        out.dedup();
        (out, discovered.errors)
    }

    pub fn spawnable_subagents() -> Vec<Self> {
        let mut out = vec![
            Self::explore(),
            Self::general(),
            Self::master(),
            Self::plan(),
        ];
        out.extend(
            custom::discover_custom_agents()
                .items
                .into_iter()
                .filter(|profile| profile.subagent),
        );
        out.retain(|profile| profile.subagent);
        out
    }
}

impl AgentToolFilter {
    pub fn builtin_with_extra(tool_ids: Vec<BuiltinToolId>) -> Self {
        Self {
            builtin: tool_ids,
            toolsets: BTreeSet::new(),
            mcp: ToolAllow::All,
            custom: ToolAllow::All,
        }
    }

    pub fn builtin_with_toolsets(
        tool_ids: Vec<BuiltinToolId>,
        toolsets: BTreeSet<AgentToolset>,
    ) -> Self {
        Self {
            builtin: tool_ids,
            toolsets,
            mcp: ToolAllow::All,
            custom: ToolAllow::All,
        }
    }

    pub fn custom(
        tool_ids: Vec<BuiltinToolId>,
        toolsets: BTreeSet<AgentToolset>,
        mcp: ToolAllow,
        custom: ToolAllow,
    ) -> Self {
        Self {
            builtin: tool_ids,
            toolsets,
            mcp,
            custom,
        }
    }
}

impl ToolAllow {
    pub fn allows(&self, id: &str) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Only(ids) => ids.contains(id),
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
        assert_eq!(profile.name, "general");
    }

    #[test]
    fn from_name_recognizes_plan() {
        let profile = AgentProfile::from_name("plan").unwrap();
        assert_eq!(profile.kind, AgentKind::Plan);
        assert_eq!(profile.name, "plan");
        assert!(!profile.tools.toolsets.contains(&AgentToolset::Goal));
    }

    #[test]
    fn from_name_recognizes_explore() {
        let profile = AgentProfile::from_name("explore").unwrap();
        let plan = AgentProfile::plan();

        assert_eq!(profile.kind, AgentKind::Plan);
        assert_eq!(profile.name, "explore");
        assert!(profile.subagent);
        assert_eq!(profile.tools.builtin, plan.tools.builtin);
        assert_eq!(profile.permissions, plan.permissions);
        assert!(!profile.tools.toolsets.contains(&AgentToolset::Goal));
        assert!(!profile.tools.toolsets.contains(&AgentToolset::MultiAgent));
    }

    #[test]
    fn list_names_includes_explore() {
        assert!(AgentProfile::list_names().contains(&"explore".to_string()));
    }

    #[test]
    fn builtin_agents_are_spawnable_subagents() {
        let names = AgentProfile::spawnable_subagents()
            .into_iter()
            .map(|profile| profile.name)
            .collect::<BTreeSet<_>>();

        assert!(names.contains("explore"));
        assert!(names.contains("general"));
        assert!(names.contains("master"));
        assert!(names.contains("plan"));
    }

    #[test]
    fn from_name_recognizes_master() {
        let profile = AgentProfile::from_name("master").unwrap();
        assert_eq!(profile.kind, AgentKind::General);
        assert_eq!(profile.name, "master");
        assert!(profile.subagent);
        assert!(profile.tools.toolsets.contains(&AgentToolset::Goal));
        assert!(profile.tools.toolsets.contains(&AgentToolset::MultiAgent));
    }

    #[test]
    fn from_name_rejects_legacy_build_alias() {
        assert!(AgentProfile::from_name("build").is_none());
    }

    #[test]
    fn custom_tool_allowlist_allows_expected_ids() {
        let allow = ToolAllow::Only(["alert_ubuntu".to_string()].into_iter().collect());
        assert!(allow.allows("alert_ubuntu"));
        assert!(!allow.allows("repo_stats"));
    }
}
