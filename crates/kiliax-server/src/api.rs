use serde::{Deserialize, Serialize};

use kiliax_core::config::Config as KiliaxConfig;

#[derive(Debug, Clone, Serialize)]
pub struct SessionListResponse {
    pub items: Vec<SessionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageListResponse {
    pub items: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventListResponse {
    pub items: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_outcome: SessionLastOutcome,
    pub status: SessionStatus,
    pub settings: SessionSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLastOutcome {
    None,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub mcp_status: Vec<McpServerStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStatus {
    pub session_state: SessionState,
    pub run_state: SessionRunState,
    pub active_run_id: Option<String>,
    pub step: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tool: Option<String>,
    pub queue_len: usize,
    pub last_event_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Live,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRunState {
    Idle,
    Running,
    Tooling,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub settings: Option<SessionSettingsPatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSettingsPatch {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub mcp: Option<McpServersPatch>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServersPatch {
    #[serde(default)]
    pub servers: Option<Vec<McpServerSetting>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSetting {
    pub id: String,
    pub enable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSettings {
    pub agent: String,
    pub model_id: String,
    pub mcp: McpServers,
    pub workspace_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServers {
    pub servers: Vec<McpServerSetting>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpServerStatus {
    pub id: String,
    pub enable: bool,
    pub state: McpConnectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionState {
    Disabled,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCreateRequest {
    pub input: RunInput,
    #[serde(default)]
    pub overrides: Option<RunOverrides>,
    #[serde(default = "default_true")]
    pub auto_resume: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunInput {
    Text { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunOverrides {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub mcp: Option<McpServersPatch>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub session_id: String,
    pub state: RunState,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RunError>,
    pub input: RunInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<RunOverrides>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Done,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub agents: Vec<String>,
    pub models: Vec<String>,
    pub mcp_servers: Vec<McpServerStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigResponse {
    pub path: String,
    pub yaml: String,
    pub config: KiliaxConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigUpdateRequest {
    pub yaml: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillListResponse {
    pub items: Vec<SkillSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_id: u64,
    pub ts: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        id: String,
        created_at: String,
        content: String,
    },
    Assistant {
        id: String,
        created_at: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        id: String,
        created_at: String,
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
