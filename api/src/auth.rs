use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::error::ApiError;

const KEYRING_SERVICE: &str = "unisrv-cli";
const KEYRING_USER: &str = "auth_session";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoginResponse {
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
        self.refresh_inner(client, base_url, false).await
    }

    pub async fn force_refresh(
        &mut self,
        client: &reqwest::Client,
        base_url: &str,
    ) -> Result<(), ApiError> {
        self.refresh_inner(client, base_url, true).await
    }

    async fn refresh_inner(
        &mut self,
        client: &reqwest::Client,
        base_url: &str,
        force: bool,
    ) -> Result<(), ApiError> {
        let now = Utc::now();

        if !force && self.access_token_expiry > now {
            return Ok(());
        }

        if self.refresh_token_expiry < now {
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
            let error_text = response.text().await.unwrap_or_default();

            if let Ok(error_json) = serde_json::from_str::<serde_json::Value>(&error_text) {
                if let Some(reason) = error_json.get("reason").and_then(|r| r.as_str()) {
                    return Err(ApiError::AuthRequired(format!(
                        "Failed to refresh tokens: {reason}. Please login again."
                    )));
                }
            }

            return Err(ApiError::AuthRequired(
                "Failed to refresh tokens. Please log in again.".into(),
            ));
        }

        let resp: LoginResponse = response.json().await.map_err(|e| {
            ApiError::Serialization(format!("Failed to parse refresh response: {e}"))
        })?;

        self.access_token = resp.token;
        self.access_token_expiry = resp.expires_at;
        self.refresh_session_id = resp.refresh_session_id;
        self.refresh_token = resp.refresh_token;
        self.refresh_token_expiry = resp.refresh_expires_at;

        log::debug!("Auth session refreshed successfully");
        Ok(())
    }
}

// ── Storage ──

fn auth_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".unisrv")
        .join("auth.json")
}

/// Persistent auth storage that tries keyring first, then falls back to a JSON file.
/// The keyring entry is created once and cached to avoid repeated OS prompts.
pub struct AuthStore {
    keyring_entry: Option<keyring::Entry>,
}

impl AuthStore {
    pub fn new() -> Self {
        let keyring_entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .inspect_err(|e| log::debug!("Keyring unavailable: {e}"))
            .ok();

        AuthStore { keyring_entry }
    }

    pub fn load(&self) -> Option<AuthSession> {
        // Try keyring first
        if let Some(session) = self.load_from_keyring() {
            return Some(session);
        }

        // Fall back to file
        self.load_from_file()
    }

    pub fn save(&self, session: &AuthSession) -> Result<(), anyhow::Error> {
        let serialized = serde_json::to_string(session)?;

        // Try keyring first
        if let Some(entry) = &self.keyring_entry {
            match entry.set_password(&serialized) {
                Ok(()) => {
                    log::debug!("Auth session saved to keyring");
                    // Remove file if it exists since keyring is now authoritative
                    let path = auth_file_path();
                    if path.exists() {
                        let _ = std::fs::remove_file(&path);
                    }
                    return Ok(());
                }
                Err(e) => log::debug!("Failed to save to keyring, falling back to file: {e}"),
            }
        }

        // Fall back to file
        self.save_to_file(&serialized)
    }

    pub fn delete(&self) {
        // Try deleting from keyring
        if let Some(entry) = &self.keyring_entry {
            if let Err(e) = entry.delete_credential() {
                log::debug!("Failed to delete from keyring: {e}");
            } else {
                log::debug!("Auth session deleted from keyring");
            }
        }

        // Also delete file if present
        let path = auth_file_path();
        if path.exists() {
            let _ = std::fs::remove_file(&path);
            log::debug!("Auth session deleted from file");
        }
    }

    fn load_from_keyring(&self) -> Option<AuthSession> {
        let entry = self.keyring_entry.as_ref()?;
        let password = entry.get_password().ok()?;
        serde_json::from_str(&password).ok()
    }

    fn load_from_file(&self) -> Option<AuthSession> {
        let path = auth_file_path();
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save_to_file(&self, serialized: &str) -> Result<(), anyhow::Error> {
        let path = auth_file_path();
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

        log::debug!("Auth session saved to file: {}", path.display());
        Ok(())
    }
}

impl Default for AuthStore {
    fn default() -> Self {
        Self::new()
    }
}
