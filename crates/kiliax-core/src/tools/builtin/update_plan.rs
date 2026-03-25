use serde::Deserialize;

use crate::llm::{ToolCall, ToolDefinition};
use crate::tools::ToolError;

use super::common::parse_args;
use super::TOOL_UPDATE_PLAN;

pub fn update_plan_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_UPDATE_PLAN.to_string(),
        description: Some("Update the UI plan (best effort).".to_string()),
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "explanation": { "type": "string", "description": "Optional brief explanation for changes." },
                "plan": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending","in_progress","completed"] }
                        },
                        "required": ["step","status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["plan"],
            "additionalProperties": false
        })),
        strict: Some(true),
    }
}

pub(super) fn execute(call: &ToolCall) -> Result<String, ToolError> {
    let _args: UpdatePlanArgs = parse_args(call, TOOL_UPDATE_PLAN)?;
    Ok("{\"ok\":true}".to_string())
}

#[derive(Debug, Deserialize)]
struct UpdatePlanArgs {
    #[allow(dead_code)]
    explanation: Option<String>,
    #[allow(dead_code)]
    plan: Vec<UpdatePlanItem>,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanItem {
    #[allow(dead_code)]
    step: String,
    #[allow(dead_code)]
    status: String,
}
