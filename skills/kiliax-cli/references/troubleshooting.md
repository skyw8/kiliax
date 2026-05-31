# Troubleshooting

## Server State

The background server writes:

- `~/.kiliax/server.json` for host, port, token, pid, and start time.
- `~/.kiliax/server.jsonl` for daemon logs.

If `server.json` is stale, `ki server start` usually repairs it. If the server is unreachable, `ki server stop` removes stale state.

## Port In Use

Default port is `8123`. If no port is configured, Kiliax scans up to `8200`. If a fixed `server.port` is occupied by another process, choose a different port or stop the process.

## Token Mismatch

Symptoms:

- HTTP `401 Unauthorized`
- token mismatch on start or stop

Read the active token from `~/.kiliax/server.json`, or stop the old server with the correct token before starting a new one.

## Config Missing

`ki` creates `~/.kiliax/kiliax.yaml` from the template when no config is found.

## Unknown Endpoint Shape

Do not guess. Fetch the server's OpenAPI document:

```bash
curl -sS -H "$AUTH" "$BASE/openapi.yaml"
```
