use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "get_capabilities",
            description: "Return Kiliax agents, models, built-in tools, and MCP server status.",
            input_schema: object_schema(vec![]),
        },
        ToolDefinition {
            name: "list_agents",
            description: "List Kiliax agent profiles available to run.",
            input_schema: object_schema(vec![]),
        },
        ToolDefinition {
            name: "list_sessions",
            description: "List Kiliax sessions.",
            input_schema: object_schema(vec![
                ("live", json!({ "type": "boolean" })),
                ("limit", json!({ "type": "integer", "minimum": 1, "maximum": 200 })),
                ("cursor", json!({ "type": "string" })),
            ]),
        },
        ToolDefinition {
            name: "get_session",
            description: "Return one Kiliax session snapshot.",
            input_schema: required_schema(vec![("session_id", json!({ "type": "string" }))], vec!["session_id"]),
        },
        ToolDefinition {
            name: "get_messages",
            description: "Return recent visible messages for a Kiliax session.",
            input_schema: required_schema(
                vec![
                    ("session_id", json!({ "type": "string" })),
                    ("limit", json!({ "type": "integer", "minimum": 1, "maximum": 200 })),
                    ("before", json!({ "type": "string" })),
                ],
                vec!["session_id"],
            ),
        },
        ToolDefinition {
            name: "run_agent",
            description: "Create a Kiliax session, submit a prompt, and optionally wait for completion.",
            input_schema: required_schema(
                vec![
                    ("prompt", json!({ "type": "string" })),
                    ("title", json!({ "type": "string" })),
                    ("workspace", json!({ "type": "string" })),
                    ("extra_workspace_roots", json!({ "type": "array", "items": { "type": "string" } })),
                    ("agent", json!({ "type": "string" })),
                    ("model_id", json!({ "type": "string" })),
                    ("mcp", json!({ "type": "object" })),
                    ("skills", json!({ "type": "object" })),
                    ("custom_tools", json!({ "type": "object" })),
                    ("overrides", json!({ "type": "object" })),
                    ("attachments", attachment_array_schema()),
                    ("wait", json!({ "type": "boolean", "default": true })),
                    ("timeout_seconds", json!({ "type": "integer", "minimum": 1 })),
                    ("message_limit", json!({ "type": "integer", "minimum": 1, "maximum": 200 })),
                ],
                vec!["prompt"],
            ),
        },
        ToolDefinition {
            name: "continue_session",
            description: "Submit a follow-up prompt to an existing Kiliax session and optionally wait for completion.",
            input_schema: required_schema(
                vec![
                    ("session_id", json!({ "type": "string" })),
                    ("prompt", json!({ "type": "string" })),
                    ("overrides", json!({ "type": "object" })),
                    ("attachments", attachment_array_schema()),
                    ("wait", json!({ "type": "boolean", "default": true })),
                    ("timeout_seconds", json!({ "type": "integer", "minimum": 1 })),
                    ("message_limit", json!({ "type": "integer", "minimum": 1, "maximum": 200 })),
                ],
                vec!["session_id", "prompt"],
            ),
        },
        ToolDefinition {
            name: "cancel_run",
            description: "Cancel an active Kiliax run.",
            input_schema: required_schema(vec![("run_id", json!({ "type": "string" }))], vec!["run_id"]),
        },
    ]
}

fn attachment_array_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "filename": { "type": "string" },
                "media_type": { "type": "string" },
                "data": { "type": "string", "description": "Raw base64 bytes without a data URL prefix." }
            },
            "required": ["filename", "media_type", "data"],
            "additionalProperties": false
        }
    })
}

fn object_schema(properties: Vec<(&'static str, Value)>) -> Value {
    required_schema(properties, Vec::new())
}

fn required_schema(properties: Vec<(&'static str, Value)>, required: Vec<&'static str>) -> Value {
    let properties = properties
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_stable_and_unique() {
        let tools = tool_definitions();
        let mut names = tools.iter().map(|t| t.name).collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        assert!(names.contains(&"run_agent"));
        assert!(names.contains(&"continue_session"));
        assert!(names.contains(&"get_capabilities"));
    }
}
