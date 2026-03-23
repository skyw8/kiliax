use crate::tools::{builtin, Permissions, ShellPermissions};

use super::{AgentKind, AgentProfile};

pub(super) fn profile() -> AgentProfile {
    AgentProfile {
        kind: AgentKind::General,
        name: "general",
        developer_prompt: PROMPT,
        tools: vec![
            builtin::read_file_tool_definition(),
            builtin::list_dir_tool_definition(),
            builtin::grep_files_tool_definition(),
            builtin::view_image_tool_definition(),
            builtin::web_search_tool_definition(),
            builtin::shell_command_tool_definition(),
            builtin::write_stdin_tool_definition(),
            builtin::apply_patch_tool_definition(),
            builtin::update_plan_tool_definition(),
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

const PROMPT: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/general.md"));
