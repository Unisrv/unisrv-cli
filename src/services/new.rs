use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::CliConfig;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
pub enum ServiceConfiguration {
    #[serde(alias = "tcp")]
    #[serde(alias = "TCP")]
    Tcp,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceInstanceTarget {
    pub instance_id: Uuid,
    pub instance_port: u16,
}

#[derive(Serialize, Debug)]
pub struct ServiceProvisionRequest {
    pub region: String,
    pub name: String,
    pub configuration: ServiceConfiguration,
    #[serde(default)]
    pub instance_targets: Vec<ServiceInstanceTarget>,
}

#[derive(Deserialize, Debug)]
pub struct ServiceProvisionResponse {
    pub service_id: Uuid,
    pub connection_string: String,
}

pub async fn new_service(
    request: ServiceProvisionRequest,
    client: &Client,
    config: &mut CliConfig,
) -> Result<ServiceProvisionResponse> {
    let response = client
        .post(config.url("/service"))
        .bearer_auth(config.token(client).await?)
        .json(&request)
        .send()
        .await?;

    if response.status().is_success() {
        let resp: ServiceProvisionResponse = response.json().await?;
        Ok(resp)
    } else {
        Err(anyhow::anyhow!(
            "Failed to create service: {} - {}",
            response.status(),
            response.text().await?
        ))
    }
}
