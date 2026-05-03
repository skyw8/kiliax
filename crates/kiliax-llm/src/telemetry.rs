use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use opentelemetry::{Key, KeyValue, Value};

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

    fn record_llm_call(
        &self,
        _provider: &str,
        _model: &str,
        _stream: bool,
        _outcome: &str,
        _latency: Duration,
        _prompt_tokens: Option<u64>,
        _cached_tokens: Option<u64>,
        _completion_tokens: Option<u64>,
    ) {
    }

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
    with_hook(|hook| hook.capture_text(raw)).unwrap_or_else(|| CapturedText {
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
        V: Into<Value>,
    {
        set_attributes(span, [KeyValue::new(key, value)]);
    }
}

pub mod metrics {
    use super::*;

    pub fn record_llm_call(
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        latency: Duration,
        prompt_tokens: Option<u64>,
        cached_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    ) {
        let _ = with_hook(|hook| {
            hook.record_llm_call(
                provider,
                model,
                stream,
                outcome,
                latency,
                prompt_tokens,
                cached_tokens,
                completion_tokens,
            )
        });
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
