use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use opentelemetry::{Key, KeyValue, Value as OtelValue};
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

use crate::types::{
    ChatResponse, ChatStreamChunk, FinishReason, ProviderMessageMetadata, TokenUsage,
};

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

#[derive(Debug, Clone, Copy)]
pub struct LlmCallMetrics<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub stream: bool,
    pub outcome: &'a str,
    pub latency: Duration,
    pub prompt_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

pub trait LlmTelemetry: Send + Sync + 'static {
    fn capture_enabled(&self) -> bool {
        false
    }

    fn capture_full(&self) -> bool {
        false
    }

    fn capture_max_bytes(&self) -> usize {
        0
    }

    fn capture_text(&self, raw: &str) -> CapturedText {
        CapturedText {
            len: raw.len(),
            truncated: false,
            sha256: None,
            text: None,
        }
    }

    fn set_span_attributes(&self, _span: &tracing::Span, _attributes: Vec<KeyValue>) {}

    fn record_llm_call(&self, _call: &LlmCallMetrics<'_>) {}

    fn record_llm_ttft(
        &self,
        _provider: &str,
        _model: &str,
        _stream: bool,
        _outcome: &str,
        _ttft: Duration,
    ) {
    }

    fn record_llm_output_tps(
        &self,
        _provider: &str,
        _model: &str,
        _stream: bool,
        _outcome: &str,
        _output_tps: f64,
    ) {
    }

    fn record_llm_output_tps_after_ttft(
        &self,
        _provider: &str,
        _model: &str,
        _stream: bool,
        _outcome: &str,
        _output_tps: f64,
    ) {
    }
}

static HOOK: OnceLock<RwLock<Option<Arc<dyn LlmTelemetry>>>> = OnceLock::new();

fn hook_cell() -> &'static RwLock<Option<Arc<dyn LlmTelemetry>>> {
    HOOK.get_or_init(|| RwLock::new(None))
}

pub fn set_hook(hook: Arc<dyn LlmTelemetry>) {
    let mut guard = hook_cell()
        .write()
        .expect("llm telemetry hook lock poisoned");
    *guard = Some(hook);
}

fn with_hook<R>(f: impl FnOnce(&dyn LlmTelemetry) -> R) -> Option<R> {
    let guard = hook_cell()
        .read()
        .expect("llm telemetry hook lock poisoned");
    guard.as_deref().map(f)
}

pub fn capture_enabled() -> bool {
    with_hook(|hook| hook.capture_enabled()).unwrap_or(false)
}

pub fn capture_full() -> bool {
    with_hook(|hook| hook.capture_full()).unwrap_or(false)
}

pub fn capture_max_bytes() -> usize {
    with_hook(|hook| hook.capture_max_bytes()).unwrap_or(0)
}

pub fn capture_text(raw: &str) -> CapturedText {
    with_hook(|hook| hook.capture_text(raw)).unwrap_or(CapturedText {
        len: raw.len(),
        truncated: false,
        sha256: None,
        text: None,
    })
}

pub mod spans {
    use super::*;

    pub fn set_attributes(span: &tracing::Span, attributes: impl IntoIterator<Item = KeyValue>) {
        let attributes: Vec<KeyValue> = attributes.into_iter().collect();
        let _ = with_hook(|hook| hook.set_span_attributes(span, attributes));
    }

    pub fn set_attribute<K, V>(span: &tracing::Span, key: K, value: V)
    where
        K: Into<Key>,
        V: Into<OtelValue>,
    {
        set_attributes(span, [KeyValue::new(key, value)]);
    }
}

pub(crate) fn record_generation_start(span: &tracing::Span, provider: &str, model: &str) {
    spans::set_attributes(
        span,
        [
            KeyValue::new("langfuse.observation.type", "generation"),
            KeyValue::new("langfuse.observation.model.name", model.to_string()),
            KeyValue::new("gen_ai.system", provider.to_string()),
            KeyValue::new("gen_ai.request.model", model.to_string()),
        ],
    );
}

pub(crate) fn record_generation_input<T>(span: &tracing::Span, stream: bool, input: &T)
where
    T: Serialize + ?Sized,
{
    if !capture_enabled() {
        return;
    }
    let Ok(json) = serde_json::to_string(input) else {
        return;
    };

    let captured = capture_text(&json);
    if capture_full() {
        let value = captured.as_str().to_string();
        spans::set_attributes(
            span,
            [
                KeyValue::new("langfuse.observation.input", value.clone()),
                KeyValue::new("gen_ai.prompt", value),
            ],
        );
    }
    tracing::info!(
        target: "kiliax_core::telemetry",
        parent: span,
        event = "llm.request",
        llm_stream = stream,
        request_len = captured.len as u64,
        request_truncated = captured.truncated,
        request_sha256 = %captured.sha256.as_deref().unwrap_or(""),
        request = %captured.as_str(),
    );
}

pub(crate) fn record_generation_output<T>(
    span: &tracing::Span,
    stream: bool,
    finish_reason: Option<&FinishReason>,
    output: &T,
) where
    T: Serialize + ?Sized,
{
    if !capture_enabled() {
        return;
    }
    let Ok(json) = serde_json::to_string(output) else {
        return;
    };

    let captured = capture_text(&json);
    if capture_full() {
        let value = captured.as_str().to_string();
        spans::set_attributes(
            span,
            [
                KeyValue::new("langfuse.observation.output", value.clone()),
                KeyValue::new("gen_ai.completion", value),
            ],
        );
    }
    tracing::info!(
        target: "kiliax_core::telemetry",
        parent: span,
        event = "llm.response",
        llm_stream = stream,
        finish_reason = ?finish_reason,
        response_len = captured.len as u64,
        response_truncated = captured.truncated,
        response_sha256 = %captured.sha256.as_deref().unwrap_or(""),
        response = %captured.as_str(),
    );
}

pub(crate) fn record_generation_response_model(span: &tracing::Span, model: &str) {
    spans::set_attributes(
        span,
        [
            KeyValue::new("langfuse.observation.model.name", model.to_string()),
            KeyValue::new("gen_ai.response.model", model.to_string()),
        ],
    );
}

pub(crate) fn record_generation_usage(span: &tracing::Span, usage: &TokenUsage) {
    let cached = usage.cached_tokens.unwrap_or(0);
    let usage_details = json!({
        "input": usage.prompt_tokens,
        "output": usage.completion_tokens,
        "total": usage.total_tokens,
        "cached_input": cached,
    })
    .to_string();

    spans::set_attributes(
        span,
        [
            KeyValue::new("langfuse.observation.usage_details", usage_details),
            KeyValue::new("gen_ai.usage.input_tokens", usage.prompt_tokens as i64),
            KeyValue::new("gen_ai.usage.cached_input_tokens", cached as i64),
            KeyValue::new("gen_ai.usage.output_tokens", usage.completion_tokens as i64),
            KeyValue::new("gen_ai.usage.total_tokens", usage.total_tokens as i64),
        ],
    );
}

pub(crate) fn record_generation_error<E>(span: &tracing::Span, stream: bool, err: &E)
where
    E: std::fmt::Display + ?Sized,
{
    spans::set_attributes(
        span,
        [
            KeyValue::new("langfuse.observation.level", "ERROR"),
            KeyValue::new("langfuse.observation.status_message", err.to_string()),
        ],
    );
    tracing::warn!(
        target: "kiliax_core::telemetry",
        parent: span,
        event = "llm.error",
        llm_stream = stream,
        error = %err,
    );
}

pub(crate) fn record_generation_success(
    span: &tracing::Span,
    provider: &str,
    model: &str,
    stream: bool,
    latency: Duration,
    response: &ChatResponse,
) {
    record_generation_response_model(span, &response.model);
    if let Some(usage) = response.usage.as_ref() {
        record_generation_usage(span, usage);
        record_generation_output_tps(span, provider, model, stream, "ok", latency, usage);
    }
    record_generation_output(
        span,
        stream,
        response.finish_reason.as_ref(),
        &response.message,
    );
}

pub(crate) fn record_stream_generation_finish(
    span: &tracing::Span,
    finish: StreamGenerationFinish<'_>,
) {
    let StreamGenerationFinish {
        provider,
        model,
        outcome,
        latency,
        ttft,
        completion_start_time,
        usage,
        output_capture,
        error,
    } = finish;
    record_generation_response_model(span, model);
    if let Some(ttft) = ttft {
        metrics::record_llm_ttft(provider, model, true, outcome, ttft);
        spans::set_attribute(span, "kiliax.llm.ttft_ms", ttft.as_millis() as i64);
        if let Some(ts) = completion_start_time {
            spans::set_attribute(span, "langfuse.observation.completion_start_time", ts);
        }
    }
    if let Some(usage) = usage.as_ref() {
        record_generation_usage(span, usage);
        record_generation_output_tps(span, provider, model, true, outcome, latency, usage);
        if let Some(ttft) = ttft {
            let gen_s = latency.saturating_sub(ttft).as_secs_f64();
            if gen_s > 0.0 {
                let output_tps = usage.completion_tokens as f64 / gen_s;
                metrics::record_llm_output_tps_after_ttft(
                    provider, model, true, outcome, output_tps,
                );
                spans::set_attribute(span, "kiliax.llm.output_tps_after_ttft", output_tps);
            }
        }
    }
    if let Some(error) = error {
        record_generation_error(span, true, error);
    } else if outcome == "ok" || outcome == "cancelled" {
        if let Some(output_capture) = output_capture {
            let finish_reason = output_capture.finish_reason().cloned();
            if let Some(output) = output_capture.into_value() {
                record_generation_output(span, true, finish_reason.as_ref(), &output);
            }
        }
    } else {
        spans::set_attribute(span, "langfuse.observation.level", "ERROR");
    }
}

pub(crate) struct StreamGenerationFinish<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub outcome: &'a str,
    pub latency: Duration,
    pub ttft: Option<Duration>,
    pub completion_start_time: Option<String>,
    pub usage: Option<TokenUsage>,
    pub output_capture: Option<StreamOutputCapture>,
    pub error: Option<&'a str>,
}

fn record_generation_output_tps(
    span: &tracing::Span,
    provider: &str,
    model: &str,
    stream: bool,
    outcome: &str,
    latency: Duration,
    usage: &TokenUsage,
) {
    let total_s = latency.as_secs_f64();
    if total_s <= 0.0 {
        return;
    }

    let output_tps = usage.completion_tokens as f64 / total_s;
    spans::set_attribute(span, "kiliax.llm.output_tps", output_tps);
    metrics::record_llm_output_tps(provider, model, stream, outcome, output_tps);
}

#[derive(Debug)]
pub(crate) struct StreamOutputCapture {
    content: String,
    thinking: String,
    tool_calls: std::collections::BTreeMap<u32, crate::types::ToolCallDelta>,
    finish_reason: Option<FinishReason>,
    provider_metadata: Option<ProviderMessageMetadata>,
    max_bytes: usize,
    bytes: usize,
    truncated: bool,
}

impl StreamOutputCapture {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            content: String::new(),
            thinking: String::new(),
            tool_calls: std::collections::BTreeMap::new(),
            finish_reason: None,
            provider_metadata: None,
            max_bytes,
            bytes: 0,
            truncated: false,
        }
    }

    pub(crate) fn observe_chunk(&mut self, chunk: &ChatStreamChunk) {
        if let Some(delta) = chunk.content_delta.as_deref() {
            Self::append_limited(
                self.max_bytes,
                &mut self.bytes,
                &mut self.truncated,
                &mut self.content,
                delta,
            );
        }
        if let Some(delta) = chunk.thinking_delta.as_deref() {
            Self::append_limited(
                self.max_bytes,
                &mut self.bytes,
                &mut self.truncated,
                &mut self.thinking,
                delta,
            );
        }
        for delta in &chunk.tool_calls {
            let entry =
                self.tool_calls
                    .entry(delta.index)
                    .or_insert_with(|| crate::types::ToolCallDelta {
                        index: delta.index,
                        id: None,
                        name: None,
                        arguments: None,
                    });
            if let Some(id) = delta.id.as_ref() {
                entry.id = Some(id.clone());
            }
            if let Some(name) = delta.name.as_ref() {
                entry.name = Some(name.clone());
            }
            if let Some(arguments) = delta.arguments.as_deref() {
                let target = entry.arguments.get_or_insert_with(String::new);
                Self::append_limited(
                    self.max_bytes,
                    &mut self.bytes,
                    &mut self.truncated,
                    target,
                    arguments,
                );
            }
        }
        if chunk.provider_metadata.is_some() {
            self.provider_metadata = chunk.provider_metadata.clone();
        }
        if let Some(finish_reason) = chunk.finish_reason.as_ref() {
            self.finish_reason = Some(finish_reason.clone());
        }
    }

    pub(crate) fn finish_reason(&self) -> Option<&FinishReason> {
        self.finish_reason.as_ref()
    }

    pub(crate) fn into_value(self) -> Option<JsonValue> {
        let has_output = !self.content.is_empty()
            || !self.thinking.is_empty()
            || !self.tool_calls.is_empty()
            || self.finish_reason.is_some()
            || self.provider_metadata.is_some()
            || self.truncated;
        if !has_output {
            return None;
        }

        let mut output = serde_json::Map::new();
        if !self.content.is_empty() {
            output.insert("content".to_string(), JsonValue::String(self.content));
        }
        if !self.thinking.is_empty() {
            output.insert(
                "reasoning_content".to_string(),
                JsonValue::String(self.thinking),
            );
        }
        if !self.tool_calls.is_empty() {
            output.insert(
                "tool_calls".to_string(),
                serde_json::to_value(self.tool_calls.into_values().collect::<Vec<_>>())
                    .unwrap_or(JsonValue::Array(Vec::new())),
            );
        }
        if let Some(finish_reason) = self.finish_reason {
            output.insert(
                "finish_reason".to_string(),
                serde_json::to_value(finish_reason).unwrap_or(JsonValue::Null),
            );
        }
        if let Some(provider_metadata) = self.provider_metadata {
            output.insert(
                "provider_metadata".to_string(),
                serde_json::to_value(provider_metadata).unwrap_or(JsonValue::Null),
            );
        }
        if self.truncated {
            output.insert("truncated".to_string(), JsonValue::Bool(true));
        }

        Some(JsonValue::Object(output))
    }

    fn append_limited(
        max_bytes: usize,
        bytes: &mut usize,
        truncated: &mut bool,
        target: &mut String,
        value: &str,
    ) {
        if value.is_empty() || *truncated {
            return;
        }
        let remaining = max_bytes.saturating_sub(*bytes);
        if remaining == 0 {
            *truncated = true;
            return;
        }
        if value.len() <= remaining {
            target.push_str(value);
            *bytes += value.len();
            return;
        }

        let mut end = remaining.min(value.len());
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        if end > 0 {
            target.push_str(&value[..end]);
            *bytes += end;
        }
        *truncated = true;
    }
}

pub mod metrics {
    use super::*;

    pub fn record_llm_call(call: LlmCallMetrics<'_>) {
        let _ = with_hook(|hook| hook.record_llm_call(&call));
    }

    pub fn record_llm_ttft(
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        ttft: Duration,
    ) {
        let _ = with_hook(|hook| hook.record_llm_ttft(provider, model, stream, outcome, ttft));
    }

    pub fn record_llm_output_tps(
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        output_tps: f64,
    ) {
        let _ = with_hook(|hook| {
            hook.record_llm_output_tps(provider, model, stream, outcome, output_tps)
        });
    }

    pub fn record_llm_output_tps_after_ttft(
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        output_tps: f64,
    ) {
        let _ = with_hook(|hook| {
            hook.record_llm_output_tps_after_ttft(provider, model, stream, outcome, output_tps)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolCallDelta;

    #[test]
    fn stream_output_capture_collects_text_thinking_tool_calls_and_finish_reason() {
        let mut capture = StreamOutputCapture::new(1024);

        capture.observe_chunk(&ChatStreamChunk {
            id: "msg_1".to_string(),
            created: 0,
            model: "model".to_string(),
            content_delta: Some("hello".to_string()),
            thinking_delta: Some("think".to_string()),
            tool_calls: vec![ToolCallDelta {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("read_file".to_string()),
                arguments: Some("{\"filePath\":".to_string()),
            }],
            finish_reason: None,
            usage: None,
            provider_metadata: None,
        });
        capture.observe_chunk(&ChatStreamChunk {
            id: "msg_1".to_string(),
            created: 0,
            model: "model".to_string(),
            content_delta: Some(" world".to_string()),
            thinking_delta: None,
            tool_calls: vec![ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments: Some("\"README.md\"}".to_string()),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            provider_metadata: None,
        });

        let out = capture.into_value().expect("output");
        assert_eq!(out["content"], "hello world");
        assert_eq!(out["reasoning_content"], "think");
        assert_eq!(out["finish_reason"], "tool_calls");
        assert_eq!(out["tool_calls"][0]["id"], "call_1");
        assert_eq!(out["tool_calls"][0]["name"], "read_file");
        assert_eq!(
            out["tool_calls"][0]["arguments"],
            "{\"filePath\":\"README.md\"}"
        );
    }

    #[test]
    fn stream_output_capture_truncates_on_utf8_boundaries() {
        let mut capture = StreamOutputCapture::new("a好".len() - 1);
        capture.observe_chunk(&ChatStreamChunk {
            id: "msg_1".to_string(),
            created: 0,
            model: "model".to_string(),
            content_delta: Some("a好b".to_string()),
            thinking_delta: None,
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
            provider_metadata: None,
        });

        let out = capture.into_value().expect("output");
        assert_eq!(out["content"], "a");
        assert_eq!(out["truncated"], true);
    }
}
