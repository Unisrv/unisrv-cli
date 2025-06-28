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

pub async fn handle_http_error(response: Response, operation: &str) -> Result<()> {
    let status = response.status();

    if status.is_success() {
        return Ok(());
    }

    match status.as_u16() {
        400..=499 => {
            if let Ok(error_body) = response.text().await {
                if let Ok(error_response) = serde_json::from_str::<ErrorResponse>(&error_body) {
                    return Err(anyhow!(
                        "{}{}",
                        NO_ENTRY,
                        style(error_response.reason).red()
                    ));
                }

                return Err(anyhow!(
                    "{}Client error ({}): {}",
                    ERROR,
                    status.as_u16(),
                    error_body
                ));
            }

            Err(anyhow!(
                "{}Client error: {} - {}",
                ERROR,
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown error")
            ))
        }
        503 => {
            if let Ok(error_body) = response.text().await {
                if let Ok(error_response) = serde_json::from_str::<ErrorResponse>(&error_body) {
                    return Err(anyhow!(
                        "{}{}",
                        WARNING,
                        style(format!(
                            "Service temporarily unavailable: {}",
                            error_response.reason
                        ))
                        .yellow()
                    ));
                }

                return Err(anyhow!(
                    "{}Service temporarily unavailable: {}",
                    WARNING,
                    error_body
                ));
            }

            Err(anyhow!("{}Service temporarily unavailable", WARNING))
        }
        _ => {
            let error_text = response.text().await.unwrap_or_default();
            Err(anyhow!(
                "Failed to {}: {} - {}",
                operation,
                status,
                error_text
            ))
        }
    }
}
