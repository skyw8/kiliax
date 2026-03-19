use std::path::{Path, PathBuf};

use crate::llm::{Message, ToolCall};
use crate::tools::{builtin, mcp::McpHub, Permissions, ToolError};

#[derive(Clone)]
pub struct ToolEngine {
    workspace_root: PathBuf,
    mcp: Option<McpHub>,
}

impl ToolEngine {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            mcp: None,
        }
    }

    pub fn with_mcp(mut self, hub: McpHub) -> Self {
        self.mcp = Some(hub);
        self
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub async fn execute(&self, perms: &Permissions, call: &ToolCall) -> Result<String, ToolError> {
        match call.name.as_str() {
            builtin::TOOL_READ | builtin::TOOL_WRITE | builtin::TOOL_SHELL => {
                builtin::execute(&self.workspace_root, perms, call).await
            }
            _ => {
                if let Some(hub) = &self.mcp {
                    if McpHub::is_mcp_tool_name(&call.name) {
                        let args = serde_json::from_str::<serde_json::Value>(&call.arguments)
                            .map_err(|source| ToolError::InvalidArgs {
                                tool: call.name.clone(),
                                source,
                            })?;
                        return hub.call_exposed_tool(&call.name, args).await;
                    }
                }
                Err(ToolError::UnknownTool(call.name.clone()))
            }
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
    async fn plan_denies_write() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_WRITE.to_string(),
            arguments: r#"{"path":"a.txt","content":"x"}"#.to_string(),
        };

        let err = engine.execute(&plan_permissions(), &call).await.unwrap_err();
        let ToolError::PermissionDenied(_) = err else {
            panic!("unexpected error: {err:?}");
        };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_denies_disallowed_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_SHELL.to_string(),
            arguments: r#"{"argv":["rm","-rf","/"]}"#.to_string(),
        };

        let err = engine.execute(&plan_permissions(), &call).await.unwrap_err();
        let ToolError::PermissionDenied(_) = err else {
            panic!("unexpected error: {err:?}");
        };
    }

    #[tokio::test(flavor = "current_thread")]
    async fn build_can_write() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = ToolEngine::new(tmp.path());

        let call = ToolCall {
            id: "call_1".to_string(),
            name: builtin::TOOL_WRITE.to_string(),
            arguments: r#"{"path":"a.txt","content":"hello"}"#.to_string(),
        };

        let out = engine.execute(&build_permissions(), &call).await.unwrap();
        assert_eq!(out, "ok");
        let s = tokio::fs::read_to_string(tmp.path().join("a.txt"))
            .await
            .unwrap();
        assert_eq!(s, "hello");
    }
}
