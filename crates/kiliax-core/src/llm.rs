use async_openai::{
    config::Config as OpenAIConfigTrait,
    error::OpenAIError,
    types::{
        ChatChoice, ChatCompletionMessageToolCall, ChatCompletionNamedToolChoice,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestDeveloperMessage, ChatCompletionRequestDeveloperMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessage,
        ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, ChatCompletionTool, ChatCompletionToolChoiceOption,
        ChatCompletionToolType, CompletionUsage, CreateChatCompletionRequestArgs,
        CreateChatCompletionResponse, FinishReason, FunctionCall, FunctionName, FunctionObject,
    },
    Client,
};
use reqwest::header::{HeaderMap, AUTHORIZATION};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ConfigError, ResolvedModel};

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("missing model id (provide explicitly or set `default_model` in config)")]
    MissingModel,

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    OpenAI(#[from] OpenAIError),

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
        let messages = req
            .messages
            .iter()
            .map(to_openai_message)
            .collect::<Result<Vec<_>, LlmError>>()?;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    Developer { content: String },
    System { content: String },
    User { content: String },
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

fn to_openai_message(msg: &Message) -> Result<ChatCompletionRequestMessage, LlmError> {
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
        Message::User { content } => ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Text(content.clone()),
            name: None,
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_message_roundtrip_builds_openai_message() {
        let msg = Message::Tool {
            tool_call_id: "call_123".to_string(),
            content: "{\"ok\":true}".to_string(),
        };
        let openai = to_openai_message(&msg).unwrap();
        let ChatCompletionRequestMessage::Tool(t) = openai else {
            panic!("expected tool message");
        };
        assert_eq!(t.tool_call_id, "call_123");
    }

    #[test]
    fn assistant_message_includes_tool_calls() {
        let msg = Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            }],
        };
        let openai = to_openai_message(&msg).unwrap();
        let ChatCompletionRequestMessage::Assistant(a) = openai else {
            panic!("expected assistant message");
        };
        assert!(a.content.is_none());
        assert_eq!(a.tool_calls.as_ref().unwrap().len(), 1);
    }
}
