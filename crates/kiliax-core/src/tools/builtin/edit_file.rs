use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::llm::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::{FileAccessTracker, TOOL_EDIT_FILE};

const DESCRIPTION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/tools/edit_file.md"
));

pub fn edit_file_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_EDIT_FILE.to_string(),
        description: Some(DESCRIPTION.to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "filePath": { "type": "string", "description": "The absolute path to the file to modify" },
                "oldString": { "type": "string", "description": "The text to replace" },
                "newString": { "type": "string", "description": "The text to replace it with (must be different from oldString)" },
                "replaceAll": { "type": "boolean", "description": "Replace all occurrences of oldString (default false)" }
            },
            "required": ["filePath", "oldString", "newString"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct EditFileArgs {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(rename = "oldString")]
    old_string: String,
    #[serde(rename = "newString")]
    new_string: String,
    #[serde(default, rename = "replaceAll")]
    replace_all: bool,
}

pub(super) async fn execute(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    file_tracker: &FileAccessTracker,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_write {
        return Err(ToolError::PermissionDenied(TOOL_EDIT_FILE.to_string()));
    }
    let args: EditFileArgs = parse_args(call, TOOL_EDIT_FILE)?;
    if args.old_string.is_empty() {
        return Err(ToolError::InvalidCommand(
            "oldString must not be empty".to_string(),
        ));
    }
    if args.old_string == args.new_string {
        return Err(ToolError::InvalidCommand(
            "No changes to apply: oldString and newString are identical.".to_string(),
        ));
    }

    let abs = resolve_workspace_path(workspace_root, extra_workspace_roots, &args.file_path)?;
    let meta = tokio::fs::metadata(&abs).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::InvalidCommand(format!("file not found: {}", abs.display()))
        } else {
            ToolError::Io(e)
        }
    })?;
    if meta.is_dir() {
        return Err(ToolError::InvalidCommand(format!(
            "path is a directory, not a file: {}",
            abs.display()
        )));
    }

    file_tracker
        .assert_read_unchanged(&abs, TOOL_EDIT_FILE)
        .await?;

    let content_old = tokio::fs::read_to_string(&abs).await?;
    let match_count = content_old.match_indices(&args.old_string).count();

    if match_count == 0 {
        return Err(ToolError::InvalidCommand(
            "oldString not found in content".to_string(),
        ));
    }
    if match_count > 1 && !args.replace_all {
        return Err(ToolError::InvalidCommand(
            "Found multiple matches for oldString. Provide more surrounding lines in oldString to identify the correct match."
                .to_string(),
        ));
    }

    let content_new = if args.replace_all {
        content_old.replace(&args.old_string, &args.new_string)
    } else {
        content_old.replacen(&args.old_string, &args.new_string, 1)
    };

    tokio::fs::write(&abs, content_new).await?;
    file_tracker.record_read(&abs).await?;

    Ok("Edit applied successfully.".to_string())
}
