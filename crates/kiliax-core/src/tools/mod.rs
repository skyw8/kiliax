pub mod builtin;
pub mod engine;
pub mod mcp;
pub mod policy;
pub mod skills;

pub use engine::{McpServerConnectionState, McpServerStatus, ToolEngine};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolParallelism {
    /// Safe to execute in parallel with other parallel tool calls.
    Parallel,
    /// Must be executed alone (acts as a barrier).
    Exclusive,
}

impl ToolParallelism {
    pub fn is_parallel(self) -> bool {
        matches!(self, ToolParallelism::Parallel)
    }
}

pub fn tool_parallelism(tool_name: &str) -> ToolParallelism {
    match tool_name {
        builtin::TOOL_READ_FILE
        | builtin::TOOL_LIST_DIR
        | builtin::TOOL_GREP_FILES
        | builtin::TOOL_VIEW_IMAGE
        | builtin::TOOL_WEB_SEARCH
        | builtin::TOOL_SHELL_COMMAND
        | builtin::TOOL_WRITE_STDIN
        | builtin::TOOL_APPLY_PATCH => ToolParallelism::Parallel,
        builtin::TOOL_UPDATE_PLAN | builtin::TOOL_WRITE_FILE | builtin::TOOL_EDIT_FILE => {
            ToolParallelism::Exclusive
        }
        _ => ToolParallelism::Exclusive,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permissions {
    pub file_read: bool,
    pub file_write: bool,
    pub shell: ShellPermissions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellPermissions {
    DenyAll,
    AllowAll,
    /// Allow when argv begins with one of the prefixes (exact token match).
    AllowList(Vec<Vec<String>>),
}

impl ShellPermissions {
    pub fn allows(&self, argv: &[String]) -> bool {
        match self {
            ShellPermissions::DenyAll => false,
            ShellPermissions::AllowAll => true,
            ShellPermissions::AllowList(prefixes) => prefixes.iter().any(|p| is_prefix(p, argv)),
        }
    }
}

fn is_prefix(prefix: &[String], argv: &[String]) -> bool {
    if prefix.is_empty() || argv.len() < prefix.len() {
        return false;
    }
    prefix
        .iter()
        .zip(argv.iter())
        .all(|(a, b)| a.as_str() == b.as_str())
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("unknown tool: {0}")]
    UnknownTool(String),

    #[error("invalid args for {tool}: {source}")]
    InvalidArgs {
        tool: String,
        source: serde_json::Error,
    },

    #[error("invalid path {path:?}: {reason}")]
    InvalidPath { path: String, reason: String },

    #[error("invalid command: {0}")]
    InvalidCommand(String),

    #[error("mcp error: {0}")]
    Mcp(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultFormat {
    /// Plain text (best effort). This is what we currently send back to the model as a tool message.
    Text,
}
