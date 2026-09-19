//! Request error mapping.
//!
//! Mirrors the Tauri host's `CommandError` categories so the frontend's existing
//! error handling (integrity conflicts in particular) keeps working unchanged.

use axum::Json;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde_json::json;
use tt_application::errors::ApplicationError;
use tt_domain::errors::DomainError;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    Conflict(String),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Unauthorized(String),

    #[error("{0}")]
    Cancelled(String),

    #[error("{0}")]
    TooManyRequests(String),

    #[error("{0}")]
    Internal(String),
}

impl ServerError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Cancelled(_) => StatusCode::REQUEST_TIMEOUT,
            Self::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// True when the failure is a chat-integrity rejection.
    ///
    /// Upstream SillyTavern answers these with `400 {"error":"integrity"}`
    /// (`src/endpoints/chats.js:490`) and the frontend keys its overwrite prompt
    /// off that exact shape, so the server must not reword it.
    pub fn is_integrity_conflict(&self) -> bool {
        matches!(self, Self::Conflict(message) | Self::BadRequest(message)
            if message.to_ascii_lowercase().contains("integrity"))
    }
}

impl From<ApplicationError> for ServerError {
    fn from(error: ApplicationError) -> Self {
        match error {
            ApplicationError::ValidationError(msg) => Self::BadRequest(msg),
            ApplicationError::Conflict(msg) => Self::Conflict(msg),
            ApplicationError::NotFound(msg) => Self::NotFound(msg),
            ApplicationError::Unauthorized(msg) | ApplicationError::PermissionDenied(msg) => {
                Self::Unauthorized(msg)
            }
            ApplicationError::RateLimited(msg) => Self::TooManyRequests(msg),
            ApplicationError::Cancelled(msg) => Self::Cancelled(msg),
            ApplicationError::UpstreamFailure(failure) => Self::Internal(failure.to_string()),
            ApplicationError::Transient(msg) | ApplicationError::InternalError(msg) => {
                Self::Internal(msg)
            }
        }
    }
}

impl From<DomainError> for ServerError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::NotFound(msg) => Self::NotFound(msg),
            DomainError::InvalidData(msg) => Self::BadRequest(msg),
            DomainError::Conflict(msg) => Self::Conflict(msg),
            DomainError::AuthenticationError(msg) => Self::Unauthorized(msg),
            DomainError::Cancelled(msg) => Self::Cancelled(msg),
            DomainError::RateLimited { message } => Self::TooManyRequests(message),
            DomainError::UpstreamFailure(failure) => Self::Internal(failure.to_string()),
            DomainError::Transient(msg) | DomainError::InternalError(msg) => Self::Internal(msg),
            DomainError::WorkspacePathIsDirectory { path } => {
                Self::BadRequest(format!("Workspace path is a directory: {path}"))
            }
            DomainError::WorkspaceWriteConflict { kind, .. } => {
                Self::BadRequest(format!("Workspace write conflict: {kind}"))
            }
        }
    }
}

impl From<serde_json::Error> for ServerError {
    fn from(error: serde_json::Error) -> Self {
        Self::BadRequest(format!("Invalid request payload: {error}"))
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, "Request failed");
        } else {
            tracing::debug!(error = %self, status = %status, "Request rejected");
        }

        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_rejection_is_detected_from_the_storage_error() {
        // `verify_integrity_match` fails with DomainError::InvalidData("integrity").
        let error = ServerError::from(DomainError::InvalidData("integrity".to_string()));
        assert!(error.is_integrity_conflict());
    }

    #[test]
    fn unrelated_bad_request_is_not_an_integrity_conflict() {
        let error = ServerError::BadRequest("Invalid chat payload".to_string());
        assert!(!error.is_integrity_conflict());
    }

    #[test]
    fn statuses_match_the_command_error_categories() {
        assert_eq!(
            ServerError::from(ApplicationError::NotFound("x".into())).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ServerError::from(ApplicationError::Conflict("x".into())).status(),
            StatusCode::CONFLICT
        );
    }
}
