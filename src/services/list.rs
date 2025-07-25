use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config::CliConfig, default_spinner, error};

static SERVICE: Emoji = Emoji("🔧 ", "");
static LIST: Emoji = Emoji("📋 ", "");

#[derive(Serialize, Deserialize, Debug)]
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
    progress.set_prefix("Loading services");
    progress.set_message(format!("{} Loading service list...", LIST));

    let response = client
        .get(&config.url("/services"))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let resp: ServiceListResponse = response.json().await?;

        if resp.services.is_empty() {
            println!("{} No services found.", console::style("ℹ️").dim());
            return Ok(());
        }

        println!(
            "{} {}",
            SERVICE,
            console::style("Services").bold().underlined()
        );
        println!();
        println!(
            "{:<8} {:<20} {:<10}",
            console::style("ID").bold().cyan(),
            console::style("NAME").bold().cyan(),
            console::style("TYPE").bold().cyan()
        );
        println!("{}", "-".repeat(45));

        for service in resp.services {
            let short_id = &service.id.to_string()[..8];
            println!(
                "{:<8} {:<20} {:<10}",
                console::style(short_id).yellow(),
                console::style(&service.name).green(),
                console::style(&service.service_type).blue()
            );
        }
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
