use std::collections::BTreeMap;

use crate::login::LoginResponse;
use anyhow::Result;
use chrono::DateTime;
use console::Emoji;

const NO_ENTRY: Emoji = Emoji("⛔ ", "");
const LOCK: Emoji = Emoji("🔒 ", "");

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AuthSession {
    pub user_id: uuid::Uuid,
    access_token: String,
    pub access_token_expiry: DateTime<chrono::Utc>,
    refresh_session_id: uuid::Uuid,
    refresh_token: String,
    pub refresh_token_expiry: DateTime<chrono::Utc>,
    pub container_registry_auth: Option<BTreeMap<String, RegistryToken>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RegistryToken {
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
    pub token_expiry: Option<DateTime<chrono::Utc>>,
}

impl AuthSession {
    fn keyring_credential() -> Result<keyring::Entry> {
        Ok(keyring::Entry::new("unisrv-cli", "auth_session")?)
    }

    fn init() -> Option<Self> {
        AuthSession::keyring_credential().ok().and_then(|entry| {
            entry
                .get_password()
                .ok()
                .and_then(|password| serde_json::from_str(&password).ok())
        })
    }

    fn save(&self) -> Result<(), anyhow::Error> {
        let serialized = serde_json::to_string(self)?;
        AuthSession::keyring_credential()?.set_password(&serialized)?;
        log::debug!("Auth session saved to keyring");
        Ok(())
    }

    fn expired(&self) -> bool {
        let now: DateTime<chrono::Utc> = chrono::Utc::now();
        now > self.access_token_expiry && now > self.refresh_token_expiry
    }
    fn delete(&self) -> Result<(), anyhow::Error> {
        AuthSession::keyring_credential()?.delete_credential()?;
        log::debug!("Auth session deleted from keyring");
        Ok(())
    }

    async fn refresh(&mut self, client: &reqwest::Client) -> Result<(), anyhow::Error> {
        let now = chrono::Utc::now();
        if self.access_token_expiry > now {
            return Ok(());
        }

        if self.refresh_token_expiry < now {
            self.delete()?;
            return Err(anyhow::anyhow!(
                "{}Refresh token has expired. Please log in again.",
                NO_ENTRY
            ));
        }

        let response = client
            .post(format!("{DEFAULT_API_HOST}/auth/refresh"))
            .json(&serde_json::json!({
                "id": self.refresh_session_id,
                "token": self.refresh_token,
            }))
            .bearer_auth(&self.refresh_token)
            .send()
            .await?;

        if !response.status().is_success() {
            self.delete()?;
            let error_text = response.text().await?;

            // Try to parse as JSON error response with reason field
            if let Ok(error_response) = serde_json::from_str::<serde_json::Value>(&error_text) {
                if let Some(reason) = error_response.get("reason").and_then(|r| r.as_str()) {
                    return Err(anyhow::anyhow!(
                        "{}{}",
                        NO_ENTRY,
                        console::style(format!(
                            "Failed to refresh tokens: {reason}. Please login again."
                        ))
                        .red()
                    ));
                }
            }

            return Err(anyhow::anyhow!(
                "{}Failed to refresh tokens. You need to log in again.",
                NO_ENTRY
            ));
        }

        let response: LoginResponse = response.json().await?;

        self.access_token = response.token;
        self.access_token_expiry = response.expires_at;
        self.refresh_session_id = response.refresh_session_id;
        self.refresh_token = response.refresh_token;
        self.refresh_token_expiry = response.refresh_expires_at;

        log::debug!("Auth session refreshed successfully");

        self.save()?;
        Ok(())
    }
}

pub struct CliConfig {
    api_host: String,
    use_https: bool,

    auth_session: Option<AuthSession>,
}

const DEFAULT_API_HOST: &str = if cfg!(debug_assertions) {
    "http://localhost:8080"
} else {
    "https://api.unisrv.io"
};

impl CliConfig {
    pub fn init() -> Self {
        let mut api_host =
            std::env::var("API_HOST").unwrap_or_else(|_| DEFAULT_API_HOST.to_string());
        let mut use_https = true;
        if api_host.starts_with("http://") {
            api_host = api_host.trim_start_matches("http://").to_string();
            use_https = false;
        } else if api_host.starts_with("https://") {
            api_host = api_host.trim_start_matches("https://").to_string();
            use_https = true;
        }
        log::debug!("Using API host: {api_host}");

        CliConfig {
            api_host,
            use_https,
            auth_session: AuthSession::init(),
        }
    }

    pub fn url(&self, path: &str) -> String {
        let scheme = if self.use_https { "https" } else { "http" };
        format!("{}://{}{}", scheme, self.api_host, path)
    }

    pub fn ws_url(&self, path: &str) -> String {
        let scheme = if self.use_https { "wss" } else { "ws" };
        format!("{}://{}{}", scheme, self.api_host, path)
    }

    pub fn ensure_auth(&self) -> Result<(), anyhow::Error> {
        let program = std::env::args().nth(0).unwrap_or("<program>".to_string());
        let login_command = console::style(format!("{program} login")).bold();

        if let Some(auth_session) = &self.auth_session {
            if auth_session.expired() {
                return Err(anyhow::anyhow!(
                    "{}Authentication session expired. Please log in again with {}.",
                    NO_ENTRY,
                    login_command
                ));
            }
        } else {
            return Err(anyhow::anyhow!(
                "{}No authentication session found. Please log in with {}.",
                LOCK,
                login_command
            ));
        }
        Ok(())
    }

    pub fn save_auth_from_login(&mut self, response: LoginResponse) -> Result<(), anyhow::Error> {
        let auth_session = AuthSession {
            user_id: response.user_id,
            access_token: response.token,
            access_token_expiry: response.expires_at,
            refresh_session_id: response.refresh_session_id,
            refresh_token: response.refresh_token,
            refresh_token_expiry: response.refresh_expires_at,
            container_registry_auth: None,
        };
        auth_session.save()?;
        self.auth_session = Some(auth_session);
        Ok(())
    }

    /// Save container registry authentication
    pub fn save_registry_auth(
        &mut self,
        registry: &str,
        username: Option<String>,
        password: Option<String>,
        token: Option<String>,
        token_expiry: Option<DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let auth_session = self
            .auth_session
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No authentication session found"))?;

        let registry_token = RegistryToken {
            username,
            password,
            token,
            token_expiry,
        };

        if auth_session.container_registry_auth.is_none() {
            auth_session.container_registry_auth = Some(BTreeMap::new());
        }

        auth_session
            .container_registry_auth
            .as_mut()
            .unwrap()
            .insert(registry.to_string(), registry_token);

        auth_session.save()?;
        log::debug!("Saved registry auth for {}", registry);
        Ok(())
    }

    pub async fn token(&mut self, client: &reqwest::Client) -> Result<String, anyhow::Error> {
        self.ensure_auth()?;
        let auth_session = self.auth_session.as_mut().unwrap();
        auth_session.refresh(client).await?;
        Ok(auth_session.access_token.clone())
    }

    pub fn auth_session(&self) -> Option<&AuthSession> {
        self.auth_session.as_ref()
    }
}
