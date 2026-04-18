use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mcp_protocol::messages::JsonRpcMessage;
use modelcontextprotocol_client::{Client as McpClient, ClientBuilder as McpClientBuilder};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::Instrument;

use crate::config::McpServerConfig;
use crate::protocol::ToolDefinition;
use crate::telemetry;
use crate::tools::ToolError;

const MCP_PREFIX: &str = "mcp__";
const MCP_SEP: &str = "__";
const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Stdio transport that drains server stderr to avoid corrupting the TUI.
///
/// Some MCP servers log to stderr; inheriting it can break our terminal UI.
pub struct QuietStdioTransport {
    child_process: Arc<Mutex<Option<tokio::process::Child>>>,
    tx: tokio::sync::mpsc::Sender<JsonRpcMessage>,
    command: String,
    args: Vec<String>,
    stdin: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
}

impl QuietStdioTransport {
    pub fn new(
        command: &str,
        args: Vec<String>,
    ) -> (Self, tokio::sync::mpsc::Receiver<JsonRpcMessage>) {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        (
            Self {
                child_process: Arc::new(Mutex::new(None)),
                tx,
                command: command.to_string(),
                args,
                stdin: Arc::new(Mutex::new(None)),
            },
            rx,
        )
    }
}

#[async_trait]
impl modelcontextprotocol_client::transport::Transport for QuietStdioTransport {
    async fn start(&self) -> anyhow::Result<()> {
        use std::process::Stdio;
        use tokio::io::AsyncBufReadExt;
        use tokio::io::BufReader;
        use tokio::process::Command;

        let mut child = Command::new(&self.command)
            .args(&self.args)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("mcp stdout piped");
        let stderr = child.stderr.take().expect("mcp stderr piped");
        let stdin = child.stdin.take().expect("mcp stdin piped");

        {
            let mut guard = self.child_process.lock().await;
            *guard = Some(child);
        }

        {
            let mut stdin_guard = self.stdin.lock().await;
            *stdin_guard = Some(stdin);
        }

        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                match serde_json::from_str::<JsonRpcMessage>(&line) {
                    Ok(message) => {
                        if tx.send(message).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::warn!("mcp stdout parse error: {err}");
                    }
                }
                line.clear();
            }
        });

        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                // Drain server stderr so it doesn't corrupt the terminal UI.
                line.clear();
            }
        });

        Ok(())
    }

    async fn send(&self, message: JsonRpcMessage) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        let mut stdin_guard = self.stdin.lock().await;
        let stdin = stdin_guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Child process not started"))?;

        let serialized = serde_json::to_string(&message)?;
        stdin.write_all(serialized.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn close(&self) -> anyhow::Result<()> {
        {
            let mut stdin_guard = self.stdin.lock().await;
            *stdin_guard = None;
        }

        let mut guard = self.child_process.lock().await;
        if let Some(mut child) = guard.take() {
            let wait_future = child.wait();
            match tokio::time::timeout(std::time::Duration::from_secs(1), wait_future).await {
                Ok(Ok(_)) => return Ok(()),
                _ => {
                    child.kill().await?;
                    child.wait().await?;
                }
            }
        }

        Ok(())
    }

    fn box_clone(&self) -> Box<dyn modelcontextprotocol_client::transport::Transport> {
        Box::new(self.clone())
    }
}

impl Clone for QuietStdioTransport {
    fn clone(&self) -> Self {
        Self {
            child_process: self.child_process.clone(),
            tx: self.tx.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            stdin: self.stdin.clone(),
        }
    }
}

#[derive(Clone)]
struct McpClientHandle(Arc<McpClient>);

impl McpClientHandle {
    fn new(client: Arc<McpClient>) -> Self {
        Self(client)
    }
}

impl Deref for McpClientHandle {
    type Target = McpClient;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl fmt::Debug for McpClientHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("McpClientHandle(<opaque>)")
    }
}

struct McpServer {
    client: McpClientHandle,
    tools: Vec<modelcontextprotocol_client::mcp_protocol::types::tool::Tool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSummary {
    pub name: String,
    pub tool_count: usize,
}

#[derive(Clone, Default)]
pub struct McpHub {
    servers: Arc<Mutex<BTreeMap<String, Arc<McpServer>>>>,
}

impl McpHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn connect_stdio(&self, cfg: McpServerConfig) -> Result<(), ToolError> {
        validate_component(&cfg.name)?;
        let name = cfg.name.clone();
        let started = Instant::now();

        let span = tracing::info_span!(
            "kiliax.mcp",
            mcp.server = %name,
            mcp.command = %cfg.command,
            mcp.args = ?cfg.args,
            mcp.duration_ms = tracing::field::Empty,
        );
        telemetry::spans::update_name(&span, format!("kiliax.mcp.{name}"));

        let res: Result<(), ToolError> = async {
            let (transport, mut rx) = QuietStdioTransport::new(&cfg.command, cfg.args.clone());
            let client = Arc::new(
                McpClientBuilder::new("kiliax", "0.1.0")
                    .with_transport(transport)
                    .build()
                    .map_err(|e| ToolError::Mcp(e.to_string()))?,
            );

            // Drive the inbound message loop so requests can complete.
            let client_for_task = client.clone();
            tokio::spawn(
                async move {
                    while let Some(msg) = rx.recv().await {
                        if let Err(err) = client_for_task.handle_message(msg).await {
                            tracing::error!("mcp handle_message error: {}", err);
                        }
                    }
                }
                .instrument(tracing::Span::current()),
            );

            let tools = match tokio::time::timeout(MCP_CONNECT_TIMEOUT, async {
                client
                    .initialize()
                    .await
                    .map_err(|e| ToolError::Mcp(e.to_string()))?;
                client
                    .list_tools()
                    .await
                    .map_err(|e| ToolError::Mcp(e.to_string()))
                    .map(|result| result.tools)
            })
            .await
            {
                Ok(Ok(tools)) => tools,
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    return Err(ToolError::Mcp(format!(
                        "connect timed out after {}s",
                        MCP_CONNECT_TIMEOUT.as_secs()
                    )));
                }
            };

            let server = Arc::new(McpServer {
                client: McpClientHandle::new(client),
                tools,
            });

            let mut map = self.servers.lock().await;
            map.insert(name, server);
            Ok(())
        }
        .instrument(span.clone())
        .await;

        let latency = started.elapsed();
        span.record("mcp.duration_ms", latency.as_millis() as u64);

        if let Err(err) = &res {
            telemetry::metrics::record_mcp_connect_failure(&cfg.name);
            tracing::warn!(
                target: "kiliax_core::telemetry",
                parent: &span,
                event = "mcp.connect_error",
                error = %err,
            );
        }

        res
    }

    pub async fn shutdown_server(&self, name: &str) {
        let server = {
            let mut map = self.servers.lock().await;
            map.remove(name)
        };

        let Some(server) = server else {
            return;
        };

        let res = tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, server.client.shutdown()).await;
        if let Err(err) = res.unwrap_or_else(|_| Err(anyhow::anyhow!("shutdown timed out"))) {
            tracing::warn!("mcp shutdown_server({name:?}) error: {err}");
        }
    }

    pub async fn shutdown_all(&self) {
        let servers = {
            let mut map = self.servers.lock().await;
            std::mem::take(&mut *map)
        };

        for (name, server) in servers {
            let res = tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, server.client.shutdown()).await;
            if let Err(err) = res.unwrap_or_else(|_| Err(anyhow::anyhow!("shutdown timed out"))) {
                tracing::warn!("mcp shutdown_all({name:?}) error: {err}");
            }
        }
    }

    pub async fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let map = self.servers.lock().await;
        let mut out = Vec::new();
        for (server_name, server) in map.iter() {
            for tool in &server.tools {
                if validate_component(&tool.name).is_err() {
                    continue;
                }
                let name = mcp_tool_name(server_name, &tool.name);
                if name.len() > 64 {
                    continue;
                }
                out.push(ToolDefinition {
                    name,
                    description: tool.description.clone(),
                    parameters: Some(tool.input_schema.clone()),
                    strict: None,
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub async fn server_summaries(&self) -> Vec<McpServerSummary> {
        let map = self.servers.lock().await;
        map.iter()
            .map(|(name, server)| McpServerSummary {
                name: name.clone(),
                tool_count: server.tools.len(),
            })
            .collect()
    }

    pub async fn call_exposed_tool(
        &self,
        exposed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, ToolError> {
        let Some((server_name, tool_name)) = parse_mcp_tool_name(exposed_name) else {
            return Err(ToolError::UnknownTool(exposed_name.to_string()));
        };

        let started = Instant::now();
        let span = tracing::info_span!(
            "kiliax.mcp",
            mcp.server = %server_name,
            mcp.tool = %tool_name,
            mcp.duration_ms = tracing::field::Empty,
        );
        telemetry::spans::update_name(&span, format!("kiliax.mcp.{server_name}"));

        let res: Result<String, ToolError> = async {
            let server = self
                .servers
                .lock()
                .await
                .get(server_name)
                .cloned()
                .ok_or_else(|| ToolError::UnknownTool(exposed_name.to_string()))?;

            let result = server
                .client
                .call_tool(tool_name, &arguments)
                .await
                .map_err(|e| ToolError::Mcp(e.to_string()))?;

            Ok(render_tool_result(result))
        }
        .instrument(span.clone())
        .await;

        let latency = started.elapsed();
        span.record("mcp.duration_ms", latency.as_millis() as u64);
        let outcome = if res.is_ok() { "ok" } else { "error" };
        telemetry::metrics::record_mcp_call(server_name, tool_name, outcome, latency);

        if let Err(err) = &res {
            tracing::warn!(
                target: "kiliax_core::telemetry",
                parent: &span,
                event = "mcp.call_error",
                error = %err,
            );
        }

        res
    }

    pub fn is_mcp_tool_name(name: &str) -> bool {
        name.starts_with(MCP_PREFIX) && name.matches(MCP_SEP).count() >= 2
    }

    pub fn parse_exposed_tool_name(name: &str) -> Option<(&str, &str)> {
        parse_mcp_tool_name(name)
    }
}

fn render_tool_result(
    result: modelcontextprotocol_client::mcp_protocol::types::tool::ToolCallResult,
) -> String {
    use modelcontextprotocol_client::mcp_protocol::types::tool::ToolContent;

    let mut parts = Vec::new();
    for c in result.content {
        match c {
            ToolContent::Text { text } => parts.push(text),
            ToolContent::Resource { resource } => parts.push(resource.to_string()),
            ToolContent::Image { mime_type, data } => parts.push(format!(
                "[image mime_type={mime_type} base64_len={}]",
                data.len()
            )),
            ToolContent::Audio { mime_type, data } => parts.push(format!(
                "[audio mime_type={mime_type} base64_len={}]",
                data.len()
            )),
        }
    }
    parts.join("\n")
}

fn mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    format!("{MCP_PREFIX}{server_name}{MCP_SEP}{tool_name}")
}

fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    if !name.starts_with(MCP_PREFIX) {
        return None;
    }
    let rest = &name[MCP_PREFIX.len()..];
    let (server, tool) = rest.split_once(MCP_SEP)?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

fn validate_component(s: &str) -> Result<(), ToolError> {
    if s.is_empty() {
        return Err(ToolError::Mcp("component must not be empty".to_string()));
    }
    if s.contains(MCP_SEP) {
        return Err(ToolError::Mcp(format!(
            "component {s:?} must not contain {MCP_SEP:?}"
        )));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ToolError::Mcp(format!(
            "component {s:?} must be [A-Za-z0-9_-]"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_roundtrip() {
        let name = mcp_tool_name("srv", "t");
        assert_eq!(parse_mcp_tool_name(&name), Some(("srv", "t")));
    }
}
