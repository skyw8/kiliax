
Kiliax is a high-performance, cross-platform AI agent tool (Rust).

- Design: `docs/design.md`
- Tooling (skills / tools / MCP): `docs/tooling.md`

## Installation

### Quick Install (Recommended)

**macOS / Linux:**
```bash
curl -fsSL https://raw.githubusercontent.com/skyw8/kiliax/main/install.sh | bash
```

**Windows (PowerShell):**
```powershell
iwr -useb https://raw.githubusercontent.com/skyw8/kiliax/main/install.ps1 | iex
```

The install script will detect if you already have kiliax-tui installed and update it to the latest version automatically. If already up to date, it will skip the installation.

To force reinstall (even if same version):
```bash
# macOS/Linux
FORCE=1 curl -fsSL https://raw.githubusercontent.com/skyw8/kiliax/main/install.sh | bash

# Windows
$env:FORCE=1; iwr -useb https://raw.githubusercontent.com/skyw8/kiliax/main/install.ps1 | iex
```

### Manual Install

Download the latest binary for your platform from [GitHub Releases](https://github.com/skyw8/kiliax/releases) and extract it to a directory in your PATH.

### Build from Source

```bash
git clone https://github.com/skyw8/kiliax.git
cd kiliax
cargo build --release -p kiliax-tui
```

## run

```bash
cargo run -p kiliax-core --example chat_hello
cargo run -p kiliax-core --example stream_chat
cargo run -p kiliax-core --example agent_loop
```

## config

See `killiax.example.yaml`.

- `runtime.max_steps`: default max steps for all agents
- `agents.plan.max_steps` / `agents.general.max_steps`: per-agent overrides

## tui

```bash
cargo run -p kiliax-tui


cd workspace && cargo run -p kiliax-tui --manifest-path=../Cargo.toml
```

## References

This project is inspired by and built in the spirit of the following open-source AI coding agents:

- **[OpenAI Codex CLI](https://github.com/openai/codex)** - OpenAI's official Rust-based terminal coding agent
- **[OpenCode](https://opencode.ai/)** - Open-source, model-agnostic CLI coding agent supporting 75+ LLM providers

