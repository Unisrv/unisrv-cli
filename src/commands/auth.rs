use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use unisrv_api::ApiClient;

#[derive(Serialize)]
struct JsonToken {
    token: String,
    expires_at: DateTime<Utc>,
}

pub async fn token(client: &dyn ApiClient, json: bool) -> Result<()> {
    if !json {
        let token = client.access_token().await?;
        print!("{token}");
        return Ok(());
    }

    let session = client.auth_session().await?;
    let json_token = JsonToken {
        token: session.access_token().to_string(),
        expires_at: session.access_token_expiry,
    };
    println!("{}", serde_json::to_string(&json_token)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use unisrv_api::test_support::MockApiClient;
    use unisrv_api::ApiError;

    #[tokio::test]
    async fn token_returns_access_token() {
        let mock = MockApiClient::logged_in();
        let result = token(&mock, false).await;
        assert!(result.is_ok());

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.access_token_calls, 1);
        // Should not call auth_session for plain output
        assert_eq!(calls.auth_session_calls, 0);
    }

    #[tokio::test]
    async fn token_json_includes_expiry() {
        let mock = MockApiClient::logged_in();
        let result = token(&mock, true).await;
        assert!(result.is_ok());

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.access_token_calls, 0);
        assert_eq!(calls.auth_session_calls, 1);
    }

    #[tokio::test]
    async fn token_fails_when_not_logged_in() {
        let mock = MockApiClient::logged_out();
        let result = token(&mock, false).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.downcast_ref::<ApiError>().is_some());
    }

    #[tokio::test]
    async fn token_json_fails_when_not_logged_in() {
        let mock = MockApiClient::logged_out();
        let result = token(&mock, true).await;
        assert!(result.is_err());
    }
}
