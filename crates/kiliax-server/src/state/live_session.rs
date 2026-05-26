use async_trait::async_trait;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::http::StatusCode;
use kiliax_core::agents::{AgentProfile, AgentToolset};
use kiliax_core::compact;
use kiliax_core::config::Config;
use kiliax_core::protocol::{Message as CoreMessage, UserContentPart, UserMessageContent};
use kiliax_core::runtime::{AgentEvent, AgentRuntime, AgentRuntimeError, AgentRuntimeOptions};
use kiliax_core::session::{FileSessionStore, SessionId, SessionMcpServerSetting, SessionState};
use kiliax_core::session::{SessionGoal, SessionGoalStatus};
use kiliax_core::tools::builtin::multi_agents as core_ma;
use kiliax_core::tools::{Permissions, ShellPermissions};
use kiliax_core::tools::{ToolEngine, ToolError};
use tokio::sync::{broadcast, watch, Mutex, Notify};
use tokio_stream::StreamExt;
use tracing::{Instrument, Span};

use crate::error::{ApiError, ApiErrorCode};
use crate::infra::{validate_client_extra_workspace_roots, validate_client_workspace_root};

use super::multi_agent::{default_root_identity, MultiAgentIdentity, MultiAgentToolBackend};
use super::preamble::{build_preamble, replace_preamble, replace_preamble_with_ids};
use super::{
    append_event, apply_settings_patch, config_with_mcp_overrides,
    custom_tools_config_from_settings, error_chain_vec, format_error_chain_text,
    map_core_message_to_domain_event_message, map_mcp_status, map_session_err, merge_mcp_settings,
    new_run_id, now_rfc3339, read_last_event_id, resolve_session_settings, runtime_error_code,
    runtime_error_hint, session_events_api_path, skills_config_from_settings, ts_ms_to_rfc3339,
    write_run_file, MultiAgentControl, ServerState,
};

use super::domain;
#[cfg(test)]
use super::{default_settings, read_events_after};

pub struct LiveSession {
    session_id: SessionId,
    store: FileSessionStore,
    config: Arc<ArcSwap<Config>>,
    runs_dir: PathBuf,
    fallback_workspace_root: PathBuf,
    runner_enabled: bool,

    session: Mutex<SessionState>,
    settings: Mutex<domain::SessionSettings>,
    settings_dirty: AtomicBool,
    closing: AtomicBool,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,

    tools: Mutex<ToolEngine>,
    goal_backend: Mutex<Option<Arc<dyn kiliax_core::tools::builtin::GoalBackend>>>,
    multi_agent_control: Option<std::sync::Weak<MultiAgentControl>>,

    status: Mutex<domain::SessionStatus>,
    queue: Mutex<VecDeque<QueuedRun>>,
    notify: Notify,
    active_cancel: Mutex<Option<watch::Sender<bool>>>,

    events_api_path: PathBuf,
    events_tx: broadcast::Sender<domain::Event>,
    events_ring: Mutex<VecDeque<domain::Event>>,
    events_ring_size: AtomicUsize,
    next_event_id: AtomicU64,
    stream_snapshot: Mutex<Option<domain::StreamSnapshot>>,
    self_weak: std::sync::OnceLock<std::sync::Weak<LiveSession>>,
}

#[derive(Debug, Clone)]
struct QueuedRun {
    run: domain::Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventPersistence {
    Durable,
    Ephemeral,
}

fn user_content_has_input(content: &UserMessageContent) -> bool {
    match content {
        UserMessageContent::Text(text) => !text.trim().is_empty(),
        UserMessageContent::Parts(parts) => parts.iter().any(|part| match part {
            UserContentPart::Text { text } => !text.trim().is_empty(),
            UserContentPart::Image { .. } | UserContentPart::File { .. } => true,
        }),
    }
}

fn user_content_trace_text(content: &UserMessageContent) -> String {
    let first_text = content.first_text().unwrap_or("").trim();
    if !first_text.is_empty() {
        first_text.to_string()
    } else {
        content.display_text()
    }
}

fn append_text_to_user_content(content: UserMessageContent, text: String) -> UserMessageContent {
    if text.trim().is_empty() {
        return content;
    }
    match content {
        UserMessageContent::Text(existing) => {
            if existing.trim().is_empty() {
                UserMessageContent::Text(text)
            } else {
                UserMessageContent::Text(format!("{text}\n\n{existing}"))
            }
        }
        UserMessageContent::Parts(mut parts) => {
            parts.insert(0, UserContentPart::Text { text });
            UserMessageContent::Parts(parts)
        }
    }
}

fn render_mailbox_messages(messages: &[core_ma::MailboxUpdate]) -> String {
    if messages.is_empty() {
        return String::new();
    }
    let mut out = String::from("<inter_agent_messages>");
    for message in messages {
        out.push_str("\nFrom ");
        out.push_str(&message.from);
        out.push_str(":\n");
        out.push_str(message.message.trim());
        out.push('\n');
    }
    out.push_str("</inter_agent_messages>");
    out
}

fn last_assistant_text(messages: &[CoreMessage]) -> Option<String> {
    messages.iter().rev().find_map(|message| match message {
        CoreMessage::Assistant {
            content: Some(content),
            ..
        } => {
            let content = content.trim();
            (!content.is_empty()).then(|| content.to_string())
        }
        _ => None,
    })
}

fn run_text_content(
    text: &str,
    attachments: &[domain::RunAttachment],
) -> Result<UserMessageContent, ApiError> {
    let has_text = !text.trim().is_empty();
    if !has_text && attachments.is_empty() {
        return Err(ApiError::invalid_argument(
            "input text or attachments must not be empty",
        ));
    }
    if attachments.len() > 8 {
        return Err(ApiError::invalid_argument("too many attachments"));
    }
    if attachments.is_empty() {
        return Ok(UserMessageContent::Text(text.to_string()));
    }

    let mut parts = Vec::new();
    if has_text {
        parts.push(UserContentPart::Text {
            text: text.to_string(),
        });
    }
    for attachment in attachments {
        let filename = attachment.filename.trim();
        let media_type = attachment.media_type.trim().to_ascii_lowercase();
        let data = attachment.data.trim();
        if filename.is_empty() {
            return Err(ApiError::invalid_argument(
                "attachment filename must not be empty",
            ));
        }
        if data.is_empty() {
            return Err(ApiError::invalid_argument(
                "attachment base64 data must not be empty",
            ));
        }
        if data.starts_with("data:") {
            return Err(ApiError::invalid_argument(
                "attachment data must be raw base64, not a data URL",
            ));
        }
        if is_supported_image_media_type(&media_type) {
            parts.push(UserContentPart::Image {
                path: format!("data:{media_type};base64,{data}"),
                filename: Some(filename.to_string()),
                detail: None,
            });
        } else if media_type == "application/pdf" {
            parts.push(UserContentPart::File {
                filename: filename.to_string(),
                media_type,
                data: data.to_string(),
            });
        } else {
            return Err(ApiError::invalid_argument(format!(
                "unsupported attachment media type `{}`",
                attachment.media_type
            )));
        }
    }
    Ok(UserMessageContent::Parts(parts))
}

fn is_supported_image_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

fn api_error_to_tool_error(err: ApiError) -> ToolError {
    ToolError::InvalidCommand(err.to_string())
}

fn session_error_to_tool_error(err: kiliax_core::session::SessionError) -> ToolError {
    ToolError::Io(std::io::Error::other(err))
}

fn permissions_allow_child(parent: &Permissions, child: &Permissions) -> bool {
    if child.file_read && !parent.file_read {
        return false;
    }
    if child.file_write && !parent.file_write {
        return false;
    }
    shell_permissions_allow_child(&parent.shell, &child.shell)
}

fn shell_permissions_allow_child(parent: &ShellPermissions, child: &ShellPermissions) -> bool {
    match (parent, child) {
        (_, ShellPermissions::DenyAll) => true,
        (ShellPermissions::AllowAll, _) => true,
        (ShellPermissions::DenyAll, _) => false,
        (
            ShellPermissions::AllowList(parent_prefixes),
            ShellPermissions::AllowList(child_prefixes),
        ) => child_prefixes.iter().all(|child_prefix| {
            parent_prefixes
                .iter()
                .any(|parent_prefix| parent_prefix == child_prefix)
        }),
        (ShellPermissions::AllowList(_), ShellPermissions::AllowAll) => false,
    }
}

fn parse_fork_turns(raw: Option<&str>) -> Result<Option<usize>, ToolError> {
    let raw = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("all");
    if raw.eq_ignore_ascii_case("none") {
        return Ok(Some(0));
    }
    if raw.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let n = raw.parse::<usize>().map_err(|_| {
        ToolError::InvalidCommand(
            "fork_turns must be `none`, `all`, or a positive integer string".to_string(),
        )
    })?;
    if n == 0 {
        return Err(ToolError::InvalidCommand(
            "fork_turns must be `none`, `all`, or a positive integer string".to_string(),
        ));
    }
    Ok(Some(n))
}

fn goal_continuation_prompt(goal: &SessionGoal) -> String {
    let mut prompt = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../kiliax-core/prompts/goal/goal_continuation.md"
    ))
    .to_string();
    prompt.push_str("\n\nActive goal:\n");
    prompt.push_str(goal.objective.trim());
    prompt
}

fn rfc3339_after_ms(delay_ms: u64) -> String {
    use time::format_description::well_known::Rfc3339;

    let delay = time::Duration::milliseconds(delay_ms.min(i64::MAX as u64) as i64);
    (time::OffsetDateTime::now_utc() + delay)
        .format(&Rfc3339)
        .unwrap_or_else(|_| now_rfc3339())
}

impl LiveSession {
    fn config_snapshot(&self) -> Arc<Config> {
        self.config.load_full()
    }

    async fn install_tool_backends(&self, tools: &ToolEngine) -> Result<(), ApiError> {
        tools
            .set_goal_backend(self.goal_backend.lock().await.clone())
            .map_err(ApiError::internal_error)?;
        let Some(control) = self.multi_agent_control.as_ref().and_then(|c| c.upgrade()) else {
            tools
                .set_multi_agent_backend(None)
                .map_err(ApiError::internal_error)?;
            return Ok(());
        };
        let Some(self_weak) = self.self_weak.get().cloned() else {
            return Ok(());
        };
        let backend: Arc<dyn kiliax_core::tools::builtin::multi_agents::MultiAgentBackend> =
            Arc::new(MultiAgentToolBackend::new(
                self_weak,
                Arc::downgrade(&control),
            ));
        tools
            .set_multi_agent_backend(Some(backend))
            .map_err(ApiError::internal_error)?;
        Ok(())
    }

    pub(super) async fn settings_snapshot(&self) -> domain::SessionSettings {
        self.settings.lock().await.clone()
    }

    pub(super) async fn workspace_root(&self) -> PathBuf {
        self.settings.lock().await.workspace_root.clone()
    }

    pub(super) async fn multi_agent_identity(&self) -> MultiAgentIdentity {
        let session = self.session.lock().await;
        match (
            session.meta.root_session_id.clone(),
            session.meta.agent_path.clone(),
        ) {
            (Some(root_session_id), Some(agent_path)) => MultiAgentIdentity {
                session_id: self.session_id.clone(),
                root_session_id,
                parent_session_id: session.meta.parent_session_id.clone(),
                agent_path,
                closed: session.meta.closed,
            },
            _ => default_root_identity(self.session_id.clone(), session.meta.closed),
        }
    }

    pub(super) async fn multi_agent_status(&self) -> String {
        if self.session.lock().await.meta.closed {
            return "closed".to_string();
        }
        let st = self.status.lock().await.clone();
        if st.queue_len > 0 {
            return "queued".to_string();
        }
        match st.run_state {
            domain::SessionRunState::Idle => {
                let session = self.session.lock().await;
                if session.meta.last_error.is_some() {
                    "error".to_string()
                } else {
                    "idle".to_string()
                }
            }
            domain::SessionRunState::Running
            | domain::SessionRunState::Tooling
            | domain::SessionRunState::Retrying => "running".to_string(),
        }
    }

    pub async fn on_config_updated(&self) -> Result<(), ApiError> {
        let config = self.config_snapshot();
        let meta = { self.session.lock().await.meta.clone() };

        self.events_ring_size
            .store(config.server.events_ring_size, Ordering::SeqCst);
        if config.server.events_ring_size == 0 {
            self.events_ring.lock().await.clear();
        } else {
            let mut ring = self.events_ring.lock().await;
            while ring.len() > config.server.events_ring_size {
                ring.pop_front();
            }
        }

        let mut settings =
            resolve_session_settings(&meta, config.as_ref(), &self.fallback_workspace_root)?;

        let workspace_root = match settings
            .workspace_root
            .to_str()
            .and_then(|s| validate_client_workspace_root(s).ok())
        {
            Some(p) => p,
            None => self.default_tmp_workspace_root()?,
        };
        settings.workspace_root = workspace_root.clone();
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(ApiError::internal_error)?;

        *self.settings.lock().await = settings.clone();

        self.settings_dirty.store(true, Ordering::SeqCst);
        let is_idle = { self.status.lock().await.run_state == domain::SessionRunState::Idle };
        if is_idle {
            self.apply_settings_now(true).await?;
            self.settings_dirty.store(false, Ordering::SeqCst);
        }

        Ok(())
    }

    pub fn id(&self) -> &SessionId {
        &self.session_id
    }

    pub async fn resume(
        server: &ServerState,
        session_id: &SessionId,
    ) -> Result<Arc<Self>, ApiError> {
        let config = server.config_snapshot();
        let session = server
            .store
            .load(session_id)
            .await
            .map_err(map_session_err)?;
        let settings =
            resolve_session_settings(&session.meta, config.as_ref(), &server.workspace_root)?;
        let workspace_root = settings.workspace_root.clone();
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(ApiError::internal_error)?;

        let resumed_agent = settings.agent.clone();
        let resumed_model_id = settings.model_id.clone();
        let resumed_workspace_root = settings.workspace_root.clone();

        let mut cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
        cfg_for_tools.custom_tools = custom_tools_config_from_settings(&settings.custom_tools);
        let tools = ToolEngine::new(&workspace_root, cfg_for_tools);

        let live = Self::from_state(server, session, settings, tools, true).await?;
        tracing::info!(
            event = "session.resumed",
            session_id = %live.session_id,
            agent = %resumed_agent,
            model_id = %resumed_model_id,
            workspace_root = %resumed_workspace_root.display(),
        );
        Ok(live)
    }

    pub async fn from_state(
        server: &ServerState,
        session: SessionState,
        settings: domain::SessionSettings,
        tools: ToolEngine,
        rebuild_preamble: bool,
    ) -> Result<Arc<Self>, ApiError> {
        let events_api_path = session_events_api_path(&server.store, session.id());
        let last_event_id = read_last_event_id(&events_api_path).await?;
        let (events_tx, _) = broadcast::channel(2048);
        let events_ring_size = server.config_snapshot().server.events_ring_size;

        let live = Arc::new(Self {
            session_id: session.meta.id.clone(),
            store: server.store.clone(),
            config: server.config.clone(),
            runs_dir: server.runs_dir.clone(),
            fallback_workspace_root: server.workspace_root.clone(),
            runner_enabled: server.runner_enabled(),
            session: Mutex::new(session),
            settings: Mutex::new(settings.clone()),
            settings_dirty: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            worker: Mutex::new(None),
            tools: Mutex::new(tools),
            goal_backend: Mutex::new(None),
            multi_agent_control: Some(Arc::downgrade(&server.multi_agent_control)),
            status: Mutex::new(domain::SessionStatus {
                run_state: domain::SessionRunState::Idle,
                active_run_id: None,
                active_run_started_at: None,
                step: 0,
                active_tool: None,
                retry_status: None,
                queue_len: 0,
                last_event_id,
            }),
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            active_cancel: Mutex::new(None),
            events_api_path,
            events_tx,
            events_ring: Mutex::new(VecDeque::new()),
            events_ring_size: AtomicUsize::new(events_ring_size),
            next_event_id: AtomicU64::new(last_event_id),
            stream_snapshot: Mutex::new(None),
            self_weak: std::sync::OnceLock::new(),
        });
        let _ = live.self_weak.set(Arc::downgrade(&live));

        {
            let backend: Arc<dyn kiliax_core::tools::builtin::GoalBackend> = live.clone();
            *live.goal_backend.lock().await = Some(backend.clone());
            let tools = live.tools.lock().await;
            tools
                .set_goal_backend(Some(backend))
                .map_err(ApiError::internal_error)?;
            let multi_backend: Arc<
                dyn kiliax_core::tools::builtin::multi_agents::MultiAgentBackend,
            > = Arc::new(MultiAgentToolBackend::new(
                Arc::downgrade(&live),
                Arc::downgrade(&server.multi_agent_control),
            ));
            tools
                .set_multi_agent_backend(Some(multi_backend))
                .map_err(ApiError::internal_error)?;
        }

        // Ensure meta reflects current defaults when a session is resumed.
        {
            let mut session = live.session.lock().await;
            session.meta.agent = settings.agent.clone();
            session.meta.model_id = Some(settings.model_id.clone());
            session.meta.workspace_root = Some(settings.workspace_root.display().to_string());
            session.meta.extra_workspace_roots = settings
                .extra_workspace_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            session.meta.mcp_servers = settings
                .mcp
                .servers
                .iter()
                .map(|s| SessionMcpServerSetting {
                    id: s.id.clone(),
                    enable: s.enable,
                })
                .collect();
            session.meta.skills = Some(skills_config_from_settings(&settings.skills));
            session.meta.custom_tools =
                Some(custom_tools_config_from_settings(&settings.custom_tools));
            live.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        if rebuild_preamble {
            live.apply_settings_now(false).await?;
        }

        server
            .multi_agent_control
            .register_live_agent(live.multi_agent_identity().await, live.clone())
            .await;

        if server.runner_enabled() {
            let worker = live.clone();
            let handle = tokio::spawn(async move {
                worker.worker_loop().await;
            });
            *live.worker.lock().await = Some(handle);
        }

        Ok(live)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn from_parts(
        store: FileSessionStore,
        config: Arc<ArcSwap<Config>>,
        runs_dir: PathBuf,
        fallback_workspace_root: PathBuf,
        runner_enabled: bool,
        session: SessionState,
        settings: domain::SessionSettings,
        tools: ToolEngine,
        rebuild_preamble: bool,
        multi_agent_control: Option<Arc<MultiAgentControl>>,
    ) -> Result<Arc<Self>, ApiError> {
        let events_api_path = session_events_api_path(&store, session.id());
        let last_event_id = read_last_event_id(&events_api_path).await?;
        let (events_tx, _) = broadcast::channel(2048);
        let events_ring_size = config.load_full().server.events_ring_size;

        let live = Arc::new(Self {
            session_id: session.meta.id.clone(),
            store,
            config,
            runs_dir,
            fallback_workspace_root,
            runner_enabled,
            session: Mutex::new(session),
            settings: Mutex::new(settings.clone()),
            settings_dirty: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            worker: Mutex::new(None),
            tools: Mutex::new(tools),
            goal_backend: Mutex::new(None),
            multi_agent_control: multi_agent_control.as_ref().map(Arc::downgrade),
            status: Mutex::new(domain::SessionStatus {
                run_state: domain::SessionRunState::Idle,
                active_run_id: None,
                active_run_started_at: None,
                step: 0,
                active_tool: None,
                retry_status: None,
                queue_len: 0,
                last_event_id,
            }),
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            active_cancel: Mutex::new(None),
            events_api_path,
            events_tx,
            events_ring: Mutex::new(VecDeque::new()),
            events_ring_size: AtomicUsize::new(events_ring_size),
            next_event_id: AtomicU64::new(last_event_id),
            stream_snapshot: Mutex::new(None),
            self_weak: std::sync::OnceLock::new(),
        });
        let _ = live.self_weak.set(Arc::downgrade(&live));

        {
            let backend: Arc<dyn kiliax_core::tools::builtin::GoalBackend> = live.clone();
            *live.goal_backend.lock().await = Some(backend);
            let tools = live.tools.lock().await;
            live.install_tool_backends(&tools).await?;
        }

        {
            let mut session = live.session.lock().await;
            session.meta.agent = settings.agent.clone();
            session.meta.model_id = Some(settings.model_id.clone());
            session.meta.workspace_root = Some(settings.workspace_root.display().to_string());
            session.meta.extra_workspace_roots = settings
                .extra_workspace_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            session.meta.mcp_servers = settings
                .mcp
                .servers
                .iter()
                .map(|s| SessionMcpServerSetting {
                    id: s.id.clone(),
                    enable: s.enable,
                })
                .collect();
            session.meta.skills = Some(skills_config_from_settings(&settings.skills));
            session.meta.custom_tools =
                Some(custom_tools_config_from_settings(&settings.custom_tools));
            live.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        if rebuild_preamble {
            live.apply_settings_now(false).await?;
        }

        if runner_enabled {
            let worker = live.clone();
            let handle = tokio::spawn(async move {
                worker.worker_loop().await;
            });
            *live.worker.lock().await = Some(handle);
        }

        Ok(live)
    }

    fn default_tmp_workspace_root(&self) -> Result<PathBuf, ApiError> {
        if self.runner_enabled {
            crate::infra::default_tmp_workspace_root()
        } else {
            Ok(self
                .fallback_workspace_root
                .join(".kiliax")
                .join("workspace")
                .join(format!("tmp_{}", SessionId::new())))
        }
    }

    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);

        let queued = {
            let q = self.queue.lock().await;
            q.iter().map(|r| r.run.id.clone()).collect::<Vec<_>>()
        };
        for run_id in queued {
            let _ = self.cancel_run(&self.runs_dir, &run_id).await;
        }

        let active = { self.status.lock().await.active_run_id.clone() };
        if let Some(run_id) = active {
            let _ = self.cancel_run(&self.runs_dir, &run_id).await;
        }

        self.notify.notify_one();

        let handle = self.worker.lock().await.take();
        let Some(mut handle) = handle else {
            return;
        };

        let timeout = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(timeout);
        tokio::select! {
            _ = &mut handle => {}
            _ = &mut timeout => {
                handle.abort();
                let _ = handle.await;
            }
        }
    }

    pub async fn is_idle_for_eviction(&self) -> bool {
        let q = self.queue.lock().await;
        let st = self.status.lock().await;
        q.is_empty() && st.run_state == domain::SessionRunState::Idle
    }

    pub async fn summary(&self) -> Result<domain::SessionSummary, ApiError> {
        let session = self.session.lock().await;
        let settings = self.settings.lock().await.clone();
        let status = self.status.lock().await.clone();
        let last_outcome = if session.meta.last_error.is_some() {
            domain::SessionLastOutcome::Error
        } else if session.meta.last_finish_reason.is_some() {
            domain::SessionLastOutcome::Done
        } else {
            domain::SessionLastOutcome::None
        };
        Ok(domain::SessionSummary {
            id: self.session_id.to_string(),
            title: session
                .meta
                .title
                .clone()
                .unwrap_or_else(|| self.session_id.to_string()),
            created_at: ts_ms_to_rfc3339(session.meta.created_at_ms),
            updated_at: ts_ms_to_rfc3339(session.meta.updated_at_ms),
            last_outcome,
            status,
            settings,
            goal: session.meta.goal.clone(),
        })
    }

    pub async fn snapshot(&self) -> Result<domain::SessionSnapshot, ApiError> {
        let tools = { self.tools.lock().await.clone() };
        Ok(domain::SessionSnapshot {
            summary: self.summary().await?,
            mcp_status: map_mcp_status(tools.mcp_status().await),
            stream: self.stream_snapshot.lock().await.clone(),
        })
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<domain::Event> {
        self.events_tx.subscribe()
    }

    pub async fn last_event_id(&self) -> u64 {
        self.status.lock().await.last_event_id
    }

    pub async fn backlog_after(
        &self,
        after_event_id: u64,
        limit: usize,
    ) -> Option<Vec<domain::Event>> {
        let ring = self.events_ring.lock().await;
        let first = ring.front()?.event_id;
        if after_event_id.saturating_add(1) < first {
            return None;
        }
        let out = ring
            .iter()
            .filter(|e| e.event_id > after_event_id)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        Some(out)
    }

    pub async fn patch_settings(
        &self,
        patch: domain::SessionSettingsPatch,
    ) -> Result<(), ApiError> {
        let config = self.config_snapshot();
        let mut settings = self.settings.lock().await.clone();
        let mut patch = patch;
        if let Some(roots) = patch.extra_workspace_roots.take() {
            let validated =
                validate_client_extra_workspace_roots(&roots, &settings.workspace_root)?;
            patch.extra_workspace_roots = Some(
                validated
                    .into_iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            );
        }

        apply_settings_patch(&mut settings, &patch, config.as_ref(), true)?;
        *self.settings.lock().await = settings.clone();

        {
            let mut session = self.session.lock().await;
            session.meta.agent = settings.agent.clone();
            session.meta.model_id = Some(settings.model_id.clone());
            session.meta.workspace_root = Some(settings.workspace_root.display().to_string());
            session.meta.extra_workspace_roots = settings
                .extra_workspace_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            session.meta.mcp_servers = settings
                .mcp
                .servers
                .iter()
                .map(|s| SessionMcpServerSetting {
                    id: s.id.clone(),
                    enable: s.enable,
                })
                .collect();
            session.meta.skills = Some(skills_config_from_settings(&settings.skills));
            session.meta.custom_tools =
                Some(custom_tools_config_from_settings(&settings.custom_tools));
            self.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        self.settings_dirty.store(true, Ordering::SeqCst);

        let is_idle = { self.status.lock().await.run_state == domain::SessionRunState::Idle };
        if is_idle {
            self.apply_settings_now(true).await?;
            self.settings_dirty.store(false, Ordering::SeqCst);
        }

        Ok(())
    }

    pub async fn goal(&self) -> Option<SessionGoal> {
        self.session.lock().await.meta.goal.clone()
    }

    pub async fn set_goal(&self, objective: String) -> Result<SessionGoal, ApiError> {
        let goal = {
            let mut session = self.session.lock().await;
            let goal = self
                .store
                .set_goal(&mut session, objective)
                .await
                .map_err(map_session_err)?;
            self.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
            goal
        };
        self.emit_event(domain::Event {
            event_id: self.alloc_event_id(),
            ts: now_rfc3339(),
            session_id: self.session_id.to_string(),
            run_id: None,
            event_type: "session_goal_changed".to_string(),
            data: serde_json::json!({ "goal": goal }),
        })
        .await?;
        Ok(goal)
    }

    pub async fn clear_goal(&self) -> Result<(), ApiError> {
        {
            let mut q = self.queue.lock().await;
            q.retain(|item| !matches!(item.run.input, domain::RunInput::GoalContinuation));
            self.status.lock().await.queue_len = q.len();
        }
        {
            let mut session = self.session.lock().await;
            self.store
                .clear_goal(&mut session)
                .await
                .map_err(map_session_err)?;
            self.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }
        self.emit_event(domain::Event {
            event_id: self.alloc_event_id(),
            ts: now_rfc3339(),
            session_id: self.session_id.to_string(),
            run_id: None,
            event_type: "session_goal_changed".to_string(),
            data: serde_json::json!({ "goal": null }),
        })
        .await?;
        Ok(())
    }

    async fn add_goal_usage_and_emit(
        &self,
        time_used_seconds: u64,
        tokens_used: u64,
    ) -> Result<(), ApiError> {
        if time_used_seconds == 0 && tokens_used == 0 {
            return Ok(());
        }
        let goal = {
            let mut session = self.session.lock().await;
            if session.meta.goal.is_none() {
                return Ok(());
            }
            self.store
                .add_goal_usage(&mut session, time_used_seconds, tokens_used)
                .await
                .map_err(map_session_err)?;
            session.meta.goal.clone()
        };
        self.emit_event(domain::Event {
            event_id: self.alloc_event_id(),
            ts: now_rfc3339(),
            session_id: self.session_id.to_string(),
            run_id: None,
            event_type: "session_goal_changed".to_string(),
            data: serde_json::json!({ "goal": goal }),
        })
        .await?;
        Ok(())
    }

    async fn ensure_history_mutable(&self) -> Result<(), ApiError> {
        let q = self.queue.lock().await;
        let st = self.status.lock().await;

        let busy =
            !q.is_empty() || st.queue_len > 0 || st.run_state != domain::SessionRunState::Idle;
        if busy {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                ApiErrorCode::Conflict,
                "session is busy",
            ));
        }
        Ok(())
    }

    async fn apply_edit_user_message(
        &self,
        user_message_id: u64,
        content: &str,
    ) -> Result<(), ApiError> {
        self.ensure_history_mutable().await?;

        let text = content.trim();
        if text.is_empty() {
            return Err(ApiError::invalid_argument("content must not be empty"));
        }

        {
            let mut session = self.session.lock().await;
            let idx = session
                .message_ids
                .iter()
                .position(|id| *id == user_message_id)
                .ok_or_else(|| ApiError::not_found("message not found"))?;
            if !matches!(session.messages[idx], CoreMessage::User { .. }) {
                return Err(ApiError::invalid_argument(
                    "user_message_id must refer to a user message",
                ));
            }

            self.store
                .edit_message(
                    &mut session,
                    user_message_id,
                    CoreMessage::User {
                        content: UserMessageContent::Text(text.to_string()),
                        hidden: false,
                    },
                )
                .await
                .map_err(map_session_err)?;
            self.store
                .truncate_after(&mut session, user_message_id)
                .await
                .map_err(map_session_err)?;
            self.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        self.emit_event(domain::Event {
            event_id: self.alloc_event_id(),
            ts: now_rfc3339(),
            session_id: self.session_id.to_string(),
            run_id: None,
            event_type: "session_messages_reset".to_string(),
            data: serde_json::json!({ "after_message_id": user_message_id, "reason": "edit" }),
        })
        .await?;
        Ok(())
    }

    async fn apply_regenerate_after_user_message(
        &self,
        user_message_id: u64,
    ) -> Result<(), ApiError> {
        self.ensure_history_mutable().await?;

        {
            let session = self.session.lock().await;
            let idx = session
                .message_ids
                .iter()
                .position(|id| *id == user_message_id)
                .ok_or_else(|| ApiError::not_found("message not found"))?;
            match &session.messages[idx] {
                CoreMessage::User { content, hidden } => {
                    if *hidden {
                        return Err(ApiError::invalid_argument(
                            "user_message_id must refer to a visible user message",
                        ));
                    }
                    if !user_content_has_input(content) {
                        return Err(ApiError::invalid_argument(
                            "cannot regenerate: user message is empty",
                        ));
                    }
                }
                _ => {
                    return Err(ApiError::invalid_argument(
                        "user_message_id must refer to a user message",
                    ));
                }
            }
        }

        {
            let mut session = self.session.lock().await;
            self.store
                .truncate_after(&mut session, user_message_id)
                .await
                .map_err(map_session_err)?;
            self.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        self.emit_event(domain::Event {
            event_id: self.alloc_event_id(),
            ts: now_rfc3339(),
            session_id: self.session_id.to_string(),
            run_id: None,
            event_type: "session_messages_reset".to_string(),
            data: serde_json::json!({
                "after_message_id": user_message_id,
                "reason": "regenerate",
            }),
        })
        .await?;
        Ok(())
    }

    async fn compact_session_history(
        &self,
        base_settings: &domain::SessionSettings,
        effective: &domain::SessionSettings,
        profile: &AgentProfile,
        tools_for_run: &ToolEngine,
        llm: &kiliax_core::llm::LlmClient,
    ) -> Result<(), ApiError> {
        let (messages_snapshot, message_ids_snapshot) = {
            let session = self.session.lock().await;
            (session.messages.clone(), session.message_ids.clone())
        };

        let summary_suffix = compact::run_compaction(llm, &messages_snapshot)
            .await
            .map_err(ApiError::internal_error)?;
        let user_messages = compact::collect_real_user_texts(&messages_snapshot);
        let compacted_history =
            compact::build_compacted_user_history(&user_messages, &summary_suffix);

        let cutoff_id = compact::find_preamble_cutoff_id(&messages_snapshot, &message_ids_snapshot)
            .or_else(|| message_ids_snapshot.first().copied())
            .unwrap_or(0);

        let skills_config = skills_config_from_settings(&base_settings.skills);
        let project_prompt =
            kiliax_core::prompt::capture_project_prompt(Some(effective.workspace_root.as_path()))
                .or_else(|| Some(String::new()));
        let new_preamble = build_preamble(
            profile,
            &effective.model_id,
            &effective.workspace_root,
            project_prompt.clone(),
            tools_for_run,
            &skills_config,
        )
        .await;

        {
            let mut session = self.session.lock().await;
            if cutoff_id > 0 {
                self.store
                    .truncate_after(&mut session, cutoff_id)
                    .await
                    .map_err(map_session_err)?;
            }

            let mut last_seq = session.meta.last_seq;
            let mut messages = std::mem::take(&mut session.messages);
            let mut message_ids = std::mem::take(&mut session.message_ids);
            replace_preamble_with_ids(&mut messages, &mut message_ids, &mut last_seq, new_preamble);
            session.messages = messages;
            session.message_ids = message_ids;
            session.meta.project_prompt = project_prompt;
            session.meta.last_seq = last_seq;
            for msg in compacted_history {
                self.store
                    .record_message(&mut session, msg)
                    .await
                    .map_err(map_session_err)?;
            }

            self.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        tracing::info!(
            event = "session.compacted",
            session_id = %self.session_id,
            model_id = %effective.model_id,
        );

        Ok(())
    }

    pub async fn enqueue_run(
        &self,
        runs_dir: &Path,
        req: domain::RunCreateRequest,
    ) -> Result<domain::Run, ApiError> {
        match &req.input {
            domain::RunInput::Text { text, attachments } => {
                run_text_content(text, attachments)?;
            }
            domain::RunInput::FromUserMessage { user_message_id } => {
                if *user_message_id == 0 {
                    return Err(ApiError::invalid_argument("user_message_id must be >= 1"));
                }
                let session = self.session.lock().await;
                let idx = session
                    .message_ids
                    .iter()
                    .position(|id| *id == *user_message_id)
                    .ok_or_else(|| ApiError::not_found("message not found"))?;
                if idx != session.messages.len().saturating_sub(1) {
                    return Err(ApiError::invalid_argument(
                        "user_message_id must refer to the last message",
                    ));
                }
                match &session.messages[idx] {
                    CoreMessage::User { content, hidden } => {
                        if *hidden {
                            return Err(ApiError::invalid_argument(
                                "user_message_id must refer to a visible user message",
                            ));
                        }
                        if !user_content_has_input(content) {
                            return Err(ApiError::invalid_argument(
                                "input text or attachments must not be empty",
                            ));
                        }
                    }
                    _ => {
                        return Err(ApiError::invalid_argument(
                            "user_message_id must refer to a user message",
                        ));
                    }
                }
            }
            domain::RunInput::EditUserMessage {
                user_message_id,
                content,
            } => {
                if *user_message_id == 0 {
                    return Err(ApiError::invalid_argument("user_message_id must be >= 1"));
                }
                self.apply_edit_user_message(*user_message_id, content)
                    .await?;
            }
            domain::RunInput::RegenerateAfterUserMessage { user_message_id } => {
                if *user_message_id == 0 {
                    return Err(ApiError::invalid_argument("user_message_id must be >= 1"));
                }
                self.apply_regenerate_after_user_message(*user_message_id)
                    .await?;
            }
            domain::RunInput::GoalContinuation => {
                return Err(ApiError::invalid_argument(
                    "goal continuation runs are internal",
                ));
            }
        }

        let run = domain::Run {
            id: new_run_id(),
            session_id: self.session_id.to_string(),
            state: domain::RunState::Queued,
            created_at: now_rfc3339(),
            started_at: None,
            finished_at: None,
            finish_reason: None,
            error: None,
            input: req.input,
            overrides: req.overrides,
        };

        write_run_file(runs_dir, &run).await?;

        {
            let mut q = self.queue.lock().await;
            q.push_back(QueuedRun { run: run.clone() });
            self.status.lock().await.queue_len = q.len();
        }
        self.notify.notify_one();

        Ok(run)
    }

    pub async fn cancel_run(&self, runs_dir: &Path, run_id: &str) -> Result<(), ApiError> {
        // queued
        {
            let mut q = self.queue.lock().await;
            if let Some(pos) = q.iter().position(|r| r.run.id == run_id) {
                let mut run = q.remove(pos).expect("pos checked").run;
                run.state = domain::RunState::Cancelled;
                run.finished_at = Some(now_rfc3339());
                write_run_file(runs_dir, &run).await?;
                self.status.lock().await.queue_len = q.len();

                self.emit_event(domain::Event {
                    event_id: self.alloc_event_id(),
                    ts: now_rfc3339(),
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "run_cancelled".to_string(),
                    data: serde_json::json!({ "reason": "cancelled" }),
                })
                .await?;

                return Ok(());
            }
        }

        // running
        {
            let st = self.status.lock().await;
            if st.active_run_id.as_deref() != Some(run_id) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    ApiErrorCode::RunNotCancellable,
                    "run is not queued or running",
                ));
            }
        }

        let tx = self.active_cancel.lock().await.clone().ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                ApiErrorCode::RunNotCancellable,
                "run cannot be cancelled",
            )
        })?;
        let _ = tx.send(true);
        Ok(())
    }

    pub(super) async fn spawn_multi_agent(
        &self,
        control: Arc<MultiAgentControl>,
        args: core_ma::SpawnAgentArgs,
    ) -> Result<core_ma::SpawnAgentResult, ToolError> {
        let cfg = self.config_snapshot();
        if !cfg.multi_agent.enabled {
            return Err(ToolError::InvalidCommand(
                "multi-agent tools are disabled by config".to_string(),
            ));
        }
        let parent_identity = self.multi_agent_identity().await;
        let task_name = args.task_name.trim().to_string();
        let child_path = control
            .reserve_spawn(
                &parent_identity,
                &task_name,
                cfg.multi_agent.max_concurrent_agents_per_root,
                cfg.multi_agent.max_depth,
            )
            .await?;

        let result = self
            .spawn_multi_agent_reserved(
                control.clone(),
                args,
                parent_identity.clone(),
                child_path.clone(),
            )
            .await;
        if result.is_err() {
            control
                .release_reserved_path(&parent_identity.root_session_id, &child_path)
                .await;
        }
        result
    }

    async fn spawn_multi_agent_reserved(
        &self,
        control: Arc<MultiAgentControl>,
        args: core_ma::SpawnAgentArgs,
        parent_identity: MultiAgentIdentity,
        child_path: String,
    ) -> Result<core_ma::SpawnAgentResult, ToolError> {
        let message = args.message.trim();
        if message.is_empty() {
            return Err(ToolError::InvalidCommand(
                "message must not be empty".to_string(),
            ));
        }

        let config = self.config_snapshot();
        let parent_settings = self.settings.lock().await.clone();
        let agent_type = args
            .agent_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("general")
            .to_string();
        let child_profile = AgentProfile::from_name(&agent_type).ok_or_else(|| {
            ToolError::InvalidCommand(format!("agent not supported: {agent_type}"))
        })?;
        if !child_profile.subagent {
            return Err(ToolError::InvalidCommand(format!(
                "agent is not spawnable: {agent_type}"
            )));
        }
        let parent_profile = AgentProfile::from_name(&parent_settings.agent).ok_or_else(|| {
            ToolError::InvalidCommand(format!("agent not supported: {}", parent_settings.agent))
        })?;
        if !permissions_allow_child(&parent_profile.permissions, &child_profile.permissions) {
            return Err(ToolError::PermissionDenied(
                "spawn_agent cannot create a child with broader permissions than the parent"
                    .to_string(),
            ));
        }

        let model_id = args
            .model_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(parent_settings.model_id.as_str())
            .to_string();
        config
            .resolve_model(&model_id)
            .map_err(|err| ToolError::InvalidCommand(err.to_string()))?;

        let mut child_settings = parent_settings.clone();
        child_settings.agent = child_profile.name.clone();
        child_settings.model_id = model_id.clone();

        let mut cfg_for_tools =
            config_with_mcp_overrides(config.as_ref(), &child_settings.mcp.servers)
                .map_err(api_error_to_tool_error)?;
        cfg_for_tools.custom_tools =
            custom_tools_config_from_settings(&child_settings.custom_tools);
        let child_tools = ToolEngine::new(&child_settings.workspace_root, cfg_for_tools);
        child_tools.set_extra_workspace_roots(child_settings.extra_workspace_roots.clone())?;
        if child_profile
            .tools
            .toolsets
            .contains(&AgentToolset::MultiAgent)
        {
            let self_weak = self.self_weak.get().cloned().ok_or_else(|| {
                ToolError::InvalidCommand("current agent session is not registered".to_string())
            })?;
            let backend: Arc<dyn kiliax_core::tools::builtin::multi_agents::MultiAgentBackend> =
                Arc::new(MultiAgentToolBackend::new(
                    self_weak,
                    Arc::downgrade(&control),
                ));
            child_tools.set_multi_agent_backend(Some(backend))?;
        }

        let skills_config = skills_config_from_settings(&child_settings.skills);
        let project_prompt = { self.session.lock().await.meta.project_prompt.clone() };
        let mut initial_messages = build_preamble(
            &child_profile,
            &child_settings.model_id,
            &child_settings.workspace_root,
            project_prompt.clone(),
            &child_tools,
            &skills_config,
        )
        .await;
        initial_messages.extend(self.fork_messages(args.fork_turns.as_deref()).await?);

        let mut child_state = self
            .store
            .create(
                child_profile.name.clone(),
                Some(child_settings.model_id.clone()),
                None,
                Some(child_settings.workspace_root.display().to_string()),
                child_settings
                    .extra_workspace_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                initial_messages,
            )
            .await
            .map_err(session_error_to_tool_error)?;

        child_state.meta.root_session_id = Some(parent_identity.root_session_id.clone());
        child_state.meta.parent_session_id = Some(parent_identity.session_id.clone());
        child_state.meta.agent_path = Some(child_path.clone());
        child_state.meta.agent_role = Some(child_profile.name.clone());
        child_state.meta.closed = false;
        self.store
            .checkpoint(&mut child_state)
            .await
            .map_err(session_error_to_tool_error)?;

        let child_session_id = child_state.meta.id.clone();
        let child_live = LiveSession::from_parts(
            self.store.clone(),
            self.config.clone(),
            self.runs_dir.clone(),
            self.fallback_workspace_root.clone(),
            self.runner_enabled,
            child_state,
            child_settings,
            child_tools,
            false,
            Some(control.clone()),
        )
        .await
        .map_err(api_error_to_tool_error)?;
        control
            .register_live_agent(child_live.multi_agent_identity().await, child_live.clone())
            .await;
        control
            .set_last_task_message(&child_session_id, message.to_string())
            .await;
        child_live
            .enqueue_run(
                &self.runs_dir,
                domain::RunCreateRequest {
                    input: domain::RunInput::Text {
                        text: message.to_string(),
                        attachments: Vec::new(),
                    },
                    overrides: None,
                    auto_resume: true,
                },
            )
            .await
            .map_err(api_error_to_tool_error)?;

        Ok(core_ma::SpawnAgentResult {
            task_name: child_path,
            session_id: child_session_id.to_string(),
        })
    }

    pub(super) async fn send_multi_agent_message(
        &self,
        control: Arc<MultiAgentControl>,
        args: core_ma::MessageAgentArgs,
        trigger_turn: bool,
    ) -> Result<core_ma::MessageAgentResult, ToolError> {
        let current = self.multi_agent_identity().await;
        let target = control.resolve_target(&current, &args.target).await?;
        if trigger_turn && target.agent_path == "/root" {
            return Err(ToolError::InvalidCommand(
                "followup_task cannot target the root agent".to_string(),
            ));
        }
        if trigger_turn {
            let live = control
                .live_for_session(&target.session_id)
                .await
                .ok_or_else(|| {
                    ToolError::InvalidCommand(format!(
                        "target agent `{}` is not live",
                        target.agent_path
                    ))
                })?;
            live.enqueue_run(
                &self.runs_dir,
                domain::RunCreateRequest {
                    input: domain::RunInput::Text {
                        text: format!(
                            "Message from {}:\n{}",
                            current.agent_path,
                            args.message.trim()
                        ),
                        attachments: Vec::new(),
                    },
                    overrides: None,
                    auto_resume: true,
                },
            )
            .await
            .map_err(api_error_to_tool_error)?;
        } else {
            control
                .queue_message(&current, &target, args.message.trim().to_string())
                .await?;
        }
        control
            .set_last_task_message(&target.session_id, args.message.trim().to_string())
            .await;
        Ok(core_ma::MessageAgentResult { ok: true })
    }

    pub(super) async fn wait_multi_agent(
        &self,
        control: Arc<MultiAgentControl>,
        args: core_ma::WaitAgentArgs,
    ) -> Result<core_ma::WaitAgentResult, ToolError> {
        let cfg = self.config_snapshot();
        let timeout_ms = args
            .timeout_ms
            .unwrap_or(cfg.multi_agent.default_wait_timeout_ms);
        if timeout_ms < cfg.multi_agent.min_wait_timeout_ms {
            return Err(ToolError::InvalidCommand(format!(
                "timeout_ms must be at least {}",
                cfg.multi_agent.min_wait_timeout_ms
            )));
        }
        if timeout_ms > cfg.multi_agent.max_wait_timeout_ms {
            return Err(ToolError::InvalidCommand(format!(
                "timeout_ms must be at most {}",
                cfg.multi_agent.max_wait_timeout_ms
            )));
        }
        Ok(control.wait_for_updates(&self.session_id, timeout_ms).await)
    }

    pub(super) async fn list_multi_agents(
        &self,
        control: Arc<MultiAgentControl>,
        args: core_ma::ListAgentsArgs,
    ) -> Result<core_ma::ListAgentsResult, ToolError> {
        let current = self.multi_agent_identity().await;
        let agents = control
            .list_agents(&current, args.path_prefix.as_deref())
            .await?;
        Ok(core_ma::ListAgentsResult { agents })
    }

    pub(super) async fn close_multi_agent(
        &self,
        control: Arc<MultiAgentControl>,
        args: core_ma::CloseAgentArgs,
    ) -> Result<core_ma::CloseAgentResult, ToolError> {
        let current = self.multi_agent_identity().await;
        let previous_status = control.close_agent(&current, &args.target).await?;
        Ok(core_ma::CloseAgentResult { previous_status })
    }

    pub(super) async fn close_for_multi_agent(&self) {
        {
            let mut session = self.session.lock().await;
            session.meta.closed = true;
            let _ = self.store.checkpoint(&mut session).await;
        }
        self.closing.store(true, Ordering::SeqCst);
        self.queue.lock().await.clear();
        self.status.lock().await.queue_len = 0;
        if let Some(tx) = self.active_cancel.lock().await.clone() {
            let _ = tx.send(true);
        }
        self.notify.notify_waiters();
    }

    async fn fork_messages(&self, fork_turns: Option<&str>) -> Result<Vec<CoreMessage>, ToolError> {
        let mode = parse_fork_turns(fork_turns)?;
        if mode == Some(0) {
            return Ok(Vec::new());
        }

        let session = self.session.lock().await;
        let fork_end = session
            .messages
            .iter()
            .rposition(|message| matches!(message, CoreMessage::User { hidden: false, .. }))
            .unwrap_or(session.messages.len());
        let mut filtered = session
            .messages
            .get(..fork_end)
            .unwrap_or(&session.messages)
            .iter()
            .filter_map(|message| match message {
                CoreMessage::User { content, hidden } if !hidden => Some(CoreMessage::User {
                    content: content.clone(),
                    hidden: false,
                }),
                CoreMessage::Assistant {
                    content: Some(content),
                    reasoning_content: _,
                    tool_calls,
                    usage: _,
                    provider_metadata: _,
                } if tool_calls.is_empty() && !content.trim().is_empty() => {
                    Some(CoreMessage::Assistant {
                        content: Some(content.clone()),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                        usage: None,
                        provider_metadata: None,
                    })
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        if let Some(turns) = mode {
            let mut seen_users = 0usize;
            let mut start = 0usize;
            for (idx, message) in filtered.iter().enumerate().rev() {
                if matches!(message, CoreMessage::User { .. }) {
                    seen_users += 1;
                    if seen_users == turns {
                        start = idx;
                        break;
                    }
                }
            }
            if seen_users < turns {
                start = 0;
            }
            filtered = filtered.split_off(start);
        }

        Ok(filtered)
    }

    async fn active_goal(&self) -> Option<SessionGoal> {
        self.session
            .lock()
            .await
            .meta
            .goal
            .clone()
            .filter(|g| g.status == SessionGoalStatus::Active)
    }

    async fn enqueue_goal_continuation(&self) -> Result<(), ApiError> {
        if self.closing.load(Ordering::SeqCst) || self.active_goal().await.is_none() {
            return Ok(());
        }
        let run = domain::Run {
            id: new_run_id(),
            session_id: self.session_id.to_string(),
            state: domain::RunState::Queued,
            created_at: now_rfc3339(),
            started_at: None,
            finished_at: None,
            finish_reason: None,
            error: None,
            input: domain::RunInput::GoalContinuation,
            overrides: None,
        };
        {
            let q = self.queue.lock().await;
            if q.iter()
                .any(|item| matches!(item.run.input, domain::RunInput::GoalContinuation))
            {
                return Ok(());
            }
        }
        write_run_file(&self.runs_dir, &run).await?;
        {
            let mut q = self.queue.lock().await;
            if q.iter()
                .any(|item| matches!(item.run.input, domain::RunInput::GoalContinuation))
            {
                return Ok(());
            }
            q.push_back(QueuedRun { run });
            self.status.lock().await.queue_len = q.len();
        }
        self.notify.notify_one();
        Ok(())
    }

    async fn worker_loop(self: Arc<Self>) {
        loop {
            if self.closing.load(Ordering::SeqCst) {
                let queue_empty = self.queue.lock().await.is_empty();
                let idle = self.status.lock().await.run_state == domain::SessionRunState::Idle;
                if queue_empty && idle {
                    break;
                }
            }

            let next = {
                let mut q = self.queue.lock().await;
                let item = q.pop_front();
                self.status.lock().await.queue_len = q.len();
                item
            };

            let Some(item) = next else {
                self.notify.notified().await;
                continue;
            };

            if let Err(err) = self.run_one(item.run).await {
                tracing::error!("run_one error: {err}");
            }

            // Apply deferred session settings once safe.
            if self.settings_dirty.load(Ordering::SeqCst)
                && self.status.lock().await.run_state == domain::SessionRunState::Idle
            {
                if let Err(err) = self.apply_settings_now(true).await {
                    tracing::error!("apply_settings_now error: {err}");
                } else {
                    self.settings_dirty.store(false, Ordering::SeqCst);
                }
            }
        }
    }

    async fn run_one(&self, mut run: domain::Run) -> Result<(), ApiError> {
        let span = tracing::info_span!(
            "kiliax.run",
            session_id = %self.session_id,
            run_id = %run.id,
            agent = tracing::field::Empty,
            model_id = tracing::field::Empty,
            workspace_root = tracing::field::Empty,
        );

        async {
            let config = self.config_snapshot();

            let (cancel_tx, mut cancel_rx) = watch::channel(false);
            *self.active_cancel.lock().await = Some(cancel_tx);
            let run_started_at = now_rfc3339();

            {
                let mut st = self.status.lock().await;
                st.run_state = domain::SessionRunState::Running;
                st.active_run_id = Some(run.id.clone());
                st.active_run_started_at = Some(run_started_at.clone());
                st.step = 0;
                st.active_tool = None;
                st.retry_status = None;
            }
            *self.stream_snapshot.lock().await = None;

            run.state = domain::RunState::Running;
            run.started_at = Some(run_started_at);
            write_run_file(&self.runs_dir, &run).await?;

            let (user_content, persist_user) = match &run.input {
                domain::RunInput::Text { text, attachments } => {
                    (run_text_content(text, attachments)?, true)
                }
                domain::RunInput::FromUserMessage { user_message_id } => {
                    let session = self.session.lock().await;
                    let by_id = session
                        .message_ids
                        .iter()
                        .position(|id| *id == *user_message_id)
                        .and_then(|idx| match &session.messages[idx] {
                            CoreMessage::User { content, hidden } if !hidden => {
                                Some(content.clone())
                            }
                            _ => None,
                        });
                    let last_user = session.messages.iter().rev().find_map(|m| match m {
                        CoreMessage::User { content, hidden } if !hidden => Some(content.clone()),
                        _ => None,
                    });
                    (
                        by_id
                            .or(last_user)
                            .unwrap_or_else(|| UserMessageContent::Text(String::new())),
                        false,
                    )
                }
                domain::RunInput::EditUserMessage { content, .. } => {
                    (UserMessageContent::Text(content.clone()), false)
                }
                domain::RunInput::RegenerateAfterUserMessage { .. } => {
                    let session = self.session.lock().await;
                    let last_user = session.messages.iter().rev().find_map(|m| match m {
                        CoreMessage::User { content, hidden } if !hidden => Some(content.clone()),
                        _ => None,
                    });
                    (
                        last_user.unwrap_or_else(|| UserMessageContent::Text(String::new())),
                        false,
                    )
                }
                domain::RunInput::GoalContinuation => {
                    let Some(goal) = self.active_goal().await else {
                        return Ok(());
                    };
                    (
                        UserMessageContent::Text(goal_continuation_prompt(&goal)),
                        true,
                    )
                }
            };
            let user_content = if let Some(control) =
                self.multi_agent_control.as_ref().and_then(|c| c.upgrade())
            {
                let mailbox = control.drain_messages_for_run(&self.session_id).await;
                append_text_to_user_content(user_content, render_mailbox_messages(&mailbox))
            } else {
                user_content
            };
            let hidden_user = matches!(run.input, domain::RunInput::GoalContinuation);
            let user_text = user_content_trace_text(&user_content);

            let base_settings = self.settings.lock().await.clone();
            let overrides = run.overrides.take();
            let effective =
                apply_run_overrides(&base_settings, overrides.as_ref(), config.as_ref())?;
            run.overrides = overrides;

            Span::current().record("agent", effective.agent.as_str());
            Span::current().record("model_id", effective.model_id.as_str());

            let mcp_enabled = effective.mcp.servers.iter().filter(|s| s.enable).count();
            tracing::info!(
                event = "run.started",
                session_id = %self.session_id,
                run_id = %run.id,
                agent = %effective.agent,
                model_id = %effective.model_id,
                workspace_root = %effective.workspace_root.display(),
                mcp_enabled = mcp_enabled as u64,
            );

            // Langfuse OTEL ingest expects trace-level attributes on spans.
            let current_span = Span::current();
            kiliax_core::telemetry::spans::set_attribute(
                &current_span,
                "langfuse.session.id",
                self.session_id.to_string(),
            );
            kiliax_core::telemetry::spans::set_attribute(
                &current_span,
                "langfuse.environment",
                config.otel.environment.clone(),
            );
            let trace_name = if kiliax_core::telemetry::capture_full() {
                user_text.chars().take(80).collect::<String>()
            } else {
                format!("{} {}", effective.agent, effective.model_id)
            };
            if !trace_name.trim().is_empty() {
                kiliax_core::telemetry::spans::set_attribute(
                    &current_span,
                    "langfuse.trace.name",
                    trace_name,
                );
            }
            if kiliax_core::telemetry::capture_full() {
                let captured = kiliax_core::telemetry::capture_text(&user_text);
                kiliax_core::telemetry::spans::set_attribute(
                    &current_span,
                    "langfuse.trace.input",
                    captured.as_str().to_string(),
                );
            }

            // Per-run tool config.
            let mut cfg_for_run =
                config_with_mcp_overrides(config.as_ref(), &effective.mcp.servers)?;
            cfg_for_run.custom_tools = custom_tools_config_from_settings(&effective.custom_tools);
            let workspace_root = effective.workspace_root.clone();
            Span::current().record(
                "workspace_root",
                tracing::field::display(workspace_root.display()),
            );
            tokio::fs::create_dir_all(&workspace_root)
                .await
                .map_err(ApiError::internal_error)?;

            let tools_for_run = {
                let tools = self.tools.lock().await;
                tools
                    .set_config(cfg_for_run)
                    .map_err(ApiError::internal_error)?;
                tools.clone()
            };

            let profile = AgentProfile::from_name(&effective.agent).ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    ApiErrorCode::AgentNotSupported,
                    "agent not supported",
                )
            })?;
            let mut options = AgentRuntimeOptions::from_config_for_model(
                &profile,
                config.as_ref(),
                Some(effective.model_id.as_str()),
            );
            if matches!(run.input, domain::RunInput::GoalContinuation) {
                options.retry_mode = kiliax_core::llm::LlmRetryMode::Goal;
            }
            options.cancel_rx = Some(cancel_rx.clone());
            let max_steps = options.max_steps;

            let llm =
                kiliax_core::llm::client_from_config(config.as_ref(), Some(&effective.model_id))
                    .map_err(|e| {
                        ApiError::new(
                            StatusCode::BAD_REQUEST,
                            ApiErrorCode::ModelNotSupported,
                            e.to_string(),
                        )
                    })?;
            let prompt_cache_key = {
                let session = self.session.lock().await;
                session
                    .meta
                    .prompt_cache_key
                    .clone()
                    .filter(|k| !k.trim().is_empty())
            };
            let llm = llm.with_prompt_cache_key(prompt_cache_key);

            if persist_user {
                if let Some(limit) = options.auto_compact_token_limit {
                    let estimated = {
                        let session = self.session.lock().await;
                        compact::estimate_context_tokens(&session.messages)
                    };
                    if estimated >= limit {
                        tracing::info!(
                            event = "run.auto_compact.triggered",
                            session_id = %self.session_id,
                            run_id = %run.id,
                            estimated_tokens = estimated as u64,
                            limit = limit as u64,
                        );
                        if let Err(err) = self
                            .compact_session_history(
                                &base_settings,
                                &effective,
                                &profile,
                                &tools_for_run,
                                &llm,
                            )
                            .await
                        {
                            tracing::warn!(
                                event = "run.auto_compact.failed",
                                session_id = %self.session_id,
                                run_id = %run.id,
                                error = %err,
                            );
                        }
                    }
                }

                // Persist user message at execution time (after optional pre-turn compaction).
                self.record_message(CoreMessage::User {
                    content: user_content.clone(),
                    hidden: hidden_user,
                })
                .await?;
            }

            let runtime = AgentRuntime::new(llm, tools_for_run.clone());

            let mut messages = { self.session.lock().await.messages.clone() };
            let goal_time_started = std::time::Instant::now();
            if effective.agent != base_settings.agent
                || effective.model_id != base_settings.model_id
                || effective.mcp.servers != base_settings.mcp.servers
                || effective.custom_tools.default_enable
                    != base_settings.custom_tools.default_enable
                || effective.custom_tools.overrides != base_settings.custom_tools.overrides
            {
                let skills_config = skills_config_from_settings(&base_settings.skills);
                let preamble = build_preamble(
                    &profile,
                    &effective.model_id,
                    &workspace_root,
                    {
                        let session = self.session.lock().await;
                        session.meta.project_prompt.clone()
                    },
                    &tools_for_run,
                    &skills_config,
                )
                .await;
                replace_preamble(&mut messages, preamble);
            }

            let stream = runtime
                .run_stream(&profile, messages, options)
                .await
                .map_err(ApiError::internal_error)?;
            tokio::pin!(stream);

            let mut finish_reason: Option<String> = None;
            let mut cancelled = false;
            let mut runtime_error: Option<AgentRuntimeError> = None;
            let mut runtime_diagnostics: Option<serde_json::Value> = None;

            loop {
                tokio::select! {
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            cancelled = true;
                            break;
                        }
                    }
                    maybe = stream.next() => {
                        let Some(item) = maybe else { break; };
                        match item {
                            Ok(ev) => {
                                if let Some(fr) = self.handle_agent_event(&run, ev).await? {
                                    finish_reason = Some(fr);
                                }
                            }
                            Err(err) => {
                                match err {
                                    AgentRuntimeError::Cancelled => cancelled = true,
                                    other => runtime_error = Some(other),
                                }
                                break;
                            }
                        }
                    }
                }
            }

            let _ = self
                .add_goal_usage_and_emit(goal_time_started.elapsed().as_secs(), 0)
                .await;

            let (step, active_tool) = {
                let st = self.status.lock().await;
                (st.step, st.active_tool.clone())
            };
            let trace_id = kiliax_core::telemetry::spans::current_trace_id();

            // Restore tool config to current session defaults (may have changed).
            let current_settings = self.settings.lock().await.clone();
            let mut cfg_for_tools =
                config_with_mcp_overrides(config.as_ref(), &current_settings.mcp.servers)?;
            cfg_for_tools.custom_tools =
                custom_tools_config_from_settings(&current_settings.custom_tools);
            {
                let tools = self.tools.lock().await;
                let _ = tools.set_config(cfg_for_tools);
            }

            {
                let mut st = self.status.lock().await;
                st.run_state = domain::SessionRunState::Idle;
                st.active_run_id = None;
                st.active_run_started_at = None;
                st.step = 0;
                st.active_tool = None;
                st.retry_status = None;
            }
            *self.active_cancel.lock().await = None;

            run.finished_at = Some(now_rfc3339());
            run.finish_reason = finish_reason.clone();

            if cancelled {
                run.state = domain::RunState::Cancelled;
            } else if let Some(err) = runtime_error {
                run.state = domain::RunState::Error;
                let code = runtime_error_code(&err).to_string();
                let hint = runtime_error_hint(&code, effective.agent.as_str());
                let mut meta_error = format_error_chain_text(&err);
                if let Some(tid) = trace_id.as_deref() {
                    meta_error.push_str("\ntrace_id: ");
                    meta_error.push_str(tid);
                }
                if let Some(hint) = hint.as_deref() {
                    meta_error.push_str("\nhint: ");
                    meta_error.push_str(hint);
                }

                run.error = Some(domain::RunError {
                    code: code.clone(),
                    message: meta_error.clone(),
                });
                {
                    let mut session = self.session.lock().await;
                    let _ = self.store.record_error(&mut session, meta_error).await;
                }
                runtime_diagnostics = Some(serde_json::json!({
                    "code": code,
                    "session_id": self.session_id.to_string(),
                    "run_id": run.id.clone(),
                    "agent": effective.agent,
                    "model_id": effective.model_id,
                    "step": step,
                    "active_tool": active_tool,
                    "max_steps": max_steps,
                    "trace_id": trace_id,
                    "hint": hint,
                    "error_chain": error_chain_vec(&err),
                }));
            } else {
                run.state = domain::RunState::Done;
                let persisted_finish_reason =
                    finish_reason.clone().or_else(|| Some("done".to_string()));
                run.finish_reason = persisted_finish_reason.clone();
                let mut session = self.session.lock().await;
                let _ = self
                    .store
                    .record_finish(&mut session, persisted_finish_reason)
                    .await;
            }

            tracing::info!(
                event = "run.finished",
                session_id = %self.session_id,
                run_id = %run.id,
                state = ?run.state,
                finish_reason = %run.finish_reason.as_deref().unwrap_or(""),
                error = %run.error.as_ref().map(|e| e.message.as_str()).unwrap_or(""),
            );

            write_run_file(&self.runs_dir, &run).await?;
            if let Some(control) = self.multi_agent_control.as_ref().and_then(|c| c.upgrade()) {
                let identity = self.multi_agent_identity().await;
                if identity.parent_session_id.is_some() {
                    let message = match run.state {
                        domain::RunState::Done => {
                            let session = self.session.lock().await;
                            last_assistant_text(&session.messages)
                                .unwrap_or_else(|| "Agent finished.".to_string())
                        }
                        domain::RunState::Error => run
                            .error
                            .as_ref()
                            .map(|err| err.message.clone())
                            .unwrap_or_else(|| "Agent errored.".to_string()),
                        domain::RunState::Cancelled => "Agent cancelled.".to_string(),
                        _ => String::new(),
                    };
                    if !message.is_empty() {
                        control
                            .notify_finished(
                                &identity,
                                format!("{:?}", run.state).to_lowercase(),
                                message,
                            )
                            .await;
                    }
                }
            }
            let should_continue_goal = run.state == domain::RunState::Done
                && self
                    .active_goal()
                    .await
                    .is_some_and(|g| g.status == SessionGoalStatus::Active);
            *self.stream_snapshot.lock().await = None;

            match run.state {
                domain::RunState::Done => {
                    let run_id = run.id.clone();
                    self.emit_event(domain::Event {
                        event_id: self.alloc_event_id(),
                        ts: now_rfc3339(),
                        session_id: self.session_id.to_string(),
                        run_id: Some(run_id),
                        event_type: "run_done".to_string(),
                        data: serde_json::json!({ "run": run }),
                    })
                    .await?;
                    if should_continue_goal {
                        self.enqueue_goal_continuation().await?;
                    }
                }
                domain::RunState::Cancelled => {
                    self.emit_event(domain::Event {
                        event_id: self.alloc_event_id(),
                        ts: now_rfc3339(),
                        session_id: self.session_id.to_string(),
                        run_id: Some(run.id.clone()),
                        event_type: "run_cancelled".to_string(),
                        data: serde_json::json!({ "reason": "cancelled" }),
                    })
                    .await?;
                }
                domain::RunState::Error => {
                    let run_id = run.id.clone();
                    self.emit_event(domain::Event {
                        event_id: self.alloc_event_id(),
                        ts: now_rfc3339(),
                        session_id: self.session_id.to_string(),
                        run_id: Some(run_id),
                        event_type: "run_error".to_string(),
                        data: serde_json::json!({ "run": run, "diagnostics": runtime_diagnostics }),
                    })
                    .await?;
                }
                _ => {}
            }

            Ok(())
        }
        .instrument(span)
        .await
    }

    async fn apply_settings_now(&self, emit_event: bool) -> Result<(), ApiError> {
        let config = self.config_snapshot();
        let settings = self.settings.lock().await.clone();

        if settings.workspace_root.as_os_str().is_empty() {
            return Err(ApiError::invalid_argument(
                "workspace_root must not be empty",
            ));
        }
        let workspace_root = settings.workspace_root.clone();
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(ApiError::internal_error)?;

        let extra_workspace_roots = settings.extra_workspace_roots.clone();

        let cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
        let tools = {
            let mut tools = self.tools.lock().await;
            if tools.workspace_root() != workspace_root.as_path() {
                let next = ToolEngine::new(&workspace_root, cfg_for_tools);
                next.set_extra_workspace_roots(extra_workspace_roots)
                    .map_err(ApiError::internal_error)?;
                self.install_tool_backends(&next).await?;
                *tools = next;
            } else {
                tools
                    .set_config(cfg_for_tools)
                    .map_err(ApiError::internal_error)?;
                tools
                    .set_extra_workspace_roots(extra_workspace_roots)
                    .map_err(ApiError::internal_error)?;
                self.install_tool_backends(&tools).await?;
            }
            tools.clone()
        };

        let profile = AgentProfile::from_name(&settings.agent).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ApiErrorCode::AgentNotSupported,
                "agent not supported",
            )
        })?;

        let skills_config = skills_config_from_settings(&settings.skills);
        let preamble = build_preamble(
            &profile,
            &settings.model_id,
            &workspace_root,
            {
                let session = self.session.lock().await;
                session.meta.project_prompt.clone()
            },
            &tools,
            &skills_config,
        )
        .await;

        let mut session = self.session.lock().await;
        let mut last_seq = session.meta.last_seq;
        let mut messages = std::mem::take(&mut session.messages);
        let mut message_ids = std::mem::take(&mut session.message_ids);
        replace_preamble_with_ids(&mut messages, &mut message_ids, &mut last_seq, preamble);
        session.messages = messages;
        session.message_ids = message_ids;
        session.meta.last_seq = last_seq;
        session.meta.agent = profile.name.to_string();
        session.meta.model_id = Some(settings.model_id.clone());
        session.meta.workspace_root = Some(settings.workspace_root.display().to_string());
        session.meta.extra_workspace_roots = settings
            .extra_workspace_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        session.meta.mcp_servers = settings
            .mcp
            .servers
            .iter()
            .map(|s| SessionMcpServerSetting {
                id: s.id.clone(),
                enable: s.enable,
            })
            .collect();
        session.meta.skills = Some(skills_config_from_settings(&settings.skills));
        session.meta.custom_tools = Some(custom_tools_config_from_settings(&settings.custom_tools));
        self.store
            .checkpoint(&mut session)
            .await
            .map_err(map_session_err)?;

        if emit_event {
            self.emit_event(domain::Event {
                event_id: self.alloc_event_id(),
                ts: now_rfc3339(),
                session_id: self.session_id.to_string(),
                run_id: None,
                event_type: "session_settings_changed".to_string(),
                data: serde_json::json!({ "settings": settings }),
            })
            .await?;
        }

        Ok(())
    }

    async fn record_message(&self, message: CoreMessage) -> Result<u64, ApiError> {
        let mut session = self.session.lock().await;
        self.store
            .record_message(&mut session, message)
            .await
            .map_err(map_session_err)?;
        Ok(session.meta.last_seq)
    }

    async fn handle_agent_event(
        &self,
        run: &domain::Run,
        ev: AgentEvent,
    ) -> Result<Option<String>, ApiError> {
        match ev {
            AgentEvent::StepStart { step } => {
                {
                    let mut st = self.status.lock().await;
                    st.run_state = domain::SessionRunState::Running;
                    st.step = step as u32;
                    st.retry_status = None;
                }
                let event_id = self.alloc_event_id();
                let ts = now_rfc3339();
                self.ensure_stream_snapshot(run, event_id).await;
                self.emit_event(domain::Event {
                    event_id,
                    ts,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "step_start".to_string(),
                    data: serde_json::json!({ "step": step }),
                })
                .await?;
            }
            AgentEvent::LlmRetry(retry) => {
                let event_id = self.alloc_event_id();
                let ts = now_rfc3339();
                let retry_status = domain::RetryStatus {
                    kind: retry.kind.as_str().to_string(),
                    attempt: retry.attempt,
                    max_attempts: retry.max_attempts,
                    next_attempt_at: rfc3339_after_ms(retry.delay_ms),
                    delay_ms: retry.delay_ms,
                    message: retry.message,
                    trace_id: kiliax_core::telemetry::spans::current_trace_id(),
                };
                {
                    let mut st = self.status.lock().await;
                    st.run_state = domain::SessionRunState::Retrying;
                    st.active_tool = None;
                    st.retry_status = Some(retry_status.clone());
                }
                self.emit_ephemeral_event(domain::Event {
                    event_id,
                    ts,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "run_retry".to_string(),
                    data: serde_json::json!({ "retry_status": retry_status }),
                })
                .await?;
            }
            AgentEvent::StepEnd { step } => {
                let event_id = self.alloc_event_id();
                let ts = now_rfc3339();
                self.bump_stream_snapshot(run, event_id).await;
                self.emit_event(domain::Event {
                    event_id,
                    ts,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "step_end".to_string(),
                    data: serde_json::json!({ "step": step }),
                })
                .await?;
            }
            AgentEvent::AssistantThinkingDelta { delta } => {
                {
                    let mut st = self.status.lock().await;
                    st.run_state = domain::SessionRunState::Running;
                    st.retry_status = None;
                }
                let event_id = self.alloc_event_id();
                let ts = now_rfc3339();
                self.apply_thinking_delta(run, event_id, ts.clone(), &delta)
                    .await;
                self.emit_ephemeral_event(domain::Event {
                    event_id,
                    ts,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "assistant_thinking_delta".to_string(),
                    data: serde_json::json!({ "delta": delta }),
                })
                .await?;
            }
            AgentEvent::AssistantDelta { delta } => {
                {
                    let mut st = self.status.lock().await;
                    st.run_state = domain::SessionRunState::Running;
                    st.retry_status = None;
                }
                let event_id = self.alloc_event_id();
                let ts = now_rfc3339();
                self.apply_assistant_delta(run, event_id, ts.clone(), &delta)
                    .await;
                self.emit_ephemeral_event(domain::Event {
                    event_id,
                    ts,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "assistant_delta".to_string(),
                    data: serde_json::json!({ "delta": delta }),
                })
                .await?;
            }
            AgentEvent::AssistantMessage { message } => {
                {
                    let mut st = self.status.lock().await;
                    st.run_state = domain::SessionRunState::Running;
                    st.retry_status = None;
                }
                let tokens_used = match &message {
                    CoreMessage::Assistant {
                        usage: Some(usage), ..
                    } => usage.completion_tokens as u64,
                    _ => 0,
                };
                let seq = self.record_message(message.clone()).await?;
                let created_at = now_rfc3339();
                let msg =
                    map_core_message_to_domain_event_message(seq, created_at.clone(), message)
                        .unwrap_or(domain::Message::Assistant {
                            id: seq.to_string(),
                            created_at: created_at.clone(),
                            content: String::new(),
                            reasoning_content: None,
                            tool_calls: Vec::new(),
                            usage: None,
                        });
                self.emit_event(domain::Event {
                    event_id: self.alloc_event_id(),
                    ts: created_at,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "assistant_message".to_string(),
                    data: serde_json::json!({ "message": msg }),
                })
                .await?;
                *self.stream_snapshot.lock().await = None;
                let _ = self.add_goal_usage_and_emit(0, tokens_used).await;
            }
            AgentEvent::ToolCall { call } => {
                {
                    let mut st = self.status.lock().await;
                    st.run_state = domain::SessionRunState::Tooling;
                    st.active_tool = Some(call.name.clone());
                    st.retry_status = None;
                }
                let event_id = self.alloc_event_id();
                let ts = now_rfc3339();
                self.apply_tool_call(
                    run,
                    event_id,
                    ts.clone(),
                    &call.id,
                    &call.name,
                    &call.arguments,
                )
                .await;
                self.emit_event(domain::Event {
                    event_id,
                    ts,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "tool_call".to_string(),
                    data: serde_json::json!({ "call": { "id": call.id, "name": call.name, "arguments": call.arguments } }),
                })
                .await?;
            }
            AgentEvent::ToolResult { message } => {
                let seq = self.record_message(message.clone()).await?;
                {
                    let mut st = self.status.lock().await;
                    st.run_state = domain::SessionRunState::Running;
                    st.active_tool = None;
                    st.retry_status = None;
                }
                let created_at = now_rfc3339();
                let msg =
                    map_core_message_to_domain_event_message(seq, created_at.clone(), message)
                        .unwrap_or(domain::Message::Tool {
                            id: seq.to_string(),
                            created_at: created_at.clone(),
                            tool_call_id: "".to_string(),
                            content: "".to_string(),
                        });
                let event_id = self.alloc_event_id();
                self.finish_tool_call(run, event_id, &msg).await;
                self.emit_event(domain::Event {
                    event_id,
                    ts: created_at,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "tool_result".to_string(),
                    data: serde_json::json!({ "message": msg }),
                })
                .await?;
            }
            AgentEvent::Done(out) => {
                return Ok(out.finish_reason.map(|r| format!("{r:?}")));
            }
        }
        Ok(None)
    }

    fn alloc_event_id(&self) -> u64 {
        self.next_event_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn emit_event(&self, event: domain::Event) -> Result<(), ApiError> {
        self.emit_event_with_persistence(event, EventPersistence::Durable)
            .await
    }

    async fn emit_ephemeral_event(&self, event: domain::Event) -> Result<(), ApiError> {
        self.emit_event_with_persistence(event, EventPersistence::Ephemeral)
            .await
    }

    async fn emit_event_with_persistence(
        &self,
        event: domain::Event,
        persistence: EventPersistence,
    ) -> Result<(), ApiError> {
        if persistence == EventPersistence::Durable {
            append_event(&self.events_api_path, &event).await?;
        }
        let ring_size = self.events_ring_size.load(Ordering::SeqCst);
        if ring_size > 0 {
            let mut ring = self.events_ring.lock().await;
            ring.push_back(event.clone());
            while ring.len() > ring_size {
                ring.pop_front();
            }
        }
        {
            let mut st = self.status.lock().await;
            st.last_event_id = st.last_event_id.max(event.event_id);
        }
        let _ = self.events_tx.send(event);
        Ok(())
    }

    async fn ensure_stream_snapshot(&self, run: &domain::Run, last_event_id: u64) {
        let mut snapshot = self.stream_snapshot.lock().await;
        match snapshot.as_mut() {
            Some(existing) if existing.run_id == run.id => {
                existing.last_event_id = existing.last_event_id.max(last_event_id);
            }
            _ => {
                *snapshot = Some(domain::StreamSnapshot {
                    run_id: run.id.clone(),
                    last_event_id,
                    thinking: String::new(),
                    assistant: String::new(),
                    assistant_started: false,
                    tool_calls: Vec::new(),
                    thinking_started_at: None,
                    assistant_started_at: None,
                    tool_started_at: BTreeMap::new(),
                });
            }
        }
    }

    async fn bump_stream_snapshot(&self, run: &domain::Run, last_event_id: u64) {
        let mut snapshot = self.stream_snapshot.lock().await;
        if let Some(snapshot) = snapshot.as_mut().filter(|s| s.run_id == run.id) {
            snapshot.last_event_id = snapshot.last_event_id.max(last_event_id);
        }
    }

    async fn finish_tool_call(
        &self,
        run: &domain::Run,
        last_event_id: u64,
        message: &domain::Message,
    ) {
        let mut snapshot = self.stream_snapshot.lock().await;
        if let Some(snapshot) = snapshot.as_mut().filter(|s| s.run_id == run.id) {
            snapshot.last_event_id = snapshot.last_event_id.max(last_event_id);
            if let domain::Message::Tool { tool_call_id, .. } = message {
                snapshot.tool_calls.retain(|call| call.id != *tool_call_id);
                snapshot.tool_started_at.remove(tool_call_id);
            }
        }
    }

    async fn apply_thinking_delta(
        &self,
        run: &domain::Run,
        event_id: u64,
        ts: String,
        delta: &str,
    ) {
        self.ensure_stream_snapshot(run, event_id).await;
        let mut snapshot = self.stream_snapshot.lock().await;
        if let Some(snapshot) = snapshot.as_mut().filter(|s| s.run_id == run.id) {
            snapshot.last_event_id = snapshot.last_event_id.max(event_id);
            if !snapshot.assistant_started {
                if snapshot.thinking_started_at.is_none() {
                    snapshot.thinking_started_at = Some(ts);
                }
                snapshot.thinking.push_str(delta);
            }
        }
    }

    async fn apply_assistant_delta(
        &self,
        run: &domain::Run,
        event_id: u64,
        ts: String,
        delta: &str,
    ) {
        self.ensure_stream_snapshot(run, event_id).await;
        let mut snapshot = self.stream_snapshot.lock().await;
        if let Some(snapshot) = snapshot.as_mut().filter(|s| s.run_id == run.id) {
            snapshot.last_event_id = snapshot.last_event_id.max(event_id);
            snapshot.assistant_started = true;
            if snapshot.assistant_started_at.is_none() {
                snapshot.assistant_started_at = Some(ts);
            }
            snapshot.assistant.push_str(delta);
        }
    }

    async fn apply_tool_call(
        &self,
        run: &domain::Run,
        event_id: u64,
        ts: String,
        id: &str,
        name: &str,
        arguments: &str,
    ) {
        self.ensure_stream_snapshot(run, event_id).await;
        let mut snapshot = self.stream_snapshot.lock().await;
        if let Some(snapshot) = snapshot.as_mut().filter(|s| s.run_id == run.id) {
            snapshot.last_event_id = snapshot.last_event_id.max(event_id);
            snapshot.tool_calls.push(domain::StreamToolCallSnapshot {
                id: id.to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
            });
            snapshot.tool_started_at.entry(id.to_string()).or_insert(ts);
        }
    }
}

#[async_trait]
impl kiliax_core::tools::builtin::GoalBackend for LiveSession {
    async fn get_goal(&self) -> Result<Option<SessionGoal>, kiliax_core::tools::ToolError> {
        Ok(self.session.lock().await.meta.goal.clone())
    }

    async fn complete_goal(&self) -> Result<Option<SessionGoal>, kiliax_core::tools::ToolError> {
        let mut session = self.session.lock().await;
        self.store
            .complete_goal(&mut session)
            .await
            .map_err(|e| kiliax_core::tools::ToolError::Io(std::io::Error::other(e)))
    }
}

fn apply_run_overrides(
    base: &domain::SessionSettings,
    overrides: Option<&domain::RunOverrides>,
    config: &Config,
) -> Result<domain::SessionSettings, ApiError> {
    let mut out = base.clone();
    if let Some(o) = overrides {
        if let Some(agent) = o.agent.as_deref() {
            let profile = AgentProfile::from_name(agent).ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    ApiErrorCode::AgentNotSupported,
                    "agent not supported",
                )
            })?;
            out.agent = profile.name.to_string();
        }
        if let Some(model_id) = o.model_id.as_deref() {
            config.resolve_model(model_id).map_err(|e| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    ApiErrorCode::ModelNotSupported,
                    e.to_string(),
                )
            })?;
            out.model_id = model_id.to_string();
        }
        if let Some(mcp) = o.mcp.as_ref().and_then(|m| m.servers.as_ref()) {
            merge_mcp_settings(&mut out.mcp.servers, mcp, config, false)?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiliax_core::config::{ModelConfig, ProviderApi, ProviderConfig};
    use kiliax_core::protocol::{Message as CoreMessage, TokenUsage, ToolCall, UserMessageContent};
    use tempfile::TempDir;

    fn test_config() -> Config {
        let mut cfg = Config {
            default_model: Some("test/test-model".to_string()),
            ..Default::default()
        };
        cfg.providers.insert(
            "test".to_string(),
            ProviderConfig {
                api: ProviderApi::OpenAiChatCompletions,
                base_url: "http://127.0.0.1:1".to_string(),
                api_key: None,
                models: vec![ModelConfig::new("test-model")],
            },
        );
        cfg
    }

    #[tokio::test]
    async fn streaming_events_stay_in_memory_until_message_is_finalized() {
        let dir = TempDir::new().expect("tempdir");
        let workspace_root = dir.path().to_path_buf();
        let server = ServerState::new_for_tests(
            workspace_root.clone(),
            workspace_root.join("kiliax.yaml"),
            test_config(),
            None,
        )
        .await
        .expect("server");

        let session = server
            .store
            .create(
                "general",
                Some("test/test-model".to_string()),
                None,
                Some(workspace_root.display().to_string()),
                Vec::new(),
                vec![CoreMessage::User {
                    content: UserMessageContent::Text("hello".to_string()),
                    hidden: false,
                }],
            )
            .await
            .expect("session");
        let settings = default_settings(server.config_snapshot().as_ref(), Some(&session.meta))
            .expect("settings");
        let tools = ToolEngine::new(
            &workspace_root,
            config_with_mcp_overrides(server.config_snapshot().as_ref(), &settings.mcp.servers)
                .expect("tool config"),
        );
        let live = LiveSession::from_state(&server, session, settings, tools, false)
            .await
            .expect("live session");

        live.emit_ephemeral_event(domain::Event {
            event_id: live.alloc_event_id(),
            ts: now_rfc3339(),
            session_id: live.session_id.to_string(),
            run_id: None,
            event_type: "assistant_delta".to_string(),
            data: serde_json::json!({ "delta": "Hello" }),
        })
        .await
        .expect("ephemeral event");

        let path = session_events_api_path(&server.store, live.id());
        assert!(
            tokio::fs::metadata(&path).await.is_err(),
            "ephemeral events should not be persisted"
        );

        let backlog = live.backlog_after(0, 10).await.expect("ring backlog");
        assert_eq!(backlog.len(), 1);
        assert_eq!(backlog[0].event_type, "assistant_delta");

        live.emit_event(domain::Event {
            event_id: live.alloc_event_id(),
            ts: now_rfc3339(),
            session_id: live.session_id.to_string(),
            run_id: None,
            event_type: "assistant_message".to_string(),
            data: serde_json::json!({ "message": { "id": "1", "role": "assistant", "content": "Hello" } }),
        })
        .await
        .expect("durable event");

        let logged = read_events_after(&path, 0, 10)
            .await
            .expect("logged events");
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].event_type, "assistant_message");

        let backlog = live.backlog_after(0, 10).await.expect("ring backlog");
        assert_eq!(backlog.len(), 2);
        assert_eq!(backlog[0].event_type, "assistant_delta");
        assert_eq!(backlog[1].event_type, "assistant_message");
    }

    #[tokio::test]
    async fn live_session_stream_snapshot_restores_partial_output_and_clears_on_final_message() {
        let dir = TempDir::new().expect("tempdir");
        let workspace_root = dir.path().to_path_buf();
        let server = ServerState::new_for_tests(
            workspace_root.clone(),
            workspace_root.join("kiliax.yaml"),
            test_config(),
            None,
        )
        .await
        .expect("server");

        let session = server
            .store
            .create(
                "general",
                Some("test/test-model".to_string()),
                None,
                Some(workspace_root.display().to_string()),
                Vec::new(),
                vec![CoreMessage::User {
                    content: UserMessageContent::Text("hello".to_string()),
                    hidden: false,
                }],
            )
            .await
            .expect("session");
        let settings = default_settings(server.config_snapshot().as_ref(), Some(&session.meta))
            .expect("settings");
        let tools = ToolEngine::new(
            &workspace_root,
            config_with_mcp_overrides(server.config_snapshot().as_ref(), &settings.mcp.servers)
                .expect("tool config"),
        );
        let live = LiveSession::from_state(&server, session, settings, tools, false)
            .await
            .expect("live session");
        let run = domain::Run {
            id: "run-1".to_string(),
            session_id: live.id().to_string(),
            state: domain::RunState::Running,
            created_at: now_rfc3339(),
            started_at: Some(now_rfc3339()),
            finished_at: None,
            finish_reason: None,
            error: None,
            input: domain::RunInput::Text {
                text: "hello".to_string(),
                attachments: Vec::new(),
            },
            overrides: None,
        };

        live.handle_agent_event(
            &run,
            AgentEvent::AssistantThinkingDelta {
                delta: "thinking".to_string(),
            },
        )
        .await
        .expect("thinking delta");
        live.handle_agent_event(
            &run,
            AgentEvent::AssistantDelta {
                delta: "Hello".to_string(),
            },
        )
        .await
        .expect("assistant delta");
        live.handle_agent_event(
            &run,
            AgentEvent::ToolCall {
                call: ToolCall {
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    arguments: "{\"filePath\":\"AGENTS.md\"}".to_string(),
                },
            },
        )
        .await
        .expect("tool call");

        let snapshot = live.snapshot().await.expect("snapshot").stream.unwrap();
        assert_eq!(snapshot.run_id, "run-1");
        assert_eq!(snapshot.thinking, "thinking");
        assert_eq!(snapshot.assistant, "Hello");
        assert!(snapshot.assistant_started);
        assert_eq!(snapshot.tool_calls.len(), 1);
        assert_eq!(snapshot.tool_calls[0].name, "read_file");
        assert!(snapshot.thinking_started_at.is_some());
        assert!(snapshot.assistant_started_at.is_some());
        assert!(snapshot.tool_started_at.contains_key("call-1"));

        live.handle_agent_event(
            &run,
            AgentEvent::ToolResult {
                message: CoreMessage::Tool {
                    tool_call_id: "call-1".to_string(),
                    content: "done".to_string(),
                },
            },
        )
        .await
        .expect("tool result");

        let snapshot = live.snapshot().await.expect("snapshot").stream.unwrap();
        assert!(snapshot.tool_calls.is_empty());
        assert!(!snapshot.tool_started_at.contains_key("call-1"));

        live.handle_agent_event(
            &run,
            AgentEvent::AssistantMessage {
                message: CoreMessage::Assistant {
                    content: Some("Hello".to_string()),
                    reasoning_content: Some("thinking".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    provider_metadata: None,
                },
            },
        )
        .await
        .expect("assistant message");

        assert!(live.snapshot().await.expect("snapshot").stream.is_none());
    }

    #[tokio::test]
    async fn goal_tokens_count_only_assistant_output_tokens() {
        let dir = TempDir::new().expect("tempdir");
        let workspace_root = dir.path().to_path_buf();
        let server = ServerState::new_for_tests(
            workspace_root.clone(),
            workspace_root.join("kiliax.yaml"),
            test_config(),
            None,
        )
        .await
        .expect("server");

        let mut session = server
            .store
            .create(
                "general",
                Some("test/test-model".to_string()),
                None,
                Some(workspace_root.display().to_string()),
                Vec::new(),
                vec![CoreMessage::User {
                    content: UserMessageContent::Text("hello".to_string()),
                    hidden: false,
                }],
            )
            .await
            .expect("session");
        server
            .store
            .set_goal(&mut session, "ship output token accounting")
            .await
            .expect("goal");
        let settings = default_settings(server.config_snapshot().as_ref(), Some(&session.meta))
            .expect("settings");
        let tools = ToolEngine::new(
            &workspace_root,
            config_with_mcp_overrides(server.config_snapshot().as_ref(), &settings.mcp.servers)
                .expect("tool config"),
        );
        let live = LiveSession::from_state(&server, session, settings, tools, false)
            .await
            .expect("live session");
        let run = domain::Run {
            id: "run-usage".to_string(),
            session_id: live.id().to_string(),
            state: domain::RunState::Running,
            created_at: now_rfc3339(),
            started_at: Some(now_rfc3339()),
            finished_at: None,
            finish_reason: None,
            error: None,
            input: domain::RunInput::Text {
                text: "hello".to_string(),
                attachments: Vec::new(),
            },
            overrides: None,
        };

        live.handle_agent_event(
            &run,
            AgentEvent::AssistantMessage {
                message: CoreMessage::Assistant {
                    content: Some("Hello".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: Some(TokenUsage {
                        prompt_tokens: 100,
                        completion_tokens: 7,
                        total_tokens: 107,
                        cached_tokens: None,
                    }),
                    provider_metadata: None,
                },
            },
        )
        .await
        .expect("assistant message");

        assert_eq!(live.goal().await.unwrap().tokens_used, 7);
    }

    #[tokio::test]
    async fn master_spawns_lists_messages_and_closes_child_agent() {
        let dir = TempDir::new().expect("tempdir");
        let workspace_root = dir.path().to_path_buf();
        let server = ServerState::new_for_tests(
            workspace_root.clone(),
            workspace_root.join("kiliax.yaml"),
            test_config(),
            None,
        )
        .await
        .expect("server");

        let session = server
            .store
            .create(
                "master",
                Some("test/test-model".to_string()),
                None,
                Some(workspace_root.display().to_string()),
                Vec::new(),
                vec![
                    CoreMessage::User {
                        content: UserMessageContent::Text(
                            "prior context: keep weather replies concise".to_string(),
                        ),
                        hidden: false,
                    },
                    CoreMessage::Assistant {
                        content: Some("ack".to_string()),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                        usage: None,
                        provider_metadata: None,
                    },
                    CoreMessage::User {
                        content: UserMessageContent::Text("coordinate".to_string()),
                        hidden: false,
                    },
                ],
            )
            .await
            .expect("session");
        let mut settings = default_settings(server.config_snapshot().as_ref(), Some(&session.meta))
            .expect("settings");
        settings.agent = "master".to_string();
        let tools = ToolEngine::new(
            &workspace_root,
            config_with_mcp_overrides(server.config_snapshot().as_ref(), &settings.mcp.servers)
                .expect("tool config"),
        );
        let live = LiveSession::from_state(&server, session, settings, tools, false)
            .await
            .expect("live session");

        let spawned = live
            .spawn_multi_agent(
                server.multi_agent_control.clone(),
                core_ma::SpawnAgentArgs {
                    task_name: "worker".to_string(),
                    message: "inspect the repo".to_string(),
                    agent_type: Some("plan".to_string()),
                    model_id: None,
                    fork_turns: Some("none".to_string()),
                },
            )
            .await
            .expect("spawn");
        assert_eq!(spawned.task_name, "/root/worker");

        let duplicate = live
            .spawn_multi_agent(
                server.multi_agent_control.clone(),
                core_ma::SpawnAgentArgs {
                    task_name: "worker".to_string(),
                    message: "again".to_string(),
                    agent_type: None,
                    model_id: None,
                    fork_turns: Some("none".to_string()),
                },
            )
            .await
            .expect_err("duplicate path should fail");
        assert!(duplicate.to_string().contains("already exists"));

        let second_session = server
            .store
            .create(
                "master",
                Some("test/test-model".to_string()),
                None,
                Some(workspace_root.display().to_string()),
                Vec::new(),
                vec![CoreMessage::User {
                    content: UserMessageContent::Text("coordinate elsewhere".to_string()),
                    hidden: false,
                }],
            )
            .await
            .expect("second session");
        let mut second_settings = default_settings(
            server.config_snapshot().as_ref(),
            Some(&second_session.meta),
        )
        .expect("second settings");
        second_settings.agent = "master".to_string();
        let second_tools = ToolEngine::new(
            &workspace_root,
            config_with_mcp_overrides(
                server.config_snapshot().as_ref(),
                &second_settings.mcp.servers,
            )
            .expect("second tool config"),
        );
        let second_live = LiveSession::from_state(
            &server,
            second_session,
            second_settings,
            second_tools,
            false,
        )
        .await
        .expect("second live session");
        let second_spawned = second_live
            .spawn_multi_agent(
                server.multi_agent_control.clone(),
                core_ma::SpawnAgentArgs {
                    task_name: "worker".to_string(),
                    message: "same path under another root".to_string(),
                    agent_type: Some("plan".to_string()),
                    model_id: None,
                    fork_turns: Some("none".to_string()),
                },
            )
            .await
            .expect("same path in another root");
        assert_eq!(second_spawned.task_name, "/root/worker");

        let forked = live
            .spawn_multi_agent(
                server.multi_agent_control.clone(),
                core_ma::SpawnAgentArgs {
                    task_name: "forked".to_string(),
                    message: "check child context".to_string(),
                    agent_type: Some("plan".to_string()),
                    model_id: None,
                    fork_turns: Some("all".to_string()),
                },
            )
            .await
            .expect("forked spawn");
        let forked_id = SessionId::parse(&forked.session_id).expect("forked session id");
        let forked_live = server
            .multi_agent_control
            .live_for_session(&forked_id)
            .await
            .expect("forked live");
        let queued_prompt = {
            let queue = forked_live.queue.lock().await;
            let Some(QueuedRun { run }) = queue.front() else {
                panic!("forked child should have queued initial run");
            };
            match &run.input {
                domain::RunInput::Text { text, .. } => text.clone(),
                other => panic!("unexpected queued input: {other:?}"),
            }
        };
        assert_eq!(queued_prompt, "check child context");

        let forked_state = server.store.load(&forked_id).await.expect("forked state");
        let forked_text = forked_state
            .messages
            .iter()
            .filter_map(|message| match message {
                CoreMessage::User {
                    content: UserMessageContent::Text(text),
                    ..
                } => Some(text.as_str()),
                CoreMessage::Assistant { content, .. } => content.as_deref(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(forked_text.contains("prior context: keep weather replies concise"));
        assert!(forked_text.contains("ack"));
        assert!(!forked_text.contains("coordinate"));

        let listed = live
            .list_multi_agents(
                server.multi_agent_control.clone(),
                core_ma::ListAgentsArgs { path_prefix: None },
            )
            .await
            .expect("list");
        assert!(listed
            .agents
            .iter()
            .any(|agent| agent.agent_name == "/root/worker"));

        live.send_multi_agent_message(
            server.multi_agent_control.clone(),
            core_ma::MessageAgentArgs {
                target: "/root/worker".to_string(),
                message: "queued note".to_string(),
            },
            false,
        )
        .await
        .expect("send message");
        let child_id = SessionId::parse(&spawned.session_id).expect("child session id");
        let child_live = server
            .multi_agent_control
            .live_for_session(&child_id)
            .await
            .expect("child live");
        let waited = child_live
            .wait_multi_agent(
                server.multi_agent_control.clone(),
                core_ma::WaitAgentArgs {
                    timeout_ms: Some(1_000),
                },
            )
            .await
            .expect("wait");
        assert!(!waited.timed_out);
        assert_eq!(waited.updates[0].message, "queued note");

        let closed = live
            .close_multi_agent(
                server.multi_agent_control.clone(),
                core_ma::CloseAgentArgs {
                    target: "worker".to_string(),
                },
            )
            .await
            .expect("close");
        assert!(!closed.previous_status.is_empty());
        let listed = live
            .list_multi_agents(
                server.multi_agent_control.clone(),
                core_ma::ListAgentsArgs { path_prefix: None },
            )
            .await
            .expect("list after close");
        assert!(!listed
            .agents
            .iter()
            .any(|agent| agent.agent_name == "/root/worker"));
    }
}
