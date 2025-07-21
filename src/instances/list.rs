use crate::{config::CliConfig, default_spinner, error};
use anyhow::{Ok, Result};
use console::Emoji;
use reqwest::Client;
use serde::Deserialize;
use tabled::{Table, Tabled, settings::Style};
use uuid::Uuid;

static CROSS: Emoji = Emoji("❌ ", "");
pub const ACTIVE_STATE: &str = "active";

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

#[derive(Tabled)]
struct InstanceTableEntry {
    #[tabled(rename = "Created At")]
    created_at: chrono::NaiveDateTime,
    #[tabled(rename = "Id")]
    id: Uuid,
    #[tabled(rename = "Image")]
    image: String,
    #[tabled(rename = "State")]
    state: String,
}

pub async fn list(client: &Client, config: &mut CliConfig) -> Result<InstanceListResponse> {
    let response = client
        .get(&config.url("/instance/list"))
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
    let _ = progress.set_prefix("Listing instances");
    let resp = list(client, config).await;
    progress.finish_and_clear();
    let resp = resp?;

    let table = resp
        .instances
        .iter()
        .filter(|instance| !filter_only_running || instance.state == ACTIVE_STATE)
        .map(|instance| InstanceTableEntry {
            id: instance.id,
            image: instance
                .configuration
                .get("container_image")
                .map_or("Unknown".to_string(), |img| {
                    img.as_str().unwrap_or("Unknown").to_string()
                }),
            state: instance.state.clone(),
            created_at: instance.created_at,
        })
        .collect::<Vec<_>>();
    if table.is_empty() {
        if filter_only_running {
            eprintln!("{}No running instances found.", CROSS);
            return Ok(());
        } else {
            eprintln!("{}No instances found. How about running one?", CROSS);
            return Ok(());
        }
    }

    let mut table: Table = Table::new(table.iter());
    table.with(Style::modern_rounded());
    eprintln!("{}", table);
    Ok(())
}
