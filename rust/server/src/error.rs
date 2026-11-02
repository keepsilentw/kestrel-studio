use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Application error with the HTTP status the Nest version produced, so the
/// existing client keeps working: 401 for a missing session on JSON endpoints,
/// 409 for a second turn on a busy conversation, 403 for a visibility miss.
#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    Unavailable(String),
    Internal(anyhow::Error),
}

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::BadRequest(message)
            | Self::Unauthorized(message)
            | Self::Forbidden(message)
            | Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Unavailable(message) => message.clone(),
            // Never the provider's own text: the client gets a sentence, while
            // `describe` keeps the detail for logs, tool results and task rows.
            Self::Internal(_) => "服务器内部错误".to_string(),
        }
    }

    /// The full detail of a failure, for anything a developer or the model reads
    /// rather than the browser: the provider's HTTP status and body, the IO error.
    pub fn describe(&self) -> String {
        match self {
            Self::Internal(error) => format!("{error:#}"),
            other => other.message(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized(message.into())
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable(message.into())
    }
    pub fn internal(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for AppError {}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(anyhow::anyhow!(error))
    }
}

impl From<anyhow::Error> for AppError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

/// Same body shape as Nest's default exception response, which the client reads.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if let Self::Internal(error) = &self {
            tracing::error!(%error, "request failed");
        }
        let body = json!({ "statusCode": status.as_u16(), "message": self.message() });
        (status, axum::Json(body)).into_response()
    }
}

/// Turns a failure into text a model can read. Shared by the turn loop and the
/// tool dispatcher: an abort is a normal outcome here (the 120s tool timeout, or
/// the client closing the connection), so it gets a sentence rather than the
/// runtime's own message.
pub fn describe_error(error: &AppError) -> String {
    let text = error.describe();
    if text.contains("aborted") || text.contains("cancelled") {
        return "请求已取消。".to_string();
    }
    text
}
