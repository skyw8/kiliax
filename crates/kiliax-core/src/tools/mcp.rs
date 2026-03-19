use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use modelcontextprotocol_client::{
    transport::StdioTransport,
    Client as McpClient,
    ClientBuilder as McpClientBuilder,
};
use tokio::sync::Mutex;

use crate::llm::ToolDefinition;
use crate::tools::ToolError;

const MCP_PREFIX: &str = "mcp__";
const MCP_SEP: &str = "__";

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
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

        let (transport, mut rx) = StdioTransport::new(&cfg.command, cfg.args.clone());
        let client = Arc::new(
            McpClientBuilder::new("kiliax", "0.1.0")
                .with_transport(transport)
                .build()
                .map_err(|e| ToolError::Mcp(e.to_string()))?,
        );

        // Drive the inbound message loop so requests can complete.
        let client_for_task = client.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(err) = client_for_task.handle_message(msg).await {
                    tracing::error!("mcp handle_message error: {}", err);
                }
            }
        });

        client
            .initialize()
            .await
            .map_err(|e| ToolError::Mcp(e.to_string()))?;

        let result = client
            .list_tools()
            .await
            .map_err(|e| ToolError::Mcp(e.to_string()))?;
        let tools = result.tools;

        let server = Arc::new(McpServer {
            client: McpClientHandle::new(client),
            tools,
        });

        let mut map = self.servers.lock().await;
        map.insert(cfg.name, server);
        Ok(())
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
        out
    }

    pub async fn call_exposed_tool(
        &self,
        exposed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, ToolError> {
        let Some((server_name, tool_name)) = parse_mcp_tool_name(exposed_name) else {
            return Err(ToolError::UnknownTool(exposed_name.to_string()));
        };

        let map = self.servers.lock().await;
        let server = map
            .get(server_name)
            .ok_or_else(|| ToolError::UnknownTool(exposed_name.to_string()))?;

        let result = server
            .client
            .call_tool(tool_name, &arguments)
            .await
            .map_err(|e| ToolError::Mcp(e.to_string()))?;

        Ok(render_tool_result(result))
    }

    pub fn is_mcp_tool_name(name: &str) -> bool {
        name.starts_with(MCP_PREFIX) && name.matches(MCP_SEP).count() >= 2
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
