use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorResponse {
    pub error: ApiErrorBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorBody {
    pub code: ApiErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    InvalidArgument,
    Unauthorized,
    #[allow(dead_code)]
    Forbidden,
    NotFound,
    #[allow(dead_code)]
    Conflict,
    #[allow(dead_code)]
    RateLimited,
    Internal,
    SessionNotLive,
    ModelNotSupported,
    AgentNotSupported,
    McpServerNotFound,
    RunNotCancellable,
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: ApiErrorCode,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:?})", self.message, self.code)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    pub fn new(status: StatusCode, code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ApiErrorCode::InvalidArgument, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ApiErrorCode::NotFound, message)
    }

    pub fn session_not_live(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, ApiErrorCode::SessionNotLive, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiErrorCode::Internal,
            message,
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ApiErrorResponse {
            error: ApiErrorBody {
                code: self.code,
                message: self.message,
                details: self.details,
            },
            trace_id: None,
        };
        (self.status, Json(body)).into_response()
    }
}
