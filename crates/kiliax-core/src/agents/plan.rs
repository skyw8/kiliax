use crate::tools::{builtin, Permissions, ShellPermissions};

use super::{AgentKind, AgentProfile};

pub(super) fn profile() -> AgentProfile {
    AgentProfile {
        kind: AgentKind::Plan,
        name: "plan",
        developer_prompt: PROMPT,
        tools: vec![
            builtin::read_file_tool_definition(),
            builtin::list_dir_tool_definition(),
            builtin::grep_files_tool_definition(),
            builtin::shell_command_tool_definition(),
            builtin::write_stdin_tool_definition(),
            builtin::update_plan_tool_definition(),
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

