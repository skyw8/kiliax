use async_openai::error::OpenAIError;
use serde::Deserialize;
use serde_json::Value;

pub(super) async fn map_eventsource_error(err: reqwest_eventsource::Error) -> OpenAIError {
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

pub(super) async fn map_api_error_response(
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

pub(super) fn map_stream_error_payload(data: &str) -> Option<OpenAIError> {
    #[derive(Debug, Deserialize)]
    struct ErrorWrapper {
        error: Value,
    }

    #[derive(Debug, Deserialize)]
    struct ApiErrorLike {
        message: String,
        #[serde(default)]
        r#type: Option<String>,
        #[serde(default)]
        param: Option<String>,
        #[serde(default)]
        code: Option<String>,
    }

    let wrapped = serde_json::from_str::<ErrorWrapper>(data).ok()?;
    let mut error = match wrapped.error {
        Value::Object(obj) => {
            let raw = Value::Object(obj.clone());
            if let Ok(mut api) =
                serde_json::from_value::<async_openai::error::ApiError>(raw.clone())
            {
                api.message = format!("stream error: {}", api.message);
                return Some(OpenAIError::ApiError(api));
            }
            if let Ok(api) = serde_json::from_value::<ApiErrorLike>(raw.clone()) {
                async_openai::error::ApiError {
                    message: format!("stream error: {}", api.message),
                    r#type: api.r#type,
                    param: api.param,
                    code: api.code,
                }
            } else {
                async_openai::error::ApiError {
                    message: format!("stream error: {raw}"),
                    r#type: None,
                    param: None,
                    code: None,
                }
            }
        }
        Value::String(message) => async_openai::error::ApiError {
            message: format!("stream error: {message}"),
            r#type: None,
            param: None,
            code: None,
        },
        other => async_openai::error::ApiError {
            message: format!("stream error: {other}"),
            r#type: None,
            param: None,
            code: None,
        },
    };

    if error.message.trim().is_empty() {
        error.message = "stream error".to_string();
    }
    Some(OpenAIError::ApiError(error))
}
