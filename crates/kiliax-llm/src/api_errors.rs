use async_openai::error::OpenAIError;
use serde::Deserialize;

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
