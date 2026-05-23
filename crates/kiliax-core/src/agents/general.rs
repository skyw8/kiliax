use std::collections::BTreeSet;

use crate::tools::{builtin, Permissions, ShellPermissions};

use super::{AgentKind, AgentProfile, AgentSource, AgentToolFilter, AgentToolset};

pub(super) fn profile() -> AgentProfile {
    AgentProfile {
        kind: AgentKind::General,
        source: AgentSource::Builtin,
        name: "general".to_string(),
        subagent: true,
        display_name: Some("General".to_string()),
        description: Some("General-purpose agent.".to_string()),
        developer_prompt: PROMPT.to_string(),
        tools: AgentToolFilter::builtin_with_toolsets(
            vec![
                builtin::BuiltinToolId::ReadFile,
                builtin::BuiltinToolId::ListDir,
                builtin::BuiltinToolId::GrepFiles,
                builtin::BuiltinToolId::ViewImage,
                builtin::BuiltinToolId::WebSearch,
                builtin::BuiltinToolId::ShellCommand,
                builtin::BuiltinToolId::WriteStdin,
                builtin::BuiltinToolId::WriteFile,
                builtin::BuiltinToolId::EditFile,
                builtin::BuiltinToolId::ApplyPatch,
                builtin::BuiltinToolId::UpdatePlan,
            ],
            BTreeSet::from([AgentToolset::Goal]),
        ),
        permissions: permissions(),
        runtime: None,
    }
}

fn permissions() -> Permissions {
    Permissions {
        file_read: true,
        file_write: true,
        shell: ShellPermissions::AllowAll,
    }
}

const PROMPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/general.md"));
