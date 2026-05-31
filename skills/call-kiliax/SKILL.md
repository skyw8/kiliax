---
name: call-kiliax
description: Operate Kiliax from another agent through its CLI and MCP server. Use when the user asks to use Kiliax, call Kiliax, delegate work to Kiliax, start or manage a Kiliax server, install this Kiliax-calling skill, run or continue a Kiliax agent session, run a specific Kiliax skill, inspect Kiliax sessions/messages, or manage a Kiliax session goal.
---

# Call Kiliax

Use Kiliax as an agent service. Prefer MCP tools when they are available in the host agent; use the `ki` CLI to start/export Kiliax, install this skill, or manage local session goals.

## Decision Guide

- If MCP tools are already connected, use them directly. This is the normal path for delegation.
- If no Kiliax MCP server is connected, start or export one with `ki mcp serve`.
- If the user wants the web UI or daemon, use `ki`, `ki server start`, `ki server run`, `ki server stop`, or `ki server restart`.
- If the user asks to install Kiliax instructions into a skills directory, use `ki mcp skill install`.
- If the user asks about a persisted session goal, use `ki goal get`, `ki goal set`, or `ki goal clear`.
- For remote Kiliax servers, remember that workspace paths are paths on the Kiliax server filesystem, not the browser/client filesystem.

## CLI Reference

The installed command is `ki` when Kiliax is installed. The source package name remains `kiliax`.

### Launch and Server Control

- `ki`: ensure the local Kiliax server is running and open the Web UI. On first run, Kiliax silently writes a template config if none exists.
- `ki server start`: ensure the server is running and open the Web UI.
- `ki server stop`: stop the daemon tracked by `~/.kiliax/server.json`.
- `ki server restart`: stop the tracked daemon, then start/open Kiliax again.
- `ki server run [OPTIONS]`: run the HTTP server in the foreground.

`ki server run` options:

- `--host <ip>`: bind host, default `127.0.0.1`.
- `--port <port>`: bind port, default `8123`.
- `--workspace-root <dir>`: server-side workspace root, default current directory.
- `--config <path>`: config path, default auto-detect `kiliax.yaml`.
- `--token <token>`: bearer/web auth token.

Use foreground `ki server run` for managed processes, containers, remote hosts, and debugging. Use `ki` or `ki server start` for ordinary local desktop usage.

### MCP Export

Use MCP export when another agent should call Kiliax as a tool provider.

- `ki mcp serve --transport stdio`: serve MCP over stdio. This is best for local MCP client config.
- `ki mcp serve --transport http`: serve MCP over Streamable HTTP at `/mcp` by default.
- `ki mcp serve [--transport stdio|http] [--base-url URL] [--token TOKEN]`: connect the MCP adapter to an existing Kiliax HTTP control plane.

`ki mcp serve` options:

- `--base-url <url>`: upstream Kiliax server URL. If omitted, Kiliax starts/uses the local daemon.
- `--token <token>`: bearer token for the upstream Kiliax server.
- `--host <host>`: HTTP MCP bind host, default `127.0.0.1`.
- `--port <port>`: HTTP MCP port, default `8124`.
- `--path <path>`: HTTP MCP endpoint path, default `/mcp`.
- `--mcp-token <token>`: bearer token required by the HTTP MCP endpoint.
- `--allow-origin <origin>`: allow an additional browser Origin for HTTP MCP requests.

Environment variables:

- `KILIAX_BASE_URL`: default upstream server URL.
- `KILIAX_TOKEN`: default upstream bearer token.
- `KILIAX_MCP_HOST`: default HTTP MCP bind host.
- `KILIAX_MCP_PORT`: default HTTP MCP port.
- `KILIAX_MCP_PATH`: default HTTP MCP path.
- `KILIAX_MCP_TOKEN`: default HTTP MCP bearer token.

Security rule: HTTP MCP on a non-loopback bind host requires `--mcp-token` or `KILIAX_MCP_TOKEN`.

### Install This Skill

- `ki mcp skill install`: install `call-kiliax` into `~/.kiliax/skills/call-kiliax`.
- `ki mcp skill install --path <dir>`: install into a custom skills root.
- `ki mcp skill install --force`: overwrite an existing installed copy.

### Session Goals

These commands operate on local persisted session metadata.

- `ki goal get --session <SESSION_ID>`: print the current goal JSON.
- `ki goal set --session <SESSION_ID> <OBJECTIVE...>`: set a persistent session goal.
- `ki goal clear --session <SESSION_ID>`: clear the session goal.

## MCP Workflow

1. Call `get_capabilities` before choosing agents, models, skills, or overrides.
2. For a new delegated task, call `run_agent` with the user's task in `prompt`.
3. If the user names exactly one Kiliax skill, call `run_skill` with `skill_id` and `prompt`.
4. If continuing prior work, call `continue_session` with the known `session_id`.
5. For long-running work, set `wait: false`, keep the returned `run.id`, and poll with `get_session`, `get_messages`, or `kiliax://runs/{run_id}`.
6. If a run must stop, call `cancel_run` with `run_id`.

## MCP Tools

- `get_capabilities`: inspect agents, models, built-in tools, skills, custom tools, and server status.
- `list_agents`: list available agent profiles.
- `list_sessions`: list recent sessions. Supports `live`, `limit`, and `cursor`.
- `get_session`: fetch one session snapshot by `session_id`.
- `get_messages`: fetch recent visible messages by `session_id`.
- `list_skills`: list global skills or workspace/session skills.
- `get_config_skills`: read global skill enablement defaults and overrides.
- `set_config_skills`: update global skill enablement defaults and overrides.
- `set_session_skills`: update skill enablement for a specific session.
- `run_agent`: create a session, submit a prompt, optionally wait, and return messages/final text.
- `run_skill`: create a session with exactly one skill enabled, submit a prompt, optionally wait, and return messages/final text.
- `continue_session`: submit a follow-up prompt to an existing session.
- `cancel_run`: cancel an active run.

## MCP Resources

- `kiliax://capabilities`: capabilities and server status.
- `kiliax://sessions`: recent sessions.
- `kiliax://skills`: discovered skills.
- `kiliax://config/skills`: global skill configuration.
- `kiliax://custom-tools`: discovered custom tools.
- `kiliax://sessions/{session_id}`: one session snapshot.
- `kiliax://sessions/{session_id}/messages`: recent visible messages.
- `kiliax://sessions/{session_id}/skills`: skills discovered for a session workspace.
- `kiliax://runs/{run_id}`: one run snapshot.

## MCP Prompts

- `run_agent`: ask the host agent to start a Kiliax session for a task.
- `continue_session`: ask the host agent to continue an existing Kiliax session.

## Argument Guidance

- Pass `workspace` only when the target path is on the Kiliax server filesystem. For remote Kiliax servers, do not pass a local browser or client-side path.
- Pass `extra_workspace_roots` only for additional server-side roots that Kiliax should be allowed to access.
- Use `wait: true` for ordinary delegation so the response includes final messages. Use `wait: false` for long-running tasks and poll with `get_session`, `get_messages`, or run resources.
- Use `timeout_seconds` when waiting for a run that may take longer than the default.
- Prefer `run_skill` over manually setting skill overrides when exactly one Kiliax skill should be active.
- Use `mcp`, `skills`, `custom_tools`, and `overrides` only when the user asks for specific runtime settings or when `get_capabilities` shows a needed option.
- Attachments use raw base64 in `data`, with `filename` and `media_type`.

## Common Patterns

### Delegate a Task

1. Call `get_capabilities`.
2. Call `run_agent` with `prompt`, optional `workspace`, optional `agent`, and optional `model_id`.
3. Read `final_message` first. If missing or incomplete, inspect `messages`.

### Run a Specific Skill

1. Call `list_skills` or `get_capabilities`.
2. Call `run_skill` with `skill_id` and `prompt`.
3. Use `wait: true` unless the task is expected to run for a long time.

### Continue a Session

1. Call `get_session` or `get_messages` if context is needed.
2. Call `continue_session` with `session_id` and `prompt`.
3. Return the latest assistant result from `final_message` or `messages`.

### Connect a Local MCP Client

Configure the client to launch:

```sh
ki mcp serve --transport stdio
```

When connecting to an existing remote Kiliax server:

```sh
ki mcp serve --transport stdio --base-url https://kiliax.example.com --token <token>
```

### Serve MCP over HTTP

For local HTTP MCP:

```sh
ki mcp serve --transport http --host 127.0.0.1 --port 8124 --path /mcp
```

For remote HTTP MCP:

```sh
ki mcp serve --transport http --host 0.0.0.0 --port 8124 --mcp-token <token> --base-url https://kiliax.example.com --token <upstream-token>
```

## Output Handling

- Prefer `structuredContent` when the MCP client exposes it.
- Fall back to the JSON text in `content[0].text` when structured content is unavailable.
- For `run_agent`, `run_skill`, and `continue_session`, report `final_message` when present and include `session.id` or `session_id` plus `run.id` when the user may need to continue later.
- If a run ends in `error` or `cancelled`, inspect the run object and recent messages before summarizing.

## Remote Usage Notes

- Treat every workspace path as server-side.
- Do not assume a native file picker can see remote server files.
- For remote Web UI usage, prefer server-side folder listing/picking features.
- Never expose bearer tokens in user-visible summaries unless the user explicitly asked to print them.
