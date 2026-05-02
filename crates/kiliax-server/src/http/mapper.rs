use crate::api;
use crate::state::domain;

impl From<domain::SessionRunState> for api::SessionRunState {
    fn from(value: domain::SessionRunState) -> Self {
        match value {
            domain::SessionRunState::Idle => Self::Idle,
            domain::SessionRunState::Running => Self::Running,
            domain::SessionRunState::Tooling => Self::Tooling,
        }
    }
}

impl From<domain::SessionStatus> for api::SessionStatus {
    fn from(value: domain::SessionStatus) -> Self {
        Self {
            run_state: value.run_state.into(),
            active_run_id: value.active_run_id,
            step: value.step,
            active_tool: value.active_tool,
            queue_len: value.queue_len,
            last_event_id: value.last_event_id,
        }
    }
}

impl From<domain::SkillEnableSetting> for api::SkillEnableSetting {
    fn from(value: domain::SkillEnableSetting) -> Self {
        Self {
            id: value.id,
            enable: value.enable,
        }
    }
}

impl From<domain::SkillsSettings> for api::SkillsSettings {
    fn from(value: domain::SkillsSettings) -> Self {
        Self {
            default_enable: value.default_enable,
            overrides: value.overrides.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<domain::SkillsSettingsPatch> for api::SkillsSettingsPatch {
    fn from(value: domain::SkillsSettingsPatch) -> Self {
        Self {
            default_enable: value.default_enable,
            overrides: value
                .overrides
                .map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<api::SkillEnableSetting> for domain::SkillEnableSetting {
    fn from(value: api::SkillEnableSetting) -> Self {
        Self {
            id: value.id,
            enable: value.enable,
        }
    }
}

impl From<api::SkillsSettingsPatch> for domain::SkillsSettingsPatch {
    fn from(value: api::SkillsSettingsPatch) -> Self {
        Self {
            default_enable: value.default_enable,
            overrides: value
                .overrides
                .map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<domain::McpServerSetting> for api::McpServerSetting {
    fn from(value: domain::McpServerSetting) -> Self {
        Self {
            id: value.id,
            enable: value.enable,
        }
    }
}

impl From<domain::McpServers> for api::McpServers {
    fn from(value: domain::McpServers) -> Self {
        Self {
            servers: value.servers.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<domain::McpServersPatch> for api::McpServersPatch {
    fn from(value: domain::McpServersPatch) -> Self {
        Self {
            servers: value
                .servers
                .map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<api::McpServerSetting> for domain::McpServerSetting {
    fn from(value: api::McpServerSetting) -> Self {
        Self {
            id: value.id,
            enable: value.enable,
        }
    }
}

impl From<api::McpServersPatch> for domain::McpServersPatch {
    fn from(value: api::McpServersPatch) -> Self {
        Self {
            servers: value
                .servers
                .map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<domain::SessionSettings> for api::SessionSettings {
    fn from(value: domain::SessionSettings) -> Self {
        Self {
            agent: value.agent,
            model_id: value.model_id,
            skills: value.skills.into(),
            mcp: value.mcp.into(),
            workspace_root: value.workspace_root.display().to_string(),
            extra_workspace_roots: value
                .extra_workspace_roots
                .into_iter()
                .map(|p| p.display().to_string())
                .collect(),
        }
    }
}

impl From<domain::SessionLastOutcome> for api::SessionLastOutcome {
    fn from(value: domain::SessionLastOutcome) -> Self {
        match value {
            domain::SessionLastOutcome::None => Self::None,
            domain::SessionLastOutcome::Done => Self::Done,
            domain::SessionLastOutcome::Error => Self::Error,
        }
    }
}

impl From<domain::SessionSummary> for api::SessionSummary {
    fn from(value: domain::SessionSummary) -> Self {
        Self {
            id: value.id,
            title: value.title,
            created_at: value.created_at,
            updated_at: value.updated_at,
            last_outcome: value.last_outcome.into(),
            status: value.status.into(),
            settings: value.settings.into(),
        }
    }
}

impl From<domain::McpConnectionState> for api::McpConnectionState {
    fn from(value: domain::McpConnectionState) -> Self {
        match value {
            domain::McpConnectionState::Disabled => Self::Disabled,
            domain::McpConnectionState::Connecting => Self::Connecting,
            domain::McpConnectionState::Connected => Self::Connected,
            domain::McpConnectionState::Error => Self::Error,
        }
    }
}

impl From<domain::McpServerStatus> for api::McpServerStatus {
    fn from(value: domain::McpServerStatus) -> Self {
        Self {
            id: value.id,
            enable: value.enable,
            state: value.state.into(),
            last_error: value.last_error,
            tools: value.tools,
        }
    }
}

impl From<domain::SessionSnapshot> for api::Session {
    fn from(value: domain::SessionSnapshot) -> Self {
        Self {
            summary: value.summary.into(),
            mcp_status: value.mcp_status.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<domain::SessionList> for api::SessionListResponse {
    fn from(value: domain::SessionList) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

impl From<domain::TokenUsage> for api::TokenUsage {
    fn from(value: domain::TokenUsage) -> Self {
        Self {
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
            cached_tokens: value.cached_tokens,
        }
    }
}

impl From<domain::ToolCall> for api::ToolCall {
    fn from(value: domain::ToolCall) -> Self {
        Self {
            id: value.id,
            name: value.name,
            arguments: value.arguments,
        }
    }
}

impl From<domain::Message> for api::Message {
    fn from(value: domain::Message) -> Self {
        match value {
            domain::Message::User {
                id,
                created_at,
                content,
            } => Self::User {
                id,
                created_at,
                content,
            },
            domain::Message::Assistant {
                id,
                created_at,
                content,
                reasoning_content,
                tool_calls,
                usage,
            } => Self::Assistant {
                id,
                created_at,
                content,
                reasoning_content,
                tool_calls: tool_calls.into_iter().map(Into::into).collect(),
                usage: usage.map(Into::into),
            },
            domain::Message::Tool {
                id,
                created_at,
                tool_call_id,
                content,
            } => Self::Tool {
                id,
                created_at,
                tool_call_id,
                content,
            },
        }
    }
}

impl From<domain::MessageList> for api::MessageListResponse {
    fn from(value: domain::MessageList) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_before: value.next_before,
        }
    }
}

impl From<domain::Event> for api::Event {
    fn from(value: domain::Event) -> Self {
        Self {
            event_id: value.event_id,
            ts: value.ts,
            session_id: value.session_id,
            run_id: value.run_id,
            event_type: value.event_type,
            data: value.data,
        }
    }
}

impl From<domain::EventList> for api::EventListResponse {
    fn from(value: domain::EventList) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_after: value.next_after,
        }
    }
}

impl From<domain::RunInput> for api::RunInput {
    fn from(value: domain::RunInput) -> Self {
        match value {
            domain::RunInput::Text { text } => Self::Text { text },
            domain::RunInput::FromUserMessage { user_message_id } => {
                Self::FromUserMessage { user_message_id }
            }
            domain::RunInput::EditUserMessage {
                user_message_id,
                content,
            } => Self::EditUserMessage {
                user_message_id,
                content,
            },
            domain::RunInput::RegenerateAfterUserMessage { user_message_id } => {
                Self::RegenerateAfterUserMessage { user_message_id }
            }
        }
    }
}

impl From<api::RunInput> for domain::RunInput {
    fn from(value: api::RunInput) -> Self {
        match value {
            api::RunInput::Text { text } => Self::Text { text },
            api::RunInput::FromUserMessage { user_message_id } => {
                Self::FromUserMessage { user_message_id }
            }
            api::RunInput::EditUserMessage {
                user_message_id,
                content,
            } => Self::EditUserMessage {
                user_message_id,
                content,
            },
            api::RunInput::RegenerateAfterUserMessage { user_message_id } => {
                Self::RegenerateAfterUserMessage { user_message_id }
            }
        }
    }
}

impl From<domain::RunOverrides> for api::RunOverrides {
    fn from(value: domain::RunOverrides) -> Self {
        Self {
            agent: value.agent,
            model_id: value.model_id,
            mcp: value.mcp.map(Into::into),
        }
    }
}

impl From<api::RunOverrides> for domain::RunOverrides {
    fn from(value: api::RunOverrides) -> Self {
        Self {
            agent: value.agent,
            model_id: value.model_id,
            mcp: value.mcp.map(Into::into),
        }
    }
}

impl From<domain::RunState> for api::RunState {
    fn from(value: domain::RunState) -> Self {
        match value {
            domain::RunState::Queued => Self::Queued,
            domain::RunState::Running => Self::Running,
            domain::RunState::Done => Self::Done,
            domain::RunState::Error => Self::Error,
            domain::RunState::Cancelled => Self::Cancelled,
        }
    }
}

impl From<domain::RunError> for api::RunError {
    fn from(value: domain::RunError) -> Self {
        Self {
            code: value.code,
            message: value.message,
        }
    }
}

impl From<domain::Run> for api::Run {
    fn from(value: domain::Run) -> Self {
        Self {
            id: value.id,
            session_id: value.session_id,
            state: value.state.into(),
            created_at: value.created_at,
            started_at: value.started_at,
            finished_at: value.finished_at,
            finish_reason: value.finish_reason,
            error: value.error.map(Into::into),
            input: value.input.into(),
            overrides: value.overrides.map(Into::into),
        }
    }
}

impl From<api::SessionCreateSettings> for domain::SessionCreateSettings {
    fn from(value: api::SessionCreateSettings) -> Self {
        Self {
            agent: value.agent,
            model_id: value.model_id,
            skills: value.skills.map(Into::into),
            mcp: value.mcp.map(Into::into),
            workspace_root: value.workspace_root,
            extra_workspace_roots: value.extra_workspace_roots,
        }
    }
}

impl From<api::SessionCreateRequest> for domain::SessionCreateRequest {
    fn from(value: api::SessionCreateRequest) -> Self {
        Self {
            title: value.title,
            settings: value.settings.map(Into::into),
        }
    }
}

impl From<api::SessionSettingsPatch> for domain::SessionSettingsPatch {
    fn from(value: api::SessionSettingsPatch) -> Self {
        Self {
            agent: value.agent,
            model_id: value.model_id,
            skills: value.skills.map(Into::into),
            mcp: value.mcp.map(Into::into),
            extra_workspace_roots: value.extra_workspace_roots,
        }
    }
}

impl From<api::SessionSaveDefaultsRequest> for domain::SessionSaveDefaultsRequest {
    fn from(value: api::SessionSaveDefaultsRequest) -> Self {
        Self {
            model: value.model,
            agent: value.agent,
            mcp: value.mcp,
            skills: value.skills,
        }
    }
}

impl From<api::ForkSessionRequest> for domain::ForkSessionRequest {
    fn from(value: api::ForkSessionRequest) -> Self {
        Self {
            message_id: value.message_id,
        }
    }
}

impl From<domain::ForkSessionResponse> for api::ForkSessionResponse {
    fn from(value: domain::ForkSessionResponse) -> Self {
        Self {
            session: value.session.into(),
        }
    }
}

impl From<api::RunCreateRequest> for domain::RunCreateRequest {
    fn from(value: api::RunCreateRequest) -> Self {
        Self {
            input: value.input.into(),
            overrides: value.overrides.map(Into::into),
            auto_resume: value.auto_resume,
        }
    }
}
