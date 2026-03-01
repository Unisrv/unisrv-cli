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
    /// Optional name for the instance.
    pub name: Option<String>,
    /// The current state of the instance.
    pub state: String,
    /// The container image reference.
    pub container_image: String,
    /// The timestamp when the instance was created.
    pub created_at: chrono::NaiveDateTime,
}

pub async fn list(client: &Client, config: &mut CliConfig) -> Result<InstanceListResponse> {
    let response = client
        .get(config.url("/instances"))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    let response = error::check_response(response, "list instances").await?;
    let resp: InstanceListResponse = response.json().await?;
    Ok(resp)
}

pub async fn list_instances(
    client: &Client,
    config: &mut CliConfig,
    filter_only_running: bool,
) -> Result<()> {
    let progress = default_spinner();
    progress.set_prefix("Listing instances");
    progress.set_message(format!("{LIST} Loading instance list..."));
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
            println!(
                "{} No instances found. How about running one?",
                console::style("ℹ️").dim()
            );
            return Ok(());
        }
    }

    let title_with_emoji = format!(
        "{} {}",
        INSTANCE,
        if filter_only_running {
            "Running Instances"
        } else {
            "Instances"
        }
    );

    let headers = vec![
        "ID".to_string(),
        "NAME".to_string(),
        "IMAGE".to_string(),
        "STATE".to_string(),
        "CREATED".to_string(),
    ];

    let mut content = Vec::new();
    for instance in filtered_instances {
        let short_id = &instance.id.to_string()[..8];
        let name = instance.name.as_deref().unwrap_or("<unnamed>");
        let image = &instance.container_image;
        let created_str = instance.created_at.format("%Y-%m-%d %H:%M").to_string();

        content.push(vec![
            short_id.to_string(),
            name.to_string(),
            image.to_string(),
            instance.state.clone(),
            created_str,
        ]);
    }

    crate::table::draw_table(title_with_emoji, headers, content);
    Ok(())
}
