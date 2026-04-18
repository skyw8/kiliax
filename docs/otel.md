# Kiliax OpenTelemetry（OTel）技术文档

> 目标：快速掌握 Kiliax 当前的 OTel 接入点、导出的数据（traces / metrics / logs）、配置方式、以及后续改进方向。

## 1. OTel 快速入门（结合本项目）

### 1.1 三大信号（Signals）

- **Traces（链路追踪）**：由一棵 span 树组成。一个请求/一次运行通常对应一个 trace。
- **Metrics（指标）**：counter/histogram 等时间序列，用于统计量与分布（例如 QPS、延迟、token 数）。
- **Logs（日志）**：结构化事件；在 OTel 里可与 trace/span 关联（带 trace_id/span_id）。

在 Kiliax 中，这三类信号全部通过 **OTLP** 导出（HTTP 或 gRPC），并且统一依赖 `tracing` 作为埋点入口：

- `tracing` 的 **span** → OTel **trace spans**
- `tracing` 的 **event** → OTel **logs**
- 业务侧显式调用 `opentelemetry` meter → OTel **metrics**

### 1.2 OTLP、Collector、Backend

- **OTLP** 是导出协议：Kiliax 作为 SDK Producer，把数据发给 OTLP endpoint。
- **Collector（可选）**：作为“中转/处理”层，接收 OTLP，再转发到不同后端（Prometheus、Tempo、Jaeger、Langfuse 等）。
- **Backend**：最终的可视化与存储系统。

Kiliax 既可以直接对接后端（如 Langfuse 的 OTLP ingest），也可以对接本地/集群 Collector。

### 1.3 Resource / Attribute / Context Propagation

- **Resource**：描述“谁”在发数据（`service.name`、`service.version`、环境、host 等）。
- **Attribute**：挂在 span/log/metric 上的 key-value（例如 `http.method`、`gen_ai.request.model`）。
- **Context Propagation**：跨进程串联 trace 的关键。Kiliax 使用 W3C `traceparent` / `tracestate`。

## 2. 项目内 OTel 架构与代码入口

### 2.1 关键模块

- 初始化与 exporter：`crates/kiliax-otel/src/lib.rs`、`crates/kiliax-otel/src/otlp.rs`
- 统一埋点工具（capture、metrics、span attributes）：`crates/kiliax-core/src/telemetry.rs`
- 主要埋点位置：
  - LLM：`crates/kiliax-core/src/llm.rs`
  - Agent runtime：`crates/kiliax-core/src/runtime.rs`
  - Tools：`crates/kiliax-core/src/tools/engine.rs`
  - MCP：`crates/kiliax-core/src/tools/mcp.rs`
  - Skills：`crates/kiliax-core/src/tools/skills.rs`
  - Server HTTP：`crates/kiliax-server/src/http/mod.rs`
  - Server trace_id 注入：`crates/kiliax-server/src/error.rs`
  - Langfuse trace-level attributes：`crates/kiliax-server/src/state.rs`

### 2.2 初始化流程（CLI/Server）

- CLI：`crates/kiliax-cli/src/main.rs` 调用 `kiliax_otel::init(&config, "kiliax-cli", version, local_logs)`
  - `local_logs` 默认写到 `~/.kiliax/tui.log`（可用 `KILIAX_TUI_LOG_PATH` 覆盖）。
- Server：`crates/kiliax-server/src/runner.rs` 调用 `kiliax_otel::init(&config, "kiliax-server", version, Stdout)`

`kiliax_otel::init(...)` 做的事情（概览）：

1) **把 capture 配置写入 `kiliax-core`（进程全局）**  
   `kiliax_core::telemetry::set_capture_config(cfg.otel.enabled.then_some(cfg.otel.capture.clone()))`
2) 读取 `RUST_LOG`（`EnvFilter`）；默认 `"info"`
3) 若 `otel.enabled: false`：只按 `LocalLogs` 配置本地 fmt 日志，**不安装任何 OTel exporter**
4) 若 `otel.enabled: true`：按 signals 构建 logs/traces/metrics provider，并安装到 `tracing_subscriber`

## 3. 配置（`kiliax.yaml`）

配置入口：`crates/kiliax-core/src/config.rs`（含校验逻辑 `validate_otel_config`）。

示例见：`kiliax.example.yaml` 与 `README.md` 的 observability 章节。

### 3.1 字段说明

```yaml
otel:
  enabled: false
  environment: dev
  otlp:
    endpoint: http://localhost:4318
    protocol: http_protobuf # http_protobuf | http_json | grpc
    headers: {}             # 例如 authorization
    tls:                    # 可选：自签 CA / mTLS
      ca_cert: /path/to/ca.pem
      client_cert: /path/to/client.pem
      client_key: /path/to/client.key
  signals:
    logs: true
    traces: true
    metrics: true
  capture:
    mode: full              # metadata | full
    max_bytes: 65536        # >= 1024（当 enabled=true）
    include_images: false   # 目前未实现（见“改进点”）
    hash: sha256            # none | sha256
```

#### `otel.enabled`

- `false`：不导出 OTel 数据；可选写本地日志（CLI 默认写文件，server 默认 stdout）。
- `true`：启用 exporter，并且 **才会启用 capture**（prompt/tool 输入输出抓取）。

#### `otel.environment`

会写入 resource attribute：`deployment.environment.name=<environment>`（在 `kiliax-otel` 构建 resource 时添加）。

#### `otel.otlp.endpoint`

**必须是 collector/base endpoint（不包含 `/v1/...`）**：

- ✅ `http://localhost:4318`（OTLP HTTP）
- ✅ `http://localhost:4317`（OTLP gRPC）
- ✅ `https://<LANGFUSE_HOST>/api/public/otel`（Langfuse OTLP ingest base）
- ❌ `http://localhost:4318/v1/traces`（会被配置校验拒绝）

原因：Kiliax 在 HTTP 模式下会按 signal 自动拼接：`{endpoint}/v1/traces|logs|metrics`。

#### `otel.otlp.protocol`

对应 `OtelOtlpProtocol`：

- `http_protobuf` → `opentelemetry_otlp::Protocol::HttpBinary`
- `http_json` → `Protocol::HttpJson`
- `grpc` → `Protocol::Grpc`

#### `otel.otlp.headers`

会注入到 exporter 的请求头（HTTP）或 metadata（gRPC）。

Langfuse 常见配置：

- `headers.authorization: Basic <base64(public_key:secret_key)>`

#### `otel.otlp.tls`

用于两类场景：

- 自签 CA：`ca_cert`
- mTLS：`client_cert` + `client_key`（必须同时提供）

实现位置：

- gRPC：`crates/kiliax-otel/src/otlp.rs::build_grpc_tls_config`
- HTTP：`crates/kiliax-otel/src/otlp.rs::build_http_client(_inner)` / `build_async_http_client`

#### `otel.signals`

分别控制是否构建/安装：

- logs provider（OTel logs）
- tracer provider（OTel traces）
- meter provider（OTel metrics）

注意：`otel.enabled=true` 但若某个 signal 关闭，则对应 provider 为 `None`，相关埋点会变成 no-op（例如 span attributes 会因为 span 无效而被忽略）。

#### `otel.capture`

capture 的用途：把“高价值但可能高敏感”的内容（prompt/response/tool args/output）以结构化字段导出到 OTel。

- `mode: full`：导出正文（最多 `max_bytes`，并且可附带 sha256）
- `mode: metadata`：不导出正文，只导出 `len/truncated/sha256`
- `hash: sha256`：对原文做 sha256（用于去重/对比）
- `max_bytes`：对正文做 UTF-8 边界截断（避免非法 UTF-8）

实现位置：`crates/kiliax-core/src/telemetry.rs::capture_text`

配置校验（当 `otel.enabled: true`）：

- `otel.environment` 不能为空
- `otel.otlp.endpoint` 不能为空，且必须 `http://` 或 `https://`
- endpoint 不能以 `/v1`、`/v1/traces|logs|metrics` 结尾
- `otel.capture.max_bytes >= 1024`
- mTLS 的 `client_cert` / `client_key` 必须成对出现

## 4. Exporter 行为（`crates/kiliax-otel`）

### 4.1 Resource（服务维度属性）

`crates/kiliax-otel/src/lib.rs::make_resource` 设置：

- `service.name`（来自 `init(service_name, ...)`）
- `service.version`
- `deployment.environment.name`
- `host.name`（若可获取）

### 4.2 OTLP endpoint 拼接规则（HTTP）

HTTP 模式下（`http_protobuf` / `http_json`）：

- traces：`{endpoint}/v1/traces`
- logs：`{endpoint}/v1/logs`
- metrics：`{endpoint}/v1/metrics`

实现：`crates/kiliax-otel/src/lib.rs::endpoint_for_signal`

### 4.3 Traces exporter（Span）

实现：`crates/kiliax-otel/src/lib.rs::build_tracer_provider`

- gRPC：`SpanExporter::builder().with_tonic()` + `BatchSpanProcessor`
- HTTP：
  - 若当前 tokio runtime 是 **multi-thread**：使用 `TokioBatchSpanProcessor` + async reqwest client（带 timeout 支持）
  - 否则：使用 blocking reqwest client（仅当配置了 `otel.otlp.tls` 时才构建自定义 client）

### 4.4 Logs exporter（Event → OTel log）

实现：`crates/kiliax-otel/src/lib.rs::build_logger_provider`

- gRPC：`LogExporter::builder().with_tonic()` + batch exporter
- HTTP：`LogExporter::builder().with_http()` + batch exporter（自定义 blocking client 同样只在配置了 TLS 时注入）

OTel logs 与 `tracing` 的桥接：

- `OpenTelemetryTracingBridge` 作为 `tracing_subscriber` layer，将 `tracing::event!` 导出为 OTel logs

### 4.5 Metrics exporter

实现：`crates/kiliax-otel/src/lib.rs::build_meter_provider`

- 使用 `PeriodicReader`，固定 `interval=10s`
- HTTP exporter 仅在配置了 TLS 时注入自定义 blocking client

### 4.6 Timeout 环境变量（OTLP）

实现：`crates/kiliax-otel/src/otlp.rs::resolve_otlp_timeout`

优先级：

1) signal-specific timeout（例如 `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT`）
2) 全局 `OTEL_EXPORTER_OTLP_TIMEOUT`
3) SDK 默认值（`OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT`）

注意：目前 timeout 通过自定义 reqwest client 生效，因此：

- traces（HTTP, tokio multi-thread）总会使用自定义 async client（timeout 生效）
- logs/metrics（HTTP）只有配置了 `otel.otlp.tls` 才会注入自定义 blocking client（timeout 才生效）

### 4.7 过滤规则（降低噪音）

`crates/kiliax-otel/src/lib.rs::is_kiliax_target`：

- 只导出 **`target` 以 `"kiliax"` 开头** 的 spans/logs
- traces 额外要求 `meta.is_span()`

这会屏蔽第三方库的大量 span/event，避免 OTLP 成本和噪音。

## 5. Traces：当前 span 列表与字段

> 下面列出 Kiliax 自己创建的关键 span 名称、主要字段、以及记录点。

### 5.1 Server HTTP

**span 名称**：`http.request`  
**位置**：`crates/kiliax-server/src/http/mod.rs::http_trace_layer`

字段：

- `otel.kind = "server"`
- `http.method`
- `http.route`（优先 `MatchedPath`）
- `http.target`（剔除 token query）
- `http.user_agent`
- `http.status_code`（响应后记录）
- `http.latency_ms`（响应后记录）

上下文继承：

- 从请求头提取 `traceparent` / `tracestate`，并设置为 parent span  
  `kiliax_otel::set_parent_from_http_headers(&span, request.headers())`

### 5.2 Agent runtime

**span 名称**：

- `kiliax.agent.run`（一次 agent run）
- `kiliax.agent.step`（每一个 ReAct step）

**位置**：`crates/kiliax-core/src/runtime.rs`

字段：

- `kiliax.agent.run`：`agent`、`max_steps`
- `kiliax.agent.step`：`agent`、`step`

Langfuse 对齐：

- `kiliax.agent.run`：`langfuse.observation.type = "agent"`
- `kiliax.agent.step`：`langfuse.observation.type = "chain"`

### 5.3 LLM 调用

**span 名称**：

- `kiliax.llm.chat`（非流式）
- `kiliax.llm.chat_stream`（SSE 流式）

**位置**：`crates/kiliax-core/src/llm.rs`

字段（基础）：

- `llm.provider`
- `llm.model`
- `llm.base_url`
- `llm.stream`
- `request.messages`（消息数）
- `request.tools`（tool 数）

属性（span attributes）：

- `langfuse.observation.type = "generation"`
- `gen_ai.system = <provider>`
- `gen_ai.request.model = <model>`
- usage（如果 provider 返回）：`gen_ai.usage.input_tokens` / `cached_input_tokens` / `output_tokens`
- 性能：`kiliax.llm.output_tps`，以及流式额外的：
  - `kiliax.llm.ttft_ms`（time-to-first-token）
  - `kiliax.llm.output_tps_after_ttft`
  - `langfuse.observation.completion_start_time`（RFC3339，流式首 token 对应时间）

捕获（capture，见 Logs 章节）：

- `gen_ai.prompt` / `gen_ai.completion`（仅 `capture.mode=full` 时写入 span attribute）

### 5.4 Tool 执行

**span 名称**：

- builtin tool：`kiliax.tool.{tool_name}`
- MCP tool：`kiliax.mcp.{mcp_name}`

**位置**：`crates/kiliax-core/src/tools/engine.rs`

字段：

- `tool.name` / `tool.call_id`
- `tool.kind`：`builtin` 或 `mcp`
- `mcp.server` / `mcp.tool`（若是 MCP tool）
- `tool.duration_ms`

Langfuse 对齐：

- `langfuse.observation.type = "tool"`
- `langfuse.observation.input` / `output`（仅 capture full）
- `langfuse.observation.status_message`（error 时）

### 5.5 MCP

**span 名称**：`kiliax.mcp.{mcp_name}`

**位置**：`crates/kiliax-core/src/tools/mcp.rs`

字段：

- connect：`mcp.server` / `mcp.command` / `mcp.args` / `mcp.duration_ms`
- call：`mcp.server` / `mcp.tool` / `mcp.duration_ms`

### 5.6 Skills

**span 名称**：`kiliax.skills.discover`  
**位置**：`crates/kiliax-core/src/tools/skills.rs`

字段：

- `skills.roots`
- `skills.discovered`
- `skills.duration_ms`
- `langfuse.observation.type = "span"`

## 6. Logs：当前导出的事件与字段

Kiliax 的 OTel logs 基本来自两类：

1) 常规 `tracing::info!/warn!/error!`（结构化字段）
2) telemetry 专用事件（`target: "kiliax_core::telemetry"` / `"kiliax_otel"` 等）

当 `otel.signals.logs=true` 时，`OpenTelemetryTracingBridge` 会把这些 event 导出成 OTel logs，并尽可能关联当前 span（带 trace_id/span_id）。

### 6.1 LLM 相关（`crates/kiliax-core/src/llm.rs`）

事件（字段省略部分常量）：

- `event="llm.request"`：`llm_stream`、`request_len`、`request_truncated`、`request_sha256`、`request`
- `event="llm.response"`：`llm_stream`、`finish_reason`、`response_len`、`response_truncated`、`response_sha256`、`response`
- `event="llm.error"`：`llm_stream`、`error`
- `event="llm.stream_end"`（warn）：`outcome`（非 ok 时）

在 `capture.mode=metadata` 时，`request/response` 字段为 `""`，但 `*_len/*_sha256` 仍保留（用于定位与对比）。

### 6.2 Tool 相关（`crates/kiliax-core/src/tools/engine.rs`）

- `event="tool.args"`：`tool`、`kind`、`call_id`、`args_len`、`args_truncated`、`args_sha256`、`args`
- `event="tool.output"`：`tool`、`kind`、`call_id`、`output_len`、`output_truncated`、`output_sha256`、`output`
- `event="tool.error"`（warn）：`tool`、`kind`、`call_id`、`error`

### 6.3 MCP 相关（`crates/kiliax-core/src/tools/mcp.rs`）

- `event="mcp.connect_error"`（warn）：`error`
- `event="mcp.call_error"`（warn）：`error`

### 6.4 `kiliax-otel` 自身事件

`crates/kiliax-otel/src/lib.rs`：

- `event="local_log_file_failed"`（warn）：本地日志文件写入失败但不致命
- `event="otel_enabled"`（info）：OTel 启用且 capture full 时打印 endpoint/protocol（便于确认生效）

## 7. Metrics：当前指标清单

指标实现：`crates/kiliax-core/src/telemetry.rs::metrics`

> 说明：Metrics 的导出依赖 `otel.signals.metrics=true` 且 `otel.enabled=true`（`kiliax-otel` 会设置全局 meter provider）。

### 7.1 LLM

| 指标名 | 类型 | 单位 | 标签（attributes） | 记录点 |
|---|---|---|---|---|
| `kiliax_llm_requests_total` | Counter(u64) | - | `provider` `model` `stream` `outcome` | `llm.rs` |
| `kiliax_llm_latency_ms` | Histogram(f64) | `ms` | 同上 | `llm.rs` |
| `kiliax_llm_tokens_prompt_total` | Counter(u64) | - | 同上 | `llm.rs` |
| `kiliax_llm_tokens_prompt_cached_total` | Counter(u64) | - | 同上 | `llm.rs` |
| `kiliax_llm_tokens_completion_total` | Counter(u64) | - | 同上 | `llm.rs` |

`outcome` 可能值（当前实现）：`ok` / `error` / `cancelled`（流式取消）。

### 7.2 Tools

| 指标名 | 类型 | 单位 | 标签 | 记录点 |
|---|---|---|---|---|
| `kiliax_tool_calls_total` | Counter(u64) | - | `tool` `kind` `outcome` | `tools/engine.rs` |
| `kiliax_tool_latency_ms` | Histogram(f64) | `ms` | 同上 | `tools/engine.rs` |

`kind`：`builtin` / `mcp`

### 7.3 MCP

| 指标名 | 类型 | 单位 | 标签 | 记录点 |
|---|---|---|---|---|
| `kiliax_mcp_calls_total` | Counter(u64) | - | `server` `tool` `outcome` | `tools/mcp.rs` |
| `kiliax_mcp_latency_ms` | Histogram(f64) | `ms` | 同上 | `tools/mcp.rs` |
| `kiliax_mcp_connect_failures_total` | Counter(u64) | - | `server` | `tools/mcp.rs` |

### 7.4 Skills

| 指标名 | 类型 | 单位 | 标签 | 记录点 |
|---|---|---|---|---|
| `kiliax_skills_discovered_total` | Counter(u64) | - | - | `tools/skills.rs` |

### 7.5 Agent runs

| 指标名 | 类型 | 单位 | 标签 | 记录点 |
|---|---|---|---|---|
| `kiliax_runs_total` | Counter(u64) | - | `agent` `outcome` | `runtime.rs` |
| `kiliax_steps_total` | Counter(u64) | - | `agent` `outcome` | `runtime.rs` |
| `kiliax_run_duration_ms` | Histogram(f64) | `ms` | `agent` `outcome` | `runtime.rs` |

`outcome`：`done` / `error` / `max_steps`（以及取消在上层如何映射取决于 runtime error handling）。

## 8. Langfuse 集成：当前约定与落点

Kiliax 在埋点中显式加入了多处 `langfuse.*` 属性，目的是兼容 Langfuse 的 OTEL ingest 语义。

### 8.1 配置要点

Langfuse endpoint 要填 ingest base（不带 `/v1/traces`）：

```yaml
otel:
  enabled: true
  otlp:
    endpoint: https://<LANGFUSE_HOST>/api/public/otel
    protocol: http_protobuf
    headers:
      authorization: "Basic <base64(public_key:secret_key)>"
  signals:
    traces: true
    logs: true
    metrics: true
```

生成 auth header（见 `README.md`）：

```bash
echo -n "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" | base64 | tr -d '\n'
```

### 8.2 Trace-level attributes（server）

`crates/kiliax-server/src/state.rs` 在 run 开始时设置：

- `langfuse.session.id`
- `langfuse.environment`（取自 `config.otel.environment`）
- `langfuse.trace.name`
  - capture full：取用户输入前 80 字符
  - 否则：`"<agent> <model_id>"`
- `langfuse.trace.input`（仅 capture full）

### 8.3 Observation attributes（core）

| span | `langfuse.observation.type` |
|---|---|
| `kiliax.llm.chat*` | `generation` |
| `kiliax.agent.run` | `agent` |
| `kiliax.agent.step` | `chain` |
| `kiliax.tool.{tool_name}` | `tool` |
| `kiliax.mcp.{mcp_name}` | `tool` |
| `kiliax.skills.discover` | `span` |

Tool 还会设置：

- `langfuse.observation.input` / `output`（capture full）
- `langfuse.observation.status_message`（错误）

LLM 流式会设置：

- `langfuse.observation.completion_start_time`

## 9. 与 HTTP API 的关联：trace_id 回传

为了让调用方能从 API 错误快速跳转到 trace：

- `crates/kiliax-server/src/error.rs`：`ApiErrorResponse` 中包含 `trace_id`（取自当前 span 的 trace_id）
- `crates/kiliax-server/src/state.rs`：run 出错时会把 `trace_id` 拼进错误文本（便于在 UI/日志中复制）

这意味着：如果你在服务端开启 traces，并且请求走 `http.request` span，那么客户端拿到的 `trace_id` 可以直接用于后端检索。

## 10. 验证与排查（Troubleshooting）

### 10.1 “看不到数据”的常见原因

1) `otel.enabled=false` 或 `signals.*` 被关闭  
2) `otel.otlp.endpoint` 配错（尤其是把 `/v1/traces` 写进 endpoint）
3) `headers` 缺失（如 Langfuse Basic auth）
4) 网络/TLS 问题（自签证书缺 `ca_cert`、mTLS 缺 key/cert）
5) `RUST_LOG` 过滤导致本地日志看不到（不影响 exporter，但会影响排查）
6) 过滤规则：只有 `target` 以 `"kiliax"` 开头的 spans/logs 会导出（见 `kiliax-otel` 的 filter）

### 10.2 端到端自检建议

- 启动时检查 `kiliax_otel` 的 `otel_enabled` 日志（capture full 时会打印 endpoint/protocol）
- 在后端查 `service.name`（`kiliax-cli` / `kiliax-server`）确认资源归属正确
- 发一个简单请求，确认能看到：
  - `http.request` span（server）
  - `kiliax.llm.chat_stream` span（若触发 LLM）
  - `kiliax.tool.{tool_name}` span（若触发 builtin tool）
  - `kiliax.mcp.{mcp_name}` span（若触发 MCP tool）
  - 以及对应 metrics 增量

## 11. 改进点（建议按优先级）

### 11.1 安全与隐私（最高优先级）

1) **`otel.capture.include_images` 目前未生效**  
   配置存在但没有在任何 capture 路径里使用（目前只在 `config.rs` 定义）。  
   建议：在 capture prompt/tool args 时对图片/base64 内容做剥离或替换为 metadata（路径、mime、len、hash）。

2) **敏感信息脱敏/白名单化**  
   prompt、tool args、tool output 都可能包含 token/密钥/用户隐私。  
   建议：增加 redaction（例如对 `authorization`、`api_key` 等字段做掩码），并支持“只导出 hash”模式的更严格实现（例如 metadata 模式下不输出空 `request` 字段，直接 omit）。

### 11.2 可配置性与稳定性

3) **采样与批处理参数配置化**  
   当前 trace sampler/批大小/flush 行为基本依赖 SDK 默认值。  
   建议：通过 `kiliax.yaml` 暴露（或支持标准 OTEL env vars），并在文档里明确默认值与推荐值。

4) **logs/metrics 的 HTTP exporter 也统一使用自定义 client（timeout 生效）**  
   当前 logs/metrics 只有配置了 TLS 才注入自定义 blocking client，timeout 不一定生效。  
   建议：无论 TLS 与否，都构建带 timeout 的 client（并在 tokio runtime 场景避免阻塞）。

5) **capture 配置的作用域**  
   `kiliax-core` 的 capture 配置是进程全局（`OnceLock<RwLock<...>>`），而 server 可能同时跑多 session。  
   建议：明确“全局一致”的约束，或把 capture 配置做成更细粒度（例如 run/session 级别）。

### 11.3 语义与可观测性质量

6) **进一步对齐 OTel Semantic Conventions**  
   目前 `http.*` 字段较完整；LLM 使用了 `gen_ai.*` 与 `kiliax.*` 自定义属性。  
   建议：统一属性命名、减少高基数属性（尤其是把长文本作为 attribute 的场景），并补充必要的 `error.*` 语义字段。

7) **补齐 Langfuse/可观测后端的 span 分类**  
   MCP spans 目前没有设置 `langfuse.observation.type`（仅有普通 span 字段）。  
   建议：视 Langfuse 展示需求，给 `kiliax.mcp.{mcp_name}`（connect/call）增加 observation type 或更清晰的层级关系。

8) **更多关键路径埋点**  
   例如：session store I/O、事件广播、workspace 清理、patch apply 阶段拆分、队列长度与背压等。
