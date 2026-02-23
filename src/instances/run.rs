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

pub async fn verify_and_get_token(
    container_image: &str,
    config: &mut CliConfig,
) -> Result<Option<String>> {
    let reference = Reference::from_str(container_image).map_err(|e| {
        anyhow::anyhow!(
            "Invalid container image reference '{}': {}",
            container_image,
            e
        )
    })?;

    log::debug!(
        "Parsed image reference: registry={}, repository={}, tag={}",
        reference.resolve_registry(),
        reference.repository(),
        reference.tag().unwrap_or("latest")
    );

    let token = registry::client::get_token(&reference, config).await?;

    registry::client::get_manifest_and_config(&reference, token.as_deref())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to verify container image: {}", e))?;

    Ok(token)
}

pub async fn create_instance(
    client: &Client,
    config: &mut CliConfig,
    params: &RunInstanceParams<'_>,
    scoped_token: Option<String>,
) -> Result<Uuid> {
    // Parse and resolve network configuration if provided
    let network_config = if let Some(network_str) = &params.network {
        let parts: Vec<&str> = network_str.splitn(2, '@').collect();
        let (instance_ip, network_identifier) = match parts.len() {
            1 => (None, parts[0]),
            2 => {
                let ip = if parts[0].is_empty() {
                    None
                } else {
                    Some(parts[0])
                };
                (ip, parts[1])
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Invalid network format: '{}'. Expected format: [ip]@<network_id/name>",
                    network_str
                ));
            }
        };

        let network_list = networks::list::list(client, config).await?;
        let network_id = networks::resolve_network_id(network_identifier, &network_list).await?;

        let final_ip = if let Some(ip) = instance_ip {
            ip.to_string()
        } else {
            let network_response = client
                .get(config.url(&format!("/network/{network_id}")))
                .bearer_auth(config.token(client).await?)
                .send()
                .await?;

            if !network_response.status().is_success() {
                return error::handle_http_error(network_response, "fetch network details")
                    .await
                    .map(|_| unreachable!());
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

    if let Some(network_config) = network_config {
        payload["network"] = network_config;
    }

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
        Ok(id)
    } else {
        error::handle_http_error(response, "start instance")
            .await
            .map(|_| unreachable!())
    }
}

pub async fn run_instance(
    client: &Client,
    config: &mut CliConfig,
    params: RunInstanceParams<'_>,
) -> Result<()> {
    let progress = default_spinner();
    progress.set_message("Verifying container image...");

    let scoped_token = verify_and_get_token(params.container_image, config)
        .await
        .map_err(|e| {
            progress.finish_and_clear();
            e
        })?;

    progress.set_message(format!(
        "{ROCKET} Starting instance with image: {}",
        params.container_image
    ));

    let id = create_instance(client, config, &params, scoped_token)
        .await
        .map_err(|e| {
            progress.finish_and_clear();
            e
        })?;

    progress.println(format!(
        "{} Instance {} started successfully",
        ROCKET,
        id.to_string().get(0..8).unwrap_or(&id.to_string())
    ));
    logs::stream_logs(client, config, id, Some(progress)).await?;

    Ok(())
}
