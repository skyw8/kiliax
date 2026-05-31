# Sessions And Runs

Create a session:

```bash
curl -sS -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"title":"Agent task","settings":{"workspace_root":"/path/to/workspace"}}' \
  "$BASE/sessions"
```

List sessions with `GET /sessions?limit=20`. Get exact optional fields from `/openapi.yaml`.

## Runs

Send a prompt:

```bash
curl -sS -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"input":{"type":"text","text":"Inspect the project and summarize the risks."}}' \
  "$BASE/sessions/<session_id>/runs"
```

Override agent, model, skills, custom tools, or MCP for one run only when needed:

```json
{
  "input": { "type": "text", "text": "Use the kiliax-cli skill to inspect this workspace." },
  "overrides": {
    "agent": "general",
    "skills": {
      "default_enable": false,
      "overrides": [{ "id": "kiliax-cli", "enable": true }]
    }
  }
}
```

Poll a run:

```bash
curl -sS -H "$AUTH" "$BASE/runs/<run_id>"
```

Cancel with `POST /runs/<run_id>/cancel`.

## Messages And Events

Read recent visible messages:

```bash
curl -sS -H "$AUTH" "$BASE/sessions/<session_id>/messages?limit=20"
```

Stream session events when the caller supports SSE:

```bash
curl -N -H "$AUTH" "$BASE/sessions/<session_id>/events/stream"
```

Use polling when the caller cannot consume SSE cleanly.
