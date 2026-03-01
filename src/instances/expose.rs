use crate::{config::CliConfig, default_spinner, error, instances::list};
use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

static INFO: Emoji = Emoji("ℹ️ ", "");

#[derive(Serialize)]
struct ExposePortRequest {
    port: u16,
}

#[derive(Deserialize)]
struct ExposePortResponse {
    #[allow(dead_code)]
    id: Uuid,
    external_address: String,
}

pub async fn expose_port(
    client: &Client,
    config: &mut CliConfig,
    instance_input: &str,
    port: u16,
) -> Result<()> {
    let progress = default_spinner();
    progress.set_prefix("Resolving instance");
    progress.set_message(format!("🔍 Looking up instance '{instance_input}'"));

    // Resolve instance ID (could be UUID, name, or prefix)
    let resolved_id = super::resolve_uuid(instance_input, &list::list(client, config).await?)?;

    progress.set_prefix("Exposing port");
    progress.set_message(format!("{INFO} Exposing port {port}..."));

    let request_body = ExposePortRequest { port };

    let response = client
        .post(config.url(&format!("/instance/{resolved_id}/tcp")))
        .bearer_auth(config.token(client).await?)
        .json(&request_body)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let expose_response: ExposePortResponse = response.json().await?;
        println!("{}", expose_response.external_address);
    } else {
        error::handle_http_error(response, "expose port").await?;
    }

    Ok(())
}
