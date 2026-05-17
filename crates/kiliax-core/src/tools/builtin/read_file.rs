use std::path::{Path, PathBuf};

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};
use serde::Deserialize;

use super::common::{parse_args, resolve_read_path};
use super::TOOL_READ_FILE;

pub fn read_file_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_READ_FILE.to_string(),
        description: Some(
            "Read a UTF-8 text file with line numbers. Use offset and limit for pagination."
                .to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "filePath": { "type": "string", "description": "Path relative to workspace root (no `..`), or an absolute path within an allowed skills root." },
                "offset": { "type": "integer", "minimum": 1, "description": "1-based start line." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum lines to return. Defaults to 2000." }
            },
            "required": ["filePath"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

const DEFAULT_LIMIT: usize = 2_000;
const MAX_LINE_CHARS: usize = 2_000;
const MAX_CONTENT_BYTES: usize = 50 * 1024;
const LINE_TRUNCATION_SUFFIX: &str = "... [line truncated]";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
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
    let path = resolve_read_path(workspace_root, extra_workspace_roots, &args.file_path)?;
    let text = tokio::fs::read_to_string(&path).await?;

    file_tracker.record_read(&path).await?;

    Ok(format_read_file_output(
        &path,
        &text,
        args.offset.unwrap_or(1).max(1),
        args.limit.unwrap_or(DEFAULT_LIMIT).max(1),
    ))
}

fn format_read_file_output(path: &Path, text: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total_lines = lines.len();
    let start_idx = offset.saturating_sub(1);
    let mut next_offset = offset;
    let mut returned = 0usize;
    let mut content_bytes = 0usize;
    let mut capped = false;
    let mut content = String::new();

    if start_idx < total_lines {
        for (idx, line) in lines.iter().enumerate().skip(start_idx).take(limit) {
            let line_no = idx + 1;
            let rendered = format!("{line_no}: {}\n", truncate_line(line));
            let rendered_len = rendered.len();
            if content_bytes + rendered_len > MAX_CONTENT_BYTES {
                capped = true;
                break;
            }
            content.push_str(&rendered);
            content_bytes += rendered_len;
            returned += 1;
            next_offset = line_no + 1;
        }
    }

    let mut out = String::new();
    out.push_str(&format!("<path>{}</path>\n", path.display()));
    out.push_str("<type>file</type>\n");
    out.push_str("<content>\n");
    out.push_str(&content);
    out.push_str("</content>\n");

    if next_offset <= total_lines && (capped || returned == limit) {
        if capped {
            out.push_str(&format!(
                "Output capped at {MAX_CONTENT_BYTES} bytes. Use offset={next_offset} to continue"
            ));
        } else {
            out.push_str(&format!("Use offset={next_offset} to continue"));
        }
    } else {
        out.push_str(&format!("(End of file - total {total_lines} lines)"));
    }

    out
}

fn truncate_line(line: &str) -> String {
    let mut chars = line.chars();
    let prefix: String = chars.by_ref().take(MAX_LINE_CHARS).collect();
    if chars.next().is_some() {
        format!("{prefix}{LINE_TRUNCATION_SUFFIX}")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ShellPermissions;

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            name: TOOL_READ_FILE.to_string(),
            arguments: args.to_string(),
        }
    }

    fn permissions() -> Permissions {
        Permissions {
            file_read: true,
            file_write: false,
            shell: ShellPermissions::DenyAll,
        }
    }

    async fn run_read(root: &Path, args: serde_json::Value) -> Result<String, ToolError> {
        execute(
            root,
            &[],
            &permissions(),
            &super::super::FileAccessTracker::new(),
            &call(args),
        )
        .await
    }

    #[test]
    fn tool_definition_uses_opencode_style_read_file_args() {
        let def = read_file_tool_definition();
        assert_eq!(def.name, TOOL_READ_FILE);
        assert_eq!(def.strict, Some(true));

        let params = def.parameters.unwrap();
        assert_eq!(params["required"], serde_json::json!(["filePath"]));
        assert_eq!(params["additionalProperties"], false);
        assert!(params["properties"].get("filePath").is_some());
        assert!(params["properties"].get("offset").is_some());
        assert!(params["properties"].get("limit").is_some());
        assert!(params["properties"].get("path").is_none());
        assert!(params["properties"].get("start_line").is_none());
        assert!(params["properties"].get("end_line").is_none());
        assert!(params["properties"].get("max_bytes").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn default_read_returns_line_numbered_content_and_eof_total() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("sample.txt"), "alpha\nbeta\ngamma")
            .await
            .unwrap();

        let out = run_read(tmp.path(), serde_json::json!({ "filePath": "sample.txt" }))
            .await
            .unwrap();

        assert!(out.contains(&format!(
            "<path>{}</path>",
            tmp.path().join("sample.txt").display()
        )));
        assert!(out.contains("<type>file</type>"));
        assert!(out.contains("<content>\n1: alpha\n2: beta\n3: gamma\n</content>"));
        assert!(out.ends_with("(End of file - total 3 lines)"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn offset_and_limit_return_window_and_continuation_hint() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("sample.txt"), "one\ntwo\nthree\nfour")
            .await
            .unwrap();

        let out = run_read(
            tmp.path(),
            serde_json::json!({
                "filePath": "sample.txt",
                "offset": 2,
                "limit": 2
            }),
        )
        .await
        .unwrap();

        assert!(out.contains("<content>\n2: two\n3: three\n</content>"));
        assert!(out.ends_with("Use offset=4 to continue"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn long_lines_are_truncated_at_two_thousand_chars() {
        let tmp = tempfile::tempdir().unwrap();
        let long = "x".repeat(MAX_LINE_CHARS + 1);
        tokio::fs::write(tmp.path().join("sample.txt"), &long)
            .await
            .unwrap();

        let out = run_read(tmp.path(), serde_json::json!({ "filePath": "sample.txt" }))
            .await
            .unwrap();
        let expected = format!("1: {}{LINE_TRUNCATION_SUFFIX}", "x".repeat(MAX_LINE_CHARS));

        assert!(out.contains(&expected));
        assert!(!out.contains(&format!("1: {}", long)));
    }

    #[test]
    fn returned_content_is_capped_with_continuation_hint() {
        let text = (0..100)
            .map(|_| "x".repeat(MAX_LINE_CHARS))
            .collect::<Vec<_>>()
            .join("\n");

        let out = format_read_file_output(Path::new("sample.txt"), &text, 1, 100);

        assert!(out.contains("Output capped at 51200 bytes. Use offset="));
        assert!(out.len() < 60 * 1024);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn old_read_file_args_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("sample.txt"), "hello")
            .await
            .unwrap();

        let err = run_read(
            tmp.path(),
            serde_json::json!({
                "path": "sample.txt",
                "start_line": 1,
                "end_line": 1,
                "max_bytes": 10
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ToolError::InvalidArgs { .. }));
    }
}
