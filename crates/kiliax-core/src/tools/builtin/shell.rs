#[cfg(windows)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ShellPermissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::{TOOL_SHELL_COMMAND, TOOL_WRITE_STDIN};

pub fn shell_command_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_SHELL_COMMAND.to_string(),
        description: Some(
            "Run a command in the workspace through the user's default shell. The command inherits the full kiliax process environment and uses login/profile shell semantics by default. If the result includes `session_id`, use `write_stdin` to interact/poll."
                .to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "description": "Shell command string to execute." },
                "cwd": { "type": "string", "description": "Optional working dir relative to workspace root (no `..`)." },
                "login": { "type": "boolean", "description": "Whether to use login/profile shell semantics. Defaults to true." },
                "yield_time_ms": { "type": "integer", "minimum": 0, "description": "If >0, return after this time with partial output and a session_id if still running." },
                "max_output_bytes": { "type": "integer", "minimum": 1, "description": "Maximum bytes to return per call (stdout+stderr best effort)." }
            },
            "required": ["cmd"],
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
    cmd: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    login: Option<bool>,
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
    extra_workspace_roots: &[PathBuf],
    perms: &Permissions,
    shell_sessions: &ShellSessions,
    call: &ToolCall,
) -> Result<String, ToolError> {
    if matches!(perms.shell, ShellPermissions::DenyAll) {
        return Err(ToolError::PermissionDenied(TOOL_SHELL_COMMAND.to_string()));
    }
    let args: ShellCommandArgs = parse_args(call, TOOL_SHELL_COMMAND)?;
    if args.cmd.trim().is_empty() {
        return Err(ToolError::InvalidCommand(
            "cmd must not be empty".to_string(),
        ));
    }
    if matches!(perms.shell, ShellPermissions::AllowList(_)) && has_restricted_shell_meta(&args.cmd)
    {
        return Err(ToolError::PermissionDenied(format!(
            "shell command contains restricted shell syntax: {}",
            args.cmd
        )));
    }
    let permission_groups = command_permission_token_groups(&args.cmd);
    if !perms.shell.allows_all(&permission_groups) {
        return Err(ToolError::PermissionDenied(format!(
            "shell command not allowed: {}",
            args.cmd
        )));
    }
    let shell_argv = default_user_shell().derive_exec_args(&args.cmd, args.login.unwrap_or(true));

    let cwd = match args.cwd.as_deref() {
        None => workspace_root.to_path_buf(),
        Some(p) => resolve_workspace_path(workspace_root, extra_workspace_roots, p)?,
    };

    let yield_time_ms = args.yield_time_ms.unwrap_or(0);
    if yield_time_ms == 0 {
        let mut cmd = Command::new(&shell_argv[0]);
        cmd.args(&shell_argv[1..])
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

    let mut cmd = Command::new(&shell_argv[0]);
    cmd.args(&shell_argv[1..])
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

    let code = *session.exit_code.lock().await;
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

    let code = *sess.exit_code.lock().await;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserShellKind {
    Posix,
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Cmd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UserShell {
    path: PathBuf,
    kind: UserShellKind,
}

impl UserShell {
    fn derive_exec_args(&self, command: &str, use_login_shell: bool) -> Vec<String> {
        match self.kind {
            UserShellKind::Bash | UserShellKind::Zsh | UserShellKind::Posix => {
                let script = if use_login_shell {
                    self.profile_script(command)
                } else {
                    command.to_string()
                };
                vec![
                    self.path.to_string_lossy().to_string(),
                    if use_login_shell { "-lc" } else { "-c" }.to_string(),
                    script,
                ]
            }
            UserShellKind::Fish => {
                let mut args = vec![self.path.to_string_lossy().to_string()];
                if use_login_shell {
                    args.push("-l".to_string());
                }
                args.push("-c".to_string());
                args.push(command.to_string());
                args
            }
            UserShellKind::PowerShell => {
                let mut args = vec![self.path.to_string_lossy().to_string()];
                if !use_login_shell {
                    args.push("-NoProfile".to_string());
                }
                args.push("-Command".to_string());
                args.push(command.to_string());
                args
            }
            UserShellKind::Cmd => vec![
                self.path.to_string_lossy().to_string(),
                "/c".to_string(),
                command.to_string(),
            ],
        }
    }

    fn profile_script(&self, command: &str) -> String {
        let setup = match self.kind {
            UserShellKind::Bash => {
                r#"if [ -z "$BASH_ENV" ] && [ -r "$HOME/.bashrc" ]; then
  . "$HOME/.bashrc"
fi"#
            }
            UserShellKind::Zsh => {
                r#"if [ -n "$ZDOTDIR" ]; then
  __kiliax_zshrc="$ZDOTDIR/.zshrc"
else
  __kiliax_zshrc="$HOME/.zshrc"
fi
if [ -r "$__kiliax_zshrc" ]; then
  . "$__kiliax_zshrc"
fi
unset __kiliax_zshrc"#
            }
            UserShellKind::Posix => {
                r#"if [ -n "$ENV" ] && [ -r "$ENV" ]; then
  . "$ENV"
fi"#
            }
            _ => "",
        };

        if setup.is_empty() {
            command.to_string()
        } else {
            format!("{setup}\n{command}")
        }
    }
}

fn default_user_shell() -> UserShell {
    if let Some(path) = non_empty_env_path("SHELL") {
        return shell_from_path(path);
    }

    #[cfg(windows)]
    {
        if let Some(path) = find_program_in_path("pwsh") {
            return shell_from_path(path);
        }
        if let Some(path) = find_program_in_path("powershell") {
            return shell_from_path(path);
        }
        if let Some(path) = non_empty_env_path("COMSPEC") {
            return shell_from_path(path);
        }
        return shell_from_path(PathBuf::from("cmd.exe"));
    }

    #[cfg(not(windows))]
    {
        for path in ["/bin/bash", "/usr/bin/bash", "/bin/zsh", "/bin/sh"] {
            let path = PathBuf::from(path);
            if path.is_file() {
                return shell_from_path(path);
            }
        }
        shell_from_path(PathBuf::from("/bin/sh"))
    }
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    if value.as_os_str().is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn shell_from_path(path: PathBuf) -> UserShell {
    let kind = shell_kind_from_path(&path);
    UserShell { path, kind }
}

fn shell_kind_from_path(path: &Path) -> UserShellKind {
    let raw = path.to_string_lossy();
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw.as_ref())
        .to_ascii_lowercase();
    let name = name.trim_end_matches(".exe");
    match name {
        "bash" => UserShellKind::Bash,
        "zsh" => UserShellKind::Zsh,
        "fish" => UserShellKind::Fish,
        "pwsh" | "powershell" => UserShellKind::PowerShell,
        "cmd" => UserShellKind::Cmd,
        _ => UserShellKind::Posix,
    }
}

#[cfg(windows)]
fn find_program_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions = std::env::var_os("PATHEXT")
        .map(split_pathext)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()]);

    for dir in std::env::split_paths(&path) {
        let direct = dir.join(program);
        if direct.is_file() {
            return Some(direct);
        }
        for ext in &extensions {
            let candidate = dir.join(format!("{program}{}", ext.to_string_lossy()));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn split_pathext(value: OsString) -> Vec<OsString> {
    value
        .to_string_lossy()
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(OsString::from)
        .collect()
}

fn command_permission_token_groups(command: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for segment in split_shell_segments(command) {
        let segment = segment.trim();
        if segment.is_empty() || is_setup_shell_segment(segment) {
            continue;
        }

        for stage in split_shell_pipeline(segment) {
            let mut words = split_shell_words(stage);
            if words.first().is_some_and(|w| w == "env") {
                words.remove(0);
            }
            while words.first().is_some_and(|w| is_env_assignment_token(w)) {
                words.remove(0);
            }
            if !words.is_empty() {
                out.push(words);
            }
        }
    }

    out
}

fn split_shell_segments(script: &str) -> Vec<&str> {
    split_shell_top_level(script, &["&&", ";", "\n"])
}

fn split_shell_pipeline(segment: &str) -> Vec<&str> {
    split_shell_top_level(segment, &["|"])
}

fn split_shell_top_level<'a>(text: &'a str, separators: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut quote: Option<char> = None;
    let mut escape = false;
    let mut iter = text.char_indices().peekable();

    while let Some((idx, ch)) = iter.next() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }

        for sep in separators {
            if text[idx..].starts_with(sep) {
                out.push(text[start..idx].trim());
                start = idx + sep.len();
                for _ in 1..sep.chars().count() {
                    let _ = iter.next();
                }
                break;
            }
        }
    }

    out.push(text[start..].trim());
    out
}

fn split_shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escape = false;

    for ch in s.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                cur.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(ch);
    }

    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_setup_shell_segment(segment: &str) -> bool {
    let words = split_shell_words(segment);
    let first = words.first().map(String::as_str);
    if matches!(
        first,
        Some("cd" | "export" | "source" | "." | "unset" | "set")
    ) {
        return true;
    }
    !words.is_empty() && words.iter().all(|w| is_env_assignment_token(w))
}

fn is_env_assignment_token(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    matches!(chars.next(), Some('_') | Some('a'..='z') | Some('A'..='Z'))
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn has_restricted_shell_meta(command: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut escape = false;
    let chars: Vec<char> = command.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if ch == '\\' {
            escape = true;
            i += 1;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else if q == '"' && (ch == '`' || (ch == '$' && chars.get(i + 1) == Some(&'('))) {
                return true;
            }
            i += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            i += 1;
            continue;
        }
        if ch == '<' || ch == '>' || ch == '`' || (ch == '$' && chars.get(i + 1) == Some(&'(')) {
            return true;
        }
        if ch == '&' {
            if chars.get(i + 1) == Some(&'&') {
                i += 2;
                continue;
            }
            return true;
        }
        i += 1;
    }

    false
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_login_command_sources_bashrc_before_user_command() {
        let shell = shell_from_path(PathBuf::from("/bin/bash"));
        let argv = shell.derive_exec_args("echo ok", true);

        assert_eq!(argv[0], "/bin/bash");
        assert_eq!(argv[1], "-lc");
        assert!(argv[2].contains(r#". "$HOME/.bashrc""#));
        assert!(argv[2].ends_with("echo ok"));
    }

    #[test]
    fn bash_non_login_command_does_not_source_profile() {
        let shell = shell_from_path(PathBuf::from("/bin/bash"));
        let argv = shell.derive_exec_args("echo ok", false);

        assert_eq!(argv, vec!["/bin/bash", "-c", "echo ok"]);
    }

    #[test]
    fn command_permission_token_groups_skip_setup_and_env() {
        let groups = command_permission_token_groups(
            "cd /tmp && FOO=bar BAR=baz rg -n shell_command crates | head -n 5",
        );

        assert_eq!(
            groups,
            vec![
                vec!["rg", "-n", "shell_command", "crates"],
                vec!["head", "-n", "5"],
            ]
        );
    }

    #[test]
    fn command_permission_token_groups_keep_git_subcommand() {
        let groups = command_permission_token_groups("git status --short");

        assert_eq!(groups, vec![vec!["git", "status", "--short"]]);
    }

    #[test]
    fn restricted_shell_meta_detects_write_and_substitution_syntax() {
        assert!(has_restricted_shell_meta("rg foo > out.txt"));
        assert!(has_restricted_shell_meta("echo $(rm -rf /)"));
        assert!(has_restricted_shell_meta("echo `rm -rf /`"));
        assert!(has_restricted_shell_meta("rg foo & rm -rf /"));
        assert!(!has_restricted_shell_meta("rg foo && head file"));
        assert!(!has_restricted_shell_meta("rg '>' file"));
    }

    #[test]
    fn shell_kind_detection_handles_windows_exe_case() {
        assert_eq!(
            shell_kind_from_path(Path::new(r"C:\Windows\System32\CMD.EXE")),
            UserShellKind::Cmd
        );
        assert_eq!(
            shell_kind_from_path(Path::new(r"C:\Program Files\PowerShell\7\pwsh.EXE")),
            UserShellKind::PowerShell
        );
    }
}
