use std::collections::BTreeMap;

use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest_eventsource::{Event, RequestBuilderExt};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tracing::Instrument;

use crate::types::{
    ChatRequest, ChatResponse, ChatStreamChunk, FinishReason, Message, TokenUsage, ToolCall,
    ToolCallDelta, ToolChoice, ToolDefinition, UserContentPart, UserMessageContent,
};

use super::{ChatStream, LlmError, LlmProvider, ProviderRoute};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(super) struct AnthropicProvider {
    http: reqwest::Client,
    route: ProviderRoute,
}

impl AnthropicProvider {
    pub(super) fn new(route: ProviderRoute) -> Self {
        Self {
            http: reqwest::Client::new(),
            route,
        }
    }

    fn endpoint(&self) -> String {
        let base = self.route.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        }
    }

    fn headers(&self) -> Result<HeaderMap, LlmError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        if let Some(api_key) = self.route.api_key.as_deref() {
            let value = HeaderValue::from_str(api_key).map_err(|err| {
                LlmError::InvalidRequest(format!("invalid Anthropic api key header: {err}"))
            })?;
            headers.insert("x-api-key", value);
        }
        Ok(headers)
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    fn route(&self) -> &ProviderRoute {
        &self.route
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let started = std::time::Instant::now();
        let span = tracing::info_span!(
            "kiliax.llm.chat",
            llm.provider = %self.route.provider,
            llm.model = %self.route.model,
            llm.base_url = %self.route.base_url,
            llm.stream = false,
            request.messages = req.messages.len() as u64,
            request.tools = req.tools.len() as u64,
        );

        let body = to_anthropic_request(&self.route.model, req, false).await?;
        let endpoint = self.endpoint();
        let response: Result<ChatResponse, LlmError> = async {
            let resp = self
                .http
                .post(endpoint)
                .headers(self.headers()?)
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            if !status.is_success() {
                return Err(api_error_response(status, resp).await);
            }
            let bytes = resp.bytes().await?;
            let parsed: AnthropicMessageResponse = serde_json::from_slice(&bytes)?;
            Ok(chat_response_from_anthropic(parsed))
        }
        .instrument(span.clone())
        .await;

        let outcome = if response.is_ok() { "ok" } else { "error" };
        let usage = response.as_ref().ok().and_then(|r| r.usage);
        crate::telemetry::metrics::record_llm_call(
            &self.route.provider,
            &self.route.model,
            false,
            outcome,
            started.elapsed(),
            usage.map(|u| u.prompt_tokens as u64),
            usage.and_then(|u| u.cached_tokens.map(|v| v as u64)),
            usage.map(|u| u.completion_tokens as u64),
        );

        response
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, LlmError> {
        let started = std::time::Instant::now();
        let span = tracing::info_span!(
            "kiliax.llm.chat_stream",
            llm.provider = %self.route.provider,
            llm.model = %self.route.model,
            llm.base_url = %self.route.base_url,
            llm.stream = true,
            request.messages = req.messages.len() as u64,
            request.tools = req.tools.len() as u64,
        );

        let body = to_anthropic_request(&self.route.model, req, true).await?;
        let endpoint = self.endpoint();
        let headers = self.headers()?;
        let provider = self.route.provider.clone();
        let fallback_model = self.route.model.clone();
        let http = self.http.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamChunk, LlmError>>();

        let event_source = http
            .post(endpoint)
            .headers(headers)
            .json(&body)
            .eventsource()
            .map_err(|err| LlmError::Stream(err.to_string()))?;

        tokio::spawn(
            async move {
                let mut event_source = event_source;
                let mut state = AnthropicStreamState::new(fallback_model);
                let mut outcome = "ok";

                while let Some(event) = event_source.next().await {
                    match event {
                        Ok(Event::Open) => {}
                        Ok(Event::Message(message)) => {
                            if message.data.trim().is_empty() {
                                continue;
                            }
                            match handle_stream_event(&mut state, &message.event, &message.data) {
                                Ok(StreamEventAction::None) => {}
                                Ok(StreamEventAction::Chunk(chunk)) => {
                                    if tx.send(Ok(chunk)).is_err() {
                                        outcome = "cancelled";
                                        break;
                                    }
                                }
                                Ok(StreamEventAction::Stop) => break,
                                Err(err) => {
                                    let _ = tx.send(Err(err));
                                    outcome = "error";
                                    break;
                                }
                            }
                        }
                        Err(reqwest_eventsource::Error::StreamEnded) => break,
                        Err(err) => {
                            let mapped = map_eventsource_error(err).await;
                            let _ = tx.send(Err(mapped));
                            outcome = "error";
                            break;
                        }
                    }
                }
                event_source.close();
                crate::telemetry::metrics::record_llm_call(
                    &provider,
                    &state.model,
                    true,
                    outcome,
                    started.elapsed(),
                    state.usage.map(|u| u.prompt_tokens as u64),
                    state.usage.and_then(|u| u.cached_tokens.map(|v| v as u64)),
                    state.usage.map(|u| u.completion_tokens as u64),
                );
            }
            .instrument(span),
        );

        Ok(Box::pin(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ))
    }
}

#[derive(Debug, Serialize)]
struct AnthropicMessagesRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicRequestMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_choice: Option<AnthropicToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct AnthropicRequestMessage {
    role: AnthropicRole,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum AnthropicRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    Image {
        source: AnthropicImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    input_schema: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicToolChoice {
    Auto {
        #[serde(default, skip_serializing_if = "is_false")]
        disable_parallel_tool_use: bool,
    },
    Any {
        #[serde(default, skip_serializing_if = "is_false")]
        disable_parallel_tool_use: bool,
    },
    Tool {
        name: String,
        #[serde(default, skip_serializing_if = "is_false")]
        disable_parallel_tool_use: bool,
    },
    None,
}

fn is_false(value: &bool) -> bool {
    !*value
}

async fn to_anthropic_request(
    model: &str,
    req: ChatRequest,
    stream: bool,
) -> Result<AnthropicMessagesRequest, LlmError> {
    let ChatRequest {
        messages,
        tools,
        tool_choice,
        parallel_tool_calls,
        temperature,
        max_completion_tokens,
    } = req;

    let mut system_parts = Vec::new();
    let mut out_messages = Vec::new();
    let mut messages = messages.into_iter().peekable();
    while let Some(message) = messages.next() {
        match message {
            Message::Developer { content } | Message::System { content } => {
                if !content.trim().is_empty() {
                    system_parts.push(content);
                }
            }
            Message::User { content } => {
                out_messages.push(AnthropicRequestMessage {
                    role: AnthropicRole::User,
                    content: user_content_to_anthropic(content).await?,
                });
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut blocks = Vec::new();
                if let Some(content) = content.filter(|c| !c.trim().is_empty()) {
                    blocks.push(AnthropicContentBlock::Text { text: content });
                }
                for call in tool_calls {
                    let input = serde_json::from_str(&call.arguments).map_err(|err| {
                        LlmError::InvalidRequest(format!(
                            "tool call {} arguments are not valid JSON: {err}",
                            call.id
                        ))
                    })?;
                    blocks.push(AnthropicContentBlock::ToolUse {
                        id: call.id,
                        name: call.name,
                        input,
                    });
                }
                if blocks.is_empty() {
                    return Err(LlmError::InvalidRequest(
                        "assistant message content must not be empty for Anthropic".to_string(),
                    ));
                }
                out_messages.push(AnthropicRequestMessage {
                    role: AnthropicRole::Assistant,
                    content: blocks,
                });
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                let mut blocks = vec![AnthropicContentBlock::ToolResult {
                    tool_use_id: tool_call_id,
                    content,
                }];
                while matches!(messages.peek(), Some(Message::Tool { .. })) {
                    let Some(Message::Tool {
                        tool_call_id,
                        content,
                    }) = messages.next()
                    else {
                        unreachable!("peeked tool message");
                    };
                    blocks.push(AnthropicContentBlock::ToolResult {
                        tool_use_id: tool_call_id,
                        content,
                    });
                }
                out_messages.push(AnthropicRequestMessage {
                    role: AnthropicRole::User,
                    content: blocks,
                });
            }
        }
    }

    if out_messages.is_empty() {
        return Err(LlmError::InvalidRequest(
            "Anthropic requests require at least one user or assistant message".to_string(),
        ));
    }

    let tools = tools.into_iter().map(to_anthropic_tool).collect::<Vec<_>>();
    let tool_choice = if tools.is_empty() {
        None
    } else {
        Some(to_anthropic_tool_choice(tool_choice, parallel_tool_calls))
    };

    Ok(AnthropicMessagesRequest {
        model: model.to_string(),
        max_tokens: max_completion_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        messages: out_messages,
        system: (!system_parts.is_empty()).then(|| system_parts.join("\n\n")),
        tools,
        tool_choice,
        temperature,
        stream,
    })
}

async fn user_content_to_anthropic(
    content: UserMessageContent,
) -> Result<Vec<AnthropicContentBlock>, LlmError> {
    match content {
        UserMessageContent::Text(text) => {
            if text.trim().is_empty() {
                return Err(LlmError::InvalidRequest(
                    "user message text must not be empty".to_string(),
                ));
            }
            Ok(vec![AnthropicContentBlock::Text { text }])
        }
        UserMessageContent::Parts(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                match part {
                    UserContentPart::Text { text } => {
                        if !text.trim().is_empty() {
                            blocks.push(AnthropicContentBlock::Text { text });
                        }
                    }
                    UserContentPart::Image { path, .. } => {
                        blocks.push(AnthropicContentBlock::Image {
                            source: image_source_from_path(&path).await?,
                        });
                    }
                }
            }
            if blocks.is_empty() {
                return Err(LlmError::InvalidRequest(
                    "user message content must not be empty".to_string(),
                ));
            }
            Ok(blocks)
        }
    }
}

fn to_anthropic_tool(tool: ToolDefinition) -> AnthropicTool {
    AnthropicTool {
        name: tool.name,
        description: tool.description,
        input_schema: tool
            .parameters
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
    }
}

fn to_anthropic_tool_choice(
    choice: ToolChoice,
    parallel_tool_calls: Option<bool>,
) -> AnthropicToolChoice {
    let disable_parallel_tool_use = parallel_tool_calls == Some(false);
    match choice {
        ToolChoice::None => AnthropicToolChoice::None,
        ToolChoice::Auto => AnthropicToolChoice::Auto {
            disable_parallel_tool_use,
        },
        ToolChoice::Required => AnthropicToolChoice::Any {
            disable_parallel_tool_use,
        },
        ToolChoice::Named { name } => AnthropicToolChoice::Tool {
            name,
            disable_parallel_tool_use,
        },
    }
}

async fn image_source_from_path(path: &str) -> Result<AnthropicImageSource, LlmError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(LlmError::InvalidImage("path must not be empty".to_string()));
    }

    if path.starts_with("http://") || path.starts_with("https://") {
        return Ok(AnthropicImageSource::Url {
            url: path.to_string(),
        });
    }

    if let Some(rest) = path.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',').ok_or_else(|| {
            LlmError::InvalidImage("data URL image must contain a comma".to_string())
        })?;
        let media_type = meta
            .split_once(';')
            .map(|(media_type, _)| media_type)
            .unwrap_or(meta);
        if media_type.trim().is_empty() {
            return Err(LlmError::InvalidImage(
                "data URL image media type must not be empty".to_string(),
            ));
        }
        return Ok(AnthropicImageSource::Base64 {
            media_type: media_type.to_string(),
            data: data.to_string(),
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
    let media_type = guess_image_mime_type(fs_path).ok_or_else(|| {
        LlmError::InvalidImage(format!(
            "unsupported image extension for `{}`",
            fs_path.display()
        ))
    })?;
    let bytes = tokio::fs::read(fs_path).await?;
    Ok(AnthropicImageSource::Base64 {
        media_type: media_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn guess_image_mime_type(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.trim().to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageResponse {
    id: String,
    model: String,
    #[serde(default)]
    content: Vec<serde_json::Value>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

impl AnthropicUsage {
    fn into_token_usage(self) -> TokenUsage {
        TokenUsage {
            prompt_tokens: self.input_tokens,
            completion_tokens: self.output_tokens,
            total_tokens: self.input_tokens.saturating_add(self.output_tokens),
            cached_tokens: self.cache_read_input_tokens.filter(|v| *v > 0),
        }
    }
}

fn chat_response_from_anthropic(resp: AnthropicMessageResponse) -> ChatResponse {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for block in resp.content {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    content.push_str(text);
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let input = block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
            _ => {}
        }
    }

    let usage = resp.usage.map(AnthropicUsage::into_token_usage);
    ChatResponse {
        id: resp.id,
        created: 0,
        model: resp.model,
        message: Message::Assistant {
            content: (!content.is_empty()).then_some(content),
            reasoning_content: None,
            tool_calls,
            usage,
            provider_metadata: None,
        },
        finish_reason: resp
            .stop_reason
            .as_deref()
            .map(finish_reason_from_anthropic),
        usage,
    }
}

fn finish_reason_from_anthropic(reason: &str) -> FinishReason {
    match reason {
        "end_turn" => FinishReason::Stop,
        "stop_sequence" => FinishReason::StopSequence,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::Refusal,
        "pause_turn" => FinishReason::PauseTurn,
        other => FinishReason::Other(other.to_string()),
    }
}

#[derive(Debug)]
enum StreamEventAction {
    None,
    Chunk(ChatStreamChunk),
    Stop,
}

#[derive(Debug, Default)]
struct ToolUseStreamBuf {
    id: Option<String>,
    name: Option<String>,
    initial_input: Option<serde_json::Value>,
    partial_json: String,
}

#[derive(Debug)]
struct AnthropicStreamState {
    id: String,
    model: String,
    usage: Option<TokenUsage>,
    tool_uses: BTreeMap<u32, ToolUseStreamBuf>,
}

impl AnthropicStreamState {
    fn new(model: String) -> Self {
        Self {
            id: String::new(),
            model,
            usage: None,
            tool_uses: BTreeMap::new(),
        }
    }

    fn chunk(&self) -> ChatStreamChunk {
        ChatStreamChunk {
            id: self.id.clone(),
            created: 0,
            model: self.model.clone(),
            content_delta: None,
            thinking_delta: None,
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
            provider_metadata: None,
        }
    }
}

fn handle_stream_event(
    state: &mut AnthropicStreamState,
    event: &str,
    data: &str,
) -> Result<StreamEventAction, LlmError> {
    match event {
        "message_start" => {
            let parsed: MessageStartEvent = serde_json::from_str(data)?;
            state.id = parsed.message.id;
            state.model = parsed.message.model;
            if let Some(usage) = parsed.message.usage {
                state.usage = Some(usage.into_token_usage());
            }
            Ok(StreamEventAction::None)
        }
        "content_block_start" => {
            let parsed: ContentBlockStartEvent = serde_json::from_str(data)?;
            if let Some(block_type) = parsed.content_block.get("type").and_then(|v| v.as_str()) {
                if block_type == "tool_use" {
                    let id = parsed
                        .content_block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let name = parsed
                        .content_block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let initial_input = parsed.content_block.get("input").cloned();
                    state.tool_uses.insert(
                        parsed.index,
                        ToolUseStreamBuf {
                            id,
                            name,
                            initial_input,
                            partial_json: String::new(),
                        },
                    );
                } else if block_type == "text" {
                    if let Some(text) = parsed.content_block.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            let mut chunk = state.chunk();
                            chunk.content_delta = Some(text.to_string());
                            return Ok(StreamEventAction::Chunk(chunk));
                        }
                    }
                }
            }
            Ok(StreamEventAction::None)
        }
        "content_block_delta" => {
            let parsed: ContentBlockDeltaEvent = serde_json::from_str(data)?;
            match parsed.delta.delta_type.as_str() {
                "text_delta" => {
                    if let Some(text) = parsed.delta.text {
                        let mut chunk = state.chunk();
                        chunk.content_delta = Some(text);
                        Ok(StreamEventAction::Chunk(chunk))
                    } else {
                        Ok(StreamEventAction::None)
                    }
                }
                "input_json_delta" => {
                    if let Some(partial) = parsed.delta.partial_json {
                        state
                            .tool_uses
                            .entry(parsed.index)
                            .or_default()
                            .partial_json
                            .push_str(&partial);
                    }
                    Ok(StreamEventAction::None)
                }
                "thinking_delta" => {
                    if let Some(thinking) = parsed.delta.thinking {
                        let mut chunk = state.chunk();
                        chunk.thinking_delta = Some(thinking);
                        Ok(StreamEventAction::Chunk(chunk))
                    } else {
                        Ok(StreamEventAction::None)
                    }
                }
                _ => Ok(StreamEventAction::None),
            }
        }
        "content_block_stop" => {
            let parsed: ContentBlockStopEvent = serde_json::from_str(data)?;
            let Some(buf) = state.tool_uses.remove(&parsed.index) else {
                return Ok(StreamEventAction::None);
            };
            let arguments = if buf.partial_json.trim().is_empty() {
                buf.initial_input
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
                    .unwrap_or_else(|| "{}".to_string())
            } else {
                buf.partial_json
            };
            let mut chunk = state.chunk();
            chunk.tool_calls = vec![ToolCallDelta {
                index: parsed.index,
                id: buf.id,
                name: buf.name,
                arguments: Some(arguments),
            }];
            Ok(StreamEventAction::Chunk(chunk))
        }
        "message_delta" => {
            let parsed: MessageDeltaEvent = serde_json::from_str(data)?;
            if let Some(usage) = parsed.usage {
                let mut current = state.usage.unwrap_or(TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    cached_tokens: None,
                });
                if let Some(output_tokens) = usage.output_tokens {
                    current.completion_tokens = output_tokens;
                }
                current.total_tokens = current
                    .prompt_tokens
                    .saturating_add(current.completion_tokens);
                state.usage = Some(current);
            }
            let mut chunk = state.chunk();
            chunk.finish_reason = parsed
                .delta
                .stop_reason
                .as_deref()
                .map(finish_reason_from_anthropic);
            chunk.usage = state.usage;
            if chunk.finish_reason.is_some() || chunk.usage.is_some() {
                Ok(StreamEventAction::Chunk(chunk))
            } else {
                Ok(StreamEventAction::None)
            }
        }
        "message_stop" => Ok(StreamEventAction::Stop),
        "ping" => Ok(StreamEventAction::None),
        "error" => Err(stream_error(data)),
        _ => Ok(StreamEventAction::None),
    }
}

#[derive(Debug, Deserialize)]
struct MessageStartEvent {
    message: StreamMessageStart,
}

#[derive(Debug, Deserialize)]
struct StreamMessageStart {
    id: String,
    model: String,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStartEvent {
    index: u32,
    content_block: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ContentBlockDeltaEvent {
    index: u32,
    delta: ContentBlockDelta,
}

#[derive(Debug, Deserialize)]
struct ContentBlockDelta {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStopEvent {
    index: u32,
}

#[derive(Debug, Deserialize)]
struct MessageDeltaEvent {
    delta: MessageDelta,
    #[serde(default)]
    usage: Option<MessageDeltaUsage>,
}

#[derive(Debug, Deserialize)]
struct MessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageDeltaUsage {
    #[serde(default)]
    output_tokens: Option<u32>,
}

fn stream_error(data: &str) -> LlmError {
    #[derive(Debug, Deserialize)]
    struct ErrorEvent {
        error: ErrorBody,
    }
    #[derive(Debug, Deserialize)]
    struct ErrorBody {
        #[serde(default)]
        message: String,
    }

    match serde_json::from_str::<ErrorEvent>(data) {
        Ok(event) if !event.error.message.is_empty() => LlmError::Stream(event.error.message),
        _ => LlmError::Stream(data.to_string()),
    }
}

async fn map_eventsource_error(err: reqwest_eventsource::Error) -> LlmError {
    match err {
        reqwest_eventsource::Error::Transport(err) => LlmError::Http(err),
        reqwest_eventsource::Error::InvalidStatusCode(status, response) => {
            api_error_response(status, response).await
        }
        reqwest_eventsource::Error::InvalidContentType(_ct, response) => {
            api_error_response(response.status(), response).await
        }
        reqwest_eventsource::Error::StreamEnded => LlmError::Stream("Stream ended".to_string()),
        other => LlmError::Stream(other.to_string()),
    }
}

async fn api_error_response(status: reqwest::StatusCode, response: reqwest::Response) -> LlmError {
    const MAX_BODY_BYTES: usize = 16 * 1024;
    let bytes = match response.bytes().await {
        Ok(bytes) => {
            if bytes.len() > MAX_BODY_BYTES {
                bytes.slice(..MAX_BODY_BYTES)
            } else {
                bytes
            }
        }
        Err(err) => return LlmError::Http(err),
    };
    let body = String::from_utf8_lossy(&bytes).trim().to_string();
    LlmError::Api { status, body }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LlmClient;
    use crate::ProviderApi;

    fn route() -> ProviderRoute {
        ProviderRoute {
            provider: "anthropic".to_string(),
            api: ProviderApi::AnthropicMessages,
            model: "claude-3-5-sonnet-latest".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key: Some("key".to_string()),
        }
    }

    struct TestHttpServer {
        base_url: String,
        request: std::sync::mpsc::Receiver<String>,
        join: std::thread::JoinHandle<()>,
    }

    impl TestHttpServer {
        fn spawn(response: String) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let join = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                tx.send(request).unwrap();
                use std::io::Write as _;
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            });
            Self {
                base_url: format!("http://{addr}"),
                request: rx,
                join,
            }
        }

        fn take_request(self) -> String {
            let request = self.request.recv().unwrap();
            self.join.join().unwrap();
            request
        }
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read as _;

        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        let mut header_end = None;
        while header_end.is_none() {
            let n = stream.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            header_end = find_header_end(&buf);
        }

        let Some(header_end) = header_end else {
            return String::from_utf8_lossy(&buf).to_string();
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let body_start = header_end + 4;
        while buf.len().saturating_sub(body_start) < content_length {
            let n = stream.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn sse_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn client_for(base_url: String) -> LlmClient {
        LlmClient::new(ProviderRoute {
            provider: "anthropic".to_string(),
            api: ProviderApi::AnthropicMessages,
            model: "claude-test".to_string(),
            base_url,
            api_key: Some("test-key".to_string()),
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn maps_system_user_and_tools_to_messages_request() {
        let req = ChatRequest {
            messages: vec![
                Message::System {
                    content: "sys".to_string(),
                },
                Message::Developer {
                    content: "dev".to_string(),
                },
                Message::User {
                    content: UserMessageContent::Text("hi".to_string()),
                },
            ],
            tools: vec![ToolDefinition {
                name: "read".to_string(),
                description: Some("Read a file".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}}
                })),
                strict: Some(true),
            }],
            tool_choice: ToolChoice::Named {
                name: "read".to_string(),
            },
            parallel_tool_calls: Some(true),
            temperature: Some(0.2),
            max_completion_tokens: Some(128),
        };

        let body = to_anthropic_request("claude", req, false).await.unwrap();
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(json["system"], serde_json::json!("sys\n\ndev"));
        assert_eq!(json["messages"][0]["role"], serde_json::json!("user"));
        assert_eq!(
            json["messages"][0]["content"][0]["type"],
            serde_json::json!("text")
        );
        assert_eq!(
            json["tools"][0]["input_schema"]["type"],
            serde_json::json!("object")
        );
        assert_eq!(
            json["tool_choice"],
            serde_json::json!({"type": "tool", "name": "read"})
        );
        assert_eq!(json["max_tokens"], serde_json::json!(128));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn maps_disabled_parallel_tool_calls_to_anthropic_tool_choice() {
        let req = ChatRequest {
            messages: vec![Message::User {
                content: UserMessageContent::Text("hi".to_string()),
            }],
            tools: vec![ToolDefinition {
                name: "read".to_string(),
                description: None,
                parameters: Some(serde_json::json!({"type": "object"})),
                strict: None,
            }],
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: Some(false),
            temperature: None,
            max_completion_tokens: None,
        };

        let body = to_anthropic_request("claude", req, false).await.unwrap();
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(
            json["tool_choice"],
            serde_json::json!({"type": "auto", "disable_parallel_tool_use": true})
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn maps_assistant_tool_call_and_tool_result_history() {
        let req = ChatRequest {
            messages: vec![
                Message::User {
                    content: UserMessageContent::Text("use tool".to_string()),
                },
                Message::Assistant {
                    content: None,
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "toolu_1".to_string(),
                        name: "read".to_string(),
                        arguments: "{\"path\":\"README.md\"}".to_string(),
                    }],
                    usage: None,
                    provider_metadata: None,
                },
                Message::Tool {
                    tool_call_id: "toolu_1".to_string(),
                    content: "ok".to_string(),
                },
            ],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
            temperature: None,
            max_completion_tokens: None,
        };

        let body = to_anthropic_request("claude", req, false).await.unwrap();
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(json["messages"][1]["role"], serde_json::json!("assistant"));
        assert_eq!(
            json["messages"][1]["content"][0]["type"],
            serde_json::json!("tool_use")
        );
        assert_eq!(
            json["messages"][1]["content"][0]["input"]["path"],
            serde_json::json!("README.md")
        );
        assert_eq!(json["messages"][2]["role"], serde_json::json!("user"));
        assert_eq!(
            json["messages"][2]["content"][0]["type"],
            serde_json::json!("tool_result")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn groups_parallel_tool_results_into_one_user_message() {
        let req = ChatRequest {
            messages: vec![
                Message::User {
                    content: UserMessageContent::Text("use tools".to_string()),
                },
                Message::Assistant {
                    content: None,
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "toolu_1".to_string(),
                            name: "read".to_string(),
                            arguments: "{\"path\":\"README.md\"}".to_string(),
                        },
                        ToolCall {
                            id: "toolu_2".to_string(),
                            name: "grep".to_string(),
                            arguments: "{\"query\":\"fn main\"}".to_string(),
                        },
                    ],
                    usage: None,
                    provider_metadata: None,
                },
                Message::Tool {
                    tool_call_id: "toolu_1".to_string(),
                    content: "read ok".to_string(),
                },
                Message::Tool {
                    tool_call_id: "toolu_2".to_string(),
                    content: "grep ok".to_string(),
                },
            ],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
            temperature: None,
            max_completion_tokens: None,
        };

        let body = to_anthropic_request("claude", req, false).await.unwrap();
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(json["messages"].as_array().unwrap().len(), 3);
        assert_eq!(json["messages"][2]["role"], serde_json::json!("user"));
        assert_eq!(
            json["messages"][2]["content"][0]["tool_use_id"],
            serde_json::json!("toolu_1")
        );
        assert_eq!(
            json["messages"][2]["content"][1]["tool_use_id"],
            serde_json::json!("toolu_2")
        );
    }

    #[test]
    fn maps_non_streaming_response_text_and_tool_calls() {
        let raw = serde_json::json!({
            "id": "msg_1",
            "model": "claude",
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {"path": "README.md"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 3}
        });
        let parsed: AnthropicMessageResponse = serde_json::from_value(raw).unwrap();
        let resp = chat_response_from_anthropic(parsed);
        assert_eq!(resp.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(resp.usage.unwrap().cached_tokens, Some(3));
        let Message::Assistant {
            content,
            tool_calls,
            ..
        } = resp.message
        else {
            panic!("expected assistant");
        };
        assert_eq!(content.as_deref(), Some("hello"));
        assert_eq!(tool_calls[0].arguments, "{\"path\":\"README.md\"}");
    }

    #[test]
    fn streaming_aggregates_tool_use_delta_until_block_stop() {
        let mut state = AnthropicStreamState::new("claude".to_string());
        handle_stream_event(
            &mut state,
            "message_start",
            r#"{"message":{"id":"msg_1","model":"claude","usage":{"input_tokens":7,"output_tokens":0}}}"#,
        )
        .unwrap();
        handle_stream_event(
            &mut state,
            "content_block_start",
            r#"{"index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}}"#,
        )
        .unwrap();
        handle_stream_event(
            &mut state,
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
        )
        .unwrap();
        handle_stream_event(
            &mut state,
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"\"README.md\"}"}}"#,
        )
        .unwrap();
        let action =
            handle_stream_event(&mut state, "content_block_stop", r#"{"index":1}"#).unwrap();
        let StreamEventAction::Chunk(chunk) = action else {
            panic!("expected chunk");
        };
        assert_eq!(chunk.tool_calls.len(), 1);
        assert_eq!(chunk.tool_calls[0].id.as_deref(), Some("toolu_1"));
        assert_eq!(chunk.tool_calls[0].name.as_deref(), Some("read"));
        assert_eq!(
            chunk.tool_calls[0].arguments.as_deref(),
            Some("{\"path\":\"README.md\"}")
        );
    }

    #[test]
    fn endpoint_adds_v1_when_base_url_omits_it() {
        let provider = AnthropicProvider::new(ProviderRoute {
            base_url: "https://api.anthropic.com".to_string(),
            ..route()
        });
        assert_eq!(provider.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires binding a local TCP listener"]
    async fn client_posts_non_streaming_text_to_messages_endpoint() {
        let body = r#"{
            "id":"msg_1",
            "model":"claude-test",
            "content":[{"type":"text","text":"hello"}],
            "stop_reason":"end_turn",
            "usage":{"input_tokens":4,"output_tokens":2}
        }"#;
        let server = TestHttpServer::spawn(json_response(body));
        let client = client_for(server.base_url.clone());

        let resp = client
            .chat(ChatRequest::new(vec![Message::User {
                content: UserMessageContent::Text("hi".to_string()),
            }]))
            .await
            .unwrap();

        let request = server.take_request();
        assert!(request.starts_with("POST /v1/messages HTTP/1.1"));
        assert!(request
            .to_ascii_lowercase()
            .contains("anthropic-version: 2023-06-01"));
        let Message::Assistant { content, .. } = resp.message else {
            panic!("expected assistant");
        };
        assert_eq!(content.as_deref(), Some("hello"));
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
        assert_eq!(resp.usage.unwrap().total_tokens, 6);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires binding a local TCP listener"]
    async fn client_maps_non_streaming_tool_call_response() {
        let body = r#"{
            "id":"msg_1",
            "model":"claude-test",
            "content":[{"type":"tool_use","id":"toolu_1","name":"read","input":{"path":"README.md"}}],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":8,"output_tokens":3}
        }"#;
        let server = TestHttpServer::spawn(json_response(body));
        let client = client_for(server.base_url.clone());

        let resp = client
            .chat(ChatRequest::new(vec![Message::User {
                content: UserMessageContent::Text("read".to_string()),
            }]))
            .await
            .unwrap();

        let _request = server.take_request();
        let Message::Assistant { tool_calls, .. } = resp.message else {
            panic!("expected assistant");
        };
        assert_eq!(resp.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "toolu_1");
        assert_eq!(tool_calls[0].name, "read");
        assert_eq!(tool_calls[0].arguments, "{\"path\":\"README.md\"}");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires binding a local TCP listener"]
    async fn client_streams_text_chunks_and_usage() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"message\":{\"id\":\"msg_1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hel\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
            "event: message_delta\n",
            "data: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\n",
            "data: {}\n\n",
        );
        let server = TestHttpServer::spawn(sse_response(sse));
        let client = client_for(server.base_url.clone());

        let mut stream = client
            .chat_stream(ChatRequest::new(vec![Message::User {
                content: UserMessageContent::Text("hi".to_string()),
            }]))
            .await
            .unwrap();
        let mut text = String::new();
        let mut usage = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            if let Some(delta) = chunk.content_delta {
                text.push_str(&delta);
            }
            if chunk.usage.is_some() {
                usage = chunk.usage;
            }
        }

        let request = server.take_request();
        assert!(request.contains("\"stream\":true"));
        assert_eq!(text, "hello");
        assert_eq!(usage.unwrap().total_tokens, 7);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires binding a local TCP listener"]
    async fn client_streams_tool_call_when_input_json_completes() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"message\":{\"id\":\"msg_1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"README.md\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\n",
            "data: {}\n\n",
        );
        let server = TestHttpServer::spawn(sse_response(sse));
        let client = client_for(server.base_url.clone());

        let mut stream = client
            .chat_stream(ChatRequest::new(vec![Message::User {
                content: UserMessageContent::Text("read".to_string()),
            }]))
            .await
            .unwrap();
        let mut tool_calls = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            tool_calls.extend(chunk.tool_calls);
        }

        let _request = server.take_request();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id.as_deref(), Some("toolu_1"));
        assert_eq!(tool_calls[0].name.as_deref(), Some("read"));
        assert_eq!(
            tool_calls[0].arguments.as_deref(),
            Some("{\"path\":\"README.md\"}")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires binding a local TCP listener"]
    async fn api_error_includes_status_and_body() {
        let body = r#"{"error":{"type":"authentication_error","message":"bad key"}}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let server = TestHttpServer::spawn(response);
        let client = client_for(server.base_url.clone());

        let err = client
            .chat(ChatRequest::new(vec![Message::User {
                content: UserMessageContent::Text("hi".to_string()),
            }]))
            .await
            .unwrap_err();

        let _request = server.take_request();
        let LlmError::Api { status, body } = err else {
            panic!("expected api error, got {err:?}");
        };
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
        assert!(body.contains("bad key"));
    }
}
