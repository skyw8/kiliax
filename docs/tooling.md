# Tooling 设计（skills / tools / MCP）

本文描述 `kiliax-core` 当前的“工具系统”设计：内置工具（read/write/shell）、MCP 外部工具接入，以及 skills 的发现方式。目标是让不同 agent 拥有不同工具集合与权限，并能用 OpenAI-compatible tool calling 形成可控的执行闭环。

## 总览

- LLM 侧（`crates/kiliax-core/src/llm.rs`）
  - 定义对话消息 `Message`、工具定义 `ToolDefinition`、模型返回的 `ToolCall`（tool calling）。
  - 支持非流式 `chat()` 与流式 `chat_stream()`。
- Agent 侧（`crates/kiliax-core/src/agents.rs`）
  - `AgentProfile`：不同 agent 的 `developer_prompt`、可用 `tools`、以及 `permissions`。
  - 目前内置两类：`plan`（只读/受限命令）和 `build`（可读写/可执行）。
- 工具执行侧（`crates/kiliax-core/src/tools/*`）
  - `tools::builtin`：内置 `read` / `write` / `shell` 的 schema + 执行逻辑。
  - `tools::mcp`：通过 MCP（stdio transport）接入外部工具，并转换为 `ToolDefinition`。
  - `tools::engine::ToolEngine`：统一路由与执行入口，输出 `Message::Tool` 回填给模型。
  - `tools::skills`：发现 skills（目录扫描），提供内容以便后续注入提示词或工具集。

## tools：read / write / shell

内置工具位于 `crates/kiliax-core/src/tools/builtin.rs`：

- `read`
  - 输入：`{ "path": "relative/path" }`
  - 输出：文件内容（UTF-8）
  - 约束：必须在 `workspace_root` 内，禁止 `..`
- `write`
  - 输入：`{ "path": "...", "content": "...", "create_dirs": false }`
  - 输出：`ok`
  - 约束：必须在 `workspace_root` 内，禁止 `..`
- `shell`
  - 输入：`{ "argv": ["cmd","..."], "cwd": "optional/rel/path" }`
  - 输出：`exit_code/stdout/stderr` 文本
  - 约束：由 `Permissions.shell` 控制 allowlist 或全允许

权限模型位于 `crates/kiliax-core/src/tools/mod.rs`：

- `Permissions { file_read, file_write, shell }`
- `ShellPermissions`：
  - `DenyAll` / `AllowAll` / `AllowList(Vec<Vec<String>>)`
  - allowlist 的匹配规则是“argv 前缀匹配”（token 级精确匹配）。

## MCP：外部工具接入

MCP 接入位于 `crates/kiliax-core/src/tools/mcp.rs`：

- `McpHub::connect_stdio(McpServerConfig)`
  - 启动一个子进程 MCP server（stdio）
  - 初始化握手 `initialize`
  - 拉取 `tools/list`
  - 启动后台任务：把 transport 收到的 JSON-RPC 消息喂给 `client.handle_message()`（否则请求会卡住）
- `McpHub::tool_definitions() -> Vec<ToolDefinition>`
  - 把 MCP 的 `Tool { name, description, input_schema }` 转成 OpenAI-compatible `ToolDefinition`
  - 目前会跳过：
    - 名称包含非法字符（仅允许 `[A-Za-z0-9_-]`）
    - 名称过长导致最终 tool name > 64
- MCP tool name 命名空间
  - 对模型暴露的工具名格式：`mcp__<server>__<tool>`
  - 原因：OpenAI function/tool name 只允许字母数字、下划线、短横线；不能用 `/` 或 `.`

调用阶段由 `ToolEngine` 统一处理：

- 识别 `mcp__...` 的 tool call
- 解析参数 JSON（`ToolCall.arguments`）
- 调用 `McpHub::call_exposed_tool()` 并将返回渲染成文本 tool message

注意：当前使用 `modelcontextprotocol-client` 0.1.3，需要将 `mcp-protocol` 固定到 `=0.2.6` 以避免上游 semver 不兼容导致的编译问题（后续可替换为更成熟的 MCP SDK 或升级依赖）。

## skills：发现与加载

skills 发现位于 `crates/kiliax-core/src/tools/skills.rs`：

- 默认扫描 roots（按顺序）：
  - `<workspace>/skills/*/SKILL.md`
  - `<workspace>/.killiax/skills/*/SKILL.md`
  - `~/.killiax/skills/*/SKILL.md`
- `SKILL.md` 支持可选的 YAML front matter（推荐），用于提供元信息：
  - `name`：显示名
  - `description`：简短描述
  - 例如：
    ```md
    ---
    name: My Skill
    description: What this skill is for.
    ---
    # My Skill
    ...
    ```
- `discover_skills()` 返回的结构包含：
  - `id`：目录名（稳定标识）
  - `name/description`：来自 front matter（或从 markdown 的标题/首段推断）
  - `content`：已剥离 front matter 的 markdown 正文
- skills 可用于：
  - 注入到 agent 的 prompt（developer/system）
  - 作为“工具集/策略”的配置来源（启用哪些 MCP servers、哪些工具）

## 典型执行闭环（tool calling）

1) 选定 agent profile（例如 plan/build），构造 `ChatRequest`：
   - `messages`：对话上下文
   - `tools`：`profile.tools` +（可选）`mcp_hub.tool_definitions()`
2) LLM 返回 `Message::Assistant { tool_calls: Vec<ToolCall> }`
3) 对每个 `ToolCall`：
   - `ToolEngine::execute_to_message(&profile.permissions, &call)` 生成 `Message::Tool`
4) 把 assistant/tool 消息追加回 `messages`，继续下一轮 `chat()` / `chat_stream()`
