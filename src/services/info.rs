use anyhow::Result;
use console::Emoji;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::new::HTTPServiceConfig;
use crate::{config::CliConfig, default_spinner, error};

static SERVICE: Emoji = Emoji("🔌 ", "");

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceTarget {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub target_group: Option<String>,
    pub instance_port: u16,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceStatistics {
    pub incoming_bytes: u64,
    pub outgoing_bytes: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceInfoResponse {
    pub id: Uuid,
    pub name: String,
    pub configuration: HTTPServiceConfig,
    pub user_id: Option<Uuid>,
    pub created_at: String,
    pub updated_at: String,
    pub targets: Vec<ServiceTarget>,
    pub statistics: Option<ServiceStatistics>,
}

pub async fn get_service_info(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let service_id = args.get_one::<String>("service_id").unwrap();

    let progress = default_spinner();
    progress.set_prefix("Resolving service...");

    // Resolve service ID (could be UUID or name)
    let resolved_id =
        super::resolve_service_id(service_id, super::list::list(client, config).await?).await?;

    progress.set_prefix("Loading service info...");

    let response = client
        .get(config.url(&format!("/service/{resolved_id}")))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let service: ServiceInfoResponse = response.json().await?;
        display_service_info(&service);
    } else {
        error::handle_http_error(response, "get service info").await?;
    }

    Ok(())
}

fn display_service_info(service: &ServiceInfoResponse) {
    let header = format!("{} Service {}", SERVICE, service.id);
    let fields = vec![
        (
            "Name".to_string(),
            console::style(service.name.clone()).bold().green(),
        ),
        (
            "ID".to_string(),
            console::style(service.id.to_string()).yellow(),
        ),
        (
            "Type".to_string(),
            console::style("HTTP".to_string()).cyan(),
        ),
        (
            "Created".to_string(),
            console::style(service.created_at.clone()).dim(),
        ),
    ];

    crate::table::draw_info_section(header, fields);

    // Statistics — single prominent line
    if let Some(stats) = &service.statistics {
        println!(
            "📊 Statistics: {} {}  {} {}",
            console::style(format_bytes(stats.incoming_bytes)).bold().green(),
            console::style("IN").bold(),
            console::style(format_bytes(stats.outgoing_bytes)).bold().cyan(),
            console::style("OUT").bold(),
        );
        println!();
    }

    // Configuration
    println!(
        "⚙️  Allow HTTP: {}",
        if service.configuration.allow_http {
            console::style("Yes").green()
        } else {
            console::style("No").red()
        }
    );
    println!();

    // Locations — compact list
    println!(
        "{}",
        console::style(format!(
            "📍 Locations ({})",
            service.configuration.locations.len()
        ))
        .bold()
    );
    if service.configuration.locations.is_empty() {
        println!("   {}", console::style("None").dim());
    } else {
        for loc in &service.configuration.locations {
            let target_str = match &loc.target {
                super::new::HTTPLocationTarget::Instance { group } => {
                    format!("→ instances ({})", group)
                }
                super::new::HTTPLocationTarget::Url { url } => {
                    format!("→ {}", url)
                }
            };
            let suffix = loc
                .override_404
                .as_ref()
                .map(|p| format!("  [404: {}]", p))
                .unwrap_or_default();
            println!(
                "   {} {} {}{}",
                console::style(&loc.path).yellow(),
                console::style(target_str).dim(),
                console::style(suffix).dim(),
                ""
            );
        }
    }
    println!();

    // Targets — compact list
    println!(
        "{}",
        console::style(format!("🎯 Targets ({})", service.targets.len())).bold()
    );
    if service.targets.is_empty() {
        println!("   {}", console::style("None").dim());
    } else {
        for t in &service.targets {
            let group = t
                .target_group
                .as_deref()
                .unwrap_or("default");
            println!(
                "   {} → {}:{}  {}",
                console::style(t.id.to_string().get(..8).unwrap_or(&t.id.to_string())).yellow(),
                console::style(t.instance_id.to_string().get(..8).unwrap_or(&t.instance_id.to_string())).cyan(),
                console::style(t.instance_port).bold(),
                console::style(format!("({})", group)).dim(),
            );
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;

    let b = bytes as f64;
    if b >= TB {
        format!("{:.1}Tb", b / TB)
    } else if b >= GB {
        format!("{:.1}Gb", b / GB)
    } else if b >= MB {
        format!("{:.1}Mb", b / MB)
    } else if b >= KB {
        format!("{:.1}Kb", b / KB)
    } else {
        format!("{}b", bytes)
    }
}
