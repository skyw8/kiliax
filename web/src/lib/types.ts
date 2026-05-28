export type SessionRunState = "idle" | "running" | "tooling" | "retrying";
export type SessionLastOutcome = "none" | "done" | "error";
export type SessionGoalStatus = "active" | "complete";

export type McpConnectionState =
  | "disabled"
  | "connecting"
  | "connected"
  | "error";

export interface McpServerSetting {
  id: string;
  enable: boolean;
}

export interface McpServers {
  servers: McpServerSetting[];
}

export interface SkillsSettings {
  default_enable: boolean;
  overrides: SkillEnableSetting[];
}

export interface CustomToolsSettings {
  default_enable: boolean;
  overrides: CustomToolEnableSetting[];
}

export interface SessionSettings {
  agent: string;
  model_id: string;
  workspace_root: string;
  extra_workspace_roots: string[];
  skills: SkillsSettings;
  custom_tools: CustomToolsSettings;
  mcp: McpServers;
}

export interface SessionSaveDefaultsRequest {
  model: boolean;
  agent?: boolean;
  mcp: boolean;
  skills?: boolean;
  custom_tools?: boolean;
}

export interface SessionStatus {
  run_state: SessionRunState;
  active_run_id: string | null;
  active_run_started_at?: string | null;
  step: number;
  active_tool?: string | null;
  retry_status?: RetryStatus | null;
  queue_len: number;
  last_event_id: number;
}

export interface RetryStatus {
  kind: string;
  attempt: number;
  max_attempts?: number | null;
  next_attempt_at: string;
  delay_ms: number;
  message: string;
  trace_id?: string | null;
}

export interface SessionSummary {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  last_outcome: SessionLastOutcome;
  status: SessionStatus;
  settings: SessionSettings;
  goal?: SessionGoal | null;
}

export interface SessionGoal {
  objective: string;
  status: SessionGoalStatus;
  session_id: string;
  time_used_seconds: number;
  created_at: string;
  updated_at: string;
  tokens_used: number;
}

export interface McpServerStatus {
  id: string;
  enable: boolean;
  state: McpConnectionState;
  last_error?: string | null;
  tools?: string[] | null;
}

export interface StreamToolCallSnapshot {
  id: string;
  name: string;
  arguments: string;
}

export interface StreamSnapshot {
  run_id: string;
  last_event_id: number;
  thinking: string;
  assistant: string;
  assistant_started: boolean;
  tool_calls: StreamToolCallSnapshot[];
  thinking_started_at?: string | null;
  assistant_started_at?: string | null;
  tool_started_at?: Record<string, string>;
}

export interface Session extends SessionSummary {
  mcp_status: McpServerStatus[];
  stream?: StreamSnapshot | null;
}

export interface SessionListResponse {
  items: SessionSummary[];
  next_cursor?: string | null;
}

export interface ToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface TokenUsage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cached_tokens?: number | null;
}

export type Message = UserMessage | AssistantMessage | ToolMessage;

export interface UserMessage {
  role: "user";
  id: string;
  created_at: string;
  content: string;
  attachments?: MessageAttachment[];
  delivery_state?: "queued" | "sent";
}

export interface MessageAttachment {
  filename: string;
  media_type: string;
  data?: string | null;
}

export interface AssistantMessage {
  role: "assistant";
  id: string;
  created_at: string;
  content: string;
  reasoning_content?: string | null;
  tool_calls?: ToolCall[];
  usage?: TokenUsage | null;
}

export interface ToolMessage {
  role: "tool";
  id: string;
  created_at: string;
  tool_call_id: string;
  content: string;
}

export interface MessageListResponse {
  items: Message[];
  next_before?: string | null;
}

export interface Capabilities {
  agents: string[];
  agent_errors?: AgentLoadError[];
  models: string[];
  builtin_tools: BuiltinToolSummary[];
  mcp_servers: McpServerStatus[];
}

export interface AgentLoadError {
  id: string;
  path: string;
  error: string;
}

export interface BuiltinToolSummary {
  id: string;
  name: string;
  description?: string | null;
}

export interface ConfigResponse {
  path: string;
  yaml: string;
  config: unknown;
}

export interface ConfigUpdateRequest {
  yaml: string;
}

export interface ConfigProviderSummary {
  id: string;
  api: string;
  base_url: string;
  api_key_set: boolean;
  models: ConfigModelSummary[];
}

export interface ConfigModelSummary {
  id: string;
  auto_compact_token_limit?: number | null;
  temperature?: number | null;
  reasoning_effort?: string | null;
}

export interface ConfigProvidersResponse {
  default_model?: string;
  providers: ConfigProviderSummary[];
}

export interface ConfigProviderUpsert {
  id: string;
  api?: string;
  base_url?: string;
  api_key?: string | null;
  models?: ConfigModelUpsert[];
}

export type ConfigModelUpsert =
  | string
  | {
      id: string;
      auto_compact_token_limit?: number | null;
      temperature?: number | null;
      reasoning_effort?: string | null;
    };

export interface ConfigProvidersPatchRequest {
  default_model?: string | null;
  upsert?: ConfigProviderUpsert[];
  delete?: string[];
}

export interface ConfigRuntimeResponse {
  runtime_max_steps: number | null;
  agents_plan_max_steps: number | null;
  agents_general_max_steps: number | null;
}

export interface ConfigRuntimePatchRequest {
  runtime_max_steps?: number | null;
  agents_plan_max_steps?: number | null;
  agents_general_max_steps?: number | null;
}

export interface SkillEnableSetting {
  id: string;
  enable: boolean;
}

export interface CustomToolEnableSetting {
  id: string;
  enable: boolean;
}

export interface ConfigSkillsResponse {
  default_enable: boolean;
  skills: SkillEnableSetting[];
}

export interface ConfigCustomToolsResponse {
  default_enable: boolean;
  custom_tools: CustomToolEnableSetting[];
}

export interface SkillSummary {
  id: string;
  name: string;
  description?: string | null;
}

export interface SkillListResponse {
  items: SkillSummary[];
  errors: SkillLoadError[];
}

export interface SkillLoadError {
  id: string;
  path: string;
  error: string;
}

export interface CustomToolSummary {
  id: string;
  name: string;
  description?: string | null;
  parallel: boolean;
}

export interface CustomToolListResponse {
  items: CustomToolSummary[];
  errors: CustomToolLoadError[];
}

export interface CustomToolLoadError {
  id: string;
  path: string;
  error: string;
}

export interface FsListResponse {
  path: string;
  parent?: string | null;
  entries: FsEntry[];
}

export interface FsEntry {
  name: string;
  path: string;
  is_dir: boolean;
}

export type OpenWorkspaceTarget = "vscode" | "file_manager" | "terminal";

export type RunInput =
  | { type: "text"; text: string; attachments?: RunAttachment[] }
  | { type: "from_user_message"; user_message_id: number }
  | { type: "edit_user_message"; user_message_id: number; content: string }
  | { type: "regenerate_after_user_message"; user_message_id: number };

export interface RunAttachment {
  filename: string;
  media_type: string;
  data: string;
}

export interface RunOverrides {
  agent?: string;
  model_id?: string;
  mcp?: { servers?: McpServerSetting[] };
}

export interface RunCreateRequest {
  input: RunInput;
  overrides?: RunOverrides;
  auto_resume?: boolean;
  client_message_id?: string;
}

export interface ApiErrorShape {
  error?: { code?: string; message?: string; details?: unknown };
  trace_id?: string;
}
