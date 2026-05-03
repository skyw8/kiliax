use std::sync::Arc;

pub use kiliax_llm::*;

use crate::config::{Config, ConfigError};

#[derive(Debug, thiserror::Error)]
pub enum LlmConfigError {
    #[error("missing model id (provide explicitly or set `default_model` in config)")]
    MissingModel,

    #[error(transparent)]
    Config(#[from] ConfigError),
}

pub fn client_from_config(
    config: &Config,
    model_id: Option<&str>,
) -> Result<kiliax_llm::LlmClient, LlmConfigError> {
    install_llm_telemetry();
    let model_id = match model_id {
        Some(m) => m,
        None => config
            .default_model
            .as_deref()
            .ok_or(LlmConfigError::MissingModel)?,
    };
    let route = config.resolve_model(model_id)?;
    Ok(kiliax_llm::LlmClient::new(route))
}

pub fn install_llm_telemetry() {
    kiliax_llm::telemetry::set_hook(Arc::new(CoreLlmTelemetry));
}

struct CoreLlmTelemetry;

impl kiliax_llm::telemetry::LlmTelemetry for CoreLlmTelemetry {
    fn capture_enabled(&self) -> bool {
        crate::telemetry::capture_enabled()
    }

    fn capture_full(&self) -> bool {
        crate::telemetry::capture_full()
    }

    fn capture_max_bytes(&self) -> usize {
        crate::telemetry::capture_max_bytes()
    }

    fn capture_text(&self, raw: &str) -> kiliax_llm::telemetry::CapturedText {
        let captured = crate::telemetry::capture_text(raw);
        kiliax_llm::telemetry::CapturedText {
            len: captured.len,
            truncated: captured.truncated,
            sha256: captured.sha256,
            text: captured.text,
        }
    }

    fn set_span_attributes(&self, span: &tracing::Span, attributes: Vec<opentelemetry::KeyValue>) {
        crate::telemetry::spans::set_attributes(span, attributes);
    }

    fn record_llm_call(
        &self,
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        latency: std::time::Duration,
        prompt_tokens: Option<u64>,
        cached_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    ) {
        crate::telemetry::metrics::record_llm_call(
            provider,
            model,
            stream,
            outcome,
            latency,
            prompt_tokens,
            cached_tokens,
            completion_tokens,
        );
    }

    fn record_llm_ttft(
        &self,
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        ttft: std::time::Duration,
    ) {
        crate::telemetry::metrics::record_llm_ttft(provider, model, stream, outcome, ttft);
    }

    fn record_llm_output_tps(
        &self,
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        output_tps: f64,
    ) {
        crate::telemetry::metrics::record_llm_output_tps(
            provider, model, stream, outcome, output_tps,
        );
    }

    fn record_llm_output_tps_after_ttft(
        &self,
        provider: &str,
        model: &str,
        stream: bool,
        outcome: &str,
        output_tps: f64,
    ) {
        crate::telemetry::metrics::record_llm_output_tps_after_ttft(
            provider, model, stream, outcome, output_tps,
        );
    }
}
