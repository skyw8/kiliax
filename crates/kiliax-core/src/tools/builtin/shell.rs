#[cfg(windows)]
use std::ffi::OsString;
#[cfg(unix)]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::{Permissions, ShellPermissions, ToolError};

use super::common::{parse_args, resolve_workspace_path};
use super::{TOOL_SHELL_COMMAND, TOOL_WRITE_STDIN};

const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_SESSION_BUFFER_BYTES: usize = 1024 * 1024;
const APPROX_BYTES_PER_TOKEN: usize = 4;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub fn shell_command_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_SHELL_COMMAND.to_string(),
        description: Some(
            "Run a command in the workspace through a shell. The command inherits the full kiliax process environment and uses login/profile shell semantics by default. If the result includes `session_id`, use `write_stdin` to interact/poll."
                .to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "description": "Shell command string to execute." },
                "cwd": { "type": "string", "description": "Optional working dir relative to workspace root (no `..`)." },
                "shell": { "type": "string", "description": "Optional shell binary to launch. Defaults to the user's default shell." },
                "login": { "type": "boolean", "description": "Whether to use login/profile shell semantics. Defaults to true." },
                "tty": { "type": "boolean", "description": "Whether to allocate a TTY for the command. Unix only; other platforms fall back to pipes." },
                "timeout_ms": { "type": "integer", "minimum": 1, "description": "Maximum command runtime in milliseconds before termination." },
                "yield_time_ms": { "type": "integer", "minimum": 0, "description": "If >0, return after this time with partial output and a session_id if still running." },
                "max_output_tokens": { "type": "integer", "minimum": 1, "description": "Approximate maximum output tokens to return per call." },
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
                "max_output_tokens": { "type": "integer", "minimum": 1, "description": "Approximate maximum output tokens to return per call." },
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
    stdin: tokio::sync::Mutex<Option<SessionStdin>>,
    process: tokio::sync::Mutex<Option<SessionProcess>>,
    stdout: StdMutex<OutputBuffer>,
    stderr: StdMutex<OutputBuffer>,
    exit_code: tokio::sync::Mutex<Option<i32>>,
    timed_out: tokio::sync::Mutex<bool>,
    started_at: Instant,
    timeout_at: Option<Instant>,
    tty: bool,
}

impl ShellSession {
    fn new(
        stdin: Option<SessionStdin>,
        process: SessionProcess,
        timeout_ms: Option<u64>,
        tty: bool,
    ) -> Self {
        Self {
            stdin: tokio::sync::Mutex::new(stdin),
            process: tokio::sync::Mutex::new(Some(process)),
            stdout: StdMutex::new(OutputBuffer::default()),
            stderr: StdMutex::new(OutputBuffer::default()),
            exit_code: tokio::sync::Mutex::new(None),
            timed_out: tokio::sync::Mutex::new(false),
            started_at: Instant::now(),
            timeout_at: timeout_ms.map(|ms| Instant::now() + Duration::from_millis(ms)),
            tty,
        }
    }

    async fn mark_exit(&self, code: i32) {
        let mut slot = self.exit_code.lock().await;
        if slot.is_none() {
            *slot = Some(code);
        }
    }

    async fn mark_timed_out(&self) {
        let mut slot = self.timed_out.lock().await;
        *slot = true;
    }

    async fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock().await
    }

    async fn timed_out(&self) -> bool {
        *self.timed_out.lock().await
    }

    fn wall_time_ms(&self) -> u64 {
        self.started_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

enum SessionStdin {
    Pipe(tokio::process::ChildStdin),
    #[cfg(unix)]
    Blocking(Arc<StdMutex<Box<dyn Write + Send>>>),
}

enum SessionProcess {
    Pipe(tokio::process::Child),
    #[cfg(unix)]
    Pty(Box<dyn portable_pty::Child + Send + Sync>),
}

impl SessionProcess {
    fn try_wait_exit_code(&mut self) -> std::io::Result<Option<i32>> {
        match self {
            SessionProcess::Pipe(child) => child
                .try_wait()
                .map(|status| status.map(|s| s.code().unwrap_or(-1))),
            #[cfg(unix)]
            SessionProcess::Pty(child) => child
                .try_wait()
                .map(|status| status.map(|s| s.exit_code() as i32)),
        }
    }

    fn terminate(&mut self) {
        match self {
            SessionProcess::Pipe(child) => {
                let _ = child.start_kill();
            }
            #[cfg(unix)]
            SessionProcess::Pty(child) => {
                let _ = child.kill();
            }
        }
    }
}

#[derive(Default)]
struct OutputBuffer {
    text: String,
    truncated_before: bool,
}

impl OutputBuffer {
    fn push_lossy(&mut self, bytes: &[u8]) {
        self.text.push_str(&String::from_utf8_lossy(bytes));
        if self.text.len() > MAX_SESSION_BUFFER_BYTES {
            let drop_bytes = self.text.len() - MAX_SESSION_BUFFER_BYTES;
            let split = next_char_boundary(&self.text, drop_bytes);
            self.text.drain(..split);
            self.truncated_before = true;
        }
    }

    fn drain_with_limit(&mut self, max_bytes: usize) -> DrainOutput {
        let was_truncated = self.truncated_before;
        self.truncated_before = false;

        if self.text.is_empty() {
            return DrainOutput {
                text: String::new(),
                truncated: was_truncated,
            };
        }
        if self.text.len() <= max_bytes {
            return DrainOutput {
                text: std::mem::take(&mut self.text),
                truncated: was_truncated,
            };
        }

        let split_idx = prev_char_boundary(&self.text, max_bytes);
        let out = self.text[..split_idx].to_string();
        self.text.drain(..split_idx);
        DrainOutput {
            text: out,
            truncated: true,
        }
    }
}

struct DrainOutput {
    text: String,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct ShellCommandArgs {
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    login: Option<bool>,
    #[serde(default)]
    tty: Option<bool>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
    #[serde(default, rename = "description")]
    _description: Option<String>,
}

impl ShellCommandArgs {
    fn command(&self) -> Result<&str, ToolError> {
        match (
            non_empty_str(self.cmd.as_deref()),
            non_empty_str(self.command.as_deref()),
        ) {
            (Some(cmd), Some(command)) if cmd != command => Err(ToolError::InvalidCommand(
                "cmd and command must not conflict".to_string(),
            )),
            (Some(cmd), _) => Ok(cmd),
            (_, Some(command)) => Ok(command),
            _ => Err(ToolError::InvalidCommand(
                "cmd or command must not be empty".to_string(),
            )),
        }
    }

    fn cwd(&self) -> Result<Option<&str>, ToolError> {
        match (
            non_empty_str(self.cwd.as_deref()),
            non_empty_str(self.workdir.as_deref()),
        ) {
            (Some(cwd), Some(workdir)) if cwd != workdir => Err(ToolError::InvalidPath {
                path: format!("cwd={cwd:?}, workdir={workdir:?}"),
                reason: "cwd and workdir must not conflict".to_string(),
            }),
            (Some(cwd), _) => Ok(Some(cwd)),
            (_, Some(workdir)) => Ok(Some(workdir)),
            _ => Ok(None),
        }
    }

    fn timeout_ms(&self) -> Result<Option<u64>, ToolError> {
        match (self.timeout_ms, self.timeout) {
            (Some(timeout_ms), Some(timeout)) if timeout_ms != timeout => Err(
                ToolError::InvalidCommand("timeout_ms and timeout must not conflict".to_string()),
            ),
            (Some(timeout_ms), _) => Ok(Some(timeout_ms)),
            (_, Some(timeout)) => Ok(Some(timeout)),
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Serialize)]
struct ShellCommandOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<u64>,
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    timed_out: bool,
    wall_time_ms: u64,
    truncated: bool,
    tty: bool,
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
    let command = args.command()?;
    if matches!(perms.shell, ShellPermissions::AllowList(_)) && has_restricted_shell_meta(command) {
        return Err(ToolError::PermissionDenied(format!(
            "shell command contains restricted shell syntax: {}",
            command
        )));
    }
    let permission_groups = command_permission_token_groups(command);
    if !perms.shell.allows_all(&permission_groups) {
        return Err(ToolError::PermissionDenied(format!(
            "shell command not allowed: {}",
            command
        )));
    }
    let shell = args
        .shell
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| shell_from_path(PathBuf::from(s.trim())))
        .unwrap_or_else(default_user_shell);
    let shell_argv = shell.derive_exec_args(command, args.login.unwrap_or(true));

    let cwd = match args.cwd()? {
        None => workspace_root.to_path_buf(),
        Some(p) => resolve_workspace_path(workspace_root, extra_workspace_roots, p)?,
    };

    let yield_time_ms = args.yield_time_ms.unwrap_or(0);
    let requested_tty = args.tty.unwrap_or(false);
    let session = spawn_shell_session(shell_argv, cwd, args.timeout_ms()?, requested_tty).await?;

    let id = if yield_time_ms > 0 {
        let id = shell_sessions.alloc_id();
        shell_sessions.insert(id, session.clone()).await;
        Some(id)
    } else {
        None
    };

    wait_for_shell_session(
        &session,
        Duration::from_millis(yield_time_ms),
        yield_time_ms == 0,
    )
    .await;

    let out = render_shell_output(&session, args.max_output_bytes, args.max_output_tokens).await;
    let code = session.exit_code().await;
    let running = code.is_none();
    if !running {
        if let Some(id) = id {
            let _ = shell_sessions.remove(id).await;
        }
    }

    let out = ShellCommandOutput {
        session_id: id.filter(|_| running),
        running,
        exit_code: code,
        timed_out: session.timed_out().await,
        wall_time_ms: session.wall_time_ms(),
        truncated: out.truncated,
        tty: session.tty,
        stdout: out.stdout,
        stderr: out.stderr,
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
    max_output_tokens: Option<usize>,
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
            write_session_stdin(stdin, chars.as_bytes()).await?;
        }
    }

    let yield_ms = args.yield_time_ms.unwrap_or(0);
    wait_for_shell_session(&sess, Duration::from_millis(yield_ms), false).await;

    let out = render_shell_output(&sess, args.max_output_bytes, args.max_output_tokens).await;
    let code = sess.exit_code().await;
    let running = code.is_none();
    if !running {
        let _ = shell_sessions.remove(args.session_id).await;
    }

    let out = ShellCommandOutput {
        session_id: if running { Some(args.session_id) } else { None },
        running,
        exit_code: code,
        timed_out: sess.timed_out().await,
        wall_time_ms: sess.wall_time_ms(),
        truncated: out.truncated,
        tty: sess.tty,
        stdout: out.stdout,
        stderr: out.stderr,
    };
    Ok(serde_json::to_string(&out).unwrap_or_else(|_| "ok".to_string()))
}

struct RenderedOutput {
    stdout: String,
    stderr: String,
    truncated: bool,
}

#[cfg(unix)]
async fn spawn_shell_session(
    shell_argv: Vec<String>,
    cwd: PathBuf,
    timeout_ms: Option<u64>,
    requested_tty: bool,
) -> Result<Arc<ShellSession>, ToolError> {
    if requested_tty {
        return spawn_pty_shell_session(shell_argv, cwd, timeout_ms).await;
    }

    spawn_pipe_shell_session(shell_argv, cwd, timeout_ms, false).await
}

#[cfg(not(unix))]
async fn spawn_shell_session(
    shell_argv: Vec<String>,
    cwd: PathBuf,
    timeout_ms: Option<u64>,
    _requested_tty: bool,
) -> Result<Arc<ShellSession>, ToolError> {
    spawn_pipe_shell_session(shell_argv, cwd, timeout_ms, false).await
}

async fn spawn_pipe_shell_session(
    shell_argv: Vec<String>,
    cwd: PathBuf,
    timeout_ms: Option<u64>,
    tty: bool,
) -> Result<Arc<ShellSession>, ToolError> {
    let mut cmd = Command::new(&shell_argv[0]);
    cmd.args(&shell_argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take().map(SessionStdin::Pipe);
    let session = Arc::new(ShellSession::new(
        stdin,
        SessionProcess::Pipe(child),
        timeout_ms,
        tty,
    ));

    if let Some(stdout) = stdout {
        spawn_async_output_reader(stdout, session.clone(), OutputStream::Stdout);
    }
    if let Some(stderr) = stderr {
        spawn_async_output_reader(stderr, session.clone(), OutputStream::Stderr);
    }
    spawn_process_watcher(session.clone());
    Ok(session)
}

#[cfg(unix)]
async fn spawn_pty_shell_session(
    shell_argv: Vec<String>,
    cwd: PathBuf,
    timeout_ms: Option<u64>,
) -> Result<Arc<ShellSession>, ToolError> {
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| ToolError::InvalidCommand(format!("failed to open PTY: {err}")))?;

    let mut builder = portable_pty::CommandBuilder::new(&shell_argv[0]);
    for arg in &shell_argv[1..] {
        builder.arg(arg);
    }
    builder.cwd(cwd);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| ToolError::InvalidCommand(format!("failed to clone PTY reader: {err}")))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|err| ToolError::InvalidCommand(format!("failed to open PTY writer: {err}")))?;
    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|err| ToolError::InvalidCommand(format!("failed to spawn PTY command: {err}")))?;
    drop(pair.slave);

    let session = Arc::new(ShellSession::new(
        Some(SessionStdin::Blocking(Arc::new(StdMutex::new(writer)))),
        SessionProcess::Pty(child),
        timeout_ms,
        true,
    ));
    spawn_blocking_output_reader(reader, session.clone());
    spawn_process_watcher(session.clone());
    Ok(session)
}

enum OutputStream {
    Stdout,
    Stderr,
}

fn spawn_async_output_reader<R>(reader: R, session: Arc<ShellSession>, stream: OutputStream)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut rd = tokio::io::BufReader::new(reader);
        let mut tmp = [0u8; 8192];
        loop {
            match tokio::io::AsyncReadExt::read(&mut rd, &mut tmp).await {
                Ok(0) => break,
                Ok(n) => append_output(&session, &stream, &tmp[..n]),
                Err(_) => break,
            }
        }
    });
}

#[cfg(unix)]
fn spawn_blocking_output_reader(mut reader: Box<dyn Read + Send>, session: Arc<ShellSession>) {
    std::thread::spawn(move || {
        let mut tmp = [0u8; 8192];
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => append_output(&session, &OutputStream::Stdout, &tmp[..n]),
                Err(_) => break,
            }
        }
    });
}

fn append_output(session: &ShellSession, stream: &OutputStream, bytes: &[u8]) {
    let lock = match stream {
        OutputStream::Stdout => &session.stdout,
        OutputStream::Stderr => &session.stderr,
    };
    if let Ok(mut out) = lock.lock() {
        out.push_lossy(bytes);
    }
}

fn spawn_process_watcher(session: Arc<ShellSession>) {
    tokio::spawn(async move {
        loop {
            let mut exit_code = None;
            let mut timed_out = false;
            {
                let mut guard = session.process.lock().await;
                if let Some(process) = guard.as_mut() {
                    match process.try_wait_exit_code() {
                        Ok(Some(code)) => {
                            exit_code = Some(code);
                        }
                        Ok(None) => {
                            if session
                                .timeout_at
                                .is_some_and(|deadline| Instant::now() >= deadline)
                            {
                                process.terminate();
                                timed_out = true;
                                exit_code = Some(-1);
                            }
                        }
                        Err(_) => {
                            exit_code = Some(-1);
                        }
                    }
                } else {
                    exit_code = Some(-1);
                }

                if exit_code.is_some() {
                    guard.take();
                }
            }

            if timed_out {
                session.mark_timed_out().await;
            }
            if let Some(code) = exit_code {
                session.mark_exit(code).await;
                let mut stdin = session.stdin.lock().await;
                stdin.take();
                break;
            }
            tokio::time::sleep(PROCESS_POLL_INTERVAL).await;
        }
    });
}

async fn wait_for_shell_session(session: &ShellSession, duration: Duration, until_exit: bool) {
    if !until_exit {
        if !duration.is_zero() {
            tokio::time::sleep(duration).await;
        }
        return;
    }

    loop {
        if session.exit_code().await.is_some() {
            break;
        }
        tokio::time::sleep(PROCESS_POLL_INTERVAL).await;
    }
}

async fn write_session_stdin(stdin: &mut SessionStdin, bytes: &[u8]) -> Result<(), ToolError> {
    match stdin {
        SessionStdin::Pipe(stdin) => {
            stdin.write_all(bytes).await?;
            stdin.flush().await?;
        }
        #[cfg(unix)]
        SessionStdin::Blocking(writer) => {
            let writer = writer.clone();
            let bytes = bytes.to_vec();
            tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                let mut writer = writer
                    .lock()
                    .map_err(|_| std::io::Error::other("PTY writer lock poisoned"))?;
                writer.write_all(&bytes)?;
                writer.flush()
            })
            .await
            .map_err(|err| ToolError::InvalidCommand(format!("failed to write stdin: {err}")))??;
        }
    }
    Ok(())
}

async fn render_shell_output(
    session: &ShellSession,
    max_output_bytes: Option<usize>,
    max_output_tokens: Option<usize>,
) -> RenderedOutput {
    let max = output_byte_budget(max_output_bytes, max_output_tokens);
    let stdout = session
        .stdout
        .lock()
        .map(|mut out| out.drain_with_limit(max))
        .unwrap_or(DrainOutput {
            text: String::new(),
            truncated: true,
        });
    let remaining = max.saturating_sub(stdout.text.len());
    let stderr = session
        .stderr
        .lock()
        .map(|mut out| out.drain_with_limit(remaining))
        .unwrap_or(DrainOutput {
            text: String::new(),
            truncated: true,
        });

    RenderedOutput {
        stdout: stdout.text,
        stderr: stderr.text,
        truncated: stdout.truncated || stderr.truncated,
    }
}

fn output_byte_budget(max_output_bytes: Option<usize>, max_output_tokens: Option<usize>) -> usize {
    let by_bytes = max_output_bytes.unwrap_or(DEFAULT_MAX_OUTPUT_BYTES).max(1);
    let by_tokens = max_output_tokens
        .map(|tokens| tokens.max(1).saturating_mul(APPROX_BYTES_PER_TOKEN))
        .unwrap_or(usize::MAX);
    by_bytes.min(by_tokens).max(1)
}

fn non_empty_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn prev_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn next_char_boundary(s: &str, min_bytes: usize) -> usize {
    if min_bytes >= s.len() {
        return s.len();
    }
    let mut idx = min_bytes;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
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
    #[cfg(windows)]
    {
        if let Some(path) = non_empty_env_path("SHELL").and_then(existing_shell_path) {
            return shell_from_path(path);
        }
        if let Some(path) = find_program_in_path("pwsh") {
            return shell_from_path(path);
        }
        if let Some(path) = find_program_in_path("powershell") {
            return shell_from_path(path);
        }
        if let Some(path) = non_empty_env_path("COMSPEC") {
            return shell_from_path(path);
        }
        shell_from_path(PathBuf::from("cmd.exe"))
    }

    #[cfg(not(windows))]
    {
        if let Some(path) = non_empty_env_path("SHELL") {
            return shell_from_path(path);
        }
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

#[cfg(windows)]
fn existing_shell_path(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolCall;
    use crate::tools::{Permissions, ShellPermissions};

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

    #[cfg(windows)]
    #[test]
    fn windows_shell_env_must_point_to_existing_file() {
        assert!(existing_shell_path(PathBuf::from(r"C:\definitely\missing\bash.exe")).is_none());
    }

    fn allow_all_permissions() -> Permissions {
        Permissions {
            file_read: true,
            file_write: true,
            shell: ShellPermissions::AllowAll,
        }
    }

    fn tool_call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            name: TOOL_SHELL_COMMAND.to_string(),
            arguments: args.to_string(),
        }
    }

    fn stdin_call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_2".to_string(),
            name: TOOL_WRITE_STDIN.to_string(),
            arguments: args.to_string(),
        }
    }

    async fn run_shell(tmp: &Path, args: serde_json::Value) -> serde_json::Value {
        let sessions = ShellSessions::new();
        let out = execute_shell_command(
            tmp,
            &[],
            &allow_all_permissions(),
            &sessions,
            &tool_call(args),
        )
        .await
        .unwrap();
        serde_json::from_str(&out).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_returns_status_and_wall_time() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_shell(
            tmp.path(),
            serde_json::json!({
                "cmd": "printf ok",
                "login": false
            }),
        )
        .await;

        assert_eq!(out["running"], false);
        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["stdout"], "ok");
        assert_eq!(out["timed_out"], false);
        assert!(out["wall_time_ms"].as_u64().is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_accepts_codex_shell_command_args() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(tmp.path().join("sub")).await.unwrap();
        let out = run_shell(
            tmp.path(),
            serde_json::json!({
                "command": "printf codex",
                "workdir": "sub",
                "login": false,
                "timeout_ms": 1_000
            }),
        )
        .await;

        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["stdout"], "codex");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_accepts_opencode_bash_args() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_shell(
            tmp.path(),
            serde_json::json!({
                "command": "printf opencode",
                "description": "Prints opencode marker",
                "timeout": 1_000,
                "login": false
            }),
        )
        .await;

        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["stdout"], "opencode");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_rejects_conflicting_command_aliases() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = ShellSessions::new();
        let err = execute_shell_command(
            tmp.path(),
            &[],
            &allow_all_permissions(),
            &sessions,
            &tool_call(serde_json::json!({
                "cmd": "printf a",
                "command": "printf b"
            })),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ToolError::InvalidCommand(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_timeout_terminates_process() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_shell(
            tmp.path(),
            serde_json::json!({
                "cmd": "sleep 2",
                "login": false,
                "timeout_ms": 50
            }),
        )
        .await;

        assert_eq!(out["running"], false);
        assert_eq!(out["timed_out"], true);
        assert_eq!(out["exit_code"], -1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn write_stdin_interacts_with_running_session() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = ShellSessions::new();
        #[cfg(windows)]
        let command = "$line = [Console]::In.ReadLine(); Write-Output ('got:' + $line)";
        #[cfg(not(windows))]
        let command = "read line; echo got:$line";
        let first_args = serde_json::json!({
            "cmd": command,
            "login": false,
            "yield_time_ms": 20
        });
        #[cfg(windows)]
        {
            first_args["shell"] = serde_json::json!("powershell.exe");
        }

        let first = execute_shell_command(
            tmp.path(),
            &[],
            &allow_all_permissions(),
            &sessions,
            &tool_call(first_args),
        )
        .await
        .unwrap();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        let session_id = first["session_id"].as_u64().unwrap();

        let second = execute_write_stdin(
            &allow_all_permissions(),
            &sessions,
            &stdin_call(serde_json::json!({
                "session_id": session_id,
                "chars": "hello\n",
                "yield_time_ms": 1000
            })),
        )
        .await
        .unwrap();
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();

        assert_eq!(second["running"], false);
        assert_eq!(second["exit_code"], 0);
        assert_eq!(second["stdout"].as_str().unwrap_or("").trim(), "got:hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_truncates_output_by_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_shell(
            tmp.path(),
            serde_json::json!({
                "cmd": "printf abcdef",
                "login": false,
                "max_output_bytes": 3
            }),
        )
        .await;

        assert_eq!(out["stdout"], "abc");
        assert_eq!(out["truncated"], true);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_truncates_output_by_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_shell(
            tmp.path(),
            serde_json::json!({
                "cmd": "printf abcdef",
                "login": false,
                "max_output_tokens": 1
            }),
        )
        .await;

        assert_eq!(out["stdout"], "abcd");
        assert_eq!(out["truncated"], true);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_accepts_model_provided_shell() {
        let tmp = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let (shell, cmd) = ("cmd.exe", "echo sh-ok");
        #[cfg(not(windows))]
        let (shell, cmd) = ("/bin/sh", "printf sh-ok");

        let out = run_shell(
            tmp.path(),
            serde_json::json!({
                "cmd": cmd,
                "shell": shell,
                "login": false
            }),
        )
        .await;

        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["stdout"].as_str().unwrap_or("").trim(), "sh-ok");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn shell_command_tty_allocates_unix_pty() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run_shell(
            tmp.path(),
            serde_json::json!({
                "cmd": "[ -t 1 ] && printf tty || printf pipe",
                "login": false,
                "tty": true
            }),
        )
        .await;

        assert_eq!(out["tty"], true);
        assert_eq!(out["exit_code"], 0);
        assert!(out["stdout"].as_str().unwrap_or("").contains("tty"));
    }
}
