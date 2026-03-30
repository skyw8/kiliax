use crate::tools::{builtin, Permissions, ShellPermissions};

use super::{AgentKind, AgentProfile};

pub(super) fn profile() -> AgentProfile {
    AgentProfile {
        kind: AgentKind::General,
        name: "general",
        developer_prompt: PROMPT,
        tool_ids: vec![
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
        permissions: permissions(),
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
