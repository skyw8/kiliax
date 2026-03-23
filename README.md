
Kiliax is a high-performance, cross-platform AI agent tool (Rust).

- Design: `docs/design.md`
- Tooling (skills / tools / MCP): `docs/tooling.md`

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
```
