use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::protocol::{Message, ToolCall, ToolDefinition, UserContentPart, UserMessageContent};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_read_path};
use super::TOOL_VIEW_IMAGE;

pub fn view_image_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_VIEW_IMAGE.to_string(),
        description: Some(
            "View (attach) a local image from the filesystem. Supported: png/jpg/jpeg/gif/webp/bmp/tif/tiff/avif."
                .to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Image path relative to workspace root (no `..`), or an absolute path within an allowed skills root." }
            },
            "required": ["path"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct ViewImageArgs {
    path: String,
}

fn is_supported_image_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.trim().to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "avif"
            )
    )
}

pub(super) async fn execute(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    Ok(
        execute_with_attachment(workspace_root, extra_workspace_roots, perms, call)
            .await?
            .0,
    )
}

pub(crate) async fn execute_with_attachment(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    call: &ToolCall,
) -> Result<(String, Message), ToolError> {
    if !perms.file_read {
        return Err(ToolError::PermissionDenied(TOOL_VIEW_IMAGE.to_string()));
    }

    let args: ViewImageArgs = parse_args(call, TOOL_VIEW_IMAGE)?;
    let path = resolve_read_path(workspace_root, extra_workspace_roots, &args.path)?;

    let meta = tokio::fs::metadata(&path).await?;
    if !meta.is_file() {
        return Err(ToolError::InvalidPath {
            path: args.path,
            reason: "path must point to a file".to_string(),
        });
    }
    if !is_supported_image_extension(&path) {
        return Err(ToolError::InvalidPath {
            path: args.path,
            reason: "unsupported image extension".to_string(),
        });
    }

    let size = meta.len();
    let display_path = crate::prompt::workspace_relative_path(workspace_root, &path)
        .unwrap_or(path.as_path())
        .to_string_lossy()
        .replace('\\', "/");

    let tool_text = format!("ok: true\npath: {display_path}\nsize_bytes: {size}");

    let msg = Message::User {
        content: UserMessageContent::Parts(vec![UserContentPart::Image {
            path: display_path,
            detail: None,
        }]),
    };

    Ok((tool_text, msg))
}
