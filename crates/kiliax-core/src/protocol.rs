use async_openai::types::{CompletionUsage, FinishReason, ImageDetail};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}

impl TokenUsage {
    pub fn from_completion_usage(usage: &CompletionUsage) -> Self {
        let cached = usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0) as u32;
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_tokens: (cached > 0).then_some(cached),
        }
    }
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
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
