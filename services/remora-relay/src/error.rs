use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, RelayError>;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("authentication failed")]
    Unauthorized,
    #[error("resource not found")]
    NotFound,
    #[error("request conflicts with durable state")]
    Conflict,
    #[error("request is invalid: {0}")]
    Invalid(&'static str),
    #[error("request exceeded a configured limit")]
    LimitExceeded,
    #[error("resource is tombstoned")]
    Tombstoned,
    #[error("cursor reset is required")]
    ResetRequired,
    #[error("service is not configured for this operation")]
    NotConfigured,
    #[error("durable storage is unavailable")]
    Storage(#[source] rusqlite::Error),
    #[error("production database is unavailable")]
    Postgres(#[source] sqlx::Error),
    #[error("cryptographic operation failed")]
    Crypto,
    #[error("provider operation failed")]
    Provider,
    #[error("injected transaction fault")]
    InjectedFault,
    #[error("configuration is invalid: {0}")]
    Configuration(String),
    #[error("local I/O failed")]
    Io(#[source] std::io::Error),
}

impl From<rusqlite::Error> for RelayError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Storage(value)
    }
}

impl From<sqlx::Error> for RelayError {
    fn from(value: sqlx::Error) -> Self {
        Self::Postgres(value)
    }
}

impl From<std::io::Error> for RelayError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict => (StatusCode::CONFLICT, "conflict"),
            Self::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
            Self::LimitExceeded => (StatusCode::PAYLOAD_TOO_LARGE, "limit_exceeded"),
            Self::Tombstoned => (StatusCode::GONE, "tombstoned"),
            Self::ResetRequired => (StatusCode::CONFLICT, "reset_required"),
            Self::NotConfigured => (StatusCode::SERVICE_UNAVAILABLE, "not_configured"),
            Self::Storage(_)
            | Self::Postgres(_)
            | Self::Crypto
            | Self::Provider
            | Self::InjectedFault
            | Self::Io(_) => (StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
            Self::Configuration(_) => (StatusCode::INTERNAL_SERVER_ERROR, "configuration_error"),
        };
        (status, Json(ErrorBody { error })).into_response()
    }
}
