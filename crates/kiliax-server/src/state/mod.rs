pub mod domain;
mod live_session;
mod preamble;
mod server_state;

pub use live_session::LiveSession;
pub use server_state::ServerState;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use kiliax_core::agents::AgentProfile;
use kiliax_core::config::Config;
use kiliax_core::protocol::{Message as CoreMessage, UserContentPart, UserMessageContent};
use kiliax_core::runtime::AgentRuntimeError;
use kiliax_core::session::{FileSessionStore, SessionError, SessionId, SessionMeta};
use kiliax_core::tools::McpServerConnectionState;
use tokio::io::AsyncBufReadExt;

use crate::error::{ApiError, ApiErrorCode};
use crate::infra::validate_client_workspace_root;

fn map_session_err(err: SessionError) -> ApiError {
    match err {
        SessionError::NotFound(_) => ApiError::not_found("session not found"),
        SessionError::InvalidId(msg) => ApiError::invalid_argument(msg),
        other => ApiError::internal_error(other),
    }
}

fn resolve_session_settings(
    meta: &SessionMeta,
    config: &Config,
    fallback_workspace_root: &Path,
) -> Result<domain::SessionSettings, ApiError> {
    let agent = AgentProfile::from_name(&meta.agent)
        .or_else(|| {
            config
                .default_agent
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .and_then(AgentProfile::from_name)
        })
        .unwrap_or_else(AgentProfile::general)
        .name
        .to_string();

    let mut model_id = meta
        .model_id
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .or_else(|| config.default_model.clone())
        .ok_or_else(|| ApiError::invalid_argument("missing model id"))?;

    if config.resolve_model(&model_id).is_err() {
        model_id = config
            .default_model
            .clone()
            .ok_or_else(|| ApiError::invalid_argument("missing default_model in config"))?;
    }

    let workspace_root = meta
        .workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback_workspace_root.to_path_buf());

    let main_root = workspace_root.clone();
    let main_root = std::fs::canonicalize(&main_root).unwrap_or_else(|_| main_root.to_path_buf());
    let mut extras: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for raw in meta.extra_workspace_roots.iter() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let candidate = match validate_client_workspace_root(trimmed) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let meta = match std::fs::metadata(&candidate) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        let canonical = match std::fs::canonicalize(&candidate) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if canonical == main_root {
            continue;
        }
        if seen.insert(canonical.clone()) {
            extras.push(canonical);
        }
    }

    let skills = meta.skills.as_ref().unwrap_or(&config.skills);
    let skills = skills_settings_from_config(skills);

    let by_id: HashMap<&str, bool> = meta
        .mcp_servers
        .iter()
        .map(|s| (s.id.as_str(), s.enable))
        .collect();
    let servers = config
        .mcp
        .servers
        .iter()
        .map(|s| domain::McpServerSetting {
            id: s.name.clone(),
            enable: by_id.get(s.name.as_str()).copied().unwrap_or(s.enable),
        })
        .collect();

    Ok(domain::SessionSettings {
        agent,
        model_id,
        skills,
        mcp: domain::McpServers { servers },
        workspace_root,
        extra_workspace_roots: extras,
    })
}

fn session_events_api_path(store: &FileSessionStore, session_id: &SessionId) -> PathBuf {
    store.session_dir(session_id).join("events_api.jsonl")
}

fn default_settings(
    config: &Config,
    meta: Option<&SessionMeta>,
) -> Result<domain::SessionSettings, ApiError> {
    let agent = meta
        .and_then(|m| AgentProfile::from_name(&m.agent))
        .or_else(|| {
            config
                .default_agent
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .and_then(AgentProfile::from_name)
        })
        .unwrap_or_else(AgentProfile::general)
        .name
        .to_string();

    let model_id = meta
        .and_then(|m| m.model_id.clone())
        .or_else(|| config.default_model.clone())
        .ok_or_else(|| {
            ApiError::invalid_argument("missing model id (set default_model in config)")
        })?;

    let workspace_root = meta
        .and_then(|m| m.workspace_root.clone())
        .unwrap_or_default();
    let workspace_root = PathBuf::from(workspace_root.trim());

    let extra_workspace_roots = meta
        .map(|m| m.extra_workspace_roots.clone())
        .unwrap_or_default();
    let extra_workspace_roots = extra_workspace_roots
        .into_iter()
        .map(|p| PathBuf::from(p.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect::<Vec<_>>();

    let skills = match meta.and_then(|m| m.skills.as_ref()) {
        Some(skills) => skills_settings_from_config(skills),
        None => skills_settings_from_config(&config.skills),
    };

    let servers = config
        .mcp
        .servers
        .iter()
        .map(|s| domain::McpServerSetting {
            id: s.name.clone(),
            enable: s.enable,
        })
        .collect();

    Ok(domain::SessionSettings {
        agent,
        model_id,
        skills,
        mcp: domain::McpServers { servers },
        workspace_root,
        extra_workspace_roots,
    })
}

fn skills_settings_from_config(
    skills: &kiliax_core::config::SkillsConfig,
) -> domain::SkillsSettings {
    domain::SkillsSettings {
        default_enable: skills.default_enable,
        overrides: skills
            .overrides
            .iter()
            .map(|(id, enable)| domain::SkillEnableSetting {
                id: id.clone(),
                enable: *enable,
            })
            .collect(),
    }
}

fn skills_config_from_settings(
    skills: &domain::SkillsSettings,
) -> kiliax_core::config::SkillsConfig {
    let mut overrides: BTreeMap<String, bool> = BTreeMap::new();
    for s in &skills.overrides {
        let id = s.id.trim();
        if id.is_empty() {
            continue;
        }
        overrides.insert(id.to_string(), s.enable);
    }
    kiliax_core::config::SkillsConfig {
        default_enable: skills.default_enable,
        overrides,
    }
}

fn apply_settings_patch(
    settings: &mut domain::SessionSettings,
    patch: &domain::SessionSettingsPatch,
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
        config.resolve_model(model_id).map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ApiErrorCode::ModelNotSupported,
                e.to_string(),
            )
        })?;
        settings.model_id = model_id.to_string();
    }
    if let Some(skills) = patch.skills.as_ref() {
        if let Some(v) = skills.default_enable {
            settings.skills.default_enable = v;
        }
        if let Some(overrides) = skills.overrides.as_ref() {
            merge_skill_overrides(&mut settings.skills.overrides, overrides)?;
        }
    }
    if let Some(patch_servers) = patch.mcp.as_ref().and_then(|m| m.servers.as_ref()) {
        merge_mcp_settings(
            &mut settings.mcp.servers,
            patch_servers,
            config,
            allow_enable,
        )?;
    }
    if let Some(roots) = patch.extra_workspace_roots.as_ref() {
        settings.extra_workspace_roots = roots
            .iter()
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
    }
    Ok(())
}

fn merge_skill_overrides(
    existing: &mut Vec<domain::SkillEnableSetting>,
    patch: &[domain::SkillEnableSetting],
) -> Result<(), ApiError> {
    let mut map: HashMap<String, bool> =
        existing.iter().map(|s| (s.id.clone(), s.enable)).collect();

    for p in patch {
        let id = p.id.trim();
        if id.is_empty() {
            return Err(ApiError::invalid_argument("skill id must not be empty"));
        }
        map.insert(id.to_string(), p.enable);
    }

    let mut entries: Vec<(String, bool)> = map.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    existing.clear();
    existing.extend(
        entries
            .into_iter()
            .map(|(id, enable)| domain::SkillEnableSetting { id, enable }),
    );
    Ok(())
}

fn merge_mcp_settings(
    existing: &mut Vec<domain::McpServerSetting>,
    patch: &[domain::McpServerSetting],
    config: &Config,
    allow_enable: bool,
) -> Result<(), ApiError> {
    let known: HashSet<&str> = config.mcp.servers.iter().map(|s| s.name.as_str()).collect();
    let mut map: HashMap<String, bool> =
        existing.iter().map(|s| (s.id.clone(), s.enable)).collect();

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
        existing.push(domain::McpServerSetting {
            id: server.name.clone(),
            enable,
        });
    }
    Ok(())
}

fn config_with_mcp_overrides(
    base: &Config,
    servers: &[domain::McpServerSetting],
) -> Result<Config, ApiError> {
    let mut cfg = base.clone();
    kiliax_core::mcp_overrides::apply_mcp_enable_overrides(
        &mut cfg,
        servers.iter().map(|s| (s.id.as_str(), s.enable)),
    )
    .map_err(|err| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            ApiErrorCode::McpServerNotFound,
            err.to_string(),
        )
    })?;
    Ok(cfg)
}

fn mcp_status_from_settings(
    settings: &domain::SessionSettings,
    config: &Config,
) -> Vec<domain::McpServerStatus> {
    let by_id: HashMap<String, bool> = settings
        .mcp
        .servers
        .iter()
        .map(|s| (s.id.clone(), s.enable))
        .collect();
    config
        .mcp
        .servers
        .iter()
        .map(|s| {
            let enable = by_id.get(&s.name).copied().unwrap_or(s.enable);
            domain::McpServerStatus {
                id: s.name.clone(),
                enable,
                state: if enable {
                    domain::McpConnectionState::Error
                } else {
                    domain::McpConnectionState::Disabled
                },
                last_error: None,
                tools: None,
            }
        })
        .collect()
}

fn map_mcp_status(
    status: Vec<kiliax_core::tools::McpServerStatus>,
) -> Vec<domain::McpServerStatus> {
    status
        .into_iter()
        .map(|s| {
            let (state, last_error) = match s.state {
                McpServerConnectionState::Disabled => (domain::McpConnectionState::Disabled, None),
                McpServerConnectionState::Connecting => {
                    (domain::McpConnectionState::Connecting, None)
                }
                McpServerConnectionState::Connected => {
                    (domain::McpConnectionState::Connected, None)
                }
                McpServerConnectionState::Retry { error, .. } => {
                    (domain::McpConnectionState::Error, Some(error))
                }
                McpServerConnectionState::Disconnected => (domain::McpConnectionState::Error, None),
            };
            domain::McpServerStatus {
                id: s.name,
                enable: state != domain::McpConnectionState::Disabled,
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

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn user_content_attachments(content: &UserMessageContent) -> Vec<domain::MessageAttachment> {
    let UserMessageContent::Parts(parts) = content else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|part| match part {
            UserContentPart::Text { .. } => None,
            UserContentPart::Image { path, filename, .. } => Some(domain::MessageAttachment {
                filename: filename
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| image_label_from_path(path)),
                media_type: image_media_type_from_path(path)
                    .unwrap_or("image/*")
                    .to_string(),
            }),
            UserContentPart::File {
                filename,
                media_type,
                ..
            } => Some(domain::MessageAttachment {
                filename: filename.clone(),
                media_type: media_type.clone(),
            }),
        })
        .collect()
}

fn image_label_from_path(path: &str) -> String {
    if path.trim().starts_with("data:") {
        return "image".to_string();
    }
    Path::new(path)
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("image")
        .to_string()
}

fn image_media_type_from_path(path: &str) -> Option<&'static str> {
    if let Some(rest) = path.trim().strip_prefix("data:") {
        let media_type = rest.split_once(',')?.0.split_once(';')?.0.trim();
        return match media_type {
            "image/png" => Some("image/png"),
            "image/jpeg" => Some("image/jpeg"),
            "image/gif" => Some("image/gif"),
            "image/webp" => Some("image/webp"),
            _ => None,
        };
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|v| v.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn map_core_message_to_domain(seq: u64, ts_ms: u64, msg: CoreMessage) -> Option<domain::Message> {
    let id = seq.to_string();
    let created_at = ts_ms_to_rfc3339(ts_ms);
    match msg {
        CoreMessage::User { content } => {
            let text = content.first_text().unwrap_or("");
            if kiliax_core::compact::is_summary_message(text) {
                None
            } else {
                let attachments = user_content_attachments(&content);
                Some(domain::Message::User {
                    id,
                    created_at,
                    content: text.to_string(),
                    attachments,
                })
            }
        }
        CoreMessage::Assistant {
            content,
            reasoning_content,
            tool_calls,
            usage,
            ..
        } => Some(domain::Message::Assistant {
            id,
            created_at,
            content: content.unwrap_or_default(),
            reasoning_content,
            tool_calls: tool_calls
                .into_iter()
                .map(|c| domain::ToolCall {
                    id: c.id,
                    name: c.name,
                    arguments: c.arguments,
                })
                .collect(),
            usage: usage.map(domain::TokenUsage::from),
        }),
        CoreMessage::Tool {
            tool_call_id,
            content,
        } => Some(domain::Message::Tool {
            id,
            created_at,
            tool_call_id,
            content,
        }),
        CoreMessage::System { .. } | CoreMessage::Developer { .. } => None,
    }
}

fn map_core_message_to_domain_event_message(
    seq: u64,
    created_at: String,
    msg: CoreMessage,
) -> Option<domain::Message> {
    let id = seq.to_string();
    match msg {
        CoreMessage::User { content } => {
            let text = content.first_text().unwrap_or("");
            if kiliax_core::compact::is_summary_message(text) {
                None
            } else {
                let attachments = user_content_attachments(&content);
                Some(domain::Message::User {
                    id,
                    created_at,
                    content: text.to_string(),
                    attachments,
                })
            }
        }
        CoreMessage::Assistant {
            content,
            reasoning_content,
            tool_calls,
            usage,
            ..
        } => Some(domain::Message::Assistant {
            id,
            created_at,
            content: content.unwrap_or_default(),
            reasoning_content,
            tool_calls: tool_calls
                .into_iter()
                .map(|c| domain::ToolCall {
                    id: c.id,
                    name: c.name,
                    arguments: c.arguments,
                })
                .collect(),
            usage: usage.map(domain::TokenUsage::from),
        }),
        CoreMessage::Tool {
            tool_call_id,
            content,
        } => Some(domain::Message::Tool {
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
        Err(err) => return Err(ApiError::internal_error(err)),
    };
    let mut reader = tokio::io::BufReader::new(file);
    let mut line = String::new();
    let mut last = 0u64;
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(ApiError::internal_error)?;
        if n == 0 {
            break;
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<domain::Event>(raw) {
            last = last.max(ev.event_id);
        }
    }
    Ok(last)
}

async fn read_events_after(
    path: &Path,
    after: u64,
    limit: usize,
) -> Result<Vec<domain::Event>, ApiError> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(ApiError::internal_error(err)),
    };
    let mut reader = tokio::io::BufReader::new(file);
    let mut line = String::new();
    let mut out = Vec::new();
    while out.len() < limit {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(ApiError::internal_error)?;
        if n == 0 {
            break;
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let ev: domain::Event = match serde_json::from_str(raw) {
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

async fn append_event(path: &Path, event: &domain::Event) -> Result<(), ApiError> {
    use tokio::io::AsyncWriteExt;
    let text = serde_json::to_string(event).map_err(ApiError::internal_error)?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(ApiError::internal_error)?;
    file.write_all(text.as_bytes())
        .await
        .map_err(ApiError::internal_error)?;
    file.write_all(b"\n")
        .await
        .map_err(ApiError::internal_error)?;
    file.flush().await.map_err(ApiError::internal_error)?;
    Ok(())
}

async fn write_text_atomic(path: &Path, text: &str) -> Result<(), ApiError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(ApiError::internal_error)?;

    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, text)
        .await
        .map_err(ApiError::internal_error)?;

    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                let _ = tokio::fs::remove_file(path).await;
                tokio::fs::rename(&tmp, path)
                    .await
                    .map_err(ApiError::internal_error)?;
                Ok(())
            } else {
                Err(ApiError::internal_error(err))
            }
        }
    }
}

async fn write_run_file(dir: &Path, run: &domain::Run) -> Result<(), ApiError> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(ApiError::internal_error)?;
    let path = dir.join(format!("{}.json", run.id));
    let text = serde_json::to_string_pretty(run).map_err(ApiError::internal_error)?;
    tokio::fs::write(&path, text)
        .await
        .map_err(ApiError::internal_error)?;
    Ok(())
}

async fn read_run_file(dir: &Path, run_id: &str) -> Result<domain::Run, ApiError> {
    let path = dir.join(format!("{run_id}.json"));
    let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found("run not found")
        } else {
            ApiError::internal_error(e)
        }
    })?;
    serde_json::from_str(&text).map_err(ApiError::internal_error)
}

fn new_run_id() -> String {
    let ts = time::OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .to_string();
    let pid = std::process::id();
    format!("run_{ts}_{pid}")
}

fn format_error_chain_text(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(src) = cur {
        out.push_str("\ncaused by: ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}

fn error_chain_vec(err: &dyn std::error::Error) -> Vec<String> {
    let mut out = Vec::new();
    out.push(err.to_string());
    let mut cur = err.source();
    while let Some(src) = cur {
        out.push(src.to_string());
        cur = src.source();
    }
    out
}

fn runtime_error_code(err: &AgentRuntimeError) -> &'static str {
    match err {
        AgentRuntimeError::MaxSteps { .. } => "max_steps_exceeded",
        AgentRuntimeError::Llm(_) => "llm_error",
        AgentRuntimeError::Tool(_) => "tool_error",
        AgentRuntimeError::Cancelled => "cancelled",
    }
}

fn runtime_error_hint(code: &str, agent: &str) -> Option<String> {
    match code {
        "max_steps_exceeded" => Some(format!(
            "Increase `runtime.max_steps` or `agents.{agent}.max_steps` in `kiliax.yaml`, or split the task / ask for earlier output."
        )),
        "llm_error" => Some(
            "Check provider/base_url/api_key, and use `trace_id` to locate server logs.".to_string(),
        ),
        "tool_error" => Some(
            "Tool execution failed: check workspace/permissions/tool args, and use `trace_id` to locate server logs.".to_string(),
        ),
        _ => None,
    }
}
