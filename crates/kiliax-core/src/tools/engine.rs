use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use crate::llm::{Message, ToolCall, ToolDefinition};
use crate::tools::{builtin, Permissions, ToolError};

#[derive(Clone)]
pub struct ToolEngine {
    workspace_root: PathBuf,
    shell_sessions: Arc<builtin::ShellSessions>,
    config: Arc<RwLock<Arc<crate::config::Config>>>,
}

impl ToolEngine {
    pub fn new(workspace_root: impl Into<PathBuf>, config: crate::config::Config) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            shell_sessions: Arc::new(builtin::ShellSessions::new()),
            config: Arc::new(RwLock::new(Arc::new(config))),
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn set_config(&self, config: crate::config::Config) -> Result<(), ToolError> {
        let mut guard = self.config.write().map_err(|_| {
            ToolError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "tool config lock poisoned",
            ))
        })?;
        *guard = Arc::new(config);
        Ok(())
    }

    pub async fn extra_tool_definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    pub async fn execute(&self, perms: &Permissions, call: &ToolCall) -> Result<String, ToolError> {
        let cfg = self
            .config
            .read()
            .map_err(|_| {
                ToolError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "tool config lock poisoned",
                ))
            })?
            .clone();
        builtin::execute(
            &self.workspace_root,
            perms,
            self.shell_sessions.as_ref(),
            cfg.as_ref(),
            call,
        )
        .await
    }

    pub async fn execute_to_message(
        &self,
        perms: &Permissions,
        call: &ToolCall,
    ) -> Result<Message, ToolError> {
        let content = self.execute(perms, call).await?;
        Ok(Message::Tool {
            tool_call_id: call.id.clone(),
            content,
        })
    }

    pub async fn execute_to_messages(
        &self,
        perms: &Permissions,
        call: &ToolCall,
    ) -> Result<Vec<Message>, ToolError> {
        if call.name == builtin::TOOL_VIEW_IMAGE {
            let (tool_content, image_message) =
                builtin::execute_view_image_with_attachment(&self.workspace_root, perms, call)
                    .await?;
            return Ok(vec![
                Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: tool_content,
                },
                image_message,
            ]);
        }

        let content = self.execute(perms, call).await?;
        Ok(vec![Message::Tool {
            tool_call_id: call.id.clone(),
            content,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tools::{ShellPermissions, ToolError};

    fn plan_permissions() -> Permissions {
        Permissions {
            file_read: true,
            file_write: false,
            shell: ShellPermissions::AllowList(vec![vec!["ls".into()]]),
        }
    }

    fn build_permissions() -> Permissions {
        Permissions {
            file_read: true,
            file_write: true,
            shell: ShellPermissions::AllowAll,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_denies_apply_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path(), Config::default());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_APPLY_PATCH.to_string(),
            arguments: serde_json::json!({
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+hi\n*** End Patch\n"
            })
            .to_string(),
        };

        let err = engine.execute(&plan_permissions(), &call).await.unwrap_err();
        let ToolError::PermissionDenied(_) = err else {
            panic!("unexpected error: {err:?}");
        };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_denies_disallowed_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path(), Config::default());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_SHELL_COMMAND.to_string(),
            arguments: serde_json::json!({
                "argv": ["rm", "-rf", "/"],
                "yield_time_ms": 0
            })
            .to_string(),
        };

        let err = engine.execute(&plan_permissions(), &call).await.unwrap_err();
        let ToolError::PermissionDenied(_) = err else {
            panic!("unexpected error: {err:?}");
        };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn build_can_apply_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path(), Config::default());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_APPLY_PATCH.to_string(),
            arguments: serde_json::json!({
                "patch": "*** Begin Patch\n*** Add File: a.txt\n+hello\n*** End Patch\n"
            })
            .to_string(),
        };

        let out = engine.execute(&build_permissions(), &call).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.get("ok").and_then(|v| v.as_bool()), Some(true));
        let s = tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap();
        assert_eq!(s, "hello\n");
    }
}
