use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use serde::Deserialize;
use serde::Serialize;
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

#[derive(Debug, Serialize)]
struct WriteToolOutput {
    ok: bool,
    path: String,
    created: bool,
    bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    added_lines: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed_lines: Option<usize>,
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

    let (created, old_content) = match tokio::fs::read_to_string(&path).await {
        Ok(text) => (false, Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (true, None),
        Err(err) => return Err(err.into()),
    };

    let diff = old_content
        .as_deref()
        .and_then(|old| small_unified_diff(old, &args.content, &args.path));

    tokio::fs::write(&path, &args.content).await?;

    let out = WriteToolOutput {
        ok: true,
        path: args.path,
        created,
        bytes: args.content.len(),
        diff: diff.as_ref().map(|d| d.text.clone()),
        added_lines: diff.as_ref().map(|d| d.added_lines),
        removed_lines: diff.as_ref().map(|d| d.removed_lines),
    };
    Ok(serde_json::to_string(&out).unwrap_or_else(|_| "ok".to_string()))
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

#[derive(Debug, Clone)]
struct UnifiedDiff {
    text: String,
    added_lines: usize,
    removed_lines: usize,
}

fn small_unified_diff(old: &str, new: &str, path: &str) -> Option<UnifiedDiff> {
    if old == new {
        return None;
    }

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    const MAX_LINES: usize = 2_000;
    if old_lines.len() > MAX_LINES || new_lines.len() > MAX_LINES {
        return None;
    }

    let ops = diff_ops(&old_lines, &new_lines);
    let mut added_lines = 0usize;
    let mut removed_lines = 0usize;
    let mut change_indices = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        match op.kind {
            DiffOpKind::Add => {
                added_lines += 1;
                change_indices.push(idx);
            }
            DiffOpKind::Del => {
                removed_lines += 1;
                change_indices.push(idx);
            }
            DiffOpKind::Eq => {}
        }
    }

    const MAX_CHANGED_LINES: usize = 60;
    if added_lines + removed_lines > MAX_CHANGED_LINES {
        return None;
    }

    let hunks = diff_hunks(&ops, &change_indices, 3);
    let mut out_lines = Vec::new();
    out_lines.push(format!("diff --git a/{path} b/{path}"));
    out_lines.push(format!("--- a/{path}"));
    out_lines.push(format!("+++ b/{path}"));

    let mut rendered_lines = out_lines.len();
    for hunk in hunks {
        out_lines.push(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len
        ));
        rendered_lines += 1;
        for op in &ops[hunk.start..hunk.end] {
            let prefix = match op.kind {
                DiffOpKind::Eq => ' ',
                DiffOpKind::Del => '-',
                DiffOpKind::Add => '+',
            };
            out_lines.push(format!("{prefix}{}", op.text));
            rendered_lines += 1;
        }
    }

    const MAX_RENDERED_LINES: usize = 140;
    if rendered_lines > MAX_RENDERED_LINES {
        return None;
    }

    Some(UnifiedDiff {
        text: out_lines.join("\n"),
        added_lines,
        removed_lines,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOpKind {
    Eq,
    Del,
    Add,
}

#[derive(Debug, Clone)]
struct DiffOp<'a> {
    kind: DiffOpKind,
    text: &'a str,
}

fn diff_ops<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<DiffOp<'a>> {
    let n = old.len();
    let m = new.len();

    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if old[i] == new[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    let mut ops = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(DiffOp {
                kind: DiffOpKind::Eq,
                text: old[i],
            });
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(DiffOp {
                kind: DiffOpKind::Del,
                text: old[i],
            });
            i += 1;
        } else {
            ops.push(DiffOp {
                kind: DiffOpKind::Add,
                text: new[j],
            });
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffOp {
            kind: DiffOpKind::Del,
            text: old[i],
        });
        i += 1;
    }
    while j < m {
        ops.push(DiffOp {
            kind: DiffOpKind::Add,
            text: new[j],
        });
        j += 1;
    }
    ops
}

#[derive(Debug, Clone)]
struct DiffHunk {
    start: usize,
    end: usize,
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
}

fn diff_hunks(ops: &[DiffOp<'_>], change_indices: &[usize], context: usize) -> Vec<DiffHunk> {
    if change_indices.is_empty() {
        return Vec::new();
    }

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut current = (
        change_indices[0].saturating_sub(context),
        (change_indices[0] + context + 1).min(ops.len()),
    );

    for &idx in change_indices.iter().skip(1) {
        let start = idx.saturating_sub(context);
        let end = (idx + context + 1).min(ops.len());
        if start <= current.1 {
            current.1 = current.1.max(end);
        } else {
            ranges.push(current);
            current = (start, end);
        }
    }
    ranges.push(current);

    let mut old_pos = vec![0usize; ops.len() + 1];
    let mut new_pos = vec![0usize; ops.len() + 1];
    for (i, op) in ops.iter().enumerate() {
        old_pos[i + 1] = old_pos[i] + usize::from(op.kind != DiffOpKind::Add);
        new_pos[i + 1] = new_pos[i] + usize::from(op.kind != DiffOpKind::Del);
    }

    ranges
        .into_iter()
        .map(|(start, end)| DiffHunk {
            start,
            end,
            old_start: old_pos[start] + 1,
            old_len: old_pos[end].saturating_sub(old_pos[start]),
            new_start: new_pos[start] + 1,
            new_len: new_pos[end].saturating_sub(new_pos[start]),
        })
        .collect()
}
