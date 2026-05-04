use crate::types::{ToolChoice, ToolDefinition};

const WEB_SEARCH_INTERNAL: &str = "web_search";
const WEB_SEARCH_WIRE: &str = "kiliax_web_search";

pub(super) fn to_wire_tool_name(name: &str) -> &str {
    match name {
        WEB_SEARCH_INTERNAL => WEB_SEARCH_WIRE,
        _ => name,
    }
}

pub(super) fn to_internal_tool_name(name: &str) -> &str {
    match name {
        WEB_SEARCH_WIRE => WEB_SEARCH_INTERNAL,
        _ => name,
    }
}

pub(super) fn to_wire_tool_definition(mut tool: ToolDefinition) -> ToolDefinition {
    let internal_name = tool.name.clone();
    let wire_name = to_wire_tool_name(&internal_name);
    if wire_name != internal_name {
        tool.name = wire_name.to_string();
        let description = tool
            .description
            .take()
            .unwrap_or_else(|| "Tool provided by Kiliax.".to_string());
        tool.description = Some(format!("Kiliax `{internal_name}` tool. {description}"));
    }
    tool
}

pub(super) fn to_wire_tool_choice(choice: &ToolChoice) -> ToolChoice {
    match choice {
        ToolChoice::Named { name } => ToolChoice::Named {
            name: to_wire_tool_name(name).to_string(),
        },
        other => other.clone(),
    }
}
