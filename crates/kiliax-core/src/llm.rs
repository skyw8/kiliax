use std::pin::Pin;

use async_openai::{
    config::Config as OpenAIConfigTrait,
    error::OpenAIError,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionNamedToolChoice,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestDeveloperMessage, ChatCompletionRequestDeveloperMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestMessageContentPartImage,
        ChatCompletionRequestMessageContentPartText, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessage,
        ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart,
        ChatCompletionTool, ChatCompletionToolChoiceOption, ChatCompletionToolType,
        CompletionUsage, CreateChatCompletionRequestArgs, FunctionCall, FunctionName,
        FunctionObject, ImageDetail, ImageUrl,
    },
    Client,
};
use base64::Engine as _;
use opentelemetry::KeyValue;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use reqwest_eventsource::{Event, RequestBuilderExt};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tokio_stream::{Stream, StreamExt};
use tracing::Instrument;

use crate::config::{Config, ConfigError, ResolvedModel};
use crate::protocol::{
    ChatRequest, ChatResponse, ChatStreamChunk, Message, ToolChoice, ToolDefinition,
    UserContentPart, UserMessageContent,
};
use crate::telemetry;

mod byot;
mod patches;

use byot::{
    chat_response_from_byot, chat_stream_chunk_from_byot, ByotCreateChatCompletionResponse,
    ByotCreateChatCompletionStreamResponse,
};
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
    Io(#[from] std::io::Error),

    #[error("invalid image: {0}")]
    InvalidImage(String),

    #[error("chat completion response has no choices")]
    NoChoices,
}

#[derive(Debug, Clone)]
pub struct LlmClient {
    client: Client<KiliaxOpenAIConfig>,
    http: reqwest::Client,
    route: ResolvedModel,
    prompt_cache_key: Option<String>,
}

impl LlmClient {
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
        self.prompt_cache_key = prompt_cache_key
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self
    }

    pub fn route(&self) -> &ResolvedModel {
        &self.route
    }

    pub async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
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
                    let cached = usage
                        .prompt_tokens_details
                        .as_ref()
                        .and_then(|d| d.cached_tokens)
                        .unwrap_or(0) as i64;
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
                    }
                }
                telemetry::metrics::record_llm_call(
                    &self.route.provider,
                    &self.route.model,
                    false,
                    "ok",
                    latency,
                    usage.map(|u| u.prompt_tokens as u64),
                    usage.and_then(|u| {
                        u.prompt_tokens_details
                            .as_ref()
                            .and_then(|d| d.cached_tokens)
                            .map(|v| v as u64)
                    }),
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

    pub async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, LlmError> {
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
                    let mut last_usage: Option<CompletionUsage> = None;
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
                                        if let Some(usage) = chunk.usage.clone() {
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
                        last_usage.as_ref().and_then(|u| {
                            u.prompt_tokens_details
                                .as_ref()
                                .and_then(|d| d.cached_tokens)
                                .map(|v| v as u64)
                        }),
                        last_usage.as_ref().map(|u| u.completion_tokens as u64),
                    );

                    let current_span = tracing::Span::current();
                    if let Some(ttft) = ttft {
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
                        let cached = usage
                            .prompt_tokens_details
                            .as_ref()
                            .and_then(|d| d.cached_tokens)
                            .unwrap_or(0) as i64;
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

#[derive(Debug, Clone)]
struct KiliaxOpenAIConfig {
    api_base: String,
    api_key: SecretString,
    send_auth: bool,
}

impl KiliaxOpenAIConfig {
    fn new(api_base: &str, api_key: Option<&str>) -> Self {
        let api_base = normalize_api_base(api_base);
        let send_auth = api_key.is_some();
        let api_key = SecretString::from(api_key.unwrap_or_default().to_string());
        Self {
            api_base,
            api_key,
            send_auth,
        }
    }
}

impl OpenAIConfigTrait for KiliaxOpenAIConfig {
    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if self.send_auth {
            headers.insert(
                AUTHORIZATION,
                format!("Bearer {}", self.api_key.expose_secret())
                    .as_str()
                    .parse()
                    .unwrap(),
            );
        }
        headers
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api_base, path)
    }

    fn query(&self) -> Vec<(&str, &str)> {
        vec![]
    }

    fn api_base(&self) -> &str {
        &self.api_base
    }

    fn api_key(&self) -> &SecretString {
        &self.api_key
    }
}

fn normalize_api_base(api_base: &str) -> String {
    api_base.trim().trim_end_matches('/').to_string()
}

async fn map_eventsource_error(err: reqwest_eventsource::Error) -> OpenAIError {
    match err {
        reqwest_eventsource::Error::Transport(err) => OpenAIError::Reqwest(err),
        reqwest_eventsource::Error::InvalidStatusCode(status, response) => {
            map_api_error_response(status, response).await
        }
        reqwest_eventsource::Error::InvalidContentType(_ct, response) => {
            map_api_error_response(response.status(), response).await
        }
        reqwest_eventsource::Error::StreamEnded => {
            OpenAIError::StreamError("Stream ended".to_string())
        }
        other => OpenAIError::StreamError(other.to_string()),
    }
}

async fn map_api_error_response(
    status: reqwest::StatusCode,
    response: reqwest::Response,
) -> OpenAIError {
    #[derive(Debug, Deserialize)]
    struct ErrorWrapper {
        error: async_openai::error::ApiError,
    }

    const MAX_BODY_BYTES: usize = 16 * 1024;
    let bytes = match response.bytes().await {
        Ok(b) => {
            if b.len() > MAX_BODY_BYTES {
                b.slice(..MAX_BODY_BYTES)
            } else {
                b
            }
        }
        Err(err) => return OpenAIError::Reqwest(err),
    };

    if let Ok(mut wrapped) = serde_json::from_slice::<ErrorWrapper>(&bytes) {
        wrapped.error.message = format!("HTTP {status}: {}", wrapped.error.message);
        return OpenAIError::ApiError(wrapped.error);
    }

    let body = String::from_utf8_lossy(&bytes).trim().to_string();
    let message = if body.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {body}")
    };
    OpenAIError::ApiError(async_openai::error::ApiError {
        message,
        r#type: None,
        param: None,
        code: None,
    })
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatStreamChunk, LlmError>> + Send>>;

fn to_openai_tool(tool: ToolDefinition) -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
            strict: tool.strict,
        },
    }
}

fn to_openai_tool_choice(choice: &ToolChoice) -> ChatCompletionToolChoiceOption {
    match choice {
        ToolChoice::None => ChatCompletionToolChoiceOption::None,
        ToolChoice::Auto => ChatCompletionToolChoiceOption::Auto,
        ToolChoice::Required => ChatCompletionToolChoiceOption::Required,
        ToolChoice::Named { name } => {
            ChatCompletionToolChoiceOption::Named(ChatCompletionNamedToolChoice {
                r#type: ChatCompletionToolType::Function,
                function: FunctionName { name: name.clone() },
            })
        }
    }
}

async fn to_openai_message(msg: &Message) -> Result<ChatCompletionRequestMessage, LlmError> {
    Ok(match msg {
        Message::Developer { content } => {
            ChatCompletionRequestMessage::Developer(ChatCompletionRequestDeveloperMessage {
                content: ChatCompletionRequestDeveloperMessageContent::Text(content.clone()),
                name: None,
            })
        }
        Message::System { content } => {
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(content.clone()),
                name: None,
            })
        }
        Message::User { content } => {
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: to_openai_user_content(content).await?,
                name: None,
            })
        }
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let tool_calls = if tool_calls.is_empty() {
                None
            } else {
                Some(
                    tool_calls
                        .iter()
                        .map(|c| ChatCompletionMessageToolCall {
                            id: c.id.clone(),
                            r#type: ChatCompletionToolType::Function,
                            function: FunctionCall {
                                name: c.name.clone(),
                                arguments: c.arguments.clone(),
                            },
                        })
                        .collect(),
                )
            };
            ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                content: content
                    .as_ref()
                    .map(|c| ChatCompletionRequestAssistantMessageContent::Text(c.clone())),
                tool_calls,
                ..Default::default()
            })
        }
        Message::Tool {
            tool_call_id,
            content,
        } => ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
            content: ChatCompletionRequestToolMessageContent::Text(content.clone()),
            tool_call_id: tool_call_id.clone(),
        }),
    })
}

async fn to_openai_user_content(
    content: &UserMessageContent,
) -> Result<ChatCompletionRequestUserMessageContent, LlmError> {
    match content {
        UserMessageContent::Text(text) => {
            if text.trim().is_empty() {
                return Err(LlmError::OpenAI(OpenAIError::InvalidArgument(
                    "user message text must not be empty".to_string(),
                )));
            }
            Ok(ChatCompletionRequestUserMessageContent::Text(text.clone()))
        }
        UserMessageContent::Parts(parts) => {
            let mut out: Vec<ChatCompletionRequestUserMessageContentPart> = Vec::new();
            for part in parts {
                match part {
                    UserContentPart::Text { text } => {
                        if text.trim().is_empty() {
                            continue;
                        }
                        out.push(ChatCompletionRequestUserMessageContentPart::Text(
                            ChatCompletionRequestMessageContentPartText { text: text.clone() },
                        ))
                    }
                    UserContentPart::Image { path, detail } => {
                        let image_url = image_url_from_path(path, detail.clone()).await?;
                        out.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
                            ChatCompletionRequestMessageContentPartImage { image_url },
                        ));
                    }
                }
            }
            if out.is_empty() {
                return Err(LlmError::OpenAI(OpenAIError::InvalidArgument(
                    "user message content must not be empty".to_string(),
                )));
            }
            if !out
                .iter()
                .any(|p| matches!(p, ChatCompletionRequestUserMessageContentPart::Text(_)))
            {
                out.insert(
                    0,
                    ChatCompletionRequestUserMessageContentPart::Text(
                        ChatCompletionRequestMessageContentPartText {
                            text: ".".to_string(),
                        },
                    ),
                );
            }
            Ok(ChatCompletionRequestUserMessageContent::Array(out))
        }
    }
}

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

async fn image_url_from_path(
    path: &str,
    detail: Option<ImageDetail>,
) -> Result<ImageUrl, LlmError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(LlmError::InvalidImage("path must not be empty".to_string()));
    }

    if path.starts_with("http://") || path.starts_with("https://") || path.starts_with("data:") {
        return Ok(ImageUrl {
            url: path.to_string(),
            detail,
        });
    }

    let fs_path = std::path::Path::new(path);
    let meta = tokio::fs::metadata(fs_path).await?;
    if !meta.is_file() {
        return Err(LlmError::InvalidImage(format!(
            "path `{}` is not a file",
            fs_path.display()
        )));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(LlmError::InvalidImage(format!(
            "image `{}` is too large ({} bytes > {} bytes)",
            fs_path.display(),
            meta.len(),
            MAX_IMAGE_BYTES
        )));
    }

    let mime_type = guess_image_mime_type(fs_path).ok_or_else(|| {
        LlmError::InvalidImage(format!(
            "unsupported image extension for `{}`",
            fs_path.display()
        ))
    })?;

    let bytes = tokio::fs::read(fs_path).await?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ImageUrl {
        url: format!("data:{mime_type};base64,{b64}"),
        detail,
    })
}

fn guess_image_mime_type(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.trim().to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        "avif" => Some("image/avif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::ToolCall;

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
        let openai = to_openai_user_content(&content).await.unwrap();
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
}
