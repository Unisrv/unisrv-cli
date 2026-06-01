use anyhow::{Result, anyhow, bail};
use chrono::NaiveDateTime;
use chrono_humanize::{Accuracy, HumanTime, Tense};
use comfy_table::{Attribute, Cell, ContentArrangement, Table, presets::UTF8_FULL};
use dialoguer::Confirm;
use std::io::Read;
use unisrv_api::ApiClient;
use unisrv_api::ApiError;
use unisrv_api::models::{
    CreateRegistryRequest, RegistryKind, RegistryResponse, UpdateRegistryRequest, UserpassConfig,
    UserpassSecret,
};
use uuid::Uuid;
use yapp::PasswordReader;

pub async fn add(
    client: &dyn ApiClient,
    hostname: &str,
    username: Option<&str>,
    password_stdin: bool,
    validate: bool,
) -> Result<()> {
    let username = resolve_username(username)?;
    let password = read_password(password_stdin)?;

    let req = CreateRegistryRequest {
        hostname: hostname.to_string(),
        kind: RegistryKind::Userpass,
        config: serde_json::to_value(UserpassConfig {
            username: username.clone(),
        })?,
        secret: serde_json::to_value(UserpassSecret { password })?,
    };

    match client.create_registry(req, validate).await {
        Ok(reg) => {
            if validate {
                println!("\u{2713} Added {}.", reg.hostname);
            } else {
                println!(
                    "\u{2713} Added {}. Skipped validation \u{2014} run `unisrv reg test {}` to verify.",
                    reg.hostname, reg.hostname
                );
            }
            Ok(())
        }
        Err(err) => Err(map_registry_write_error(err, hostname)),
    }
}

pub async fn update(
    client: &dyn ApiClient,
    hostname: &str,
    username: Option<&str>,
    password_stdin: bool,
    validate: bool,
) -> Result<()> {
    if username.is_none() && !password_stdin {
        bail!(
            "Specify --username and/or --password-stdin to indicate what to update."
        );
    }

    let id = resolve_registry_id(client, hostname).await?;

    let config = match username {
        Some(u) => Some(serde_json::to_value(UserpassConfig {
            username: u.to_string(),
        })?),
        None => None,
    };
    let secret = if password_stdin {
        let password = read_password(true)?;
        Some(serde_json::to_value(UserpassSecret { password })?)
    } else {
        None
    };

    let req = UpdateRegistryRequest { config, secret };

    match client.update_registry(id, req, validate).await {
        Ok(reg) => {
            if validate {
                println!("\u{2713} Updated {}.", reg.hostname);
            } else {
                println!(
                    "\u{2713} Updated {}. Skipped validation \u{2014} run `unisrv reg test {}` to verify.",
                    reg.hostname, reg.hostname
                );
            }
            Ok(())
        }
        Err(err) => Err(map_registry_write_error(err, hostname)),
    }
}

pub async fn delete(client: &dyn ApiClient, hostname: &str, yes: bool) -> Result<()> {
    delete_with_confirm(client, hostname, yes, prompt_delete_confirmation).await
}

fn prompt_delete_confirmation(hostname: &str) -> Result<bool> {
    Ok(Confirm::new()
        .with_prompt(format!("Delete registry credentials for {hostname}?"))
        .default(false)
        .interact()?)
}

async fn delete_with_confirm<F>(
    client: &dyn ApiClient,
    hostname: &str,
    yes: bool,
    confirm: F,
) -> Result<()>
where
    F: FnOnce(&str) -> Result<bool>,
{
    let id = resolve_registry_id(client, hostname).await?;

    if !yes && !confirm(hostname)? {
        println!("Aborted.");
        return Ok(());
    }

    client.delete_registry(id).await?;
    println!("\u{2713} Deleted {hostname}.");
    Ok(())
}

pub async fn list(client: &dyn ApiClient, json: bool) -> Result<()> {
    let resp = client.list_registries().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&resp.registries)?);
        return Ok(());
    }

    if resp.registries.is_empty() {
        println!(
            "No registries configured. Run `unisrv registry add <hostname>` to add one."
        );
        return Ok(());
    }

    let now = chrono::Utc::now().naive_utc();
    println!("{}", render_table(&resp.registries, now));
    Ok(())
}

pub async fn test(client: &dyn ApiClient, hostname: &str) -> Result<()> {
    let id = resolve_registry_id(client, hostname).await?;
    let resp = client.test_registry(id).await?;

    if resp.ok {
        let validity = match resp.expires_in_seconds {
            Some(secs) => {
                let delta = chrono::Duration::seconds(secs as i64);
                let human = HumanTime::from(delta).to_text_en(Accuracy::Rough, Tense::Present);
                format!("token valid for {human}")
            }
            None => "token valid".to_string(),
        };
        println!("\u{2713} {hostname}: {validity}");
        Ok(())
    } else {
        let reason = resp.error.unwrap_or_else(|| "unknown error".to_string());
        eprintln!("\u{2717} {hostname}: {reason}");
        Err(anyhow!("Registry test failed"))
    }
}

fn resolve_username(username: Option<&str>) -> Result<String> {
    match username {
        Some(u) => Ok(u.to_string()),
        None => Ok(dialoguer::Input::new()
            .with_prompt("Username")
            .interact_text()?),
    }
}

fn read_password(from_stdin: bool) -> Result<String> {
    if from_stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let trimmed = buf.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            bail!("--password-stdin was set but stdin was empty");
        }
        Ok(trimmed.to_string())
    } else {
        let mut yapp = yapp::Yapp::new().with_echo_symbol('*');
        Ok(yapp.read_password_with_prompt("Password: ")?)
    }
}

async fn resolve_registry_id(client: &dyn ApiClient, hostname: &str) -> Result<Uuid> {
    let resp = client.list_registries().await?;
    let needle = hostname.to_ascii_lowercase();
    resp.registries
        .into_iter()
        .find(|r| r.hostname.to_ascii_lowercase() == needle)
        .map(|r| r.id)
        .ok_or_else(|| {
            anyhow!(
                "No registry found for {hostname}. Run `unisrv registry list` to see configured registries."
            )
        })
}

fn map_registry_write_error(err: ApiError, hostname: &str) -> anyhow::Error {
    match err {
        ApiError::Server { status: 409, .. } => anyhow!(
            "A registry for {hostname} already exists. Use `unisrv registry update {hostname}` to change credentials."
        ),
        ApiError::Server { status: 422, reason } => {
            anyhow!("Registry rejected credentials: {reason}")
        }
        ApiError::Server { status: 424, reason } => {
            anyhow!("Registry unreachable: {reason}. Retry later.")
        }
        other => other.into(),
    }
}

fn render_table(registries: &[RegistryResponse], now: NaiveDateTime) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        Cell::new("HOSTNAME").add_attribute(Attribute::Bold),
        Cell::new("KIND").add_attribute(Attribute::Bold),
        Cell::new("USERNAME").add_attribute(Attribute::Bold),
        Cell::new("CREATED").add_attribute(Attribute::Bold),
        Cell::new("UPDATED").add_attribute(Attribute::Bold),
    ]);

    for reg in registries {
        let kind = format_kind(reg.kind);
        let username = extract_username(reg.kind, &reg.config);
        let created = HumanTime::from(reg.created_at - now).to_string();
        let updated = HumanTime::from(reg.updated_at - now).to_string();

        table.add_row(vec![
            Cell::new(&reg.hostname),
            Cell::new(kind),
            Cell::new(username),
            Cell::new(created),
            Cell::new(updated),
        ]);
    }
    table.to_string()
}

fn format_kind(kind: RegistryKind) -> String {
    match kind {
        RegistryKind::Userpass => "userpass".into(),
    }
}

fn extract_username(kind: RegistryKind, config: &serde_json::Value) -> String {
    match kind {
        RegistryKind::Userpass => serde_json::from_value::<UserpassConfig>(config.clone())
            .map(|c| c.username)
            .unwrap_or_else(|_| "\u{2014}".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use unisrv_api::ApiError;
    use unisrv_api::models::{RegistryListResponse, TestRegistryResponse};
    use unisrv_api::test_support::MockApiClient;

    fn registry(hostname: &str, username: &str) -> RegistryResponse {
        let now = Utc::now().naive_utc();
        RegistryResponse {
            id: Uuid::new_v4(),
            hostname: hostname.into(),
            kind: RegistryKind::Userpass,
            config: serde_json::json!({ "username": username }),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn map_409_suggests_update() {
        let err = map_registry_write_error(
            ApiError::Server {
                status: 409,
                reason: "registry for this hostname already exists".into(),
            },
            "ghcr.io",
        );
        assert!(err.to_string().contains("already exists"));
        assert!(err.to_string().contains("unisrv registry update ghcr.io"));
    }

    #[test]
    fn map_422_uses_credential_rejection_phrasing() {
        let err = map_registry_write_error(
            ApiError::Server {
                status: 422,
                reason: "registry rejected credentials".into(),
            },
            "ghcr.io",
        );
        assert!(err.to_string().contains("Registry rejected credentials"));
    }

    #[test]
    fn map_424_marks_as_retryable() {
        let err = map_registry_write_error(
            ApiError::Server {
                status: 424,
                reason: "registry unreachable: connection refused".into(),
            },
            "ghcr.io",
        );
        assert!(err.to_string().contains("Retry later"));
    }

    #[test]
    fn map_other_status_passes_through() {
        let err = map_registry_write_error(
            ApiError::Server {
                status: 500,
                reason: "internal".into(),
            },
            "ghcr.io",
        );
        assert!(err.to_string().contains("500"));
    }

    #[tokio::test]
    async fn list_renders_table() {
        let mock = MockApiClient::logged_in().with_list_registries(Ok(RegistryListResponse {
            registries: vec![registry("ghcr.io", "alice"), registry("docker.io", "bob")],
        }));

        let result = list(&mock, false).await;
        assert!(result.is_ok());
        assert_eq!(mock.calls.lock().unwrap().list_registries_calls, 1);
    }

    #[tokio::test]
    async fn list_json_outputs_array() {
        let mock = MockApiClient::logged_in()
            .with_list_registries(Ok(RegistryListResponse { registries: vec![] }));
        let result = list(&mock, true).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn list_empty_prints_friendly_hint() {
        let mock = MockApiClient::logged_in()
            .with_list_registries(Ok(RegistryListResponse { registries: vec![] }));
        let result = list(&mock, false).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn delete_resolves_hostname_to_id_and_deletes() {
        let reg = registry("ghcr.io", "alice");
        let expected_id = reg.id;
        let mock = MockApiClient::logged_in()
            .with_list_registries(Ok(RegistryListResponse {
                registries: vec![reg],
            }))
            .push_delete_registry(Ok(()));

        let result = delete_with_confirm(&mock, "GHCR.IO", true, |_| {
            panic!("--yes should skip confirmation");
        })
        .await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.delete_registry_calls, vec![expected_id]);
    }

    #[tokio::test]
    async fn delete_unknown_hostname_returns_helpful_error() {
        let mock = MockApiClient::logged_in().with_list_registries(Ok(RegistryListResponse {
            registries: vec![registry("ghcr.io", "alice")],
        }));

        let result = delete_with_confirm(&mock, "docker.io", true, |_| Ok(true)).await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("No registry found for docker.io"));
    }

    #[tokio::test]
    async fn delete_declining_confirm_aborts() {
        let reg = registry("ghcr.io", "alice");
        let mock = MockApiClient::logged_in().with_list_registries(Ok(RegistryListResponse {
            registries: vec![reg],
        }));

        let result = delete_with_confirm(&mock, "ghcr.io", false, |_| Ok(false)).await;
        assert!(result.is_ok());
        assert!(mock.calls.lock().unwrap().delete_registry_calls.is_empty());
    }

    #[tokio::test]
    async fn test_success_prints_token_validity() {
        let reg = registry("ghcr.io", "alice");
        let expected_id = reg.id;
        let mock = MockApiClient::logged_in()
            .with_list_registries(Ok(RegistryListResponse {
                registries: vec![reg],
            }))
            .push_test_registry(Ok(TestRegistryResponse {
                ok: true,
                expires_in_seconds: Some(300),
                error: None,
            }));

        let result = test(&mock, "ghcr.io").await;
        assert!(result.is_ok());
        assert_eq!(mock.calls.lock().unwrap().test_registry_calls, vec![expected_id]);
    }

    #[tokio::test]
    async fn test_failure_returns_error() {
        let reg = registry("ghcr.io", "alice");
        let mock = MockApiClient::logged_in()
            .with_list_registries(Ok(RegistryListResponse {
                registries: vec![reg],
            }))
            .push_test_registry(Ok(TestRegistryResponse {
                ok: false,
                expires_in_seconds: None,
                error: Some("registry rejected credentials".into()),
            }));

        let result = test(&mock, "ghcr.io").await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Registry test failed"));
    }

    #[tokio::test]
    async fn update_requires_at_least_one_field() {
        let mock = MockApiClient::logged_in();
        let result = update(&mock, "ghcr.io", None, false, true).await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("--username"));
    }

    #[tokio::test]
    async fn update_username_only_sends_config_no_secret() {
        let reg = registry("ghcr.io", "alice");
        let expected_id = reg.id;
        let mock = MockApiClient::logged_in()
            .with_list_registries(Ok(RegistryListResponse {
                registries: vec![reg],
            }))
            .push_update_registry(Ok(registry("ghcr.io", "carol")));

        let result = update(&mock, "ghcr.io", Some("carol"), false, true).await;
        assert!(result.is_ok(), "expected ok, got {result:?}");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.update_registry_calls.len(), 1);
        let (id, req, validate) = &calls.update_registry_calls[0];
        assert_eq!(*id, expected_id);
        assert!(validate);
        assert_eq!(req.config, Some(serde_json::json!({ "username": "carol" })));
        assert!(req.secret.is_none());
    }

    #[test]
    fn render_table_includes_columns_and_kind() {
        let now = Utc::now().naive_utc();
        let rendered = render_table(
            &[
                RegistryResponse {
                    id: Uuid::new_v4(),
                    hostname: "ghcr.io".into(),
                    kind: RegistryKind::Userpass,
                    config: serde_json::json!({ "username": "alice" }),
                    created_at: now - Duration::days(2),
                    updated_at: now - Duration::hours(3),
                },
            ],
            now,
        );

        assert!(rendered.contains("HOSTNAME"));
        assert!(rendered.contains("KIND"));
        assert!(rendered.contains("USERNAME"));
        assert!(rendered.contains("CREATED"));
        assert!(rendered.contains("UPDATED"));
        assert!(rendered.contains("ghcr.io"));
        assert!(rendered.contains("userpass"));
        assert!(rendered.contains("alice"));
    }

    #[test]
    fn extract_username_handles_missing_config() {
        let val = extract_username(RegistryKind::Userpass, &serde_json::json!({}));
        assert_eq!(val, "\u{2014}");
    }
}
