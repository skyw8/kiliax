use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use kiliax_core::config::Config as KiliaxConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}

impl From<kiliax_core::llm::TokenUsage> for TokenUsage {
    fn from(value: kiliax_core::llm::TokenUsage) -> Self {
        Self {
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cached_tokens: value.cached_tokens,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SessionListResponse {
    pub items: Vec<SessionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessageListResponse {
    pub items: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EventListResponse {
    pub items: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_outcome: SessionLastOutcome,
    pub status: SessionStatus,
    pub settings: SessionSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionLastOutcome {
    None,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Session {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub mcp_status: Vec<McpServerStatus>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SessionStatus {
    pub run_state: SessionRunState,
    pub active_run_id: Option<String>,
    pub step: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tool: Option<String>,
    pub queue_len: usize,
    pub last_event_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionRunState {
    Idle,
    Running,
    Tooling,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionCreateRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub settings: Option<SessionCreateSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionCreateSettings {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub mcp: Option<McpServersPatch>,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub extra_workspace_roots: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionSettingsPatch {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub mcp: Option<McpServersPatch>,
    #[serde(default)]
    pub extra_workspace_roots: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionSaveDefaultsRequest {
    pub model: bool,
    pub mcp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpServersPatch {
    #[serde(default)]
    pub servers: Option<Vec<McpServerSetting>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct McpServerSetting {
    pub id: String,
    pub enable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionSettings {
    pub agent: String,
    pub model_id: String,
    pub mcp: McpServers,
    pub workspace_root: String,
    #[serde(default)]
    pub extra_workspace_roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpServers {
    pub servers: Vec<McpServerSetting>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct McpServerStatus {
    pub id: String,
    pub enable: bool,
    pub state: McpConnectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionState {
    Disabled,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunInput {
    Text {
        text: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RunOverrides {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub mcp: Option<McpServersPatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Done,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Capabilities {
    pub agents: Vec<String>,
    pub models: Vec<String>,
    pub mcp_servers: Vec<McpServerStatus>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminInfo {
    pub version: String,
    pub workspace_root: String,
    pub config_path: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConfigResponse {
    pub path: String,
    pub yaml: String,
    #[schema(value_type = Object)]
    pub config: KiliaxConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfigUpdateRequest {
    pub yaml: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfigMcpPatchRequest {
    pub servers: Vec<McpServerSetting>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConfigProvidersResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub providers: Vec<ConfigProviderSummary>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConfigProviderSummary {
    pub id: String,
    pub base_url: String,
    pub api_key_set: bool,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigProvidersPatchRequest {
    #[serde(default)]
    pub default_model: Option<Option<String>>,

    #[serde(default)]
    pub upsert: Vec<ConfigProviderUpsert>,

    #[serde(default)]
    pub delete: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigProviderUpsert {
    pub id: String,

    #[serde(default)]
    pub base_url: Option<String>,

    #[serde(default)]
    pub api_key: Option<Option<String>>,

    #[serde(default)]
    pub models: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConfigRuntimeResponse {
    pub runtime_max_steps: Option<usize>,
    pub agents_plan_max_steps: Option<usize>,
    pub agents_general_max_steps: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigRuntimePatchRequest {
    #[serde(default)]
    pub runtime_max_steps: Option<Option<usize>>,

    #[serde(default)]
    pub agents_plan_max_steps: Option<Option<usize>>,

    #[serde(default)]
    pub agents_general_max_steps: Option<Option<usize>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConfigSkillsResponse {
    pub default_enable: bool,
    pub skills: Vec<SkillEnableSetting>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct SkillEnableSetting {
    pub id: String,
    pub enable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigSkillsPatchRequest {
    #[serde(default)]
    pub default_enable: Option<bool>,

    #[serde(default)]
    pub skills: Vec<SkillEnableSetting>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SkillListResponse {
    pub items: Vec<SkillSummary>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, Serialize, ToSchema)]
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

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ForkSessionRequest {
    #[serde(default)]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ForkSessionResponse {
    pub session: Session,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FsListResponse {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub entries: Vec<FsEntry>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenWorkspaceTarget {
    Vscode,
    FileManager,
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenWorkspaceRequest {
    pub target: OpenWorkspaceTarget,
}
