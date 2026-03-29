use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use axum::http::StatusCode;
use kiliax_core::agents::AgentProfile;
use kiliax_core::config::Config;
use kiliax_core::llm::{Message as CoreMessage, UserMessageContent};
use kiliax_core::runtime::{AgentEvent, AgentRuntime, AgentRuntimeError, AgentRuntimeOptions};
use kiliax_core::session::{
    FileSessionStore, SessionError, SessionEvent, SessionEventLine, SessionId, SessionMeta,
    SessionState,
};
use kiliax_core::tools::{McpServerConnectionState, ToolEngine};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{broadcast, watch, Mutex, Notify};
use tokio_stream::StreamExt;
use tracing::{Instrument, Span};

use crate::api;
use crate::error::{ApiError, ApiErrorCode};

pub struct ServerState {
    pub workspace_root: PathBuf,
    pub config_path: PathBuf,
    pub config: Arc<RwLock<Arc<Config>>>,
    pub token: Option<String>,

    pub store: FileSessionStore,
    pub runs_dir: PathBuf,
    pub tools_for_caps: ToolEngine,

    pub shutdown: Arc<Notify>,
    runner_enabled: bool,
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
    idempotency: Mutex<HashMap<String, String>>,
}

impl ServerState {
    pub async fn new(
        workspace_root: PathBuf,
        config_path: PathBuf,
        config: Config,
        token: Option<String>,
    ) -> Result<Self, anyhow::Error> {
        let store = FileSessionStore::global()
            .ok_or_else(|| anyhow::anyhow!("failed to determine home directory for sessions (expected ~/sessions)"))?;
        Self::new_inner(workspace_root, config_path, config, token, true, store).await
    }

    #[cfg(test)]
    pub async fn new_for_tests(
        workspace_root: PathBuf,
        config_path: PathBuf,
        config: Config,
        token: Option<String>,
    ) -> Result<Self, anyhow::Error> {
        let store = FileSessionStore::project(&workspace_root);
        Self::new_inner(workspace_root, config_path, config, token, false, store).await
    }

    async fn new_inner(
        workspace_root: PathBuf,
        config_path: PathBuf,
        config: Config,
        token: Option<String>,
        runner_enabled: bool,
        store: FileSessionStore,
    ) -> Result<Self, anyhow::Error> {
        let runs_dir = workspace_root.join(".kiliax").join("runs");
        tokio::fs::create_dir_all(&runs_dir).await?;

        let tools_for_caps = ToolEngine::new(&workspace_root, config.clone());
        Ok(Self {
            workspace_root: workspace_root.clone(),
            config_path,
            config: Arc::new(RwLock::new(Arc::new(config))),
            token,
            store,
            runs_dir,
            tools_for_caps,
            shutdown: Arc::new(Notify::new()),
            runner_enabled,
            sessions: Mutex::new(HashMap::new()),
            idempotency: Mutex::new(HashMap::new()),
        })
    }

    fn config_snapshot(&self) -> Result<Arc<Config>, ApiError> {
        self.config
            .read()
            .map(|v| v.clone())
            .map_err(|_| ApiError::internal("config lock poisoned"))
    }

    pub async fn get_config(&self) -> Result<api::ConfigResponse, ApiError> {
        let yaml = match tokio::fs::read_to_string(&self.config_path).await {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(ApiError::internal(err.to_string())),
        };
        let config = self.config_snapshot()?.as_ref().clone();
        Ok(api::ConfigResponse {
            path: self.config_path.display().to_string(),
            yaml,
            config,
        })
    }

    pub async fn update_config(
        &self,
        req: api::ConfigUpdateRequest,
    ) -> Result<api::ConfigResponse, ApiError> {
        let next = kiliax_core::config::load_from_str(&req.yaml)
            .map_err(|e| ApiError::invalid_argument(e.to_string()))?;

        write_text_atomic(&self.config_path, &req.yaml).await?;

        {
            let mut guard = self
                .config
                .write()
                .map_err(|_| ApiError::internal("config lock poisoned"))?;
            *guard = Arc::new(next.clone());
        }
        self.tools_for_caps
            .set_config(next.clone())
            .map_err(|e| ApiError::internal(e.to_string()))?;

        let live_sessions = {
            let guard = self.sessions.lock().await;
            guard.values().cloned().collect::<Vec<_>>()
        };
        for live in live_sessions {
            live.on_config_updated().await?;
        }

        Ok(api::ConfigResponse {
            path: self.config_path.display().to_string(),
            yaml: req.yaml,
            config: next,
        })
    }

    pub async fn patch_config_mcp(
        &self,
        req: api::ConfigMcpPatchRequest,
    ) -> Result<(), ApiError> {
        let current = self.config_snapshot()?;
        let mut next = current.as_ref().clone();

        let known: HashSet<String> = next.mcp.servers.iter().map(|s| s.name.clone()).collect();
        for p in req.servers {
            if !known.contains(&p.id) {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    ApiErrorCode::McpServerNotFound,
                    format!("mcp server not found: {}", p.id),
                ));
            }
            for server in &mut next.mcp.servers {
                if server.name == p.id {
                    server.enable = p.enable;
                }
            }
        }

        let yaml =
            serde_yaml::to_string(&next).map_err(|e| ApiError::internal(e.to_string()))?;
        let _ = self
            .update_config(api::ConfigUpdateRequest { yaml })
            .await?;
        Ok(())
    }

    pub async fn list_skills(
        &self,
        session_id: &SessionId,
    ) -> Result<api::SkillListResponse, ApiError> {
        let workspace_root = if let Some(live) = self.get_live(session_id.as_str()).await {
            live.settings.lock().await.workspace_root.clone()
        } else {
            let state = self.store.load(session_id).await.map_err(map_session_err)?;
            let config = self.config_snapshot()?;
            let settings = load_settings_for_meta(&self.store, &state.meta, config.as_ref()).await?;
            settings.workspace_root
        };

        if workspace_root.trim().is_empty() {
            return Ok(api::SkillListResponse { items: Vec::new() });
        }
        let root = PathBuf::from(workspace_root.trim());

        let skills = kiliax_core::tools::skills::discover_skills(&root)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        Ok(api::SkillListResponse {
            items: skills
                .into_iter()
                .map(|s| api::SkillSummary {
                    id: s.id,
                    name: s.name,
                    description: s.description,
                })
                .collect(),
        })
    }

    pub async fn list_global_skills(&self) -> Result<api::SkillListResponse, ApiError> {
        let skills = kiliax_core::tools::skills::discover_skills(&self.workspace_root)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        Ok(api::SkillListResponse {
            items: skills
                .into_iter()
                .map(|s| api::SkillSummary {
                    id: s.id,
                    name: s.name,
                    description: s.description,
                })
                .collect(),
        })
    }

    pub async fn get_live(&self, session_id: &str) -> Option<Arc<LiveSession>> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    pub async fn ensure_live(&self, session_id: &SessionId) -> Result<Arc<LiveSession>, ApiError> {
        if let Some(live) = self.get_live(session_id.as_str()).await {
            return Ok(live);
        }
        let live = LiveSession::resume(self, session_id).await?;
        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), live.clone());
        Ok(live)
    }

    pub async fn create_session(
        &self,
        idem_key: Option<String>,
        req: api::SessionCreateRequest,
    ) -> Result<api::Session, ApiError> {
        if let Some(key) = idem_key {
            let map_key = format!("POST:/v1/sessions:{key}");
            if let Some(existing) = self.idempotency.lock().await.get(&map_key).cloned() {
                let id = SessionId::parse(&existing).map_err(|e| ApiError::invalid_argument(e.to_string()))?;
                let live = self.ensure_live(&id).await?;
                return live.snapshot().await;
            }
            let created = self.create_session_inner(req).await?;
            self.idempotency
                .lock()
                .await
                .insert(map_key, created.summary.id.clone());
            return Ok(created);
        }
        self.create_session_inner(req).await
    }

    async fn create_session_inner(&self, req: api::SessionCreateRequest) -> Result<api::Session, ApiError> {
        let config = self.config_snapshot()?;

        let mut settings = default_settings(config.as_ref(), None)?;
        if let Some(patch) = req.settings {
            if let Some(root) = patch.workspace_root.as_deref() {
                let root = validate_client_workspace_root(root)?;
                settings.workspace_root = root.display().to_string();
            }
            apply_settings_patch(&mut settings, &patch, config.as_ref(), true)?;
        }

        let profile = AgentProfile::from_name(&settings.agent).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ApiErrorCode::AgentNotSupported,
                "agent not supported",
            )
        })?;

        config.resolve_model(&settings.model_id).map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ApiErrorCode::ModelNotSupported,
                e.to_string(),
            )
        })?;

        if settings.workspace_root.trim().is_empty() {
            let root = default_tmp_workspace_root()?;
            settings.workspace_root = root.display().to_string();
        }
        let workspace_root = PathBuf::from(settings.workspace_root.trim());
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        let cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
        let tools = ToolEngine::new(&workspace_root, cfg_for_tools);

        let messages =
            build_preamble(&profile, &settings.model_id, &workspace_root, &tools).await;

        let mut session = self
            .store
            .create(
                profile.name.to_string(),
                Some(settings.model_id.clone()),
                Some(self.config_path.display().to_string()),
                Some(settings.workspace_root.clone()),
                messages.clone(),
            )
            .await
            .map_err(map_session_err)?;

        if let Some(title) = req.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            session.meta.title = Some(title.to_string());
            self.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        let live = LiveSession::from_state(self, session, settings, tools).await?;
        self.sessions
            .lock()
            .await
            .insert(live.id().to_string(), live.clone());
        live.snapshot().await
    }

    pub async fn list_sessions(
        &self,
        live_only: bool,
        limit: usize,
        cursor: Option<String>,
    ) -> Result<api::SessionListResponse, ApiError> {
        let config = self.config_snapshot()?;

        let limit = limit.clamp(1, 200);
        let offset = cursor
            .as_deref()
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0);

        let mut items: Vec<api::SessionSummary> = Vec::new();
        if live_only {
            let live = self.sessions.lock().await;
            for s in live.values() {
                items.push(s.summary().await?);
            }
        } else {
            for meta in self.store.list().await.map_err(map_session_err)? {
                let id = meta.id.to_string();
                if let Some(live) = self.get_live(&id).await {
                    items.push(live.summary().await?);
                    continue;
                }

                let settings = load_settings_for_meta(&self.store, &meta, config.as_ref()).await?;
                let last_event_id = read_last_event_id(&session_events_api_path(&self.store, &meta.id)).await?;
                let last_outcome = if meta.last_error.is_some() {
                    api::SessionLastOutcome::Error
                } else if meta.last_finish_reason.is_some() {
                    api::SessionLastOutcome::Done
                } else {
                    api::SessionLastOutcome::None
                };

                items.push(api::SessionSummary {
                    id: id.clone(),
                    title: meta.title.clone().unwrap_or_else(|| id.clone()),
                    created_at: ts_ms_to_rfc3339(meta.created_at_ms),
                    updated_at: ts_ms_to_rfc3339(meta.updated_at_ms),
                    last_outcome,
                    status: api::SessionStatus {
                        session_state: api::SessionState::Archived,
                        run_state: api::SessionRunState::Idle,
                        active_run_id: None,
                        step: 0,
                        active_tool: None,
                        queue_len: 0,
                        last_event_id,
                    },
                    settings,
                });
            }
        }

        let total = items.len();
        let items = items.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        let next_cursor = if offset + limit < total {
            Some((offset + limit).to_string())
        } else {
            None
        };

        Ok(api::SessionListResponse { items, next_cursor })
    }

    pub async fn get_session(&self, session_id: &SessionId) -> Result<api::Session, ApiError> {
        let config = self.config_snapshot()?;

        if let Some(live) = self.get_live(session_id.as_str()).await {
            return live.snapshot().await;
        }

        let state = self.store.load(session_id).await.map_err(map_session_err)?;
        let settings = load_settings_for_meta(&self.store, &state.meta, config.as_ref()).await?;
        let last_event_id = read_last_event_id(&session_events_api_path(&self.store, session_id)).await?;
        let last_outcome = if state.meta.last_error.is_some() {
            api::SessionLastOutcome::Error
        } else if state.meta.last_finish_reason.is_some() {
            api::SessionLastOutcome::Done
        } else {
            api::SessionLastOutcome::None
        };

        Ok(api::Session {
            summary: api::SessionSummary {
                id: session_id.to_string(),
                title: state
                    .meta
                    .title
                    .clone()
                    .unwrap_or_else(|| session_id.to_string()),
                created_at: ts_ms_to_rfc3339(state.meta.created_at_ms),
                updated_at: ts_ms_to_rfc3339(state.meta.updated_at_ms),
                last_outcome,
                status: api::SessionStatus {
                    session_state: api::SessionState::Archived,
                    run_state: api::SessionRunState::Idle,
                    active_run_id: None,
                    step: 0,
                    active_tool: None,
                    queue_len: 0,
                    last_event_id,
                },
                settings: settings.clone(),
            },
            mcp_status: mcp_status_from_settings(&settings, config.as_ref()),
        })
    }

    pub async fn delete_session(&self, session_id: &SessionId) -> Result<(), ApiError> {
        let live = self.sessions.lock().await.remove(session_id.as_str());
        if let Some(live) = live {
            live.shutdown().await;
        }
        self.store.delete(session_id).await.map_err(map_session_err)?;
        Ok(())
    }

    pub async fn resume_session(&self, session_id: &SessionId) -> Result<api::Session, ApiError> {
        let live = self.ensure_live(session_id).await?;
        live.snapshot().await
    }

    pub async fn patch_session_settings(
        &self,
        session_id: &SessionId,
        patch: api::SessionSettingsPatch,
    ) -> Result<api::Session, ApiError> {
        let live = self.ensure_live(session_id).await?;
        live.patch_settings(patch).await?;
        live.snapshot().await
    }

    pub async fn create_run(
        &self,
        session_id: &SessionId,
        idem_key: Option<String>,
        req: api::RunCreateRequest,
    ) -> Result<api::Run, ApiError> {
        if let Some(key) = idem_key {
            let map_key = format!("POST:/v1/sessions/{session_id}/runs:{key}");
            if let Some(existing) = self.idempotency.lock().await.get(&map_key).cloned() {
                return self.get_run(&existing).await;
            }
            let run = self.create_run_inner(session_id, req).await?;
            self.idempotency
                .lock()
                .await
                .insert(map_key, run.id.clone());
            return Ok(run);
        }
        self.create_run_inner(session_id, req).await
    }

    async fn create_run_inner(
        &self,
        session_id: &SessionId,
        req: api::RunCreateRequest,
    ) -> Result<api::Run, ApiError> {
        let live = if req.auto_resume {
            self.ensure_live(session_id).await?
        } else {
            match self.get_live(session_id.as_str()).await {
                Some(live) => live,
                None => {
                    self.ensure_on_disk_session_exists(session_id).await?;
                    return Err(ApiError::session_not_live("session is archived"));
                }
            }
        };
        live.enqueue_run(&self.runs_dir, req).await
    }

    pub async fn get_run(&self, run_id: &str) -> Result<api::Run, ApiError> {
        read_run_file(&self.runs_dir, run_id).await
    }

    pub async fn cancel_run(&self, run_id: &str) -> Result<api::Run, ApiError> {
        let run = read_run_file(&self.runs_dir, run_id).await?;
        let session_id = SessionId::parse(&run.session_id)
            .map_err(|e| ApiError::invalid_argument(e.to_string()))?;
        let live = self
            .get_live(session_id.as_str())
            .await
            .ok_or_else(|| ApiError::session_not_live("session is not live"))?;

        live.cancel_run(&self.runs_dir, run_id).await?;
        self.get_run(run_id).await
    }

    pub async fn get_messages(
        &self,
        session_id: &SessionId,
        limit: usize,
        before: Option<String>,
    ) -> Result<api::MessageListResponse, ApiError> {
        let limit = limit.clamp(1, 200);
        let before_seq = before
            .as_deref()
            .and_then(|v| v.parse::<u64>().ok());

        // Ensure session exists.
        let _ = self.store.load(session_id).await.map_err(map_session_err)?;

        let path = self.store.events_path(session_id);
        let file = tokio::fs::File::open(&path)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ApiError::not_found("session not found")
                } else {
                    ApiError::internal(e.to_string())
                }
            })?;
        let mut reader = tokio::io::BufReader::new(file);
        let mut line = String::new();

        let mut messages: Vec<(u64, u64, CoreMessage)> = Vec::new(); // (seq, ts_ms, message)
        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;
            if n == 0 {
                break;
            }
            let raw = line.trim();
            if raw.is_empty() {
                continue;
            }
            let parsed: SessionEventLine = match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(before_seq) = before_seq {
                if parsed.seq >= before_seq {
                    continue;
                }
            }
            if let SessionEvent::Message { message } = parsed.event {
                messages.push((parsed.seq, parsed.ts_ms, message));
            }
        }

        messages.sort_by_key(|(seq, _, _)| *seq);
        let slice = messages.into_iter().rev().take(limit).collect::<Vec<_>>();
        let slice = slice.into_iter().rev().collect::<Vec<_>>();

        let next_before = slice.first().map(|(seq, _, _)| seq.to_string());

        let mut out = Vec::new();
        for (seq, ts_ms, msg) in slice {
            if let Some(api_msg) = map_core_message_to_api(seq, ts_ms, msg) {
                out.push(api_msg);
            }
        }

        Ok(api::MessageListResponse {
            items: out,
            next_before,
        })
    }

    pub async fn get_capabilities(&self) -> Result<api::Capabilities, ApiError> {
        let config = self.config_snapshot()?;
        Ok(api::Capabilities {
            agents: vec!["general".to_string(), "plan".to_string()],
            models: list_models(config.as_ref()),
            mcp_servers: map_mcp_status(self.tools_for_caps.mcp_status().await),
        })
    }

    pub async fn list_events(
        &self,
        session_id: &SessionId,
        limit: usize,
        after: Option<u64>,
    ) -> Result<api::EventListResponse, ApiError> {
        self.ensure_on_disk_session_exists(session_id).await?;
        let limit = limit.clamp(1, 200);
        let after = after.unwrap_or(0);
        let path = session_events_api_path(&self.store, session_id);
        let events = read_events_after(&path, after, limit).await?;
        let next_after = events.last().map(|e| e.event_id);
        Ok(api::EventListResponse {
            items: events,
            next_after,
        })
    }

    pub async fn events_backlog_after(
        &self,
        session_id: &SessionId,
        after_event_id: u64,
        limit: usize,
    ) -> Result<Vec<api::Event>, ApiError> {
        self.ensure_on_disk_session_exists(session_id).await?;
        let path = session_events_api_path(&self.store, session_id);
        read_events_after(&path, after_event_id, limit).await
    }

    pub async fn live_events_stream(
        &self,
        session_id: &SessionId,
    ) -> Result<broadcast::Receiver<api::Event>, ApiError> {
        match self.get_live(session_id.as_str()).await {
            Some(live) => Ok(live.events_tx.subscribe()),
            None => {
                self.ensure_on_disk_session_exists(session_id).await?;
                Err(ApiError::session_not_live("session is archived"))
            }
        }
    }

    async fn ensure_on_disk_session_exists(&self, session_id: &SessionId) -> Result<(), ApiError> {
        let path = self.store.meta_path(session_id);
        tokio::fs::metadata(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ApiError::not_found("session not found")
            } else {
                ApiError::internal(e.to_string())
            }
        })?;
        Ok(())
    }
}

fn map_session_err(err: SessionError) -> ApiError {
    match err {
        SessionError::NotFound(_) => ApiError::not_found("session not found"),
        SessionError::InvalidId(msg) => ApiError::invalid_argument(msg),
        other => ApiError::internal(other.to_string()),
    }
}

fn session_settings_path(store: &FileSessionStore, session_id: &SessionId) -> PathBuf {
    store.session_dir(session_id).join("settings.json")
}

fn session_events_api_path(store: &FileSessionStore, session_id: &SessionId) -> PathBuf {
    store.session_dir(session_id).join("events_api.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsFile {
    agent: String,
    model_id: String,
    mcp: api::McpServers,
    #[serde(default)]
    workspace_root: String,
}

async fn read_settings_file(path: &Path) -> Result<Option<SettingsFile>, ApiError> {
    let text = match tokio::fs::read_to_string(path).await {
        Ok(t) => t,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(ApiError::internal(err.to_string())),
    };
    let parsed: SettingsFile =
        serde_json::from_str(&text).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Some(parsed))
}

async fn write_settings_file(path: &Path, settings: &api::SessionSettings) -> Result<(), ApiError> {
    let file = SettingsFile {
        agent: settings.agent.clone(),
        model_id: settings.model_id.clone(),
        mcp: settings.mcp.clone(),
        workspace_root: settings.workspace_root.clone(),
    };
    let text =
        serde_json::to_string_pretty(&file).map_err(|e| ApiError::internal(e.to_string()))?;
    tokio::fs::write(path, text)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

async fn load_settings_for_meta(
    store: &FileSessionStore,
    meta: &SessionMeta,
    config: &Config,
) -> Result<api::SessionSettings, ApiError> {
    let path = session_settings_path(store, &meta.id);

    if let Some(file) = read_settings_file(&path).await? {
        let mut settings = api::SessionSettings {
            agent: file.agent,
            model_id: file.model_id,
            mcp: file.mcp,
            workspace_root: file.workspace_root,
        };
        normalize_settings(&mut settings, meta, config)?;
        return Ok(settings);
    }

    let mut settings = default_settings(config, Some(meta))?;
    normalize_settings(&mut settings, meta, config)?;
    write_settings_file(&path, &settings).await?;
    Ok(settings)
}

fn normalize_settings(
    settings: &mut api::SessionSettings,
    meta: &SessionMeta,
    config: &Config,
) -> Result<(), ApiError> {
    if AgentProfile::from_name(&settings.agent).is_none() {
        settings.agent = AgentProfile::from_name(&meta.agent)
            .unwrap_or_else(AgentProfile::general)
            .name
            .to_string();
    } else {
        settings.agent = AgentProfile::from_name(&settings.agent)
            .unwrap_or_else(AgentProfile::general)
            .name
            .to_string();
    }

    if settings.model_id.trim().is_empty() {
        settings.model_id = meta
            .model_id
            .clone()
            .or_else(|| config.default_model.clone())
            .ok_or_else(|| ApiError::invalid_argument("missing model id"))?;
    }
    if config.resolve_model(&settings.model_id).is_err() {
        settings.model_id = config
            .default_model
            .clone()
            .ok_or_else(|| ApiError::invalid_argument("missing default_model in config"))?;
    }

    let ws = settings.workspace_root.trim().to_string();
    settings.workspace_root = if !ws.is_empty() {
        ws
    } else {
        meta.workspace_root.clone().unwrap_or_default()
    };

    settings.mcp.servers = config
        .mcp
        .servers
        .iter()
        .map(|s| api::McpServerSetting {
            id: s.name.clone(),
            enable: s.enable,
        })
        .collect();
    Ok(())
}

fn home_kiliax_dir() -> Result<PathBuf, ApiError> {
    let home = dirs::home_dir().ok_or_else(|| ApiError::internal("failed to resolve home dir"))?;
    Ok(home.join(".kiliax"))
}

fn expand_tilde(path: &str) -> Result<PathBuf, ApiError> {
    let trimmed = path.trim();
    if trimmed == "~" {
        return dirs::home_dir().ok_or_else(|| ApiError::internal("failed to resolve home dir"));
    }
    let Some(rest) = trimmed.strip_prefix("~/") else {
        return Ok(PathBuf::from(trimmed));
    };
    let home = dirs::home_dir().ok_or_else(|| ApiError::internal("failed to resolve home dir"))?;
    Ok(home.join(rest))
}

fn validate_client_workspace_root(input: &str) -> Result<PathBuf, ApiError> {
    let candidate = expand_tilde(input)?;
    if !candidate.is_absolute() {
        return Err(ApiError::invalid_argument("workspace_root must be an absolute path"));
    }
    for c in candidate.components() {
        if matches!(c, std::path::Component::ParentDir) {
            return Err(ApiError::invalid_argument(
                "workspace_root must not contain `..`",
            ));
        }
    }

    let allowed_root = home_kiliax_dir()?;
    if !candidate.starts_with(&allowed_root) {
        return Err(ApiError::invalid_argument(format!(
            "workspace_root must be within {}",
            allowed_root.display()
        )));
    }

    Ok(candidate)
}

fn default_tmp_workspace_root() -> Result<PathBuf, ApiError> {
    let base = home_kiliax_dir()?.join("workspace");
    Ok(base.join(format!("tmp_{}", SessionId::new())))
}

fn default_settings(config: &Config, meta: Option<&SessionMeta>) -> Result<api::SessionSettings, ApiError> {
    let agent = meta
        .and_then(|m| AgentProfile::from_name(&m.agent))
        .unwrap_or_else(AgentProfile::general)
        .name
        .to_string();

    let model_id = meta
        .and_then(|m| m.model_id.clone())
        .or_else(|| config.default_model.clone())
        .ok_or_else(|| ApiError::invalid_argument("missing model id (set default_model in config)"))?;

    let workspace_root = meta
        .and_then(|m| m.workspace_root.clone())
        .unwrap_or_default();

    let servers = config
        .mcp
        .servers
        .iter()
        .map(|s| api::McpServerSetting {
            id: s.name.clone(),
            enable: s.enable,
        })
        .collect();

    Ok(api::SessionSettings {
        agent,
        model_id,
        mcp: api::McpServers { servers },
        workspace_root,
    })
}

fn apply_settings_patch(
    settings: &mut api::SessionSettings,
    patch: &api::SessionSettingsPatch,
    config: &Config,
    allow_enable: bool,
) -> Result<(), ApiError> {
    if let Some(agent) = patch.agent.as_deref() {
        let profile = AgentProfile::from_name(agent).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ApiErrorCode::AgentNotSupported,
                "agent not supported",
            )
        })?;
        settings.agent = profile.name.to_string();
    }
    if let Some(model_id) = patch.model_id.as_deref() {
        config
            .resolve_model(model_id)
            .map_err(|e| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    ApiErrorCode::ModelNotSupported,
                    e.to_string(),
                )
            })?;
        settings.model_id = model_id.to_string();
    }
    if let Some(patch_servers) = patch.mcp.as_ref().and_then(|m| m.servers.as_ref()) {
        merge_mcp_settings(&mut settings.mcp.servers, patch_servers, config, allow_enable)?;
    }
    if let Some(root) = patch.workspace_root.as_deref() {
        settings.workspace_root = root.trim().to_string();
    }
    Ok(())
}

fn merge_mcp_settings(
    existing: &mut Vec<api::McpServerSetting>,
    patch: &[api::McpServerSetting],
    config: &Config,
    allow_enable: bool,
) -> Result<(), ApiError> {
    let known: HashSet<&str> = config.mcp.servers.iter().map(|s| s.name.as_str()).collect();
    let mut map: HashMap<String, bool> = existing.iter().map(|s| (s.id.clone(), s.enable)).collect();

    for p in patch {
        if !known.contains(p.id.as_str()) {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                ApiErrorCode::McpServerNotFound,
                format!("mcp server not found: {}", p.id),
            ));
        }
        if p.enable && !allow_enable && !map.get(&p.id).copied().unwrap_or(false) {
            return Err(ApiError::invalid_argument(format!(
                "per-run overrides cannot enable mcp server: {}",
                p.id
            )));
        }
        map.insert(p.id.clone(), p.enable);
    }

    existing.clear();
    for server in &config.mcp.servers {
        let enable = map.get(&server.name).copied().unwrap_or(server.enable);
        existing.push(api::McpServerSetting {
            id: server.name.clone(),
            enable,
        });
    }
    Ok(())
}

fn config_with_mcp_overrides(base: &Config, servers: &[api::McpServerSetting]) -> Result<Config, ApiError> {
    let mut cfg = base.clone();
    let known: HashSet<&str> = base.mcp.servers.iter().map(|s| s.name.as_str()).collect();
    let mut map: HashMap<String, bool> = HashMap::new();
    for s in servers {
        if !known.contains(s.id.as_str()) {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                ApiErrorCode::McpServerNotFound,
                format!("mcp server not found: {}", s.id),
            ));
        }
        map.insert(s.id.clone(), s.enable);
    }
    for server in &mut cfg.mcp.servers {
        if let Some(enable) = map.get(&server.name) {
            server.enable = *enable;
        }
    }
    Ok(cfg)
}

fn mcp_status_from_settings(settings: &api::SessionSettings, config: &Config) -> Vec<api::McpServerStatus> {
    let by_id: HashMap<String, bool> =
        settings.mcp.servers.iter().map(|s| (s.id.clone(), s.enable)).collect();
    config
        .mcp
        .servers
        .iter()
        .map(|s| {
            let enable = by_id.get(&s.name).copied().unwrap_or(s.enable);
            api::McpServerStatus {
                id: s.name.clone(),
                enable,
                state: if enable {
                    api::McpConnectionState::Error
                } else {
                    api::McpConnectionState::Disabled
                },
                last_error: None,
                tools: None,
            }
        })
        .collect()
}

fn map_mcp_status(status: Vec<kiliax_core::tools::McpServerStatus>) -> Vec<api::McpServerStatus> {
    status
        .into_iter()
        .map(|s| {
            let (state, last_error) = match s.state {
                McpServerConnectionState::Disabled => (api::McpConnectionState::Disabled, None),
                McpServerConnectionState::Connecting => (api::McpConnectionState::Connecting, None),
                McpServerConnectionState::Connected => (api::McpConnectionState::Connected, None),
                McpServerConnectionState::Retry { error, .. } => {
                    (api::McpConnectionState::Error, Some(error))
                }
                McpServerConnectionState::Disconnected => (api::McpConnectionState::Error, None),
            };
            api::McpServerStatus {
                id: s.name,
                enable: state != api::McpConnectionState::Disabled,
                state,
                last_error,
                tools: None,
            }
        })
        .collect()
}

fn list_models(config: &Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (provider, p) in &config.providers {
        if p.models.is_empty() {
            continue;
        }
        for m in &p.models {
            let qualified = if m.contains('/') {
                m.to_string()
            } else {
                format!("{provider}/{m}")
            };
            if seen.insert(qualified.clone()) {
                out.push(qualified);
            }
        }
    }
    if out.is_empty() {
        if let Some(m) = config.default_model.as_deref() {
            out.push(m.to_string());
        }
    }
    out.sort();
    out
}

fn ts_ms_to_rfc3339(ms: u64) -> String {
    use time::format_description::well_known::Rfc3339;
    let dt = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    dt.format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn map_core_message_to_api(seq: u64, ts_ms: u64, msg: CoreMessage) -> Option<api::Message> {
    let id = seq.to_string();
    let created_at = ts_ms_to_rfc3339(ts_ms);
    match msg {
        CoreMessage::User { content } => Some(api::Message::User {
            id,
            created_at,
            content: content.first_text().unwrap_or("").to_string(),
        }),
        CoreMessage::Assistant {
            content,
            reasoning_content,
            tool_calls,
        } => Some(api::Message::Assistant {
            id,
            created_at,
            content: content.unwrap_or_default(),
            reasoning_content,
            tool_calls: tool_calls
                .into_iter()
                .map(|c| api::ToolCall {
                    id: c.id,
                    name: c.name,
                    arguments: c.arguments,
                })
                .collect(),
        }),
        CoreMessage::Tool {
            tool_call_id,
            content,
        } => Some(api::Message::Tool {
            id,
            created_at,
            tool_call_id,
            content,
        }),
        CoreMessage::System { .. } | CoreMessage::Developer { .. } => None,
    }
}

fn map_core_message_to_api_event_message(
    seq: u64,
    created_at: String,
    msg: CoreMessage,
) -> Option<api::Message> {
    let id = seq.to_string();
    match msg {
        CoreMessage::User { content } => Some(api::Message::User {
            id,
            created_at,
            content: content.first_text().unwrap_or("").to_string(),
        }),
        CoreMessage::Assistant {
            content,
            reasoning_content,
            tool_calls,
        } => Some(api::Message::Assistant {
            id,
            created_at,
            content: content.unwrap_or_default(),
            reasoning_content,
            tool_calls: tool_calls
                .into_iter()
                .map(|c| api::ToolCall {
                    id: c.id,
                    name: c.name,
                    arguments: c.arguments,
                })
                .collect(),
        }),
        CoreMessage::Tool {
            tool_call_id,
            content,
        } => Some(api::Message::Tool {
            id,
            created_at,
            tool_call_id,
            content,
        }),
        CoreMessage::System { .. } | CoreMessage::Developer { .. } => None,
    }
}

async fn read_last_event_id(path: &Path) -> Result<u64, ApiError> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(ApiError::internal(err.to_string())),
    };
    let mut reader = tokio::io::BufReader::new(file);
    let mut line = String::new();
    let mut last = 0u64;
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if n == 0 {
            break;
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<api::Event>(raw) {
            last = last.max(ev.event_id);
        }
    }
    Ok(last)
}

async fn read_events_after(path: &Path, after: u64, limit: usize) -> Result<Vec<api::Event>, ApiError> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(ApiError::internal(err.to_string())),
    };
    let mut reader = tokio::io::BufReader::new(file);
    let mut line = String::new();
    let mut out = Vec::new();
    while out.len() < limit {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if n == 0 {
            break;
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let ev: api::Event = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if ev.event_id <= after {
            continue;
        }
        out.push(ev);
    }
    Ok(out)
}

async fn append_event(path: &Path, event: &api::Event) -> Result<(), ApiError> {
    use tokio::io::AsyncWriteExt;
    let text = serde_json::to_string(event).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    file.write_all(text.as_bytes())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    file.write_all(b"\n")
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    file.flush()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

async fn write_text_atomic(path: &Path, text: &str) -> Result<(), ApiError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, text)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                let _ = tokio::fs::remove_file(path).await;
                tokio::fs::rename(&tmp, path)
                    .await
                    .map_err(|e| ApiError::internal(e.to_string()))?;
                Ok(())
            } else {
                Err(ApiError::internal(err.to_string()))
            }
        }
    }
}

async fn write_run_file(dir: &Path, run: &api::Run) -> Result<(), ApiError> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let path = dir.join(format!("{}.json", run.id));
    let text = serde_json::to_string_pretty(run).map_err(|e| ApiError::internal(e.to_string()))?;
    tokio::fs::write(&path, text)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

async fn read_run_file(dir: &Path, run_id: &str) -> Result<api::Run, ApiError> {
    let path = dir.join(format!("{run_id}.json"));
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ApiError::not_found("run not found")
            } else {
                ApiError::internal(e.to_string())
            }
        })?;
    serde_json::from_str(&text).map_err(|e| ApiError::internal(e.to_string()))
}

fn new_run_id() -> String {
    let ts = time::OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .to_string();
    let pid = std::process::id();
    format!("run_{ts}_{pid}")
}

pub struct LiveSession {
    session_id: SessionId,
    store: FileSessionStore,
    config: Arc<RwLock<Arc<Config>>>,
    runs_dir: PathBuf,

    session: Mutex<SessionState>,
    settings: Mutex<api::SessionSettings>,
    settings_dirty: AtomicBool,
    closing: AtomicBool,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,

    tools: Mutex<ToolEngine>,

    status: Mutex<api::SessionStatus>,
    queue: Mutex<VecDeque<QueuedRun>>,
    notify: Notify,
    active_cancel: Mutex<Option<watch::Sender<bool>>>,

    events_api_path: PathBuf,
    settings_path: PathBuf,
    events_tx: broadcast::Sender<api::Event>,
    next_event_id: AtomicU64,
}

#[derive(Debug, Clone)]
struct QueuedRun {
    run: api::Run,
}

impl LiveSession {
    fn config_snapshot(&self) -> Result<Arc<Config>, ApiError> {
        self.config
            .read()
            .map(|v| v.clone())
            .map_err(|_| ApiError::internal("config lock poisoned"))
    }

    pub async fn on_config_updated(&self) -> Result<(), ApiError> {
        let config = self.config_snapshot()?;
        let meta = { self.session.lock().await.meta.clone() };

        let mut settings = self.settings.lock().await.clone();
        normalize_settings(&mut settings, &meta, config.as_ref())?;

        let workspace_root = if settings.workspace_root.trim().is_empty() {
            default_tmp_workspace_root()?
        } else {
            match validate_client_workspace_root(&settings.workspace_root) {
                Ok(p) => p,
                Err(_) => default_tmp_workspace_root()?,
            }
        };
        settings.workspace_root = workspace_root.display().to_string();
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        *self.settings.lock().await = settings.clone();
        write_settings_file(&self.settings_path, &settings).await?;

        self.settings_dirty.store(true, Ordering::SeqCst);
        let is_idle = { self.status.lock().await.run_state == api::SessionRunState::Idle };
        if is_idle {
            self.apply_settings_now(true).await?;
            self.settings_dirty.store(false, Ordering::SeqCst);
        }

        Ok(())
    }

    pub fn id(&self) -> &SessionId {
        &self.session_id
    }

    pub async fn resume(server: &ServerState, session_id: &SessionId) -> Result<Arc<Self>, ApiError> {
        let config = server.config_snapshot()?;
        let session = server.store.load(session_id).await.map_err(map_session_err)?;
        let mut settings = load_settings_for_meta(&server.store, &session.meta, config.as_ref()).await?;

        if settings.workspace_root.trim().is_empty() {
            settings.workspace_root = session
                .meta
                .workspace_root
                .clone()
                .unwrap_or_else(|| server.workspace_root.display().to_string());
        }
        let workspace_root = PathBuf::from(settings.workspace_root.trim());
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        let cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
        let tools = ToolEngine::new(&workspace_root, cfg_for_tools);

        Self::from_state(server, session, settings, tools).await
    }

    pub async fn from_state(
        server: &ServerState,
        session: SessionState,
        settings: api::SessionSettings,
        tools: ToolEngine,
    ) -> Result<Arc<Self>, ApiError> {
        let events_api_path = session_events_api_path(&server.store, session.id());
        let settings_path = session_settings_path(&server.store, session.id());
        let last_event_id = read_last_event_id(&events_api_path).await?;
        let (events_tx, _) = broadcast::channel(2048);

        let live = Arc::new(Self {
            session_id: session.meta.id.clone(),
            store: server.store.clone(),
            config: server.config.clone(),
            runs_dir: server.runs_dir.clone(),
            session: Mutex::new(session),
            settings: Mutex::new(settings.clone()),
            settings_dirty: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            worker: Mutex::new(None),
            tools: Mutex::new(tools),
            status: Mutex::new(api::SessionStatus {
                session_state: api::SessionState::Live,
                run_state: api::SessionRunState::Idle,
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
            settings_path,
            events_tx,
            next_event_id: AtomicU64::new(last_event_id),
        });

        write_settings_file(&live.settings_path, &settings).await?;

        // Ensure meta reflects current defaults for compatibility with TUI resume.
        {
            let mut session = live.session.lock().await;
            session.meta.agent = settings.agent.clone();
            session.meta.model_id = Some(settings.model_id.clone());
            session.meta.workspace_root = Some(settings.workspace_root.clone());
            live.store
                .checkpoint(&mut session)
                .await
                .map_err(map_session_err)?;
        }

        live.apply_settings_now(false).await?;

        if server.runner_enabled {
            let worker = live.clone();
            let handle = tokio::spawn(async move {
                worker.worker_loop().await;
            });
            *live.worker.lock().await = Some(handle);
        }

        Ok(live)
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

    pub async fn summary(&self) -> Result<api::SessionSummary, ApiError> {
        let session = self.session.lock().await;
        let settings = self.settings.lock().await.clone();
        let status = self.status.lock().await.clone();
        let last_outcome = if session.meta.last_error.is_some() {
            api::SessionLastOutcome::Error
        } else if session.meta.last_finish_reason.is_some() {
            api::SessionLastOutcome::Done
        } else {
            api::SessionLastOutcome::None
        };
        Ok(api::SessionSummary {
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

    pub async fn snapshot(&self) -> Result<api::Session, ApiError> {
        let tools = { self.tools.lock().await.clone() };
        Ok(api::Session {
            summary: self.summary().await?,
            mcp_status: map_mcp_status(tools.mcp_status().await),
        })
    }

    pub async fn patch_settings(&self, patch: api::SessionSettingsPatch) -> Result<(), ApiError> {
        let config = self.config_snapshot()?;
        let meta = { self.session.lock().await.meta.clone() };
        let mut settings = self.settings.lock().await.clone();
        let mut patch = patch;
        if let Some(root) = patch.workspace_root.take() {
            let root = validate_client_workspace_root(&root)?;
            patch.workspace_root = Some(root.display().to_string());
        }

        apply_settings_patch(&mut settings, &patch, config.as_ref(), true)?;
        normalize_settings(&mut settings, &meta, config.as_ref())?;
        *self.settings.lock().await = settings.clone();
        write_settings_file(&self.settings_path, &settings).await?;

        self.settings_dirty.store(true, Ordering::SeqCst);

        let is_idle = { self.status.lock().await.run_state == api::SessionRunState::Idle };
        if is_idle {
            self.apply_settings_now(true).await?;
            self.settings_dirty.store(false, Ordering::SeqCst);
        }

        Ok(())
    }

    pub async fn enqueue_run(
        &self,
        runs_dir: &Path,
        req: api::RunCreateRequest,
    ) -> Result<api::Run, ApiError> {
        let input_text = match &req.input {
            api::RunInput::Text { text } => text.trim(),
        };
        if input_text.is_empty() {
            return Err(ApiError::invalid_argument("input text must not be empty"));
        }

        let run = api::Run {
            id: new_run_id(),
            session_id: self.session_id.to_string(),
            state: api::RunState::Queued,
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
                run.state = api::RunState::Cancelled;
                run.finished_at = Some(now_rfc3339());
                write_run_file(runs_dir, &run).await?;
                self.status.lock().await.queue_len = q.len();

                self.emit_event(api::Event {
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
                let idle = self.status.lock().await.run_state == api::SessionRunState::Idle;
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
            if self.settings_dirty.load(Ordering::SeqCst) {
                if self.status.lock().await.run_state == api::SessionRunState::Idle {
                    if let Err(err) = self.apply_settings_now(true).await {
                        tracing::error!("apply_settings_now error: {err}");
                    } else {
                        self.settings_dirty.store(false, Ordering::SeqCst);
                    }
                }
            }
        }
    }

    async fn run_one(&self, mut run: api::Run) -> Result<(), ApiError> {
        let span = tracing::info_span!(
            "kiliax.run",
            session_id = %self.session_id,
            run_id = %run.id,
            agent = tracing::field::Empty,
            model_id = tracing::field::Empty,
            workspace_root = tracing::field::Empty,
        );

        async {
            let config = self.config_snapshot()?;

            let (cancel_tx, mut cancel_rx) = watch::channel(false);
            *self.active_cancel.lock().await = Some(cancel_tx);

            {
                let mut st = self.status.lock().await;
                st.run_state = api::SessionRunState::Running;
                st.active_run_id = Some(run.id.clone());
                st.step = 0;
                st.active_tool = None;
            }

            run.state = api::RunState::Running;
            run.started_at = Some(now_rfc3339());
            write_run_file(&self.runs_dir, &run).await?;

            // Persist user message at execution time.
            let user_text = match &run.input {
                api::RunInput::Text { text } => text.clone(),
            };
            self.record_message(CoreMessage::User {
                content: UserMessageContent::Text(user_text),
            })
            .await?;

            let base_settings = self.settings.lock().await.clone();
            let mut overrides = run.overrides.take();
            if let Some(o) = overrides.as_mut() {
                if let Some(root) = o.workspace_root.take() {
                    let root = validate_client_workspace_root(&root)?;
                    o.workspace_root = Some(root.display().to_string());
                }
            }
            let effective =
                apply_run_overrides(&base_settings, overrides.as_ref(), config.as_ref())?;
            run.overrides = overrides;

            Span::current().record("agent", effective.agent.as_str());
            Span::current().record("model_id", effective.model_id.as_str());

            // Per-run MCP config.
            let cfg_for_run = config_with_mcp_overrides(config.as_ref(), &effective.mcp.servers)?;
            let workspace_root = PathBuf::from(effective.workspace_root.trim());
            Span::current().record(
                "workspace_root",
                tracing::field::display(workspace_root.display()),
            );
            tokio::fs::create_dir_all(&workspace_root)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;

            let tools_for_run = if effective.workspace_root != base_settings.workspace_root {
                ToolEngine::new(&workspace_root, cfg_for_run)
            } else {
                let tools = self.tools.lock().await;
                tools
                    .set_config(cfg_for_run)
                    .map_err(|e| ApiError::internal(e.to_string()))?;
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
            let runtime = AgentRuntime::new(llm, tools_for_run.clone());

            let mut messages = { self.session.lock().await.messages.clone() };
            if effective.agent != base_settings.agent
                || effective.model_id != base_settings.model_id
                || effective.mcp.servers != base_settings.mcp.servers
                || effective.workspace_root != base_settings.workspace_root
            {
                let preamble = build_preamble(
                    &profile,
                    &effective.model_id,
                    &workspace_root,
                    &tools_for_run,
                )
                .await;
                replace_preamble(&mut messages, preamble);
            }

            let stream = runtime
                .run_stream(&profile, messages, options)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;
            tokio::pin!(stream);

            let mut finish_reason: Option<String> = None;
            let mut cancelled = false;
            let mut runtime_error: Option<String> = None;

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
                                    other => runtime_error = Some(other.to_string()),
                                }
                                break;
                            }
                        }
                    }
                }
            }

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
                st.run_state = api::SessionRunState::Idle;
                st.active_run_id = None;
                st.step = 0;
                st.active_tool = None;
            }
            *self.active_cancel.lock().await = None;

            run.finished_at = Some(now_rfc3339());
            run.finish_reason = finish_reason.clone();

            if cancelled {
                run.state = api::RunState::Cancelled;
            } else if let Some(err) = runtime_error {
                run.state = api::RunState::Error;
                run.error = Some(api::RunError {
                    code: "internal".to_string(),
                    message: err.clone(),
                });
                let mut session = self.session.lock().await;
                let _ = self.store.record_error(&mut session, err).await;
            } else {
                run.state = api::RunState::Done;
                let persisted_finish_reason = finish_reason
                    .clone()
                    .or_else(|| Some("done".to_string()));
                run.finish_reason = persisted_finish_reason.clone();
                let mut session = self.session.lock().await;
                let _ = self
                    .store
                    .record_finish(&mut session, persisted_finish_reason)
                    .await;
            }

            write_run_file(&self.runs_dir, &run).await?;

            match run.state {
                api::RunState::Done => {
                    self.emit_event(api::Event {
                        event_id: self.alloc_event_id(),
                        ts: now_rfc3339(),
                        session_id: self.session_id.to_string(),
                        run_id: Some(run.id.clone()),
                        event_type: "run_done".to_string(),
                        data: serde_json::json!({ "run": run }),
                    })
                    .await?;
                }
                api::RunState::Cancelled => {
                    self.emit_event(api::Event {
                        event_id: self.alloc_event_id(),
                        ts: now_rfc3339(),
                        session_id: self.session_id.to_string(),
                        run_id: Some(run.id.clone()),
                        event_type: "run_cancelled".to_string(),
                        data: serde_json::json!({ "reason": "cancelled" }),
                    })
                    .await?;
                }
                api::RunState::Error => {
                    let err = run.error.clone().map(|e| serde_json::json!({"code": e.code, "message": e.message})).unwrap_or(serde_json::json!({"code":"internal","message":"error"}));
                    self.emit_event(api::Event {
                        event_id: self.alloc_event_id(),
                        ts: now_rfc3339(),
                        session_id: self.session_id.to_string(),
                        run_id: Some(run.id.clone()),
                        event_type: "run_error".to_string(),
                        data: serde_json::json!({ "error": err }),
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
        let config = self.config_snapshot()?;
        let settings = self.settings.lock().await.clone();

        if settings.workspace_root.trim().is_empty() {
            return Err(ApiError::invalid_argument("workspace_root must not be empty"));
        }
        let workspace_root = PathBuf::from(settings.workspace_root.trim());
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;

        let cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
        let tools = {
            let mut tools = self.tools.lock().await;
            if tools.workspace_root() != workspace_root.as_path() {
                *tools = ToolEngine::new(&workspace_root, cfg_for_tools);
            } else {
                tools
                    .set_config(cfg_for_tools)
                    .map_err(|e| ApiError::internal(e.to_string()))?;
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

        let preamble =
            build_preamble(&profile, &settings.model_id, &workspace_root, &tools).await;

        let mut session = self.session.lock().await;
        replace_preamble(&mut session.messages, preamble);
        session.meta.agent = profile.name.to_string();
        session.meta.model_id = Some(settings.model_id.clone());
        session.meta.workspace_root = Some(settings.workspace_root.clone());
        self.store
            .checkpoint(&mut session)
            .await
            .map_err(map_session_err)?;

        if emit_event {
            self.emit_event(api::Event {
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
        run: &api::Run,
        ev: AgentEvent,
    ) -> Result<Option<String>, ApiError> {
        match ev {
            AgentEvent::StepStart { step } => {
                self.status.lock().await.step = step as u32;
                self.emit_event(api::Event {
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
                self.emit_event(api::Event {
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
                self.emit_event(api::Event {
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
                self.emit_event(api::Event {
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
                let api_msg = map_core_message_to_api_event_message(seq, created_at.clone(), message)
                    .unwrap_or(api::Message::Assistant {
                        id: seq.to_string(),
                        created_at: created_at.clone(),
                        content: String::new(),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                    });
                self.emit_event(api::Event {
                    event_id: self.alloc_event_id(),
                    ts: created_at,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "assistant_message".to_string(),
                    data: serde_json::json!({ "message": api_msg }),
                })
                .await?;
            }
            AgentEvent::ToolCall { call } => {
                {
                    let mut st = self.status.lock().await;
                    st.run_state = api::SessionRunState::Tooling;
                    st.active_tool = Some(call.name.clone());
                }
                self.emit_event(api::Event {
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
                    st.run_state = api::SessionRunState::Running;
                    st.active_tool = None;
                }
                let created_at = now_rfc3339();
                let api_msg = map_core_message_to_api_event_message(seq, created_at.clone(), message)
                    .unwrap_or(api::Message::Tool {
                        id: seq.to_string(),
                        created_at: created_at.clone(),
                        tool_call_id: "".to_string(),
                        content: "".to_string(),
                    });
                self.emit_event(api::Event {
                    event_id: self.alloc_event_id(),
                    ts: created_at,
                    session_id: self.session_id.to_string(),
                    run_id: Some(run.id.clone()),
                    event_type: "tool_result".to_string(),
                    data: serde_json::json!({ "message": api_msg }),
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

    async fn emit_event(&self, event: api::Event) -> Result<(), ApiError> {
        append_event(&self.events_api_path, &event).await?;
        {
            let mut st = self.status.lock().await;
            st.last_event_id = st.last_event_id.max(event.event_id);
        }
        let _ = self.events_tx.send(event);
        Ok(())
    }
}

fn apply_run_overrides(
    base: &api::SessionSettings,
    overrides: Option<&api::RunOverrides>,
    config: &Config,
) -> Result<api::SessionSettings, ApiError> {
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
            config
                .resolve_model(model_id)
                .map_err(|e| {
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
        if let Some(root) = o.workspace_root.as_deref() {
            out.workspace_root = root.trim().to_string();
        }
    }
    if out.workspace_root.trim().is_empty() {
        out.workspace_root = base.workspace_root.clone();
    }
    Ok(out)
}

fn replace_preamble(messages: &mut Vec<CoreMessage>, new_preamble: Vec<CoreMessage>) {
    let offset = messages
        .iter()
        .take_while(|m| matches!(m, CoreMessage::System { .. }))
        .count();
    let rest = messages.get(offset..).unwrap_or(&[]).to_vec();
    let mut out = new_preamble;
    out.extend(rest);
    *messages = out;
}

async fn build_preamble(
    profile: &AgentProfile,
    model_id: &str,
    workspace_root: &PathBuf,
    tools: &ToolEngine,
) -> Vec<CoreMessage> {
    let mut builder = kiliax_core::prompt::PromptBuilder::for_agent(profile)
        .with_tools({
            let mut tool_defs = profile.tools.clone();
            tool_defs.extend(tools.extra_tool_definitions().await);
            tool_defs
        })
        .with_model_id(model_id.to_string())
        .with_workspace_root(workspace_root);
    if let Ok(skills) = kiliax_core::tools::skills::discover_skills(workspace_root) {
        builder = builder.add_skills(skills);
    }
    builder.build()
}
