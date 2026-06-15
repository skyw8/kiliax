use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use arc_swap::ArcSwap;
use axum::http::StatusCode;
use kiliax_core::agents::AgentProfile;
use kiliax_core::config::{Config, ModelConfig, ProviderApi, ProviderConfig, ReasoningEffort};
use kiliax_core::session::{FileSessionStore, SessionId};
use kiliax_core::tools::ToolEngine;
use tokio::sync::{broadcast, Mutex, Notify};

use crate::api;
use crate::error::{ApiError, ApiErrorCode};
use crate::infra::{
    client_display_path, is_tmp_workspace_root, open_external,
    validate_client_extra_workspace_roots, validate_client_workspace_root,
};

use super::domain;
use super::preamble::build_preamble;
use super::{
    apply_settings_patch, config_with_mcp_overrides, custom_tools_config_from_settings,
    default_settings, list_models, map_core_message_to_domain, map_mcp_status, map_session_err,
    mcp_status_from_settings, now_ms, read_events_after, read_last_event_id, read_run_file,
    resolve_session_settings, session_events_api_path, skills_config_from_settings,
    ts_ms_to_rfc3339, write_text_atomic, LiveSession, MultiAgentControl,
};

pub struct ServerState {
    pub workspace_root: PathBuf,
    pub config_path: PathBuf,
    pub config: Arc<ArcSwap<Config>>,
    pub token: Option<String>,

    pub store: FileSessionStore,
    pub runs_dir: PathBuf,
    pub tools_for_caps: ToolEngine,
    pub(super) multi_agent_control: Arc<MultiAgentControl>,

    pub shutdown: Arc<Notify>,
    runner_enabled: bool,
    sessions: Mutex<HashMap<String, LiveSessionEntry>>,
    session_lifecycle_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    idempotency: Mutex<HashMap<String, (String, u64)>>,
}

#[derive(Clone)]
struct LiveSessionEntry {
    live: Arc<LiveSession>,
    last_access_ms: u64,
}

fn parse_provider_api(raw: &str) -> Result<ProviderApi, ApiError> {
    match raw.trim() {
        "openai_chat_completions" | "openai-compatible" | "openai_compatible" | "openai" => {
            Ok(ProviderApi::OpenAiChatCompletions)
        }
        "openai_responses" => Ok(ProviderApi::OpenAiResponses),
        "anthropic_messages" | "anthropic" => Ok(ProviderApi::AnthropicMessages),
        other => Err(ApiError::invalid_argument(format!(
            "unsupported provider api: {other}"
        ))),
    }
}

fn model_summary(model: &ModelConfig) -> api::ConfigModelSummary {
    api::ConfigModelSummary {
        id: model.id.clone(),
        auto_compact_token_limit: model.auto_compact_token_limit,
        temperature: model.temperature,
        reasoning_effort: model.reasoning_effort.map(|v| v.as_str().to_string()),
    }
}

fn normalize_model_upsert(
    value: api::ConfigModelUpsert,
) -> Result<api::ConfigModelPatch, ApiError> {
    Ok(match value {
        api::ConfigModelUpsert::Id(id) => api::ConfigModelPatch {
            id,
            auto_compact_token_limit: None,
            temperature: None,
            reasoning_effort: None,
        },
        api::ConfigModelUpsert::Full(model) => model,
    })
}

fn apply_model_patch(
    model: &mut ModelConfig,
    patch: api::ConfigModelPatch,
) -> Result<(), ApiError> {
    model.id = patch.id.trim().to_string();
    if let Some(value) = patch.auto_compact_token_limit {
        model.auto_compact_token_limit = match value {
            None => None,
            Some(0) => {
                return Err(ApiError::invalid_argument(
                    "auto_compact_token_limit must be > 0 or null",
                ))
            }
            Some(v) => Some(v),
        };
    }
    if let Some(value) = patch.temperature {
        model.temperature = match value {
            None => None,
            Some(v) if !v.is_finite() || !(0.0..=2.0).contains(&v) => {
                return Err(ApiError::invalid_argument(
                    "temperature must be between 0 and 2 or null",
                ))
            }
            Some(v) => Some(v),
        };
    }
    if let Some(value) = patch.reasoning_effort {
        model.reasoning_effort = value.map(|v| parse_reasoning_effort(&v)).transpose()?;
    }
    Ok(())
}

fn parse_reasoning_effort(raw: &str) -> Result<ReasoningEffort, ApiError> {
    match raw.trim() {
        "none" => Ok(ReasoningEffort::None),
        "minimal" => Ok(ReasoningEffort::Minimal),
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "xhigh" => Ok(ReasoningEffort::Xhigh),
        "max" => Ok(ReasoningEffort::Max),
        other => Err(ApiError::invalid_argument(format!(
            "unsupported reasoning_effort: {other}"
        ))),
    }
}

impl ServerState {
    pub async fn new(
        workspace_root: PathBuf,
        config_path: PathBuf,
        config: Config,
        token: Option<String>,
    ) -> Result<Self, anyhow::Error> {
        let store = FileSessionStore::global().ok_or_else(|| {
            anyhow::anyhow!(
                "failed to determine home directory for sessions (expected ~/.kiliax/sessions)"
            )
        })?;
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
        let runs_dir = if runner_enabled {
            let home = dirs::home_dir().ok_or_else(|| {
                anyhow::anyhow!("failed to resolve home directory for ~/.kiliax/runs")
            })?;
            home.join(".kiliax").join("runs")
        } else {
            workspace_root.join(".kiliax").join("runs")
        };
        tokio::fs::create_dir_all(&runs_dir).await?;

        let tools_for_caps = ToolEngine::new(&workspace_root, config.clone());
        Ok(Self {
            workspace_root: workspace_root.clone(),
            config_path,
            config: Arc::new(ArcSwap::from_pointee(config)),
            token,
            store,
            runs_dir,
            tools_for_caps,
            multi_agent_control: Arc::new(MultiAgentControl::new()),
            shutdown: Arc::new(Notify::new()),
            runner_enabled,
            sessions: Mutex::new(HashMap::new()),
            session_lifecycle_locks: Mutex::new(HashMap::new()),
            idempotency: Mutex::new(HashMap::new()),
        })
    }

    pub(super) fn config_snapshot(&self) -> Arc<Config> {
        self.config.load_full()
    }

    pub(super) fn runner_enabled(&self) -> bool {
        self.runner_enabled
    }

    async fn session_lifecycle_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.session_lifecycle_locks.lock().await;
        if locks.len() > 4096 {
            locks.retain(|_, lock| lock.strong_count() > 0);
        }
        if let Some(lock) = locks.get(session_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(session_id.to_string(), Arc::downgrade(&lock));
        lock
    }

    fn default_tmp_workspace_root(&self) -> Result<PathBuf, ApiError> {
        if self.runner_enabled {
            crate::infra::default_tmp_workspace_root()
        } else {
            Ok(self
                .workspace_root
                .join(".kiliax")
                .join("workspace")
                .join(format!("tmp_{}", SessionId::new())))
        }
    }

    async fn idempotency_get(&self, key: &str) -> Option<String> {
        let cfg = self.config_snapshot();
        let max = cfg.server.idempotency_max_entries;
        if max == 0 {
            return None;
        }
        let ttl_ms = cfg.server.idempotency_ttl_secs.saturating_mul(1000);
        let now = now_ms();

        let mut map = self.idempotency.lock().await;
        if ttl_ms > 0 {
            map.retain(|_, (_, ts_ms)| now.saturating_sub(*ts_ms) <= ttl_ms);
        }
        map.get(key).map(|(v, _)| v.clone())
    }

    async fn idempotency_put(&self, key: String, value: String) {
        let cfg = self.config_snapshot();
        let max = cfg.server.idempotency_max_entries;
        let ttl_ms = cfg.server.idempotency_ttl_secs.saturating_mul(1000);
        let now = now_ms();

        let mut map = self.idempotency.lock().await;
        if max == 0 {
            map.clear();
            return;
        }
        if ttl_ms > 0 {
            map.retain(|_, (_, ts_ms)| now.saturating_sub(*ts_ms) <= ttl_ms);
        }
        map.insert(key, (value, now));

        if map.len() <= max {
            return;
        }

        let mut entries = map
            .iter()
            .map(|(k, (_, ts_ms))| (*ts_ms, k.clone()))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(ts_ms, _)| *ts_ms);
        let overflow = map.len().saturating_sub(max);
        for (_, k) in entries.into_iter().take(overflow) {
            map.remove(&k);
        }
    }

    async fn enforce_live_session_limits(&self) {
        let cfg = self.config_snapshot();
        let max = cfg.server.max_live_sessions;
        let ttl_secs = cfg.server.live_session_idle_ttl_secs;
        if max == 0 && ttl_secs == 0 {
            return;
        }

        let now = now_ms();
        let ttl_deadline_ms = ttl_secs
            .checked_mul(1000)
            .and_then(|ttl_ms| now.checked_sub(ttl_ms));

        let items = {
            let guard = self.sessions.lock().await;
            guard
                .iter()
                .map(|(id, entry)| (id.clone(), entry.live.clone(), entry.last_access_ms))
                .collect::<Vec<_>>()
        };

        let mut candidates: HashSet<String> = HashSet::new();

        if let Some(deadline) = ttl_deadline_ms {
            for (id, _, last_access_ms) in &items {
                if *last_access_ms < deadline {
                    candidates.insert(id.clone());
                }
            }
        }

        if max > 0 && items.len() > max {
            let mut sorted = items
                .iter()
                .map(|(id, _, last_access_ms)| (*last_access_ms, id.clone()))
                .collect::<Vec<_>>();
            sorted.sort_by_key(|(last_access_ms, _)| *last_access_ms);
            let overflow = items.len().saturating_sub(max);
            for (_, id) in sorted.into_iter().take(overflow) {
                candidates.insert(id);
            }
        }

        if candidates.is_empty() {
            return;
        }

        for (id, live, last_access_ms) in items {
            if !candidates.contains(&id) {
                continue;
            }

            let lifecycle = self.session_lifecycle_lock(&id).await;
            let _guard = lifecycle.lock().await;

            if !live.is_idle_for_eviction().await {
                continue;
            }

            let removed = {
                let mut guard = self.sessions.lock().await;
                match guard.get(&id) {
                    Some(entry)
                        if Arc::ptr_eq(&entry.live, &live)
                            && entry.last_access_ms == last_access_ms =>
                    {
                        guard.remove(&id)
                    }
                    _ => None,
                }
            };

            if removed.is_some() {
                live.shutdown().await;
            }
        }
    }

    pub async fn get_config(&self) -> Result<api::ConfigResponse, ApiError> {
        let yaml = match tokio::fs::read_to_string(&self.config_path).await {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(ApiError::internal_error(err)),
        };
        let config = self.config_snapshot().as_ref().clone();
        Ok(api::ConfigResponse {
            path: client_display_path(&self.config_path),
            yaml,
            config,
        })
    }

    pub async fn update_config(
        &self,
        req: api::ConfigUpdateRequest,
    ) -> Result<api::ConfigResponse, ApiError> {
        let loaded = kiliax_core::config::load_from_str_normalized(&req.yaml)
            .map_err(|e| ApiError::invalid_argument(e.to_string()))?;
        let next = loaded.config;
        let yaml = if loaded.normalized {
            serde_yaml::to_string(&next).map_err(ApiError::internal_error)?
        } else {
            req.yaml
        };

        write_text_atomic(&self.config_path, &yaml).await?;

        self.config.store(Arc::new(next.clone()));
        self.tools_for_caps
            .set_config(next.clone())
            .map_err(ApiError::internal_error)?;

        let live_sessions = {
            let guard = self.sessions.lock().await;
            guard.values().map(|s| s.live.clone()).collect::<Vec<_>>()
        };
        for live in live_sessions {
            live.on_config_updated().await?;
        }

        Ok(api::ConfigResponse {
            path: client_display_path(&self.config_path),
            yaml,
            config: next,
        })
    }

    pub async fn patch_config_mcp(&self, req: api::ConfigMcpPatchRequest) -> Result<(), ApiError> {
        let current = self.config_snapshot();
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

        let yaml = serde_yaml::to_string(&next).map_err(ApiError::internal_error)?;
        let _ = self
            .update_config(api::ConfigUpdateRequest { yaml })
            .await?;
        Ok(())
    }

    pub async fn get_config_providers(&self) -> Result<api::ConfigProvidersResponse, ApiError> {
        let config = self.config_snapshot();
        Ok(api::ConfigProvidersResponse {
            default_model: config.default_model.clone(),
            providers: config
                .providers
                .iter()
                .map(|(id, p)| api::ConfigProviderSummary {
                    id: id.clone(),
                    api: p.api.as_config_str().to_string(),
                    base_url: p.base_url.clone(),
                    api_key_set: p.api_key.is_some(),
                    models: p.models.iter().map(model_summary).collect(),
                })
                .collect(),
        })
    }

    pub async fn patch_config_providers(
        &self,
        req: api::ConfigProvidersPatchRequest,
    ) -> Result<(), ApiError> {
        let current = self.config_snapshot();
        let mut next = current.as_ref().clone();

        if let Some(v) = req.default_model {
            next.default_model = v.and_then(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            });
        }

        for id in req.delete {
            let trimmed = id.trim();
            if trimmed.is_empty() {
                return Err(ApiError::invalid_argument("provider id must not be empty"));
            }
            if next.providers.remove(trimmed).is_none() {
                return Err(ApiError::not_found(format!(
                    "provider not found: {}",
                    trimmed
                )));
            }
        }

        for upsert in req.upsert {
            let id = upsert.id.trim();
            if id.is_empty() {
                return Err(ApiError::invalid_argument("provider id must not be empty"));
            }

            if let Some(existing) = next.providers.get_mut(id) {
                if let Some(api) = upsert.api {
                    existing.api = parse_provider_api(&api)?;
                }

                if let Some(base_url) = upsert.base_url {
                    let base_url = base_url.trim();
                    if base_url.is_empty() {
                        return Err(ApiError::invalid_argument(
                            "provider base_url must not be empty",
                        ));
                    }
                    existing.base_url = base_url.to_string();
                }

                if let Some(api_key) = upsert.api_key {
                    existing.api_key = api_key
                        .map(|k| {
                            let trimmed = k.trim();
                            if trimmed.is_empty() {
                                return Err(ApiError::invalid_argument(
                                    "provider api_key must not be empty (use null to clear)",
                                ));
                            }
                            Ok(trimmed.to_string())
                        })
                        .transpose()?;
                }

                if let Some(models) = upsert.models {
                    let old_models: HashMap<String, ModelConfig> = existing
                        .models
                        .iter()
                        .cloned()
                        .map(|m| (m.id.clone(), m))
                        .collect();
                    let mut seen: HashSet<String> = HashSet::new();
                    let mut out = Vec::new();
                    for m in models {
                        let model_patch = normalize_model_upsert(m)?;
                        let trimmed = model_patch.id.trim();
                        if trimmed.is_empty() {
                            return Err(ApiError::invalid_argument(
                                "provider models must not contain empty strings",
                            ));
                        }
                        if seen.insert(trimmed.to_string()) {
                            let mut model = old_models
                                .get(trimmed)
                                .cloned()
                                .unwrap_or_else(|| ModelConfig::new(trimmed));
                            apply_model_patch(&mut model, model_patch)?;
                            out.push(model);
                        }
                    }
                    existing.models = out;
                }
            } else {
                let api = upsert
                    .api
                    .as_deref()
                    .map(parse_provider_api)
                    .transpose()?
                    .unwrap_or_default();

                let Some(base_url) = upsert.base_url else {
                    return Err(ApiError::invalid_argument(
                        "provider base_url is required for new providers",
                    ));
                };
                let base_url = base_url.trim();
                if base_url.is_empty() {
                    return Err(ApiError::invalid_argument(
                        "provider base_url must not be empty",
                    ));
                }

                let api_key = match upsert.api_key {
                    None | Some(None) => None,
                    Some(Some(k)) => {
                        let trimmed = k.trim();
                        if trimmed.is_empty() {
                            return Err(ApiError::invalid_argument(
                                "provider api_key must not be empty (use null to clear)",
                            ));
                        }
                        Some(trimmed.to_string())
                    }
                };

                let models = match upsert.models {
                    None => Vec::new(),
                    Some(models) => {
                        let mut seen: HashSet<String> = HashSet::new();
                        let mut out = Vec::new();
                        for m in models {
                            let model_patch = normalize_model_upsert(m)?;
                            let trimmed = model_patch.id.trim();
                            if trimmed.is_empty() {
                                return Err(ApiError::invalid_argument(
                                    "provider models must not contain empty strings",
                                ));
                            }
                            if seen.insert(trimmed.to_string()) {
                                let mut model = ModelConfig::new(trimmed);
                                apply_model_patch(&mut model, model_patch)?;
                                out.push(model);
                            }
                        }
                        out
                    }
                };

                next.providers.insert(
                    id.to_string(),
                    ProviderConfig {
                        api,
                        base_url: base_url.to_string(),
                        api_key,
                        models,
                    },
                );
            }
        }

        let yaml = serde_yaml::to_string(&next).map_err(ApiError::internal_error)?;
        let _ = self
            .update_config(api::ConfigUpdateRequest { yaml })
            .await?;
        Ok(())
    }

    pub async fn get_config_runtime(&self) -> Result<api::ConfigRuntimeResponse, ApiError> {
        let config = self.config_snapshot();
        Ok(api::ConfigRuntimeResponse {
            runtime_max_steps: config.runtime.max_steps,
            agents_plan_max_steps: config.agents.plan.max_steps,
            agents_general_max_steps: config.agents.general.max_steps,
        })
    }

    pub async fn patch_config_runtime(
        &self,
        req: api::ConfigRuntimePatchRequest,
    ) -> Result<(), ApiError> {
        let current = self.config_snapshot();
        let mut next = current.as_ref().clone();

        if let Some(v) = req.runtime_max_steps {
            next.runtime.max_steps = match v {
                None => None,
                Some(0) => {
                    return Err(ApiError::invalid_argument(
                        "runtime_max_steps must be > 0 or null",
                    ))
                }
                Some(n) => Some(n),
            };
        }

        if let Some(v) = req.agents_plan_max_steps {
            next.agents.plan.max_steps = match v {
                None => None,
                Some(0) => {
                    return Err(ApiError::invalid_argument(
                        "agents_plan_max_steps must be > 0 or null",
                    ))
                }
                Some(n) => Some(n),
            };
        }

        if let Some(v) = req.agents_general_max_steps {
            next.agents.general.max_steps = match v {
                None => None,
                Some(0) => {
                    return Err(ApiError::invalid_argument(
                        "agents_general_max_steps must be > 0 or null",
                    ))
                }
                Some(n) => Some(n),
            };
        }

        let yaml = serde_yaml::to_string(&next).map_err(ApiError::internal_error)?;
        let _ = self
            .update_config(api::ConfigUpdateRequest { yaml })
            .await?;
        Ok(())
    }

    pub async fn get_config_skills(&self) -> Result<api::ConfigSkillsResponse, ApiError> {
        let config = self.config_snapshot();
        Ok(api::ConfigSkillsResponse {
            default_enable: config.skills.default_enable,
            skills: config
                .skills
                .overrides
                .iter()
                .map(|(id, enable)| api::SkillEnableSetting {
                    id: id.clone(),
                    enable: *enable,
                })
                .collect(),
        })
    }

    pub async fn patch_config_skills(
        &self,
        req: api::ConfigSkillsPatchRequest,
    ) -> Result<(), ApiError> {
        let current = self.config_snapshot();
        let mut next = current.as_ref().clone();
        if let Some(v) = req.default_enable {
            next.skills.default_enable = v;
        }
        for s in req.skills {
            if s.id.trim().is_empty() {
                return Err(ApiError::invalid_argument("skill id must not be empty"));
            }
            next.skills.overrides.insert(s.id, s.enable);
        }

        let yaml = serde_yaml::to_string(&next).map_err(ApiError::internal_error)?;
        let _ = self
            .update_config(api::ConfigUpdateRequest { yaml })
            .await?;
        Ok(())
    }

    pub async fn get_config_custom_tools(
        &self,
    ) -> Result<api::ConfigCustomToolsResponse, ApiError> {
        let config = self.config_snapshot();
        Ok(api::ConfigCustomToolsResponse {
            default_enable: config.custom_tools.default_enable,
            custom_tools: config
                .custom_tools
                .overrides
                .iter()
                .map(|(id, enable)| api::CustomToolEnableSetting {
                    id: id.clone(),
                    enable: *enable,
                })
                .collect(),
        })
    }

    pub async fn patch_config_custom_tools(
        &self,
        req: api::ConfigCustomToolsPatchRequest,
    ) -> Result<(), ApiError> {
        let current = self.config_snapshot();
        let mut next = current.as_ref().clone();
        if let Some(v) = req.default_enable {
            next.custom_tools.default_enable = v;
        }
        for s in req.custom_tools {
            if s.id.trim().is_empty() {
                return Err(ApiError::invalid_argument(
                    "custom tool id must not be empty",
                ));
            }
            next.custom_tools.overrides.insert(s.id, s.enable);
        }

        let yaml = serde_yaml::to_string(&next).map_err(ApiError::internal_error)?;
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
            live.workspace_root().await
        } else {
            let state = self.store.load(session_id).await.map_err(map_session_err)?;
            let config = self.config_snapshot();
            let settings =
                resolve_session_settings(&state.meta, config.as_ref(), &self.workspace_root)?;
            settings.workspace_root
        };

        if workspace_root.as_os_str().is_empty() {
            return Ok(api::SkillListResponse {
                items: Vec::new(),
                errors: Vec::new(),
            });
        }
        let root = workspace_root;

        let discovered = kiliax_core::tools::skills::discover_skills(&root);
        Ok(api::SkillListResponse {
            items: discovered
                .items
                .into_iter()
                .map(|s| api::SkillSummary {
                    id: s.id,
                    name: s.name,
                    description: s.description,
                })
                .collect(),
            errors: discovered
                .errors
                .into_iter()
                .map(|e| api::SkillLoadError {
                    id: e.id,
                    path: client_display_path(&e.path),
                    error: e.error,
                })
                .collect(),
        })
    }

    pub async fn list_global_skills(&self) -> Result<api::SkillListResponse, ApiError> {
        let discovered = kiliax_core::tools::skills::discover_skills(&self.workspace_root);
        Ok(api::SkillListResponse {
            items: discovered
                .items
                .into_iter()
                .map(|s| api::SkillSummary {
                    id: s.id,
                    name: s.name,
                    description: s.description,
                })
                .collect(),
            errors: discovered
                .errors
                .into_iter()
                .map(|e| api::SkillLoadError {
                    id: e.id,
                    path: client_display_path(&e.path),
                    error: e.error,
                })
                .collect(),
        })
    }

    pub async fn list_custom_tools(
        &self,
        _session_id: &SessionId,
    ) -> Result<api::CustomToolListResponse, ApiError> {
        self.list_global_custom_tools().await
    }

    pub async fn list_global_custom_tools(&self) -> Result<api::CustomToolListResponse, ApiError> {
        let discovered = kiliax_core::tools::custom::list_custom_tools();
        Ok(api::CustomToolListResponse {
            items: discovered
                .items
                .into_iter()
                .map(|tool| api::CustomToolSummary {
                    id: tool.id,
                    name: tool.display_name.unwrap_or(tool.name),
                    description: Some(tool.description),
                    parallel: tool.parallel,
                })
                .collect(),
            errors: discovered
                .errors
                .into_iter()
                .map(|e| api::CustomToolLoadError {
                    id: e.id,
                    path: client_display_path(&e.path),
                    error: e.error,
                })
                .collect(),
        })
    }

    pub async fn fs_list(&self, path: Option<String>) -> Result<api::FsListResponse, ApiError> {
        let candidate = match path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(p) => validate_client_workspace_root(p)?,
            None => dirs::home_dir()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("/")),
        };
        let canonical = std::fs::canonicalize(&candidate).unwrap_or(candidate);
        let meta = tokio::fs::metadata(&canonical)
            .await
            .map_err(|_| ApiError::not_found("path not found"))?;
        if !meta.is_dir() {
            return Err(ApiError::invalid_argument("path must be a directory"));
        }

        let mut rd = tokio::fs::read_dir(&canonical)
            .await
            .map_err(ApiError::internal_error)?;
        let mut entries: Vec<api::FsEntry> = Vec::new();
        while let Some(ent) = rd.next_entry().await.map_err(ApiError::internal_error)? {
            let file_type = ent.file_type().await.map_err(ApiError::internal_error)?;
            if !file_type.is_dir() {
                continue;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            let entry_path = ent.path();
            let path = client_display_path(&entry_path);
            entries.push(api::FsEntry {
                name,
                path,
                is_dir: true,
            });
        }
        entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        Ok(api::FsListResponse {
            path: client_display_path(&canonical),
            parent: canonical.parent().map(client_display_path),
            entries,
        })
    }

    pub async fn open_workspace(
        &self,
        session_id: &SessionId,
        target: api::OpenWorkspaceTarget,
    ) -> Result<(), ApiError> {
        let settings = match self.get_live(session_id.as_str()).await {
            Some(live) => live.settings_snapshot().await,
            None => {
                let config = self.config_snapshot();
                let session = self.store.load(session_id).await.map_err(map_session_err)?;
                resolve_session_settings(&session.meta, config.as_ref(), &self.workspace_root)?
            }
        };
        if settings.workspace_root.as_os_str().is_empty() {
            return Err(ApiError::invalid_argument(
                "workspace_root must not be empty",
            ));
        }
        let root = settings.workspace_root.clone();
        open_external(&root, target).await
    }

    async fn get_live_locked(&self, session_id: &str) -> Option<Arc<LiveSession>> {
        let now = now_ms();
        let mut guard = self.sessions.lock().await;
        match guard.get_mut(session_id) {
            Some(entry) if entry.live.is_closing() => {
                guard.remove(session_id);
                None
            }
            Some(entry) => {
                entry.last_access_ms = now;
                Some(entry.live.clone())
            }
            None => None,
        }
    }

    async fn ensure_live_locked(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<LiveSession>, ApiError> {
        if let Some(live) = self.get_live_locked(session_id.as_str()).await {
            return Ok(live);
        }
        let live = LiveSession::resume(self, session_id).await?;
        self.sessions.lock().await.insert(
            session_id.to_string(),
            LiveSessionEntry {
                live: live.clone(),
                last_access_ms: now_ms(),
            },
        );
        Ok(live)
    }

    pub async fn get_live(&self, session_id: &str) -> Option<Arc<LiveSession>> {
        let lifecycle = self.session_lifecycle_lock(session_id).await;
        let _guard = lifecycle.lock().await;
        let live = self.get_live_locked(session_id).await;
        drop(_guard);
        self.enforce_live_session_limits().await;
        live
    }

    pub async fn ensure_live(&self, session_id: &SessionId) -> Result<Arc<LiveSession>, ApiError> {
        let lifecycle = self.session_lifecycle_lock(session_id.as_str()).await;
        let _guard = lifecycle.lock().await;
        let live = self.ensure_live_locked(session_id).await?;
        drop(_guard);
        self.enforce_live_session_limits().await;
        Ok(live)
    }

    pub async fn create_session(
        &self,
        idem_key: Option<String>,
        req: domain::SessionCreateRequest,
    ) -> Result<domain::SessionSnapshot, ApiError> {
        if let Some(key) = idem_key {
            let map_key = format!("POST:/v1/sessions:{key}");
            if let Some(existing) = self.idempotency_get(&map_key).await {
                let id = SessionId::parse(&existing)
                    .map_err(|e| ApiError::invalid_argument(e.to_string()))?;
                let live = self.ensure_live(&id).await?;
                return live.snapshot().await;
            }
            let created = self.create_session_inner(req).await?;
            self.idempotency_put(map_key, created.summary.id.clone())
                .await;
            return Ok(created);
        }
        self.create_session_inner(req).await
    }

    pub async fn fork_session(
        &self,
        session_id: &SessionId,
        req: domain::ForkSessionRequest,
    ) -> Result<domain::ForkSessionResponse, ApiError> {
        let config = self.config_snapshot();
        let source = self.store.load(session_id).await.map_err(map_session_err)?;
        let settings =
            resolve_session_settings(&source.meta, config.as_ref(), &self.workspace_root)?;

        let message_id = req
            .message_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());

        let initial_messages = if let Some(message_id) = message_id {
            let message_id = message_id
                .parse::<u64>()
                .map_err(|_| ApiError::invalid_argument("message_id must be a number"))?;
            if message_id == 0 {
                return Err(ApiError::invalid_argument("message_id must be >= 1"));
            }
            let idx = source
                .message_ids
                .iter()
                .position(|id| *id == message_id)
                .ok_or_else(|| ApiError::not_found("message not found"))?;
            source.messages[..=idx].to_vec()
        } else {
            source.messages.clone()
        };

        if settings.workspace_root.as_os_str().is_empty() {
            return Err(ApiError::invalid_argument(
                "workspace_root must not be empty",
            ));
        }
        let workspace_root = settings.workspace_root.clone();
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(ApiError::internal_error)?;

        let mut cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
        cfg_for_tools.custom_tools = custom_tools_config_from_settings(&settings.custom_tools);
        let tools = ToolEngine::new(&workspace_root, cfg_for_tools);
        tools
            .set_extra_workspace_roots(settings.extra_workspace_roots.clone())
            .map_err(ApiError::internal_error)?;

        let mut forked = self
            .store
            .create(
                settings.agent.clone(),
                Some(settings.model_id.clone()),
                Some(client_display_path(&self.config_path)),
                Some(client_display_path(&settings.workspace_root)),
                settings
                    .extra_workspace_roots
                    .iter()
                    .map(|p| client_display_path(p))
                    .collect(),
                initial_messages,
            )
            .await
            .map_err(map_session_err)?;

        if let Some(key) = source
            .meta
            .prompt_cache_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            forked.meta.prompt_cache_key = Some(key.to_string());
        }

        let live = LiveSession::from_state(self, forked, settings, tools, false).await?;
        self.sessions.lock().await.insert(
            live.id().to_string(),
            LiveSessionEntry {
                live: live.clone(),
                last_access_ms: now_ms(),
            },
        );
        self.enforce_live_session_limits().await;

        Ok(domain::ForkSessionResponse {
            session: live.snapshot().await?,
        })
    }

    async fn create_session_inner(
        &self,
        req: domain::SessionCreateRequest,
    ) -> Result<domain::SessionSnapshot, ApiError> {
        let config = self.config_snapshot();

        let mut settings = default_settings(config.as_ref(), None)?;
        let mut extra_workspace_roots: Option<Vec<String>> = None;
        if let Some(create) = req.settings {
            if let Some(root) = create.workspace_root.as_deref() {
                let root = validate_client_workspace_root(root)?;
                settings.workspace_root = root;
            }
            extra_workspace_roots = create.extra_workspace_roots;

            let patch = domain::SessionSettingsPatch {
                agent: create.agent,
                model_id: create.model_id,
                skills: create.skills,
                custom_tools: create.custom_tools,
                mcp: create.mcp,
                extra_workspace_roots: None,
            };
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

        if settings.workspace_root.as_os_str().is_empty() {
            settings.workspace_root = self.default_tmp_workspace_root()?;
        }
        let workspace_root = settings.workspace_root.clone();
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .map_err(ApiError::internal_error)?;

        if let Some(extra) = extra_workspace_roots.as_ref() {
            settings.extra_workspace_roots =
                validate_client_extra_workspace_roots(extra, &workspace_root)?;
        }

        let mut cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
        cfg_for_tools.custom_tools = custom_tools_config_from_settings(&settings.custom_tools);
        let tools = ToolEngine::new(&workspace_root, cfg_for_tools);
        tools
            .set_extra_workspace_roots(settings.extra_workspace_roots.clone())
            .map_err(ApiError::internal_error)?;

        let skills_config = skills_config_from_settings(&settings.skills);
        let project_prompt = kiliax_core::prompt::capture_project_prompt(Some(&workspace_root))
            .or_else(|| Some(String::new()));
        let messages = build_preamble(
            &profile,
            &settings.model_id,
            &workspace_root,
            project_prompt.clone(),
            &tools,
            &skills_config,
        )
        .await;

        let mut session = self
            .store
            .create(
                profile.name.to_string(),
                Some(settings.model_id.clone()),
                Some(client_display_path(&self.config_path)),
                Some(client_display_path(&settings.workspace_root)),
                settings
                    .extra_workspace_roots
                    .iter()
                    .map(|p| client_display_path(p))
                    .collect(),
                messages.clone(),
            )
            .await
            .map_err(map_session_err)?;

        let created_session_id = session.meta.id.clone();
        let created_agent = settings.agent.clone();
        let created_model_id = settings.model_id.clone();
        let created_workspace_root = settings.workspace_root.clone();
        session.meta.project_prompt = project_prompt;
        session.meta.custom_tools = Some(custom_tools_config_from_settings(&settings.custom_tools));

        if let Some(title) = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            session.meta.title = Some(title.to_string());
        }
        self.store
            .checkpoint(&mut session)
            .await
            .map_err(map_session_err)?;

        let live = LiveSession::from_state(self, session, settings, tools, true).await?;
        self.sessions.lock().await.insert(
            live.id().to_string(),
            LiveSessionEntry {
                live: live.clone(),
                last_access_ms: now_ms(),
            },
        );
        self.enforce_live_session_limits().await;

        tracing::info!(
            event = "session.created",
            session_id = %created_session_id,
            agent = %created_agent,
            model_id = %created_model_id,
            workspace_root = %created_workspace_root.display(),
        );
        live.snapshot().await
    }

    pub async fn list_sessions(
        &self,
        live_only: bool,
        limit: usize,
        cursor: Option<String>,
    ) -> Result<domain::SessionList, ApiError> {
        let config = self.config_snapshot();

        let limit = limit.clamp(1, 200);
        let offset = cursor
            .as_deref()
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0);

        let mut items: Vec<domain::SessionSummary> = Vec::new();
        if live_only {
            let live = self.sessions.lock().await;
            for s in live.values() {
                items.push(s.live.summary().await?);
            }
        } else {
            for meta in self.store.list().await.map_err(map_session_err)? {
                let id = meta.id.to_string();
                if let Some(live) = self.get_live(&id).await {
                    items.push(live.summary().await?);
                    continue;
                }

                let settings =
                    resolve_session_settings(&meta, config.as_ref(), &self.workspace_root)?;
                let last_event_id =
                    read_last_event_id(&session_events_api_path(&self.store, &meta.id)).await?;
                let last_outcome = if meta.last_error.is_some() {
                    domain::SessionLastOutcome::Error
                } else if meta.last_finish_reason.is_some() {
                    domain::SessionLastOutcome::Done
                } else {
                    domain::SessionLastOutcome::None
                };

                items.push(domain::SessionSummary {
                    id: id.clone(),
                    title: meta.title.clone().unwrap_or_else(|| id.clone()),
                    created_at: ts_ms_to_rfc3339(meta.created_at_ms),
                    updated_at: ts_ms_to_rfc3339(meta.updated_at_ms),
                    last_outcome,
                    status: domain::SessionStatus {
                        run_state: domain::SessionRunState::Idle,
                        active_run_id: None,
                        active_run_started_at: None,
                        step: 0,
                        active_tool: None,
                        retry_status: None,
                        queue_len: 0,
                        last_event_id,
                    },
                    settings,
                    goal: meta.goal.clone(),
                });
            }
        }

        let total = items.len();
        let items = items
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let next_cursor = if offset + limit < total {
            Some((offset + limit).to_string())
        } else {
            None
        };

        Ok(domain::SessionList { items, next_cursor })
    }

    pub async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<domain::SessionSnapshot, ApiError> {
        let config = self.config_snapshot();

        if let Some(live) = self.get_live(session_id.as_str()).await {
            return live.snapshot().await;
        }

        let state = self.store.load(session_id).await.map_err(map_session_err)?;
        let settings =
            resolve_session_settings(&state.meta, config.as_ref(), &self.workspace_root)?;
        let last_event_id =
            read_last_event_id(&session_events_api_path(&self.store, session_id)).await?;
        let last_outcome = if state.meta.last_error.is_some() {
            domain::SessionLastOutcome::Error
        } else if state.meta.last_finish_reason.is_some() {
            domain::SessionLastOutcome::Done
        } else {
            domain::SessionLastOutcome::None
        };

        Ok(domain::SessionSnapshot {
            summary: domain::SessionSummary {
                id: session_id.to_string(),
                title: state
                    .meta
                    .title
                    .clone()
                    .unwrap_or_else(|| session_id.to_string()),
                created_at: ts_ms_to_rfc3339(state.meta.created_at_ms),
                updated_at: ts_ms_to_rfc3339(state.meta.updated_at_ms),
                last_outcome,
                status: domain::SessionStatus {
                    run_state: domain::SessionRunState::Idle,
                    active_run_id: None,
                    active_run_started_at: None,
                    step: 0,
                    active_tool: None,
                    retry_status: None,
                    queue_len: 0,
                    last_event_id,
                },
                settings: settings.clone(),
                goal: state.meta.goal.clone(),
            },
            mcp_status: mcp_status_from_settings(&settings, config.as_ref()),
            stream: None,
        })
    }

    async fn workspace_root_in_use(&self, root: &Path) -> Result<bool, ApiError> {
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        for meta in self.store.list().await.map_err(map_session_err)? {
            let Some(ws) = meta.workspace_root.as_deref() else {
                continue;
            };
            let ws = ws.trim();
            if ws.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(ws);
            let candidate = std::fs::canonicalize(&candidate).unwrap_or(candidate);
            if candidate == canonical_root {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn maybe_delete_tmp_workspace_root(&self, root: &Path) -> Result<(), ApiError> {
        if !is_tmp_workspace_root(root)? {
            return Ok(());
        }
        if self.workspace_root_in_use(root).await? {
            return Ok(());
        }
        match tokio::fs::remove_dir_all(root).await {
            Ok(()) => tracing::info!(event = "workspace.tmp_deleted", path = %root.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(
                event = "workspace.tmp_delete_failed",
                path = %root.display(),
                error = %err
            ),
        }
        Ok(())
    }

    pub async fn delete_session(
        &self,
        session_id: &SessionId,
        delete_workspace_root: bool,
    ) -> Result<(), ApiError> {
        let lifecycle = self.session_lifecycle_lock(session_id.as_str()).await;
        let _guard = lifecycle.lock().await;
        let workspace_root = if delete_workspace_root {
            if let Some(live) = self.get_live_locked(session_id.as_str()).await {
                Some(live.workspace_root().await)
            } else {
                let state = self.store.load(session_id).await.map_err(map_session_err)?;
                let root = state.meta.workspace_root.unwrap_or_default();
                let root = root.trim().to_string();
                if root.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(root))
                }
            }
        } else {
            None
        };

        let removed = self.sessions.lock().await.remove(session_id.as_str());
        if let Some(live) = removed.map(|e| e.live) {
            live.shutdown().await;
        }
        self.store
            .delete(session_id)
            .await
            .map_err(map_session_err)?;

        if let Some(root) = workspace_root {
            self.maybe_delete_tmp_workspace_root(&root).await?;
        }
        Ok(())
    }

    pub async fn patch_session_settings(
        &self,
        session_id: &SessionId,
        patch: domain::SessionSettingsPatch,
    ) -> Result<domain::SessionSnapshot, ApiError> {
        let live = self.ensure_live(session_id).await?;
        live.patch_settings(patch).await?;
        live.snapshot().await
    }

    pub async fn get_goal(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<domain::SessionGoal>, ApiError> {
        if let Some(live) = self.get_live(session_id.as_str()).await {
            return Ok(live.goal().await);
        }
        let state = self.store.load(session_id).await.map_err(map_session_err)?;
        Ok(state.meta.goal)
    }

    pub async fn set_goal(
        &self,
        session_id: &SessionId,
        objective: String,
    ) -> Result<domain::SessionGoal, ApiError> {
        let live = self.ensure_live(session_id).await?;
        live.set_goal(objective).await
    }

    pub async fn clear_goal(&self, session_id: &SessionId) -> Result<(), ApiError> {
        let live = self.ensure_live(session_id).await?;
        live.clear_goal().await
    }

    pub async fn save_session_defaults(
        &self,
        session_id: &SessionId,
        req: domain::SessionSaveDefaultsRequest,
    ) -> Result<(), ApiError> {
        let live = self.ensure_live(session_id).await?;
        let settings = live.settings_snapshot().await;
        let current = self.config_snapshot();
        let mut next = current.as_ref().clone();

        if req.model {
            current.resolve_model(&settings.model_id).map_err(|e| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    ApiErrorCode::ModelNotSupported,
                    e.to_string(),
                )
            })?;
            next.default_model = Some(settings.model_id.clone());
        }

        if req.agent {
            let profile = AgentProfile::from_name(&settings.agent).ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    ApiErrorCode::AgentNotSupported,
                    "agent not supported",
                )
            })?;
            next.default_agent = Some(profile.name.to_string());
        }

        if req.mcp {
            let by_id: HashMap<&str, bool> = settings
                .mcp
                .servers
                .iter()
                .map(|server| (server.id.as_str(), server.enable))
                .collect();
            for server in &mut next.mcp.servers {
                if let Some(enable) = by_id.get(server.name.as_str()) {
                    server.enable = *enable;
                }
            }
        }

        if req.skills {
            next.skills = skills_config_from_settings(&settings.skills);
        }

        if req.custom_tools {
            next.custom_tools = custom_tools_config_from_settings(&settings.custom_tools);
        }

        let yaml = serde_yaml::to_string(&next).map_err(ApiError::internal_error)?;
        let _ = self
            .update_config(api::ConfigUpdateRequest { yaml })
            .await?;
        Ok(())
    }

    pub async fn create_run(
        &self,
        session_id: &SessionId,
        idem_key: Option<String>,
        req: domain::RunCreateRequest,
    ) -> Result<domain::Run, ApiError> {
        if let Some(key) = idem_key {
            let map_key = format!("POST:/v1/sessions/{session_id}/runs:{key}");
            if let Some(existing) = self.idempotency_get(&map_key).await {
                return self.get_run(&existing).await;
            }
            let run = self.create_run_inner(session_id, req).await?;
            self.idempotency_put(map_key, run.id.clone()).await;
            return Ok(run);
        }
        self.create_run_inner(session_id, req).await
    }

    async fn create_run_inner(
        &self,
        session_id: &SessionId,
        req: domain::RunCreateRequest,
    ) -> Result<domain::Run, ApiError> {
        let lifecycle = self.session_lifecycle_lock(session_id.as_str()).await;
        let _guard = lifecycle.lock().await;
        let live = if req.auto_resume {
            self.ensure_live_locked(session_id).await?
        } else {
            match self.get_live_locked(session_id.as_str()).await {
                Some(live) => live,
                None => {
                    self.ensure_on_disk_session_exists(session_id).await?;
                    return Err(ApiError::session_not_live("session is not live"));
                }
            }
        };
        let run = live.enqueue_run(&self.runs_dir, req).await?;
        drop(_guard);
        self.enforce_live_session_limits().await;
        Ok(run)
    }

    pub async fn get_run(&self, run_id: &str) -> Result<domain::Run, ApiError> {
        read_run_file(&self.runs_dir, run_id).await
    }

    pub async fn cancel_run(&self, run_id: &str) -> Result<domain::Run, ApiError> {
        let run = read_run_file(&self.runs_dir, run_id).await?;
        let session_id = SessionId::parse(&run.session_id)
            .map_err(|e| ApiError::invalid_argument(e.to_string()))?;
        let lifecycle = self.session_lifecycle_lock(session_id.as_str()).await;
        let _guard = lifecycle.lock().await;
        let live = self
            .get_live_locked(session_id.as_str())
            .await
            .ok_or_else(|| ApiError::session_not_live("session is not live"))?;

        live.cancel_run(&self.runs_dir, run_id).await?;
        drop(_guard);
        self.get_run(run_id).await
    }
    pub async fn get_messages(
        &self,
        session_id: &SessionId,
        limit: usize,
        before: Option<String>,
    ) -> Result<domain::MessageList, ApiError> {
        let limit = limit.clamp(1, 200);
        let before_seq = before.as_deref().and_then(|v| v.parse::<u64>().ok());

        let page = self
            .store
            .read_message_page(session_id, limit, before_seq)
            .await
            .map_err(map_session_err)?;

        let mut out: Vec<domain::Message> = Vec::new();
        for entry in page.items {
            let Some(msg) = map_core_message_to_domain(entry.seq, entry.ts_ms, entry.message)
            else {
                continue;
            };
            out.push(msg);
        }

        Ok(domain::MessageList {
            items: out,
            next_before: page.next_before,
        })
    }

    pub async fn get_capabilities(&self) -> Result<api::Capabilities, ApiError> {
        let config = self.config_snapshot();
        let (agents, agent_errors) = AgentProfile::list_names_with_errors();
        Ok(api::Capabilities {
            agents,
            agent_errors: agent_errors
                .into_iter()
                .map(|e| api::AgentLoadError {
                    id: e.id,
                    path: client_display_path(&e.path),
                    error: e.error,
                })
                .collect(),
            models: list_models(config.as_ref()),
            builtin_tools: kiliax_core::tools::builtin::BuiltinToolId::ALL
                .into_iter()
                .map(|id| {
                    let definition = id.definition();
                    api::BuiltinToolSummary {
                        id: id.name().to_string(),
                        name: definition.name,
                        description: definition.description,
                    }
                })
                .collect(),
            mcp_servers: map_mcp_status(self.tools_for_caps.mcp_status().await)
                .into_iter()
                .map(mcp_status_to_api)
                .collect(),
        })
    }

    pub async fn list_events(
        &self,
        session_id: &SessionId,
        limit: usize,
        after: Option<u64>,
    ) -> Result<domain::EventList, ApiError> {
        self.ensure_on_disk_session_exists(session_id).await?;
        let limit = limit.clamp(1, 200);
        let after = after.unwrap_or(0);
        let path = session_events_api_path(&self.store, session_id);
        let events = read_events_after(&path, after, limit).await?;
        let next_after = events.last().map(|e| e.event_id);
        Ok(domain::EventList {
            items: events,
            next_after,
        })
    }

    pub async fn events_backlog_after(
        &self,
        session_id: &SessionId,
        after_event_id: u64,
        limit: usize,
    ) -> Result<Vec<domain::Event>, ApiError> {
        self.ensure_on_disk_session_exists(session_id).await?;

        if let Some(live) = self.get_live(session_id.as_str()).await {
            if let Some(events) = live.backlog_after(after_event_id, limit).await {
                return Ok(events);
            }
        }

        let path = session_events_api_path(&self.store, session_id);
        read_events_after(&path, after_event_id, limit).await
    }

    pub async fn live_events_stream(
        &self,
        session_id: &SessionId,
    ) -> Result<broadcast::Receiver<domain::Event>, ApiError> {
        let live = self.ensure_live(session_id).await?;
        Ok(live.subscribe_events())
    }

    async fn ensure_on_disk_session_exists(&self, session_id: &SessionId) -> Result<(), ApiError> {
        let path = self.store.meta_path(session_id);
        tokio::fs::metadata(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ApiError::not_found("session not found")
            } else {
                ApiError::internal_error(e)
            }
        })?;
        Ok(())
    }
}

fn mcp_status_to_api(status: domain::McpServerStatus) -> api::McpServerStatus {
    api::McpServerStatus {
        id: status.id,
        enable: status.enable,
        state: match status.state {
            domain::McpConnectionState::Disabled => api::McpConnectionState::Disabled,
            domain::McpConnectionState::Connecting => api::McpConnectionState::Connecting,
            domain::McpConnectionState::Connected => api::McpConnectionState::Connected,
            domain::McpConnectionState::Error => api::McpConnectionState::Error,
        },
        last_error: status.last_error,
        tools: status.tools,
    }
}
