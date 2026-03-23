use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::llm::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ShellPermissions, ToolError};

pub const TOOL_READ_FILE: &str = "read_file";
pub const TOOL_LIST_DIR: &str = "list_dir";
pub const TOOL_GREP_FILES: &str = "grep_files";
pub const TOOL_SHELL_COMMAND: &str = "shell_command";
pub const TOOL_WRITE_STDIN: &str = "write_stdin";
pub const TOOL_APPLY_PATCH: &str = "apply_patch";
pub const TOOL_UPDATE_PLAN: &str = "update_plan";

pub fn read_file_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_READ_FILE.to_string(),
        description: Some(
            "Read a UTF-8 text file from the workspace (or allowed skills directories)."
                .to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to workspace root, or an absolute path within an allowed skills directory." },
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

pub fn list_dir_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_LIST_DIR.to_string(),
        description: Some("List directory entries under the workspace.".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path relative to workspace root." },
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

pub fn grep_files_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_GREP_FILES.to_string(),
        description: Some("Search files under the workspace for a regex pattern.".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regex pattern to search for." },
                "path": { "type": "string", "description": "Directory path relative to workspace root.", "default": "." },
                "case_sensitive": { "type": "boolean", "description": "Case-sensitive search.", "default": true },
                "max_results": { "type": "integer", "minimum": 1, "description": "Maximum matches to return." },
                "max_bytes_per_file": { "type": "integer", "minimum": 1, "description": "Skip files larger than this size in bytes." },
                "include_hidden": { "type": "boolean", "description": "Include hidden files and directories.", "default": false }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

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
        description: Some("Write to stdin of a running shell session (or poll output).".to_string()),
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

pub fn apply_patch_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_APPLY_PATCH.to_string(),
        description: Some("Apply a file-oriented patch to the workspace.".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "Patch text in the *** Begin Patch / *** End Patch format." }
            },
            "required": ["patch"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

pub fn update_plan_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_UPDATE_PLAN.to_string(),
        description: Some("Update the UI plan (best effort).".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "explanation": { "type": "string", "description": "Optional brief explanation for changes." },
                "plan": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending","in_progress","completed"] }
                        },
                        "required": ["step","status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["plan"],
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
        self.next_id.fetch_add(1, Ordering::Relaxed).saturating_add(1)
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

pub async fn execute(
    workspace_root: &Path,
    perms: &Permissions,
    shell_sessions: &ShellSessions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    match call.name.as_str() {
        TOOL_READ_FILE => execute_read_file(workspace_root, perms, call).await,
        TOOL_LIST_DIR => execute_list_dir(workspace_root, perms, call).await,
        TOOL_GREP_FILES => execute_grep_files(workspace_root, perms, call).await,
        TOOL_SHELL_COMMAND => execute_shell_command(workspace_root, perms, shell_sessions, call).await,
        TOOL_WRITE_STDIN => execute_write_stdin(perms, shell_sessions, call).await,
        TOOL_APPLY_PATCH => execute_apply_patch(workspace_root, perms, call).await,
        TOOL_UPDATE_PLAN => execute_update_plan(call),
        other => Err(ToolError::UnknownTool(other.to_string())),
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

async fn execute_read_file(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_read {
        return Err(ToolError::PermissionDenied(TOOL_READ_FILE.to_string()));
    }
    let args: ReadFileArgs = parse_args(call, TOOL_READ_FILE)?;
    let path = resolve_read_path(workspace_root, &args.path)?;

    let mut text = if let Some(max) = args.max_bytes {
        read_to_string_capped(&path, max).await?
    } else {
        tokio::fs::read_to_string(&path).await?
    };

    if args.start_line.is_some() || args.end_line.is_some() {
        text = slice_lines(&text, args.start_line, args.end_line);
    }

    Ok(text)
}

async fn read_to_string_capped(path: &Path, max: usize) -> Result<String, ToolError> {
    use tokio::io::AsyncReadExt;

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

async fn execute_list_dir(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_read {
        return Err(ToolError::PermissionDenied(TOOL_LIST_DIR.to_string()));
    }
    let args: ListDirArgs = parse_args(call, TOOL_LIST_DIR)?;
    let path = resolve_workspace_path(workspace_root, &args.path)?;

    let max_entries = args.max_entries.unwrap_or(2_000).max(1);
    let max_depth = args.max_depth.unwrap_or(32).max(1);
    let recursive = args.recursive;
    let include_hidden = args.include_hidden;

    let base = path.clone();
    let root = workspace_root.to_path_buf();

    let mut entries = tokio::task::spawn_blocking(move || {
        list_dir_blocking(&root, &base, recursive, max_depth, include_hidden, max_entries)
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
            let rel = crate::prompt::workspace_relative_path(workspace_root, d)
                .unwrap_or(d.as_path());
            out.push(format!("{}/", rel.to_string_lossy().replace('\\', "/")));
        }
        for f in &files {
            if out.len() >= max_entries {
                break;
            }
            let rel = crate::prompt::workspace_relative_path(workspace_root, f)
                .unwrap_or(f.as_path());
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

#[derive(Debug, Deserialize)]
struct GrepFilesArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default = "default_true")]
    case_sensitive: bool,
    #[serde(default)]
    max_results: Option<usize>,
    #[serde(default)]
    max_bytes_per_file: Option<u64>,
    #[serde(default)]
    include_hidden: bool,
}

fn default_true() -> bool {
    true
}

async fn execute_grep_files(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_read {
        return Err(ToolError::PermissionDenied(TOOL_GREP_FILES.to_string()));
    }
    let args: GrepFilesArgs = parse_args(call, TOOL_GREP_FILES)?;
    let dir = args.path.as_deref().unwrap_or(".");
    let base = resolve_workspace_path(workspace_root, dir)?;

    let pattern = args.pattern.clone();
    let case_sensitive = args.case_sensitive;
    let max_results = args.max_results.unwrap_or(100).max(1);
    let max_bytes_per_file = args.max_bytes_per_file.unwrap_or(2_000_000).max(1);
    let include_hidden = args.include_hidden;
    let root = workspace_root.to_path_buf();

    let matches = tokio::task::spawn_blocking(move || {
        grep_files_blocking(
            &root,
            &base,
            &pattern,
            case_sensitive,
            max_results,
            max_bytes_per_file,
            include_hidden,
        )
    })
    .await
    .map_err(|e| ToolError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))??;

    Ok(matches.join("\n"))
}

fn should_skip_dir(name: &str) -> bool {
    matches!(name, ".git" | "target" | ".killiax")
}

fn grep_files_blocking(
    workspace_root: &Path,
    base: &Path,
    pattern: &str,
    case_sensitive: bool,
    max_results: usize,
    max_bytes_per_file: u64,
    include_hidden: bool,
) -> Result<Vec<String>, ToolError> {
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| ToolError::InvalidCommand(format!("invalid regex: {e}")))?;

    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if out.len() >= max_results {
            break;
        }

        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(err) => return Err(err.into()),
        };

        for entry in rd {
            if out.len() >= max_results {
                break;
            }
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !include_hidden && name.starts_with('.') {
                continue;
            }
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if should_skip_dir(&name) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !ft.is_file() {
                continue;
            }

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > max_bytes_per_file {
                continue;
            }

            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };

            for (idx, line) in text.lines().enumerate() {
                if out.len() >= max_results {
                    break;
                }
                for m in re.find_iter(line) {
                    if out.len() >= max_results {
                        break;
                    }
                    let rel = crate::prompt::workspace_relative_path(workspace_root, &path)
                        .unwrap_or(path.as_path());
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    let line_no = idx + 1;
                    let col = m.start() + 1;
                    out.push(format!("{rel}:{line_no}:{col}: {line}"));
                }
            }
        }
    }

    Ok(out)
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

async fn execute_shell_command(
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
        let code = status
            .ok()
            .and_then(|s| s.code())
            .unwrap_or(-1);
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

async fn execute_write_stdin(
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

#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    patch: String,
}

#[derive(Debug, Serialize)]
struct ApplyPatchOutput {
    ok: bool,
    files: Vec<PatchedFile>,
}

#[derive(Debug, Serialize)]
struct PatchedFile {
    action: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    moved_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    added_lines: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed_lines: Option<usize>,
}

async fn execute_apply_patch(
    workspace_root: &Path,
    perms: &Permissions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if !perms.file_write {
        return Err(ToolError::PermissionDenied(TOOL_APPLY_PATCH.to_string()));
    }
    let args: ApplyPatchArgs = parse_args(call, TOOL_APPLY_PATCH)?;

    let ops = parse_patch(&args.patch)
        .map_err(|e| ToolError::InvalidCommand(format!("invalid patch: {e}")))?;

    let mut out_files = Vec::new();

    for op in ops {
        match op {
            PatchOp::AddFile { path, content } => {
                let abs = resolve_workspace_path(workspace_root, &path)?;
                if abs.exists() {
                    return Err(ToolError::InvalidCommand(format!(
                        "add file failed: {path} already exists"
                    )));
                }
                if let Some(parent) = abs.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&abs, &content).await?;
                let diff = small_unified_diff("", &content, &path);
                out_files.push(PatchedFile {
                    action: "add".to_string(),
                    path,
                    moved_to: None,
                    diff: diff.as_ref().map(|d| d.text.clone()),
                    added_lines: diff.as_ref().map(|d| d.added_lines),
                    removed_lines: diff.as_ref().map(|d| d.removed_lines),
                });
            }
            PatchOp::DeleteFile { path } => {
                let abs = resolve_workspace_path(workspace_root, &path)?;
                let old = tokio::fs::read_to_string(&abs).await.unwrap_or_default();
                tokio::fs::remove_file(&abs).await?;
                let diff = small_unified_diff(&old, "", &path);
                out_files.push(PatchedFile {
                    action: "delete".to_string(),
                    path,
                    moved_to: None,
                    diff: diff.as_ref().map(|d| d.text.clone()),
                    added_lines: diff.as_ref().map(|d| d.added_lines),
                    removed_lines: diff.as_ref().map(|d| d.removed_lines),
                });
            }
            PatchOp::UpdateFile {
                path,
                move_to,
                hunks,
            } => {
                let abs = resolve_workspace_path(workspace_root, &path)?;
                let old = tokio::fs::read_to_string(&abs).await?;
                let new = apply_update_hunks(&old, &hunks)
                    .map_err(|e| ToolError::InvalidCommand(format!("patch failed: {e}")))?;

                tokio::fs::write(&abs, &new).await?;
                let mut final_path = path.clone();

                if let Some(dest) = move_to.clone() {
                    let dest_abs = resolve_workspace_path(workspace_root, &dest)?;
                    if let Some(parent) = dest_abs.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::rename(&abs, &dest_abs).await?;
                    final_path = dest;
                }

                let diff = small_unified_diff(&old, &new, &final_path);
                out_files.push(PatchedFile {
                    action: "update".to_string(),
                    path,
                    moved_to: move_to,
                    diff: diff.as_ref().map(|d| d.text.clone()),
                    added_lines: diff.as_ref().map(|d| d.added_lines),
                    removed_lines: diff.as_ref().map(|d| d.removed_lines),
                });
            }
        }
    }

    let out = ApplyPatchOutput {
        ok: true,
        files: out_files,
    };
    Ok(serde_json::to_string(&out).unwrap_or_else(|_| "ok".to_string()))
}

#[derive(Debug)]
enum PatchOp {
    AddFile { path: String, content: String },
    DeleteFile { path: String },
    UpdateFile {
        path: String,
        move_to: Option<String>,
        hunks: Vec<UpdateHunk>,
    },
}

#[derive(Debug, Default)]
struct UpdateHunk {
    #[allow(dead_code)]
    header: Option<String>,
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone)]
enum HunkLine {
    Context(String),
    Add(String),
    Del(String),
}

fn parse_patch(input: &str) -> Result<Vec<PatchOp>, String> {
    let mut lines: Vec<&str> = input.split('\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    let mut i = 0usize;
    let first = lines.get(i).ok_or("missing *** Begin Patch")?.trim_end_matches('\r');
    if first != "*** Begin Patch" {
        return Err("expected *** Begin Patch".to_string());
    }
    i += 1;

    let mut ops = Vec::new();
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        if line == "*** End Patch" {
            return Ok(ops);
        }

        if let Some(path) = line.strip_prefix("*** Add File:") {
            let path = path.trim();
            if path.is_empty() {
                return Err("add file missing path".to_string());
            }
            i += 1;
            let mut content_lines = Vec::new();
            while i < lines.len() {
                let l = lines[i].trim_end_matches('\r');
                if l.starts_with("*** ") || l == "*** End Patch" {
                    break;
                }
                let rest = l
                    .strip_prefix('+')
                    .ok_or_else(|| "add file lines must start with '+'".to_string())?;
                content_lines.push(rest.to_string());
                i += 1;
            }
            let mut content = content_lines.join("\n");
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            ops.push(PatchOp::AddFile {
                path: path.to_string(),
                content,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File:") {
            let path = path.trim();
            if path.is_empty() {
                return Err("delete file missing path".to_string());
            }
            i += 1;
            ops.push(PatchOp::DeleteFile {
                path: path.to_string(),
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File:") {
            let path = path.trim();
            if path.is_empty() {
                return Err("update file missing path".to_string());
            }
            i += 1;

            let mut move_to: Option<String> = None;
            if i < lines.len() {
                let l = lines[i].trim_end_matches('\r');
                if let Some(dest) = l.strip_prefix("*** Move to:") {
                    let dest = dest.trim();
                    if dest.is_empty() {
                        return Err("move to missing path".to_string());
                    }
                    move_to = Some(dest.to_string());
                    i += 1;
                }
            }

            let mut hunks = Vec::new();
            while i < lines.len() {
                let l = lines[i].trim_end_matches('\r');
                if l == "*** End Patch" || l.starts_with("*** ") {
                    break;
                }
                if !l.starts_with("@@") {
                    return Err(format!("expected @@ hunk header, got {l:?}"));
                }
                let header = l.strip_prefix("@@").unwrap().trim();
                let header = if header.is_empty() {
                    None
                } else {
                    Some(header.to_string())
                };
                i += 1;

                let mut hunk = UpdateHunk {
                    header,
                    lines: Vec::new(),
                };
                while i < lines.len() {
                    let l2 = lines[i].trim_end_matches('\r');
                    if l2.starts_with("@@") || l2.starts_with("*** ") || l2 == "*** End Patch" {
                        break;
                    }
                    let mut chars = l2.chars();
                    let Some(prefix) = chars.next() else {
                        return Err("empty hunk line".to_string());
                    };
                    let rest = chars.as_str().to_string();
                    match prefix {
                        ' ' => hunk.lines.push(HunkLine::Context(rest)),
                        '+' => hunk.lines.push(HunkLine::Add(rest)),
                        '-' => hunk.lines.push(HunkLine::Del(rest)),
                        _ => return Err(format!("invalid hunk line prefix {prefix:?}")),
                    }
                    i += 1;
                }
                hunks.push(hunk);
            }

            ops.push(PatchOp::UpdateFile {
                path: path.to_string(),
                move_to,
                hunks,
            });
            continue;
        }

        return Err(format!("unexpected line: {line:?}"));
    }

    Err("missing *** End Patch".to_string())
}

fn apply_update_hunks(original: &str, hunks: &[UpdateHunk]) -> Result<String, String> {
    let had_trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = original
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    if had_trailing_newline && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    let mut cursor = 0usize;
    for hunk in hunks {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for hl in &hunk.lines {
            match hl {
                HunkLine::Context(s) => {
                    before.push(s.as_str());
                    after.push(s.as_str());
                }
                HunkLine::Del(s) => before.push(s.as_str()),
                HunkLine::Add(s) => after.push(s.as_str()),
            }
        }

        let pos = if before.is_empty() {
            cursor.min(lines.len())
        } else if let Some(p) = find_subsequence(&lines, cursor, &before) {
            p
        } else if let Some(p) = find_subsequence(&lines, 0, &before) {
            p
        } else {
            return Err("hunk context not found".to_string());
        };

        let end = pos.saturating_add(before.len()).min(lines.len());
        let replacement: Vec<String> = after.iter().map(|s| (*s).to_string()).collect();
        lines.splice(pos..end, replacement.clone());
        cursor = pos.saturating_add(replacement.len());
    }

    let mut out = lines.join("\n");
    if had_trailing_newline {
        out.push('\n');
    }
    Ok(out)
}

fn find_subsequence(haystack: &[String], start: usize, needle: &[&str]) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(haystack.len()));
    }
    if haystack.len() < needle.len() || start >= haystack.len() {
        return None;
    }

    for i in start..=haystack.len().saturating_sub(needle.len()) {
        let mut ok = true;
        for (j, n) in needle.iter().enumerate() {
            if haystack[i + j] != *n {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }
    None
}

fn execute_update_plan(call: &ToolCall) -> Result<String, ToolError> {
    let _args: UpdatePlanArgs = parse_args(call, TOOL_UPDATE_PLAN)?;
    Ok("{\"ok\":true}".to_string())
}

#[derive(Debug, Deserialize)]
struct UpdatePlanArgs {
    #[allow(dead_code)]
    explanation: Option<String>,
    #[allow(dead_code)]
    plan: Vec<UpdatePlanItem>,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanItem {
    #[allow(dead_code)]
    step: String,
    #[allow(dead_code)]
    status: String,
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

    const MAX_CHANGED_LINES: usize = 80;
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

    const MAX_RENDERED_LINES: usize = 180;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_update_hunks_replaces_block() {
        let original = "a\nb\nc\n";
        let hunks = vec![UpdateHunk {
            header: None,
            lines: vec![
                HunkLine::Context("a".into()),
                HunkLine::Del("b".into()),
                HunkLine::Add("bb".into()),
                HunkLine::Context("c".into()),
            ],
        }];
        let out = apply_update_hunks(original, &hunks).unwrap();
        assert_eq!(out, "a\nbb\nc\n");
    }

    #[test]
    fn parse_patch_add_update_delete() {
        let patch = "\
*** Begin Patch
*** Add File: a.txt
+hello
*** Update File: a.txt
@@
 hello
+world
*** Delete File: a.txt
*** End Patch
";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 3);
    }
}
