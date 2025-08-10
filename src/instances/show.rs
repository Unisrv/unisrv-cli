use crate::{
    config::CliConfig,
    default_spinner, error,
    instances::{list, resolve_uuid},
};
use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::Deserialize;
use uuid::Uuid;

static INSTANCE: Emoji = Emoji("💻 ", "");
static SERVICE: Emoji = Emoji("🔌 ", "");
static INFO: Emoji = Emoji("ℹ️ ", "");

#[derive(Deserialize)]
pub struct ServiceTargetInfo {
    #[allow(dead_code)]
    pub id: Uuid,
    pub service_id: Uuid,
    pub service_type: String,
    pub service_name: String,
    pub instance_port: u16,
}

#[derive(Deserialize)]
pub struct InstanceDetailResponse {
    pub id: Uuid,
    pub name: Option<String>,
    pub node_id: Uuid,
    pub state: String,
    pub exit_code: Option<i32>,
    pub exit_reason: Option<String>,
    pub configuration: serde_json::Value,
    pub created_at: chrono::NaiveDateTime,
    #[allow(dead_code)]
    pub updated_at: chrono::NaiveDateTime,
    pub network_id: Option<Uuid>,
    pub network_ip: Option<String>,
    pub service_targets: Option<Vec<ServiceTargetInfo>>,
}

pub async fn show_instance(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let instance_input = args.get_one::<String>("instance_id").unwrap();

    let progress = default_spinner();
    progress.set_prefix("Resolving instance");
    progress.set_message(format!("🔍 Looking up instance '{instance_input}'"));

    // Resolve instance ID (could be UUID, name, or prefix)
    let resolved_id = resolve_uuid(instance_input, &list::list(client, config).await?).await?;

    progress.set_prefix("Loading instance info");
    progress.set_message(format!("{INFO} Loading instance details..."));

    let response = client
        .get(config.url(&format!(
            "/instance/{resolved_id}?include_service_targets=true"
        )))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let instance: InstanceDetailResponse = response.json().await?;
        display_instance_info(&instance);
    } else {
        error::handle_http_error(response, "get instance info").await?;
    }

    Ok(())
}

fn display_instance_info(instance: &InstanceDetailResponse) {
    let header = format!("{} Instance {}", INSTANCE, instance.id);

    let container_image = instance
        .configuration
        .get("container_image")
        .map_or("Unknown".to_string(), |img| {
            img.as_str().unwrap_or("Unknown").to_string()
        });

    let mut fields = vec![
        (
            "Name".to_string(),
            console::style(instance.name.as_deref().unwrap_or("<unnamed>").to_string())
                .bold()
                .green(),
        ),
        (
            "ID".to_string(),
            console::style(instance.id.to_string()).yellow(),
        ),
        (
            "State".to_string(),
            console::style(instance.state.clone()).cyan(),
        ),
        ("Image".to_string(), console::style(container_image).cyan()),
        (
            "Node ID".to_string(),
            console::style(instance.node_id.to_string()).dim(),
        ),
        (
            "Created".to_string(),
            console::style(instance.created_at.to_string()).dim(),
        ),
    ];

    // Add exit information if available
    if let Some(exit_code) = instance.exit_code {
        fields.push((
            "Exit Code".to_string(),
            console::style(exit_code.to_string()).red(),
        ));
    }

    if let Some(ref exit_reason) = instance.exit_reason {
        fields.push((
            "Exit Reason".to_string(),
            console::style(exit_reason.clone()).red(),
        ));
    }

    // Add network information if available
    if let Some(network_id) = instance.network_id {
        fields.push((
            "Network ID".to_string(),
            console::style(network_id.to_string()).blue(),
        ));
    }

    if let Some(ref network_ip) = instance.network_ip {
        fields.push((
            "Network IP".to_string(),
            console::style(network_ip.clone()).blue(),
        ));
    }

    crate::table::draw_info_section(header, fields);

    // Display service targets if any
    if let Some(ref service_targets) = instance.service_targets {
        if !service_targets.is_empty() {
            let targets_header = format!("{} Service Targets ({})", SERVICE, service_targets.len());
            let headers = vec![
                "SERVICE NAME".to_string(),
                "SERVICE ID".to_string(),
                "TYPE".to_string(),
                "PORT".to_string(),
            ];

            let mut content = Vec::new();
            for target in service_targets {
                content.push(vec![
                    target.service_name.clone(),
                    target.service_id.to_string(),
                    target.service_type.clone(),
                    target.instance_port.to_string(),
                ]);
            }

            crate::table::draw_table(targets_header, headers, content);
        } else {
            println!(
                "{} No service targets configured for this instance",
                console::style("ℹ️").dim()
            );
        }
    }
}
