use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{config::CliConfig, default_spinner, error};

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceProvider {
    pub id: Uuid,
    pub node_id: Uuid,
    pub route_address: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceTarget {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceInfoResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub service_type: String,
    pub configuration: Value,
    pub user_id: Uuid,
    pub created_at: String,
    pub updated_at: String,
    pub providers: Vec<ServiceProvider>,
    pub targets: Vec<ServiceTarget>,
}

pub async fn get_service_info(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let service_id = args.get_one::<String>("service_id").unwrap();

    let progress = default_spinner();
    progress.set_prefix("Resolving service...");

    // Resolve service ID (could be UUID or name)
    let resolved_id =
        super::resolve_service_id(service_id, super::list::list(client, config).await?).await?;

    progress.set_prefix("Loading service info...");

    let response = client
        .get(&config.url(&format!("/service/{}", resolved_id)))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let service: ServiceInfoResponse = response.json().await?;
        display_service_info(&service);
    } else {
        error::handle_http_error(response, "get service info").await?;
    }

    Ok(())
}

fn display_service_info(service: &ServiceInfoResponse) {
    println!("🔧 Service Information");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("Name:         {}", service.name);
    println!("ID:           {}", service.id);
    println!("Type:         {}", service.service_type);
    println!("Created:      {}", service.created_at);
    println!();

    if !service.providers.is_empty() {
        println!("🌐 Providers ({}):", service.providers.len());
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        for provider in &service.providers {
            println!("  • Route: {}", provider.route_address);
        }
        println!();
    }

    if !service.targets.is_empty() {
        println!("🎯 Targets ({}):", service.targets.len());
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        for target in &service.targets {
            println!("  • Instance: {}", target.instance_id);
        }
    }
}
