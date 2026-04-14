use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::io::AsyncReadExt;

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_read_path};
use super::TOOL_READ_FILE;

pub fn read_file_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_READ_FILE.to_string(),
        description: Some(
            "Read a UTF-8 text file from the workspace (or allowed skills roots).".to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to workspace root (no `..`), or an absolute path within an allowed skills root. Prefer workspace-relative paths; for skill files use the absolute path under the skill directory." },
                "start_line": { "type": "integer", "minimum": 1, "description": "1-based start line (inclusive)." },
                "end_line": { "type": "integer", "minimum": 1, "description": "1-based end line (inclusive)." },
                "max_bytes": { "type": "integer", "minimum": 1, "description": "Maximum bytes to read (best effort)." }
            },
            "required": ["path"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

pub(super) async fn execute(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    file_tracker: &super::FileAccessTracker,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_read {
        return Err(ToolError::PermissionDenied(TOOL_READ_FILE.to_string()));
    }
    let args: ReadFileArgs = parse_args(call, TOOL_READ_FILE)?;
    let path = resolve_read_path(workspace_root, extra_workspace_roots, &args.path)?;

    let mut text = if let Some(max) = args.max_bytes {
        read_to_string_capped(&path, max).await?
    } else {
        tokio::fs::read_to_string(&path).await?
    };

    if args.start_line.is_some() || args.end_line.is_some() {
        text = slice_lines(&text, args.start_line, args.end_line);
    }

    file_tracker.record_read(&path).await?;

    Ok(text)
}

async fn read_to_string_capped(path: &Path, max: usize) -> Result<String, ToolError> {
    let file = tokio::fs::File::open(path).await?;
    let mut buf = Vec::new();
    let mut limited = file.take(max as u64);
    limited.read_to_end(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn slice_lines(text: &str, start_line: Option<usize>, end_line: Option<usize>) -> String {
    let start = start_line.unwrap_or(1).max(1);
    let end = end_line.unwrap_or(usize::MAX);
    if end < start {
        return String::new();
    }

    let mut out = String::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if line_no < start {
            continue;
        }
        if line_no > end {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.ends_with('\n') && !text.ends_with('\n') {
        out.pop();
    }
    out
}
