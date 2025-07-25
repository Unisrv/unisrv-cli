use crate::{config::CliConfig, default_spinner};
use anyhow::Result;
use chrono::NaiveDateTime;
use console::Emoji;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

static NETWORK: Emoji = Emoji("🌐 ", "");
static LIST: Emoji = Emoji("📋 ", "");

#[derive(Deserialize, Serialize)]
pub struct InstanceInfo {
    pub id: Uuid,
    pub internal_ip: String,
}

#[derive(Deserialize, Serialize)]
pub struct NetworkListItem {
    pub id: Uuid,
    pub name: String,
    pub ipv4_cidr: String,
    pub instance_count: Option<i64>,
}

#[derive(Deserialize, Serialize)]
pub struct NetworkListResponse {
    pub networks: Vec<NetworkListItem>,
}

#[derive(Deserialize, Serialize)]
pub struct NetworkResponse {
    pub id: Uuid,
    pub name: String,
    pub ipv4_cidr: String,
    pub created_at: NaiveDateTime,
    pub instances: Vec<InstanceInfo>,
}

pub async fn list_networks(
    client: &Client,
    config: &mut CliConfig,
    _args: &clap::ArgMatches,
) -> Result<()> {
    let spinner = default_spinner();
    spinner.set_prefix("Fetching networks");
    spinner.set_message(format!("{} Loading network list...", LIST));

    let response = client
        .get(&config.url("/networks?include_instance_count=true"))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    spinner.finish_and_clear();

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await?;
        return Err(anyhow::anyhow!(
            "Failed to fetch networks. Status: {}, Response: {}",
            status,
            error_text
        ));
    }

    let network_list: NetworkListResponse = response.json().await?;

    if network_list.networks.is_empty() {
        println!("{} No networks found.", console::style("ℹ️").dim());
        return Ok(());
    }

    let title_with_emoji = format!("{} User-defined Networks", NETWORK);
    
    let headers = vec![
        "ID".to_string(),
        "NAME".to_string(),
        "CIDR".to_string(),
        "INSTANCES".to_string()
    ];
    
    let mut content = Vec::new();
    for network in network_list.networks {
        let short_id = &network.id.to_string()[..8];
        let instance_count = network.instance_count.unwrap_or(0);
        
        content.push(vec![
            short_id.to_string(),
            network.name,
            network.ipv4_cidr,
            instance_count.to_string()
        ]);
    }
    
    crate::table::draw_table(title_with_emoji, headers, content);

    Ok(())
}

pub async fn list(client: &Client, config: &mut CliConfig) -> Result<NetworkListResponse> {
    let response = client
        .get(&config.url("/networks?include_instance_count=false"))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await?;
        return Err(anyhow::anyhow!(
            "Failed to fetch networks. Status: {}, Response: {}",
            status,
            error_text
        ));
    }

    let network_list: NetworkListResponse = response.json().await?;
    Ok(network_list)
}
