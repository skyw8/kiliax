---
name: call-kiliax
description: Delegate work to a Kiliax MCP server. Use when the user asks to use Kiliax, call Kiliax, delegate a task to Kiliax, run or continue a Kiliax agent session, or run a specific Kiliax skill through MCP.
---

# Call Kiliax

Use the configured Kiliax MCP server as an agent service.

## Workflow

1. Call `get_capabilities` to inspect available agents, models, skills, and server status.
2. For a new task, call `run_agent` with the user's task in `prompt`.
3. If the user names a specific Kiliax skill, call `run_skill` with `skill_id` and `prompt`.
4. If continuing prior delegated work, call `continue_session` with the known `session_id`.
5. Use `get_session`, `get_messages`, or `kiliax://sessions/{session_id}` resources to inspect results.

## Arguments

- Pass `workspace` only when the target path is on the Kiliax server filesystem. For remote Kiliax servers, do not pass a local browser or client-side path.
- Use `wait: true` for ordinary delegation so the response includes final messages. Use `wait: false` for long-running tasks and poll with `get_session` or run resources.
- Prefer `run_skill` over manually setting skill overrides when exactly one Kiliax skill should be active.

## Output Handling

Read `structuredContent` first when the MCP client exposes it. Fall back to the JSON text in `content[0].text` when structured content is unavailable.
