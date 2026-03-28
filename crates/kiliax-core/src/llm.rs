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
        CompletionUsage, CreateChatCompletionRequestArgs, FinishReason, FunctionCall, FunctionName,
        FunctionObject, ImageDetail, ImageUrl,
    },
    Client,
};
use base64::Engine as _;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use reqwest_eventsource::{Event, RequestBuilderExt};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};
use tracing::Instrument;

use crate::config::{Config, ConfigError, ResolvedModel};
use crate::telemetry;

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
    route: ResolvedModel,
}

impl LlmClient {
    pub fn new(route: ResolvedModel) -> Self {
        let cfg = KiliaxOpenAIConfig::new(&route.base_url, route.api_key.as_deref());
        let client = Client::with_config(cfg);
        Self { client, route }
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

        if telemetry::capture_enabled() {
            if let Ok(json) = serde_json::to_string(&internal_messages) {
                let captured = telemetry::capture_text(&json);
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

            let cfg = self.client.config();
            let http = reqwest::Client::new();
            let resp = http
                .post(cfg.url("/chat/completions"))
                .query(&cfg.query())
                .headers(cfg.headers())
                .json(&body)
                .send()
                .await
                .map_err(OpenAIError::Reqwest)?;

            let status = resp.status();
            if !status.is_success() {
                let err = map_api_error_response(status, resp).await;
                return Err(LlmError::OpenAI(err));
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
                telemetry::metrics::record_llm_call(
                    &self.route.provider,
                    &self.route.model,
                    false,
                    "ok",
                    latency,
                    usage.map(|u| u.prompt_tokens as u64),
                    usage.map(|u| u.completion_tokens as u64),
                );

                if telemetry::capture_enabled() {
                    if let Ok(json) = serde_json::to_string(&ok.message) {
                        let captured = telemetry::capture_text(&json);
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
        let span = tracing::info_span!(
            "kiliax.llm.chat_stream",
            llm.provider = %self.route.provider,
            llm.model = %self.route.model,
            llm.base_url = %self.route.base_url,
            llm.stream = true,
            request.messages = internal_messages.len() as u64,
            request.tools = tools.len() as u64,
        );

        if telemetry::capture_enabled() {
            if let Ok(json) = serde_json::to_string(&internal_messages) {
                let captured = telemetry::capture_text(&json);
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

            let cfg = self.client.config();
            let http = reqwest::Client::new();
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
            tokio::spawn(
                async move {
                    let mut last_usage: Option<CompletionUsage> = None;
                    let mut outcome = "ok";

                    while let Some(ev) = event_source.next().await {
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
                                        Ok(chunk)
                                    }
                                    Err(err) => Err(LlmError::OpenAI(OpenAIError::JSONDeserialize(
                                        err,
                                    ))),
                                };

                                if tx.send(response).is_err() {
                                    outcome = "cancelled";
                                    break;
                                }
                            }
                            Err(reqwest_eventsource::Error::StreamEnded) => break,
                            Err(err) => {
                                let mapped = map_eventsource_error(err).await;
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
                        last_usage.as_ref().map(|u| u.completion_tokens as u64),
                    );

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

        Ok(Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(
            rx,
        )))
    }
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

fn should_inject_reasoning_content(route: &ResolvedModel) -> bool {
    let provider = route.provider.to_ascii_lowercase();
    let base_url = route.base_url.to_ascii_lowercase();
    provider.contains("moonshot") || base_url.contains("moonshot")
}

fn inject_reasoning_content_for_tool_calls(body: &mut serde_json::Value, messages: &[Message]) {
    let Some(body_messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return;
    };

    for (idx, msg) in messages.iter().enumerate() {
        let Message::Assistant {
            reasoning_content,
            tool_calls,
            ..
        } = msg
        else {
            continue;
        };

        if tool_calls.is_empty() {
            continue;
        }

        let Some(obj) = body_messages.get_mut(idx).and_then(|v| v.as_object_mut()) else {
            continue;
        };

        obj.insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(reasoning_content.clone().unwrap_or_default()),
        );
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Tool arguments as JSON text (the model may return invalid JSON).
    pub arguments: String,
}

impl ToolCall {
    pub fn arguments_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    None,
    Auto,
    Required,
    Named { name: String },
}

impl Default for ToolChoice {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum UserMessageContent {
    Text(String),
    Parts(Vec<UserContentPart>),
}

impl UserMessageContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub fn first_text(&self) -> Option<&str> {
        match self {
            UserMessageContent::Text(text) => Some(text.as_str()),
            UserMessageContent::Parts(parts) => parts.iter().find_map(|p| match p {
                UserContentPart::Text { text } => Some(text.as_str()),
                UserContentPart::Image { .. } => None,
            }),
        }
    }

    pub fn display_text(&self) -> String {
        match self {
            UserMessageContent::Text(text) => text.clone(),
            UserMessageContent::Parts(parts) => {
                let mut out = String::new();
                for (idx, part) in parts.iter().enumerate() {
                    if idx > 0 {
                        out.push('\n');
                    }
                    match part {
                        UserContentPart::Text { text } => out.push_str(text),
                        UserContentPart::Image { path, .. } => {
                            out.push_str("[image: ");
                            out.push_str(path);
                            out.push(']');
                        }
                    }
                }
                out
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContentPart {
    Text {
        text: String,
    },
    Image {
        /// Local filesystem path or URL.
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    Developer {
        content: String,
    },
    System {
        content: String,
    },
    User {
        content: UserMessageContent,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    pub messages: Vec<Message>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,

    #[serde(default)]
    pub tool_choice: ToolChoice,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            tool_choice: Default::default(),
            parallel_tool_calls: None,
            temperature: None,
            max_completion_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatResponse {
    pub id: String,
    pub created: u32,
    pub model: String,
    pub message: Message,
    pub finish_reason: Option<FinishReason>,
    pub usage: Option<CompletionUsage>,
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatStreamChunk, LlmError>> + Send>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatStreamChunk {
    pub id: String,
    pub created: u32,
    pub model: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_delta: Option<String>,

    /// Provider-specific chain-of-thought delta (e.g. `reasoning_content`, `thinking`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_delta: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallDelta>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CompletionUsage>,
}

fn to_openai_tool(tool: ToolDefinition) -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
            strict: None,
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
            reasoning_content: _,
            tool_calls,
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

#[derive(Debug, Clone, Deserialize)]
struct ByotCreateChatCompletionResponse {
    pub id: String,
    pub created: u32,
    pub model: String,
    #[serde(default)]
    pub choices: Vec<ByotChatChoice>,
    #[serde(default)]
    pub usage: Option<CompletionUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ByotChatChoice {
    pub message: ByotChatCompletionMessage,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ByotChatCompletionMessage {
    #[serde(default)]
    pub content: Option<String>,

    #[serde(default, rename = "reasoning_content")]
    pub reasoning_content: Option<String>,

    #[serde(default)]
    pub thinking: Option<String>,

    #[serde(default)]
    pub reasoning: Option<String>,

    #[serde(default)]
    pub tool_calls: Option<Vec<ByotToolCall>>,

    #[serde(default)]
    pub function_call: Option<ByotFunctionCall>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ByotToolCall {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<ByotFunctionCall>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ByotFunctionCall {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

fn chat_response_from_byot(
    resp: ByotCreateChatCompletionResponse,
) -> Result<ChatResponse, LlmError> {
    let ByotCreateChatCompletionResponse {
        id,
        created,
        model,
        choices,
        usage,
    } = resp;

    let choice = choices.into_iter().next().ok_or(LlmError::NoChoices)?;
    let ByotChatChoice {
        message,
        finish_reason,
    } = choice;
    let ByotChatCompletionMessage {
        content,
        reasoning_content,
        thinking,
        reasoning,
        tool_calls,
        function_call,
    } = message;

    let tool_calls = if let Some(calls) = tool_calls {
        calls
            .into_iter()
            .map(|c| ToolCall {
                id: c.id.unwrap_or_default(),
                name: c
                    .function
                    .as_ref()
                    .and_then(|f| f.name.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
                arguments: c
                    .function
                    .as_ref()
                    .and_then(|f| f.arguments.clone())
                    .unwrap_or_default(),
            })
            .collect()
    } else if let Some(call) = function_call {
        vec![ToolCall {
            id: String::new(),
            name: call.name.unwrap_or_else(|| "unknown".to_string()),
            arguments: call.arguments.unwrap_or_default(),
        }]
    } else {
        Vec::new()
    };

    let reasoning_content = if tool_calls.is_empty() {
        None
    } else {
        reasoning_content.or(thinking).or(reasoning)
    };

    Ok(ChatResponse {
        id,
        created,
        model,
        message: Message::Assistant {
            content,
            reasoning_content,
            tool_calls,
        },
        finish_reason,
        usage,
    })
}

#[derive(Debug, Clone, Deserialize)]
struct ByotCreateChatCompletionStreamResponse {
    pub id: String,
    pub created: u32,
    pub model: String,
    #[serde(default)]
    pub choices: Vec<ByotChatChoiceStream>,
    #[serde(default)]
    pub usage: Option<CompletionUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ByotChatChoiceStream {
    #[serde(default)]
    pub delta: ByotChatCompletionStreamDelta,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ByotChatCompletionStreamDelta {
    #[serde(default)]
    pub content: Option<String>,

    #[serde(default, rename = "reasoning_content")]
    pub reasoning_content: Option<String>,

    #[serde(default)]
    pub thinking: Option<String>,

    #[serde(default)]
    pub reasoning: Option<String>,

    #[serde(default)]
    pub tool_calls: Option<Vec<ByotToolCallChunk>>,

    #[serde(default)]
    pub function_call: Option<ByotFunctionCallStream>,
}

#[derive(Debug, Clone, Deserialize)]
struct ByotToolCallChunk {
    pub index: u32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<ByotFunctionCallStream>,
}

#[derive(Debug, Clone, Deserialize)]
struct ByotFunctionCallStream {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

fn chat_stream_chunk_from_byot(resp: ByotCreateChatCompletionStreamResponse) -> ChatStreamChunk {
    let mut content_delta = None;
    let mut thinking_delta = None;
    let mut tool_calls = Vec::new();
    let mut finish_reason = None;

    if let Some(choice) = resp.choices.into_iter().next() {
        content_delta = choice.delta.content;
        thinking_delta = choice
            .delta
            .reasoning_content
            .or(choice.delta.thinking)
            .or(choice.delta.reasoning);
        finish_reason = choice.finish_reason;

        if let Some(calls) = choice.delta.tool_calls {
            tool_calls = calls
                .into_iter()
                .map(|c| ToolCallDelta {
                    index: c.index,
                    id: c.id,
                    name: c.function.as_ref().and_then(|f| f.name.clone()),
                    arguments: c.function.as_ref().and_then(|f| f.arguments.clone()),
                })
                .collect();
        } else if let Some(function_call) = choice.delta.function_call {
            tool_calls = vec![ToolCallDelta {
                index: 0,
                id: None,
                name: function_call.name,
                arguments: function_call.arguments,
            }];
        }
    }

    ChatStreamChunk {
        id: resp.id,
        created: resp.created,
        model: resp.model,
        content_delta,
        thinking_delta,
        tool_calls,
        finish_reason,
        usage: resp.usage,
    }
}

#[cfg(test)]
mod tests {
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
    fn openai_tool_conversion_omits_strict() {
        let tool = ToolDefinition {
            name: "t".to_string(),
            description: None,
            parameters: Some(serde_json::json!({"type":"object"})),
            strict: Some(true),
        };
        let openai = to_openai_tool(tool);
        assert!(openai.function.strict.is_none());
    }
}
