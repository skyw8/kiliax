# Session 管理与 Compaction 协作机制

本文说明当前 Kiliax 的 session 如何落盘、如何恢复、如何向 Web UI 暴露消息与事件，以及 auto compaction 如何在不破坏可见聊天历史的前提下压缩模型上下文。

相关代码入口：

- Core session store: `crates/kiliax-core/src/session.rs`
- Compaction helpers: `crates/kiliax-core/src/compact.rs`
- Server live session/runtime glue: `crates/kiliax-server/src/state/live_session.rs`
- Server state/API mappers: `crates/kiliax-server/src/state/server_state.rs`, `crates/kiliax-server/src/state/domain.rs`
- Preamble replacement: `crates/kiliax-server/src/state/preamble.rs`
- Web event handling: `web/src/app.tsx`

## 分层职责

### Core: durable session state

`kiliax-core` 只负责 session 的持久化和事件重放，不关心 HTTP、WebSocket、SSE 或 run queue。

核心类型：

- `FileSessionStore`
  - 管理 `.kiliax/sessions/<session_id>/` 下的 session 文件。
  - 提供 `create/load/list/delete/read_message_page`。
  - 通过 `record_*` 方法追加事件并更新内存态。
- `SessionState`
  - 运行时持有的 core session 内存态。
  - 字段包括 `meta`, `messages`, `message_ids`, `context_checkpoint`。
- `SessionSnapshot`
  - `snapshot.json` 的内容。
  - 保存 `meta`, `messages`, `message_ids`, `context_checkpoint`。
- `SessionEventLine`
  - `events.jsonl` 的单行事件。
  - `seq` 是 append-only event 序号，也是 message event 的 message id。
- `SessionEvent`
  - core 级别的 durable 事件类型。
  - 包括 message、edit、truncate、context checkpoint、finish/error、goal 事件。
- `ContextCheckpoint`
  - compaction 后保存给模型上下文使用的内部 checkpoint。
  - 不代表 Web UI 可见消息。

### Server: live session control plane

`kiliax-server` 将 core session 包装成 HTTP 可操作的 live session。

核心类型：

- `ServerState`
  - 管理全局配置、session store、live session registry、runs dir。
  - 负责创建 session、resume session、fork session、读取 messages、创建/cancel run。
- `LiveSession`
  - 单个 active session 的运行时控制器。
  - 持有 `Mutex<SessionState>`、当前 `SessionSettings`、run queue、status、tool engine、事件广播 ring、stream snapshot。
  - 负责 run worker、runtime event 落盘、Web UI event 发射、auto compact。
- `domain::*`
  - Server 对 HTTP/Web UI 暴露的数据模型。
  - `domain::SessionSnapshot` 只包含 summary、MCP status、stream snapshot，不直接暴露 core 的 `SessionSnapshot` 或 `ContextCheckpoint`。

### Web: visible transcript and live stream

Web UI 通过两个通道更新：

- `/v1/sessions/{id}/messages`
  - 读取当前可见消息页。
  - 数据来自 core `events.jsonl` 的 reverse paging。
- live events
  - `user_message`, `assistant_message`, `tool_call`, `tool_result`, stream delta 等事件实时追加或更新 UI。
  - `session_messages_reset` 会触发重新拉取消息。
  - `session_context_compacted` 只刷新 session summary，不清空或重拉消息窗口。

## 磁盘布局

单个 session 目录：

```text
.kiliax/sessions/<session_id>/
  meta.json
  snapshot.json
  events.jsonl
  events_api.jsonl
```

含义：

- `meta.json`
  - `SessionMeta` 的独立副本。
  - 用于 list、summary、settings 恢复、last outcome、goal 状态等。
- `snapshot.json`
  - core session 的 checkpoint。
  - 包括 `messages/message_ids/context_checkpoint`。
  - `meta.last_snapshot_seq` 表示 snapshot 已覆盖到哪个 event seq。
- `events.jsonl`
  - core append-only log。
  - `FileSessionStore::load` 先读 snapshot，再 replay `seq > last_snapshot_seq` 的事件。
- `events_api.jsonl`
  - server/Web-facing durable event log。
  - 这里的 `event_id` 与 core `SessionEventLine.seq` 是两套序号。
  - ephemeral event 不写入该文件，只进内存 ring 和 broadcast。

Run 文件不在 session 目录内，由 `ServerState.runs_dir` 管理，`LiveSession` 在 enqueue/run/cancel/finish 时写入 run 状态。

## Core 数据结构关系

简化关系：

```text
FileSessionStore
  └─ SessionState
       ├─ SessionMeta
       ├─ messages: Vec<Message>
       ├─ message_ids: Vec<u64>
       └─ context_checkpoint: Option<ContextCheckpoint>

SessionSnapshot
  ├─ meta: SessionMeta
  ├─ messages
  ├─ message_ids
  └─ context_checkpoint

events.jsonl
  └─ SessionEventLine { seq, ts_ms, event: SessionEvent }
```

`messages` 和 `message_ids` 必须保持等长。`message_ids[i]` 是 `messages[i]` 对应的 core event `seq`。因为非 message 事件也会消耗 `seq`，所以 message id 可能有间隔。

`SessionMeta` 记录 session 级元数据：

- agent/model/config/workspace/extra roots
- session-scoped MCP/skills/custom-tools overrides
- title、last finish/error、prompt cache key、frozen project prompt
- goal
- multi-agent parent/root/path metadata
- `last_seq`, `last_snapshot_seq`, `message_count`

`ContextCheckpoint` 结构：

```rust
pub struct ContextCheckpoint {
    pub base_message_id: u64,
    pub messages: Vec<Message>,
    pub reason: String,
}
```

含义：

- `base_message_id`
  - checkpoint 覆盖到的 transcript message id。
  - 运行时会把 `message_id > base_message_id` 的非 preamble transcript message 作为 tail 接到 checkpoint 后面。
- `messages`
  - compact 后给模型看的压缩上下文片段。
  - 通常包含若干保留的真实 user message，以及一个 summary user message。
- `reason`
  - 目前主要是 `auto_compact`。

## Core 事件语义

`SessionEvent` 是 core session 的事实来源。重要事件：

- `Message { message }`
  - 追加一条 core protocol message。
  - 更新 `messages/message_ids/message_count`。
  - 如果第一条可见 user message 存在，会派生 title。
- `MessageEdit { message_id, message }`
  - 替换指定 message id 的消息。
  - 常用于编辑 user message。
- `TruncateAfter { message_id }`
  - 将 transcript 截断到指定 message id。
  - 清除 last finish/error。
  - 如果当前 `context_checkpoint.base_message_id >= message_id`，说明 checkpoint 覆盖了被截断区间，因此清除 checkpoint。
- `ContextCheckpoint { checkpoint }`
  - 只更新 `SessionState.context_checkpoint`。
  - 不追加普通消息，不影响 `messages/message_ids`。
- `Finish/Error`
  - 更新 last outcome metadata。
- `GoalSet/GoalCleared/GoalCompleted/GoalUsage`
  - 更新 session goal 状态和用量。

`FileSessionStore::record_event` 的流程：

1. 生成 `seq = state.meta.last_seq + 1`。
2. 将 `SessionEventLine` append 到 `events.jsonl`。
3. 调用 `apply_event` 更新内存态。
4. 写 `meta.json`。
5. 如果距离上次 snapshot 的事件数超过 `checkpoint_every`，写 `snapshot.json`。

## Snapshot 与恢复

`FileSessionStore::load`：

1. 读取 `snapshot.json`。
2. 构造 `SessionState`。
3. 如果 `message_ids.len() != messages.len()`，认为 snapshot 损坏，丢弃 snapshot 中的 messages，改从 `events.jsonl` 重建并写回 snapshot。
4. replay `seq > meta.last_snapshot_seq` 的 `events.jsonl`。
5. 补齐旧 session 可能缺失的 `prompt_cache_key` 和 `project_prompt`。

这样做的目标是：

- snapshot 让恢复速度稳定。
- events.jsonl 仍然是 append-only 的可重放历史。
- 即使 snapshot 被旧代码写坏，仍可从 events 修复。

## 可见消息分页

Web UI 的历史消息来自 `ServerState::get_messages`，它调用 `FileSessionStore::read_message_page`。

`read_message_page` 反向扫描 `events.jsonl`，并在扫描时处理：

- later `TruncateAfter`
  - 过滤掉被截断删除的旧消息。
- later `MessageEdit`
  - 返回编辑后的消息。
- `ContextCheckpoint`
  - 直接忽略。
- display filter
  - 不返回 `System`/`Developer`。
  - 不返回 hidden user。
  - 不返回带 `SUMMARY_PREFIX` 的 summary user message。

这意味着 Web UI 的可见 transcript 只受真实 message/edit/truncate 影响，和 compaction checkpoint 解耦。

## Server live session 状态

`LiveSession` 在内存中维护：

- `session: Mutex<SessionState>`
  - core durable session state。
- `settings: Mutex<domain::SessionSettings>`
  - 当前 session settings，可能来自 meta，也可能被 patch。
- `status: Mutex<domain::SessionStatus>`
  - run state、active run、step、active tool、retry status、queue length、last event id。
- `queue: Mutex<VecDeque<QueuedRun>>`
  - 等待执行的 run。
- `events_ring`
  - 内存中的近期 server events，用于 reconnect backlog。
- `stream_snapshot`
  - 当前 streaming assistant/thinking/tool-call 的临时快照。
  - final assistant message 后清空。

`LiveSession::snapshot` 暴露给 Web：

```text
domain::SessionSnapshot
  ├─ summary: SessionSummary
  ├─ mcp_status
  └─ stream: Option<StreamSnapshot>
```

它不包含 core `messages`。消息总是通过 `/messages` 分页读取或 live events 增量追加。

## Server/Web 事件

Server-facing event 类型使用 `domain::Event`：

```rust
pub struct Event {
    pub event_id: u64,
    pub ts: String,
    pub session_id: String,
    pub run_id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: serde_json::Value,
}
```

事件分 durable 和 ephemeral：

- durable
  - 写入 `events_api.jsonl`
  - 进入 in-memory ring
  - broadcast 给订阅者
- ephemeral
  - 不写 `events_api.jsonl`
  - 只进入 ring/broadcast
  - 用于 streaming delta、retry 等不需要长期落盘的状态

重要 event 类型：

- Session/control
  - `session_settings_changed`
  - `session_goal_changed`
  - `session_messages_reset`
  - `session_context_compacted`
- Run lifecycle
  - `user_message`
  - `step_start`
  - `step_end`
  - `run_retry`
  - `run_done`
  - `run_cancelled`
  - `run_error`
- Streaming/messages/tools
  - `assistant_thinking_delta`
  - `assistant_delta`
  - `assistant_message`
  - `tool_call`
  - `tool_result`

`session_messages_reset` 只用于可见 transcript 确实被重写的操作，例如 edit/regenerate 后的 truncate。Web 收到后会重新拉取 session/messages。

`session_context_compacted` 用于 compaction。Web 收到后只刷新 session list/summary，不重置消息窗口。

## Preamble 管理

Session 的 preamble 是开头的一组 `System` message，由 `build_preamble` 生成。

创建 session 时：

1. `ServerState::create_session_inner` 根据 profile/model/workspace/tools/skills 捕获 project prompt。
2. 调用 `build_preamble` 生成 initial messages。
3. `FileSessionStore::create` 将 initial messages 作为初始 `Message` 事件写入 `events.jsonl`。
4. `SessionMeta.project_prompt` 保存 frozen project prompt。

设置变更或 compaction 时：

- 使用 `replace_preamble_with_ids` 替换 `messages` 开头连续的 `System` message。
- 旧 preamble 对应位置优先复用原 message id。
- 如果新 preamble 更长，会分配新的 `last_seq`。
- Preamble 是 core transcript 的一部分，但不会显示在 Web messages 中。

## Auto Compaction 总览

Auto compaction 的目标是压缩发送给模型的上下文，而不是压缩 Web UI 的可见 transcript。

关键原则：

- `session.messages` 保留完整可见 transcript。
- compact 结果写入 `context_checkpoint`。
- 后续模型上下文由 `preamble + checkpoint + checkpoint 之后的新 transcript tail` 组成。
- Web 不因 compaction 重拉 messages。

## Token 阈值判断

`compact::context_tokens_for_auto_compact(messages)`：

1. 从后往前找最近的 assistant `usage.prompt_tokens`。
2. 如果找到了，使用 provider usage，`source = provider_usage`。
3. 否则估算 tokens，`source = estimate`。

阈值来自 `AgentRuntimeOptions.auto_compact_token_limit`，配置优先级在 runtime options 中处理：

1. global runtime
2. agent config
3. provider model config

## Pre-run compaction

位置：`LiveSession::run_one`，在持久化当前 user message 之前。

触发条件：

- 当前 run 会持久化 user message。
- 已配置 `auto_compact_token_limit`。
- 当前模型上下文 token 数达到阈值。

流程：

1. 从当前 session 构造模型上下文：
   - 如果没有 checkpoint，使用完整 `session.messages`。
   - 如果已有 checkpoint，使用 `build_model_context_messages`。
2. 对这个模型上下文计算 token。
3. 超过阈值时调用 `compact_session_context(...)`。
4. 成功后发 `session_context_compacted`。
5. 再持久化当前 user message。
6. 最终运行模型时重新构造上下文，此时当前 user message 的 id 大于 checkpoint base，会作为 tail 被追加到 checkpoint 后面。

这个顺序保证“触发 compact 的旧上下文”被压缩，同时“当前用户输入”仍然完整进入模型请求。

## Mid-run compaction

位置：`AgentRuntime::maybe_auto_compact_before_step` 通过 `RunAutoCompactHandler` 回调 server。

触发时机：

- 每个 runtime step 发请求前。
- 当前 in-memory runtime messages token 数达到阈值。

流程：

1. runtime 将当前 `messages` 传给 `RunAutoCompactHandler::compact`。
2. handler 调用 `LiveSession::compact_session_context(...)`。
3. server 写入 core `ContextCheckpoint` 事件和 snapshot。
4. server 发 `session_context_compacted`。
5. handler 返回 compact 后的新 messages 给 runtime。
6. runtime 用返回值替换自己的 in-memory `messages`，下一步请求直接使用压缩上下文。

Mid-run compaction 会保留本轮中已经产生但尚未落成完整历史的 runtime messages，因为 compaction source 是 runtime 当前 messages，而不是只读 session transcript。

## compact_session_context 详细流程

`LiveSession::compact_session_context` 是 server compaction 的核心函数。

输入：

- `effective` settings
- agent `profile`
- 当前 run 的 `tools_for_run`
- `llm`
- `source_messages`
- `reason`

步骤：

1. `compact::run_compaction(llm, source_messages)`
   - 对 source messages 做请求安全处理和 tool output 截断。
   - 附加 summarization prompt。
   - 如果 provider 报 context window exceeded，会删除最老的非 preamble item，并保留 tool pair 关系后重试。
2. `compact::collect_real_user_texts(source_messages)`
   - 收集非 hidden user 文本。
   - 跳过 summary message。
3. `compact::build_compacted_user_history(...)`
   - 保留最多约 20k token 的最近 user messages。
   - 追加一个带 `SUMMARY_PREFIX` 的 summary user message。
4. 重新捕获 project prompt 并 `build_preamble(...)`
   - 使用当前 effective model/workspace/tools/skills。
5. 锁定 `session`。
6. 用 `replace_preamble_with_ids` 更新 session preamble。
7. 设置 `session.meta.project_prompt`。
8. 计算 `base_message_id = session.message_ids.last().unwrap_or(0)`。
9. 写入 `ContextCheckpoint { base_message_id, messages: compacted_history, reason }`。
10. 写 snapshot。
11. 返回新的模型上下文：
    - `new_preamble`
    - checkpoint messages
    - `message_id > base_message_id` 的 transcript tail

注意：第 9 步只写 `ContextCheckpoint` 事件，不写普通 `Message` 事件。因此 Web messages 不会消失，也不会出现 summary message。

## 模型上下文构造

`build_model_context_messages_with_preamble` 是运行模型前的统一组装逻辑。

没有 checkpoint：

```text
model_context = session.messages
if override_preamble exists:
  replace preamble in model_context
```

有 checkpoint：

```text
model_context =
  override_preamble or current_preamble(session.messages)
  + checkpoint.messages
  + session.messages where message_id > checkpoint.base_message_id and message is not preamble
```

含义：

- checkpoint 前的旧 transcript 不再直接发送给模型。
- checkpoint 后的新消息完整保留。
- preamble 始终使用当前版本。
- `System`/`Developer` tail 不会被追加，避免旧 volatile prompt 重新混入上下文。

## Edit / Regenerate 与 checkpoint

Edit 和 regenerate 是真正会改变可见 transcript 的操作。

Edit:

1. 校验 session idle。
2. 校验 target 是 user message。
3. `MessageEdit` 替换 user message。
4. `TruncateAfter` 截断其后的消息。
5. checkpoint。
6. 发 `session_messages_reset`。

Regenerate:

1. 校验 session idle。
2. 校验 target 是可见 user message 且非空。
3. `TruncateAfter` 截断其后的消息。
4. checkpoint。
5. 发 `session_messages_reset`。

`TruncateAfter` 对 `context_checkpoint` 的处理：

- 如果 `checkpoint.base_message_id >= truncate_message_id`，checkpoint 覆盖了被截断的区域，必须清除。
- 如果 `checkpoint.base_message_id < truncate_message_id`，checkpoint 仍然有效，截断后的 user message 会作为 tail 留在 checkpoint 后面。

这保证 edit/regenerate 既不会引用已删除的未来消息，也不会无意义丢掉仍然可用的旧 compact summary。

## Fork 与 checkpoint

Fork 不继承 `ContextCheckpoint`。

HTTP fork:

- `ServerState::fork_session` 从 source `messages` 切片构造 `initial_messages`。
- 如果指定 `message_id`，取 `source.messages[..=idx]`。
- 如果未指定，取 `source.messages.clone()`。
- 调用 `FileSessionStore::create` 创建新 session，默认 `context_checkpoint = None`。
- prompt cache key 会继承，但 checkpoint 不继承。

Multi-agent fork:

- `LiveSession::fork_messages` 也只读 parent `session.messages`。
- 它过滤成适合 child 的 visible user 和无 tool call assistant 文本。
- 不读取 parent `context_checkpoint`。

这样 fork 得到的是一个从可见 transcript 出发的新 session，而不是共享 parent 的内部 compact state。

## 与 Web UI 的配合

Web 处理规则：

- `user_message`
  - 追加用户 bubble，并对齐 client_message_id。
- `assistant_delta` / `assistant_thinking_delta`
  - 更新 streaming row。
- `assistant_message`
  - final assistant message 落入消息列表，并清空 stream buffer。
- `tool_call` / `tool_result`
  - 更新工具调用 UI。
- `session_messages_reset`
  - 重新 `fetchSession`，重建消息窗口。
  - 用于 edit/regenerate。
- `session_context_compacted`
  - 只 `refreshSessionsIfStale(0)`。
  - 不清空 messages，不拉 `/messages`。

因此 compaction 对用户表现为后台上下文维护事件，不应让旧聊天记录从 UI 消失。

## 关键不变量

- `SessionState.messages.len() == SessionState.message_ids.len()`。
- Core message id 使用 `SessionEventLine.seq`，但非 message 事件也会消耗 seq，因此 message id 可以不连续。
- `events.jsonl` 是 core session 的 append-only source of truth。
- `events_api.jsonl` 是 server/Web event source，与 core `events.jsonl` 独立。
- `ContextCheckpoint` 是模型上下文 checkpoint，不是用户可见消息。
- Compaction 不调用 `truncate_after`，也不写普通 compact summary message。
- Web 可见历史只由 message/edit/truncate 决定。
- Edit/regenerate 可以发 `session_messages_reset`；compaction 只能发 `session_context_compacted`。
- Fork 不继承 checkpoint。
- Preamble 可在 settings 变更或 compaction 后刷新，但 Web messages 不显示 preamble。

## 排查指南

如果 compaction 后 Web 历史消失：

1. 查 server event 是否错误发了 `session_messages_reset`。
2. 查 `events.jsonl` 中是否出现了非预期 `TruncateAfter`。
3. 查 `compact_session_context` 是否写了普通 `Message` 事件。
4. 查 Web 是否把 `session_context_compacted` 当成 reset 处理。

如果 compaction 没有效果：

1. 查 `run.auto_compact.config` 日志，确认 `auto_compact_token_limit` 和 handler 是否存在。
2. 查 token source 是 `provider_usage` 还是 `estimate`。
3. 查 pre-run 是否构造了 `build_model_context_messages` 后再计数。
4. 查 mid-run handler 是否返回了 compacted messages 给 runtime。
5. 查 `snapshot.json.context_checkpoint` 或 `events.jsonl` 中是否有 `context_checkpoint` 事件。

如果 regenerate/fork 行为异常：

1. Regenerate/edit 应该产生 `TruncateAfter` 和 `session_messages_reset`。
2. `TruncateAfter` 应按 `base_message_id` 判断是否清 checkpoint。
3. Fork 后新 session 的 `context_checkpoint` 应为 `null` 或缺失。
4. Fork 的消息来源应是 source `messages`，不是 checkpoint messages。
