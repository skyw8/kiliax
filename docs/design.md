# Kiliax 设计（core / agents / tools）

本文说明当前仓库的核心设计与模块边界，重点覆盖：配置加载、OpenAI-compatible LLM 调用（含 tool calling 与流式）、agents（不同权限与提示词）、以及工具系统（内置工具 / skills / MCP）。

> 代码主要集中在 `crates/kiliax-core/`；`crates/kiliax-tui/` 目前仅是未来 TUI 的入口占位。

## 1. 配置（YAML）

代码：`crates/kiliax-core/src/config.rs`

### 1.1 查找路径与优先级

按优先级从高到低依次读取首个存在的配置文件：

1) `./killiax.yaml`
2) `./.killiax/killiax.yaml`
3) `~/.killiax/killiax.yaml`

### 1.2 配置结构

- `providers: { <provider_name>: { base_url, api_key?, models[]? } }`
- `default_model: <provider>/<model>`（推荐）

模型 ID 采用完整形式 `<provider>/<model>`（例如 `moonshot_cn/kimi-k2-turbo-preview`）用于精准路由；发送到具体 provider 的 OpenAI-compatible API 时，仅会把 `<model>` 作为 `model` 字段传递。

当且仅当只配置了 1 个 provider 时，允许用 `<model>` 这种“省略 provider 的写法”。

示例：`killiax.example.yaml`

## 2. LLM 调用（async-openai）

代码：`crates/kiliax-core/src/llm.rs`

### 2.1 路由与 Client 构造

- `LlmClient::from_config(&Config, model_id: Option<&str>)`
  - `model_id` 为 `None` 时使用 `config.default_model`。
  - 通过 `Config::resolve_model()` 解析出 `ResolvedModel { provider, model, base_url, api_key }`。
- 为了支持不同 provider 的 `base_url`，实现了一个轻量的 `KiliaxOpenAIConfig`（实现 `async_openai::config::Config`），按路由拼接 URL，并在存在 `api_key` 时加上 `Authorization: Bearer ...`。

### 2.2 Tool calling 的数据结构

- `Message`：Developer/System/User/Assistant/Tool 五种角色
- `ToolDefinition`：OpenAI function/tool schema
- `ToolCall`：模型返回的工具调用（`id/name/arguments`，arguments 以 JSON 文本存储）
- `ToolChoice`：None/Auto/Required/Named

这些类型会被转换成 `async-openai` 的 request/response 类型，以实现 OpenAI-compatible tool calling。

### 2.3 非流式与流式

- `chat(req) -> ChatResponse`
- `chat_stream(req) -> ChatStream`
  - 输出 `ChatStreamChunk`，包含：
    - `content_delta`：增量文本
    - `tool_calls`：`ToolCallDelta`（支持在流式中拼接 tool call 的 `name/arguments`）

示例：`crates/kiliax-core/examples/stream_chat.rs`

## 3. prompts（提示词）

路径：`crates/kiliax-core/prompts/*.md`

设计点：

- prompts 与 Rust 代码解耦，便于迭代
- 通过 `include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), ...))` 编译进二进制/库，避免运行时路径问题，跨平台分发更稳

## 4. agents（Profile + 权限）

代码：`crates/kiliax-core/src/agents.rs`

### 4.1 AgentProfile

`AgentProfile` 描述一个 agent 的“行为边界”：

- `developer_prompt`：该 agent 的固定提示词
- `tools`：暴露给模型的工具集合（tool definitions）
- `permissions`：工具执行侧的权限（读写 / shell 约束）

### 4.2 预置 agent

- `plan`
  - 工具：`read` + `shell`
  - 权限：只读文件；`shell` 仅允许少量安全命令（argv 前缀 allowlist）
- `build`
  - 工具：`read` + `write` + `shell`
  - 权限：可读写文件；`shell` 全允许

注意：tools 是否“暴露给模型”与执行侧权限是两层控制；即使某个工具被暴露，执行仍会二次检查权限。

## 5. PromptBuilder（提示词组装）

代码：`crates/kiliax-core/src/prompt.rs`

`PromptBuilder` 负责把多个“稳定前缀”与对话消息组装为最终传给 LLM 的 `Vec<Message>`，以便后续做 token efficient / prompt caching：

- agent 固定提示词（来自 `AgentProfile.developer_prompt`，最终以 system message 发送以保证兼容性）
- 共享的工具使用规范提示词（`crates/kiliax-core/prompts/tools.md`）
- workspace root 上下文
- skills 元信息（可选，以 `<skills_instructions>` system message 注入：列出可用 skills + 使用规则；不内嵌 `SKILL.md` 正文，按需用 `read` 打开）
- 用户/助手/工具消息（对话历史）

## 6. 工具系统（builtin / skills / MCP）

文档：`docs/tooling.md`

代码入口：`crates/kiliax-core/src/tools/*`

- 内置工具：`read` / `write` / `shell`（`tools/builtin.rs`）
- 统一执行：`ToolEngine`（`tools/engine.rs`）
- skills 发现：`discover_skills()`（`tools/skills.rs`）
- MCP 外部工具：`McpHub`（`tools/mcp.rs`，stdio transport）

### 6.1 安全与可控性（关键约束）

- 文件类工具：
  - `write`：路径必须在 `workspace_root` 内；禁止 `..`
  - `read`：路径必须在 `workspace_root` 内或允许的 skills roots 内；禁止 `..`
- shell 工具：
  - plan agent 使用 allowlist（argv 前缀匹配）
  - build agent 允许全部（后续可再收紧）
- MCP 工具命名空间：
  - 暴露给模型的名字是 `mcp__<server>__<tool>`（避免 `/`、`.` 等非法字符）

## 7. AgentRuntime（执行闭环）

代码：`crates/kiliax-core/src/runtime.rs`

`AgentRuntime` 负责跑一个最小可用的 ReAct/tool-calling 闭环：

- 调用 LLM（`chat()` / `chat_stream()`）
- 收集 `tool_calls`
- 用 `ToolEngine` 执行工具并回填 `Message::Tool`
- 继续下一轮，直到不再产生 tool calls 或达到 `max_steps`

流式版本 `run_stream()` 会以 `AgentEvent` 形式发出：assistant delta、tool call、tool result、done 等事件，便于 TUI 渲染。

## 8. Session（持久化与恢复）

代码：`crates/kiliax-core/src/session.rs`

session 的目标是把一次 agent 运行的上下文（messages + 关键元信息）持久化到磁盘，以便崩溃后恢复/继续：

- 默认 project 目录：`<workspace>/.killiax/sessions/<session_id>/`
  - `session_id` 的目录名自带创建时间（UTC 时间戳前缀）
- 存储结构（混合 JSONL + snapshot）：
  - `events.jsonl`：append-only 事件日志（每行一个 JSON）
  - `snapshot.json`：周期性 checkpoint（加速加载）
  - `meta.json`：会话元信息（用于 list/展示）
- checkpoint 策略：默认每 32 条事件写一次 `snapshot.json`（可通过 `FileSessionStore::with_checkpoint_every()` 调整）
- 恢复方式：加载 `SessionState` 后，直接用 `state.messages` 作为下一次 `AgentRuntime` 的输入（可再追加新的 user message，并写入 events）

示例：`crates/kiliax-core/examples/agent_loop.rs`（自动保存；支持 `--resume <session_id>`）。

1) 读取配置，构造 `LlmClient`
2) 选择 `AgentProfile`（plan/build）
3) 创建 `ToolEngine`（可选接入 `McpHub`）
4) 用 `PromptBuilder` 组装初始 messages
5) （可选）创建 `FileSessionStore` + `SessionState`
6) 运行过程中把 `AssistantMessage/ToolResult/Finish/Error` 追加写入 `events.jsonl`（并按策略 checkpoint）
7) 用 `AgentRuntime` 执行 ReAct/tool-calling 循环

后续可以在 `kiliax-tui` 中把上述闭环做成交互式 TUI，并加入：

- subagents / delegation
- prompt caching（对固定 developer prompt、稳定系统上下文做缓存）
- 更细粒度的权限与审计（尤其是 shell / write）
