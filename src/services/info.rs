use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config::CliConfig, default_spinner, error};
use super::new::HTTPServiceConfig;

static SERVICE: Emoji = Emoji("🔌 ", "");
static PROVIDER: Emoji = Emoji("🌐 ", "");
static TARGET: Emoji = Emoji("🎯 ", "");
static LOCATION: Emoji = Emoji("📍 ", "");

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
    pub target_group: Option<String>,
    pub instance_port: u16,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceInfoResponse {
    pub id: Uuid,
    pub name: String,
    pub configuration: HTTPServiceConfig,
    pub user_id: Option<Uuid>,
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
        .get(config.url(&format!("/service/{resolved_id}")))
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
    let fields = vec![
        (
            "Name".to_string(),
            console::style(service.name.clone()).bold().green(),
        ),
        (
            "ID".to_string(),
            console::style(service.id.to_string()).yellow(),
        ),
        (
            "Type".to_string(),
            console::style("HTTP".to_string()).cyan(),
        ),
        (
            "Created".to_string(),
            console::style(service.created_at.clone()).dim(),
        ),
    ];

    crate::table::draw_info_section(header, fields);

    // Display HTTP configuration
    println!("{} Configuration", console::style("⚙️").bold());
    println!("  Allow HTTP: {}", if service.configuration.allow_http {
        console::style("Yes").green()
    } else {
        console::style("No").red()
    });
    println!();

    // Display locations
    if !service.configuration.locations.is_empty() {
        let locations_header = format!("{} Locations ({})", LOCATION, service.configuration.locations.len());
        let headers = vec![
            "PATH".to_string(),
            "TARGET".to_string(),
            "OVERRIDE 404".to_string(),
        ];

        let mut content = Vec::new();
        for location in &service.configuration.locations {
            let target_str = match &location.target {
                super::new::HTTPLocationTarget::Instance { group } => {
                    if let Some(g) = group {
                        format!("instance (group: {})", g)
                    } else {
                        "instance (default)".to_string()
                    }
                }
                super::new::HTTPLocationTarget::Url { url } => {
                    format!("url: {}", url)
                }
            };
            content.push(vec![
                location.path.clone(),
                target_str,
                location.override_404.clone().unwrap_or_else(|| "-".to_string()),
            ]);
        }

        crate::table::draw_table(locations_header, headers, content);
        println!();
    } else {
        println!("{} No locations configured", console::style("ℹ️").dim());
        println!();
    }

    if !service.providers.is_empty() {
        let providers_header = format!("{} Providers ({})", PROVIDER, service.providers.len());
        let headers = vec!["ID".to_string(), "ROUTE ADDRESS".to_string()];

        let mut content = Vec::new();
        for provider in &service.providers {
            content.push(vec![
                provider.id.to_string(),
                provider.route_address.clone(),
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
            "INSTANCE ID".to_string(),
            "PORT".to_string(),
            "GROUP".to_string(),
        ];

        let mut content = Vec::new();
        for target in &service.targets {
            content.push(vec![
                target.id.to_string(),
                target.instance_id.to_string(),
                target.instance_port.to_string(),
                target.target_group.clone().unwrap_or_else(|| "-".to_string()),
            ]);
        }

        crate::table::draw_table(targets_header, headers, content);
    } else {
        println!("{} No targets configured", console::style("ℹ️").dim());
    }
}
