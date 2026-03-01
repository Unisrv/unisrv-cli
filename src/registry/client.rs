use anyhow::{Result, anyhow};
use oci_spec::distribution::Reference;
use oci_spec::image::{Arch, ImageIndex, ImageManifest, Os};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct RegistryTokenResponse {
    pub token: Option<String>,
    pub access_token: Option<String>,
    pub expires_in: Option<i64>,
}

/// Login to a container registry
/// Returns (token, expires_in_seconds)
pub async fn login_registry(
    registry: &str,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<RegistryTokenResponse> {
    let http_client = Client::new();
    // Normalize registry URL
    let registry_url = if registry.starts_with("http://") || registry.starts_with("https://") {
        registry.to_string()
    } else {
        format!("https://{}", registry)
    };

    log::debug!("Attempting to login to registry: {}", registry_url);

    // Step 1: GET /v2/ to check for WWW-Authenticate header
    let v2_url = format!("{}/v2/", registry_url);
    log::debug!("Checking registry endpoint: {}", v2_url);

    let resp = http_client.get(&v2_url).send().await?;

    // If we get 200, no auth needed (or already authenticated somehow)
    if resp.status().is_success() {
        log::debug!("Registry allows anonymous access");
        return Ok(RegistryTokenResponse {
            token: None,
            access_token: None,
            expires_in: None,
        });
    }

    // Step 2: Parse WWW-Authenticate header
    let www_auth_header = resp
        .headers()
        .get("www-authenticate")
        .ok_or_else(|| anyhow!("No WWW-Authenticate header found in registry response"))?
        .to_str()
        .map_err(|e| anyhow!("Invalid WWW-Authenticate header: {}", e))?;

    log::debug!("WWW-Authenticate header: {}", www_auth_header);

    let (realm, service, scope) = parse_www_authenticate(www_auth_header)?;

    log::debug!(
        "Parsed auth params - realm: {}, service: {:?}, scope: {:?}",
        realm,
        service,
        scope
    );

    // Step 3: Send authentication request to realm
    let mut auth_url = realm.clone();
    let mut query_params = vec![];

    if let Some(svc) = service {
        query_params.push(format!("service={}", svc));
    }
    if let Some(scp) = scope {
        query_params.push(format!("scope={}", scp));
    }

    if !query_params.is_empty() {
        auth_url.push('?');
        auth_url.push_str(&query_params.join("&"));
    }

    log::debug!("Auth URL: {}", auth_url);

    let mut auth_request = http_client.get(&auth_url);

    // Add basic auth if credentials provided
    if let (Some(user), Some(pass)) = (username, password) {
        auth_request = auth_request.basic_auth(user, Some(pass));
        log::debug!("Using basic auth with username: {}", user);
    }

    let auth_resp = auth_request.send().await?;

    if !auth_resp.status().is_success() {
        return Err(anyhow!(
            "Failed to authenticate with registry: HTTP {}",
            auth_resp.status()
        ));
    }

    let token_response: RegistryTokenResponse = auth_resp.json().await?;

    log::debug!(
        "Successfully obtained token, expires_in: {:?}",
        token_response.expires_in
    );

    Ok(token_response)
}

/// Parse WWW-Authenticate header
/// Format: Bearer realm="https://auth.example.com/token",service="registry.example.com",scope="repository:user/image:pull"
fn parse_www_authenticate(header: &str) -> Result<(String, Option<String>, Option<String>)> {
    if !header.starts_with("Bearer ") {
        return Err(anyhow!(
            "Unsupported authentication scheme, expected Bearer auth"
        ));
    }

    let params_str = header.trim_start_matches("Bearer ");
    let mut params: HashMap<String, String> = HashMap::new();

    // Parse key="value" pairs
    for part in params_str.split(',') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            params.insert(key.to_string(), value.to_string());
        }
    }

    let realm = params
        .get("realm")
        .ok_or_else(|| anyhow!("No realm found in WWW-Authenticate header"))?
        .clone();

    let service = params.get("service").cloned();
    let scope = params.get("scope").cloned();

    Ok((realm, service, scope))
}

/// Get token for image - checks stored credentials and handles Docker Hub anonymous fallback
pub async fn get_token(
    reference: &Reference,
    config: &mut crate::config::CliConfig,
) -> Result<Option<String>> {
    let registry = reference.resolve_registry();

    // Check if we have stored credentials
    let (username, password) = if let Some(auth_session) = config.auth_session() {
        if let Some(registry_auths) = &auth_session.container_registry_auth {
            if let Some(registry_auth) = registry_auths.get(registry) {
                (
                    registry_auth.username.as_deref(),
                    registry_auth.password.as_deref(),
                )
            } else {
                (None, None)
            }
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    // If no credentials and not Docker Hub, error
    if username.is_none() && registry != "index.docker.io" {
        let program = std::env::args()
            .next()
            .unwrap_or_else(|| "unisrv".to_string());
        return Err(anyhow!(
            "No credentials found for registry '{}'. Please login first with: {} registry login {} -u <username> --password-stdin",
            registry,
            program,
            registry
        ));
    }

    // Get scoped token for the repository
    get_scoped_token(reference, username, password)
        .await
        .map_err(|e| anyhow!("Failed to authenticate with registry '{}': {}", registry, e))
}

/// Get a scoped token for a specific repository
/// This requests a token with the appropriate scope from the registry's auth service
pub async fn get_scoped_token(
    reference: &Reference,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<Option<String>> {
    let http_client = Client::new();
    let registry = reference.resolve_registry();
    let repository = reference.repository();
    let registry_url = format!("https://{}", registry);

    // Step 1: Try to access /v2/ to get WWW-Authenticate header
    let v2_url = format!("{}/v2/", registry_url);
    log::debug!("Checking registry endpoint for scoped token: {}", v2_url);

    let resp = http_client.get(&v2_url).send().await?;

    if resp.status().is_success() {
        log::debug!("Registry allows anonymous access");
        return Ok(None);
    }

    // Step 2: Parse WWW-Authenticate header
    let www_auth_header = resp
        .headers()
        .get("www-authenticate")
        .ok_or_else(|| anyhow!("No WWW-Authenticate header found"))?
        .to_str()
        .map_err(|e| anyhow!("Invalid WWW-Authenticate header: {}", e))?;

    let (realm, service, _) = parse_www_authenticate(www_auth_header)?;

    // Step 3: Request token with repository-specific scope
    let scope = format!("repository:{}:pull", repository);
    let mut auth_url = realm.clone();
    let mut query_params = vec![];

    if let Some(svc) = service {
        query_params.push(format!("service={}", svc));
    }
    query_params.push(format!("scope={}", scope));

    auth_url.push('?');
    auth_url.push_str(&query_params.join("&"));

    log::debug!("Requesting scoped token from: {}", auth_url);

    let mut auth_request = http_client.get(&auth_url);

    if let (Some(user), Some(pass)) = (username, password) {
        auth_request = auth_request.basic_auth(user, Some(pass));
        log::debug!("Using credentials for scoped token");
    }

    let auth_resp = auth_request.send().await?;

    if !auth_resp.status().is_success() {
        return Err(anyhow!(
            "Failed to get scoped token: HTTP {}",
            auth_resp.status()
        ));
    }

    let token_response: RegistryTokenResponse = auth_resp.json().await?;
    let token = token_response.token.or(token_response.access_token);

    log::debug!("Successfully obtained scoped token");
    Ok(token)
}

/// Get manifest and config for an image reference
/// This verifies authentication and ensures a valid image exists
pub async fn get_manifest_and_config(
    reference: &Reference,
    token: Option<&str>,
) -> Result<ImageManifest> {
    let http_client = Client::new();
    let registry = reference.resolve_registry();
    let repository = reference.repository();
    let tag = reference.tag().unwrap_or("latest");

    // Fetch the manifest/index
    let manifest_url = format!("https://{}/v2/{}/manifests/{}", registry, repository, tag);
    log::debug!("Fetching manifest from {}", manifest_url);

    let mut request = http_client.get(&manifest_url).header(
        "Accept",
        "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json",
    );

    if let Some(t) = token {
        request = request.bearer_auth(t);
    }

    let response = request.send().await?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to fetch manifest: HTTP {}",
            response.status()
        ));
    }

    let manifest_data: serde_json::Value = response.json().await?;

    let schema_version = manifest_data
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    // Determine if this is an index or a manifest
    let manifest = match schema_version {
        Some(oci_spec::image::SCHEMA_VERSION) => {
            // Check if this is an image index (multi-platform)
            if manifest_data.get("manifests").is_some() {
                log::debug!("Detected image index (multi-platform)");
                let index: ImageIndex = serde_json::from_value(manifest_data)
                    .map_err(|e| anyhow!("Failed to parse image index: {}", e))?;

                // Find compatible manifest for linux/amd64
                let compatible_descriptor = index
                    .manifests()
                    .iter()
                    .find(|d| {
                        d.platform().as_ref().is_some_and(|p| {
                            *p.architecture() == Arch::Amd64 && *p.os() == Os::Linux
                        })
                    })
                    .ok_or_else(|| anyhow!("No compatible linux/amd64 image found"))?;

                // Fetch the actual manifest
                let platform_manifest_url = format!(
                    "https://{}/v2/{}/manifests/{}",
                    registry,
                    repository,
                    compatible_descriptor.digest()
                );
                log::debug!("Fetching platform manifest from {}", platform_manifest_url);

                let mut platform_request = http_client
                    .get(&platform_manifest_url)
                    .header("Accept", "application/vnd.oci.image.manifest.v1+json");

                if let Some(t) = token {
                    platform_request = platform_request.bearer_auth(t);
                }

                let platform_response = platform_request.send().await?;

                if !platform_response.status().is_success() {
                    return Err(anyhow!(
                        "Failed to fetch image platform manifest: HTTP {}",
                        platform_response.status()
                    ));
                }

                let manifest_bytes = platform_response.bytes().await?;
                ImageManifest::from_reader(&manifest_bytes[..])
                    .map_err(|e| anyhow!("Failed to parse image manifest: {}", e))?
            } else {
                // Direct manifest (single platform)
                log::debug!("Detected direct image manifest");
                let manifest_json = serde_json::to_vec(&manifest_data)?;
                ImageManifest::from_reader(&manifest_json[..])
                    .map_err(|e| anyhow!("Failed to parse image manifest: {}", e))?
            }
        }
        Some(v) => {
            return Err(anyhow!("Unsupported image manifest schema version: {}", v));
        }
        None => {
            return Err(anyhow!("No schema version found in image manifest"));
        }
    };
    Ok(manifest)
}
