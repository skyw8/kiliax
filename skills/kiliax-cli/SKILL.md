---
name: kiliax-cli
description: Use Kiliax through its CLI and HTTP control plane. Trigger when an agent needs to start Kiliax, inspect capabilities, automate sessions/runs/goals, manage Kiliax skills, or drive Kiliax from another agent such as Codex.
---

# Kiliax CLI

Compact index for operating Kiliax from another agent. Load only the reference file needed for the task.

## Quick Start

```bash
ki
ki server start
ki server stop
```

Run `ki` from the workspace Kiliax should operate on. After start, use the printed URL or `~/.kiliax/server.json` to build:

```text
BASE=http://<host>:<port>/v1
AUTH=Authorization: Bearer <token>
```

## References

- CLI/config/foreground server: `references/cli.md`
- HTTP auth/OpenAPI/idempotency: `references/http-api.md`
- Sessions/runs/messages/streaming: `references/sessions-runs.md`
- Goals: `references/goals.md`
- Skill authoring and overrides: `references/skills.md`
- Remote server/filesystem usage: `references/remote.md`
- Recovery steps: `references/troubleshooting.md`

## Rules

- Treat `~/.kiliax/server.json` as local auth material.
- Prefer `/v1/openapi.yaml` over guessing endpoint shapes.
- Use server-side filesystem APIs for remote servers.
