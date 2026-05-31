# Remote Usage

Design automation as if the Web UI may be connected to a remote `kiliax server`.

## Binding

For remote access, bind intentionally and use a strong token:

```bash
ki server run --host 0.0.0.0 --port 8123 --workspace-root /srv/project --config ~/.kiliax/kiliax.yaml --token <strong-token>
```

Expose the service only through trusted networking or a secure tunnel.

## Workspace Identity

The background daemon checks workspace root and config path when deciding whether an existing server matches the current request. Run `ki` from the workspace the agent should operate on.

## Filesystem

Use server-side APIs and paths for remote workflows. Browser-native file pickers see the browser client's filesystem, not the server's filesystem.

For exact filesystem endpoint shapes, fetch `/openapi.yaml`.
