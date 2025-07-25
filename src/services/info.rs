use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{config::CliConfig, default_spinner, error};

static SERVICE: Emoji = Emoji("🔌 ", "");
static PROVIDER: Emoji = Emoji("🌐 ", "");
static TARGET: Emoji = Emoji("🎯 ", "");

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
    let header = format!("{} Service {}", SERVICE, service.id);
    let header_bar_length = header.len();
    println!("{}", console::style(&header).bold());
    println!("{}", "━".repeat(header_bar_length));
    println!(
        "Name:         {}",
        console::style(&service.name).bold().green()
    );
    println!("ID:           {}", console::style(&service.id).yellow());
    println!(
        "Type:         {}",
        console::style(&service.service_type).cyan()
    );
    println!(
        "Created:      {}",
        console::style(&service.created_at).dim()
    );
    println!();

    if !service.providers.is_empty() {
        let providers_header = format!("{} Providers ({})", PROVIDER, service.providers.len());
        let headers = vec![
            "ID".to_string(),
            "ROUTE ADDRESS".to_string()
        ];
        
        let mut content = Vec::new();
        for provider in &service.providers {
            content.push(vec![
                provider.id.to_string(),
                provider.route_address.clone()
            ]);
        }
        
        crate::table::draw_table(providers_header, headers, content);
        println!();
    } else {
        println!("{} No providers configured", console::style("ℹ️").dim());
        println!();
    }

    if !service.targets.is_empty() {
        let targets_header = format!("{} Targets ({})", TARGET, service.targets.len());
        let headers = vec![
            "ID".to_string(),
            "INSTANCE ID".to_string()
        ];
        
        let mut content = Vec::new();
        for target in &service.targets {
            content.push(vec![
                target.id.to_string(),
                target.instance_id.to_string()
            ]);
        }
        
        crate::table::draw_table(targets_header, headers, content);
    } else {
        println!("{} No targets configured", console::style("ℹ️").dim());
    }
}
