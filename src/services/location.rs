use anyhow::Result;
use console::Emoji;
use reqwest::Client;

use super::info::ServiceInfoResponse;
use super::new::{HTTPLocation, HTTPLocationTarget};
use crate::{config::CliConfig, default_spinner, error};

static LOCATION: Emoji = Emoji("📍 ", "");
static ADD: Emoji = Emoji("➕ ", "");
static DELETE: Emoji = Emoji("🗑️ ", "");
static LIST: Emoji = Emoji("📋 ", "");

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
        "instance" | "inst" => {
            let group = target_value
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| "default".to_string());
            HTTPLocationTarget::Instance { group }
        }
        "url" => {
            let url = target_value
                .ok_or_else(|| anyhow::anyhow!("URL is required for url target type"))?
                .clone();
            HTTPLocationTarget::Url { url }
        }
        _ => {
            return Err(anyhow::anyhow!(
                "Invalid target type. Must be 'instance', 'inst', or 'url'"
            ));
        }
    };

    // Create new location
    let new_location = HTTPLocation {
        path: path.clone(),
        override_404: override_404.cloned(),
        target,
    };

    // Check if location path already exists and remove it
    let existing_index = service
        .configuration
        .locations
        .iter()
        .position(|l| l.path == *path);
    if let Some(index) = existing_index {
        service.configuration.locations.remove(index);
    }

    // Add location to configuration (at the beginning so it's checked first)
    service.configuration.locations.insert(0, new_location);

    // Update service configuration
    let response = client
        .put(config.url(&format!("/service/{resolved_service_id}")))
        .bearer_auth(config.token(client).await?)
        .json(&service.configuration)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let action = if existing_index.is_some() {
            "updated"
        } else {
            "added"
        };
        println!(
            "{} {} Location {} {} to service {}",
            ADD,
            LOCATION,
            console::style(path).yellow(),
            action,
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
        return Err(anyhow::anyhow!("Location with path '{}' not found", path));
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

pub async fn list_locations(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let service_id = args
        .get_one::<String>("service_id")
        .ok_or_else(|| anyhow::anyhow!("service_id is required"))?;

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

    let service: ServiceInfoResponse = response.json().await?;

    progress.finish_and_clear();

    // Display locations
    if service.configuration.locations.is_empty() {
        println!(
            "{} {} No locations configured for service {}",
            LIST,
            LOCATION,
            console::style(&resolved_service_id.to_string()[..8]).cyan()
        );
    } else {
        println!(
            "{} {} Locations for service {}:",
            LIST,
            LOCATION,
            console::style(&resolved_service_id.to_string()[..8]).cyan()
        );
        println!();

        for (idx, location) in service.configuration.locations.iter().enumerate() {
            println!(
                "  {} {}",
                console::style(format!("{}.", idx + 1)).dim(),
                console::style(&location.path).yellow().bold()
            );

            // Display target information
            match &location.target {
                HTTPLocationTarget::Instance { group } => {
                    println!(
                        "     {} {}",
                        console::style("Target:").dim(),
                        console::style(format!("instance (group: {})", group)).cyan()
                    );
                }
                HTTPLocationTarget::Url { url } => {
                    println!(
                        "     {} {}",
                        console::style("Target:").dim(),
                        console::style(format!("url ({})", url)).cyan()
                    );
                }
            }

            // Display 404 override if present
            if let Some(override_404) = &location.override_404 {
                println!(
                    "     {} {}",
                    console::style("404 Override:").dim(),
                    console::style(override_404).magenta()
                );
            }

            if idx < service.configuration.locations.len() - 1 {
                println!();
            }
        }
    }

    Ok(())
}
