use crate::protocol::ToolCall;
use crate::tools::{tool_parallelism, ToolParallelism};

#[derive(Debug, Clone, Copy)]
pub(super) enum ToolCallGroup<'a> {
    Exclusive(&'a ToolCall),
    Parallel(&'a [ToolCall]),
}

pub(super) fn group_tool_calls(tool_calls: &[ToolCall]) -> Vec<ToolCallGroup<'_>> {
    let mut out = Vec::new();
    let mut idx = 0usize;

    while idx < tool_calls.len() {
        let call = &tool_calls[idx];
        if tool_parallelism(call.name.as_str()).is_parallel() {
            let start = idx;
            idx += 1;
            while idx < tool_calls.len()
                && tool_parallelism(tool_calls[idx].name.as_str()) == ToolParallelism::Parallel
            {
                idx += 1;
            }
            out.push(ToolCallGroup::Parallel(&tool_calls[start..idx]));
        } else {
            out.push(ToolCallGroup::Exclusive(call));
            idx += 1;
        }
    }

    out
}

pub(super) fn normalize_tool_call_ids(step: usize, tool_calls: &mut [ToolCall]) {
    let mut used: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(tool_calls.len());

    for (idx, call) in tool_calls.iter_mut().enumerate() {
        let trimmed = call.id.trim();
        if trimmed != call.id {
            call.id = trimmed.to_string();
        }

        if call.id.is_empty() || used.contains(&call.id) {
            call.id = format!("call_step{}_{}", step + 1, idx);
        }
        used.insert(call.id.clone());
    }
}
