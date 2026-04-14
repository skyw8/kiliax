# Refactor Plan: High Cohesion, Low Coupling (2026-04-14)

本文件按“一个问题一个问题”的形式，列出当前项目在 **high cohesion / low coupling / KISS / less is more** 目标下的主要不足，并给出对应的重构方案（包含必要的示例代码/目录结构）。

> 备注：仓库已有一次偏“功能正确性 + 性能风险”的重构落地记录，见 `docs/2026-04-03-refactor-report.md`。本文更关注**结构性内聚/耦合**与长期可维护性。

---

## 目录

- P0-1：`kiliax-server` 的 `state.rs` 是 God file（低内聚）
- P0-2：`LiveSession` 直接持有 `api::*` 类型作为内部状态（高耦合）
- P0-3：协议类型被放在 `kiliax-core::llm`，形成“耦合根”（低耦合目标失败）
- P0-4：路径展开/校验规则在 server 与 cli 重复实现（重复 + 语义漂移风险）
- P0-5：MCP enablement 覆盖逻辑重复（三份），且校验语义不统一
- P0-6：`kiliax-server/src/http/mod.rs` 路由与 handler 全集中（低内聚）
- P0-7：`kiliax-cli/src/app.rs` 过大且混合 UI 与业务（低内聚）
- P0-8：`web/src/app.tsx` 单文件承担全部 UI/状态/连接（低内聚）
- P1-9：`kiliax-server` 的 `runner.rs` 承担 CLI 解析（层级职责混用）
- P1-10：`kiliax-core::telemetry` 通过全局可变状态注入配置（隐藏耦合）
- P1-11：`kiliax-core/src/config.rs` 同时包含类型/默认值/IO/校验/解析（低内聚）

## 目标与约束

### 目标

1) 降低跨层耦合（server/http ↔ server/state ↔ core），让“改 API/改 UI/改 provider 适配”不互相牵连。  
2) 提升模块内聚：一个模块只负责一个方向的事情（配置、会话、事件、工具、UI）。  
3) 减少重复：同一规则（路径校验、MCP 覆盖）只有一个实现与一套语义。  
4) 保持简单：优先“搬家/切分/收敛重复”的重构，不引入复杂框架。

### 约束（来自项目规范）

- 所有 UI 文案默认 English（重构中新增/改动的 UI string 仍保持英文）。
- 不维护旧接口/旧代码：拆分完成后，重复实现应直接删除，避免“双路逻辑”长期共存。

---

## 总览：当前热点与耦合点（证据）

- `crates/kiliax-server/src/state.rs:1`：3653 行，承担过多职责（会话、事件、workspace、tools、幂等、队列、runner 相关）。  
- `crates/kiliax-server/src/http/mod.rs:1`：1301 行，路由注册 + handler + 静态资源 + middleware 全堆一起。  
- `crates/kiliax-cli/src/app.rs:1`：3343 行，TUI state / rendering / 输入 / 流式拼装 / 设置逻辑集中。  
- `web/src/app.tsx:1`：4405 行，UI + 状态 + WS + helper 全集中。
- `crates/kiliax-core/src/llm.rs:969`：`Message/ToolCall/ToolDefinition/TokenUsage` 等“协议类型”与 LLM transport/provider 适配混在同一模块，成为跨 crate 的耦合根。

---

## P0-1：`kiliax-server` 的 `state.rs` 是 God file（低内聚）

### 问题

`crates/kiliax-server/src/state.rs:1` 同时包含：

- `ServerState` 生命周期 + 配置热更新
- sessions cache + idle TTL 清理/限制（live session limit）
- 幂等缓存（idempotency）
- session 创建/派生/fork + workspace 处理
- `LiveSession`：run 队列、执行 worker、事件广播/落盘、settings 脏写回
- 工具引擎 `ToolEngine` 初始化与按 session 覆盖

这种文件形态会导致：

- 任何小改动都要加载大量上下文（认知成本高）。
- 结构演进困难：难以明确“谁依赖谁/边界在哪”。
- 代码复用/测试变得更难（内部 helper 互相耦合）。

### 证据（引用代码）

`ServerState` 定义与字段职责混合：`crates/kiliax-server/src/state.rs:27`

```rust
pub struct ServerState {
    pub workspace_root: PathBuf,
    pub config_path: PathBuf,
    pub config: Arc<ArcSwap<Config>>,
    pub token: Option<String>,
    pub store: FileSessionStore,
    pub runs_dir: PathBuf,
    pub tools_for_caps: ToolEngine,
    pub shutdown: Arc<Notify>,
    runner_enabled: bool,
    sessions: Mutex<HashMap<String, LiveSessionEntry>>,
    idempotency: Mutex<HashMap<String, (String, u64)>>,
}
```

`create_session_inner` 同时处理 settings patch、workspace root、tools init、preamble build、store create：`crates/kiliax-server/src/state.rs:843`

```rust
async fn create_session_inner(&self, req: api::SessionCreateRequest) -> Result<api::Session, ApiError> {
    let config = self.config_snapshot();
    let mut settings = default_settings(config.as_ref(), None)?;
    // ... apply_settings_patch / validate / workspace mkdir ...
    let cfg_for_tools = config_with_mcp_overrides(config.as_ref(), &settings.mcp.servers)?;
    let tools = ToolEngine::new(&workspace_root, cfg_for_tools);
    // ... build_preamble / store.create ...
}
```

`LiveSession` 结构体同时管理 session/settings/status/tools/events/queue：`crates/kiliax-server/src/state.rs:2180`

```rust
pub struct LiveSession {
    session: Mutex<SessionState>,
    settings: Mutex<api::SessionSettings>,
    tools: Mutex<ToolEngine>,
    status: Mutex<api::SessionStatus>,
    queue: Mutex<VecDeque<QueuedRun>>,
    events_tx: broadcast::Sender<api::Event>,
    // ...
}
```

### 重构方案（最小可行，推荐）

**目标**：拆分成多个文件/模块，但不改变对外行为；先做“搬家 + 清理依赖方向”，再做“抽象/优化”。

#### 方案 A：按职责拆成 `state/` 子模块（推荐）

建议第一阶段仅拆分文件（保持类型/函数签名尽量不变）：

```
crates/kiliax-server/src/state/
  mod.rs                 // pub use ServerState; 统一入口
  server_state.rs        // ServerState + new/config_snapshot/limits/idempotency
  live_session.rs        // LiveSession + QueuedRun + worker loop
  settings.rs            // default_settings/apply_settings_patch/normalize_settings/skills helpers
  events.rs              // durable/ephemeral emit + ring buffer + persistence helpers
  tool_context.rs        // ToolEngine 创建 + set_extra_workspace_roots + mcp overrides glue
```

**迁移策略**（可逐步落地）：

1) 新建 `state/mod.rs`，把现有 `state.rs` 内容逐段 move 进去（编译通过为止）。  
2) 先移动“纯 helper”（不依赖太多 struct 字段）到 `settings.rs/events.rs`，再移动 `LiveSession`。  
3) `ServerState` 只保留 orchestrator 方法：load/ensure_live/create/fork/config_updated 等。

**验收标准**：

- `crates/kiliax-server/src/state.rs` 不再存在（或仅保留 `mod state;` 并 re-export）。
- `cargo test -p kiliax-server` 通过（至少覆盖 sessions/events/run 相关测试）。

---

## P0-2：`LiveSession` 直接持有 `api::*` 类型作为内部状态（高耦合）

### 问题

`LiveSession` 的内部状态直接用 `crate::api` 的 DTO（wire schema）存储：  
`crates/kiliax-server/src/state.rs:2187`（`settings: Mutex<api::SessionSettings>`）、`crates/kiliax-server/src/state.rs:2194`（`status: Mutex<api::SessionStatus>`）、`crates/kiliax-server/src/state.rs:2201`（`events_tx: broadcast::Sender<api::Event>`）。

这会导致：

- API schema 的字段改动会强制波及运行时状态/逻辑（本应是“边界层”变化）。
- 内部类型使用 `String` 表示路径等，会在各处重复 parse/validate/canonicalize。
- 长期演化会出现“为了 API 兼容而扭曲内部模型”的趋势。

### 证据（引用代码）

`crates/kiliax-server/src/state.rs:2186`

```rust
pub struct LiveSession {
    session: Mutex<SessionState>,
    settings: Mutex<api::SessionSettings>,
    // ...
    status: Mutex<api::SessionStatus>,
    // ...
    events_tx: broadcast::Sender<api::Event>,
    events_ring: Mutex<VecDeque<api::Event>>,
}
```

### 重构方案（推荐）

**目标**：server 内部使用“领域类型”（domain/state model），API DTO 只出现在 handler 层（或最后一步组装 response 时）。

#### 方案 A：新增 `domain::*`，并在边界做显式转换

新增内部类型（示意/伪代码，字段请以实际 `api::*` 为准做适配）：

```rust
// crates/kiliax-server/src/domain/session_settings.rs
#[derive(Debug, Clone)]
pub struct SessionSettings {
    pub agent: String,
    pub model_id: String,
    pub workspace_root: std::path::PathBuf,
    pub extra_workspace_roots: Vec<std::path::PathBuf>,
    pub mcp_servers: Vec<McpServerSetting>,
    pub skills: Option<kiliax_core::config::SkillsConfig>,
}

impl From<&SessionSettings> for crate::api::SessionSettings {
    fn from(v: &SessionSettings) -> Self {
        Self {
            agent: v.agent.clone(),
            model_id: v.model_id.clone(),
            workspace_root: v.workspace_root.display().to_string(),
            extra_workspace_roots: v.extra_workspace_roots.iter().map(|p| p.display().to_string()).collect(),
            // skills/mcp 等字段按实际 api types 填充
            // skills: ...
            // mcp: ...
        }
    }
}
```

然后把 `LiveSession` 内部字段改为 `domain::SessionSettings/domain::SessionStatus/domain::Event`：

- handler 读取 request DTO → 转换为 domain patch → 业务逻辑只处理 domain。
- 返回 response 时 domain → api DTO（一次性转换）。

#### 落地步骤（建议拆 PR）

1) 新增 `crates/kiliax-server/src/domain/`（或 `domain.rs`）并定义 `SessionSettings/SessionStatus/Event` 的最小集合。  
2) 在 domain 层实现 `From/TryFrom` 显式转换（先保证“能编译、能跑”）。  
3) 修改 `LiveSession` 字段类型，优先从 `settings/status/events` 这三类下手。  
4) 修改 HTTP handler：request DTO → domain patch；response 末端再 domain → DTO。  
5) 删除 `state` 内对 `api::*` 的直接依赖（只保留转换层依赖）。

**验收标准**：

- `LiveSession` 不再出现 `api::SessionSettings/api::SessionStatus/api::Event` 字段。
- `api.rs` 的字段变更不应强迫修改 `state/live_session.rs` 的内部逻辑（只改转换层即可）。

---

## P0-3：协议类型被放在 `kiliax-core::llm`，形成“耦合根”（低耦合目标失败）

### 问题

`Message/ToolCall/ToolDefinition/TokenUsage` 等是跨模块的“核心协议类型”，但目前定义在 `crates/kiliax-core/src/llm.rs`（与 transport/provider quirks 同文件）。

结果是：

- session、tools、prompt 等模块都必须依赖 `llm`（即使它们并不关心 HTTP/streaming/provider 细节）。
- `llm.rs` 变成巨型核心依赖点，任何修改都容易产生“蝴蝶效应”。

### 证据（引用代码）

协议类型定义在 `crates/kiliax-core/src/llm.rs:969`（ToolCall）与 `crates/kiliax-core/src/llm.rs:1095`（Message）：

```rust
pub struct ToolCall { /* ... */ }
pub struct ToolDefinition { /* ... */ }
pub struct TokenUsage { /* ... */ }
pub enum Message { /* ... */ }
```

其他模块直接依赖 `llm::Message`：

- `crates/kiliax-core/src/session.rs:11`：`use crate::llm::Message;`
- `crates/kiliax-core/src/tools/engine.rs:11`：`use crate::llm::{Message, ToolCall, ToolDefinition};`
- `crates/kiliax-core/src/prompt.rs:4`：`use crate::llm::{Message, ToolDefinition, UserMessageContent};`

### 重构方案（推荐）

**目标**：把“协议类型”从“LLM transport/provider 适配”里剥离出来，让依赖方向更清晰。

#### 方案 A：新增 `kiliax-core::protocol`（最少文件版本）

新增文件 `crates/kiliax-core/src/protocol.rs`，迁移这些类型：

- `Message`
- `ToolCall/ToolDefinition/ToolChoice`
- `UserMessageContent/UserContentPart`
- `TokenUsage`

并把 `llm.rs` 内相关引用改成：

```rust
// crates/kiliax-core/src/llm.rs
use crate::protocol::{
    Message, TokenUsage, ToolCall, ToolChoice, ToolDefinition, UserContentPart, UserMessageContent,
};
```

其他模块也改为依赖 `protocol`：

```rust
// crates/kiliax-core/src/session.rs
use crate::protocol::Message;
```

#### 落地步骤（建议拆 PR）

1) 新增 `crates/kiliax-core/src/protocol.rs`，把 `llm.rs` 中的协议类型“剪切”过去（不保留旧位置）。  
2) 全仓库替换引用：`use crate::llm::Message` → `use crate::protocol::Message`（以及 Tool* 等）。  
3) `llm.rs` 专注 transport/provider 适配：只 `use crate::protocol::*`，不再定义协议类型。  
4) `cargo test -p kiliax-core` 回归（重点：序列化、session 读写、工具调用历史修复等路径）。

**验收标准**：

- `kiliax-core::llm` 不再导出 `Message/ToolCall/...`（全仓库直接替换引用，不保留旧 re-export）。
- `session/tools/prompt` 的 `use crate::llm::Message` 全部消失，改为 `protocol`。

---

## P0-4：路径展开/校验规则在 server 与 cli 重复实现（重复 + 语义漂移风险）

### 问题

server 与 cli 都需要处理 `~` 展开、绝对路径检查、禁止 `..`，但当前重复实现且语义不同：

- server：`crates/kiliax-server/src/infra.rs:32` `expand_tilde()`，`crates/kiliax-server/src/infra.rs:44` `validate_client_workspace_root()`（不检查存在性），`crates/kiliax-server/src/infra.rs:62` `validate_client_extra_workspace_roots()`（检查存在性并 canonicalize）。
- cli：`crates/kiliax-cli/src/app.rs:2666` `expand_tilde_path()`，`crates/kiliax-cli/src/app.rs:2678` `validate_extra_workspace_root()`（检查存在性并 canonicalize）。

长期风险：

- UI/Server 对同一输入给出不同错误与行为（“为什么 CLI 能加但 Web/Server 不行？”）。
- 安全策略（允许哪些路径）难以统一验证。

### 证据（引用代码）

server：`crates/kiliax-server/src/infra.rs:32`

```rust
fn expand_tilde(path: &str) -> Result<PathBuf, ApiError> { /* ... */ }
pub(crate) fn validate_client_workspace_root(input: &str) -> Result<PathBuf, ApiError> { /* ... */ }
```

cli：`crates/kiliax-cli/src/app.rs:2666`

```rust
fn expand_tilde_path(path: &str) -> Result<PathBuf> { /* ... */ }
fn validate_extra_workspace_root(input: &str) -> Result<PathBuf> { /* ... */ }
```

### 重构方案（推荐）

**目标**：把“路径安全策略”收敛到 `kiliax-core` 的一个小模块（纯函数），server/cli 各自补充与环境相关的检查（存在性、canonicalize）。

#### 方案 A：`kiliax-core::path_policy`（示例）

```rust
// crates/kiliax-core/src/path_policy.rs
#[derive(Debug, thiserror::Error)]
pub enum PathPolicyError {
    #[error("failed to resolve home dir")]
    HomeDir,
    #[error("path must be an absolute path")]
    NotAbsolute,
    #[error("path must not contain `..`")]
    HasParentDir,
}

pub fn expand_tilde(input: &str) -> Result<std::path::PathBuf, PathPolicyError> { /* shared */ }

pub fn validate_abs_no_parent(p: &std::path::Path) -> Result<(), PathPolicyError> {
    if !p.is_absolute() { return Err(PathPolicyError::NotAbsolute); }
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(PathPolicyError::HasParentDir);
    }
    Ok(())
}
```

落地方式：

- server：`validate_client_workspace_root()` 变成薄包装（`expand_tilde` + `validate_abs_no_parent` + 业务需要时 `create_dir_all`）。
- cli：`validate_extra_workspace_root()` 复用上述基础校验，再做 `metadata/is_dir/canonicalize`。

#### 落地步骤（建议拆 PR）

1) 在 `kiliax-core` 新增 `path_policy.rs`（或合入 `utils/path.rs`），提供纯函数与错误类型。  
2) server：将 `infra.rs` 的 `expand_tilde/validate_client_workspace_root` 改为调用 core；保持现有 `ApiError` 文案不变。  
3) cli：将 `expand_tilde_path/validate_extra_workspace_root` 改为调用 core；保留 CLI 特有的存在性检查。  
4) 删除重复实现（不保留旧函数），避免“双路规则”长期共存。

**验收标准**：

- server/cli 不再各自维护 tilde 展开逻辑；基础规则只有一份实现。

---

## P0-5：MCP enablement 覆盖逻辑重复（三份），且校验语义不统一

### 问题

当前至少有三处逻辑在做“从 config 得到 session MCP servers / 把 session overrides 写回 config”：

- cli main：`crates/kiliax-cli/src/main.rs:79`、`crates/kiliax-cli/src/main.rs:93`
- cli app：`crates/kiliax-cli/src/app.rs:38`、`crates/kiliax-cli/src/app.rs:50`
- server：`crates/kiliax-server/src/state.rs:1772` `config_with_mcp_overrides()`

重复不仅浪费维护成本，还会导致边界行为不一致（例如 unknown server id 的处理）。

### 证据（引用代码）

cli main：`crates/kiliax-cli/src/main.rs:79`

```rust
fn session_mcp_servers_from_config(config: &Config) -> Vec<SessionMcpServerSetting> { /* ... */ }
fn config_with_session_mcp_overrides(base: &Config, overrides: &[SessionMcpServerSetting]) -> Config { /* ... */ }
```

cli app：`crates/kiliax-cli/src/app.rs:38`

```rust
fn session_mcp_servers_from_config(config: &Config) -> Vec<SessionMcpServerSetting> { /* duplicate */ }
fn config_with_session_mcp_overrides(base: &Config, overrides: &[SessionMcpServerSetting]) -> Config { /* duplicate */ }
```

server：`crates/kiliax-server/src/state.rs:1772`

```rust
fn config_with_mcp_overrides(base: &Config, servers: &[api::McpServerSetting]) -> Result<Config, ApiError> { /* ... */ }
```

### 重构方案（推荐）

**目标**：把“覆盖逻辑（纯变换）”放进 `kiliax-core`，server 额外做“unknown id 校验/错误映射”。

#### 方案 A：核心提供纯函数（不带 HTTP error）

```rust
// crates/kiliax-core/src/mcp_overrides.rs
pub fn apply_mcp_enable_overrides(
    cfg: &mut crate::config::Config,
    overrides: impl IntoIterator<Item = (String, bool)>,
) -> Result<(), UnknownMcpServer> { /* ... */ }

#[derive(Debug, thiserror::Error)]
#[error("mcp server not found: {0}")]
pub struct UnknownMcpServer(pub String);
```

然后：

- cli：直接调用（UI 上显示英文错误）。
- server：把 `UnknownMcpServer` 映射为 `ApiErrorCode::McpServerNotFound`。

#### 落地步骤（建议拆 PR）

1) 在 `kiliax-core` 新增 `mcp_overrides.rs`（或 `config/mcp_overrides.rs`），实现纯函数 + error。  
2) server：把 `state.rs:1772` 的 `config_with_mcp_overrides` 改为调用 core，并在边界映射为 `ApiError`。  
3) cli：删掉 `main.rs/app.rs` 的 duplicate helper，统一调用 core。  
4) 补一个单元测试覆盖 unknown server id 的行为（core 层），避免后续漂移。

**验收标准**：

- cli main/app 不再有重复函数；server 的覆盖函数变薄（或直接复用 core 逻辑）。

---

## P0-6：`kiliax-server/src/http/mod.rs` 路由与 handler 全集中（低内聚）

### 问题

`crates/kiliax-server/src/http/mod.rs:68` 的 `build_app()` 已经是“路由注册中心”，同时文件内还塞了大量 handler/middleware/static serving 逻辑（总计 1301 行）。

结果：

- 任何单个 endpoint 的变更都要碰这个大文件（review/merge 冲突概率高）。
- handler 很难按领域分组测试/复用。

### 证据（引用代码）

`crates/kiliax-server/src/http/mod.rs:68`

```rust
pub fn build_app(state: Arc<ServerState>) -> Router {
    let v1 = OpenApiRouter::<Arc<ServerState>>::default()
        .routes(routes!(create_session, list_sessions))
        .routes(routes!(get_config, put_config))
        // ... many routes ...
        .routes(routes!(stream_events_ws))
        .route_layer(http_trace_layer());
    // ...
}
```

### 重构方案（推荐）

**目标**：`http/mod.rs` 只保留 router wiring；handler 按领域拆分为小文件，减少耦合与冲突面。

#### 方案 A：`http/handlers/*`（示意）

```
crates/kiliax-server/src/http/
  mod.rs              // build_app + nest/merge + re-export handlers signatures
  middleware.rs       // auth/access log/trace layer
  handlers/
    sessions.rs       // create/list/get/delete/fork/patch_settings/save_defaults
    runs.rs           // create_run/get_run/cancel_run
    config.rs         // get/put config, providers/runtime/skills/mcp patch
    events.rs         // list_events + sse/ws streaming
    fs.rs             // fs_list + open_workspace
    admin.rs          // get_admin_info/stop_server
  web.rs              // serve_web + dist/embedded selection (如需)
```

实现策略：

- 先把 handler 函数“原样 move”到对应文件（不改签名、不改逻辑）。
- `http/mod.rs` 通过 `mod handlers; use handlers::sessions::*;` 挂载路由。

#### 落地步骤（建议拆 PR）

1) 新建 `http/middleware.rs` 与 `http/handlers/` 目录，先只搬 `create_session/list_sessions` 等一小组 endpoints。  
2) 每搬一组 endpoints 都保证编译通过并跑 `cargo test -p kiliax-server`。  
3) 最后把 `serve_web`/静态资源相关迁移到 `http/web.rs`（如仍在 `http/mod.rs`）。  
4) 删除 `http/mod.rs` 中已搬走的 handler 实现，确保没有“旧实现残留”。

**验收标准**：

- `http/mod.rs` 行数显著下降（目标：< 300 行）。
- 路由变更不再频繁触碰大段无关 handler。

---

## P0-7：`kiliax-cli/src/app.rs` 过大且混合 UI 与业务（低内聚）

### 问题

`crates/kiliax-cli/src/app.rs:1` 既负责：

- UI state machine（如 `UiMode`）
- 数据结构（stream collector）
- 与 core runtime 的事件消费
- 设置编辑（model/mcp/workspace roots）
- 渲染/样式等

而且还包含与 `main.rs` 重复的 MCP override helper（`crates/kiliax-cli/src/app.rs:38`）。

### 证据（引用代码）

重复 helper：`crates/kiliax-cli/src/app.rs:38`

```rust
fn session_mcp_servers_from_config(config: &Config) -> Vec<SessionMcpServerSetting> { /* ... */ }
```

路径校验：`crates/kiliax-cli/src/app.rs:2678`

```rust
fn validate_extra_workspace_root(input: &str) -> Result<PathBuf> { /* ... */ }
```

### 重构方案（推荐）

**目标**：把“纯逻辑/状态/渲染”拆开，让每个文件更聚焦；并删除重复 helper，改用 core 共享函数。

#### 方案 A：分层拆分（示意）

```
crates/kiliax-cli/src/app/
  mod.rs            // pub struct App + public API
  state.rs          // AppState/UiMode/Session view model
  reducer.rs        // apply(AppAction) -> state transition（纯逻辑，可测）
  view.rs           // render(state) -> ratatui widgets（尽量无副作用）
  stream.rs         // MarkdownStreamCollector 等流式拼装逻辑
  validate.rs       // CLI 专用校验（或改用 core::path_policy）
```

迁移策略：

- 先把 `MarkdownStreamCollector` 这类“纯逻辑”迁出，写单元测试（易切分）。
- 再把设置逻辑迁移到 reducer（纯函数），view 只读 state。

**验收标准**：

- `app.rs` 不再是单文件 3000+ 行。
- 相同规则（MCP/path）只在一个位置实现。

---

## P0-8：`web/src/app.tsx` 单文件承担全部 UI/状态/连接（低内聚）

### 问题

`web/src/app.tsx:1` 包含海量 import、常量、helper、状态管理、WS 处理与 UI 组件拼装（4405 行）。

这会导致：

- 任何 UI 局部改动都引起大文件 diff/冲突。
- 状态逻辑难以测试/复用（hook 与 UI 强耦合）。

### 证据（引用代码）

`web/src/app.tsx:1`（大量 import + 类型/常量 + helper 函数）

```ts
import React, { useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, wsUrl } from "./lib/api";
// ... many imports ...
const PINNED_SESSIONS_KEY = "kiliax:pinned_session_ids";
function splitModelId(modelId: string) { /* ... */ }
```

### 重构方案（推荐）

**目标**：提取“状态与连接”为 hooks/store；UI 组件拆分按功能聚合。

#### 方案 A：最少文件拆分（示意）

```
web/src/
  app.tsx                   // 只做 layout + route glue
  hooks/
    useWsEvents.ts          // WS 连接 + event dispatch
    useLocalStorageState.ts // pinned/sidebar 等通用 hook
  state/
    sessionStore.ts         // reducer + derived selectors
  components/
    sidebar/Sidebar.tsx
    chat/ChatView.tsx
    settings/SettingsDialog.tsx
```

示例：把 WS 逻辑迁移到 hook（简化 app.tsx）：

```ts
// web/src/hooks/useWsEvents.ts
// Event 类型来自 `web/src/lib/types.ts`
export function useWsEvents(sessionId: string | null, onEvent: (e: Event) => void) {
  useEffect(() => {
    if (!sessionId) return;
    const ws = new WebSocket(wsUrl(`/v1/sessions/${sessionId}/events/ws`));
    ws.onmessage = (msg) => onEvent(JSON.parse(msg.data));
    return () => ws.close();
  }, [sessionId, onEvent]);
}
```

#### 落地步骤（建议拆 PR）

1) 先抽“零风险 helper”：`splitModelId/modelLabel/stringifyUnknown` 等迁到 `web/src/lib/utils.ts` 或 `web/src/utils/*.ts`。  
2) 抽 WS 连接：实现 `useWsEvents` 并在 `app.tsx` 替换一小段逻辑（确保行为一致）。  
3) 引入 `sessionStore.ts`（reducer + selectors），逐步把 `useState` 组合迁移到 reducer。  
4) 最后拆 UI：Sidebar/Chat/Settings 从 `app.tsx` 拆出为组件。

**验收标准**：

- `app.tsx` 主要内容变成“布局 + glue”，不再承载大段 reducer/helper。
- 关键 UI 约束仍满足（例如左侧 session badge 单行不换行）。

---

## P1-9：`kiliax-server` 的 `runner.rs` 承担 CLI 解析（层级职责混用）

### 问题

`crates/kiliax-server/src/runner.rs` 包含 `parse_run_args()` 与 `print_run_help()`（CLI 关注点），且 `kiliax-cli` 直接调用它：`crates/kiliax-cli/src/main.rs:187`（通过 `kiliax_server::runner::*`）。

这会让：

- server crate 的 API 被迫围绕 CLI 形态组织；
- CLI 与 server 的依赖方向不够清晰。

### 证据（引用代码）

`crates/kiliax-server/src/runner.rs:18`

```rust
pub fn parse_run_args(args: &[String]) -> ServerRunOptions { /* ... */ }
pub fn print_run_help() { /* ... */ }
```

### 重构方案（推荐）

**目标**：server crate 只暴露“运行 server 的能力”；CLI crate 负责 argv/help。

#### 方案 A：server 暴露 `run_server(opts)`；CLI 自己解析 argv

- 保留 `kiliax_server::runner::run_server(opts)`（或移动到 `kiliax_server::lib` 更直接）。
- 把 `parse_run_args/print_run_help` 移到 `crates/kiliax-cli`（或新建 `cli_args.rs`）。

#### 落地步骤（建议拆 PR）

1) `kiliax-cli` 新增 `parse_server_run_args()` 与 help 输出，并直接构造 `ServerRunOptions`。  
2) 替换 `crates/kiliax-cli/src/main.rs` 内对 `kiliax_server::runner::parse_run_args/print_run_help` 的调用。  
3) 删除 `kiliax-server/src/runner.rs` 的 argv/help 相关函数（不保留旧入口）。

**验收标准**：

- `kiliax-server` 不再包含任何“打印帮助/解析 args”的函数。

---

## P1-10：`kiliax-core::telemetry` 通过全局可变状态注入配置（隐藏耦合）

### 问题

`crates/kiliax-core/src/telemetry.rs:8` 使用全局 `OnceLock<RwLock<Option<OtelCaptureConfig>>>` 存储 capture 配置。  
同时 `ToolEngine::new/set_config` 会设置它（见 `crates/kiliax-core/src/tools/engine.rs:57` 与 `crates/kiliax-core/src/tools/engine.rs:91`）。

这类全局状态带来：

- 构造对象产生隐式副作用（测试/并发环境更难推断）。
- 多 session/多 engine 可能互相覆盖 capture 行为（虽然当前可能“只有一个”，但架构上不稳）。

### 证据（引用代码）

`crates/kiliax-core/src/telemetry.rs:8`

```rust
static CAPTURE_CONFIG: OnceLock<RwLock<Option<OtelCaptureConfig>>> = OnceLock::new();
```

`crates/kiliax-core/src/tools/engine.rs:56`

```rust
pub fn new(workspace_root: impl Into<PathBuf>, config: crate::config::Config) -> Self {
    telemetry::set_capture_config(config.otel.enabled.then_some(config.otel.capture.clone()));
    // ...
}
```

`crates/kiliax-core/src/tools/engine.rs:91`

```rust
pub fn set_config(&self, config: crate::config::Config) -> Result<(), ToolError> {
    telemetry::set_capture_config(config.otel.enabled.then_some(config.otel.capture.clone()));
    // ...
}
```

### 重构方案（可选，P1）

**目标**：把 capture 配置变成显式依赖（注入），避免全局可变状态。

#### 方案 A：引入 `TelemetryContext`（轻量句柄）

```rust
#[derive(Clone)]
pub struct TelemetryContext {
  capture: Option<OtelCaptureConfig>,
}

impl TelemetryContext {
  pub fn capture_enabled(&self) -> bool { self.capture.is_some() }
  // ...
}
```

然后 `LlmClient/ToolEngine` 持有 `TelemetryContext`，调用处显式传入。

#### 落地步骤（可选，建议拆 PR）

1) 新增 `TelemetryContext`，先只覆盖 `capture_enabled/capture_full/capture_text` 这一小套 API。  
2) `ToolEngine::new/set_config` 改为更新自身的 `TelemetryContext`，移除对全局 `set_capture_config` 的调用。  
3) `LlmClient` 从 `TelemetryContext` 读取 capture 配置（或由更上层注入）。  
4) 删除全局 `CAPTURE_CONFIG`（或把它降级为“仅用于默认值”的只读状态）。

**验收标准**：

- 构造 `ToolEngine` 不再修改全局状态。

---

## P1-11：`kiliax-core/src/config.rs` 同时包含类型/默认值/IO/校验/解析（低内聚）

### 问题

`crates/kiliax-core/src/config.rs` 混合了：

- 大量默认值 helper（如 `default_server_*`）`crates/kiliax-core/src/config.rs:9`
- 核心类型 `Config/ProviderConfig/ResolvedModel` `crates/kiliax-core/src/config.rs:332`
- 文件 IO（`load/load_from_path`）`crates/kiliax-core/src/config.rs:597`
- 校验/解析（`validate/resolve_model`）

结果：配置相关任何改动都需要在一个大文件里滚动。

### 证据（引用代码）

默认值 helper（只是一部分）：`crates/kiliax-core/src/config.rs:25`

```rust
fn default_server_max_live_sessions() -> usize { 64 }
fn default_server_live_session_idle_ttl_secs() -> u64 { 900 }
fn default_server_idempotency_max_entries() -> usize { 1024 }
fn default_server_idempotency_ttl_secs() -> u64 { 600 }
fn default_server_events_ring_size() -> usize { 4096 }
```

核心类型：`crates/kiliax-core/src/config.rs:332`

```rust
pub struct Config {
    pub default_model: Option<String>,
    pub default_agent: Option<String>,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub server: ServerConfig,
    pub otel: OtelConfig,
    // ...
    pub runtime: AgentRuntimeConfig,
    pub agents: AgentsConfig,
    pub mcp: McpConfig,
}
```

IO/解析/校验入口：`crates/kiliax-core/src/config.rs:597`

```rust
pub fn load() -> Result<LoadedConfig, ConfigError> {
    let cwd = std::env::current_dir().map_err(ConfigError::CurrentDir)?;
    let home_dir = dirs::home_dir();
    load_from_locations(&cwd, home_dir.as_deref())
}
```

### 重构方案（可选，P1）

**目标**：按“类型/解析/IO/校验”拆分，减少单文件认知负担。

#### 方案 A：`config/` 目录拆分（示意）

```
crates/kiliax-core/src/config/
  mod.rs          // pub use ...; 对外入口
  types.rs        // Config/ProviderConfig/ServerConfig/...（纯结构体）
  defaults.rs     // default_* helper
  io.rs           // load/find_config_path/candidate_paths
  validate.rs     // validate_config + helper
  model_route.rs  // resolve_model/split_qualified_model_id
```

#### 落地步骤（可选，建议拆 PR）

1) 先“只搬家不改逻辑”：把 `load/find_config_path/candidate_paths` 等 IO 移到 `config/io.rs`。  
2) 再把 `resolve_model` 相关迁到 `config/model_route.rs`（避免 types.rs 过胖）。  
3) 最后把 `validate` 迁到 `config/validate.rs`，让 `types.rs` 只保留结构体定义。  
4) 全部搬完后删掉旧的 `config.rs`（或保留极薄的 `mod.rs` 作为统一入口）。

**验收标准**：

- `config.rs` 不再是 1000+ 行单文件。
- `kiliax_core::config::*` 的对外 API 基本不变（或明确破坏性改动并同步全仓库更新）。

---

## 建议的落地顺序（Roadmap）

1) P0-1（server/state 拆分文件）+ P0-6（server/http 拆分文件）：先解决 review 冲突与认知负担。  
2) P0-2（domain types vs api DTO）：建立稳定边界，后续 API/UI 迭代成本立刻下降。  
3) P0-3（protocol 从 llm 剥离）：降低 core 的耦合根，后续 provider 适配更安全。  
4) P0-4/P0-5（收敛重复规则）：减少行为漂移与 bug 面。  
5) P0-7/P0-8（CLI/Web 拆分）：减小 UI 变更的冲突面。  
6) P1-9/P1-10/P1-11：最后做“更深的结构健康度”。

### 回归测试建议

- `cargo test -p kiliax-core`
- `cargo test -p kiliax-server`
- `cargo test -p kiliax`
- `cd web && bun run build`（或项目当前约定的构建命令）
