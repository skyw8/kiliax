use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::protocol::{ToolCall, ToolDefinition};
use crate::tools::ToolError;

pub const TOOL_SPAWN_AGENT: &str = "spawn_agent";
pub const TOOL_SEND_MESSAGE: &str = "send_message";
pub const TOOL_FOLLOWUP_TASK: &str = "followup_task";
pub const TOOL_WAIT_AGENT: &str = "wait_agent";
pub const TOOL_LIST_AGENTS: &str = "list_agents";
pub const TOOL_CLOSE_AGENT: &str = "close_agent";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpawnAgentArgs {
    pub task_name: String,
    pub message: String,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub fork_turns: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MessageAgentArgs {
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WaitAgentArgs {
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListAgentsArgs {
    #[serde(default)]
    pub path_prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CloseAgentArgs {
    pub target: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SpawnAgentResult {
    pub task_name: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MessageAgentResult {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WaitAgentResult {
    pub timed_out: bool,
    pub updates: Vec<MailboxUpdate>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MailboxUpdate {
    pub from: String,
    pub to: String,
    pub session_id: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ListedAgent {
    pub agent_name: String,
    pub session_id: String,
    pub agent_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_task_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ListAgentsResult {
    pub agents: Vec<ListedAgent>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CloseAgentResult {
    pub previous_status: String,
}

#[async_trait]
pub trait MultiAgentBackend: Send + Sync {
    async fn spawn_agent(&self, args: SpawnAgentArgs) -> Result<SpawnAgentResult, ToolError>;
    async fn send_message(&self, args: MessageAgentArgs) -> Result<MessageAgentResult, ToolError>;
    async fn followup_task(&self, args: MessageAgentArgs) -> Result<MessageAgentResult, ToolError>;
    async fn wait_agent(&self, args: WaitAgentArgs) -> Result<WaitAgentResult, ToolError>;
    async fn list_agents(&self, args: ListAgentsArgs) -> Result<ListAgentsResult, ToolError>;
    async fn close_agent(&self, args: CloseAgentArgs) -> Result<CloseAgentResult, ToolError>;
}

pub fn is_multi_agent_tool_name(name: &str) -> bool {
    matches!(
        name,
        TOOL_SPAWN_AGENT
            | TOOL_SEND_MESSAGE
            | TOOL_FOLLOWUP_TASK
            | TOOL_WAIT_AGENT
            | TOOL_LIST_AGENTS
            | TOOL_CLOSE_AGENT
    )
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        spawn_agent_tool_definition(),
        send_message_tool_definition(),
        followup_task_tool_definition(),
        wait_agent_tool_definition(),
        list_agents_tool_definition(),
        close_agent_tool_definition(),
    ]
}

pub async fn execute(
    backend: Option<Arc<dyn MultiAgentBackend>>,
    call: &ToolCall,
) -> Result<String, ToolError> {
    let backend = backend.ok_or_else(|| {
        ToolError::InvalidCommand("multi-agent tools are unavailable in this runtime".to_string())
    })?;

    match call.name.as_str() {
        TOOL_SPAWN_AGENT => {
            let args = parse_args::<SpawnAgentArgs>(call)?;
            to_json(&backend.spawn_agent(args).await?, call.name.as_str())
        }
        TOOL_SEND_MESSAGE => {
            let args = parse_args::<MessageAgentArgs>(call)?;
            to_json(&backend.send_message(args).await?, call.name.as_str())
        }
        TOOL_FOLLOWUP_TASK => {
            let args = parse_args::<MessageAgentArgs>(call)?;
            to_json(&backend.followup_task(args).await?, call.name.as_str())
        }
        TOOL_WAIT_AGENT => {
            let args = parse_args::<WaitAgentArgs>(call)?;
            to_json(&backend.wait_agent(args).await?, call.name.as_str())
        }
        TOOL_LIST_AGENTS => {
            let args = parse_args::<ListAgentsArgs>(call)?;
            to_json(&backend.list_agents(args).await?, call.name.as_str())
        }
        TOOL_CLOSE_AGENT => {
            let args = parse_args::<CloseAgentArgs>(call)?;
            to_json(&backend.close_agent(args).await?, call.name.as_str())
        }
        other => Err(ToolError::UnknownTool(other.to_string())),
    }
}

fn parse_args<T>(call: &ToolCall) -> Result<T, ToolError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(&call.arguments).map_err(|source| ToolError::InvalidArgs {
        tool: call.name.clone(),
        source,
    })
}

fn to_json<T>(value: &T, tool: &str) -> Result<String, ToolError>
where
    T: Serialize,
{
    serde_json::to_string(value).map_err(|source| ToolError::InvalidArgs {
        tool: tool.to_string(),
        source,
    })
}

pub fn spawn_agent_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_SPAWN_AGENT.to_string(),
        description: Some("Spawn a subagent to work on a focused task.".to_string()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "task_name": {
                    "type": "string",
                    "description": "Task name for the new agent. Use lowercase letters, digits, and underscores."
                },
                "message": {
                    "type": "string",
                    "description": "Initial task message for the new agent."
                },
                "agent_type": {
                    "type": "string",
                    "description": "Optional agent profile name. Defaults to general. Choose from the Available Subagents section in the system prompt."
                },
                "model_id": {
                    "type": "string",
                    "description": "Optional model id override. Defaults to the parent model."
                },
                "fork_turns": {
                    "type": "string",
                    "description": "Completed-history fork mode: none, all, or a positive integer string. The current user turn is excluded. Defaults to all."
                }
            },
            "required": ["task_name", "message"],
            "additionalProperties": false
        })),
        strict: Some(false),
    }
}

pub fn send_message_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_SEND_MESSAGE.to_string(),
        description: Some(
            "Send a message to an existing agent without triggering a new turn.".to_string(),
        ),
        parameters: Some(message_tool_parameters(
            "Relative or canonical task path to message.",
            "Message text to queue on the target agent.",
        )),
        strict: Some(false),
    }
}

pub fn followup_task_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_FOLLOWUP_TASK.to_string(),
        description: Some(
            "Send a message to an existing non-root agent and trigger a follow-up turn."
                .to_string(),
        ),
        parameters: Some(message_tool_parameters(
            "Relative or canonical task path to message.",
            "Message text to send to the target agent.",
        )),
        strict: Some(false),
    }
}

pub fn wait_agent_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_WAIT_AGENT.to_string(),
        description: Some(
            "Wait for mailbox updates from live agents, including final-status notifications."
                .to_string(),
        ),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional timeout in milliseconds."
                }
            },
            "additionalProperties": false
        })),
        strict: Some(false),
    }
}

pub fn list_agents_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_LIST_AGENTS.to_string(),
        description: Some(
            "List agents in the current root session tree, optionally filtered by task path."
                .to_string(),
        ),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "path_prefix": {
                    "type": "string",
                    "description": "Optional relative or canonical task-path prefix."
                }
            },
            "additionalProperties": false
        })),
        strict: Some(false),
    }
}

pub fn close_agent_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_CLOSE_AGENT.to_string(),
        description: Some(
            "Close an agent and any open descendants when they are no longer needed.".to_string(),
        ),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Relative or canonical task path to close."
                }
            },
            "required": ["target"],
            "additionalProperties": false
        })),
        strict: Some(false),
    }
}

fn message_tool_parameters(
    target_description: &str,
    message_description: &str,
) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "target": {
                "type": "string",
                "description": target_description
            },
            "message": {
                "type": "string",
                "description": message_description
            }
        },
        "required": ["target", "message"],
        "additionalProperties": false
    })
}
