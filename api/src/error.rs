use std::fmt;

#[derive(Debug)]
pub enum ApiError {
    /// HTTP request failed
    Request(reqwest::Error),
    /// Server returned an error response
    Server { status: u16, reason: String },
    /// Authentication required (no session or expired)
    AuthRequired(String),
    /// Serialization/deserialization error
    Serialization(String),
    /// Other errors
    Other(anyhow::Error),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Request(e) => write!(f, "Request error: {e}"),
            ApiError::Server { status, reason } => write!(f, "Server error ({status}): {reason}"),
            ApiError::AuthRequired(msg) => write!(f, "Authentication required: {msg}"),
            ApiError::Serialization(msg) => write!(f, "Serialization error: {msg}"),
            ApiError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Request(e)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::Serialization(e.to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Other(e)
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

impl ApiError {
    pub fn not_logged_in() -> Self {
        ApiError::AuthRequired("Not logged in.".into())
    }
}

/// Extract a human-readable error reason from an HTTP error response body.
pub(crate) async fn extract_error_reason(resp: reqwest::Response) -> String {
    let text = resp.text().await.unwrap_or_default();
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(reason) = json.get("reason").and_then(|r| r.as_str()) {
            return reason.to_string();
        }
        if let Some(message) = json.get("message").and_then(|m| m.as_str()) {
            return message.to_string();
        }
    }
    if text.is_empty() {
        "Unknown error".to_string()
    } else {
        text
    }
}
