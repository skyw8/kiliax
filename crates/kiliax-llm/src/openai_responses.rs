use std::collections::{BTreeMap, HashMap, HashSet};

use async_openai::config::Config as OpenAIConfigTrait;
use async_openai::error::OpenAIError;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest_eventsource::{Event, RequestBuilderExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::StreamExt;
use tracing::Instrument;

use crate::openai_config::KiliaxOpenAIConfig;
use crate::openai_conv::{image_url_from_path, validate_pdf_file_data};
use crate::types::{
    ChatRequest, ChatResponse, ChatStreamChunk, FinishReason, ImageDetail, Message,
    ProviderMessageMetadata, TokenUsage, ToolCall, ToolCallDelta, ToolChoice, ToolDefinition,
    UserContentPart, UserMessageContent,
};

use super::api_errors::{map_api_error_response, map_eventsource_error};
use super::patches::inject_prompt_cache_fields;
use super::tool_names::{
    to_internal_tool_name, to_wire_tool_choice, to_wire_tool_definition, to_wire_tool_name,
};
use super::{ChatStream, LlmError, LlmProvider, ProviderRoute};

const DEFAULT_ARGUMENTS: &str = "{}";

#[derive(Debug, Clone)]
pub(super) struct OpenAiResponsesProvider {
    http: reqwest::Client,
    cfg: KiliaxOpenAIConfig,
    route: ProviderRoute,
    prompt_cache_key: Option<String>,
}

impl OpenAiResponsesProvider {
    pub(super) fn new(route: ProviderRoute) -> Self {
        let cfg = KiliaxOpenAIConfig::new(&route.base_url, route.api_key.as_deref());
        Self {
            http: reqwest::Client::new(),
            cfg,
            route,
            prompt_cache_key: None,
        }
    }

    pub(super) fn set_prompt_cache_key(&mut self, prompt_cache_key: Option<String>) {
        self.prompt_cache_key = prompt_cache_key
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = self.cfg.headers();
        if should_enable_dashscope_session_cache(&self.route) {
            headers.insert(
                "x-dashscope-session-cache",
                HeaderValue::from_static("enable"),
            );
        }
        headers
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiResponsesProvider {
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

        let response: Result<ChatResponse, LlmError> = async {
            let body = to_responses_request(
                &self.route.model,
                req,
                false,
                self.prompt_cache_key.as_deref(),
            )
            .await?;
            let resp = self
                .http
                .post(self.cfg.url("/responses"))
                .query(&self.cfg.query())
                .headers(self.headers())
                .json(&body)
                .send()
                .await
                .map_err(OpenAIError::Reqwest)?;
            let status = resp.status();
            if !status.is_success() {
                return Err(LlmError::OpenAI(map_api_error_response(status, resp).await));
            }
            let bytes = resp.bytes().await.map_err(OpenAIError::Reqwest)?;
            let parsed: OpenAiResponse = serde_json::from_slice(&bytes)
                .map_err(|e| LlmError::OpenAI(OpenAIError::JSONDeserialize(e)))?;
            chat_response_from_openai_responses(parsed, &self.route.model)
        }
        .instrument(span)
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

        let body = to_responses_request(
            &self.route.model,
            req,
            true,
            self.prompt_cache_key.as_deref(),
        )
        .instrument(span.clone())
        .await?;
        let event_source = self
            .http
            .post(self.cfg.url("/responses"))
            .query(&self.cfg.query())
            .headers(self.headers())
            .json(&body)
            .eventsource()
            .map_err(|err| LlmError::OpenAI(OpenAIError::StreamError(err.to_string())))?;

        let provider = self.route.provider.clone();
        let model = self.route.model.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<ChatStreamChunk, LlmError>>();

        tokio::spawn(
            async move {
                let mut event_source = event_source;
                let mut state = ResponsesStreamState::new(model.clone());
                let mut outcome = "ok";

                while let Some(event) = event_source.next().await {
                    match event {
                        Ok(Event::Open) => {}
                        Ok(Event::Message(message)) => {
                            let data = message.data.trim();
                            if data.is_empty() || data == "[DONE]" {
                                if data == "[DONE]" {
                                    break;
                                }
                                continue;
                            }
                            match handle_stream_event(&mut state, &message.event, data) {
                                Ok(StreamEventAction::None) => {}
                                Ok(StreamEventAction::Chunk(chunk)) => {
                                    if tx.send(Ok(chunk)).is_err() {
                                        outcome = "cancelled";
                                        break;
                                    }
                                }
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
                            let _ = tx.send(Err(LlmError::OpenAI(mapped)));
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

async fn to_responses_request(
    model: &str,
    req: ChatRequest,
    stream: bool,
    prompt_cache_key: Option<&str>,
) -> Result<Value, LlmError> {
    let ChatRequest {
        messages,
        tools,
        tool_choice,
        parallel_tool_calls,
        temperature,
        max_completion_tokens,
    } = req;

    let mut instructions = Vec::new();
    let mut input = Vec::new();

    for message in messages {
        match message {
            Message::Developer { content } | Message::System { content } => {
                if !content.trim().is_empty() {
                    instructions.push(content);
                }
            }
            Message::User { content } => {
                input.push(json!({
                    "role": "user",
                    "content": user_content_to_responses(content).await?,
                }));
            }
            Message::Assistant {
                content,
                tool_calls,
                provider_metadata,
                ..
            } => {
                if let Some(metadata) = provider_metadata {
                    let output = metadata.openai_responses_output().unwrap_or_default();
                    if !output.is_empty() {
                        input.extend(output.iter().map(to_wire_responses_output_item));
                        continue;
                    }
                }

                if let Some(content) = content.filter(|c| !c.trim().is_empty()) {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": content}],
                    }));
                }
                for call in tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": to_wire_tool_name(&call.name),
                        "arguments": call.arguments,
                    }));
                }
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content,
                }));
            }
        }
    }

    if input.is_empty() {
        return Err(LlmError::InvalidRequest(
            "Responses requests require at least one user, assistant, or tool item".to_string(),
        ));
    }

    let mut body = json!({
        "model": model,
        "input": input,
    });
    let obj = body.as_object_mut().expect("body is object");

    if !instructions.is_empty() {
        obj.insert(
            "instructions".to_string(),
            Value::String(instructions.join("\n\n")),
        );
    }
    if stream {
        obj.insert("stream".to_string(), Value::Bool(true));
    }
    if !tools.is_empty() {
        obj.insert(
            "tools".to_string(),
            Value::Array(tools.into_iter().map(to_responses_tool).collect()),
        );
        obj.insert(
            "tool_choice".to_string(),
            to_responses_tool_choice(tool_choice),
        );
    }
    if let Some(parallel_tool_calls) = parallel_tool_calls {
        obj.insert(
            "parallel_tool_calls".to_string(),
            Value::Bool(parallel_tool_calls),
        );
    }
    if let Some(temperature) = temperature {
        obj.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(max_completion_tokens) = max_completion_tokens {
        obj.insert(
            "max_output_tokens".to_string(),
            json!(max_completion_tokens),
        );
    }
    inject_prompt_cache_fields(&mut body, prompt_cache_key);

    Ok(body)
}

fn should_enable_dashscope_session_cache(route: &ProviderRoute) -> bool {
    let provider = route.provider.to_ascii_lowercase();
    let base_url = route.base_url.to_ascii_lowercase();
    provider.contains("dashscope") || base_url.contains("dashscope.aliyuncs.com")
}

async fn user_content_to_responses(content: UserMessageContent) -> Result<Vec<Value>, LlmError> {
    let mut out = Vec::new();
    match content {
        UserMessageContent::Text(text) => {
            if text.trim().is_empty() {
                return Err(LlmError::InvalidRequest(
                    "user message text must not be empty".to_string(),
                ));
            }
            out.push(json!({"type": "input_text", "text": text}));
        }
        UserMessageContent::Parts(parts) => {
            let mut saw_text = false;
            for part in parts {
                match part {
                    UserContentPart::Text { text } => {
                        if text.trim().is_empty() {
                            continue;
                        }
                        saw_text = true;
                        out.push(json!({"type": "input_text", "text": text}));
                    }
                    UserContentPart::Image { path, detail, .. } => {
                        let image_url = image_url_from_path(&path, detail.clone()).await?;
                        let mut item = json!({
                            "type": "input_image",
                            "image_url": image_url.url,
                        });
                        if let Some(detail) = detail {
                            item["detail"] = Value::String(image_detail_to_responses(detail));
                        }
                        out.push(item);
                    }
                    UserContentPart::File {
                        filename,
                        media_type,
                        data,
                    } => {
                        validate_pdf_file_data(&filename, &media_type, &data)?;
                        out.push(json!({
                            "type": "input_file",
                            "filename": filename,
                            "file_data": data,
                        }));
                    }
                }
            }
            if out.is_empty() {
                return Err(LlmError::InvalidRequest(
                    "user message content must not be empty".to_string(),
                ));
            }
            if !saw_text {
                out.insert(0, json!({"type": "input_text", "text": "."}));
            }
        }
    }
    Ok(out)
}

fn image_detail_to_responses(detail: ImageDetail) -> String {
    match detail {
        ImageDetail::Auto => "auto",
        ImageDetail::Low => "low",
        ImageDetail::High => "high",
    }
    .to_string()
}

fn to_responses_tool(tool: ToolDefinition) -> Value {
    let tool = to_wire_tool_definition(tool);
    let mut value = json!({
        "type": "function",
        "name": tool.name,
        "parameters": tool
            .parameters
            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
    });
    if let Some(description) = tool.description {
        value["description"] = Value::String(description);
    }
    if let Some(strict) = tool.strict {
        value["strict"] = Value::Bool(strict);
    }
    value
}

fn to_responses_tool_choice(choice: ToolChoice) -> Value {
    match to_wire_tool_choice(&choice) {
        ToolChoice::None => Value::String("none".to_string()),
        ToolChoice::Auto => Value::String("auto".to_string()),
        ToolChoice::Required => Value::String("required".to_string()),
        ToolChoice::Named { name } => json!({"type": "function", "name": name}),
    }
}

fn to_wire_responses_output_item(item: &Value) -> Value {
    let mut item = item.clone();
    if item.get("type").and_then(Value::as_str) == Some("function_call") {
        if let Some(name) = item.get("name").and_then(Value::as_str) {
            let wire_name = to_wire_tool_name(name);
            if wire_name != name {
                item["name"] = Value::String(wire_name.to_string());
            }
        }
    }
    item
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiResponse {
    id: String,
    #[serde(default)]
    created_at: Option<u32>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Vec<Value>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
    #[serde(default)]
    incomplete_details: Option<ResponseIncompleteDetails>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseIncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
    #[serde(default)]
    input_tokens_details: Option<ResponseInputTokenDetails>,
    #[serde(default)]
    x_details: Vec<ResponseUsageDetail>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct ResponseInputTokenDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct ResponseUsageDetail {
    #[serde(default)]
    prompt_tokens_details: Option<ResponseInputTokenDetails>,
}

impl ResponsesUsage {
    fn into_token_usage(self) -> TokenUsage {
        let top_cached = self
            .input_tokens_details
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0);
        let detail_cached = self
            .x_details
            .iter()
            .filter_map(|d| d.prompt_tokens_details.and_then(|p| p.cached_tokens))
            .max()
            .unwrap_or(0);
        let cached = top_cached.max(detail_cached);
        TokenUsage {
            prompt_tokens: self.input_tokens,
            completion_tokens: self.output_tokens,
            total_tokens: self.total_tokens,
            cached_tokens: (cached > 0).then_some(cached),
        }
    }
}

fn chat_response_from_openai_responses(
    resp: OpenAiResponse,
    fallback_model: &str,
) -> Result<ChatResponse, LlmError> {
    if let Some(err) = response_status_error(&resp) {
        return Err(err);
    }

    let output = resp.output;
    let (content, reasoning_content, tool_calls) = extract_output(&output);
    let usage = resp.usage.map(ResponsesUsage::into_token_usage);
    let finish_reason = finish_reason_from_response(
        resp.status.as_deref(),
        resp.incomplete_details.as_ref(),
        !tool_calls.is_empty(),
    );
    let metadata =
        (!output.is_empty()).then_some(ProviderMessageMetadata::OpenAiResponses { output });

    Ok(ChatResponse {
        id: resp.id,
        created: resp.created_at.unwrap_or(0),
        model: resp.model.unwrap_or_else(|| fallback_model.to_string()),
        message: Message::Assistant {
            content,
            reasoning_content: if tool_calls.is_empty() {
                None
            } else {
                reasoning_content
            },
            tool_calls,
            usage,
            provider_metadata: metadata,
        },
        finish_reason,
        usage,
    })
}

fn extract_output(output: &[Value]) -> (Option<String>, Option<String>, Vec<ToolCall>) {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();

    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => extract_message_item_text(item, &mut content),
            Some("output_text") => append_string_field(item, "text", &mut content),
            Some("function_call") => tool_calls.push(tool_call_from_response_item(item)),
            Some("reasoning") => extract_reasoning_text(item, &mut reasoning),
            Some("refusal") => append_string_field(item, "refusal", &mut content),
            _ => {}
        }
    }

    (
        (!content.is_empty()).then_some(content),
        (!reasoning.is_empty()).then_some(reasoning),
        tool_calls,
    )
}

fn extract_message_item_text(item: &Value, out: &mut String) {
    let Some(parts) = item.get("content").and_then(Value::as_array) else {
        return;
    };
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("output_text") | Some("input_text") => append_string_field(part, "text", out),
            Some("refusal") => append_string_field(part, "refusal", out),
            _ => append_string_field(part, "text", out),
        }
    }
}

fn extract_reasoning_text(item: &Value, out: &mut String) {
    append_string_field(item, "text", out);
    append_string_field(item, "summary_text", out);
    if let Some(summary) = item.get("summary").and_then(Value::as_array) {
        for part in summary {
            append_string_field(part, "text", out);
            append_string_field(part, "summary_text", out);
        }
    }
    if let Some(parts) = item.get("content").and_then(Value::as_array) {
        for part in parts {
            append_string_field(part, "text", out);
            append_string_field(part, "summary_text", out);
        }
    }
}

fn append_string_field(item: &Value, key: &str, out: &mut String) {
    let Some(text) = item.get(key).and_then(Value::as_str) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(text);
}

fn tool_call_from_response_item(item: &Value) -> ToolCall {
    ToolCall {
        id: item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: item
            .get("name")
            .and_then(Value::as_str)
            .map(to_internal_tool_name)
            .unwrap_or("unknown")
            .to_string(),
        arguments: item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_ARGUMENTS)
            .to_string(),
    }
}

fn finish_reason_from_response(
    status: Option<&str>,
    incomplete: Option<&ResponseIncompleteDetails>,
    has_tool_calls: bool,
) -> Option<FinishReason> {
    if has_tool_calls {
        return Some(FinishReason::ToolCalls);
    }

    match status {
        Some("completed") => Some(FinishReason::Stop),
        Some("incomplete") => match incomplete.and_then(|d| d.reason.as_deref()) {
            Some("max_output_tokens") | Some("max_tokens") => Some(FinishReason::Length),
            Some("content_filter") => Some(FinishReason::ContentFilter),
            Some(other) => Some(FinishReason::Other(other.to_string())),
            None => Some(FinishReason::Length),
        },
        Some("failed") => Some(FinishReason::Other("failed".to_string())),
        Some(other) => Some(FinishReason::Other(other.to_string())),
        None => None,
    }
}

fn response_status_error(resp: &OpenAiResponse) -> Option<LlmError> {
    if resp.status.as_deref() != Some("failed") {
        return None;
    }
    let body = match resp.error.as_ref() {
        Some(error) => json!({
            "error": {
                "message": error.message.clone().unwrap_or_else(|| "OpenAI response failed".to_string()),
                "type": error.r#type,
                "code": error.code,
            }
        })
        .to_string(),
        None => json!({"error": {"message": "OpenAI response failed"}}).to_string(),
    };
    Some(LlmError::Api {
        status: reqwest::StatusCode::BAD_REQUEST,
        body,
    })
}

#[derive(Debug)]
enum StreamEventAction {
    None,
    Chunk(ChatStreamChunk),
}

#[derive(Debug)]
struct ResponsesStreamState {
    id: String,
    created: u32,
    model: String,
    usage: Option<TokenUsage>,
    output_items: BTreeMap<u32, Value>,
    index_by_item_id: HashMap<String, u32>,
    function_arg_delta_seen: HashSet<u32>,
}

impl ResponsesStreamState {
    fn new(model: String) -> Self {
        Self {
            id: String::new(),
            created: 0,
            model,
            usage: None,
            output_items: BTreeMap::new(),
            index_by_item_id: HashMap::new(),
            function_arg_delta_seen: HashSet::new(),
        }
    }

    fn chunk(&self) -> ChatStreamChunk {
        ChatStreamChunk {
            id: self.id.clone(),
            created: self.created,
            model: self.model.clone(),
            content_delta: None,
            thinking_delta: None,
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
            provider_metadata: None,
        }
    }

    fn provider_metadata(&self) -> Option<ProviderMessageMetadata> {
        let output = self.output_items.values().cloned().collect::<Vec<_>>();
        (!output.is_empty()).then_some(ProviderMessageMetadata::OpenAiResponses { output })
    }

    fn absorb_response_value(&mut self, response: &Value) {
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            self.id = id.to_string();
        }
        if let Some(created) = response
            .get("created_at")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
        {
            self.created = created;
        }
        if let Some(model) = response.get("model").and_then(Value::as_str) {
            self.model = model.to_string();
        }
        if let Some(output) = response.get("output").and_then(Value::as_array) {
            self.output_items.clear();
            self.index_by_item_id.clear();
            for (index, item) in output.iter().cloned().enumerate() {
                let index = index as u32;
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    self.index_by_item_id.insert(id.to_string(), index);
                }
                self.output_items.insert(index, item);
            }
        }
    }

    fn remember_output_item(&mut self, index: u32, item: Value) {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            self.index_by_item_id.insert(id.to_string(), index);
        }
        self.output_items.insert(index, item);
    }

    fn index_from_event(&self, event: &Value) -> Option<u32> {
        event
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .or_else(|| {
                event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .and_then(|id| self.index_by_item_id.get(id).copied())
            })
    }
}

fn handle_stream_event(
    state: &mut ResponsesStreamState,
    event: &str,
    data: &str,
) -> Result<StreamEventAction, LlmError> {
    let value: Value = serde_json::from_str(data)?;
    let event_name = value
        .get("type")
        .and_then(Value::as_str)
        .filter(|ty| !ty.is_empty())
        .unwrap_or(event);
    match event_name {
        "response.created" | "response.in_progress" => {
            if let Some(response) = value.get("response") {
                state.absorb_response_value(response);
            }
            Ok(StreamEventAction::None)
        }
        "response.output_item.added" => {
            let Some(index) = state.index_from_event(&value) else {
                return Ok(StreamEventAction::None);
            };
            let Some(item) = value.get("item").cloned() else {
                return Ok(StreamEventAction::None);
            };
            state.remember_output_item(index, item.clone());
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                let mut chunk = state.chunk();
                chunk.tool_calls = vec![ToolCallDelta {
                    index,
                    id: item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .map(to_internal_tool_name)
                        .map(str::to_string),
                    arguments: None,
                }];
                return Ok(StreamEventAction::Chunk(chunk));
            }
            Ok(StreamEventAction::None)
        }
        "response.output_item.done" => {
            let Some(index) = state.index_from_event(&value) else {
                return Ok(StreamEventAction::None);
            };
            let Some(item) = value.get("item").cloned() else {
                return Ok(StreamEventAction::None);
            };
            let send_arguments = item.get("type").and_then(Value::as_str) == Some("function_call")
                && !state.function_arg_delta_seen.contains(&index)
                && item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .is_some_and(|args| !args.is_empty());
            state.remember_output_item(index, item.clone());
            if send_arguments {
                let mut chunk = state.chunk();
                chunk.tool_calls = vec![ToolCallDelta {
                    index,
                    id: item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .map(to_internal_tool_name)
                        .map(str::to_string),
                    arguments: item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }];
                return Ok(StreamEventAction::Chunk(chunk));
            }
            Ok(StreamEventAction::None)
        }
        "response.output_text.delta" => {
            let Some(delta) = value.get("delta").and_then(Value::as_str) else {
                return Ok(StreamEventAction::None);
            };
            let mut chunk = state.chunk();
            chunk.content_delta = Some(delta.to_string());
            Ok(StreamEventAction::Chunk(chunk))
        }
        "response.function_call_arguments.delta" => {
            let Some(index) = state.index_from_event(&value) else {
                return Ok(StreamEventAction::None);
            };
            let Some(delta) = value.get("delta").and_then(Value::as_str) else {
                return Ok(StreamEventAction::None);
            };
            state.function_arg_delta_seen.insert(index);
            append_function_arguments(state, index, delta);
            let mut chunk = state.chunk();
            chunk.tool_calls = vec![ToolCallDelta {
                index,
                id: None,
                name: None,
                arguments: Some(delta.to_string()),
            }];
            Ok(StreamEventAction::Chunk(chunk))
        }
        "response.function_call_arguments.done" => {
            let Some(index) = state.index_from_event(&value) else {
                return Ok(StreamEventAction::None);
            };
            let Some(arguments) = value.get("arguments").and_then(Value::as_str) else {
                return Ok(StreamEventAction::None);
            };
            set_function_arguments(state, index, arguments);
            if state.function_arg_delta_seen.contains(&index) {
                return Ok(StreamEventAction::None);
            }
            let mut chunk = state.chunk();
            chunk.tool_calls = vec![ToolCallDelta {
                index,
                id: None,
                name: None,
                arguments: Some(arguments.to_string()),
            }];
            Ok(StreamEventAction::Chunk(chunk))
        }
        "response.completed" | "response.incomplete" => {
            let response_value = value.get("response").cloned().unwrap_or(value);
            state.absorb_response_value(&response_value);
            let parsed: OpenAiResponse = serde_json::from_value(response_value)
                .map_err(|e| LlmError::OpenAI(OpenAIError::JSONDeserialize(e)))?;
            if let Some(err) = response_status_error(&parsed) {
                return Err(err);
            }
            let has_tool_calls = parsed
                .output
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
                || state
                    .output_items
                    .values()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"));
            if let Some(usage) = parsed.usage {
                state.usage = Some(usage.into_token_usage());
            }
            let mut chunk = state.chunk();
            chunk.finish_reason = finish_reason_from_response(
                parsed.status.as_deref(),
                parsed.incomplete_details.as_ref(),
                has_tool_calls,
            );
            chunk.usage = state.usage;
            chunk.provider_metadata = state.provider_metadata();
            Ok(StreamEventAction::Chunk(chunk))
        }
        "response.failed" => {
            let response_value = value.get("response").cloned().unwrap_or(value);
            let parsed: OpenAiResponse = serde_json::from_value(response_value)
                .map_err(|e| LlmError::OpenAI(OpenAIError::JSONDeserialize(e)))?;
            Err(
                response_status_error(&parsed).unwrap_or_else(|| LlmError::Api {
                    status: reqwest::StatusCode::BAD_REQUEST,
                    body: json!({"error": {"message": "OpenAI response failed"}}).to_string(),
                }),
            )
        }
        "error" => Err(stream_error(data)),
        _ if event.contains("reasoning") && event.ends_with(".delta") => {
            let delta = value
                .get("delta")
                .or_else(|| value.get("text"))
                .and_then(Value::as_str);
            if let Some(delta) = delta {
                let mut chunk = state.chunk();
                chunk.thinking_delta = Some(delta.to_string());
                Ok(StreamEventAction::Chunk(chunk))
            } else {
                Ok(StreamEventAction::None)
            }
        }
        _ => Ok(StreamEventAction::None),
    }
}

fn append_function_arguments(state: &mut ResponsesStreamState, index: u32, delta: &str) {
    let Some(item) = state.output_items.get_mut(&index) else {
        return;
    };
    let Some(obj) = item.as_object_mut() else {
        return;
    };
    let current = obj
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    obj.insert(
        "arguments".to_string(),
        Value::String(format!("{current}{delta}")),
    );
}

fn set_function_arguments(state: &mut ResponsesStreamState, index: u32, arguments: &str) {
    let Some(item) = state.output_items.get_mut(&index) else {
        return;
    };
    let Some(obj) = item.as_object_mut() else {
        return;
    };
    obj.insert(
        "arguments".to_string(),
        Value::String(arguments.to_string()),
    );
}

fn stream_error(data: &str) -> LlmError {
    #[derive(Debug, Deserialize)]
    struct ErrorEvent {
        #[serde(default)]
        error: Option<ResponseError>,
    }

    match serde_json::from_str::<ErrorEvent>(data) {
        Ok(event) => {
            let message = event
                .error
                .and_then(|err| err.message)
                .unwrap_or_else(|| data.to_string());
            LlmError::Stream(message)
        }
        Err(_) => LlmError::Stream(data.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message::User {
            content: UserMessageContent::Text(text.to_string()),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn maps_messages_tools_and_limits_to_responses_request() {
        let req = ChatRequest {
            messages: vec![
                Message::System {
                    content: "sys".to_string(),
                },
                Message::Developer {
                    content: "dev".to_string(),
                },
                user("hi"),
            ],
            tools: vec![ToolDefinition {
                name: "read".to_string(),
                description: Some("Read file".to_string()),
                parameters: Some(json!({"type":"object"})),
                strict: Some(true),
            }],
            tool_choice: ToolChoice::Named {
                name: "read".to_string(),
            },
            parallel_tool_calls: Some(false),
            temperature: Some(0.2),
            max_completion_tokens: Some(64),
        };

        let body = to_responses_request("gpt-test", req, false, None)
            .await
            .unwrap();
        assert_eq!(body["model"], json!("gpt-test"));
        assert_eq!(body["instructions"], json!("sys\n\ndev"));
        assert_eq!(body["input"][0]["role"], json!("user"));
        assert_eq!(body["input"][0]["content"][0]["type"], json!("input_text"));
        assert_eq!(body["tools"][0]["type"], json!("function"));
        assert_eq!(body["tools"][0]["strict"], json!(true));
        assert_eq!(
            body["tool_choice"],
            json!({"type":"function","name":"read"})
        );
        assert_eq!(body["parallel_tool_calls"], json!(false));
        assert_eq!(body["max_output_tokens"], json!(64));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn maps_pdf_file_data_to_responses_input_file() {
        let req = ChatRequest::new(vec![Message::User {
            content: UserMessageContent::Parts(vec![
                UserContentPart::Text {
                    text: "read".to_string(),
                },
                UserContentPart::File {
                    filename: "paper.pdf".to_string(),
                    media_type: "application/pdf".to_string(),
                    data: "JVBERi0=".to_string(),
                },
            ]),
        }]);

        let body = to_responses_request("gpt-test", req, false, None)
            .await
            .unwrap();

        assert_eq!(body["input"][0]["content"][1]["type"], json!("input_file"));
        assert_eq!(
            body["input"][0]["content"][1]["filename"],
            json!("paper.pdf")
        );
        assert_eq!(
            body["input"][0]["content"][1]["file_data"],
            json!("JVBERi0=")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn includes_prompt_cache_key_when_present() {
        let req = ChatRequest::new(vec![user("hi")]);

        let body = to_responses_request("gpt-test", req, false, Some(" session-key "))
            .await
            .unwrap();

        assert_eq!(body["prompt_cache_key"], json!("session-key"));
    }

    #[test]
    fn enables_dashscope_session_cache_only_for_dashscope_routes() {
        let dashscope = ProviderRoute {
            provider: "qwen_dashscope_responses".to_string(),
            api: crate::ProviderApi::OpenAiResponses,
            model: "qwen3.5-plus".to_string(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            api_key: None,
        };
        let openai = ProviderRoute {
            provider: "openai".to_string(),
            api: crate::ProviderApi::OpenAiResponses,
            model: "gpt-5".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: None,
        };

        assert!(should_enable_dashscope_session_cache(&dashscope));
        assert!(!should_enable_dashscope_session_cache(&openai));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn aliases_kiliax_web_search_function_tool() {
        let req = ChatRequest {
            messages: vec![user("search")],
            tools: vec![
                ToolDefinition {
                    name: "web_search".to_string(),
                    description: Some("Search the web".to_string()),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {"query": {"type": "string"}},
                        "required": ["query"],
                    })),
                    strict: Some(true),
                },
                ToolDefinition {
                    name: "write_file".to_string(),
                    description: Some("Write a file".to_string()),
                    parameters: Some(json!({"type": "object"})),
                    strict: Some(true),
                },
            ],
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: None,
            temperature: None,
            max_completion_tokens: None,
        };

        let body = to_responses_request("qwen3.5-plus", req, false, None)
            .await
            .unwrap();

        assert_eq!(body["tools"][0]["type"], json!("function"));
        assert_eq!(body["tools"][0]["name"], json!("kiliax_web_search"));
        assert!(body["tools"][0]["description"]
            .as_str()
            .unwrap()
            .contains("Kiliax `web_search` tool"));
        assert_eq!(body["tools"][1]["type"], json!("function"));
        assert_eq!(body["tools"][1]["name"], json!("write_file"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replays_stored_responses_output_items_before_tool_output() {
        let stored = json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "web_search",
            "arguments": "{\"query\":\"x\"}"
        });
        let req = ChatRequest::new(vec![
            user("read"),
            Message::Assistant {
                content: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "web_search".to_string(),
                    arguments: "{\"query\":\"x\"}".to_string(),
                }],
                usage: None,
                provider_metadata: Some(ProviderMessageMetadata::OpenAiResponses {
                    output: vec![stored.clone()],
                }),
            },
            Message::Tool {
                tool_call_id: "call_1".to_string(),
                content: "ok".to_string(),
            },
        ]);

        let body = to_responses_request("gpt-test", req, false, None)
            .await
            .unwrap();
        assert_eq!(body["input"][1]["name"], json!("kiliax_web_search"));
        assert_eq!(body["input"][2]["type"], json!("function_call_output"));
        assert_eq!(body["input"][2]["call_id"], json!("call_1"));
    }

    #[test]
    fn parses_non_streaming_text_tool_usage_and_metadata() {
        let raw = json!({
            "id": "resp_1",
            "created_at": 1,
            "model": "gpt-test",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "hello"}]
                },
                {
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "used read"}]
                },
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read",
                    "arguments": "{\"path\":\"README.md\"}"
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15,
                "input_tokens_details": {"cached_tokens": 4}
            }
        });
        let parsed: OpenAiResponse = serde_json::from_value(raw).unwrap();
        let resp = chat_response_from_openai_responses(parsed, "fallback").unwrap();
        assert_eq!(resp.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(resp.usage.unwrap().cached_tokens, Some(4));
        let Message::Assistant {
            content,
            reasoning_content,
            tool_calls,
            provider_metadata,
            ..
        } = resp.message
        else {
            panic!("expected assistant");
        };
        assert_eq!(content.as_deref(), Some("hello"));
        assert_eq!(reasoning_content.as_deref(), Some("used read"));
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].name, "read");
        assert!(provider_metadata.is_some());
    }

    #[test]
    fn parses_dashscope_cached_tokens_from_x_details() {
        let raw = json!({
            "id": "resp_1",
            "created_at": 1,
            "model": "qwen3.5-plus",
            "status": "completed",
            "output": [],
            "usage": {
                "input_tokens": 8072,
                "output_tokens": 265,
                "total_tokens": 8337,
                "input_tokens_details": {"cached_tokens": 0},
                "x_details": [
                    {
                        "prompt_tokens_details": {
                            "cache_creation_input_tokens": 18,
                            "cached_tokens": 8048
                        }
                    }
                ]
            }
        });

        let parsed: OpenAiResponse = serde_json::from_value(raw).unwrap();
        let resp = chat_response_from_openai_responses(parsed, "fallback").unwrap();

        assert_eq!(resp.usage.unwrap().cached_tokens, Some(8048));
    }

    #[test]
    fn maps_web_search_alias_back_to_internal_tool_name() {
        let item = json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "kiliax_web_search",
            "arguments": "{\"query\":\"x\"}"
        });

        let call = tool_call_from_response_item(&item);
        assert_eq!(call.name, "web_search");
    }

    #[test]
    fn streaming_maps_text_function_call_and_completed_usage() {
        let mut state = ResponsesStreamState::new("gpt-test".to_string());
        handle_stream_event(
            &mut state,
            "response.created",
            r#"{"response":{"id":"resp_1","created_at":2,"model":"gpt-test"}}"#,
        )
        .unwrap();
        let text = handle_stream_event(
            &mut state,
            "response.output_text.delta",
            r#"{"output_index":0,"delta":"hel"}"#,
        )
        .unwrap();
        let StreamEventAction::Chunk(text) = text else {
            panic!("expected chunk");
        };
        assert_eq!(text.content_delta.as_deref(), Some("hel"));

        let added = handle_stream_event(
            &mut state,
            "response.output_item.added",
            r#"{"output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":""}}"#,
        )
        .unwrap();
        let StreamEventAction::Chunk(added) = added else {
            panic!("expected chunk");
        };
        assert_eq!(added.tool_calls[0].id.as_deref(), Some("call_1"));

        let args = handle_stream_event(
            &mut state,
            "response.function_call_arguments.delta",
            r#"{"output_index":1,"delta":"{\"path\":\"README.md\"}"}"#,
        )
        .unwrap();
        let StreamEventAction::Chunk(args) = args else {
            panic!("expected chunk");
        };
        assert_eq!(
            args.tool_calls[0].arguments.as_deref(),
            Some("{\"path\":\"README.md\"}")
        );

        let completed = handle_stream_event(
            &mut state,
            "response.completed",
            r#"{"response":{"id":"resp_1","created_at":2,"model":"gpt-test","status":"completed","output":[{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":"{\"path\":\"README.md\"}"}],"usage":{"input_tokens":4,"output_tokens":3,"total_tokens":7}}}"#,
        )
        .unwrap();
        let StreamEventAction::Chunk(completed) = completed else {
            panic!("expected chunk");
        };
        assert_eq!(completed.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(completed.usage.unwrap().total_tokens, 7);
        assert!(completed.provider_metadata.is_some());
    }
}
