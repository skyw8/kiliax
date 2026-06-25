use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::TOOL_WRITE_FILE;

const DESCRIPTION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/tools/write_file.md"
));

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
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    content: String,
    #[serde(rename = "filePath")]
    file_path: String,
}

pub(super) async fn execute(
    workspace_root: &Path,
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_write {
        return Err(ToolError::PermissionDenied(TOOL_WRITE_FILE.to_string()));
    }
    let args: WriteFileArgs = parse_args(call, TOOL_WRITE_FILE)?;
    let abs = resolve_workspace_path(workspace_root, extra_workspace_roots, &args.file_path)?;

    let source_had_bom = match tokio::fs::metadata(&abs).await {
        Ok(meta) => {
            if meta.is_dir() {
                return Err(ToolError::InvalidCommand(format!(
                    "path is a directory, not a file: {}",
                    abs.display()
                )));
            }
            let existing = tokio::fs::read_to_string(&abs).await?;
            has_bom(&existing)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => return Err(err.into()),
    };

    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let (content_had_bom, content) = split_bom(&args.content);
    tokio::fs::write(&abs, join_bom(content, source_had_bom || content_had_bom)).await?;

    Ok("Wrote file successfully.".to_string())
}

fn has_bom(text: &str) -> bool {
    text.starts_with('\u{feff}')
}

fn split_bom(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix('\u{feff}') {
        (true, rest)
    } else {
        (false, text)
    }
}

fn join_bom(text: &str, bom: bool) -> String {
    if bom {
        format!("\u{feff}{text}")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolCall;
    use crate::tools::{Permissions, ShellPermissions};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            name: TOOL_WRITE_FILE.to_string(),
            arguments: args.to_string(),
        }
    }

    fn permissions() -> Permissions {
        Permissions {
            file_read: true,
            file_write: true,
            shell: ShellPermissions::DenyAll,
        }
    }

    async fn run_write(root: &Path, args: serde_json::Value) -> Result<String, ToolError> {
        execute(
            root,
            &[],
            &permissions(),
            &call(args),
        )
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_unknown_args() {
        let tmp = tempfile::tempdir().unwrap();
        let err = run_write(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "content": "hello",
                "extra": true
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ToolError::InvalidArgs { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_relative_path_and_creates_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_write(
            tmp.path(),
            serde_json::json!({
                "filePath": "nested/a.txt",
                "content": "hello"
            }),
        )
        .await
        .unwrap();

        assert_eq!(out, "Wrote file successfully.");
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("nested/a.txt"))
                .await
                .unwrap(),
            "hello"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn overwrites_existing_file_without_prior_read() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "old")
            .await
            .unwrap();

        run_write(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "content": "new"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.txt"))
                .await
                .unwrap(),
            "new"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_existing_bom_when_overwriting() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "\u{feff}old")
            .await
            .unwrap();

        run_write(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "content": "new"
            }),
        )
        .await
        .unwrap();

        let out = tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap();
        assert!(out.starts_with('\u{feff}'));
        assert_eq!(out.trim_start_matches('\u{feff}'), "new");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_bom_from_input_content() {
        let tmp = tempfile::tempdir().unwrap();
        run_write(
            tmp.path(),
            serde_json::json!({
                "filePath": "a.txt",
                "content": "\u{feff}new"
            }),
        )
        .await
        .unwrap();

        let out = tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap();
        assert!(out.starts_with('\u{feff}'));
        assert_eq!(out.trim_start_matches('\u{feff}'), "new");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_directory_path() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(tmp.path().join("dir")).await.unwrap();
        let err = run_write(
            tmp.path(),
            serde_json::json!({
                "filePath": "dir",
                "content": "new"
            }),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ToolError::InvalidCommand(msg) if msg.contains("directory")));
    }
}
