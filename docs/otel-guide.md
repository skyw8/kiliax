# OpenTelemetry 可观测性完整指南

> 本指南基于 Kiliax 项目的 OTel 实现，面向 OpenTelemetry 新手，从概念到实践全面讲解。

## 目录

1. [什么是可观测性](#什么是可观测性)
2. [OpenTelemetry 简介](#opentelemetry-简介)
3. [三大信号：Traces、Metrics、Logs](#三大信号tracesmetricslogs)
4. [项目架构解析](#项目架构解析)
5. [配置指南](#配置指南)
6. [代码实践](#代码实践)
7. [与 Langfuse 集成](#与-langfuse-集成)
8. [最佳实践](#最佳实践)
9. [故障排查](#故障排查)

---

## 什么是可观测性

可观测性（Observability）是指通过系统的外部输出（Logs、Metrics、Traces）来理解系统内部状态的能力。传统监控告诉你系统是否正常运行，而可观测性帮助你理解**为什么**出现问题。

### 可观测性的价值

```
┌─────────────────────────────────────────────────────────────┐
│                      可观测性金字塔                           │
├─────────────────────────────────────────────────────────────┤
│  🔍 Traces (分布式追踪)                                       │
│     └── 请求在多个服务间的完整链路                              │
│  📊 Metrics (指标)                                           │
│     └── 系统性能和业务指标的量化                                │
│  📝 Logs (日志)                                              │
│     └── 详细的程序运行信息                                    │
│  ⚡ Profiles (性能剖析)                                       │
│     └── 代码级别的性能分析                                    │
└─────────────────────────────────────────────────────────────┘
```

**实际场景示例：**
- 用户报告"页面加载慢"→ Metrics 显示 API 延迟高 → Traces 定位到数据库查询慢 → Logs 发现特定 SQL 问题

---

## OpenTelemetry 简介

### 什么是 OpenTelemetry

OpenTelemetry（简称 OTel）是一个开源的可观测性框架，由 CNCF（云原生计算基金会）托管，提供：

1. **标准化 API/SDK**：统一的可观测性数据采集标准
2. **多语言支持**：Rust、Go、Java、Python、JavaScript 等
3. **与后端解耦**：采集一次，发送到任意分析平台（Jaeger、Prometheus、Grafana、Langfuse 等）

### OTel 架构概览

```
┌────────────────────────────────────────────────────────────────┐
│                        你的应用程序                              │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                     │
│  │  Traces  │  │ Metrics  │  │   Logs   │                     │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘                     │
│       │             │             │                            │
│       └─────────────┼─────────────┘                            │
│                     ▼                                          │
│  ┌─────────────────────────────────────┐                      │
│  │        OpenTelemetry SDK            │                      │
│  │  ┌─────────┐ ┌─────────┐ ┌────────┐ │                      │
│  │  │ Resource│ │Context  │ │Propagator│ │                      │
│  │  └─────────┘ └─────────┘ └────────┘ │                      │
│  └──────────────────┬──────────────────┘                      │
└─────────────────────┼──────────────────────────────────────────┘
                      ▼
         ┌──────────────────────┐
         │   OTLP Exporter      │
         │  (gRPC/HTTP/HTTP+JSON)│
         └──────────┬───────────┘
                    │
                    ▼
         ┌──────────────────────┐
         │  OTel Collector      │
         │  (接收/处理/导出)     │
         └──────────┬───────────┘
                    │
        ┌───────────┼───────────┐
        ▼           ▼           ▼
   ┌─────────┐ ┌─────────┐ ┌─────────┐
   │ Langfuse│ │ Jaeger  │ │Grafana  │
   │         │ │         │ │         │
   └─────────┘ └─────────┘ └─────────┘
```

### 核心概念

| 概念 | 说明 | 类比 |
|------|------|------|
| **Resource** | 描述产生遥测数据的实体（服务名、版本、主机） | 快递包裹上的发件人信息 |
| **Context** | 跨进程的上下文传递（Trace ID、Span ID） | 快递单号 |
| **Propagator** | 在请求边界传播上下文（HTTP Headers） | 快递面单 |
| **Exporter** | 将数据发送到后端的组件 | 快递员 |
| **Collector** | 接收、处理、导出遥测数据的代理 | 快递分拣中心 |

---

## 三大信号：Traces、Metrics、Logs

### 1. Traces（分布式追踪）

#### 什么是 Trace

Trace 记录一个请求在系统中经过的完整路径，由多个 **Span**（跨度）组成：

```
Trace: 用户下单请求 (Trace ID: abc123)
├── Span: API Gateway (duration: 5ms)
├── Span: 订单服务 (duration: 50ms)
│   ├── Span: 验证用户 (duration: 10ms)
│   ├── Span: 查询库存 (duration: 20ms)
│   └── Span: 创建订单 (duration: 15ms)
├── Span: 支付服务 (duration: 100ms)
│   └── Span: 调用第三方支付 (duration: 80ms)
└── Span: 通知服务 (duration: 30ms)
```

#### Span 的核心属性

```rust
// Span 的核心字段
struct Span {
    trace_id: String,      // 整个链路的唯一标识
    span_id: String,       // 当前跨度的唯一标识
    parent_span_id: Option<String>,  // 父跨度（构成层级关系）
    name: String,          // 跨度名称（如 "llm.chat"）
    start_time: Timestamp,
    end_time: Timestamp,
    attributes: Map<String, Value>,  // 自定义属性
    events: Vec<Event>,    // 时间戳事件（如异常）
    status: Status,        // Ok / Error
}
```

#### 项目中的 Trace 实践

```rust
// crates/kiliax-core/src/llm.rs
use tracing::{info_span, Instrument};
use kiliax_core::telemetry::spans;

async fn chat_completion(&self, messages: Vec<Message>) -> Result<Message, Error> {
    // 创建 span，自动关联到当前 trace
    let span = info_span!(
        "llm.chat",
        gen_ai.system = "openai",
        gen_ai.request.model = %self.route.model,
        gen_ai.usage.input_tokens = tracing::field::Empty,
    );
    
    // 设置属性到当前 span
    spans::set_attribute(&span, "gen_ai.response.model", &self.route.model);
    
    async {
        // LLM 调用逻辑...
        let response = self.do_chat(messages).await?;
        
        // 记录 token 使用量
        spans::set_attribute(
            &span,
            "gen_ai.usage.input_tokens",
            response.usage.prompt_tokens,
        );
        
        Ok(response)
    }
    .instrument(span)  // 将 future 绑定到 span
    .await
}
```

### 2. Metrics（指标）

#### 指标类型

| 类型 | 用途 | 示例 |
|------|------|------|
| **Counter** | 单调递增的计数 | 请求总数、错误数 |
| **Histogram** | 采样值的分布 | 延迟分布、请求大小 |
| **Gauge** | 可增可减的值 | 当前连接数、CPU 使用率 |
| **UpDownCounter** | 可增可减的计数 | 队列长度、内存分配 |

#### 项目中的 Metrics 实践

```rust
// crates/kiliax-core/src/telemetry.rs
pub mod metrics {
    use opentelemetry::metrics::{Counter, Histogram};
    use opentelemetry::KeyValue;
    
    // LLM 请求计数器（带标签）
    pub fn record_llm_call(
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        latency: Duration,
        prompt_tokens: Option<u64>,
        cached_prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    ) {
        let tags = &[
            KeyValue::new("provider", provider.to_string()),
            KeyValue::new("model", model.to_string()),
            KeyValue::new("stream", stream.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ];

        // 计数器 +1
        llm_requests_total().add(1, tags);
        
        // 直方图记录延迟
        llm_latency_ms().record(latency.as_secs_f64() * 1000.0, tags);
        
        // Token 使用统计
        if let Some(tokens) = prompt_tokens {
            llm_tokens_prompt_total().add(tokens, tags);
        }
    }
}
```

**生成的指标示例：**

```
# 按 provider 和 model 聚合的 LLM 请求数
kiliax_llm_requests_total{provider="openai",model="gpt-4o",outcome="ok"} 42
kiliax_llm_requests_total{provider="moonshot_cn",model="kimi-k2",outcome="error"} 3

# 延迟分布（P50, P90, P99）
kiliax_llm_latency_ms_bucket{le="100"} 50
kiliax_llm_latency_ms_bucket{le="500"} 95
kiliax_llm_latency_ms_bucket{le="+Inf"} 100
```

### 3. Logs（日志）

#### OTel Logs 与传统日志的区别

传统日志：
```json
{
  "timestamp": "2025-01-01T12:00:00Z",
  "level": "INFO",
  "message": "User logged in",
  "user_id": "12345"
}
```

OTel Logs（与 Trace 关联）：
```json
{
  "timestamp": "2025-01-01T12:00:00Z",
  "severity": "INFO",
  "body": "User logged in",
  "trace_id": "abc123",
  "span_id": "def456",
  "attributes": {
    "user.id": "12345",
    "service.name": "kiliax"
  }
}
```

#### 项目中的 Logs 实践

```rust
// 使用 tracing 记录日志，自动转换为 OTel Logs
tracing::info!(
    target: "kiliax_otel",
    event = "otel_enabled",
    capture = "full",
    endpoint = %cfg.otlp.endpoint,
    protocol = ?cfg.otlp.protocol,
);
```

---

## 项目架构解析

### 模块结构

```
crates/
├── kiliax-otel/           # OTel 初始化与导出
│   ├── src/
│   │   ├── lib.rs         # Guard 模式初始化
│   │   └── otlp.rs        # TLS、HTTP 客户端构建
│   └── Cargo.toml         # OTel 依赖
├── kiliax-core/
│   ├── src/
│   │   ├── telemetry.rs   # 指标定义 + Span 工具
│   │   ├── config.rs      # OTel 配置结构
│   │   ├── llm.rs         # LLM Trace/Metrics 埋点
│   │   └── tools/engine.rs # Tool 调用 Trace/Metrics
│   └── Cargo.toml
└── kiliax-server/
    └── src/
        └── http/mod.rs    # HTTP 头传播 Trace
```

### 关键设计模式

#### 1. Guard 模式（资源管理）

```rust
// crates/kiliax-otel/src/lib.rs
pub struct OtelGuard {
    provider: Option<OtelProvider>,
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        // 确保优雅关闭，刷新所有数据
        if let Some(provider) = self.provider.take() {
            provider.shutdown();
        }
    }
}

// 使用方式
fn main() {
    let guard = kiliax_otel::init(&config, "kiliax", "1.0.0", LocalLogs::Stdout);
    
    // 应用逻辑...
    
    // 程序退出时自动刷新数据
    drop(guard);
}
```

#### 2. 分层订阅者（Layered Subscriber）

```rust
// 构建复合的 tracing 订阅者
tracing_subscriber::registry()
    .with(env_filter)           // 日志级别过滤
    .with(tracing_layer)        // OTel Trace 导出
    .with(logger_layer)         // OTel Log 导出
    .with(fmt_layer)            // 本地日志输出
    .try_init()
    .ok();
```

#### 3. 全局传播器（Propagator）

```rust
// 设置全局 Trace 传播器
global::set_text_map_propagator(TraceContextPropagator::new());

// 从 HTTP 头提取父 Trace 上下文
pub fn set_parent_from_http_headers(
    span: &tracing::Span,
    headers: &http::HeaderMap,
) -> bool {
    let traceparent = headers.get("traceparent").and_then(|v| v.to_str().ok());
    let tracestate = headers.get("tracestate").and_then(|v| v.to_str().ok());
    
    // 解析 W3C Trace Context 格式
    let mut headers = HashMap::new();
    headers.insert("traceparent".to_string(), traceparent?.to_string());
    
    let context = TraceContextPropagator::new().extract(&headers);
    span.set_parent(context);
    true
}
```

---

## 配置指南

### 完整配置示例

```yaml
# ~/.kiliax/kiliax.yaml
otel:
  # 总开关
  enabled: true
  
  # 环境标识（dev/staging/production）
  environment: production
  
  # OTLP 导出配置
  otlp:
    # 端点地址
    # - 本地 Collector: http://localhost:4318
    # - Langfuse Cloud: https://cloud.langfuse.com/api/public/otel
    endpoint: https://otel.mycompany.com
    
    # 协议选择
    # - http_protobuf: 默认，高效二进制
    # - http_json: 便于调试
    # - grpc: 高性能流式传输
    protocol: http_protobuf
    
    # 认证头（如 Langfuse Basic Auth）
    headers:
      authorization: "Basic <base64(public:secret)>"
      x-custom-header: "value"
    
    # TLS 配置（mTLS 支持）
    tls:
      ca_cert: /path/to/ca.pem
      client_cert: /path/to/client.pem
      client_key: /path/to/client.key
  
  # 信号开关
  signals:
    logs: true      # 启用日志收集
    traces: true    # 启用分布式追踪
    metrics: true   # 启用指标收集
  
  # 数据采集配置
  capture:
    # 模式：metadata（仅元数据）| full（完整内容）
    mode: full
    
    # 最大字节数（防止超大请求）
    max_bytes: 65536
    
    # 是否包含 base64 图片
    include_images: false
    
    # 内容哈希算法（用于去重）
    hash: sha256
```

### 不同环境配置

#### 开发环境

```yaml
otel:
  enabled: true
  environment: dev
  otlp:
    endpoint: http://localhost:4318
    protocol: http_protobuf
  capture:
    mode: full        # 开发时收集完整数据
    max_bytes: 1048576  # 1MB
```

#### 生产环境

```yaml
otel:
  enabled: true
  environment: production
  otlp:
    endpoint: https://otel-collector.internal:4318
    protocol: grpc    # 更高性能
    tls:
      ca_cert: /etc/ssl/certs/ca.crt
  signals:
    logs: false       # 生产可能只开 Trace/Metrics
    traces: true
    metrics: true
  capture:
    mode: metadata    # 只收集元数据，保护敏感信息
    max_bytes: 4096
```

---

## 代码实践

### 1. 初始化 OTel

```rust
use kiliax_otel::{init, LocalLogs};
use kiliax_core::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载配置
    let config = Config::load()?;
    
    // 初始化 OTel
    let _otel_guard = init(
        &config,
        "kiliax-server",      // 服务名
        env!("CARGO_PKG_VERSION"),  // 版本号
        LocalLogs::Stdout,    // 本地日志输出
    )?;
    
    // 运行应用...
    run_server().await;
    
    // Guard 在 drop 时会自动关闭 OTel
    Ok(())
}
```

### 2. 创建自定义 Span

```rust
use tracing::{info_span, info};
use kiliax_core::telemetry::spans;

async fn process_user_request(user_id: &str, request: Request) -> Result<Response, Error> {
    // 创建带属性的 span
    let span = info_span!(
        "process_request",
        user.id = %user_id,
        request.type = %request.type_(),
        request.size = request.body.len(),
    );
    
    // 使用 instrument 将 async 块绑定到 span
    async move {
        info!("开始处理请求");
        
        // 业务逻辑...
        let result = do_processing(request).await?;
        
        // 动态添加属性
        spans::set_attribute(
            &tracing::Span::current(),
            "result.status",
            "success"
        );
        
        info!("请求处理完成");
        Ok(result)
    }
    .instrument(span)
    .await
}
```

### 3. 记录自定义指标

```rust
use kiliax_core::telemetry::metrics;
use std::time::Instant;

async fn call_external_api(params: &Params) -> Result<ApiResponse, Error> {
    let start = Instant::now();
    
    let result = api_client.call(params).await;
    
    // 记录指标
    metrics::record_tool_call(
        "external_api",           // 工具名
        "http",                   // 类型
        if result.is_ok() { "ok" } else { "error" },
        start.elapsed(),
    );
    
    result
}

// 添加新的指标类型
pub fn record_custom_business_metric(
    customer_tier: &str,
    value: f64,
) {
    use opentelemetry::global;
    use opentelemetry::metrics::Histogram;
    use opentelemetry::KeyValue;
    use std::sync::OnceLock;
    
    static METRIC: OnceLock<Histogram<f64>> = OnceLock::new();
    
    let histogram = METRIC.get_or_init(|| {
        global::meter("kiliax")
            .f64_histogram("kiliax_business_value")
            .with_description("Business value per transaction")
            .with_unit("usd")
            .build()
    });
    
    histogram.record(value, &[KeyValue::new("tier", customer_tier.to_string())]);
}
```

### 4. 传播 Trace 上下文

```rust
use kiliax_otel::set_parent_from_http_headers;
use axum::{extract::Request, middleware::Next};

// 中间件：从请求头提取 trace 上下文
async fn trace_propagation_middleware(
    request: Request,
    next: Next,
) -> Response {
    let span = info_span!("http_request",
        http.method = %request.method(),
        http.route = %request.uri().path(),
    );
    
    // 从传入请求的 traceparent 头提取父 span
    set_parent_from_http_headers(&span, request.headers());
    
    next.run(request).instrument(span).await
}
```

### 5. 条件性数据采集

```rust
use kiliax_core::telemetry;

fn handle_sensitive_data(data: &SensitiveData) {
    // 只在 full 模式下捕获详细内容
    if telemetry::capture_full() {
        let json = serde_json::to_string(&data.details).unwrap();
        let captured = telemetry::capture_text(&json);
        
        spans::set_attribute(
            &tracing::Span::current(),
            "request.details",
            captured.as_str(),
        );
    }
    
    // 总是记录元数据
    spans::set_attribute(
        &tracing::Span::current(),
        "request.size",
        data.size,
    );
}
```

---

## 与 Langfuse 集成

### 为什么选择 Langfuse

Langfuse 是专为 LLM 应用设计的可观测性平台，完美支持 OpenTelemetry：

```
┌─────────────────────────────────────────────────────────────┐
│                     Langfuse 特性                            │
├─────────────────────────────────────────────────────────────┤
│  🔍 Trace 可视化 - 查看 LLM 调用链                           │
│  💰 成本追踪 - 自动计算 token 费用                           │
│  📊 延迟分析 - TTFT (Time To First Token) 等指标             │
│  🧪 提示词管理 - 版本控制和 A/B 测试                         │
│  📈 评估指标 - 集成人工评估和自动化评分                       │
└─────────────────────────────────────────────────────────────┘
```

### Langfuse 配置

```yaml
otel:
  enabled: true
  environment: production
  otlp:
    endpoint: https://cloud.langfuse.com/api/public/otel
    protocol: http_protobuf
    headers:
      # 生成方法：echo -n "public_key:secret_key" | base64
      authorization: "Basic <base64_encoded_credentials>"
  capture:
    mode: full
    max_bytes: 65536
```

### 特殊属性映射

项目针对 Langfuse 优化了一些属性名：

```rust
// 设置 Langfuse 特定的观察类型
spans::set_attribute(&span, "langfuse.observation.type", "tool");

// 输入输出（在 Langfuse UI 中特殊展示）
spans::set_attribute(&span, "langfuse.observation.input", prompt_json);
spans::set_attribute(&span, "langfuse.observation.output", completion_json);

// 设置 completion_start_time 用于 TTFT 计算
spans::set_attribute(
    &span,
    "langfuse.observation.completion_start_time",
    "2025-01-01T12:00:00.123Z"
);
```

### 查看效果

配置完成后，在 Langfuse 仪表板中你可以看到：

```
Trace: chat_completion
├── Generation: llm.chat (gpt-4o)
│   ├── Input: {"messages": [{"role": "user", "content": "Hello"}]}
│   ├── Output: {"content": "Hi there!"}
│   ├── Tokens: 10 prompt / 20 completion
│   ├── Cost: $0.0015
│   └── Latency: 850ms (TTFT: 120ms)
└── Span: tool.read_file
    ├── Input: {"path": "/tmp/test.txt"}
    └── Output: {"content": "Hello World"}
```

---

## 最佳实践

### 1. Span 命名规范

```rust
// ✅ 好的命名：具体、可识别
"llm.chat"
"tool.read_file"
"db.query"
"http.request"

// ❌ 避免：太笼统
"request"
"operation"
"handler"
```

### 2. 属性命名规范

使用命名空间前缀：

```rust
// OpenTelemetry 语义约定
"http.method" = "GET"
"http.status_code" = 200
"db.system" = "postgresql"

// GenAI 语义约定
"gen_ai.system" = "openai"
"gen_ai.request.model" = "gpt-4o"
"gen_ai.usage.input_tokens" = 150

// 项目自定义
"kiliax.tool.name" = "read_file"
"kiliax.llm.ttft_ms" = 120
```

### 3. 采样策略

```rust
// 基于概率的头部采样（适合高流量）
use opentelemetry_sdk::trace::Sampler;

let tracer_provider = SdkTracerProvider::builder()
    .with_sampler(Sampler::TraceIdRatioBased(0.1))  // 10% 采样
    .build();

// 尾部采样（捕获错误请求）
// 需要 OTel Collector 配置
```

### 4. 性能优化

```rust
// 1. 使用 BatchSpanProcessor（默认）
// 批量发送，减少网络开销

// 2. 异步运行时选择
// - 多线程 tokio: 使用 TokioBatchSpanProcessor
// - 单线程: 使用标准 BatchSpanProcessor

// 3. 超时配置
// OTEL_EXPORTER_OTLP_TIMEOUT=10000  # 10s

// 4. 本地日志与 OTel 分离
// - 开发：都开启
// - 生产：只开 Metrics，Trace 采样
```

### 5. 安全考虑

```rust
// ✅ 安全的做法：记录元数据，过滤敏感内容
spans::set_attribute(&span, "user.id", user_id);  // 安全
spans::set_attribute(&span, "request.size", body.len());  // 安全

// ❌ 危险：记录敏感信息
spans::set_attribute(&span, "user.password", password);
spans::set_attribute(&span, "request.body", body);  // 可能包含敏感信息

// ✅ 使用 capture 配置控制
capture:
  mode: metadata  # 生产环境
  max_bytes: 4096
```

---

## 故障排查

### 常见问题

#### 1. 数据未到达后端

```bash
# 检查网络连通性
curl -v http://localhost:4318/v1/traces

# 启用 OTel 调试日志
export OTEL_LOG_LEVEL=debug

# 检查 Rust 日志
RUST_LOG=opentelemetry=debug,kiliax_otel=debug cargo run
```

#### 2. Trace 未关联

```rust
// 确保使用了正确的传播器
global::set_text_map_propagator(TraceContextPropagator::new());

// 检查 traceparent 头格式
// 正确：00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
// 版本-TraceId-SpanId-标志
```

#### 3. 性能问题

```rust
// 诊断：启用内部指标
// 检查 span 队列堆积

// 解决：调整批处理参数
let processor = BatchSpanProcessor::builder(exporter)
    .with_max_queue_size(2048)
    .with_max_export_batch_size(512)
    .with_scheduled_delay(Duration::from_millis(1000))
    .build();
```

#### 4. 内存泄漏

```rust
// 确保 Guard 被正确持有
let _guard = init(&config, ...)?;

// 避免在循环中创建过多 span
// 使用 span 的 is_none() 检查
```

### 调试技巧

```rust
// 打印当前 span 信息
let span = tracing::Span::current();
println!("Current span: {:?}", span.metadata());

// 获取 trace_id
if let Some(trace_id) = telemetry::spans::current_trace_id() {
    println!("Trace ID: {}", trace_id);
}

// 本地验证：使用 stdout exporter
otel:
  enabled: true
  # 配置 stdout 输出查看数据结构
```

---

## 学习资源

### 官方文档

- [OpenTelemetry 官方文档](https://opentelemetry.io/docs/)
- [Rust OTel API 文档](https://docs.rs/opentelemetry/)
- [OpenTelemetry 语义约定](https://opentelemetry.io/docs/specs/semconv/)

### 项目参考

- `crates/kiliax-otel/src/lib.rs` - 完整初始化流程
- `crates/kiliax-core/src/telemetry.rs` - 指标定义和工具函数
- `crates/kiliax-core/src/llm.rs` - 实际埋点示例

### 推荐工具

- **Jaeger**: 本地 Trace 查看（`docker run -p 16686:16686 jaegertracing/all-in-one`）
- **Prometheus**: Metrics 存储和查询
- **Grafana**: 可视化仪表板
- **Langfuse**: LLM 应用可观测性

---

## 总结

OpenTelemetry 为现代应用提供了统一的可观测性解决方案。通过本项目，你学习了：

1. **基础概念**: Traces、Metrics、Logs 的定义和用途
2. **架构设计**: Guard 模式、分层订阅者、上下文传播
3. **实际应用**: 在 Rust 中实现 LLM 应用的可观测性
4. **最佳实践**: 命名规范、性能优化、安全考虑
5. **平台集成**: 与 Langfuse 等平台的对接

下一步建议：
- 在本地搭建 Jaeger 练习 Trace 分析
- 尝试接入 Langfuse Cloud 观察真实 LLM 调用
- 阅读 OpenTelemetry 语义约定，了解标准属性命名

---

*本文档基于 Kiliax 项目 v0.1.0 版本编写*
