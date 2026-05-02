use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::http::StatusCode;
use kiliax_core::agents::AgentProfile;
use kiliax_core::compact;
use kiliax_core::config::Config;
use kiliax_core::protocol::{Message as CoreMessage, UserMessageContent};
use kiliax_core::runtime::{AgentEvent, AgentRuntime, AgentRuntimeError, AgentRuntimeOptions};
use kiliax_core::session::{FileSessionStore, SessionId, SessionMcpServerSetting, SessionState};
use kiliax_core::tools::ToolEngine;
use tokio::sync::{broadcast, watch, Mutex, Notify};
use tokio_stream::StreamExt;
use tracing::{Instrument, Span};

use crate::error::{ApiError, ApiErrorCode};
use crate::infra::{validate_client_extra_workspace_roots, validate_client_workspace_root};

use super::preamble::{build_preamble, insert_preamble_updates_before_last_user, preamble_updates};
use super::{
    append_event, apply_settings_patch, config_with_mcp_overrides, error_chain_vec,
    format_error_chain_text, map_core_message_to_domain_event_message, map_mcp_status,
    map_session_err, merge_mcp_settings, new_run_id, now_rfc3339, read_last_event_id,
    resolve_session_settings, runtime_error_code, runtime_error_hint, session_events_api_path,
    skills_config_from_settings, ts_ms_to_rfc3339, write_run_file, ServerState,
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

    status: Mutex<domain::SessionStatus>,
    queue: Mutex<VecDeque<QueuedRun>>,
    notify: Notify,
    active_cancel: Mutex<Option<watch::Sender<bool>>>,

    events_api_path: PathBuf,
    events_tx: broadcast::Sender<domain::Event>,
    events_ring: Mutex<VecDeque<domain::Event>>,
    events_ring_size: AtomicUsize,
    next_event_id: AtomicU64,
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

impl LiveSession {
    fn config_snapshot(&self) -> Arc<Config> {
        self.config.load_full()
    }

    pub(super) async fn settings_snapshot(&self) -> domain::SessionSettings {
        self.settings.lock().await.clone()
    }

    pub(super) async fn workspace_root(&self) -> PathBuf {
        self.settings.lock().await.workspace_root.clone()
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

        let cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
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
            status: Mutex::new(domain::SessionStatus {
                run_state: domain::SessionRunState::Idle,
                active_run_id: None,
                step: 0,
                active_tool: None,
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
        });

        // Ensure meta reflects current defaults for compatibility with TUI resume.
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
            live.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        if rebuild_preamble {
            live.apply_settings_now(false).await?;
        }

        if server.runner_enabled() {
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
        })
    }

    pub async fn snapshot(&self) -> Result<domain::SessionSnapshot, ApiError> {
        let tools = { self.tools.lock().await.clone() };
        Ok(domain::SessionSnapshot {
            summary: self.summary().await?,
            mcp_status: map_mcp_status(tools.mcp_status().await),
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
                CoreMessage::User { content } => {
                    if content.first_text().unwrap_or("").trim().is_empty() {
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
        let new_preamble = build_preamble(
            profile,
            &effective.model_id,
            &effective.workspace_root,
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

            for msg in preamble_updates(session.messages.as_slice(), new_preamble) {
                self.store
                    .record_message(&mut session, msg)
                    .await
                    .map_err(map_session_err)?;
            }
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
            domain::RunInput::Text { text } => {
                if text.trim().is_empty() {
                    return Err(ApiError::invalid_argument("input text must not be empty"));
                }
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
                    CoreMessage::User { content } => {
                        if content.first_text().unwrap_or("").trim().is_empty() {
                            return Err(ApiError::invalid_argument("input text must not be empty"));
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

            {
                let mut st = self.status.lock().await;
                st.run_state = domain::SessionRunState::Running;
                st.active_run_id = Some(run.id.clone());
                st.step = 0;
                st.active_tool = None;
            }

            run.state = domain::RunState::Running;
            run.started_at = Some(now_rfc3339());
            write_run_file(&self.runs_dir, &run).await?;

            let (user_text, persist_user) = match &run.input {
                domain::RunInput::Text { text } => (text.clone(), true),
                domain::RunInput::FromUserMessage { user_message_id } => {
                    let session = self.session.lock().await;
                    let by_id = session
                        .message_ids
                        .iter()
                        .position(|id| *id == *user_message_id)
                        .and_then(|idx| match &session.messages[idx] {
                            CoreMessage::User { content } => {
                                Some(content.first_text().unwrap_or("").to_string())
                            }
                            _ => None,
                        });
                    let last_user = session.messages.iter().rev().find_map(|m| match m {
                        CoreMessage::User { content } => {
                            Some(content.first_text().unwrap_or("").to_string())
                        }
                        _ => None,
                    });
                    (by_id.or(last_user).unwrap_or_default(), false)
                }
                domain::RunInput::EditUserMessage { content, .. } => (content.clone(), false),
                domain::RunInput::RegenerateAfterUserMessage { .. } => {
                    let session = self.session.lock().await;
                    let last_user = session.messages.iter().rev().find_map(|m| match m {
                        CoreMessage::User { content } => {
                            Some(content.first_text().unwrap_or("").to_string())
                        }
                        _ => None,
                    });
                    (last_user.unwrap_or_default(), false)
                }
            };

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

            // Per-run MCP config.
            let cfg_for_run = config_with_mcp_overrides(config.as_ref(), &effective.mcp.servers)?;
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
            let options = AgentRuntimeOptions::from_config(&profile, config.as_ref());
            let max_steps = options.max_steps;

            let llm = kiliax_core::llm::LlmClient::from_config(
                config.as_ref(),
                Some(&effective.model_id),
            )
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
                    content: UserMessageContent::Text(user_text.clone()),
                })
                .await?;
            }

            let runtime = AgentRuntime::new(llm, tools_for_run.clone());

            let mut messages = { self.session.lock().await.messages.clone() };
            if effective.agent != base_settings.agent
                || effective.model_id != base_settings.model_id
                || effective.mcp.servers != base_settings.mcp.servers
            {
                let skills_config = skills_config_from_settings(&base_settings.skills);
                let preamble = build_preamble(
                    &profile,
                    &effective.model_id,
                    &workspace_root,
                    &tools_for_run,
                    &skills_config,
                )
                .await;
                insert_preamble_updates_before_last_user(&mut messages, preamble);
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

            let (step, active_tool) = {
                let st = self.status.lock().await;
                (st.step, st.active_tool.clone())
            };
            let trace_id = kiliax_core::telemetry::spans::current_trace_id();

            // Restore tool config to current session defaults (may have changed).
            let current_settings = self.settings.lock().await.clone();
            let cfg_for_tools =
                config_with_mcp_overrides(config.as_ref(), &current_settings.mcp.servers)?;
            {
                let tools = self.tools.lock().await;
                let _ = tools.set_config(cfg_for_tools);
            }

            {
                let mut st = self.status.lock().await;
                st.run_state = domain::SessionRunState::Idle;
                st.active_run_id = None;
                st.step = 0;
                st.active_tool = None;
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
                *tools = next;
            } else {
                tools
                    .set_config(cfg_for_tools)
                    .map_err(ApiError::internal_error)?;
                tools
                    .set_extra_workspace_roots(extra_workspace_roots)
                    .map_err(ApiError::internal_error)?;
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
            &tools,
            &skills_config,
        )
        .await;

        let mut session = self.session.lock().await;
        let updates = preamble_updates(session.messages.as_slice(), preamble);
        for msg in updates {
            self.store
                .record_message(&mut session, msg)
                .await
                .map_err(map_session_err)?;
        }
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
                self.status.lock().await.step = step as u32;
                self.emit_event(domain::Event {
                    event_id: self.alloc_event_id(),
                    ts: now_rfc3339(),
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "step_start".to_string(),
                    data: serde_json::json!({ "step": step }),
                })
                .await?;
            }
            AgentEvent::StepEnd { step } => {
                self.emit_event(domain::Event {
                    event_id: self.alloc_event_id(),
                    ts: now_rfc3339(),
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "step_end".to_string(),
                    data: serde_json::json!({ "step": step }),
                })
                .await?;
            }
            AgentEvent::AssistantThinkingDelta { delta } => {
                self.emit_ephemeral_event(domain::Event {
                    event_id: self.alloc_event_id(),
                    ts: now_rfc3339(),
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "assistant_thinking_delta".to_string(),
                    data: serde_json::json!({ "delta": delta }),
                })
                .await?;
            }
            AgentEvent::AssistantDelta { delta } => {
                self.emit_ephemeral_event(domain::Event {
                    event_id: self.alloc_event_id(),
                    ts: now_rfc3339(),
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "assistant_delta".to_string(),
                    data: serde_json::json!({ "delta": delta }),
                })
                .await?;
            }
            AgentEvent::AssistantMessage { message } => {
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
            }
            AgentEvent::ToolCall { call } => {
                {
                    let mut st = self.status.lock().await;
                    st.run_state = domain::SessionRunState::Tooling;
                    st.active_tool = Some(call.name.clone());
                }
                self.emit_event(domain::Event {
                    event_id: self.alloc_event_id(),
                    ts: now_rfc3339(),
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
                self.emit_event(domain::Event {
                    event_id: self.alloc_event_id(),
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
    use kiliax_core::config::{ProviderConfig, ProviderKind};
    use kiliax_core::protocol::{Message as CoreMessage, UserMessageContent};
    use tempfile::TempDir;

    fn test_config() -> Config {
        let mut cfg = Config {
            default_model: Some("test/test-model".to_string()),
            ..Default::default()
        };
        cfg.providers.insert(
            "test".to_string(),
            ProviderConfig {
                kind: ProviderKind::OpenAICompatible,
                base_url: "http://127.0.0.1:1".to_string(),
                api_key: None,
                models: vec!["test-model".to_string()],
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
}
