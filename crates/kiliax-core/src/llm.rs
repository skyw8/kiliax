use std::pin::Pin;

use async_openai::{
    config::Config as OpenAIConfigTrait,
    error::OpenAIError,
    types::{ChatCompletionRequestMessage, ChatCompletionTool, CreateChatCompletionRequestArgs},
    Client,
};
use opentelemetry::KeyValue;
use reqwest_eventsource::{Event, RequestBuilderExt};
use tokio_stream::{Stream, StreamExt};
use tracing::Instrument;

use crate::config::{Config, ConfigError, ProviderKind, ResolvedModel};
use crate::protocol::{ChatRequest, ChatResponse, ChatStreamChunk, TokenUsage, ToolChoice};
use crate::telemetry;

mod anthropic;
mod api_errors;
mod byot;
mod openai_config;
mod openai_conv;
mod patches;

use anthropic::AnthropicProvider;
use api_errors::{map_api_error_response, map_eventsource_error};
use byot::{
    chat_response_from_byot, chat_stream_chunk_from_byot, ByotCreateChatCompletionResponse,
    ByotCreateChatCompletionStreamResponse,
};
use openai_config::KiliaxOpenAIConfig;
use openai_conv::{to_openai_message, to_openai_tool, to_openai_tool_choice};
use patches::{
    inject_prompt_cache_fields, inject_reasoning_content_for_tool_calls,
    is_reasoning_content_missing_error, should_inject_reasoning_content,
};

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("missing model id (provide explicitly or set `default_model` in config)")]
    MissingModel,

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    OpenAI(#[from] OpenAIError),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("HTTP {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("stream error: {0}")]
    Stream(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("invalid image: {0}")]
    InvalidImage(String),

    #[error("chat completion response has no choices")]
    NoChoices,
}

impl LlmError {
    pub fn is_context_window_exceeded(&self) -> bool {
        match self {
            LlmError::OpenAI(OpenAIError::ApiError(api_err)) => {
                is_context_window_exceeded_message(&api_err.message)
            }
            LlmError::Api { body, .. } => is_context_window_exceeded_api_body(body),
            _ => false,
        }
    }
}

fn is_context_window_exceeded_api_body(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return is_context_window_exceeded_message(body);
    };

    [
        "/error/message",
        "/message",
        "/error/code",
        "/code",
        "/error/type",
        "/type",
    ]
    .iter()
    .filter_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_str))
    .any(is_context_window_exceeded_message)
}

fn is_context_window_exceeded_message(message: &str) -> bool {
    let msg = message.to_ascii_lowercase();
    let mentions_context_input = msg.contains("context")
        || msg.contains("token")
        || msg.contains("prompt")
        || msg.contains("input")
        || msg.contains("message")
        || msg.contains("request");
    let mentions_limit = msg.contains("too long")
        || msg.contains("too large")
        || msg.contains("exceed")
        || msg.contains("over limit")
        || msg.contains("token limit")
        || msg.contains("context limit")
        || msg.contains("maximum context")
        || msg.contains("max context");

    mentions_context_input && mentions_limit
}

#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    fn route(&self) -> &ResolvedModel;
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError>;
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, LlmError>;
}

#[derive(Debug, Clone)]
pub struct LlmClient {
    provider: ProviderClient,
}

#[derive(Debug, Clone)]
enum ProviderClient {
    OpenAICompatible(OpenAICompatibleProvider),
    Anthropic(AnthropicProvider),
}

impl LlmClient {
    pub fn new(route: ResolvedModel) -> Self {
        let provider = match route.kind.clone() {
            ProviderKind::OpenAICompatible => {
                ProviderClient::OpenAICompatible(OpenAICompatibleProvider::new(route))
            }
            ProviderKind::Anthropic => ProviderClient::Anthropic(AnthropicProvider::new(route)),
        };
        Self { provider }
    }

    pub fn from_config(config: &Config, model_id: Option<&str>) -> Result<Self, LlmError> {
        let model_id = match model_id {
            Some(m) => m,
            None => config
                .default_model
                .as_deref()
                .ok_or(LlmError::MissingModel)?,
        };
        let route = config.resolve_model(model_id)?;
        Ok(Self::new(route))
    }

    pub fn with_prompt_cache_key(mut self, prompt_cache_key: Option<String>) -> Self {
        match &mut self.provider {
            ProviderClient::OpenAICompatible(provider) => {
                provider.set_prompt_cache_key(prompt_cache_key);
            }
            ProviderClient::Anthropic(_) => {}
        }
        self
    }

    pub fn route(&self) -> &ResolvedModel {
        match &self.provider {
            ProviderClient::OpenAICompatible(provider) => provider.route(),
            ProviderClient::Anthropic(provider) => provider.route(),
        }
    }

    pub async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        match &self.provider {
            ProviderClient::OpenAICompatible(provider) => provider.chat(req).await,
            ProviderClient::Anthropic(provider) => provider.chat(req).await,
        }
    }

    pub async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, LlmError> {
        match &self.provider {
            ProviderClient::OpenAICompatible(provider) => provider.chat_stream(req).await,
            ProviderClient::Anthropic(provider) => provider.chat_stream(req).await,
        }
    }
}

#[derive(Debug, Clone)]
struct OpenAICompatibleProvider {
    client: Client<KiliaxOpenAIConfig>,
    http: reqwest::Client,
    route: ResolvedModel,
    prompt_cache_key: Option<String>,
}

impl OpenAICompatibleProvider {
    pub fn new(route: ResolvedModel) -> Self {
        let cfg = KiliaxOpenAIConfig::new(&route.base_url, route.api_key.as_deref());
        let client = Client::with_config(cfg);
        let http = reqwest::Client::new();
        Self {
            client,
            http,
            route,
            prompt_cache_key: None,
        }
    }

    pub fn set_prompt_cache_key(&mut self, prompt_cache_key: Option<String>) {
        self.prompt_cache_key = prompt_cache_key
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAICompatibleProvider {
    fn route(&self) -> &ResolvedModel {
        &self.route
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let ChatRequest {
            messages: internal_messages,
            tools,
            tool_choice,
            parallel_tool_calls,
            temperature,
            max_completion_tokens,
        } = req;

        let started = std::time::Instant::now();
        let span = tracing::info_span!(
            "kiliax.llm.chat",
            llm.provider = %self.route.provider,
            llm.model = %self.route.model,
            llm.base_url = %self.route.base_url,
            llm.stream = false,
            request.messages = internal_messages.len() as u64,
            request.tools = tools.len() as u64,
        );

        telemetry::spans::set_attributes(
            &span,
            [
                KeyValue::new("langfuse.observation.type", "generation"),
                KeyValue::new("gen_ai.system", self.route.provider.clone()),
                KeyValue::new("gen_ai.request.model", self.route.model.clone()),
            ],
        );

        if telemetry::capture_enabled() {
            if let Ok(json) = serde_json::to_string(&internal_messages) {
                let captured = telemetry::capture_text(&json);
                if telemetry::capture_full() {
                    telemetry::spans::set_attribute(
                        &span,
                        "gen_ai.prompt",
                        captured.as_str().to_string(),
                    );
                }
                tracing::info!(
                    target: "kiliax_core::telemetry",
                    parent: &span,
                    event = "llm.request",
                    llm_stream = false,
                    request_len = captured.len as u64,
                    request_truncated = captured.truncated,
                    request_sha256 = %captured.sha256.as_deref().unwrap_or(""),
                    request = %captured.as_str(),
                );
            }
        }

        let res: Result<ChatResponse, LlmError> = async {
            let mut messages: Vec<ChatCompletionRequestMessage> =
                Vec::with_capacity(internal_messages.len());
            for msg in &internal_messages {
                messages.push(to_openai_message(msg).await?);
            }

            let mut builder = CreateChatCompletionRequestArgs::default();
            builder.model(&self.route.model).messages(messages);

            if !tools.is_empty() {
                let tools: Vec<ChatCompletionTool> =
                    tools.into_iter().map(to_openai_tool).collect();
                builder.tools(tools);
                if tool_choice != ToolChoice::Auto {
                    builder.tool_choice(to_openai_tool_choice(&tool_choice));
                }
            }

            if let Some(parallel_tool_calls) = parallel_tool_calls {
                builder.parallel_tool_calls(parallel_tool_calls);
            }

            if let Some(temperature) = temperature {
                builder.temperature(temperature);
            }

            if let Some(max_completion_tokens) = max_completion_tokens {
                builder.max_completion_tokens(max_completion_tokens);
            }

            let request = builder.build()?;
            let mut body = serde_json::to_value(&request).map_err(|e| {
                LlmError::OpenAI(OpenAIError::InvalidArgument(format!(
                    "failed to serialize request: {e}"
                )))
            })?;

            if should_inject_reasoning_content(&self.route) {
                inject_reasoning_content_for_tool_calls(&mut body, &internal_messages);
            }
            inject_prompt_cache_fields(&mut body, self.prompt_cache_key.as_deref());

            let cfg = self.client.config();
            let mut resp = self
                .http
                .post(cfg.url("/chat/completions"))
                .query(&cfg.query())
                .headers(cfg.headers())
                .json(&body)
                .send()
                .await
                .map_err(OpenAIError::Reqwest)?;

            let mut status = resp.status();
            if !status.is_success() {
                let err = map_api_error_response(status, resp).await;
                if is_reasoning_content_missing_error(&err) {
                    // WHY: Be resilient to mis-routed provider detection or gateway differences.
                    // If the provider explicitly complains about missing `reasoning_content`, patch and retry once.
                    inject_reasoning_content_for_tool_calls(&mut body, &internal_messages);
                    resp = self
                        .http
                        .post(cfg.url("/chat/completions"))
                        .query(&cfg.query())
                        .headers(cfg.headers())
                        .json(&body)
                        .send()
                        .await
                        .map_err(OpenAIError::Reqwest)?;
                    status = resp.status();
                    if !status.is_success() {
                        let err = map_api_error_response(status, resp).await;
                        return Err(LlmError::OpenAI(err));
                    }
                } else {
                    return Err(LlmError::OpenAI(err));
                }
            }

            let bytes = resp.bytes().await.map_err(OpenAIError::Reqwest)?;
            let parsed: ByotCreateChatCompletionResponse = serde_json::from_slice(&bytes)
                .map_err(|e| LlmError::OpenAI(OpenAIError::JSONDeserialize(e)))?;
            chat_response_from_byot(parsed)
        }
        .instrument(span.clone())
        .await;

        let latency = started.elapsed();

        match &res {
            Ok(ok) => {
                let usage = ok.usage.as_ref();
                if let Some(usage) = usage {
                    let cached = usage.cached_tokens.unwrap_or(0) as i64;
                    telemetry::spans::set_attributes(
                        &span,
                        [
                            KeyValue::new("gen_ai.usage.input_tokens", usage.prompt_tokens as i64),
                            KeyValue::new("gen_ai.usage.cached_input_tokens", cached),
                            KeyValue::new(
                                "gen_ai.usage.output_tokens",
                                usage.completion_tokens as i64,
                            ),
                        ],
                    );
                    let total_s = latency.as_secs_f64();
                    if total_s > 0.0 {
                        let output_tps = usage.completion_tokens as f64 / total_s;
                        telemetry::spans::set_attribute(&span, "kiliax.llm.output_tps", output_tps);
                        telemetry::metrics::record_llm_output_tps(
                            &self.route.provider,
                            &self.route.model,
                            false,
                            "ok",
                            output_tps,
                        );
                    }
                }
                telemetry::metrics::record_llm_call(
                    &self.route.provider,
                    &self.route.model,
                    false,
                    "ok",
                    latency,
                    usage.map(|u| u.prompt_tokens as u64),
                    usage.and_then(|u| u.cached_tokens.map(|v| v as u64)),
                    usage.map(|u| u.completion_tokens as u64),
                );

                if telemetry::capture_enabled() {
                    if let Ok(json) = serde_json::to_string(&ok.message) {
                        let captured = telemetry::capture_text(&json);
                        if telemetry::capture_full() {
                            telemetry::spans::set_attribute(
                                &span,
                                "gen_ai.completion",
                                captured.as_str().to_string(),
                            );
                        }
                        tracing::info!(
                            target: "kiliax_core::telemetry",
                            parent: &span,
                            event = "llm.response",
                            llm_stream = false,
                            finish_reason = ?ok.finish_reason,
                            response_len = captured.len as u64,
                            response_truncated = captured.truncated,
                            response_sha256 = %captured.sha256.as_deref().unwrap_or(""),
                            response = %captured.as_str(),
                        );
                    }
                }
            }
            Err(err) => {
                telemetry::metrics::record_llm_call(
                    &self.route.provider,
                    &self.route.model,
                    false,
                    "error",
                    latency,
                    None,
                    None,
                    None,
                );
                tracing::warn!(
                    target: "kiliax_core::telemetry",
                    parent: &span,
                    event = "llm.error",
                    llm_stream = false,
                    error = %err,
                );
            }
        }

        res
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, LlmError> {
        let ChatRequest {
            messages: internal_messages,
            tools,
            tool_choice,
            parallel_tool_calls,
            temperature,
            max_completion_tokens,
        } = req;

        let started = std::time::Instant::now();
        let started_wall_time = std::time::SystemTime::now();
        let span = tracing::info_span!(
            "kiliax.llm.chat_stream",
            llm.provider = %self.route.provider,
            llm.model = %self.route.model,
            llm.base_url = %self.route.base_url,
            llm.stream = true,
            request.messages = internal_messages.len() as u64,
            request.tools = tools.len() as u64,
        );

        telemetry::spans::set_attributes(
            &span,
            [
                KeyValue::new("langfuse.observation.type", "generation"),
                KeyValue::new("gen_ai.system", self.route.provider.clone()),
                KeyValue::new("gen_ai.request.model", self.route.model.clone()),
            ],
        );

        if telemetry::capture_enabled() {
            if let Ok(json) = serde_json::to_string(&internal_messages) {
                let captured = telemetry::capture_text(&json);
                if telemetry::capture_full() {
                    telemetry::spans::set_attribute(
                        &span,
                        "gen_ai.prompt",
                        captured.as_str().to_string(),
                    );
                }
                tracing::info!(
                    target: "kiliax_core::telemetry",
                    parent: &span,
                    event = "llm.request",
                    llm_stream = true,
                    request_len = captured.len as u64,
                    request_truncated = captured.truncated,
                    request_sha256 = %captured.sha256.as_deref().unwrap_or(""),
                    request = %captured.as_str(),
                );
            }
        }

        let provider = self.route.provider.clone();
        let model = self.route.model.clone();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamChunk, LlmError>>();

        let setup: Result<(), LlmError> = async {
            let mut messages: Vec<ChatCompletionRequestMessage> =
                Vec::with_capacity(internal_messages.len());
            for msg in &internal_messages {
                messages.push(to_openai_message(msg).await?);
            }

            let mut builder = CreateChatCompletionRequestArgs::default();
            builder.model(&self.route.model).messages(messages);

            if !tools.is_empty() {
                let tools: Vec<ChatCompletionTool> =
                    tools.into_iter().map(to_openai_tool).collect();
                builder.tools(tools);
                if tool_choice != ToolChoice::Auto {
                    builder.tool_choice(to_openai_tool_choice(&tool_choice));
                }
            }

            if let Some(parallel_tool_calls) = parallel_tool_calls {
                builder.parallel_tool_calls(parallel_tool_calls);
            }

            if let Some(temperature) = temperature {
                builder.temperature(temperature);
            }

            if let Some(max_completion_tokens) = max_completion_tokens {
                builder.max_completion_tokens(max_completion_tokens);
            }

            let mut request = builder.build()?;
            request.stream = Some(true);
            let mut body = serde_json::to_value(&request).map_err(|e| {
                LlmError::OpenAI(OpenAIError::InvalidArgument(format!(
                    "failed to serialize request: {e}"
                )))
            })?;

            if should_inject_reasoning_content(&self.route) {
                inject_reasoning_content_for_tool_calls(&mut body, &internal_messages);
            }
            inject_prompt_cache_fields(&mut body, self.prompt_cache_key.as_deref());
            if self.route.provider.eq_ignore_ascii_case("openai") {
                if let Some(obj) = body.as_object_mut() {
                    let stream_options = obj
                        .entry("stream_options".to_string())
                        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                    if let Some(opts) = stream_options.as_object_mut() {
                        opts.insert("include_usage".to_string(), serde_json::Value::Bool(true));
                    }
                }
            }

            let cfg = self.client.config().clone();
            let http = self.http.clone();
            let internal_messages_for_patch = internal_messages.clone();
            let mut event_source = http
                .post(cfg.url("/chat/completions"))
                .query(&cfg.query())
                .headers(cfg.headers())
                .json(&body)
                .eventsource()
                .map_err(|e| OpenAIError::StreamError(e.to_string()))?;

            let span_for_task = span.clone();
            let provider = provider.clone();
            let model = model.clone();
            let capture_completion = telemetry::capture_full();
            let max_completion_bytes = telemetry::capture_max_bytes();
            tokio::spawn(
                async move {
                    let mut body = body;
                    let mut retried_reasoning_patch = false;
                    let mut forwarded_any = false;
                    let mut last_usage: Option<TokenUsage> = None;
                    let mut outcome = "ok";
                    let mut completion = String::new();
                    let mut completion_truncated = false;
                    let mut ttft: Option<std::time::Duration> = None;
                    let mut completion_start_time: Option<String> = None;

                    loop {
                        let ev = event_source.next().await;
                        let Some(ev) = ev else {
                            break;
                        };

                        match ev {
                            Ok(Event::Open) => continue,
                            Ok(Event::Message(message)) => {
                                let data = message.data.trim();
                                if data.is_empty() || message.event == "keepalive" {
                                    continue;
                                }
                                if data == "[DONE]" {
                                    break;
                                }

                                let response = match serde_json::from_str::<
                                    ByotCreateChatCompletionStreamResponse,
                                >(data)
                                {
                                    Ok(resp) => {
                                        let chunk = chat_stream_chunk_from_byot(resp);
                                        if let Some(usage) = chunk.usage {
                                            last_usage = Some(usage);
                                        }
                                        if ttft.is_none()
                                            && chunk
                                                .content_delta
                                                .as_deref()
                                                .is_some_and(|d| !d.is_empty())
                                        {
                                            let seen = started.elapsed();
                                            ttft = Some(seen);
                                            if let Some(wall) = started_wall_time.checked_add(seen)
                                            {
                                                completion_start_time =
                                                    format_system_time_rfc3339(wall);
                                            }
                                        }
                                        if capture_completion && !completion_truncated {
                                            if let Some(delta) = chunk.content_delta.as_deref() {
                                                if max_completion_bytes == 0 {
                                                    completion_truncated = true;
                                                } else if completion.len() + delta.len()
                                                    <= max_completion_bytes
                                                {
                                                    completion.push_str(delta);
                                                } else {
                                                    let remaining = max_completion_bytes
                                                        .saturating_sub(completion.len());
                                                    if remaining > 0 {
                                                        let mut end = remaining;
                                                        while end > 0
                                                            && !delta.is_char_boundary(end)
                                                        {
                                                            end -= 1;
                                                        }
                                                        completion.push_str(&delta[..end]);
                                                    }
                                                    completion_truncated = true;
                                                }
                                            }
                                        }
                                        Ok(chunk)
                                    }
                                    Err(err) => {
                                        Err(LlmError::OpenAI(OpenAIError::JSONDeserialize(err)))
                                    }
                                };

                                if response.is_ok() {
                                    forwarded_any = true;
                                }

                                if tx.send(response).is_err() {
                                    outcome = "cancelled";
                                    break;
                                }
                            }
                            Err(reqwest_eventsource::Error::StreamEnded) => break,
                            Err(err) => {
                                let mapped = map_eventsource_error(err).await;
                                if !retried_reasoning_patch
                                    && !forwarded_any
                                    && is_reasoning_content_missing_error(&mapped)
                                {
                                    // WHY: Some providers validate the prompt before streaming any chunks.
                                    // If they reject missing tool-call `reasoning_content`, patch and reconnect once.
                                    inject_reasoning_content_for_tool_calls(
                                        &mut body,
                                        &internal_messages_for_patch,
                                    );
                                    event_source.close();
                                    match http
                                        .post(cfg.url("/chat/completions"))
                                        .query(&cfg.query())
                                        .headers(cfg.headers())
                                        .json(&body)
                                        .eventsource()
                                    {
                                        Ok(es) => {
                                            event_source = es;
                                            retried_reasoning_patch = true;
                                            continue;
                                        }
                                        Err(err) => {
                                            let _ = tx.send(Err(LlmError::OpenAI(
                                                OpenAIError::StreamError(err.to_string()),
                                            )));
                                            outcome = "error";
                                            break;
                                        }
                                    }
                                }

                                let _ = tx.send(Err(LlmError::OpenAI(mapped)));
                                outcome = "error";
                                break;
                            }
                        }
                    }
                    event_source.close();

                    let latency = started.elapsed();
                    telemetry::metrics::record_llm_call(
                        &provider,
                        &model,
                        true,
                        outcome,
                        latency,
                        last_usage.as_ref().map(|u| u.prompt_tokens as u64),
                        last_usage
                            .as_ref()
                            .and_then(|u| u.cached_tokens.map(|v| v as u64)),
                        last_usage.as_ref().map(|u| u.completion_tokens as u64),
                    );

                    let current_span = tracing::Span::current();
                    if let Some(ttft) = ttft {
                        telemetry::metrics::record_llm_ttft(&provider, &model, true, outcome, ttft);
                        telemetry::spans::set_attribute(
                            &current_span,
                            "kiliax.llm.ttft_ms",
                            ttft.as_millis() as i64,
                        );
                        if let Some(ts) = completion_start_time {
                            telemetry::spans::set_attribute(
                                &current_span,
                                "langfuse.observation.completion_start_time",
                                ts,
                            );
                        }
                    }
                    if capture_completion && !completion.is_empty() {
                        telemetry::spans::set_attribute(
                            &current_span,
                            "gen_ai.completion",
                            completion,
                        );
                    }
                    if let Some(usage) = last_usage.as_ref() {
                        let cached = usage.cached_tokens.unwrap_or(0) as i64;
                        telemetry::spans::set_attributes(
                            &current_span,
                            [
                                KeyValue::new(
                                    "gen_ai.usage.input_tokens",
                                    usage.prompt_tokens as i64,
                                ),
                                KeyValue::new("gen_ai.usage.cached_input_tokens", cached),
                                KeyValue::new(
                                    "gen_ai.usage.output_tokens",
                                    usage.completion_tokens as i64,
                                ),
                            ],
                        );
                        let total_s = latency.as_secs_f64();
                        if total_s > 0.0 {
                            let output_tps = usage.completion_tokens as f64 / total_s;
                            telemetry::metrics::record_llm_output_tps(
                                &provider, &model, true, outcome, output_tps,
                            );
                            telemetry::spans::set_attribute(
                                &current_span,
                                "kiliax.llm.output_tps",
                                output_tps,
                            );
                        }
                        if let Some(ttft) = ttft {
                            let gen = latency.saturating_sub(ttft);
                            let gen_s = gen.as_secs_f64();
                            if gen_s > 0.0 {
                                let output_tps = usage.completion_tokens as f64 / gen_s;
                                telemetry::metrics::record_llm_output_tps_after_ttft(
                                    &provider, &model, true, outcome, output_tps,
                                );
                                telemetry::spans::set_attribute(
                                    &current_span,
                                    "kiliax.llm.output_tps_after_ttft",
                                    output_tps,
                                );
                            }
                        }
                    }

                    if outcome != "ok" {
                        tracing::warn!(
                            target: "kiliax_core::telemetry",
                            event = "llm.stream_end",
                            outcome = outcome,
                        );
                    }
                }
                .instrument(span_for_task),
            );

            Ok(())
        }
        .instrument(span.clone())
        .await;

        if let Err(err) = setup {
            telemetry::metrics::record_llm_call(
                &provider,
                &model,
                true,
                "error",
                started.elapsed(),
                None,
                None,
                None,
            );
            tracing::warn!(
                target: "kiliax_core::telemetry",
                parent: &span,
                event = "llm.error",
                llm_stream = true,
                error = %err,
            );
            return Err(err);
        }

        Ok(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ))
    }
}

fn format_system_time_rfc3339(ts: std::time::SystemTime) -> Option<String> {
    use time::format_description::well_known::Rfc3339;

    let unix = ts.duration_since(std::time::UNIX_EPOCH).ok()?;
    let nanos = i128::try_from(unix.as_nanos()).ok()?;
    let dt = time::OffsetDateTime::from_unix_timestamp_nanos(nanos).ok()?;
    dt.format(&Rfc3339).ok()
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatStreamChunk, LlmError>> + Send>>;

#[cfg(test)]
mod tests {
    use crate::protocol::ToolCall;
    use crate::protocol::{Message, ToolDefinition};
    use crate::protocol::{UserContentPart, UserMessageContent};
    use async_openai::types::{
        ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart,
    };

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn tool_message_roundtrip_builds_openai_message() {
        let msg = Message::Tool {
            tool_call_id: "call_123".to_string(),
            content: "{\"ok\":true}".to_string(),
        };
        let openai = to_openai_message(&msg).await.unwrap();
        let ChatCompletionRequestMessage::Tool(t) = openai else {
            panic!("expected tool message");
        };
        assert_eq!(t.tool_call_id, "call_123");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn assistant_message_includes_tool_calls() {
        let msg = Message::Assistant {
            content: None,
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            }],
            usage: None,
        };
        let openai = to_openai_message(&msg).await.unwrap();
        let ChatCompletionRequestMessage::Assistant(a) = openai else {
            panic!("expected assistant message");
        };
        assert!(a.content.is_none());
        assert_eq!(a.tool_calls.as_ref().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn image_only_user_message_includes_non_empty_text_part() {
        let content = UserMessageContent::Parts(vec![UserContentPart::Image {
            path: "data:image/png;base64,AA==".to_string(),
            detail: None,
        }]);
        let openai = openai_conv::to_openai_user_content(&content).await.unwrap();
        let ChatCompletionRequestUserMessageContent::Array(parts) = openai else {
            panic!("expected user content array");
        };
        let ChatCompletionRequestUserMessageContentPart::Text(t) = parts.first().unwrap() else {
            panic!("expected first part to be text");
        };
        assert!(!t.text.trim().is_empty());
    }

    #[test]
    fn chat_stream_maps_reasoning_content_to_thinking_delta() {
        let raw = serde_json::json!({
            "id": "chat_1",
            "created": 0,
            "model": "m",
            "choices": [
                {
                    "index": 0,
                    "delta": {
                        "reasoning_content": "step 1\nstep 2\n",
                        "content": "final"
                    },
                    "finish_reason": null
                }
            ]
        });

        let resp: ByotCreateChatCompletionStreamResponse = serde_json::from_value(raw).unwrap();
        let chunk = chat_stream_chunk_from_byot(resp);
        assert_eq!(chunk.thinking_delta.as_deref(), Some("step 1\nstep 2\n"));
        assert_eq!(chunk.content_delta.as_deref(), Some("final"));
    }

    #[test]
    fn openai_tool_conversion_preserves_strict() {
        let tool = ToolDefinition {
            name: "t".to_string(),
            description: None,
            parameters: Some(serde_json::json!({"type":"object"})),
            strict: Some(true),
        };
        let openai = to_openai_tool(tool);
        assert_eq!(openai.function.strict, Some(true));
    }

    #[test]
    fn inject_prompt_cache_fields_noop_when_missing_key() {
        let mut body = serde_json::json!({"model":"m"});
        inject_prompt_cache_fields(&mut body, None);
        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn inject_prompt_cache_fields_sets_key() {
        let mut body = serde_json::json!({"model":"m"});
        inject_prompt_cache_fields(&mut body, Some("k"));
        assert_eq!(body["prompt_cache_key"], serde_json::json!("k"));
    }

    #[test]
    fn should_inject_reasoning_content_matches_kimi_model_even_behind_proxy() {
        let route = ResolvedModel {
            provider: "proxy".to_string(),
            kind: ProviderKind::OpenAICompatible,
            model: "kimi-k2.5".to_string(),
            base_url: "http://127.0.0.1:8000/v1".to_string(),
            api_key: None,
        };
        assert!(should_inject_reasoning_content(&route));
    }

    #[test]
    fn inject_reasoning_content_for_tool_calls_inserts_empty_string() {
        let messages = vec![
            Message::User {
                content: UserMessageContent::Text("hi".to_string()),
            },
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "t".to_string(),
                    arguments: "{}".to_string(),
                }],
                usage: None,
            },
            Message::Tool {
                tool_call_id: "call_1".to_string(),
                content: "ok".to_string(),
            },
        ];

        let mut body = serde_json::json!({
            "messages": [
                {"role":"user","content":"hi"},
                {
                    "role":"assistant",
                    "tool_calls":[{"id":"call_1","type":"function","function":{"name":"t","arguments":"{}"}}]
                },
                {"role":"tool","tool_call_id":"call_1","content":"ok"}
            ]
        });

        inject_reasoning_content_for_tool_calls(&mut body, &messages);
        assert_eq!(
            body["messages"][1]["reasoning_content"],
            serde_json::json!(" ")
        );
    }

    #[test]
    fn inject_reasoning_content_for_tool_calls_does_not_override_existing_value() {
        let messages = vec![Message::Assistant {
            content: None,
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "t".to_string(),
                arguments: "{}".to_string(),
            }],
            usage: None,
        }];

        let mut body = serde_json::json!({
            "messages": [
                {
                    "role":"assistant",
                    "reasoning_content":"keep",
                    "tool_calls":[{"id":"call_1","type":"function","function":{"name":"t","arguments":"{}"}}]
                }
            ]
        });

        inject_reasoning_content_for_tool_calls(&mut body, &messages);
        assert_eq!(
            body["messages"][0]["reasoning_content"],
            serde_json::json!("keep")
        );
    }

    #[test]
    fn inject_reasoning_content_for_tool_calls_overrides_empty_string() {
        let messages = vec![Message::Assistant {
            content: None,
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "t".to_string(),
                arguments: "{}".to_string(),
            }],
            usage: None,
        }];

        let mut body = serde_json::json!({
            "messages": [
                {
                    "role":"assistant",
                    "reasoning_content":"",
                    "tool_calls":[{"id":"call_1","type":"function","function":{"name":"t","arguments":"{}"}}]
                }
            ]
        });

        inject_reasoning_content_for_tool_calls(&mut body, &messages);
        assert_eq!(
            body["messages"][0]["reasoning_content"],
            serde_json::json!(" ")
        );
    }

    #[test]
    fn inject_reasoning_content_for_tool_calls_patches_body_even_if_indices_drift() {
        let messages = vec![Message::User {
            content: UserMessageContent::Text("hi".to_string()),
        }];

        let mut body = serde_json::json!({
            "messages": [
                {
                    "role":"assistant",
                    "tool_calls":[{"id":"call_1","type":"function","function":{"name":"t","arguments":"{}"}}]
                }
            ]
        });

        inject_reasoning_content_for_tool_calls(&mut body, &messages);
        assert_eq!(
            body["messages"][0]["reasoning_content"],
            serde_json::json!(" ")
        );
    }

    #[test]
    fn detects_openai_context_window_errors() {
        let err = LlmError::OpenAI(OpenAIError::ApiError(async_openai::error::ApiError {
            message: "This model's maximum context length is 128000 tokens.".to_string(),
            r#type: None,
            param: None,
            code: None,
        }));

        assert!(err.is_context_window_exceeded());
    }

    #[test]
    fn detects_anthropic_prompt_too_long_errors() {
        let err = LlmError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            body: r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 200001 tokens > 200000 maximum"}}"#.to_string(),
        };

        assert!(err.is_context_window_exceeded());
    }

    #[test]
    fn ignores_unrelated_api_errors() {
        let err = LlmError::Api {
            status: reqwest::StatusCode::UNAUTHORIZED,
            body: r#"{"error":{"message":"invalid api key"}}"#.to_string(),
        };

        assert!(!err.is_context_window_exceeded());
    }
}
