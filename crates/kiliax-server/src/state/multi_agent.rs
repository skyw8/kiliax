use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use kiliax_core::session::SessionId;
use kiliax_core::tools::builtin::multi_agents as core_ma;
use kiliax_core::tools::ToolError;
use tokio::sync::{watch, Mutex};
use tokio::time::{timeout_at, Instant};

use super::LiveSession;

const ROOT_PATH: &str = "/root";

#[derive(Debug, Clone)]
pub(super) struct MultiAgentIdentity {
    pub session_id: SessionId,
    pub root_session_id: SessionId,
    pub parent_session_id: Option<SessionId>,
    pub agent_path: String,
    pub closed: bool,
}

#[derive(Clone)]
struct AgentEntry {
    identity: MultiAgentIdentity,
    live: Option<Arc<LiveSession>>,
    last_task_message: Option<String>,
}

#[derive(Debug, Clone)]
enum MailboxItem {
    Message(core_ma::MailboxUpdate),
    Update(core_ma::MailboxUpdate),
}

#[derive(Default)]
struct MultiAgentState {
    by_session: HashMap<String, AgentEntry>,
    session_by_path: HashMap<String, String>,
    mailbox: HashMap<String, VecDeque<MailboxItem>>,
}

pub(crate) struct MultiAgentControl {
    state: Mutex<MultiAgentState>,
    mailbox_tx: watch::Sender<u64>,
}

impl MultiAgentControl {
    pub(super) fn new() -> Self {
        let (mailbox_tx, _) = watch::channel(0);
        Self {
            state: Mutex::new(MultiAgentState::default()),
            mailbox_tx,
        }
    }

    pub(super) async fn register_live_agent(
        &self,
        identity: MultiAgentIdentity,
        live: Arc<LiveSession>,
    ) {
        let session_key = identity.session_id.to_string();
        let path_key = scoped_path_key(&identity.root_session_id, &identity.agent_path);
        let mut state = self.state.lock().await;
        state.session_by_path.insert(path_key, session_key.clone());
        let last_task_message = state
            .by_session
            .get(&session_key)
            .and_then(|entry| entry.last_task_message.clone());
        state.by_session.insert(
            session_key,
            AgentEntry {
                identity,
                live: Some(live),
                last_task_message,
            },
        );
    }

    pub(super) async fn reserve_spawn(
        &self,
        parent: &MultiAgentIdentity,
        task_name: &str,
        max_concurrent_agents_per_root: usize,
        max_depth: usize,
    ) -> Result<String, ToolError> {
        validate_task_name(task_name)?;
        let child_path = join_agent_path(&parent.agent_path, task_name)?;
        let child_depth = agent_depth(&child_path);
        if child_depth > max_depth {
            return Err(ToolError::InvalidCommand(format!(
                "spawn_agent max depth exceeded: depth {child_depth}, max {max_depth}"
            )));
        }

        let root_key = parent.root_session_id.to_string();
        let mut state = self.state.lock().await;
        let path_key = scoped_path_key(&parent.root_session_id, &child_path);
        if state.session_by_path.contains_key(&path_key) {
            return Err(ToolError::InvalidCommand(format!(
                "agent path `{child_path}` already exists"
            )));
        }
        let active = state
            .by_session
            .values()
            .filter(|entry| {
                entry.identity.root_session_id.to_string() == root_key
                    && entry.identity.agent_path != ROOT_PATH
                    && !entry.identity.closed
            })
            .count();
        if active >= max_concurrent_agents_per_root {
            return Err(ToolError::InvalidCommand(format!(
                "multi-agent limit reached: max_concurrent_agents_per_root={max_concurrent_agents_per_root}"
            )));
        }
        state.session_by_path.insert(path_key, String::new());
        Ok(child_path)
    }

    pub(super) async fn release_reserved_path(&self, root_session_id: &SessionId, path: &str) {
        let path_key = scoped_path_key(root_session_id, path);
        let mut state = self.state.lock().await;
        if state
            .session_by_path
            .get(&path_key)
            .is_some_and(|session_id| session_id.is_empty())
        {
            state.session_by_path.remove(&path_key);
        }
    }

    pub(super) async fn set_last_task_message(&self, session_id: &SessionId, message: String) {
        let mut state = self.state.lock().await;
        if let Some(entry) = state.by_session.get_mut(session_id.as_str()) {
            entry.last_task_message = Some(message);
        }
    }

    pub(super) async fn resolve_target(
        &self,
        current: &MultiAgentIdentity,
        target: &str,
    ) -> Result<MultiAgentIdentity, ToolError> {
        let target = target.trim();
        if target.is_empty() {
            return Err(ToolError::InvalidCommand(
                "target must not be empty".to_string(),
            ));
        }

        let state = self.state.lock().await;
        if let Ok(session_id) = SessionId::parse(target) {
            if let Some(entry) = state.by_session.get(session_id.as_str()) {
                if !entry.identity.closed {
                    return Ok(entry.identity.clone());
                }
            }
        }

        let path = resolve_agent_path(&current.agent_path, target)?;
        let path_key = scoped_path_key(&current.root_session_id, &path);
        let Some(session_id) = state.session_by_path.get(&path_key) else {
            return Err(ToolError::InvalidCommand(format!(
                "live agent path `{path}` not found"
            )));
        };
        if session_id.is_empty() {
            return Err(ToolError::InvalidCommand(format!(
                "agent path `{path}` is still being created"
            )));
        }
        state
            .by_session
            .get(session_id)
            .filter(|entry| !entry.identity.closed)
            .map(|entry| entry.identity.clone())
            .ok_or_else(|| ToolError::InvalidCommand(format!("agent path `{path}` not found")))
    }

    pub(super) async fn live_for_session(
        &self,
        session_id: &SessionId,
    ) -> Option<Arc<LiveSession>> {
        self.state
            .lock()
            .await
            .by_session
            .get(session_id.as_str())
            .and_then(|entry| entry.live.clone())
    }

    pub(super) async fn queue_message(
        &self,
        from: &MultiAgentIdentity,
        to: &MultiAgentIdentity,
        message: String,
    ) -> Result<(), ToolError> {
        if message.trim().is_empty() {
            return Err(ToolError::InvalidCommand(
                "message must not be empty".to_string(),
            ));
        }
        let update = core_ma::MailboxUpdate {
            from: from.agent_path.clone(),
            to: to.agent_path.clone(),
            session_id: from.session_id.to_string(),
            status: "message".to_string(),
            message,
        };
        let mut state = self.state.lock().await;
        state
            .mailbox
            .entry(to.session_id.to_string())
            .or_default()
            .push_back(MailboxItem::Message(update));
        self.bump_mailbox();
        Ok(())
    }

    pub(super) async fn drain_messages_for_run(
        &self,
        session_id: &SessionId,
    ) -> Vec<core_ma::MailboxUpdate> {
        let mut state = self.state.lock().await;
        let Some(queue) = state.mailbox.get_mut(session_id.as_str()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut keep = VecDeque::new();
        while let Some(item) = queue.pop_front() {
            match item {
                MailboxItem::Message(update) => out.push(update),
                other => keep.push_back(other),
            }
        }
        *queue = keep;
        out
    }

    pub(super) async fn notify_finished(
        &self,
        identity: &MultiAgentIdentity,
        status: String,
        message: String,
    ) {
        let Some(parent_session_id) = identity.parent_session_id.as_ref() else {
            return;
        };
        let parent = {
            let state = self.state.lock().await;
            state.by_session.get(parent_session_id.as_str()).cloned()
        };
        let Some(parent) = parent else {
            return;
        };
        let update = core_ma::MailboxUpdate {
            from: identity.agent_path.clone(),
            to: parent.identity.agent_path,
            session_id: identity.session_id.to_string(),
            status,
            message,
        };
        let mut state = self.state.lock().await;
        state
            .mailbox
            .entry(parent_session_id.to_string())
            .or_default()
            .push_back(MailboxItem::Update(update));
        self.bump_mailbox();
    }

    pub(super) async fn wait_for_updates(
        &self,
        session_id: &SessionId,
        timeout_ms: u64,
    ) -> core_ma::WaitAgentResult {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut rx = self.mailbox_tx.subscribe();
        loop {
            let updates = self.drain_mailbox(session_id).await;
            if !updates.is_empty() {
                return core_ma::WaitAgentResult {
                    timed_out: false,
                    updates,
                };
            }
            if Instant::now() >= deadline {
                return core_ma::WaitAgentResult {
                    timed_out: true,
                    updates: Vec::new(),
                };
            }
            match timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => {
                    return core_ma::WaitAgentResult {
                        timed_out: true,
                        updates: Vec::new(),
                    };
                }
            }
        }
    }

    async fn drain_mailbox(&self, session_id: &SessionId) -> Vec<core_ma::MailboxUpdate> {
        let mut state = self.state.lock().await;
        state
            .mailbox
            .remove(session_id.as_str())
            .unwrap_or_default()
            .into_iter()
            .map(|item| match item {
                MailboxItem::Message(update) | MailboxItem::Update(update) => update,
            })
            .collect()
    }

    pub(super) async fn list_agents(
        &self,
        current: &MultiAgentIdentity,
        path_prefix: Option<&str>,
    ) -> Result<Vec<core_ma::ListedAgent>, ToolError> {
        let prefix = path_prefix
            .map(|prefix| resolve_agent_path(&current.agent_path, prefix))
            .transpose()?;

        let mut entries = self
            .state
            .lock()
            .await
            .by_session
            .values()
            .filter(|entry| entry.identity.root_session_id == current.root_session_id)
            .filter(|entry| !entry.identity.closed)
            .filter(|entry| {
                prefix
                    .as_deref()
                    .is_none_or(|prefix| path_matches_prefix(&entry.identity.agent_path, prefix))
            })
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.identity.agent_path.cmp(&right.identity.agent_path));

        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let status = match entry.live.as_ref() {
                Some(live) => live.multi_agent_status().await,
                None => "closed".to_string(),
            };
            out.push(core_ma::ListedAgent {
                agent_name: entry.identity.agent_path,
                session_id: entry.identity.session_id.to_string(),
                agent_status: status,
                last_task_message: entry.last_task_message,
            });
        }
        Ok(out)
    }

    pub(super) async fn close_agent(
        &self,
        current: &MultiAgentIdentity,
        target: &str,
    ) -> Result<String, ToolError> {
        let target = self.resolve_target(current, target).await?;
        if target.agent_path == ROOT_PATH {
            return Err(ToolError::InvalidCommand(
                "root is not a spawned agent".to_string(),
            ));
        }
        let target_live = {
            let state = self.state.lock().await;
            state
                .by_session
                .get(target.session_id.as_str())
                .and_then(|entry| entry.live.clone())
        };
        let previous_status = match target_live.as_ref() {
            Some(live) => live.multi_agent_status().await,
            None => "closed".to_string(),
        };
        let mut state = self.state.lock().await;
        let descendants = state
            .by_session
            .values_mut()
            .filter(|entry| {
                entry.identity.root_session_id == target.root_session_id
                    && path_matches_prefix(&entry.identity.agent_path, &target.agent_path)
            })
            .filter_map(|entry| {
                entry.identity.closed = true;
                entry.live.take()
            })
            .collect::<Vec<_>>();
        drop(state);

        for live in descendants {
            live.close_for_multi_agent().await;
        }

        Ok(previous_status)
    }

    fn bump_mailbox(&self) {
        let next = self.mailbox_tx.borrow().saturating_add(1);
        self.mailbox_tx.send_replace(next);
    }
}

pub(super) struct MultiAgentToolBackend {
    live: Weak<LiveSession>,
    control: Weak<MultiAgentControl>,
}

impl MultiAgentToolBackend {
    pub(super) fn new(live: Weak<LiveSession>, control: Weak<MultiAgentControl>) -> Self {
        Self { live, control }
    }

    fn live_and_control(&self) -> Result<(Arc<LiveSession>, Arc<MultiAgentControl>), ToolError> {
        let live = self.live.upgrade().ok_or_else(|| {
            ToolError::InvalidCommand("current agent session is no longer live".to_string())
        })?;
        let control = self.control.upgrade().ok_or_else(|| {
            ToolError::InvalidCommand("multi-agent control is no longer available".to_string())
        })?;
        Ok((live, control))
    }
}

#[async_trait]
impl core_ma::MultiAgentBackend for MultiAgentToolBackend {
    async fn spawn_agent(
        &self,
        args: core_ma::SpawnAgentArgs,
    ) -> Result<core_ma::SpawnAgentResult, ToolError> {
        let (live, control) = self.live_and_control()?;
        live.spawn_multi_agent(control, args).await
    }

    async fn send_message(
        &self,
        args: core_ma::MessageAgentArgs,
    ) -> Result<core_ma::MessageAgentResult, ToolError> {
        let (live, control) = self.live_and_control()?;
        live.send_multi_agent_message(control, args, false).await
    }

    async fn followup_task(
        &self,
        args: core_ma::MessageAgentArgs,
    ) -> Result<core_ma::MessageAgentResult, ToolError> {
        let (live, control) = self.live_and_control()?;
        live.send_multi_agent_message(control, args, true).await
    }

    async fn wait_agent(
        &self,
        args: core_ma::WaitAgentArgs,
    ) -> Result<core_ma::WaitAgentResult, ToolError> {
        let (live, control) = self.live_and_control()?;
        live.wait_multi_agent(control, args).await
    }

    async fn list_agents(
        &self,
        args: core_ma::ListAgentsArgs,
    ) -> Result<core_ma::ListAgentsResult, ToolError> {
        let (live, control) = self.live_and_control()?;
        live.list_multi_agents(control, args).await
    }

    async fn close_agent(
        &self,
        args: core_ma::CloseAgentArgs,
    ) -> Result<core_ma::CloseAgentResult, ToolError> {
        let (live, control) = self.live_and_control()?;
        live.close_multi_agent(control, args).await
    }
}

pub(super) fn default_root_identity(session_id: SessionId, closed: bool) -> MultiAgentIdentity {
    MultiAgentIdentity {
        root_session_id: session_id.clone(),
        session_id,
        parent_session_id: None,
        agent_path: ROOT_PATH.to_string(),
        closed,
    }
}

pub(super) fn validate_task_name(task_name: &str) -> Result<(), ToolError> {
    let task_name = task_name.trim();
    if task_name.is_empty()
        || !task_name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(ToolError::InvalidCommand(
            "task_name must contain only lowercase letters, digits, and underscores".to_string(),
        ));
    }
    Ok(())
}

fn join_agent_path(parent: &str, task_name: &str) -> Result<String, ToolError> {
    validate_agent_path(parent)?;
    validate_task_name(task_name)?;
    Ok(if parent == ROOT_PATH {
        format!("{ROOT_PATH}/{task_name}")
    } else {
        format!("{parent}/{task_name}")
    })
}

fn resolve_agent_path(current: &str, target: &str) -> Result<String, ToolError> {
    validate_agent_path(current)?;
    let target = target.trim().trim_end_matches('/');
    if target.is_empty() {
        return Err(ToolError::InvalidCommand(
            "agent path must not be empty".to_string(),
        ));
    }
    let path = if target.starts_with('/') {
        target.to_string()
    } else if current == ROOT_PATH {
        format!("{ROOT_PATH}/{target}")
    } else {
        format!("{current}/{target}")
    };
    validate_agent_path(&path)?;
    Ok(path)
}

fn validate_agent_path(path: &str) -> Result<(), ToolError> {
    if path == ROOT_PATH {
        return Ok(());
    }
    let Some(rest) = path.strip_prefix("/root/") else {
        return Err(ToolError::InvalidCommand(format!(
            "agent path `{path}` must start with /root"
        )));
    };
    if rest.is_empty() {
        return Err(ToolError::InvalidCommand(
            "agent path must not end with /".to_string(),
        ));
    }
    for segment in rest.split('/') {
        validate_task_name(segment)?;
    }
    Ok(())
}

fn agent_depth(path: &str) -> usize {
    if path == ROOT_PATH {
        0
    } else {
        path.trim_start_matches("/root/")
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count()
    }
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    prefix == ROOT_PATH
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|s| s.starts_with('/'))
}

fn scoped_path_key(root_session_id: &SessionId, path: &str) -> String {
    format!("{}\0{path}", root_session_id.as_str())
}
