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

The install script will detect if you already have kiliax installed and update it to the latest version automatically. If already up to date, it will skip the installation.

To force reinstall (even if same version):
```bash
# macOS/Linux
FORCE=1 curl -fsSL https://raw.githubusercontent.com/skyw8/kiliax/master/install.sh | bash

# Windows
$env:FORCE=1; iwr -useb https://raw.githubusercontent.com/skyw8/kiliax/master/install.ps1 | iex
```

### Manual Install

Download the latest binary for your platform from [GitHub Releases](https://github.com/skyw8/kiliax/releases) and extract it to a directory in your PATH.

### Build from Source

```bash
git clone https://github.com/skyw8/kiliax.git
cd kiliax
cargo build --release -p kiliax
```

## usage
session control server

Manage the optional background `kiliax-server` (REST + SSE/WS) with:

```bash
# tui
kiliax

# server
kiliax server start
kiliax server stop
kiliax server restart
```


## build&run

```bash
cargo run -p kiliax
cargo run -p kiliax -- server start
# http://127.0.0.1:8123/docs
curl http://127.0.0.1:8123/v1/openapi.yaml > openapi.yaml

cd workspace 
cargo run -p kiliax --manifest-path=../Cargo.toml
cargo run -p kiliax --manifest-path=../Cargo.toml -- server start
```

demo example
```bash
cargo run -p kiliax-core --example chat_hello
cargo run -p kiliax-core --example stream_chat
cargo run -p kiliax-core --example agent_loop
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

This project is inspired by and built in the spirit of the following open-source AI coding agents:

- **[OpenAI Codex CLI](https://github.com/openai/codex)** - OpenAI's official Rust-based terminal coding agent
- **[OpenCode](https://opencode.ai/)** - Open-source, model-agnostic CLI coding agent supporting 75+ LLM providers
