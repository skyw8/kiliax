use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

use crate::tools::ToolError;

#[derive(Clone, Default)]
pub struct FileAccessTracker {
    inner: Arc<Mutex<HashMap<PathBuf, FileStamp>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified_nanos: Option<u128>,
    len: u64,
}

impl FileAccessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn record_read(&self, path: &Path) -> Result<(), ToolError> {
        let canonical = tokio::fs::canonicalize(path).await?;
        let meta = tokio::fs::metadata(&canonical).await?;
        let stamp = FileStamp::from_meta(&meta);

        let mut guard = self.inner.lock().await;
        guard.insert(canonical, stamp);
        Ok(())
    }

    pub async fn assert_read_unchanged(
        &self,
        path: &Path,
        tool_name: &str,
    ) -> Result<(), ToolError> {
        let canonical = tokio::fs::canonicalize(path).await?;
        let meta = tokio::fs::metadata(&canonical).await?;
        let current = FileStamp::from_meta(&meta);

        let guard = self.inner.lock().await;
        let Some(prev) = guard.get(&canonical) else {
            return Err(ToolError::InvalidCommand(format!(
                "must use read_file before {tool_name} for {}",
                canonical.display()
            )));
        };

        if *prev != current {
            return Err(ToolError::InvalidCommand(format!(
                "file changed since last read: {} (use read_file again)",
                canonical.display()
            )));
        }

        Ok(())
    }
}

impl FileStamp {
    fn from_meta(meta: &std::fs::Metadata) -> Self {
        Self {
            modified_nanos: meta.modified().ok().and_then(system_time_nanos),
            len: meta.len(),
        }
    }
}

fn system_time_nanos(t: SystemTime) -> Option<u128> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_nanos())
}
