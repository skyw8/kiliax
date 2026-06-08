<h1 align="center">Kiliax</h1>

<p align="center">
  <img src="assets/kiliax.png" width="240" alt="Kiliax logo">
</p>

Kiliax is a high-performance, cross-platform AI agent tool (Rust).


## Installation

### Quick Install (Recommended)

**macOS / Linux:**
```bash
curl -fsSL https://raw.githubusercontent.com/skyw8/kiliax/master/install.sh | bash
```

**Windows (PowerShell):**
```powershell
iwr -useb https://raw.githubusercontent.com/skyw8/kiliax/master/install.ps1 | iex
```

The install script will detect if you already have `ki` installed and update it to the latest version automatically. If already up to date, it will skip the installation.

To force reinstall (even if same version):
```bash
# macOS/Linux
FORCE=1 curl -fsSL https://raw.githubusercontent.com/skyw8/kiliax/master/install.sh | bash

# Windows
$env:FORCE=1; iwr -useb https://raw.githubusercontent.com/skyw8/kiliax/master/install.ps1 | iex
```


## Quick Start
Manage the background server (REST + SSE/WS + Web UI) with:

```bash
# open or start the Web UI directly
ki

# server
ki server start
ki server stop
ki server restart
```

## Manual Install

Download the latest binary for your platform from [GitHub Releases](https://github.com/skyw8/kiliax/releases), rename it to `ki`, and place it in a directory in your PATH.

### Build from Source

```bash
git clone https://github.com/skyw8/kiliax.git
cd kiliax
cargo build --release -p kiliax
```

### build & run

```bash
# web
cd web
bun install & bun run build

cargo run -p kiliax -- server start
# http://127.0.0.1:8123/docs
curl http://127.0.0.1:8123/v1/openapi.yaml > openapi.yaml

```

## Nonfunctional Testing

Kiliax includes security checks, unit-level benchmarks, and server API load testing.

### Security

Run dependency and secret checks locally:

```bash
cargo audit
(cd web && bun audit)
gitleaks detect --source . --redact
```

The nonfunctional CI workflow also runs these checks and the server security boundary tests:

```bash
cargo test -p kiliax-server
```

### Benchmarks

Run unit-level hot-path benchmarks for core prompt, history, compaction, and session paging code:

```bash
cargo bench -p kiliax-core --bench core_hot_paths
```

### Server API Load Test

Install `oha` once:

```bash
cargo install oha --locked
```

Run the local authenticated server API load test:

```bash
REQUESTS=100 CONCURRENCY=10 scripts/perf/server-api-load.sh
```

The script builds or finds the `kiliax` binary, starts a temporary local server, then exercises:

- `GET /v1/capabilities`
- `POST /v1/sessions`
- `GET /v1/sessions`
- `GET /v1/sessions/{id}/messages`
- `POST /v1/sessions/{id}/runs`

Useful overrides:

```bash
KILIAX_BIN=/path/to/kiliax \
KILIAX_LOAD_HOST=127.0.0.1 \
KILIAX_LOAD_PORT=18123 \
KILIAX_LOAD_TOKEN=load-test-token \
REQUESTS=1000 \
CONCURRENCY=20 \
scripts/perf/server-api-load.sh
```

Write a full run log under `/tmp` when comparing results:

```bash
REQUESTS=100 CONCURRENCY=10 scripts/perf/server-api-load.sh \
  | tee /tmp/kiliax-server-api-load-results.txt
```

## observability (OpenTelemetry / Langfuse)

Kiliax exports OTEL logs/traces/metrics via OTLP (HTTP/gRPC). Configure it in `kiliax.yaml`:

```yaml
otel:
  enabled: true
  environment: dev
  otlp:
    # Langfuse OTLP ingest base endpoint (no /v1/traces suffix).
    endpoint: https://cloud.langfuse.com/api/public/otel
    protocol: http_protobuf
    headers:
      # Basic base64(public_key:secret_key)
      authorization: "Basic <...>"
  signals:
    traces: true
    logs: false
    metrics: false
```

Generate the auth header:
```bash
echo -n "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" | base64 | tr -d '\n'
```

## References

- **[Zilliax](https://hearthstone.blizzard.com/en-us/cards/49184-zilliax?set=wild&textFilte)** - Unity. Precision. Perfection.
- **[OpenAI Codex CLI](https://github.com/openai/codex)** - OpenAI's official Rust-based terminal coding agent
- **[OpenCode](https://opencode.ai/)** - Open-source, model-agnostic CLI coding agent supporting 75+ LLM providers
