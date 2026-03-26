use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;

use crate::llm::Message;

const SESSION_SCHEMA_VERSION: u32 = 1;
const DEFAULT_CHECKPOINT_EVERY: u64 = 32;

const SESSION_ID_FORMAT: &[time::format_description::FormatItem<'static>] = time::macros::format_description!(
    "[year][month][day]T[hour][minute][second]Z_[subsecond digits:3]"
);

/// Session identifier used as the on-disk directory name.
///
/// The generated id includes a timestamp prefix so the directory name is self-describing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    pub fn new() -> Self {
        let ts = time::OffsetDateTime::now_utc()
            .format(SESSION_ID_FORMAT)
            .unwrap_or_else(|_| "unknown".to_string());

        // Add extra entropy to avoid collisions.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let nanos = now.as_nanos();
        let pid = std::process::id();

        Self(format!("{ts}_{nanos:x}-{pid:x}"))
    }

    pub fn parse(s: impl AsRef<str>) -> Result<Self, SessionError> {
        let s = s.as_ref().trim();
        if s.is_empty() {
            return Err(SessionError::InvalidId(
                "session id must not be empty".to_string(),
            ));
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(SessionError::InvalidId(
                "session id must be [A-Za-z0-9_-]".to_string(),
            ));
        }
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMeta {
    pub schema_version: u32,
    pub id: SessionId,

    pub created_at_ms: u64,
    pub updated_at_ms: u64,

    /// "plan" / "general" / future custom agent names.
    pub agent: String,

    /// Fully-qualified model id used for routing, e.g. "moonshot_cn/kimi-k2-turbo-preview".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,

    /// Absolute path to the config file used (if known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,

    /// Absolute path of workspace root (if known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_finish_reason: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,

    /// Last appended event sequence number.
    pub last_seq: u64,

    /// Sequence number included in the latest snapshot.
    pub last_snapshot_seq: u64,

    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSnapshot {
    pub schema_version: u32,
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEventLine {
    pub schema_version: u32,
    pub seq: u64,
    pub ts_ms: u64,
    #[serde(flatten)]
    pub event: SessionEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    Message { message: Message },
    Finish { finish_reason: Option<String> },
    Error { error: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionState {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
}

impl SessionState {
    pub fn id(&self) -> &SessionId {
        &self.meta.id
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("invalid session id: {0}")]
    InvalidId(String),

    #[error("session not found: {0}")]
    NotFound(String),

    #[error("unsupported schema_version: {0}")]
    UnsupportedSchema(u32),

    #[error("failed to serialize: {0}")]
    Serialize(serde_json::Error),

    #[error("failed to deserialize: {0}")]
    Deserialize(serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct FileSessionStore {
    root_dir: PathBuf,
    checkpoint_every: u64,
}

impl FileSessionStore {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            checkpoint_every: DEFAULT_CHECKPOINT_EVERY,
        }
    }

    pub fn with_checkpoint_every(mut self, n: u64) -> Self {
        self.checkpoint_every = n.max(1);
        self
    }

    pub fn project(workspace_root: &Path) -> Self {
        Self::new(workspace_root.join(".kiliax").join("sessions"))
    }

    pub fn global() -> Option<Self> {
        dirs::home_dir().map(|home| Self::new(home.join(".kiliax").join("sessions")))
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn session_dir(&self, id: &SessionId) -> PathBuf {
        self.root_dir.join(id.as_str())
    }

    pub fn meta_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("meta.json")
    }

    pub fn snapshot_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("snapshot.json")
    }

    pub fn events_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("events.jsonl")
    }

    pub async fn create(
        &self,
        agent: impl Into<String>,
        model_id: Option<String>,
        config_path: Option<String>,
        workspace_root: Option<String>,
        initial_messages: Vec<Message>,
    ) -> Result<SessionState, SessionError> {
        tokio::fs::create_dir_all(&self.root_dir).await?;

        let id = SessionId::new();
        let now = now_ms();
        let agent = agent.into();

        let dir = self.session_dir(&id);
        tokio::fs::create_dir_all(&dir).await?;

        // Seed the append-only log with the initial messages so events.jsonl is a complete history.
        let mut last_seq = 0u64;
        for msg in &initial_messages {
            last_seq += 1;
            let line = SessionEventLine {
                schema_version: SESSION_SCHEMA_VERSION,
                seq: last_seq,
                ts_ms: now,
                event: SessionEvent::Message {
                    message: msg.clone(),
                },
            };
            self.append_event_line(&id, &line).await?;
        }
        if last_seq == 0 {
            let _ = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.events_path(&id))
                .await?;
        }

        let meta = SessionMeta {
            schema_version: SESSION_SCHEMA_VERSION,
            id: id.clone(),
            created_at_ms: now,
            updated_at_ms: now,
            agent,
            model_id,
            config_path,
            workspace_root,
            title: derive_title(&initial_messages),
            last_finish_reason: None,
            last_error: None,
            last_seq,
            last_snapshot_seq: last_seq,
            message_count: initial_messages.len(),
        };

        let state = SessionState {
            meta: meta.clone(),
            messages: initial_messages,
        };

        self.write_meta(&meta).await?;
        self.write_snapshot(&meta, &state.messages).await?;

        Ok(state)
    }

    pub async fn load(&self, id: &SessionId) -> Result<SessionState, SessionError> {
        let _ = SessionId::parse(id.as_str())?;

        let snapshot = self.read_snapshot(id).await?;
        let mut state = SessionState {
            meta: snapshot.meta,
            messages: snapshot.messages,
        };

        self.replay_events_after_snapshot(&mut state).await?;
        Ok(state)
    }

    pub async fn list(&self) -> Result<Vec<SessionMeta>, SessionError> {
        let mut out = Vec::new();
        let mut rd = match tokio::fs::read_dir(&self.root_dir).await {
            Ok(rd) => rd,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(err) => return Err(err.into()),
        };

        while let Some(entry) = rd.next_entry().await? {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let meta_path = dir.join("meta.json");
            let text = match tokio::fs::read_to_string(&meta_path).await {
                Ok(t) => t,
                Err(_) => continue,
            };
            let meta: SessionMeta = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.schema_version > SESSION_SCHEMA_VERSION {
                continue;
            }
            out.push(meta);
        }

        out.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
        Ok(out)
    }

    pub async fn delete(&self, id: &SessionId) -> Result<(), SessionError> {
        let dir = self.session_dir(id);
        tokio::fs::remove_dir_all(&dir).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SessionError::NotFound(id.to_string())
            } else {
                e.into()
            }
        })?;
        Ok(())
    }

    pub async fn record_message(
        &self,
        state: &mut SessionState,
        message: Message,
    ) -> Result<(), SessionError> {
        self.record_event(state, SessionEvent::Message { message })
            .await
    }

    pub async fn record_finish(
        &self,
        state: &mut SessionState,
        finish_reason: Option<String>,
    ) -> Result<(), SessionError> {
        self.record_event(state, SessionEvent::Finish { finish_reason })
            .await
    }

    pub async fn record_error(
        &self,
        state: &mut SessionState,
        error: impl Into<String>,
    ) -> Result<(), SessionError> {
        self.record_event(
            state,
            SessionEvent::Error {
                error: error.into(),
            },
        )
        .await
    }

    pub async fn checkpoint(&self, state: &mut SessionState) -> Result<(), SessionError> {
        state.meta.last_snapshot_seq = state.meta.last_seq;
        state.meta.schema_version = SESSION_SCHEMA_VERSION;
        self.write_snapshot(&state.meta, &state.messages).await?;
        self.write_meta(&state.meta).await?;
        Ok(())
    }

    async fn record_event(
        &self,
        state: &mut SessionState,
        event: SessionEvent,
    ) -> Result<(), SessionError> {
        let id = state.meta.id.clone();
        let ts_ms = now_ms();
        let seq = state.meta.last_seq + 1;

        let line = SessionEventLine {
            schema_version: SESSION_SCHEMA_VERSION,
            seq,
            ts_ms,
            event: event.clone(),
        };

        self.append_event_line(&id, &line).await?;
        apply_event(state, event, ts_ms, seq);
        self.write_meta(&state.meta).await?;

        if state
            .meta
            .last_seq
            .saturating_sub(state.meta.last_snapshot_seq)
            >= self.checkpoint_every
        {
            self.checkpoint(state).await?;
        }

        Ok(())
    }

    async fn replay_events_after_snapshot(
        &self,
        state: &mut SessionState,
    ) -> Result<(), SessionError> {
        let id = state.meta.id.clone();
        let path = self.events_path(&id);
        let file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };

        let snapshot_seq = state.meta.last_snapshot_seq;

        let mut reader = tokio::io::BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }

            let raw = line.trim_end_matches(&['\r', '\n'][..]);
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }

            let parsed: Result<SessionEventLine, _> = serde_json::from_str(raw);
            let parsed = match parsed {
                Ok(v) => v,
                Err(err) => {
                    // Best-effort recovery: tolerate a truncated last line (no trailing newline).
                    let ends_with_newline = line.ends_with('\n');
                    if !ends_with_newline {
                        break;
                    }
                    return Err(SessionError::Deserialize(err));
                }
            };

            if parsed.schema_version > SESSION_SCHEMA_VERSION {
                return Err(SessionError::UnsupportedSchema(parsed.schema_version));
            }
            if parsed.seq <= snapshot_seq {
                continue;
            }
            apply_event(state, parsed.event, parsed.ts_ms, parsed.seq);
        }

        Ok(())
    }

    async fn append_event_line(
        &self,
        id: &SessionId,
        line: &SessionEventLine,
    ) -> Result<(), SessionError> {
        let text = serde_json::to_string(line).map_err(SessionError::Serialize)?;
        let path = self.events_path(id);

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(text.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;
        Ok(())
    }

    async fn write_meta(&self, meta: &SessionMeta) -> Result<(), SessionError> {
        let path = self.meta_path(&meta.id);
        write_json_atomic(&path, meta).await
    }

    async fn write_snapshot(
        &self,
        meta: &SessionMeta,
        messages: &[Message],
    ) -> Result<(), SessionError> {
        let path = self.snapshot_path(&meta.id);
        let snapshot = SessionSnapshot {
            schema_version: SESSION_SCHEMA_VERSION,
            meta: meta.clone(),
            messages: messages.to_vec(),
        };
        write_json_atomic(&path, &snapshot).await
    }

    async fn read_snapshot(&self, id: &SessionId) -> Result<SessionSnapshot, SessionError> {
        let path = self.snapshot_path(id);
        let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SessionError::NotFound(id.to_string())
            } else {
                e.into()
            }
        })?;
        let snapshot: SessionSnapshot =
            serde_json::from_str(&text).map_err(SessionError::Deserialize)?;
        if snapshot.schema_version > SESSION_SCHEMA_VERSION {
            return Err(SessionError::UnsupportedSchema(snapshot.schema_version));
        }
        Ok(snapshot)
    }
}

fn apply_event(state: &mut SessionState, event: SessionEvent, ts_ms: u64, seq: u64) {
    state.meta.updated_at_ms = state.meta.updated_at_ms.max(ts_ms);
    state.meta.last_seq = state.meta.last_seq.max(seq);

    match event {
        SessionEvent::Message { message } => {
            if state.meta.title.is_none() {
                if let Message::User { content } = &message {
                    if let Some(t) = content.first_text() {
                        let t = t.trim();
                        if !t.is_empty() {
                            state.meta.title = Some(truncate_title(t));
                        }
                    }
                }
            }
            state.messages.push(message);
            state.meta.message_count = state.messages.len();
        }
        SessionEvent::Finish { finish_reason } => {
            state.meta.last_finish_reason = finish_reason;
        }
        SessionEvent::Error { error } => {
            state.meta.last_error = Some(error);
        }
    }
}

fn derive_title(messages: &[Message]) -> Option<String> {
    for m in messages {
        if let Message::User { content } = m {
            if let Some(t) = content.first_text() {
                let t = t.trim();
                if !t.is_empty() {
                    return Some(truncate_title(t));
                }
            }
        }
    }
    None
}

fn truncate_title(input: &str) -> String {
    let mut s = input.to_string();
    const MAX: usize = 80;
    if s.len() > MAX {
        s.truncate(MAX);
        s.push_str("…");
    }
    s
}

async fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), SessionError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(dir).await?;

    let tmp = path.with_extension("tmp");
    let text = serde_json::to_string_pretty(value).map_err(SessionError::Serialize)?;
    tokio::fs::write(&tmp, text).await?;

    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                let _ = tokio::fs::remove_file(path).await;
                tokio::fs::rename(&tmp, path).await?;
                Ok(())
            } else {
                Err(err.into())
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn roundtrip_create_record_load_list() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(tmp.path()).with_checkpoint_every(1000);

        let mut state = store
            .create(
                "plan",
                Some("p/m".to_string()),
                None,
                None,
                vec![Message::User {
                    content: crate::llm::UserMessageContent::Text("hello".to_string()),
                }],
            )
            .await
            .unwrap();

        store
            .record_message(
                &mut state,
                Message::Assistant {
                    content: Some("hi".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                },
            )
            .await
            .unwrap();

        // Not checkpointed yet: load must replay events.
        let loaded = store.load(state.id()).await.unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.meta.message_count, 2);

        let list = store.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, state.meta.id);
        assert_eq!(list[0].title.as_deref(), Some("hello"));
    }
}
