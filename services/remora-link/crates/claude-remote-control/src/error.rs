use std::time::Duration;

use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteControlError {
    #[error("remote-control URL error: {0}")]
    Url(#[from] url::ParseError),
    #[error("remote-control HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("remote-control websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("remote-control JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("remote-control IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("remote-control base64 error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("remote-control API error: {0}")]
    Api(#[from] BridgeApiError),
    #[error("remote-control protocol error: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct BridgeApiError {
    pub status: StatusCode,
    pub kind: BridgeApiErrorKind,
    pub message: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeApiErrorKind {
    Unauthorized,
    ExpiredCookie,
    NotFound,
    Gone,
    RateLimited { retry_after: Option<Duration> },
    Fatal,
}

impl BridgeApiError {
    pub fn from_response_parts(
        status: StatusCode,
        cookie_header: Option<&str>,
        retry_after_header: Option<&str>,
        body: String,
    ) -> Self {
        let error_type = error_type_from_body(&body);
        let response_message = response_message_from_body(&body);
        let kind = if status == StatusCode::UNAUTHORIZED {
            BridgeApiErrorKind::Unauthorized
        } else if status == StatusCode::FORBIDDEN
            && (error_type
                .as_deref()
                .map(error_type_is_expired)
                .unwrap_or(false)
                || cookie_header
                    .map(|cookie| cookie.to_ascii_lowercase().contains("expired"))
                    .unwrap_or(false))
        {
            BridgeApiErrorKind::ExpiredCookie
        } else if status == StatusCode::NOT_FOUND {
            BridgeApiErrorKind::NotFound
        } else if status == StatusCode::GONE {
            BridgeApiErrorKind::Gone
        } else if status == StatusCode::TOO_MANY_REQUESTS {
            BridgeApiErrorKind::RateLimited {
                retry_after: parse_retry_after(retry_after_header),
            }
        } else {
            BridgeApiErrorKind::Fatal
        };
        let message = match &kind {
            BridgeApiErrorKind::Unauthorized => {
                "Remote Control request was unauthorized; refresh the Claude.ai OAuth token."
                    .to_string()
            }
            BridgeApiErrorKind::ExpiredCookie => {
                "Remote Control session has expired. Please restart with claude remote-control or /remote-control.".to_string()
            }
            BridgeApiErrorKind::NotFound => {
                response_message.unwrap_or_else(|| {
                    "Remote Control may not be available for this organization.".to_string()
                })
            }
            BridgeApiErrorKind::Gone => response_message.unwrap_or_else(|| {
                "Remote Control session has expired. Please restart with claude remote-control or /remote-control.".to_string()
            }),
            BridgeApiErrorKind::RateLimited { retry_after } => match retry_after {
                Some(delay) => format!(
                    "Remote Control request was rate limited; retry after {}s.",
                    delay.as_secs()
                ),
                None => "Remote Control request was rate limited.".to_string(),
            },
            BridgeApiErrorKind::Fatal => {
                if let Some(message) = response_message {
                    format!("Remote Control request failed with HTTP {status}: {message}")
                } else {
                    format!("Remote Control request failed with HTTP {status}")
                }
            }
        };
        Self {
            status,
            kind,
            message,
            body,
        }
    }
}

fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    value?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

fn error_type_is_expired(value: &str) -> bool {
    value.contains("expired") || value.contains("lifetime")
}

fn error_type_from_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("error")
        .and_then(|error| error.get("type"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn response_message_from_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .or_else(|| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_expired_cookie_403_to_session_expired() {
        let err = BridgeApiError::from_response_parts(
            StatusCode::FORBIDDEN,
            Some("session=expired"),
            None,
            String::new(),
        );
        assert_eq!(err.kind, BridgeApiErrorKind::ExpiredCookie);
        assert!(err.message.contains("session has expired"));
    }

    #[test]
    fn maps_retry_after_seconds() {
        let err = BridgeApiError::from_response_parts(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            Some("7"),
            String::new(),
        );
        assert_eq!(
            err.kind,
            BridgeApiErrorKind::RateLimited {
                retry_after: Some(Duration::from_secs(7))
            }
        );
    }
}
