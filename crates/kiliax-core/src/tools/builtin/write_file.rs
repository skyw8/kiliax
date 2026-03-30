use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::llm::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::{FileAccessTracker, TOOL_WRITE_FILE};

const DESCRIPTION: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/tools/write_file.md"));

pub fn write_file_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_WRITE_FILE.to_string(),
        description: Some(DESCRIPTION.to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "The content to write to the file" },
                "filePath": { "type": "string", "description": "The absolute path to the file to write (must be absolute, not relative)" }
            },
            "required": ["content", "filePath"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct WriteFileArgs {
    content: String,
    #[serde(rename = "filePath")]
    file_path: String,
}

pub(super) async fn execute(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    file_tracker: &FileAccessTracker,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_write {
        return Err(ToolError::PermissionDenied(TOOL_WRITE_FILE.to_string()));
    }
    let args: WriteFileArgs = parse_args(call, TOOL_WRITE_FILE)?;
    let abs = resolve_workspace_path(workspace_root, extra_workspace_roots, &args.file_path)?;

    let exists = match tokio::fs::metadata(&abs).await {
        Ok(meta) => {
            if meta.is_dir() {
                return Err(ToolError::InvalidCommand(format!(
                    "path is a directory, not a file: {}",
                    abs.display()
                )));
            }
            true
        }
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => return Err(err.into()),
    };

    if exists {
        file_tracker
            .assert_read_unchanged(&abs, TOOL_WRITE_FILE)
            .await?;
    }

    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&abs, args.content).await?;
    file_tracker.record_read(&abs).await?;

    Ok("Wrote file successfully.".to_string())
}
