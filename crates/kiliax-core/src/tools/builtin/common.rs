use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::llm::ToolCall;
use crate::tools::ToolError;

pub(super) fn parse_args<T: for<'de> Deserialize<'de>>(
    call: &ToolCall,
    tool_name: &str,
) -> Result<T, ToolError> {
    serde_json::from_str::<T>(&call.arguments).map_err(|source| ToolError::InvalidArgs {
        tool: tool_name.to_string(),
        source,
    })
}

pub(super) fn resolve_workspace_path(root: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let input = Path::new(path);
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };

    for c in candidate.components() {
        if matches!(c, Component::ParentDir) {
            return Err(ToolError::InvalidPath {
                path: path.to_string(),
                reason: "path must not contain `..`".to_string(),
            });
        }
    }

    if !candidate.starts_with(root) {
        return Err(ToolError::InvalidPath {
            path: path.to_string(),
            reason: format!("path must be within workspace root {}", root.display()),
        });
    }

    Ok(candidate)
}

pub(super) fn resolve_read_path(workspace_root: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let input = Path::new(path);
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        workspace_root.join(input)
    };

    for c in candidate.components() {
        if matches!(c, Component::ParentDir) {
            return Err(ToolError::InvalidPath {
                path: path.to_string(),
                reason: "path must not contain `..`".to_string(),
            });
        }
    }

    if candidate.starts_with(workspace_root) {
        return Ok(candidate);
    }

    for root in crate::tools::skills::skill_roots(workspace_root) {
        if candidate.starts_with(&root) {
            return Ok(candidate);
        }
    }

    Err(ToolError::InvalidPath {
        path: path.to_string(),
        reason: format!(
            "path must be within workspace root {} or skills roots",
            workspace_root.display()
        ),
    })
}
