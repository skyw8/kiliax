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

### crates/kiliax-core

核心库：配置、LLM(OpenAI-compatible)、agents、prompt、tools、runtime、session。

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
- `crates/kiliax-core/src/session.rs`: session 持久化（目录式：`meta.json` + `snapshot.json` + `events.jsonl`，默认写入 `<workspace>/.killiax/sessions/<session_id>/`）
- `crates/kiliax-core/src/tools/`: 工具系统
  - `mod.rs`: 权限/错误类型；导出 `ToolEngine`
  - `builtin.rs`: 内置工具 `read/write/shell` 的 schema + 执行（路径约束、shell argv allowlist）
  - `engine.rs`: 工具路由与统一执行（builtin vs MCP），以及 MCP 工具 definitions 注入
  - `mcp.rs`: MCP stdio hub（连接 server、列出 tools、`mcp__<server>__<tool>` 命名空间、调用工具）
  - `skills.rs`: skills 发现（扫描 roots；解析 `SKILL.md` YAML front matter 的 `name/description`；剥离 front matter 得到正文）

### crates/kiliax-tui

TUI crate（ratatui）入口占位，后续承载交互式 UI 与 runtime 事件渲染。

- `crates/kiliax-tui/Cargo.toml`: 依赖 `kiliax-core`
- `crates/kiliax-tui/src/main.rs`: 入口（当前为 stub）

## ATTENTION

- 修改完代码后，更新AGENTS.md中的arch部分，只用说明这些核心代码部分
- 优先遵循最佳实践，不用考虑兼容性，遗留代码大胆重构
- 不要修改TODO.md
