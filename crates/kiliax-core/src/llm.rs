use std::pin::Pin;

use async_openai::{
    config::Config as OpenAIConfigTrait,
    error::OpenAIError,
    types::{
        ChatChoice, ChatCompletionMessageToolCall, ChatCompletionNamedToolChoice,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestDeveloperMessage, ChatCompletionRequestDeveloperMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestMessageContentPartImage,
        ChatCompletionRequestMessageContentPartText, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessage,
        ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart,
        ChatCompletionTool, ChatCompletionToolChoiceOption, ChatCompletionToolType, CompletionUsage,
        CreateChatCompletionRequestArgs, CreateChatCompletionResponse, FinishReason, FunctionCall,
        FunctionName, FunctionObject, ImageDetail, ImageUrl,
    },
    Client,
};
use base64::Engine as _;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use crate::config::{Config, ConfigError, ResolvedModel};

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
        let mut messages: Vec<ChatCompletionRequestMessage> = Vec::with_capacity(req.messages.len());
        for msg in &req.messages {
            messages.push(to_openai_message(msg).await?);
        }

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.route.model).messages(messages);

        if !req.tools.is_empty() {
            let tools: Vec<ChatCompletionTool> =
                req.tools.into_iter().map(to_openai_tool).collect();
            builder.tools(tools);
            builder.tool_choice(to_openai_tool_choice(&req.tool_choice));
        }

        if let Some(parallel_tool_calls) = req.parallel_tool_calls {
            builder.parallel_tool_calls(parallel_tool_calls);
        }

        if let Some(temperature) = req.temperature {
            builder.temperature(temperature);
        }

        if let Some(max_completion_tokens) = req.max_completion_tokens {
            builder.max_completion_tokens(max_completion_tokens);
        }

        let request = builder.build()?;

        let response: CreateChatCompletionResponse = self.client.chat().create(request).await?;
        chat_response_from_openai(response)
    }

    pub async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, LlmError> {
        let mut messages: Vec<ChatCompletionRequestMessage> = Vec::with_capacity(req.messages.len());
        for msg in &req.messages {
            messages.push(to_openai_message(msg).await?);
        }

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.route.model).messages(messages);

        if !req.tools.is_empty() {
            let tools: Vec<ChatCompletionTool> =
                req.tools.into_iter().map(to_openai_tool).collect();
            builder.tools(tools);
            builder.tool_choice(to_openai_tool_choice(&req.tool_choice));
        }

        if let Some(parallel_tool_calls) = req.parallel_tool_calls {
            builder.parallel_tool_calls(parallel_tool_calls);
        }

        if let Some(temperature) = req.temperature {
            builder.temperature(temperature);
        }

        if let Some(max_completion_tokens) = req.max_completion_tokens {
            builder.max_completion_tokens(max_completion_tokens);
        }

        let mut request = builder.build()?;
        request.stream = Some(true);

        let stream = self
            .client
            .chat()
            .create_stream_byot::<_, ByotCreateChatCompletionStreamResponse>(request)
            .await?;
        Ok(Box::pin(stream.map(|res| match res {
            Ok(chunk) => Ok(chat_stream_chunk_from_byot(chunk)),
            Err(err) => Err(err.into()),
        })))
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
    Text { text: String },
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
    Developer { content: String },
    System { content: String },
    User { content: UserMessageContent },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
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
            strict: tool.strict,
        },
    }
}

fn to_openai_tool_choice(choice: &ToolChoice) -> ChatCompletionToolChoiceOption {
    match choice {
        ToolChoice::None => ChatCompletionToolChoiceOption::None,
        ToolChoice::Auto => ChatCompletionToolChoiceOption::Auto,
        ToolChoice::Required => ChatCompletionToolChoiceOption::Required,
        ToolChoice::Named { name } => ChatCompletionToolChoiceOption::Named(
            ChatCompletionNamedToolChoice {
                r#type: ChatCompletionToolType::Function,
                function: FunctionName { name: name.clone() },
            },
        ),
    }
}

async fn to_openai_message(msg: &Message) -> Result<ChatCompletionRequestMessage, LlmError> {
    Ok(match msg {
        Message::Developer { content } => ChatCompletionRequestMessage::Developer(
            ChatCompletionRequestDeveloperMessage {
                content: ChatCompletionRequestDeveloperMessageContent::Text(content.clone()),
                name: None,
            },
        ),
        Message::System { content } => ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(content.clone()),
                name: None,
            },
        ),
        Message::User { content } => {
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: to_openai_user_content(content).await?,
                name: None,
            })
        }
        Message::Assistant { content, tool_calls } => {
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
        UserMessageContent::Text(text) => Ok(ChatCompletionRequestUserMessageContent::Text(text.clone())),
        UserMessageContent::Parts(parts) => {
            let mut out: Vec<ChatCompletionRequestUserMessageContentPart> = Vec::new();
            for part in parts {
                match part {
                    UserContentPart::Text { text } => out.push(
                        ChatCompletionRequestUserMessageContentPart::Text(
                            ChatCompletionRequestMessageContentPartText { text: text.clone() },
                        ),
                    ),
                    UserContentPart::Image { path, detail } => {
                        let image_url = image_url_from_path(path, detail.clone()).await?;
                        out.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
                            ChatCompletionRequestMessageContentPartImage { image_url },
                        ));
                    }
                }
            }
            if !out
                .iter()
                .any(|p| matches!(p, ChatCompletionRequestUserMessageContentPart::Text(_)))
            {
                out.insert(
                    0,
                    ChatCompletionRequestUserMessageContentPart::Text(
                        ChatCompletionRequestMessageContentPartText {
                            text: String::new(),
                        },
                    ),
                );
            }
            Ok(ChatCompletionRequestUserMessageContent::Array(out))
        }
    }
}

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

async fn image_url_from_path(path: &str, detail: Option<ImageDetail>) -> Result<ImageUrl, LlmError> {
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

fn chat_response_from_openai(resp: CreateChatCompletionResponse) -> Result<ChatResponse, LlmError> {
    let CreateChatCompletionResponse {
        id,
        created,
        model,
        choices,
        usage,
        ..
    } = resp;

    let ChatChoice {
        message,
        finish_reason,
        ..
    } = choices.into_iter().next().ok_or(LlmError::NoChoices)?;

    let tool_calls = message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|c| ToolCall {
            id: c.id,
            name: c.function.name,
            arguments: c.function.arguments,
        })
        .collect();

    Ok(ChatResponse {
        id,
        created,
        model,
        message: Message::Assistant {
            content: message.content,
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
}
