# HTTP API

After `ki` or `ki server start`, get API connection details from the printed URL or `~/.kiliax/server.json`.

```bash
cat ~/.kiliax/server.json
```

The file contains `host`, `port`, and `token`.

```text
BASE=http://<host>:<port>/v1
AUTH=Authorization: Bearer <token>
```

Use bearer auth on every API request when the token is set.

## Discovery

```bash
curl -sS -H "$AUTH" "$BASE/admin/info"
curl -sS -H "$AUTH" "$BASE/capabilities"
curl -sS -H "$AUTH" "$BASE/openapi.yaml"
```

Use `/v1/openapi.yaml` for exact request and response shapes before writing automation against a Kiliax version you have not inspected.

## Idempotency

Use `Idempotency-Key` for retried creates:

```bash
curl -sS -H "$AUTH" -H "Idempotency-Key: <stable-key>" ...
```

Apply it to:

- `POST /v1/sessions`
- `POST /v1/sessions/<session_id>/runs`

## Admin

```bash
curl -sS -X POST -H "$AUTH" "$BASE/admin/stop"
```

Prefer `ki server stop` for local shutdown; use HTTP stop when already controlling a foreground or remote server.
