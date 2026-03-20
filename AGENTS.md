## description
一个高性能、强大、跨平台的AI Agent工具
subagents
token efficient
prompt caching


## tech stack

- rust
- [rust-sdk](https://github.com/modelcontextprotocol/rust-sdk) MCP
- [async-openai](https://github.com/64bit/async-openai) openai compatible API
- [ratatui](https://github.com/ratatui/ratatui) TUI


## arch

### repo root

- `Cargo.toml`: workspace 定义（成员 crates、公共依赖版本等）
- `Cargo.lock`: cargo 依赖锁定文件
- `README.md`: 最小使用说明（当前主要是 examples 入口）
- `TODO.md`: 需求/待办清单（**不要修改**）
- `killiax.example.yaml`: 配置示例（providers、default_model 等）
- `killiax.yaml`: 本地配置（已在 `.gitignore` 中忽略；可能包含密钥）
- `crates/`: Rust workspace 成员 crate
- `docs/`: 设计与模块说明文档
- `target/`: Rust 构建产物（自动生成）
- `tmp/`: 临时目录（自动生成/调试用）

### crates/kiliax-core

核心库：配置、LLM(OpenAI-compatible)、agents、tools、runtime。

- `crates/kiliax-core/Cargo.toml`: core 依赖（`async-openai`、MCP client 等）
- `crates/kiliax-core/prompts/`: agent 提示词（markdown，编译期 `include_str!`）
  - `plan.md`: plan agent 提示词（只读/受限命令）
  - `build.md`: build agent 提示词（可写/可执行）
  - `tools.md`: 通用工具使用规范（read/write/shell/mcp 命名空间）
- `crates/kiliax-core/examples/`: 可运行示例
  - `chat_hello.rs`: 非流式 chat 示例
  - `stream_chat.rs`: 流式 chat 示例（展示 delta 与 tool_call delta 合并）
  - `agent_loop.rs`: AgentRuntime + PromptBuilder 的闭环示例（流式输出 + 自动执行工具）
- `crates/kiliax-core/src/lib.rs`: 模块导出入口
- `crates/kiliax-core/src/config.rs`: 配置查找/解析（优先级路径）、provider/base_url/api_key、`<provider>/<model>` 路由
- `crates/kiliax-core/src/llm.rs`: 基于 `async-openai` 的 OpenAI-compatible 客户端封装；消息/工具定义；非流式与流式接口
- `crates/kiliax-core/src/agents.rs`: `AgentProfile`（plan/build）及其可用工具集合与权限模型
- `crates/kiliax-core/src/prompt.rs`: `PromptBuilder`（组装 system 前缀：agent prompt + tools 规范 + workspace root + skills + 对话消息）
- `crates/kiliax-core/src/runtime.rs`: `AgentRuntime`（ReAct/tool-calling 执行闭环：LLM→tool_calls→执行→回填→继续）
- `crates/kiliax-core/src/tools/`: 工具系统
  - `mod.rs`: 权限/错误类型；导出 `ToolEngine`
  - `builtin.rs`: 内置工具 `read/write/shell` 的 schema + 执行（路径约束、shell argv allowlist）
  - `engine.rs`: 工具路由与统一执行（builtin vs MCP），以及 MCP 工具 definitions 注入
  - `mcp.rs`: MCP stdio hub（连接 server、列出 tools、`mcp__<server>__<tool>` 命名空间、调用工具）
  - `skills.rs`: skills 发现（扫描 `skills/*/SKILL.md`、`.killiax/skills`、`~/.killiax/skills`）

### crates/kiliax-tui

TUI crate（ratatui）入口占位，后续承载交互式 UI 与 runtime 事件渲染。

- `crates/kiliax-tui/Cargo.toml`: 依赖 `kiliax-core`
- `crates/kiliax-tui/src/main.rs`: 入口（当前为 stub）

### docs

- `docs/design.md`: 总体设计与模块边界（config/llm/agents/tools/prompt/runtime）
- `docs/tooling.md`: 工具系统设计（builtin tools、skills、MCP、执行闭环）

## ATTENTION

- 不要修改TODO.md
