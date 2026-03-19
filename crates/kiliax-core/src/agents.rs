use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::llm::{Message, ToolCall, ToolDefinition};

pub const TOOL_READ_FILE: &str = "read_file";
pub const TOOL_WRITE_FILE: &str = "write_file";
pub const TOOL_SHELL: &str = "shell";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Plan,
    Build,
}

#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub kind: AgentKind,
    pub name: &'static str,
    pub developer_prompt: &'static str,
    pub tools: Vec<ToolDefinition>,
    pub permissions: Permissions,
}

impl AgentProfile {
    pub fn plan() -> Self {
        Self {
            kind: AgentKind::Plan,
            name: "plan",
            developer_prompt: PLAN_PROMPT,
            tools: vec![tool_read_file(), tool_shell()],
            permissions: Permissions::plan(),
        }
    }

    pub fn build() -> Self {
        Self {
            kind: AgentKind::Build,
            name: "build",
            developer_prompt: BUILD_PROMPT,
            tools: vec![tool_read_file(), tool_write_file(), tool_shell()],
            permissions: Permissions::build(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permissions {
    pub file_read: bool,
    pub file_write: bool,
    pub shell: ShellPermissions,
}

impl Permissions {
    pub fn plan() -> Self {
        Self {
            file_read: true,
            file_write: false,
            shell: ShellPermissions::AllowList(vec![
                vec!["ls".into()],
                vec!["cat".into()],
                vec!["rg".into()],
                vec!["find".into()],
                vec!["sed".into()],
                vec!["head".into()],
                vec!["tail".into()],
                vec!["pwd".into()],
                vec!["git".into(), "status".into()],
                vec!["git".into(), "diff".into()],
            ]),
        }
    }

    pub fn build() -> Self {
        Self {
            file_read: true,
            file_write: true,
            shell: ShellPermissions::AllowAll,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellPermissions {
    DenyAll,
    AllowAll,
    /// Allow when argv begins with one of the prefixes (exact token match).
    AllowList(Vec<Vec<String>>),
}

impl ShellPermissions {
    pub fn allows(&self, argv: &[String]) -> bool {
        match self {
            ShellPermissions::DenyAll => false,
            ShellPermissions::AllowAll => true,
            ShellPermissions::AllowList(prefixes) => prefixes.iter().any(|p| is_prefix(p, argv)),
        }
    }
}

fn is_prefix(prefix: &[String], argv: &[String]) -> bool {
    if prefix.is_empty() || argv.len() < prefix.len() {
        return false;
    }
    prefix
        .iter()
        .zip(argv.iter())
        .all(|(a, b)| a.as_str() == b.as_str())
}

#[derive(Debug, Clone)]
pub struct ToolRuntime {
    workspace_root: PathBuf,
}

impl ToolRuntime {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub async fn execute(&self, perms: &Permissions, call: &ToolCall) -> Result<String, ToolError> {
        match call.name.as_str() {
            TOOL_READ_FILE => {
                if !perms.file_read {
                    return Err(ToolError::PermissionDenied("read_file".to_string()));
                }
                let args: ReadFileArgs =
                    parse_args(call).map_err(|e| ToolError::InvalidArgs {
                        tool: TOOL_READ_FILE.to_string(),
                        source: e,
                    })?;
                let path = resolve_workspace_path(&self.workspace_root, &args.path)?;
                Ok(tokio::fs::read_to_string(path).await?)
            }
            TOOL_WRITE_FILE => {
                if !perms.file_write {
                    return Err(ToolError::PermissionDenied("write_file".to_string()));
                }
                let args: WriteFileArgs =
                    parse_args(call).map_err(|e| ToolError::InvalidArgs {
                        tool: TOOL_WRITE_FILE.to_string(),
                        source: e,
                    })?;
                let path = resolve_workspace_path(&self.workspace_root, &args.path)?;
                if args.create_dirs {
                    if let Some(parent) = path.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                }
                tokio::fs::write(path, args.content).await?;
                Ok("ok".to_string())
            }
            TOOL_SHELL => {
                if matches!(perms.shell, ShellPermissions::DenyAll) {
                    return Err(ToolError::PermissionDenied("shell".to_string()));
                }
                let args: ShellArgs =
                    parse_args(call).map_err(|e| ToolError::InvalidArgs {
                        tool: TOOL_SHELL.to_string(),
                        source: e,
                    })?;
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
                    None => self.workspace_root.clone(),
                    Some(p) => resolve_workspace_path(&self.workspace_root, p)?,
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
            other => Err(ToolError::UnknownTool(other.to_string())),
        }
    }

    pub async fn execute_to_message(
        &self,
        perms: &Permissions,
        call: &ToolCall,
    ) -> Result<Message, ToolError> {
        let content = self.execute(perms, call).await?;
        Ok(Message::Tool {
            tool_call_id: call.id.clone(),
            content,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("unknown tool: {0}")]
    UnknownTool(String),

    #[error("invalid args for {tool}: {source}")]
    InvalidArgs {
        tool: String,
        source: serde_json::Error,
    },

    #[error("invalid path {path:?}: {reason}")]
    InvalidPath { path: String, reason: String },

    #[error("invalid command: {0}")]
    InvalidCommand(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn parse_args<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T, serde_json::Error> {
    serde_json::from_str::<T>(&call.arguments)
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

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
    #[serde(default)]
    create_dirs: bool,
}

#[derive(Debug, Deserialize)]
struct ShellArgs {
    argv: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

fn tool_read_file() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_READ_FILE.to_string(),
        description: Some("Read a UTF-8 text file from the workspace.".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to workspace root." }
            },
            "required": ["path"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

fn tool_write_file() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_WRITE_FILE.to_string(),
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

fn tool_shell() -> ToolDefinition {
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

const PLAN_PROMPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/plan.md"));
const BUILD_PROMPT: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/build.md"));

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn plan_agent_denies_write_file() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = ToolRuntime::new(tmp.path());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: TOOL_WRITE_FILE.to_string(),
            arguments: r#"{"path":"a.txt","content":"x"}"#.to_string(),
        };

        let err = rt.execute(&Permissions::plan(), &call).await.unwrap_err();
        let ToolError::PermissionDenied(_) = err else {
            panic!("unexpected error: {err:?}");
        };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_agent_denies_disallowed_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = ToolRuntime::new(tmp.path());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: TOOL_SHELL.to_string(),
            arguments: r#"{"argv":["rm","-rf","/"]}"#.to_string(),
        };

        let err = rt.execute(&Permissions::plan(), &call).await.unwrap_err();
        let ToolError::PermissionDenied(_) = err else {
            panic!("unexpected error: {err:?}");
        };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn build_agent_can_write_file() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = ToolRuntime::new(tmp.path());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: TOOL_WRITE_FILE.to_string(),
            arguments: r#"{"path":"a.txt","content":"hello"}"#.to_string(),
        };

        let out = rt.execute(&Permissions::build(), &call).await.unwrap();
        assert_eq!(out, "ok");
        let s = tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap();
        assert_eq!(s, "hello");
    }
}
