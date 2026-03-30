use crate::tools::{builtin, Permissions, ShellPermissions};

use super::{AgentKind, AgentProfile};

pub(super) fn profile() -> AgentProfile {
    AgentProfile {
        kind: AgentKind::Plan,
        name: "plan",
        developer_prompt: PROMPT,
        tool_ids: vec![
            builtin::BuiltinToolId::ReadFile,
            builtin::BuiltinToolId::ListDir,
            builtin::BuiltinToolId::GrepFiles,
            builtin::BuiltinToolId::ViewImage,
            builtin::BuiltinToolId::WebSearch,
            builtin::BuiltinToolId::ShellCommand,
            builtin::BuiltinToolId::WriteStdin,
            builtin::BuiltinToolId::UpdatePlan,
        ],
        permissions: permissions(),
    }
}

fn permissions() -> Permissions {
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

const PROMPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/plan.md"));
