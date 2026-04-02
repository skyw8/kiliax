use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiErrorResponse {
    pub error: ApiErrorBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiErrorBody {
    pub code: ApiErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
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

fn error_chain(err: &dyn std::error::Error) -> Vec<String> {
    let mut out = Vec::new();
    out.push(err.to_string());
    let mut cur = err.source();
    while let Some(src) = cur {
        out.push(src.to_string());
        cur = src.source();
    }
    out
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

    pub fn with_detail_field(mut self, key: &str, value: serde_json::Value) -> Self {
        match self.details.take() {
            None => {
                self.details = Some(serde_json::json!({ key: value }));
            }
            Some(mut existing) => {
                if let Some(obj) = existing.as_object_mut() {
                    obj.insert(key.to_string(), value);
                    self.details = Some(existing);
                } else {
                    self.details = Some(serde_json::json!({ "details": existing, key: value }));
                }
            }
        }
        self
    }

    pub fn with_error_chain(self, err: &dyn std::error::Error) -> Self {
        self.with_detail_field("error_chain", serde_json::json!(error_chain(err)))
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

    pub fn internal_error<E>(err: E) -> Self
    where
        E: std::error::Error,
    {
        ApiError::internal(err.to_string()).with_error_chain(&err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let trace_id = kiliax_core::telemetry::spans::current_trace_id();
        if self.status.is_server_error() {
            tracing::error!(
                api_error.code = ?self.code,
                api_error.message = %self.message,
                api_error.details = ?self.details,
                trace_id = %trace_id.as_deref().unwrap_or(""),
                "api error"
            );
        } else {
            tracing::warn!(
                api_error.code = ?self.code,
                api_error.message = %self.message,
                api_error.details = ?self.details,
                trace_id = %trace_id.as_deref().unwrap_or(""),
                "api error"
            );
        }

        let body = ApiErrorResponse {
            error: ApiErrorBody {
                code: self.code,
                message: self.message,
                details: self.details,
            },
            trace_id,
        };
        (self.status, Json(body)).into_response()
    }
}
