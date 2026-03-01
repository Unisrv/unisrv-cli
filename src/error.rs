use anyhow::{Result, anyhow};
use console::{Emoji, style};
use reqwest::Response;
use serde::Deserialize;

static WARNING: Emoji = Emoji("⚠️  ", "");
static ERROR: Emoji = Emoji("❌ ", "");
static NO_ENTRY: Emoji = Emoji("⛔ ", "");

#[derive(Deserialize)]
struct ErrorResponse {
    reason: String,
}

/// Format a non-success HTTP response into a descriptive error.
async fn format_http_error(response: Response, operation: &str) -> anyhow::Error {
    let status = response.status();

    match status.as_u16() {
        400..=499 => {
            if let Ok(error_body) = response.text().await {
                if let Ok(error_response) = serde_json::from_str::<ErrorResponse>(&error_body) {
                    return anyhow!(
                        "{}{}: {}",
                        NO_ENTRY,
                        operation,
                        style(error_response.reason).red()
                    );
                }

                return anyhow!(
                    "{}Failed to {}: client error ({}): {}",
                    ERROR,
                    operation,
                    status.as_u16(),
                    error_body
                );
            }

            anyhow!(
                "{}Failed to {}: {} - {}",
                ERROR,
                operation,
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown error")
            )
        }
        503 => {
            if let Ok(error_body) = response.text().await {
                if let Ok(error_response) = serde_json::from_str::<ErrorResponse>(&error_body) {
                    return anyhow!(
                        "{}{}",
                        WARNING,
                        style(format!(
                            "Service temporarily unavailable: {}",
                            error_response.reason
                        ))
                        .yellow()
                    );
                }

                return anyhow!("{}Service temporarily unavailable: {}", WARNING, error_body);
            }

            anyhow!("{}Service temporarily unavailable", WARNING)
        }
        _ => {
            let error_text = response.text().await.unwrap_or_default();
            anyhow!("Failed to {}: {} - {}", operation, status, error_text)
        }
    }
}

/// Check that an HTTP response was successful, returning the response on success
/// or a descriptive error on failure. This eliminates the need for `unreachable!()`
/// after error handling.
pub async fn check_response(response: Response, operation: &str) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    Err(format_http_error(response, operation).await)
}

/// Handle an HTTP error response by returning Err with a descriptive message.
/// For code that doesn't need the response body on success, prefer `check_response`.
pub async fn handle_http_error(response: Response, operation: &str) -> Result<()> {
    if response.status().is_success() {
        return Ok(());
    }
    Err(format_http_error(response, operation).await)
}
