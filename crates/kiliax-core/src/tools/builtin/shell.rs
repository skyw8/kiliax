use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::llm::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ShellPermissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::{TOOL_SHELL_COMMAND, TOOL_WRITE_STDIN};

pub fn shell_command_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_SHELL_COMMAND.to_string(),
        description: Some("Run a command in the workspace (argv form).".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "cwd": { "type": "string", "description": "Optional working dir relative to workspace root." },
                "yield_time_ms": { "type": "integer", "minimum": 0, "description": "If >0, return after this time with partial output and a session_id if still running." },
                "max_output_bytes": { "type": "integer", "minimum": 1, "description": "Maximum bytes to return per call (stdout+stderr best effort)." }
            },
            "required": ["argv"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

pub fn write_stdin_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_WRITE_STDIN.to_string(),
        description: Some(
            "Write to stdin of a running shell session (or poll output).".to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "integer", "minimum": 1 },
                "chars": { "type": "string", "description": "Text to write to stdin. If omitted/empty, this tool only polls output." },
                "yield_time_ms": { "type": "integer", "minimum": 0, "description": "Wait this long for output after writing." },
                "max_output_bytes": { "type": "integer", "minimum": 1, "description": "Maximum bytes to return per call (stdout+stderr best effort)." }
            },
            "required": ["session_id"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Default)]
pub struct ShellSessions {
    next_id: AtomicU64,
    sessions: tokio::sync::Mutex<std::collections::HashMap<u64, Arc<ShellSession>>>,
}

impl ShellSessions {
    pub fn new() -> Self {
        Self::default()
    }

    fn alloc_id(&self) -> u64 {
        self.next_id
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    async fn insert(&self, id: u64, sess: Arc<ShellSession>) {
        let mut map = self.sessions.lock().await;
        map.insert(id, sess);
    }

    async fn get(&self, id: u64) -> Option<Arc<ShellSession>> {
        let map = self.sessions.lock().await;
        map.get(&id).cloned()
    }

    async fn remove(&self, id: u64) -> Option<Arc<ShellSession>> {
        let mut map = self.sessions.lock().await;
        map.remove(&id)
    }
}

struct ShellSession {
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    stdout: tokio::sync::Mutex<String>,
    stderr: tokio::sync::Mutex<String>,
    exit_code: tokio::sync::Mutex<Option<i32>>,
}

impl ShellSession {
    fn new(stdin: Option<tokio::process::ChildStdin>) -> Self {
        Self {
            stdin: tokio::sync::Mutex::new(stdin),
            stdout: tokio::sync::Mutex::new(String::new()),
            stderr: tokio::sync::Mutex::new(String::new()),
            exit_code: tokio::sync::Mutex::new(None),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ShellCommandArgs {
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ShellCommandOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<u64>,
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

pub(super) async fn execute_shell_command(
    workspace_root: &Path,
    perms: &Permissions,
    shell_sessions: &ShellSessions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if matches!(perms.shell, ShellPermissions::DenyAll) {
        return Err(ToolError::PermissionDenied(TOOL_SHELL_COMMAND.to_string()));
    }
    let args: ShellCommandArgs = parse_args(call, TOOL_SHELL_COMMAND)?;
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

    let yield_time_ms = args.yield_time_ms.unwrap_or(0);
    if yield_time_ms == 0 {
        let mut cmd = Command::new(&args.argv[0]);
        cmd.args(&args.argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd.output().await?;
        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let out = ShellCommandOutput {
            session_id: None,
            running: false,
            exit_code: Some(code),
            stdout,
            stderr,
        };
        return Ok(serde_json::to_string(&out).unwrap_or_else(|_| "ok".to_string()));
    }

    let mut cmd = Command::new(&args.argv[0]);
    cmd.args(&args.argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();

    let session = Arc::new(ShellSession::new(stdin));
    if let Some(stdout) = stdout {
        let session = session.clone();
        tokio::spawn(async move {
            let mut rd = tokio::io::BufReader::new(stdout);
            let mut tmp = [0u8; 8192];
            loop {
                match tokio::io::AsyncReadExt::read(&mut rd, &mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let s = String::from_utf8_lossy(&tmp[..n]);
                        let mut out = session.stdout.lock().await;
                        out.push_str(&s);
                    }
                    Err(_) => break,
                }
            }
        });
    }
    if let Some(stderr) = stderr {
        let session = session.clone();
        tokio::spawn(async move {
            let mut rd = tokio::io::BufReader::new(stderr);
            let mut tmp = [0u8; 8192];
            loop {
                match tokio::io::AsyncReadExt::read(&mut rd, &mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let s = String::from_utf8_lossy(&tmp[..n]);
                        let mut out = session.stderr.lock().await;
                        out.push_str(&s);
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let session_for_wait = session.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
        let mut slot = session_for_wait.exit_code.lock().await;
        *slot = Some(code);
    });

    let id = shell_sessions.alloc_id();
    shell_sessions.insert(id, session.clone()).await;

    tokio::time::sleep(Duration::from_millis(yield_time_ms)).await;

    let max = args.max_output_bytes.unwrap_or(64 * 1024).max(1);
    let mut stdout_buf = session.stdout.lock().await;
    let stdout = drain_with_limit(&mut stdout_buf, max);
    let mut stderr_buf = session.stderr.lock().await;
    let stderr = drain_with_limit(&mut stderr_buf, max.saturating_sub(stdout.len()));

    let code = session.exit_code.lock().await.clone();
    let running = code.is_none();
    if !running {
        let _ = shell_sessions.remove(id).await;
    }

    let out = ShellCommandOutput {
        session_id: if running { Some(id) } else { None },
        running,
        exit_code: code,
        stdout,
        stderr,
    };
    Ok(serde_json::to_string(&out).unwrap_or_else(|_| "ok".to_string()))
}

#[derive(Debug, Deserialize)]
struct WriteStdinArgs {
    session_id: u64,
    #[serde(default)]
    chars: Option<String>,
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

pub(super) async fn execute_write_stdin(
    perms: &Permissions,
    shell_sessions: &ShellSessions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if matches!(perms.shell, ShellPermissions::DenyAll) {
        return Err(ToolError::PermissionDenied(TOOL_WRITE_STDIN.to_string()));
    }
    let args: WriteStdinArgs = parse_args(call, TOOL_WRITE_STDIN)?;
    let Some(sess) = shell_sessions.get(args.session_id).await else {
        return Err(ToolError::InvalidCommand(format!(
            "unknown session_id {}",
            args.session_id
        )));
    };

    if let Some(chars) = args.chars.as_deref().filter(|s| !s.is_empty()) {
        let mut stdin = sess.stdin.lock().await;
        if let Some(stdin) = stdin.as_mut() {
            stdin.write_all(chars.as_bytes()).await?;
            stdin.flush().await?;
        }
    }

    let yield_ms = args.yield_time_ms.unwrap_or(0);
    if yield_ms > 0 {
        tokio::time::sleep(Duration::from_millis(yield_ms)).await;
    }

    let max = args.max_output_bytes.unwrap_or(64 * 1024).max(1);
    let mut stdout_buf = sess.stdout.lock().await;
    let stdout = drain_with_limit(&mut stdout_buf, max);
    let mut stderr_buf = sess.stderr.lock().await;
    let stderr = drain_with_limit(&mut stderr_buf, max.saturating_sub(stdout.len()));

    let code = sess.exit_code.lock().await.clone();
    let running = code.is_none();
    if !running {
        let _ = shell_sessions.remove(args.session_id).await;
    }

    let out = ShellCommandOutput {
        session_id: if running { Some(args.session_id) } else { None },
        running,
        exit_code: code,
        stdout,
        stderr,
    };
    Ok(serde_json::to_string(&out).unwrap_or_else(|_| "ok".to_string()))
}

fn drain_with_limit(buf: &mut String, max_bytes: usize) -> String {
    if buf.is_empty() {
        return String::new();
    }
    if buf.len() <= max_bytes {
        return std::mem::take(buf);
    }

    let mut split_idx = 0usize;
    for (idx, _ch) in buf.char_indices() {
        if idx > max_bytes {
            break;
        }
        split_idx = idx;
    }
    if split_idx == 0 {
        split_idx = max_bytes.min(buf.len());
        while split_idx > 0 && !buf.is_char_boundary(split_idx) {
            split_idx -= 1;
        }
    }

    let out = buf[..split_idx].to_string();
    buf.drain(..split_idx);
    out
}
