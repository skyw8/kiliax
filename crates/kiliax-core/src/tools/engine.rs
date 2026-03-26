use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::llm::{Message, ToolCall, ToolDefinition};
use crate::tools::{builtin, Permissions, ToolError};

#[derive(Debug, Default)]
struct McpState {
    connected: BTreeMap<String, ConnectedMcpServer>,
    connecting: BTreeMap<String, ConnectedMcpServer>,
    retry: BTreeMap<String, McpRetry>,
}

#[derive(Debug, Clone)]
struct McpRetry {
    attempt: u32,
    next_attempt_at: Instant,
    last_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConnectedMcpServer {
    command: String,
    args: Vec<String>,
}

impl ConnectedMcpServer {
    fn from_cfg(cfg: &crate::config::McpServerConfig) -> Self {
        Self {
            command: cfg.command.clone(),
            args: cfg.args.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ToolEngine {
    workspace_root: PathBuf,
    shell_sessions: Arc<builtin::ShellSessions>,
    config: Arc<RwLock<Arc<crate::config::Config>>>,
    mcp: crate::tools::mcp::McpHub,
    mcp_state: Arc<Mutex<McpState>>,
}

impl ToolEngine {
    pub fn new(workspace_root: impl Into<PathBuf>, config: crate::config::Config) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            shell_sessions: Arc::new(builtin::ShellSessions::new()),
            config: Arc::new(RwLock::new(Arc::new(config))),
            mcp: crate::tools::mcp::McpHub::new(),
            mcp_state: Arc::new(Mutex::new(McpState::default())),
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn set_config(&self, config: crate::config::Config) -> Result<(), ToolError> {
        let mut guard = self.config.write().map_err(|_| {
            ToolError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "tool config lock poisoned",
            ))
        })?;
        *guard = Arc::new(config);
        Ok(())
    }

    pub async fn extra_tool_definitions(&self) -> Vec<ToolDefinition> {
        let cfg = self
            .config
            .read()
            .map_err(|_| {
                ToolError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "tool config lock poisoned",
                ))
            })
            .ok()
            .map(|g| g.clone());

        let Some(cfg) = cfg else {
            return Vec::new();
        };

        self.sync_mcp_servers_background(cfg.as_ref()).await;

        self.mcp.tool_definitions().await
    }

    pub async fn mcp_status(&self) -> Vec<McpServerStatus> {
        let cfg = self
            .config
            .read()
            .map_err(|_| {
                ToolError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "tool config lock poisoned",
                ))
            })
            .ok()
            .map(|g| g.clone());

        let Some(cfg) = cfg else {
            return Vec::new();
        };

        self.sync_mcp_servers_background(cfg.as_ref()).await;

        let summaries = self.mcp.server_summaries().await;
        let mut tool_counts: BTreeMap<String, usize> = BTreeMap::new();
        for s in summaries {
            tool_counts.insert(s.name, s.tool_count);
        }

        let now = Instant::now();
        let state = self.mcp_state.lock().await;

        let mut out = Vec::new();
        for server in &cfg.mcp.servers {
            let name = server.name.clone();
            let signature = ConnectedMcpServer::from_cfg(server);
            let tool_count = tool_counts.get(&name).copied();

            let status = if !server.enable {
                McpServerConnectionState::Disabled
            } else if state.connected.get(&name) == Some(&signature) {
                McpServerConnectionState::Connected
            } else if state.connecting.contains_key(&name) {
                McpServerConnectionState::Connecting
            } else if let Some(retry) = state.retry.get(&name) {
                let retry_in = if retry.next_attempt_at > now {
                    retry.next_attempt_at - now
                } else {
                    Duration::from_secs(0)
                };
                McpServerConnectionState::Retry {
                    attempt: retry.attempt,
                    retry_in,
                    error: retry.last_error.clone(),
                }
            } else {
                McpServerConnectionState::Disconnected
            };

            out.push(McpServerStatus {
                name,
                command: server.command.clone(),
                args: server.args.clone(),
                tool_count,
                state: status,
            });
        }

        out
    }

    pub async fn execute(&self, perms: &Permissions, call: &ToolCall) -> Result<String, ToolError> {
        let cfg = self
            .config
            .read()
            .map_err(|_| {
                ToolError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "tool config lock poisoned",
                ))
            })?
            .clone();

        if crate::tools::mcp::McpHub::is_mcp_tool_name(call.name.as_str()) {
            let Some((server_name, _tool_name)) =
                crate::tools::mcp::McpHub::parse_exposed_tool_name(call.name.as_str())
            else {
                return Err(ToolError::UnknownTool(call.name.clone()));
            };
            self.ensure_mcp_server(cfg.as_ref(), server_name).await?;

            let arguments = call
                .arguments_json()
                .map_err(|source| ToolError::InvalidArgs {
                    tool: call.name.clone(),
                    source,
                })?;
            let res = self
                .mcp
                .call_exposed_tool(call.name.as_str(), arguments)
                .await;
            if let Err(ToolError::Mcp(err)) = &res {
                self.mark_mcp_server_disconnected(server_name, err).await;
            }
            return res;
        }

        builtin::execute(
            &self.workspace_root,
            perms,
            self.shell_sessions.as_ref(),
            cfg.as_ref(),
            call,
        )
        .await
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

    pub async fn execute_to_messages(
        &self,
        perms: &Permissions,
        call: &ToolCall,
    ) -> Result<Vec<Message>, ToolError> {
        if call.name == builtin::TOOL_VIEW_IMAGE {
            let (tool_content, image_message) =
                builtin::execute_view_image_with_attachment(&self.workspace_root, perms, call)
                    .await?;
            return Ok(vec![
                Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: tool_content,
                },
                image_message,
            ]);
        }

        let content = self.execute(perms, call).await?;
        Ok(vec![Message::Tool {
            tool_call_id: call.id.clone(),
            content,
        }])
    }

    async fn sync_mcp_servers_background(&self, config: &crate::config::Config) {
        let mut desired: BTreeMap<String, ConnectedMcpServer> = BTreeMap::new();
        for server in &config.mcp.servers {
            if !server.enable {
                continue;
            }
            desired.insert(server.name.clone(), ConnectedMcpServer::from_cfg(server));
        }

        let now = Instant::now();
        let (to_shutdown, to_connect) = {
            let mut state = self.mcp_state.lock().await;

            state.retry.retain(|name, _| desired.contains_key(name));
            state
                .connecting
                .retain(|name, _| desired.contains_key(name));

            let mut to_shutdown: Vec<String> = Vec::new();
            for (name, connected) in state.connected.iter() {
                match desired.get(name) {
                    Some(wanted) if wanted == connected => {}
                    _ => to_shutdown.push(name.clone()),
                }
            }

            for name in &to_shutdown {
                state.connected.remove(name);
            }

            let mut to_connect = Vec::new();
            for server in &config.mcp.servers {
                if !server.enable {
                    continue;
                }
                if state.connected.contains_key(&server.name) {
                    continue;
                }
                if state.connecting.contains_key(&server.name) {
                    continue;
                }
                if state
                    .retry
                    .get(&server.name)
                    .is_some_and(|retry| now < retry.next_attempt_at)
                {
                    continue;
                }
                to_connect.push(server.clone());
                state
                    .connecting
                    .insert(server.name.clone(), ConnectedMcpServer::from_cfg(server));
            }

            (to_shutdown, to_connect)
        };

        for name in to_shutdown {
            self.mcp.shutdown_server(&name).await;
        }

        for server in to_connect {
            let mcp = self.mcp.clone();
            let state = self.mcp_state.clone();
            let name = server.name.clone();
            let signature = ConnectedMcpServer::from_cfg(&server);
            tokio::spawn(async move {
                let res = mcp.connect_stdio(server).await;
                let mut shutdown_after_connect = false;
                {
                    let mut state = state.lock().await;
                    let still_desired = state.connecting.remove(&name).is_some();
                    if !still_desired {
                        shutdown_after_connect = res.is_ok();
                    } else {
                        match res {
                            Ok(()) => {
                                state.retry.remove(&name);
                                state.connected.insert(name.clone(), signature);
                            }
                            Err(err) => {
                                let next_attempt = state
                                    .retry
                                    .get(&name)
                                    .map(|retry| retry.attempt.saturating_add(1))
                                    .unwrap_or(0);
                                let backoff = mcp_retry_backoff(next_attempt);
                                state.retry.insert(
                                    name.clone(),
                                    McpRetry {
                                        attempt: next_attempt,
                                        next_attempt_at: Instant::now() + backoff,
                                        last_error: err.to_string(),
                                    },
                                );
                                tracing::warn!(
                                    "mcp connect error: {name}: {err} (retry in {backoff:?})"
                                );
                            }
                        }
                    }
                }

                if shutdown_after_connect {
                    mcp.shutdown_server(&name).await;
                }
            });
        }
    }

    async fn ensure_mcp_server(
        &self,
        config: &crate::config::Config,
        server_name: &str,
    ) -> Result<(), ToolError> {
        self.sync_mcp_servers_background(config).await;

        let Some(server_cfg) = config
            .mcp
            .servers
            .iter()
            .find(|s| s.name == server_name)
            .cloned()
        else {
            return Err(ToolError::UnknownTool(server_name.to_string()));
        };
        if !server_cfg.enable {
            return Err(ToolError::Mcp(format!(
                "mcp server {server_name:?} is disabled"
            )));
        }
        let signature = ConnectedMcpServer::from_cfg(&server_cfg);

        let should_shutdown = {
            let mut state = self.mcp_state.lock().await;
            match state.connected.get(server_name) {
                Some(existing) if existing == &signature => return Ok(()),
                Some(_) => {
                    state.connected.remove(server_name);
                    state.retry.remove(server_name);
                    true
                }
                None => false,
            }
        };
        if should_shutdown {
            self.mcp.shutdown_server(server_name).await;
        }

        let wait_deadline = Instant::now() + Duration::from_secs(35);
        loop {
            let (connected, connecting) = {
                let state = self.mcp_state.lock().await;
                let connected = state
                    .connected
                    .get(server_name)
                    .is_some_and(|existing| existing == &signature);
                let connecting = state.connecting.contains_key(server_name);
                (connected, connecting)
            };

            if connected {
                return Ok(());
            }
            if !connecting {
                break;
            }
            if Instant::now() >= wait_deadline {
                return Err(ToolError::Mcp(format!(
                    "mcp server {server_name:?} is still connecting"
                )));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        {
            let mut state = self.mcp_state.lock().await;
            if state
                .connected
                .get(server_name)
                .is_some_and(|existing| existing == &signature)
            {
                return Ok(());
            }
            if !state.connecting.contains_key(server_name) {
                state
                    .connecting
                    .insert(server_name.to_string(), signature.clone());
            }
            state.retry.remove(server_name);
        }

        let res = self.mcp.connect_stdio(server_cfg).await;
        let mut state = self.mcp_state.lock().await;
        state.connecting.remove(server_name);
        match res {
            Ok(()) => {
                state.retry.remove(server_name);
                state.connected.insert(server_name.to_string(), signature);
                Ok(())
            }
            Err(err) => {
                let next_attempt = state
                    .retry
                    .get(server_name)
                    .map(|retry| retry.attempt.saturating_add(1))
                    .unwrap_or(0);
                let backoff = mcp_retry_backoff(next_attempt);
                state.retry.insert(
                    server_name.to_string(),
                    McpRetry {
                        attempt: next_attempt,
                        next_attempt_at: Instant::now() + backoff,
                        last_error: err.to_string(),
                    },
                );
                Err(err)
            }
        }
    }

    async fn mark_mcp_server_disconnected(&self, name: &str, error: &str) {
        let now = Instant::now();
        {
            let mut state = self.mcp_state.lock().await;
            state.connected.remove(name);
            state.connecting.remove(name);
            let attempt = state
                .retry
                .get(name)
                .map(|retry| retry.attempt.saturating_add(1))
                .unwrap_or(0);
            let backoff = mcp_retry_backoff(attempt);
            state.retry.insert(
                name.to_string(),
                McpRetry {
                    attempt,
                    next_attempt_at: now + backoff,
                    last_error: error.to_string(),
                },
            );
        }
        self.mcp.shutdown_server(name).await;
    }
}

fn mcp_retry_backoff(attempt: u32) -> Duration {
    const BASE: Duration = Duration::from_secs(10);
    const MAX: Duration = Duration::from_secs(300);

    let attempt = attempt.min(8);
    let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    let secs = BASE.as_secs().saturating_mul(factor);
    Duration::from_secs(secs.min(MAX.as_secs()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerConnectionState {
    Disabled,
    Connected,
    Connecting,
    Retry {
        attempt: u32,
        retry_in: Duration,
        error: String,
    },
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerStatus {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub tool_count: Option<usize>,
    pub state: McpServerConnectionState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tools::{ShellPermissions, ToolError};

    fn plan_permissions() -> Permissions {
        Permissions {
            file_read: true,
            file_write: false,
            shell: ShellPermissions::AllowList(vec![vec!["ls".into()]]),
        }
    }

    fn build_permissions() -> Permissions {
        Permissions {
            file_read: true,
            file_write: true,
            shell: ShellPermissions::AllowAll,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_denies_apply_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path(), Config::default());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_APPLY_PATCH.to_string(),
            arguments: serde_json::json!({
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+hi\n*** End Patch\n"
            })
            .to_string(),
        };

        let err = engine
            .execute(&plan_permissions(), &call)
            .await
            .unwrap_err();
        let ToolError::PermissionDenied(_) = err else {
            panic!("unexpected error: {err:?}");
        };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_denies_disallowed_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path(), Config::default());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_SHELL_COMMAND.to_string(),
            arguments: serde_json::json!({
                "argv": ["rm", "-rf", "/"],
                "yield_time_ms": 0
            })
            .to_string(),
        };

        let err = engine
            .execute(&plan_permissions(), &call)
            .await
            .unwrap_err();
        let ToolError::PermissionDenied(_) = err else {
            panic!("unexpected error: {err:?}");
        };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn build_can_apply_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path(), Config::default());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_APPLY_PATCH.to_string(),
            arguments: serde_json::json!({
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+hello\n*** End Patch\n"
            })
            .to_string(),
        };

        let out = engine.execute(&build_permissions(), &call).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.get("ok").and_then(|v| v.as_bool()), Some(true));
        let s = tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap();
        assert_eq!(s, "hello\n");
    }
}
