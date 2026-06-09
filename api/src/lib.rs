pub mod auth;
pub mod client;
pub mod error;
pub mod models;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use auth::{AuthSession, AuthStore};
pub use client::{API_HOST_ENV, ApiClient, DEFAULT_API_HOST, HttpApiClient};
pub use error::{ApiError, Result};

/// The unisrv config directory, `~/.unisrv` — the single home for the auth store,
/// remembered preferences, and any other per-user state. `None` if the home
/// directory can't be determined.
pub fn config_dir() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".unisrv"))
}
