use anyhow::Result;
use unisrv_api::ApiClient;
use yapp::PasswordReader;

pub async fn run(
    client: &dyn ApiClient,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<()> {
    let username = match username {
        Some(u) => u.to_string(),
        None => dialoguer::Input::new()
            .with_prompt("Username")
            .interact_text()?,
    };

    let password = match password {
        Some(p) => {
            tracing::warn!(
                "Passing password via CLI argument is insecure and may be visible in shell history"
            );
            p.to_string()
        }
        None => {
            let mut yapp = yapp::Yapp::new().with_echo_symbol('*');
            yapp.read_password_with_prompt("Password: ")?
        }
    };

    client.login(&username, &password).await?;
    println!("\u{1f512} Successfully logged in as user: {username}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use unisrv_api::test_support::MockApiClient;
    use unisrv_api::ApiError;

    #[tokio::test]
    async fn login_with_provided_credentials() {
        let mock = MockApiClient::logged_out();
        let result = run(&mock, Some("alice"), Some("secret")).await;
        assert!(result.is_ok());

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.login_calls.len(), 1);
        assert_eq!(calls.login_calls[0], ("alice".into(), "secret".into()));
    }

    #[tokio::test]
    async fn login_propagates_server_error() {
        let mock = MockApiClient::login_fails(ApiError::Server {
            status: 401,
            reason: "Invalid credentials".into(),
        });
        let result = run(&mock, Some("alice"), Some("wrong")).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("401"));
        assert!(err.to_string().contains("Invalid credentials"));
    }

    #[tokio::test]
    async fn login_propagates_auth_required_error() {
        let mock = MockApiClient::login_fails(ApiError::AuthRequired("Account locked".into()));
        let result = run(&mock, Some("alice"), Some("pass")).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Account locked"));
    }
}
