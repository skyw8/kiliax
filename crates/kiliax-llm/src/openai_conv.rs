use async_openai::{
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
        ChatCompletionTool, ChatCompletionToolChoiceOption, ChatCompletionToolType, FunctionCall,
        FunctionName, FunctionObject, ImageDetail as OpenAIImageDetail, ImageUrl,
    },
};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::types::{
    ImageDetail, Message, ToolChoice, ToolDefinition, UserContentPart, UserMessageContent,
};

use super::{
    tool_names::{to_wire_tool_choice, to_wire_tool_definition, to_wire_tool_name},
    LlmError,
};

pub(super) fn to_openai_tool(tool: ToolDefinition) -> ChatCompletionTool {
    let tool = to_wire_tool_definition(tool);
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

pub(super) fn to_openai_tool_choice(choice: &ToolChoice) -> ChatCompletionToolChoiceOption {
    let choice = to_wire_tool_choice(choice);
    match choice {
        ToolChoice::None => ChatCompletionToolChoiceOption::None,
        ToolChoice::Auto => ChatCompletionToolChoiceOption::Auto,
        ToolChoice::Required => ChatCompletionToolChoiceOption::Required,
        ToolChoice::Named { name } => {
            ChatCompletionToolChoiceOption::Named(ChatCompletionNamedToolChoice {
                r#type: ChatCompletionToolType::Function,
                function: FunctionName { name },
            })
        }
    }
}

pub(super) async fn to_openai_message(
    msg: &Message,
) -> Result<ChatCompletionRequestMessage, LlmError> {
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
                                name: to_wire_tool_name(&c.name).to_string(),
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

pub(super) async fn to_openai_message_value(msg: &Message) -> Result<Value, LlmError> {
    match msg {
        Message::User { content } => Ok(json!({
            "role": "user",
            "content": to_openai_user_content_value(content).await?,
        })),
        _ => serde_json::to_value(to_openai_message(msg).await?).map_err(LlmError::Json),
    }
}

pub(super) async fn to_openai_user_content(
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
                    UserContentPart::Image { path, detail, .. } => {
                        let image_url = image_url_from_path(path, detail.clone()).await?;
                        out.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
                            ChatCompletionRequestMessageContentPartImage { image_url },
                        ));
                    }
                    UserContentPart::File { .. } => {
                        return Err(LlmError::OpenAI(OpenAIError::InvalidArgument(
                            "file content is only supported by the raw OpenAI chat request path"
                                .to_string(),
                        )));
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

pub(super) async fn to_openai_user_content_value(
    content: &UserMessageContent,
) -> Result<Value, LlmError> {
    match content {
        UserMessageContent::Text(text) => {
            if text.trim().is_empty() {
                return Err(LlmError::OpenAI(OpenAIError::InvalidArgument(
                    "user message text must not be empty".to_string(),
                )));
            }
            Ok(Value::String(text.clone()))
        }
        UserMessageContent::Parts(parts) => {
            let mut out = Vec::new();
            let mut saw_text = false;
            for part in parts {
                match part {
                    UserContentPart::Text { text } => {
                        if text.trim().is_empty() {
                            continue;
                        }
                        saw_text = true;
                        out.push(json!({"type": "text", "text": text}));
                    }
                    UserContentPart::Image { path, detail, .. } => {
                        let image_url = image_url_from_path(path, detail.clone()).await?;
                        out.push(json!({
                            "type": "image_url",
                            "image_url": image_url,
                        }));
                    }
                    UserContentPart::File {
                        filename,
                        media_type,
                        data,
                    } => {
                        validate_pdf_file_data(filename, media_type, data)?;
                        out.push(json!({
                            "type": "file",
                            "file": {
                                "filename": filename,
                                "file_data": data,
                            },
                        }));
                    }
                }
            }
            if out.is_empty() {
                return Err(LlmError::OpenAI(OpenAIError::InvalidArgument(
                    "user message content must not be empty".to_string(),
                )));
            }
            if !saw_text {
                out.insert(0, json!({"type": "text", "text": "."}));
            }
            Ok(Value::Array(out))
        }
    }
}

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

pub(super) fn validate_pdf_file_data(
    filename: &str,
    media_type: &str,
    data: &str,
) -> Result<(), LlmError> {
    if filename.trim().is_empty() {
        return Err(LlmError::InvalidRequest(
            "file filename must not be empty".to_string(),
        ));
    }
    if !media_type.eq_ignore_ascii_case("application/pdf") {
        return Err(LlmError::InvalidRequest(format!(
            "unsupported file media type `{media_type}`"
        )));
    }
    let data = data.trim();
    if data.is_empty() {
        return Err(LlmError::InvalidRequest(
            "file base64 data must not be empty".to_string(),
        ));
    }
    if data.starts_with("data:") {
        return Err(LlmError::InvalidRequest(
            "file data must be raw base64, not a data URL".to_string(),
        ));
    }
    Ok(())
}

pub(super) async fn image_url_from_path(
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
            detail: detail.map(to_openai_image_detail),
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
        detail: detail.map(to_openai_image_detail),
    })
}

fn to_openai_image_detail(detail: ImageDetail) -> OpenAIImageDetail {
    match detail {
        ImageDetail::Auto => OpenAIImageDetail::Auto,
        ImageDetail::Low => OpenAIImageDetail::Low,
        ImageDetail::High => OpenAIImageDetail::High,
    }
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
