use std::sync::{OnceLock, RwLock};

use sha2::Digest as _;
use sha2::Sha256;

use crate::config::{OtelCaptureConfig, OtelCaptureHash, OtelCaptureMode};

static CAPTURE_CONFIG: OnceLock<RwLock<Option<OtelCaptureConfig>>> = OnceLock::new();

fn lock() -> &'static RwLock<Option<OtelCaptureConfig>> {
    CAPTURE_CONFIG.get_or_init(|| RwLock::new(None))
}

pub fn set_capture_config(cfg: Option<OtelCaptureConfig>) {
    let mut guard = lock()
        .write()
        .expect("telemetry capture config lock poisoned");
    *guard = cfg;
}

pub fn capture_config() -> Option<OtelCaptureConfig> {
    lock()
        .read()
        .expect("telemetry capture config lock poisoned")
        .clone()
}

pub fn capture_enabled() -> bool {
    lock()
        .read()
        .expect("telemetry capture config lock poisoned")
        .is_some()
}

pub fn capture_full() -> bool {
    capture_config().is_some_and(|c| matches!(c.mode, OtelCaptureMode::Full))
}

pub fn capture_max_bytes() -> usize {
    capture_config().map(|c| c.max_bytes).unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct CapturedText {
    pub len: usize,
    pub truncated: bool,
    pub sha256: Option<String>,
    pub text: Option<String>,
}

impl CapturedText {
    pub fn as_str(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }
}

pub fn capture_text(raw: &str) -> CapturedText {
    let Some(cfg) = capture_config() else {
        return CapturedText {
            len: raw.len(),
            truncated: false,
            sha256: None,
            text: None,
        };
    };

    let sha256 = match cfg.hash {
        OtelCaptureHash::None => None,
        OtelCaptureHash::Sha256 => Some(sha256_hex(raw.as_bytes())),
    };

    let max_bytes = cfg.max_bytes;
    let truncated = max_bytes > 0 && raw.len() > max_bytes;
    let text = match cfg.mode {
        OtelCaptureMode::Metadata => None,
        OtelCaptureMode::Full => {
            let clipped = if max_bytes == 0 {
                ""
            } else {
                truncate_utf8(raw, max_bytes)
            };
            Some(clipped.to_string())
        }
    };

    CapturedText {
        len: raw.len(),
        truncated,
        sha256,
        text,
    }
}

fn truncate_utf8(raw: &str, max_bytes: usize) -> &str {
    if raw.len() <= max_bytes {
        return raw;
    }

    let mut idx = max_bytes;
    while idx > 0 && !raw.is_char_boundary(idx) {
        idx -= 1;
    }
    &raw[..idx]
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        use std::fmt::Write as _;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

pub mod spans {
    use opentelemetry::trace::TraceContextExt as _;
    use opentelemetry::{Key, KeyValue, Value};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    pub fn trace_id_hex(span: &tracing::Span) -> Option<String> {
        let ctx = span.context();
        let otel_span = ctx.span();
        let sc = otel_span.span_context();
        if !sc.is_valid() {
            return None;
        }
        Some(sc.trace_id().to_string())
    }

    pub fn current_trace_id() -> Option<String> {
        trace_id_hex(&tracing::Span::current())
    }

    pub fn set_attributes(span: &tracing::Span, attributes: impl IntoIterator<Item = KeyValue>) {
        let ctx = span.context();
        let otel_span = ctx.span();
        if !otel_span.span_context().is_valid() {
            return;
        }

        for kv in attributes {
            otel_span.set_attribute(kv);
        }
    }

    pub fn set_attribute<K, V>(span: &tracing::Span, key: K, value: V)
    where
        K: Into<Key>,
        V: Into<Value>,
    {
        set_attributes(span, [KeyValue::new(key, value)]);
    }
}

pub mod metrics {
    use std::sync::OnceLock;
    use std::time::Duration;

    use opentelemetry::global;
    use opentelemetry::metrics::{Counter, Histogram};
    use opentelemetry::KeyValue;

    fn meter() -> opentelemetry::metrics::Meter {
        global::meter("kiliax")
    }

    static LLM_REQUESTS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static LLM_LATENCY_MS: OnceLock<Histogram<f64>> = OnceLock::new();
    static LLM_TOKENS_PROMPT_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static LLM_TOKENS_PROMPT_CACHED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static LLM_TOKENS_COMPLETION_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    static TOOL_CALLS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static TOOL_LATENCY_MS: OnceLock<Histogram<f64>> = OnceLock::new();

    static MCP_CALLS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static MCP_LATENCY_MS: OnceLock<Histogram<f64>> = OnceLock::new();
    static MCP_CONNECT_FAILURES_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    static SKILLS_DISCOVERED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    static RUNS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static STEPS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static RUN_DURATION_MS: OnceLock<Histogram<f64>> = OnceLock::new();

    fn llm_requests_total() -> &'static Counter<u64> {
        LLM_REQUESTS_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_llm_requests_total")
                .with_description("Total number of LLM chat requests")
                .build()
        })
    }

    fn llm_latency_ms() -> &'static Histogram<f64> {
        LLM_LATENCY_MS.get_or_init(|| {
            meter()
                .f64_histogram("kiliax_llm_latency_ms")
                .with_description("LLM request latency in milliseconds")
                .with_unit("ms")
                .build()
        })
    }

    fn llm_tokens_prompt_total() -> &'static Counter<u64> {
        LLM_TOKENS_PROMPT_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_llm_tokens_prompt_total")
                .with_description("Total prompt tokens consumed by LLM requests")
                .build()
        })
    }

    fn llm_tokens_prompt_cached_total() -> &'static Counter<u64> {
        LLM_TOKENS_PROMPT_CACHED_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_llm_tokens_prompt_cached_total")
                .with_description("Total cached prompt tokens consumed by LLM requests")
                .build()
        })
    }

    fn llm_tokens_completion_total() -> &'static Counter<u64> {
        LLM_TOKENS_COMPLETION_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_llm_tokens_completion_total")
                .with_description("Total completion tokens generated by LLM requests")
                .build()
        })
    }

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

        llm_requests_total().add(1, tags);
        llm_latency_ms().record(latency.as_secs_f64() * 1000.0, tags);

        if let Some(tokens) = prompt_tokens {
            llm_tokens_prompt_total().add(tokens, tags);
        }
        if let Some(tokens) = cached_prompt_tokens {
            llm_tokens_prompt_cached_total().add(tokens, tags);
        }
        if let Some(tokens) = completion_tokens {
            llm_tokens_completion_total().add(tokens, tags);
        }
    }

    fn tool_calls_total() -> &'static Counter<u64> {
        TOOL_CALLS_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_tool_calls_total")
                .with_description("Total number of tool calls")
                .build()
        })
    }

    fn tool_latency_ms() -> &'static Histogram<f64> {
        TOOL_LATENCY_MS.get_or_init(|| {
            meter()
                .f64_histogram("kiliax_tool_latency_ms")
                .with_description("Tool call latency in milliseconds")
                .with_unit("ms")
                .build()
        })
    }

    pub fn record_tool_call(tool: &str, kind: &str, outcome: &str, latency: Duration) {
        let tags = &[
            KeyValue::new("tool", tool.to_string()),
            KeyValue::new("kind", kind.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ];
        tool_calls_total().add(1, tags);
        tool_latency_ms().record(latency.as_secs_f64() * 1000.0, tags);
    }

    fn mcp_calls_total() -> &'static Counter<u64> {
        MCP_CALLS_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_mcp_calls_total")
                .with_description("Total number of MCP tool calls")
                .build()
        })
    }

    fn mcp_latency_ms() -> &'static Histogram<f64> {
        MCP_LATENCY_MS.get_or_init(|| {
            meter()
                .f64_histogram("kiliax_mcp_latency_ms")
                .with_description("MCP tool call latency in milliseconds")
                .with_unit("ms")
                .build()
        })
    }

    fn mcp_connect_failures_total() -> &'static Counter<u64> {
        MCP_CONNECT_FAILURES_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_mcp_connect_failures_total")
                .with_description("Total number of MCP connect failures")
                .build()
        })
    }

    pub fn record_mcp_call(server: &str, tool: &str, outcome: &str, latency: Duration) {
        let tags = &[
            KeyValue::new("server", server.to_string()),
            KeyValue::new("tool", tool.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ];
        mcp_calls_total().add(1, tags);
        mcp_latency_ms().record(latency.as_secs_f64() * 1000.0, tags);
    }

    pub fn record_mcp_connect_failure(server: &str) {
        let tags = &[KeyValue::new("server", server.to_string())];
        mcp_connect_failures_total().add(1, tags);
    }

    fn skills_discovered_total() -> &'static Counter<u64> {
        SKILLS_DISCOVERED_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_skills_discovered_total")
                .with_description("Total number of discovered skills")
                .build()
        })
    }

    pub fn record_skills_discovered(count: usize) {
        skills_discovered_total().add(count as u64, &[]);
    }

    fn runs_total() -> &'static Counter<u64> {
        RUNS_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_runs_total")
                .with_description("Total number of agent runs")
                .build()
        })
    }

    fn steps_total() -> &'static Counter<u64> {
        STEPS_TOTAL.get_or_init(|| {
            meter()
                .u64_counter("kiliax_steps_total")
                .with_description("Total number of agent steps")
                .build()
        })
    }

    fn run_duration_ms() -> &'static Histogram<f64> {
        RUN_DURATION_MS.get_or_init(|| {
            meter()
                .f64_histogram("kiliax_run_duration_ms")
                .with_description("Agent run duration in milliseconds")
                .with_unit("ms")
                .build()
        })
    }

    pub fn record_run_finished(agent: &str, outcome: &str, steps: u64, duration: Duration) {
        let tags = &[
            KeyValue::new("agent", agent.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ];
        runs_total().add(1, tags);
        steps_total().add(steps, tags);
        run_duration_ms().record(duration.as_secs_f64() * 1000.0, tags);
    }
}
