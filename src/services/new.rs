use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::CliConfig;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum HTTPLocationTarget {
    Instance { group: Option<String> },
    Url { url: String },
}

impl Default for HTTPLocationTarget {
    fn default() -> Self {
        HTTPLocationTarget::Instance { group: None }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HTTPLocation {
    pub path: String,
    #[serde(default)]
    pub override_404: Option<String>,
    #[serde(default)]
    pub target: HTTPLocationTarget,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HTTPServiceConfig {
    #[serde(default)]
    pub locations: Vec<HTTPLocation>,
    #[serde(default)]
    pub allow_http: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceInstanceTarget {
    pub instance_id: Uuid,
    pub instance_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct ServiceProvisionRequest {
    pub region: String,
    pub name: String,
    pub host: String,
    pub configuration: HTTPServiceConfig,
    #[serde(default)]
    pub instance_targets: Vec<ServiceInstanceTarget>,
}

#[derive(Deserialize, Debug)]
pub struct ServiceProvisionResponse {
    pub service_id: Uuid,
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
