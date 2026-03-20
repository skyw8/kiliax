use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use serde::Deserialize;
use tokio::process::Command;

use crate::llm::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ShellPermissions, ToolError};

pub const TOOL_READ: &str = "read";
pub const TOOL_WRITE: &str = "write";
pub const TOOL_SHELL: &str = "shell";

pub fn read_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_READ.to_string(),
        description: Some(
            "Read a UTF-8 text file from the workspace or from the skills directories.".to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to workspace root, or an absolute path within an allowed skills directory." }
            },
            "required": ["path"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

pub fn write_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_WRITE.to_string(),
        description: Some("Write a UTF-8 text file to the workspace.".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to workspace root." },
                "content": { "type": "string", "description": "Full file contents to write." },
                "create_dirs": { "type": "boolean", "description": "Create parent directories if missing.", "default": false }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

pub fn shell_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_SHELL.to_string(),
        description: Some("Run a command in the workspace (argv form).".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "cwd": { "type": "string", "description": "Optional working dir relative to workspace root." }
            },
            "required": ["argv"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

pub async fn execute(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    match call.name.as_str() {
        TOOL_READ => execute_read(workspace_root, perms, call).await,
        TOOL_WRITE => execute_write(workspace_root, perms, call).await,
        TOOL_SHELL => execute_shell(workspace_root, perms, call).await,
        other => Err(ToolError::UnknownTool(other.to_string())),
    }
}

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
}

async fn execute_read(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_read {
        return Err(ToolError::PermissionDenied(TOOL_READ.to_string()));
    }
    let args: ReadArgs = parse_args(call, TOOL_READ)?;
    let path = resolve_read_path(workspace_root, &args.path)?;
    Ok(tokio::fs::read_to_string(path).await?)
}

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    create_dirs: bool,
}

async fn execute_write(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_write {
        return Err(ToolError::PermissionDenied(TOOL_WRITE.to_string()));
    }
    let args: WriteArgs = parse_args(call, TOOL_WRITE)?;
    let path = resolve_workspace_path(workspace_root, &args.path)?;
    if args.create_dirs {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    tokio::fs::write(path, args.content).await?;
    Ok("ok".to_string())
}

#[derive(Debug, Deserialize)]
struct ShellArgs {
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

async fn execute_shell(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if matches!(perms.shell, ShellPermissions::DenyAll) {
        return Err(ToolError::PermissionDenied(TOOL_SHELL.to_string()));
    }
    let args: ShellArgs = parse_args(call, TOOL_SHELL)?;
    if args.argv.is_empty() {
        return Err(ToolError::InvalidCommand(
            "argv must not be empty".to_string(),
        ));
    }
    if !perms.shell.allows(&args.argv) {
        return Err(ToolError::PermissionDenied(format!(
            "shell argv not allowed: {:?}",
            args.argv
        )));
    }

    let cwd = match args.cwd.as_deref() {
        None => workspace_root.to_path_buf(),
        Some(p) => resolve_workspace_path(workspace_root, p)?,
    };

    let mut cmd = Command::new(&args.argv[0]);
    cmd.args(&args.argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output().await?;
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    Ok(format!(
        "exit_code: {code}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    ))
}

fn parse_args<T: for<'de> Deserialize<'de>>(
    call: &ToolCall,
    tool_name: &str,
) -> Result<T, ToolError> {
    serde_json::from_str::<T>(&call.arguments).map_err(|source| ToolError::InvalidArgs {
        tool: tool_name.to_string(),
        source,
    })
}

fn resolve_workspace_path(root: &Path, path: &str) -> Result<PathBuf, ToolError> {
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

fn resolve_read_path(workspace_root: &Path, path: &str) -> Result<PathBuf, ToolError> {
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
