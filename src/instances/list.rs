use crate::{config::CliConfig, default_spinner, error};
use anyhow::{Ok, Result};
use console::Emoji;
use reqwest::Client;
use serde::Deserialize;
use uuid::Uuid;

static INSTANCE: Emoji = Emoji("💻 ", "");
static LIST: Emoji = Emoji("📋 ", "");

pub const RUNNING_STATE: &str = "running";

#[derive(Deserialize)]
pub struct InstanceListResponse {
    pub instances: Vec<InstanceResponse>,
}

#[derive(Deserialize)]
pub struct InstanceResponse {
    pub id: Uuid,
    configuration: serde_json::Value,
    pub state: String,
    // exit_reason: Option<String>,
    created_at: chrono::NaiveDateTime,
    // updated_at: chrono::NaiveDateTime,
}


pub async fn list(client: &Client, config: &mut CliConfig) -> Result<InstanceListResponse> {
    let response = client
        .get(&config.url("/instances"))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    if response.status().is_success() {
        let resp: InstanceListResponse = response.json().await?;
        Ok(resp)
    } else {
        error::handle_http_error(response, "list instances").await?;
        unreachable!()
    }
}

pub async fn list_instances(
    client: &Client,
    config: &mut CliConfig,
    filter_only_running: bool,
) -> Result<()> {
    let progress = default_spinner();
    progress.set_prefix("Listing instances");
    progress.set_message(format!("{} Loading instance list...", LIST));
    let resp = list(client, config).await;
    progress.finish_and_clear();
    let resp = resp?;

    let filtered_instances: Vec<&InstanceResponse> = resp
        .instances
        .iter()
        .filter(|instance| !filter_only_running || instance.state == RUNNING_STATE)
        .collect();

    if filtered_instances.is_empty() {
        if filter_only_running {
            println!("{} No running instances found.", console::style("ℹ️").dim());
            return Ok(());
        } else {
            println!("{} No instances found. How about running one?", console::style("ℹ️").dim());
            return Ok(());
        }
    }

    let title_with_emoji = format!("{} {}", INSTANCE, if filter_only_running { "Running Instances" } else { "Instances" });
    
    let headers = vec![
        "ID".to_string(),
        "IMAGE".to_string(), 
        "STATE".to_string(),
        "CREATED".to_string()
    ];
    
    let mut content = Vec::new();
    for instance in filtered_instances {
        let short_id = &instance.id.to_string()[..8];
        let image = instance
            .configuration
            .get("container_image")
            .map_or("Unknown".to_string(), |img| {
                img.as_str().unwrap_or("Unknown").to_string()
            });
        let created_str = instance.created_at.format("%Y-%m-%d %H:%M").to_string();
        
        content.push(vec![
            short_id.to_string(),
            image,
            instance.state.clone(),
            created_str
        ]);
    }
    
    crate::table::draw_table(title_with_emoji, headers, content);
    Ok(())
}
