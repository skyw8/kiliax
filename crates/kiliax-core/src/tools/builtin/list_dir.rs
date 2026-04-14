use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::TOOL_LIST_DIR;

pub fn list_dir_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_LIST_DIR.to_string(),
        description: Some("List directory entries under the workspace.".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path relative to workspace root (no `..`)." },
                "recursive": { "type": "boolean", "description": "Recurse into subdirectories.", "default": false },
                "max_depth": { "type": "integer", "minimum": 1, "description": "Maximum recursion depth (when recursive)." },
                "include_hidden": { "type": "boolean", "description": "Include entries starting with '.'.", "default": false },
                "max_entries": { "type": "integer", "minimum": 1, "description": "Maximum number of entries to return." }
            },
            "required": ["path"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct ListDirArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    max_depth: Option<usize>,
    #[serde(default)]
    include_hidden: bool,
    #[serde(default)]
    max_entries: Option<usize>,
}

pub(super) async fn execute(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_read {
        return Err(ToolError::PermissionDenied(TOOL_LIST_DIR.to_string()));
    }
    let args: ListDirArgs = parse_args(call, TOOL_LIST_DIR)?;
    let path = resolve_workspace_path(workspace_root, extra_workspace_roots, &args.path)?;

    let max_entries = args.max_entries.unwrap_or(2_000).max(1);
    let max_depth = args.max_depth.unwrap_or(32).max(1);
    let recursive = args.recursive;
    let include_hidden = args.include_hidden;

    let base = path.clone();
    let root = workspace_root.to_path_buf();

    let mut entries = tokio::task::spawn_blocking(move || {
        list_dir_blocking(
            &root,
            &base,
            recursive,
            max_depth,
            include_hidden,
            max_entries,
        )
    })
    .await
    .map_err(|e| ToolError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))??;

    entries.sort();
    Ok(entries.join("\n"))
}

fn list_dir_blocking(
    workspace_root: &Path,
    base: &Path,
    recursive: bool,
    max_depth: usize,
    include_hidden: bool,
    max_entries: usize,
) -> Result<Vec<String>, ToolError> {
    let mut out = Vec::new();
    let mut stack = vec![(base.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        if out.len() >= max_entries {
            break;
        }

        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(err) => return Err(err.into()),
        };

        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in rd {
            if out.len() >= max_entries {
                break;
            }
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !include_hidden && name.starts_with('.') {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                dirs.push(path);
            } else if ft.is_file() {
                files.push(path);
            }
        }

        dirs.sort();
        files.sort();

        for d in &dirs {
            if out.len() >= max_entries {
                break;
            }
            let rel =
                crate::prompt::workspace_relative_path(workspace_root, d).unwrap_or(d.as_path());
            out.push(format!("{}/", rel.to_string_lossy().replace('\\', "/")));
        }
        for f in &files {
            if out.len() >= max_entries {
                break;
            }
            let rel =
                crate::prompt::workspace_relative_path(workspace_root, f).unwrap_or(f.as_path());
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }

        if recursive && depth + 1 < max_depth {
            for d in dirs.into_iter().rev() {
                stack.push((d, depth + 1));
            }
        }
    }

    Ok(out)
}
