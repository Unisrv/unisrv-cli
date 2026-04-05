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

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Other(e)
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;
