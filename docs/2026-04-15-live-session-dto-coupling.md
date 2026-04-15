# LiveSession 与 API DTO 深度耦合问题说明

## 背景

当前 `crates/kiliax-server/src/state/live_session.rs` 和 `crates/kiliax-server/src/state/server_state.rs` 已经从“大文件”拆开，但内部职责还没有真正分层。

表面上看，`LiveSession` 是服务端会话运行时，`ServerState` 是全局状态容器；实际上它们仍然直接持有、返回、修改 `crate::api::*` 类型。也就是说，**HTTP 接口层的 DTO 已经渗透进了运行时内核**。

这会让“状态管理”和“接口协议”绑在一起：只要 API 字段改名、增删字段、调整序列化格式，运行时逻辑、持久化逻辑和测试都要跟着动。

---

## 问题一：`LiveSession` 内部状态直接存 `api::*`

### 现状代码

`LiveSession` 的核心字段现在就是 API DTO：

```rust
pub struct LiveSession {
    session: Mutex<SessionState>,
    settings: Mutex<api::SessionSettings>,
    status: Mutex<api::SessionStatus>,
    queue: Mutex<VecDeque<QueuedRun>>,
    events_tx: broadcast::Sender<api::Event>,
    events_ring: Mutex<VecDeque<api::Event>>,
}
```

对应文件：

- `crates/kiliax-server/src/state/live_session.rs`

### 为什么这是问题

`LiveSession` 本质上应该管理“一个活跃会话”的业务状态：

- 当前 session 的有效配置
- 当前运行态
- 队列里的 run
- 事件流
- 任务取消与恢复

但现在它保存的是“HTTP 返回体”：

- `api::SessionSettings`
- `api::SessionStatus`
- `api::Run`
- `api::Event`

这意味着：

1. **运行时逻辑被序列化格式污染**  
   运行时不应该关心 `serde`、`ToSchema`、`skip_serializing_if` 这类 API 层细节。

2. **内部演进空间变小**  
   例如要把 `SessionSettings` 拆成更小的内部结构，或者把事件拆成更稳定的领域事件模型，都会受到 `api.rs` 结构限制。

3. **测试变脆**  
   测试现在容易写成“检查某个 DTO 是否完全相等”，而不是“检查运行态是否正确”。

4. **跨入口复用困难**  
   CLI、TUI、HTTP、后续的自动化入口其实都需要会话运行时，但 DTO 绑定让 `LiveSession` 很难变成真正可复用的核心对象。

---

## 问题二：`LiveSession` 的公共方法也直接暴露 DTO

### 现状代码

`LiveSession` 的接口现在直接吃/吐 API 类型：

```rust
pub async fn settings_snapshot(&self) -> api::SessionSettings
pub async fn summary(&self) -> Result<api::SessionSummary, ApiError>
pub async fn snapshot(&self) -> Result<api::Session, ApiError>
pub fn subscribe_events(&self) -> broadcast::Receiver<api::Event>
pub async fn patch_settings(&self, patch: api::SessionSettingsPatch) -> Result<(), ApiError>
```

对应文件：

- `crates/kiliax-server/src/state/live_session.rs`

### 为什么这是问题

这会把“内部 API”和“外部 HTTP 协议”混成一层。

按高内聚、低耦合的目标，`LiveSession` 应该暴露的是“领域语义”：

- `SessionSnapshot`
- `SessionStatus`
- `SessionSettings`
- `Run`
- `SessionEvent`

而不是 `api::Session` / `api::SessionSettingsPatch` 这种 transport DTO。

现在的问题是：**调用方只要拿到 `LiveSession`，就默认被迫依赖 `api.rs` 的字段设计**。

这会造成两个后果：

- `LiveSession` 很难被非 HTTP 场景直接使用；
- `api.rs` 一旦为了 REST 或前端方便做调整，运行时接口也要跟着改。

---

## 问题三：`ServerState` 仍然在做 DTO 拼装与映射

### 现状代码

`ServerState` 里仍然保留大量 API 映射逻辑，例如：

```rust
fn resolve_session_settings(
    meta: &SessionMeta,
    config: &Config,
    fallback_workspace_root: &Path,
) -> Result<api::SessionSettings, ApiError> {
    // 直接拼 api::SessionSettings
}
```

同一个模块里还保留了很多 `api::*` 转换函数，例如：

```rust
fn default_settings(...) -> Result<api::SessionSettings, ApiError>
fn skills_settings_from_config(...) -> api::SkillsSettings
fn skills_config_from_settings(...) -> kiliax_core::config::SkillsConfig
fn map_core_message_to_api(...)
fn map_mcp_status(...)
```

对应文件：

- `crates/kiliax-server/src/state/mod.rs`
- `crates/kiliax-server/src/state/server_state.rs`

### 为什么这是问题

`ServerState` 本来应该是：

- 管理 session 生命周期
- 管理持久化
- 协调 `LiveSession`
- 维护全局配置和缓存

但现在它还承担了“API 组装器”的职责。这会导致：

- 状态管理和接口协议混在一起；
- 代码搜索时，很难区分“业务逻辑”与“字段映射”；
- 一旦新增一个 HTTP 字段，可能要改三层：`api.rs`、`state/mod.rs`、`live_session.rs`。

这不符合“less is more”：表面上少了几个小模块，实际上把多个变化原因塞进了同一处。

---

## 问题四：当前耦合已经形成“改一个字段，多个层级一起动”的传播链

### 典型传播路径

举一个最常见的例子：改 `SessionSettings` 结构。

现在它会同时影响：

1. `crates/kiliax-server/src/api.rs`
2. `crates/kiliax-server/src/state/mod.rs`
3. `crates/kiliax-server/src/state/live_session.rs`
4. `crates/kiliax-server/src/http/handlers/*`
5. 持久化读写逻辑
6. 测试用例

这不是“单一模型被不同层引用”，而是“DTO 侵入了核心状态对象”。

### 直接后果

- 扩展字段时，容易遗漏某一层转换；
- 重构时会出现“为了适配 API，不得不污染核心逻辑”的情况；
- 后续要把 server 核心复用到别的入口时，成本会很高。

---

## 目标状态

建议把服务端拆成两层：

### 1）领域层

只表达业务，不依赖 HTTP：

- `SessionSettings`
- `SessionStatus`
- `SessionSnapshot`
- `RunRecord`
- `SessionEvent`
- `McpSettings`
- `SkillSettings`

### 2）接口层

只负责请求/响应和序列化：

- `api::SessionSettings`
- `api::SessionSettingsPatch`
- `api::Session`
- `api::Run`
- `api::Event`

接口层只做转换，不参与运行时状态决策。

---

## 大概解决方案

### 方案一：先把领域模型从 DTO 中剥离出来

在 `crates/kiliax-server/src/domain.rs` 或新的 `state/domain.rs` 中定义内部模型。

#### 示例

```rust
pub struct SessionSettings {
    pub agent: String,
    pub model_id: String,
    pub skills: SkillsSettings,
    pub mcp: McpSettings,
    pub workspace_root: PathBuf,
    pub extra_workspace_roots: Vec<PathBuf>,
}

pub struct SessionStatus {
    pub run_state: SessionRunState,
    pub active_run_id: Option<String>,
    pub step: u32,
    pub active_tool: Option<String>,
    pub queue_len: usize,
    pub last_event_id: u64,
}
```

然后让 `LiveSession` 只存领域类型：

```rust
pub struct LiveSession {
    settings: Mutex<SessionSettings>,
    status: Mutex<SessionStatus>,
    events_tx: broadcast::Sender<SessionEvent>,
    queue: Mutex<VecDeque<QueuedRun>>,
}
```

### 方案二：把 API 映射限制在边界层

让 `http/handlers` 或一个专门的 mapper 模块负责转换。

#### 示例

```rust
impl From<domain::SessionSettings> for api::SessionSettings {
    fn from(value: domain::SessionSettings) -> Self {
        Self {
            agent: value.agent,
            model_id: value.model_id,
            skills: value.skills.into(),
            mcp: value.mcp.into(),
            workspace_root: value.workspace_root.display().to_string(),
            extra_workspace_roots: value
                .extra_workspace_roots
                .into_iter()
                .map(|p| p.display().to_string())
                .collect(),
        }
    }
}
```

`LiveSession` 返回领域快照：

```rust
pub async fn snapshot(&self) -> Result<SessionSnapshot, ApiError> {
    Ok(SessionSnapshot {
        settings: self.settings_snapshot().await,
        status: self.status.lock().await.clone(),
        // ...
    })
}
```

然后 handler 再转成 API：

```rust
let snapshot = live_session.snapshot().await?;
Ok(Json(api::Session::from(snapshot)))
```

### 方案三：把 patch 也变成内部 patch

现在 `patch_settings` 直接吃 `api::SessionSettingsPatch`，建议改成内部 patch：

```rust
pub struct SessionSettingsPatch {
    pub agent: Option<String>,
    pub model_id: Option<String>,
    pub skills: Option<SkillsSettingsPatch>,
    pub mcp: Option<McpSettingsPatch>,
    pub extra_workspace_roots: Option<Vec<PathBuf>>,
}
```

API 层把请求体转换成内部 patch：

```rust
let patch = domain::SessionSettingsPatch::from(req.patch);
live_session.patch_settings(patch).await?;
```

这样内部逻辑只处理“字段变化”，不处理“JSON 形状”。

---

## 推荐的重构顺序

### 第一步：新增 domain model，不改行为

- 保留当前 API。
- 先把 `LiveSession` 内部字段替换成领域模型。
- 暂时保留 `From<T> for api::T` / `From<api::T> for domain::T`。

### 第二步：把 handler 边界上的 DTO 转换抽走

- `http/handlers` 负责把 request 转 patch。
- `http/handlers` 负责把 snapshot 转 response。
- `ServerState` 不再直接拼 `api::*`。

### 第三步：清理 `state/mod.rs` 的映射函数

把以下逻辑移出 `state/mod.rs`：

- `resolve_session_settings`
- `default_settings`
- `skills_settings_from_config`
- `skills_config_from_settings`
- `map_core_message_to_api`
- `map_mcp_status`

最后 `state/mod.rs` 只保留真正的协调逻辑。

---

## 设计原则

- **领域对象优先**：核心状态先表达业务，再映射到 API。
- **DTO 只在边界存在**：`serde` 类型不要进入运行时内核。
- **转换单向集中**：映射函数集中在少量边界模块，避免散落各处。
- **先稳后简**：先把职责拆开，再考虑进一步压缩结构。

---

## 结论

当前 `LiveSession` 的问题不是“文件不够小”，而是**核心状态仍然直接依赖 API DTO**。

这会把运行时、持久化、HTTP 协议绑成一个变化集合，违背：

- high cohesion
- low coupling
- keep it simple, stupid
- less is more

下一步最值得做的不是继续拆文件，而是把 `LiveSession` 和 `ServerState` 从 `api::*` 中解耦出来，先建立领域模型，再把 DTO 变成边界适配层。
