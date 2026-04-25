use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::error::{ApiError, extract_error_reason};

const KEYRING_SERVICE: &str = "unisrv-cli";
const KEYRING_USER: &str = "auth_session";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    user_id: Uuid,
    token: String,
    expires_at: DateTime<Utc>,
    refresh_session_id: Uuid,
    refresh_token: String,
    refresh_expires_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AuthSession {
    pub user_id: Uuid,
    access_token: String,
    pub access_token_expiry: DateTime<Utc>,
    refresh_session_id: Uuid,
    refresh_token: String,
    pub refresh_token_expiry: DateTime<Utc>,
}

impl std::fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthSession")
            .field("user_id", &self.user_id)
            .field("access_token_expiry", &self.access_token_expiry)
            .field("refresh_token_expiry", &self.refresh_token_expiry)
            .finish()
    }
}

impl AuthSession {
    /// Create a test session with the given token and validity duration.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_session(token: &str, valid_for: chrono::Duration) -> Self {
        let now = Utc::now();
        AuthSession {
            user_id: Uuid::new_v4(),
            access_token: token.to_string(),
            access_token_expiry: now + valid_for,
            refresh_session_id: Uuid::new_v4(),
            refresh_token: format!("{token}-refresh"),
            refresh_token_expiry: now + valid_for + valid_for,
        }
    }

    pub(crate) fn from_login_response(resp: LoginResponse) -> Self {
        AuthSession {
            user_id: resp.user_id,
            access_token: resp.token,
            access_token_expiry: resp.expires_at,
            refresh_session_id: resp.refresh_session_id,
            refresh_token: resp.refresh_token,
            refresh_token_expiry: resp.refresh_expires_at,
        }
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn expired(&self) -> bool {
        let now = Utc::now();
        now > self.access_token_expiry && now > self.refresh_token_expiry
    }

    pub fn access_token_expired(&self) -> bool {
        Utc::now() > self.access_token_expiry
    }

    pub async fn refresh(
        &mut self,
        client: &reqwest::Client,
        base_url: &str,
    ) -> Result<(), ApiError> {
        if self.access_token_expiry > Utc::now() {
            return Ok(());
        }
        self.force_refresh(client, base_url).await
    }

    pub async fn force_refresh(
        &mut self,
        client: &reqwest::Client,
        base_url: &str,
    ) -> Result<(), ApiError> {
        if self.refresh_token_expiry < Utc::now() {
            return Err(ApiError::AuthRequired(
                "Refresh token has expired. Please log in again.".into(),
            ));
        }

        let response = client
            .post(format!("{base_url}/auth/refresh"))
            .json(&serde_json::json!({
                "id": self.refresh_session_id,
                "token": self.refresh_token,
            }))
            .bearer_auth(&self.refresh_token)
            .send()
            .await?;

        if !response.status().is_success() {
            let reason = extract_error_reason(response).await;
            return Err(ApiError::AuthRequired(format!(
                "Failed to refresh tokens: {reason}. Please login again."
            )));
        }

        let resp: LoginResponse = response.json().await?;

        self.access_token = resp.token;
        self.access_token_expiry = resp.expires_at;
        self.refresh_session_id = resp.refresh_session_id;
        self.refresh_token = resp.refresh_token;
        self.refresh_token_expiry = resp.refresh_expires_at;

        tracing::debug!("Auth session refreshed successfully");
        Ok(())
    }
}

// ── Storage ──

fn auth_file_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".unisrv").join("auth.json"))
}

/// Persistent auth storage that tries keyring first, then falls back to a JSON file.
/// The keyring entry is created once and cached to avoid repeated OS prompts.
pub struct AuthStore {
    keyring_entry: Option<keyring::Entry>,
}

impl AuthStore {
    pub fn new() -> Self {
        let keyring_entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .inspect_err(|e| tracing::debug!("Keyring unavailable: {e}"))
            .ok();

        AuthStore { keyring_entry }
    }

    pub fn load(&self) -> Option<AuthSession> {
        self.load_from_keyring().or_else(|| self.load_from_file())
    }

    pub fn save(&self, session: &AuthSession) -> Result<(), anyhow::Error> {
        let serialized = serde_json::to_string(session)?;

        if let Some(entry) = &self.keyring_entry {
            match entry.set_password(&serialized) {
                Ok(()) => {
                    tracing::debug!("Auth session saved to keyring");
                    if let Some(path) = auth_file_path() {
                        let _ = std::fs::remove_file(&path);
                    }
                    return Ok(());
                }
                Err(e) => tracing::debug!("Failed to save to keyring, falling back to file: {e}"),
            }
        }

        self.save_to_file(&serialized)
    }

    pub fn delete(&self) {
        if let Some(entry) = &self.keyring_entry {
            if let Err(e) = entry.delete_credential() {
                tracing::debug!("Failed to delete from keyring: {e}");
            } else {
                tracing::debug!("Auth session deleted from keyring");
            }
        }

        if let Some(path) = auth_file_path() {
            if std::fs::remove_file(&path).is_ok() {
                tracing::debug!("Auth session deleted from file");
            }
        }
    }

    fn load_from_keyring(&self) -> Option<AuthSession> {
        let entry = self.keyring_entry.as_ref()?;
        let password = entry.get_password().ok()?;
        serde_json::from_str(&password).ok()
    }

    fn load_from_file(&self) -> Option<AuthSession> {
        let path = auth_file_path()?;
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save_to_file(&self, serialized: &str) -> Result<(), anyhow::Error> {
        let path = auth_file_path().ok_or_else(|| {
            anyhow::anyhow!("Could not determine home directory for auth storage")
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write with restrictive permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)?;
            std::io::Write::write_all(&mut file, serialized.as_bytes())?;
        }

        #[cfg(not(unix))]
        {
            std::fs::write(&path, serialized)?;
        }

        tracing::debug!("Auth session saved to file: {}", path.display());
        Ok(())
    }
}

impl Default for AuthStore {
    fn default() -> Self {
        Self::new()
    }
}
