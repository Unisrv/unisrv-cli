use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tabled::{settings::Style, Table, Tabled};
use uuid::Uuid;

use crate::{config::CliConfig, default_spinner, error};

#[derive(Serialize, Deserialize, Debug, Tabled)]
pub struct Service {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub service_type: String,
}

#[derive(Deserialize, Debug)]
pub struct ServiceListResponse {
    pub services: Vec<Service>,
}

pub async fn list_services(
    client: &Client,
    config: &mut CliConfig,
    _args: &clap::ArgMatches,
) -> Result<()> {
    let progress = default_spinner();
    progress.set_prefix("Loading services...");

    let response = client
        .get(&config.url("/services"))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let resp: ServiceListResponse = response.json().await?;
        
        if resp.services.is_empty() {
            println!("No services found.");
            return Ok(());
        }

        let mut table: Table = Table::new(resp.services.iter());
        table.with(Style::modern_rounded());
        println!("{}", table);
    } else {
        error::handle_http_error(response, "list services").await?;
    }

    Ok(())
}

pub async fn list(client: &Client, config: &mut CliConfig) -> Result<ServiceListResponse> {
    let response = client
        .get(&config.url("/services"))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    if response.status().is_success() {
        let resp: ServiceListResponse = response.json().await?;
        Ok(resp)
    } else {
        error::handle_http_error(response, "list services").await?;
        unreachable!()
    }
}