use crate::protocol::ToolDefinition;

use super::{
    apply_patch_tool_definition, edit_file_tool_definition, get_goal_tool_definition,
    grep_files_tool_definition, list_dir_tool_definition, read_file_tool_definition,
    shell_command_tool_definition, update_goal_tool_definition, update_plan_tool_definition,
    view_image_tool_definition, web_search_tool_definition, write_file_tool_definition,
    write_stdin_tool_definition, TOOL_APPLY_PATCH, TOOL_EDIT_FILE, TOOL_GET_GOAL, TOOL_GREP_FILES,
    TOOL_LIST_DIR, TOOL_READ_FILE, TOOL_SHELL_COMMAND, TOOL_UPDATE_GOAL, TOOL_UPDATE_PLAN,
    TOOL_VIEW_IMAGE, TOOL_WEB_SEARCH, TOOL_WRITE_FILE, TOOL_WRITE_STDIN,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinToolId {
    ReadFile,
    ListDir,
    GrepFiles,
    ViewImage,
    WebSearch,
    ShellCommand,
    WriteStdin,
    WriteFile,
    EditFile,
    ApplyPatch,
    UpdatePlan,
    GetGoal,
    UpdateGoal,
}

impl BuiltinToolId {
    pub const ALL: [Self; 13] = [
        Self::ReadFile,
        Self::ListDir,
        Self::GrepFiles,
        Self::ViewImage,
        Self::WebSearch,
        Self::ShellCommand,
        Self::WriteStdin,
        Self::WriteFile,
        Self::EditFile,
        Self::ApplyPatch,
        Self::UpdatePlan,
        Self::GetGoal,
        Self::UpdateGoal,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::ReadFile => TOOL_READ_FILE,
            Self::ListDir => TOOL_LIST_DIR,
            Self::GrepFiles => TOOL_GREP_FILES,
            Self::ViewImage => TOOL_VIEW_IMAGE,
            Self::WebSearch => TOOL_WEB_SEARCH,
            Self::ShellCommand => TOOL_SHELL_COMMAND,
            Self::WriteStdin => TOOL_WRITE_STDIN,
            Self::WriteFile => TOOL_WRITE_FILE,
            Self::EditFile => TOOL_EDIT_FILE,
            Self::ApplyPatch => TOOL_APPLY_PATCH,
            Self::UpdatePlan => TOOL_UPDATE_PLAN,
            Self::GetGoal => TOOL_GET_GOAL,
            Self::UpdateGoal => TOOL_UPDATE_GOAL,
        }
    }

    pub fn definition(self) -> ToolDefinition {
        match self {
            Self::ReadFile => read_file_tool_definition(),
            Self::ListDir => list_dir_tool_definition(),
            Self::GrepFiles => grep_files_tool_definition(),
            Self::ViewImage => view_image_tool_definition(),
            Self::WebSearch => web_search_tool_definition(),
            Self::ShellCommand => shell_command_tool_definition(),
            Self::WriteStdin => write_stdin_tool_definition(),
            Self::WriteFile => write_file_tool_definition(),
            Self::EditFile => edit_file_tool_definition(),
            Self::ApplyPatch => apply_patch_tool_definition(),
            Self::UpdatePlan => update_plan_tool_definition(),
            Self::GetGoal => get_goal_tool_definition(),
            Self::UpdateGoal => update_goal_tool_definition(),
        }
    }
}

pub fn builtin_tool_id_by_name(name: &str) -> Option<BuiltinToolId> {
    match name {
        TOOL_READ_FILE => Some(BuiltinToolId::ReadFile),
        TOOL_LIST_DIR => Some(BuiltinToolId::ListDir),
        TOOL_GREP_FILES => Some(BuiltinToolId::GrepFiles),
        TOOL_VIEW_IMAGE => Some(BuiltinToolId::ViewImage),
        TOOL_WEB_SEARCH => Some(BuiltinToolId::WebSearch),
        TOOL_SHELL_COMMAND => Some(BuiltinToolId::ShellCommand),
        TOOL_WRITE_STDIN => Some(BuiltinToolId::WriteStdin),
        TOOL_WRITE_FILE => Some(BuiltinToolId::WriteFile),
        TOOL_EDIT_FILE => Some(BuiltinToolId::EditFile),
        TOOL_APPLY_PATCH => Some(BuiltinToolId::ApplyPatch),
        TOOL_UPDATE_PLAN => Some(BuiltinToolId::UpdatePlan),
        TOOL_GET_GOAL => Some(BuiltinToolId::GetGoal),
        TOOL_UPDATE_GOAL => Some(BuiltinToolId::UpdateGoal),
        _ => None,
    }
}

pub fn builtin_tool_definition_by_name(name: &str) -> Option<ToolDefinition> {
    builtin_tool_id_by_name(name).map(BuiltinToolId::definition)
}
