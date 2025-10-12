use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config::CliConfig, default_spinner, error, instances};

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
    let target_id = args.get_one::<String>("target_id").unwrap();

    let progress = default_spinner();
    progress.set_prefix("Resolving service...");

    // Resolve service ID (could be UUID or name)
    let resolved_service_id =
        super::resolve_service_id(service_id, super::list::list(client, config).await?).await?;

    // Parse target ID as UUID
    let target_uuid = Uuid::parse_str(target_id)
        .map_err(|_| anyhow::anyhow!("Invalid target UUID: {}", target_id))?;

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
