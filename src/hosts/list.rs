use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::Deserialize;
use uuid::Uuid;

use crate::{config::CliConfig, default_spinner, error};

static HOST: Emoji = Emoji("🌐 ", "");
static LIST: Emoji = Emoji("📋 ", "");

#[derive(Deserialize, Debug, Clone)]
pub struct HostResponse {
    pub id: Uuid,
    pub host: String,
    pub user_id: Uuid,
    pub service_id: Option<Uuid>,
    pub certificate_type: Option<String>,
    pub certificate_valid_until: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub async fn list_hosts(
    client: &Client,
    config: &mut CliConfig,
    _args: &clap::ArgMatches,
) -> Result<()> {
    let progress = default_spinner();
    progress.set_prefix("Loading hosts");
    progress.set_message(format!("{LIST} Loading host list..."));

    let hosts = list(client, config).await?;

    progress.finish_and_clear();

    if hosts.is_empty() {
        println!("{} No hosts found.", console::style("ℹ️").dim());
        return Ok(());
    }

    let title = format!("{HOST} Hosts");
    let headers = vec![
        "ID".to_string(),
        "DOMAIN".to_string(),
        "CERTIFICATE".to_string(),
        "SERVICE".to_string(),
    ];

    let mut content = Vec::new();
    for host in &hosts {
        let short_id = &host.id.to_string()[..8];
        let cert = match &host.certificate_type {
            Some(ct) => ct.clone(),
            None => "-".to_string(),
        };
        let service = match &host.service_id {
            Some(sid) => sid.to_string()[..8].to_string(),
            None => "-".to_string(),
        };
        content.push(vec![short_id.to_string(), host.host.clone(), cert, service]);
    }

    crate::table::draw_table(title, headers, content);

    Ok(())
}

pub async fn list(client: &Client, config: &mut CliConfig) -> Result<Vec<HostResponse>> {
    let response = client
        .get(config.url("/hosts"))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    let response = error::check_response(response, "list hosts").await?;
    let hosts: Vec<HostResponse> = response.json().await?;
    Ok(hosts)
}
