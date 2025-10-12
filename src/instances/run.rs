use std::collections::HashMap;
use std::str::FromStr;

use crate::{config::CliConfig, default_spinner, error, instances::logs, networks, registry};
use anyhow::Result;
use cidr::Ipv4Cidr;
use console::Emoji;
use oci_spec::distribution::Reference;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

static ROCKET: Emoji = Emoji("🚀 ", "");

pub struct RunInstanceParams<'a> {
    pub container_image: &'a str,
    pub vcpu_count: u8,
    pub memory_mb: u32,
    pub args: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,
    pub name: Option<String>,
    pub network: Option<String>,
}

pub async fn run_instance(
    client: &Client,
    config: &mut CliConfig,
    params: RunInstanceParams<'_>,
) -> Result<()> {
    // Step 1: Parse container image as Reference
    let reference = Reference::from_str(params.container_image)
        .map_err(|e| anyhow::anyhow!("Invalid container image reference '{}': {}", params.container_image, e))?;

    log::debug!("Parsed image reference: registry={}, repository={}, tag={}",
        reference.resolve_registry(),
        reference.repository(),
        reference.tag().unwrap_or("latest")
    );

    // Step 2: Authenticate and verify image
    let spinner = default_spinner();
    spinner.set_message("Verifying container image...");

    // Get token (checks credentials, handles Docker Hub anonymous fallback)
    let token = registry::client::get_token(&reference, config)
        .await
        .map_err(|e| {
            spinner.finish_and_clear();
            e
        })?;

    // Verify manifest exists
    registry::client::get_manifest_and_config(&reference, token.as_deref())
        .await
        .map_err(|e| {
            spinner.finish_and_clear();
            anyhow::anyhow!("Failed to verify container image: {}", e)
        })?;

    let scoped_token = token;

    // Parse and resolve network configuration if provided
    let network_config = if let Some(network_str) = params.network {
        let parts: Vec<&str> = network_str.splitn(2, '@').collect();
        let (instance_ip, network_identifier) = match parts.len() {
            1 => (None, parts[0]), // network-name
            2 => {
                let ip = if parts[0].is_empty() {
                    None
                } else {
                    Some(parts[0])
                };
                (ip, parts[1]) // [ip]@network-name
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Invalid network format: '{}'. Expected format: [ip]@<network_id/name>",
                    network_str
                ));
            }
        };

        // Resolve network ID
        let network_list = networks::list::list(client, config).await?;
        let network_id = networks::resolve_network_id(network_identifier, &network_list).await?;

        let final_ip = if let Some(ip) = instance_ip {
            // Explicit IP provided
            ip.to_string()
        } else {
            // Auto-assign IP - fetch network details
            let network_response = client
                .get(config.url(&format!("/network/{network_id}")))
                .bearer_auth(config.token(client).await?)
                .send()
                .await?;

            if !network_response.status().is_success() {
                return error::handle_http_error(network_response, "fetch network details").await;
            }

            let network: networks::list::NetworkResponse = network_response.json().await?;
            let network_cidr: Ipv4Cidr = network
                .ipv4_cidr
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid CIDR format: {}", network.ipv4_cidr))?;

            let used_ips: Vec<String> = network
                .instances
                .iter()
                .map(|instance| instance.internal_ip.clone())
                .collect();

            networks::next_ip(network_cidr, &used_ips).await?
        };

        Some(json!({
            "network_id": network_id,
            "instance_ip": final_ip
        }))
    } else {
        None
    };

    let mut payload = json!({
        "region": "dev",
        "vcpu_ratio": 1.0,
        "vcpu_count": params.vcpu_count,
        "memory_mb": params.memory_mb,
        "name": params.name,
        "configuration": {
            "container_image": params.container_image,
            "args": params.args,
            "env": params.env,
        },
        "container_registry_token": scoped_token,
    });

    // Add network configuration if provided
    if let Some(network_config) = network_config {
        payload["network"] = network_config;
    }

    let progress = default_spinner();
    progress.set_message(format!(
        "{ROCKET} Starting instance with image: {}",
        params.container_image
    ));

    let response = client
        .post(config.url("/instance"))
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
        progress.println(format!(
            "{} Instance {} started successfully",
            ROCKET,
            id.to_string().get(0..8).unwrap_or(&id.to_string())
        ));
        logs::stream_logs(client, config, id, Some(progress)).await?;
    } else {
        return error::handle_http_error(response, "start instance").await;
    }

    Ok(())
}
