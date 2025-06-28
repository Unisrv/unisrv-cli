use std::collections::HashMap;

use crate::{config::CliConfig, default_spinner, instances::logs};
use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

static ROCKET: Emoji = Emoji("🚀 ", "");

pub async fn run_instance(
    client: &Client,
    config: &mut CliConfig,
    container_image: &str,
    vcpu_count: u8,
    memory_mb: u32,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
) -> Result<()> {
    let payload = json!({
        "region": "dev",
        "vcpu_ratio": 1.0,
        "vcpu_count": vcpu_count,
        "memory_mb": memory_mb,
        "configuration": {
            "container_image": container_image,
            "args": args,
            "env": env,
        },
    });

    let progress = default_spinner();
    progress.set_message(format!(
        "{} Starting instance with image: {}",
        ROCKET, container_image
    ));

    let response = client
        .post(&config.url("/instance"))
        .bearer_auth(config.token(client).await?)
        .json(&payload)
        .send()
        .await?;

    #[derive(Deserialize)]
    struct InstanceResponse {
        id: Uuid,
    }

    if response.status().is_success() {
        let id = response.json::<InstanceResponse>().await?.id;
        logs::stream_logs(client, config, id, Some(progress)).await?;
    } else {
        return Err(anyhow::anyhow!(
            "Failed to start instance: {} - {}",
            response.status(),
            response.text().await?
        ));
    }

    Ok(())
}
