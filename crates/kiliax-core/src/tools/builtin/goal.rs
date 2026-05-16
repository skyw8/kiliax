use async_trait::async_trait;
use serde::Deserialize;

use crate::protocol::{ToolCall, ToolDefinition};
use crate::session::{SessionGoal, SessionGoalStatus};
use crate::tools::ToolError;

pub const TOOL_GET_GOAL: &str = "get_goal";
pub const TOOL_UPDATE_GOAL: &str = "update_goal";

#[async_trait]
pub trait GoalBackend: Send + Sync {
    async fn get_goal(&self) -> Result<Option<SessionGoal>, ToolError>;
    async fn complete_goal(&self) -> Result<Option<SessionGoal>, ToolError>;
}

pub fn get_goal_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_GET_GOAL.to_string(),
        description: Some("Get the active session goal, if any.".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

pub fn update_goal_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_UPDATE_GOAL.to_string(),
        description: Some(
            "Mark the active session goal complete. Only status=complete is supported.".to_string(),
        ),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["complete"] }
            },
            "required": ["status"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

#[derive(Debug, Deserialize)]
struct UpdateGoalArgs {
    status: SessionGoalStatus,
}

pub async fn execute_get(backend: &dyn GoalBackend) -> Result<String, ToolError> {
    let goal = backend.get_goal().await?;
    serde_json::to_string(&goal)
        .map_err(|e| ToolError::InvalidCommand(format!("failed to serialize goal: {e}")))
}

pub async fn execute_update(
    backend: &dyn GoalBackend,
    call: &ToolCall,
) -> Result<String, ToolError> {
    let args: UpdateGoalArgs =
        serde_json::from_str(&call.arguments).map_err(|source| ToolError::InvalidArgs {
            tool: call.name.clone(),
            source,
        })?;
    if args.status != SessionGoalStatus::Complete {
        return Err(ToolError::InvalidCommand(
            "update_goal only supports status=complete".to_string(),
        ));
    }
    let goal = backend.complete_goal().await?;
    serde_json::to_string(&goal)
        .map_err(|e| ToolError::InvalidCommand(format!("failed to serialize goal: {e}")))
}
