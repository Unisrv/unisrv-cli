use anyhow::Result;
use console::Emoji;
use reqwest::Client;

use crate::{config::CliConfig, default_spinner, error};
use super::new::{HTTPLocation, HTTPLocationTarget};
use super::info::ServiceInfoResponse;

static LOCATION: Emoji = Emoji("📍 ", "");
static ADD: Emoji = Emoji("➕ ", "");
static DELETE: Emoji = Emoji("🗑️ ", "");

pub async fn add_location(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let service_id = args.get_one::<String>("service_id").unwrap();
    let path = args.get_one::<String>("path").unwrap();
    let target_type = args.get_one::<String>("target_type").unwrap();
    let target_value = args.get_one::<String>("target_value");
    let override_404 = args.get_one::<String>("override_404");

    let progress = default_spinner();
    progress.set_prefix("Resolving service...");

    // Resolve service ID (could be UUID or name)
    let resolved_service_id =
        super::resolve_service_id(service_id, super::list::list(client, config).await?).await?;

    progress.set_prefix("Fetching service configuration...");

    // Fetch current service configuration
    let response = client
        .get(config.url(&format!("/service/{resolved_service_id}")))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    if !response.status().is_success() {
        progress.finish_and_clear();
        error::handle_http_error(response, "fetch service").await?;
        return Ok(());
    }

    let mut service: ServiceInfoResponse = response.json().await?;

    progress.set_prefix("Adding location...");

    // Parse target
    let target = match target_type.as_str() {
        "service" | "srv" => {
            let group = target_value.and_then(|v| {
                if v.is_empty() {
                    None
                } else {
                    Some(v.clone())
                }
            });
            HTTPLocationTarget::Service { group }
        }
        "url" => {
            let url = target_value
                .ok_or_else(|| anyhow::anyhow!("URL is required for url target type"))?
                .clone();
            HTTPLocationTarget::Url { url }
        }
        _ => {
            return Err(anyhow::anyhow!(
                "Invalid target type. Must be 'service', 'srv', or 'url'"
            ));
        }
    };

    // Create new location
    let new_location = HTTPLocation {
        path: path.clone(),
        override_404: override_404.cloned(),
        target,
    };

    // Check if location path already exists
    if service.configuration.locations.iter().any(|l| l.path == *path) {
        progress.finish_and_clear();
        return Err(anyhow::anyhow!(
            "Location with path '{}' already exists. Delete it first or use a different path.",
            path
        ));
    }

    // Add location to configuration
    service.configuration.locations.push(new_location);

    // Update service configuration
    let response = client
        .put(config.url(&format!("/service/{resolved_service_id}")))
        .bearer_auth(config.token(client).await?)
        .json(&service.configuration)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        println!(
            "{} {} Location {} added to service {}",
            ADD,
            LOCATION,
            console::style(path).yellow(),
            console::style(&resolved_service_id.to_string()[..8]).cyan()
        );
    } else {
        error::handle_http_error(response, "add location").await?;
    }

    Ok(())
}

pub async fn delete_location(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let service_id = args.get_one::<String>("service_id").unwrap();
    let path = args.get_one::<String>("path").unwrap();

    let progress = default_spinner();
    progress.set_prefix("Resolving service...");

    // Resolve service ID (could be UUID or name)
    let resolved_service_id =
        super::resolve_service_id(service_id, super::list::list(client, config).await?).await?;

    progress.set_prefix("Fetching service configuration...");

    // Fetch current service configuration
    let response = client
        .get(config.url(&format!("/service/{resolved_service_id}")))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    if !response.status().is_success() {
        progress.finish_and_clear();
        error::handle_http_error(response, "fetch service").await?;
        return Ok(());
    }

    let mut service: ServiceInfoResponse = response.json().await?;

    progress.set_prefix("Removing location...");

    // Find and remove location
    let original_len = service.configuration.locations.len();
    service.configuration.locations.retain(|l| l.path != *path);

    if service.configuration.locations.len() == original_len {
        progress.finish_and_clear();
        return Err(anyhow::anyhow!(
            "Location with path '{}' not found",
            path
        ));
    }

    // Update service configuration
    let response = client
        .put(config.url(&format!("/service/{resolved_service_id}")))
        .bearer_auth(config.token(client).await?)
        .json(&service.configuration)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        println!(
            "{} {} Location {} deleted from service {}",
            DELETE,
            LOCATION,
            console::style(path).yellow(),
            console::style(&resolved_service_id.to_string()[..8]).cyan()
        );
    } else {
        error::handle_http_error(response, "delete location").await?;
    }

    Ok(())
}
