mod apply_patch;
mod common;
mod grep_files;
mod list_dir;
mod read_file;
mod shell;
mod update_plan;
mod view_image;
mod web_search;

pub use apply_patch::apply_patch_tool_definition;
pub use grep_files::grep_files_tool_definition;
pub use list_dir::list_dir_tool_definition;
pub use read_file::read_file_tool_definition;
pub use shell::{shell_command_tool_definition, write_stdin_tool_definition, ShellSessions};
pub use update_plan::update_plan_tool_definition;
pub use view_image::view_image_tool_definition;
pub use web_search::web_search_tool_definition;
pub(crate) use view_image::execute_with_attachment as execute_view_image_with_attachment;

use std::path::Path;

use crate::llm::ToolCall;
use crate::config::Config;
use crate::tools::{Permissions, ToolError};

pub const TOOL_READ_FILE: &str = "read_file";
pub const TOOL_LIST_DIR: &str = "list_dir";
pub const TOOL_GREP_FILES: &str = "grep_files";
pub const TOOL_VIEW_IMAGE: &str = "view_image";
pub const TOOL_SHELL_COMMAND: &str = "shell_command";
pub const TOOL_WRITE_STDIN: &str = "write_stdin";
pub const TOOL_APPLY_PATCH: &str = "apply_patch";
pub const TOOL_UPDATE_PLAN: &str = "update_plan";
pub const TOOL_WEB_SEARCH: &str = "web_search";

pub async fn execute(
    workspace_root: &Path,
    perms: &Permissions,
    shell_sessions: &ShellSessions,
    config: &Config,
    call: &ToolCall,
) -> Result<String, ToolError> {
    match call.name.as_str() {
        TOOL_READ_FILE => read_file::execute(workspace_root, perms, call).await,
        TOOL_LIST_DIR => list_dir::execute(workspace_root, perms, call).await,
        TOOL_GREP_FILES => grep_files::execute(workspace_root, perms, call).await,
        TOOL_VIEW_IMAGE => view_image::execute(workspace_root, perms, call).await,
        TOOL_SHELL_COMMAND => {
            shell::execute_shell_command(workspace_root, perms, shell_sessions, call).await
        }
        TOOL_WRITE_STDIN => shell::execute_write_stdin(perms, shell_sessions, call).await,
        TOOL_APPLY_PATCH => apply_patch::execute(workspace_root, perms, call).await,
        TOOL_UPDATE_PLAN => update_plan::execute(call),
        TOOL_WEB_SEARCH => web_search::execute(config, call).await,
        other => Err(ToolError::UnknownTool(other.to_string())),
    }
}
