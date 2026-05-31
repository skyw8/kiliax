# CLI

Use `ki`.

## Open Or Start

Run from the intended workspace root:

```bash
ki
```

This ensures the background server is running, prints an authenticated Web UI URL, and opens the browser when possible. On first run, it silently creates `~/.kiliax/kiliax.yaml` from the bundled template.

## Server Lifecycle

```bash
ki server start
ki server stop
```

`start` has the same ensure-and-open behavior as `ki`.

## Foreground Server

Use foreground mode for logs, fixed ports, or intentional remote binding:

```bash
ki server run --host 127.0.0.1 --port 8123 --workspace-root . --config ~/.kiliax/kiliax.yaml --token <token>
```

Options:

- `--workspace-root <dir>` defaults to the current directory.
- `--config <path>` defaults to auto-detected `kiliax.yaml`.
- `--token <token>` is required bearer/web auth for foreground runs.

## Goal CLI

For local session goal state:

```bash
ki goal get --session <session_id>
ki goal set --session <session_id> Finish the requested migration end to end
ki goal clear --session <session_id>
```
