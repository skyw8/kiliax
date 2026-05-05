use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatus {
    pub run_state: SessionRunState,
    pub active_run_id: Option<String>,
    pub step: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tool: Option<String>,
    pub queue_len: usize,
    pub last_event_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRunState {
    Idle,
    Running,
    Tooling,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}

impl From<kiliax_core::protocol::TokenUsage> for TokenUsage {
    fn from(value: kiliax_core::protocol::TokenUsage) -> Self {
        Self {
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cached_tokens: value.cached_tokens,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillEnableSetting {
    pub id: String,
    pub enable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsSettings {
    pub default_enable: bool,
    #[serde(default)]
    pub overrides: Vec<SkillEnableSetting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsSettingsPatch {
    #[serde(default)]
    pub default_enable: Option<bool>,

    #[serde(default)]
    pub overrides: Option<Vec<SkillEnableSetting>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSetting {
    pub id: String,
    pub enable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServers {
    pub servers: Vec<McpServerSetting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServersPatch {
    #[serde(default)]
    pub servers: Option<Vec<McpServerSetting>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSettings {
    pub agent: String,
    pub model_id: String,
    pub skills: SkillsSettings,
    pub mcp: McpServers,
    pub workspace_root: PathBuf,
    #[serde(default)]
    pub extra_workspace_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub settings: Option<SessionCreateSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateSettings {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub skills: Option<SkillsSettingsPatch>,
    #[serde(default)]
    pub mcp: Option<McpServersPatch>,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub extra_workspace_roots: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSettingsPatch {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub skills: Option<SkillsSettingsPatch>,
    #[serde(default)]
    pub mcp: Option<McpServersPatch>,
    #[serde(default)]
    pub extra_workspace_roots: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSaveDefaultsRequest {
    pub model: bool,
    #[serde(default)]
    pub agent: bool,
    pub mcp: bool,
    #[serde(default)]
    pub skills: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionState {
    Disabled,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerStatus {
    pub id: String,
    pub enable: bool,
    pub state: McpConnectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLastOutcome {
    None,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_outcome: SessionLastOutcome,
    pub status: SessionStatus,
    pub settings: SessionSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub summary: SessionSummary,
    pub mcp_status: Vec<McpServerStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionList {
    pub items: Vec<SessionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageList {
    pub items: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventList {
    pub items: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        id: String,
        created_at: String,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<MessageAttachment>,
    },
    Assistant {
        id: String,
        created_at: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },
    Tool {
        id: String,
        created_at: String,
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageAttachment {
    pub filename: String,
    pub media_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunInput {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<RunAttachment>,
    },
    FromUserMessage {
        user_message_id: u64,
    },
    EditUserMessage {
        user_message_id: u64,
        content: String,
    },
    RegenerateAfterUserMessage {
        user_message_id: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAttachment {
    pub filename: String,
    pub media_type: String,
    /// Raw base64 bytes without a data URL prefix.
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOverrides {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub mcp: Option<McpServersPatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCreateRequest {
    pub input: RunInput,
    #[serde(default)]
    pub overrides: Option<RunOverrides>,
    #[serde(default)]
    pub auto_resume: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkSessionRequest {
    #[serde(default)]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkSessionResponse {
    pub session: SessionSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenWorkspaceTarget {
    Vscode,
    FileManager,
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenWorkspaceRequest {
    pub target: OpenWorkspaceTarget,
}
