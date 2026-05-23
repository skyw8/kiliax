use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::config::CustomToolsConfig;
use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::ToolError;

const CUSTOM_PREFIX: &str = "custom__";
const CUSTOM_TOOL_DIR: &str = "tools";
const MANIFEST: &str = "TOOL.yaml";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_STDOUT_LINE: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomToolDiscoveryError {
    pub id: String,
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomToolDiscovery {
    pub items: Vec<CustomTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<CustomToolDiscoveryError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomTool {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub description: String,
    pub path: PathBuf,
    pub command: Vec<String>,
    pub input_schema: serde_json::Value,
    pub timeout_ms: u64,
    pub parallel: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomToolManifest {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    description: String,
    command: Vec<String>,
    input_schema: serde_json::Value,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    parallel: bool,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    id: serde_json::Value,
    #[serde(default)]
    result: Option<RpcResult>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcResult {
    content: String,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    message: String,
}

pub fn exposed_name(name: &str) -> String {
    format!("{CUSTOM_PREFIX}{name}")
}

pub fn is_custom_tool_name(name: &str) -> bool {
    name.starts_with(CUSTOM_PREFIX)
}

pub fn parse_exposed_tool_name(name: &str) -> Option<&str> {
    name.strip_prefix(CUSTOM_PREFIX)
        .filter(|n| is_valid_tool_name(n))
}

pub fn discover_custom_tools(config: &CustomToolsConfig) -> CustomToolDiscovery {
    let mut out: BTreeMap<String, CustomTool> = BTreeMap::new();
    let mut errors = Vec::new();

    for root in custom_tool_roots() {
        if !root.is_dir() {
            continue;
        }
        let rd = match std::fs::read_dir(&root) {
            Ok(v) => v,
            Err(err) => {
                errors.push(CustomToolDiscoveryError {
                    id: "<root>".to_string(),
                    path: root,
                    error: err.to_string(),
                });
                continue;
            }
        };

        for entry in rd {
            let entry = match entry {
                Ok(v) => v,
                Err(err) => {
                    errors.push(CustomToolDiscoveryError {
                        id: "<entry>".to_string(),
                        path: root.clone(),
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            if out.contains_key(&id) {
                continue;
            }
            let manifest_path = dir.join(MANIFEST);
            if !manifest_path.is_file() {
                continue;
            }
            match load_custom_tool(&id, &manifest_path) {
                Ok(tool) => {
                    if enabled(config, &tool.name) {
                        out.insert(tool.name.clone(), tool);
                    }
                }
                Err(err) => errors.push(CustomToolDiscoveryError {
                    id,
                    path: manifest_path,
                    error: err,
                }),
            }
        }
    }

    CustomToolDiscovery {
        items: out.into_values().collect(),
        errors,
    }
}

pub fn list_custom_tools() -> CustomToolDiscovery {
    let cfg = CustomToolsConfig {
        default_enable: true,
        overrides: BTreeMap::new(),
    };
    discover_custom_tools(&cfg)
}

pub fn tool_definition(tool: &CustomTool) -> ToolDefinition {
    ToolDefinition {
        name: exposed_name(&tool.name),
        description: Some(tool.description.clone()),
        parameters: Some(tool.input_schema.clone()),
        strict: Some(true),
    }
}

pub fn tool_definitions(config: &CustomToolsConfig) -> Vec<ToolDefinition> {
    discover_custom_tools(config)
        .items
        .iter()
        .map(tool_definition)
        .collect()
}

pub fn tool_parallelism(
    config: &CustomToolsConfig,
    exposed: &str,
) -> crate::tools::ToolParallelism {
    let Some(name) = parse_exposed_tool_name(exposed) else {
        return crate::tools::ToolParallelism::Exclusive;
    };
    let discovery = discover_custom_tools(config);
    let Some(tool) = discovery.items.iter().find(|t| t.name == name) else {
        return crate::tools::ToolParallelism::Exclusive;
    };
    if tool.parallel {
        crate::tools::ToolParallelism::Parallel
    } else {
        crate::tools::ToolParallelism::Exclusive
    }
}

#[derive(Clone, Default)]
pub struct CustomToolRuntime {
    processes: Arc<Mutex<HashMap<String, Arc<Mutex<CustomToolProcess>>>>>,
}

impl CustomToolRuntime {
    pub async fn execute(
        &self,
        workspace_root: &Path,
        config: &CustomToolsConfig,
        call: &ToolCall,
    ) -> Result<String, ToolError> {
        let name = parse_exposed_tool_name(&call.name)
            .ok_or_else(|| ToolError::UnknownTool(call.name.clone()))?;
        let discovery = discover_custom_tools(config);
        let tool = discovery
            .items
            .into_iter()
            .find(|t| t.name == name)
            .ok_or_else(|| ToolError::UnknownTool(call.name.clone()))?;

        let key = tool.name.clone();
        let proc = {
            let mut map = self.processes.lock().await;
            map.entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(CustomToolProcess::new(tool.clone()))))
                .clone()
        };

        let mut proc = proc.lock().await;
        proc.call(workspace_root, call).await
    }
}

struct CustomToolProcess {
    tool: CustomTool,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
}

impl CustomToolProcess {
    fn new(tool: CustomTool) -> Self {
        Self {
            tool,
            child: None,
            stdin: None,
            stdout: None,
        }
    }

    async fn call(&mut self, workspace_root: &Path, call: &ToolCall) -> Result<String, ToolError> {
        let arguments = call
            .arguments_json()
            .map_err(|source| ToolError::InvalidArgs {
                tool: call.name.clone(),
                source,
            })?;
        self.ensure_started().await?;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": call.id,
            "method": "call",
            "params": {
                "tool": self.tool.name,
                "arguments": arguments,
                "workspace_root": workspace_root,
            }
        });
        let line = serde_json::to_string(&request).map_err(|err| {
            ToolError::InvalidCommand(format!("failed to encode custom tool request: {err}"))
        })?;

        let timeout_dur = Duration::from_millis(self.tool.timeout_ms);
        let res = timeout(timeout_dur, async {
            let stdin = self.stdin.as_mut().ok_or_else(|| {
                ToolError::InvalidCommand("custom tool stdin is not available".to_string())
            })?;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;

            let stdout = self.stdout.as_mut().ok_or_else(|| {
                ToolError::InvalidCommand("custom tool stdout is not available".to_string())
            })?;
            let mut buf = Vec::new();
            let n = stdout.read_until(b'\n', &mut buf).await?;
            if n == 0 {
                return Err(ToolError::InvalidCommand(
                    "custom tool exited without a response".to_string(),
                ));
            }
            if buf.len() > MAX_STDOUT_LINE {
                return Err(ToolError::InvalidCommand(
                    "custom tool response exceeded size limit".to_string(),
                ));
            }
            parse_response(&call.id, &buf)
        })
        .await;

        match res {
            Ok(Ok(content)) => Ok(content),
            Ok(Err(err)) => {
                self.stop().await;
                Err(err)
            }
            Err(_) => {
                self.stop().await;
                Err(ToolError::InvalidCommand(format!(
                    "custom tool {} timed out after {} ms",
                    self.tool.name, self.tool.timeout_ms
                )))
            }
        }
    }

    async fn ensure_started(&mut self) -> Result<(), ToolError> {
        if self.child.is_some() && self.stdin.is_some() && self.stdout.is_some() {
            return Ok(());
        }
        self.stop().await;

        let (cmd, args) =
            self.tool.command.split_first().ok_or_else(|| {
                ToolError::InvalidCommand("custom tool command is empty".to_string())
            })?;
        let mut command = Command::new(cmd);
        command
            .args(args)
            .current_dir(self.tool.path.parent().unwrap_or_else(|| Path::new(".")))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = command.spawn().map_err(|err| {
            ToolError::InvalidCommand(format!(
                "failed to start custom tool {}: {err}",
                self.tool.name
            ))
        })?;
        self.stdin = child.stdin.take();
        self.stdout = child.stdout.take().map(BufReader::new);
        self.child = Some(child);
        Ok(())
    }

    async fn stop(&mut self) {
        self.stdin = None;
        self.stdout = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

fn parse_response(call_id: &str, buf: &[u8]) -> Result<String, ToolError> {
    let response: RpcResponse = serde_json::from_slice(buf).map_err(|err| {
        ToolError::InvalidCommand(format!("failed to parse custom tool response: {err}"))
    })?;
    if response.id != serde_json::Value::String(call_id.to_string()) {
        return Err(ToolError::InvalidCommand(
            "custom tool response id did not match request".to_string(),
        ));
    }
    if let Some(err) = response.error {
        return Err(ToolError::InvalidCommand(err.message));
    }
    let result = response.result.ok_or_else(|| {
        ToolError::InvalidCommand("custom tool response missing result".to_string())
    })?;
    Ok(result.content)
}

fn load_custom_tool(id: &str, manifest_path: &Path) -> Result<CustomTool, String> {
    let raw = std::fs::read_to_string(manifest_path).map_err(|err| err.to_string())?;
    let manifest: CustomToolManifest = serde_yaml::from_str(&raw).map_err(|err| err.to_string())?;
    let name = manifest.name.trim();
    if !is_valid_tool_name(name) {
        return Err("custom tool name must contain only ASCII letters, digits, '_' or '-'".into());
    }
    if name != id {
        return Err("custom tool name must match its directory name".into());
    }
    if manifest.description.trim().is_empty() {
        return Err("custom tool description must not be empty".into());
    }
    if manifest.command.is_empty() || manifest.command.iter().any(|s| s.trim().is_empty()) {
        return Err("custom tool command must not be empty".into());
    }
    if !manifest.input_schema.is_object() {
        return Err("custom tool input_schema must be a JSON object".into());
    }

    Ok(CustomTool {
        id: id.to_string(),
        name: name.to_string(),
        display_name: manifest
            .display_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        description: manifest.description.trim().to_string(),
        path: manifest_path.to_path_buf(),
        command: manifest.command,
        input_schema: manifest.input_schema,
        timeout_ms: manifest.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).max(1),
        parallel: manifest.parallel,
    })
}

fn enabled(config: &CustomToolsConfig, name: &str) -> bool {
    config
        .overrides
        .get(name)
        .copied()
        .unwrap_or(config.default_enable)
}

fn custom_tool_roots() -> Vec<PathBuf> {
    dirs::home_dir()
        .map(|home| vec![custom_tool_root_for_home(&home)])
        .unwrap_or_default()
}

fn custom_tool_root_for_home(home: &Path) -> PathBuf {
    home.join(".kiliax").join(CUSTOM_TOOL_DIR)
}

fn is_valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_tool_root_uses_tools_directory() {
        assert_eq!(
            custom_tool_root_for_home(Path::new("/home/alice")),
            PathBuf::from("/home/alice/.kiliax/tools")
        );
    }
}
