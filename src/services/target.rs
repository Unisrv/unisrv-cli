use anyhow::Result;
use console::Emoji;
use dialoguer::Select;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config::CliConfig, default_spinner, error, instances};

use super::info::{ServiceInfoResponse, ServiceTarget};

static TARGET: Emoji = Emoji("🎯 ", "");
static ADD: Emoji = Emoji("➕ ", "");
static DELETE: Emoji = Emoji("🗑️ ", "");

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceInstanceTarget {
    pub instance_id: Uuid,
    pub instance_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CreateTargetResponse {
    pub target_id: Uuid,
}

pub async fn add_target(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let service_id = args.get_one::<String>("service_id").unwrap();
    let target = args.get_one::<String>("target").unwrap();
    let group = args.get_one::<String>("group").cloned();

    let progress = default_spinner();
    progress.set_prefix("Resolving service...");

    // Resolve service ID (could be UUID or name)
    let resolved_service_id =
        super::resolve_service_id(service_id, super::list::list(client, config).await?).await?;

    progress.set_prefix("Parsing target...");

    // Parse target (instance_id:port)
    let (instance_id, port) =
        super::parse_target(target, &instances::list::list(client, config).await?).await?;

    let target_request = ServiceInstanceTarget {
        instance_id,
        instance_port: port,
        group: group.clone(),
    };

    progress.set_prefix("Adding target...");

    let response = client
        .post(config.url(&format!("/service/{resolved_service_id}/target")))
        .bearer_auth(config.token(client).await?)
        .json(&target_request)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let create_response: CreateTargetResponse = response.json().await?;
        let group_str = if let Some(g) = group {
            format!(" [group: {}]", console::style(g).magenta())
        } else {
            String::new()
        };
        println!(
            "{} {} Target {} added to service {} ({}:{}{})",
            ADD,
            TARGET,
            console::style(&create_response.target_id.to_string()[..8]).yellow(),
            console::style(&resolved_service_id.to_string()[..8]).cyan(),
            console::style(&instance_id.to_string()[..8]).green(),
            console::style(port).blue(),
            group_str
        );
    } else {
        error::handle_http_error(response, "add target").await?;
    }

    Ok(())
}

pub async fn delete_target(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let service_id = args.get_one::<String>("service_id").unwrap();
    let target_id = args.get_one::<String>("target_id");

    let progress = default_spinner();
    progress.set_prefix("Resolving service...");

    // Resolve service ID (could be UUID or name)
    let resolved_service_id =
        super::resolve_service_id(service_id, super::list::list(client, config).await?).await?;

    progress.set_prefix("Loading targets...");

    let targets = fetch_targets(&resolved_service_id, client, config).await?;

    progress.finish_and_clear();

    let target_uuid = match target_id {
        Some(id) => resolve_target_id(id, &targets)?,
        None => interactive_select_target(&targets)?,
    };

    let progress = default_spinner();
    progress.set_prefix("Deleting target...");

    let response = client
        .delete(config.url(&format!(
            "/service/{resolved_service_id}/target/{target_uuid}"
        )))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        println!(
            "{} {} Target {} deleted from service {}",
            DELETE,
            TARGET,
            console::style(&target_uuid.to_string()[..8]).yellow(),
            console::style(&resolved_service_id.to_string()[..8]).cyan()
        );
    } else {
        error::handle_http_error(response, "delete target").await?;
    }

    Ok(())
}

async fn fetch_targets(
    service_id: &Uuid,
    client: &Client,
    config: &mut CliConfig,
) -> Result<Vec<ServiceTarget>> {
    let response = client
        .get(config.url(&format!("/service/{service_id}")))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    if response.status().is_success() {
        let service: ServiceInfoResponse = response.json().await?;
        Ok(service.targets)
    } else {
        error::handle_http_error(response, "fetch service targets").await?;
        Ok(vec![])
    }
}

fn resolve_target_id(input: &str, targets: &[ServiceTarget]) -> Result<Uuid> {
    // Try exact UUID parse first
    if let Ok(parsed) = Uuid::parse_str(input) {
        return Ok(parsed);
    }

    // Try UUID prefix match
    if input.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let matches: Vec<_> = targets
            .iter()
            .filter(|t| t.id.to_string().starts_with(input))
            .collect();

        match matches.len() {
            1 => return Ok(matches[0].id),
            0 => {
                return Err(anyhow::anyhow!(
                    "No target found with UUID starting with '{}'",
                    input
                ));
            }
            n => {
                return Err(anyhow::anyhow!(
                    "Multiple targets ({}) found with UUID starting with '{}'. Be more specific.",
                    n,
                    input
                ));
            }
        }
    }

    Err(anyhow::anyhow!("Invalid target identifier: {}", input))
}

fn interactive_select_target(targets: &[ServiceTarget]) -> Result<Uuid> {
    if targets.is_empty() {
        return Err(anyhow::anyhow!("No targets configured for this service"));
    }

    let items: Vec<String> = targets
        .iter()
        .map(|t| {
            let group = t.target_group.as_deref().unwrap_or("-");
            format!(
                "{}  instance:{}  port:{}  group:{}",
                &t.id.to_string()[..8],
                &t.instance_id.to_string()[..8],
                t.instance_port,
                group,
            )
        })
        .collect();

    let selection = Select::new()
        .with_prompt("Select target to delete")
        .items(&items)
        .default(0)
        .interact()?;

    Ok(targets[selection].id)
}
