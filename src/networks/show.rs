use crate::{
    config::CliConfig,
    default_spinner, error,
    networks::{list::NetworkResponse, resolve_network_id},
};
use anyhow::Result;
use console::Emoji;
use reqwest::Client;

static NETWORK: Emoji = Emoji("🌐 ", "");
static INSTANCE: Emoji = Emoji("💻 ", "");
static INFO: Emoji = Emoji("ℹ️ ", "");

pub async fn show_network(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let network_input = args.get_one::<String>("network_id").unwrap();

    let progress = default_spinner();
    progress.set_prefix("Resolving network");
    progress.set_message(format!("🔍 Looking up network '{network_input}'"));

    // Resolve network ID (could be UUID or name)
    let resolved_id =
        resolve_network_id(network_input, &super::list::list(client, config).await?).await?;

    progress.set_prefix("Loading network info");
    progress.set_message(format!("{INFO} Loading network details..."));

    let response = client
        .get(config.url(&format!("/network/{resolved_id}")))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let network: NetworkResponse = response.json().await?;
        display_network_info(&network);
    } else {
        error::handle_http_error(response, "get network info").await?;
    }

    Ok(())
}

fn display_network_info(network: &NetworkResponse) {
    let header = format!("{} Network {}", NETWORK, network.id);
    let fields = vec![
        (
            "Name".to_string(),
            console::style(network.name.clone()).bold().green(),
        ),
        (
            "ID".to_string(),
            console::style(network.id.to_string()).yellow(),
        ),
        (
            "CIDR".to_string(),
            console::style(network.ipv4_cidr.clone()).cyan(),
        ),
        (
            "Created".to_string(),
            console::style(network.created_at.to_string()).dim(),
        ),
    ];

    crate::table::draw_info_section(header, fields);

    if !network.instances.is_empty() {
        let instances_header = format!(
            "{} Attached Instances ({})",
            INSTANCE,
            network.instances.len()
        );
        let headers = vec!["ID".to_string(), "IP".to_string()];

        let mut content = Vec::new();
        for instance in &network.instances {
            content.push(vec![instance.id.to_string(), instance.internal_ip.clone()]);
        }

        crate::table::draw_table(instances_header, headers, content);
    } else {
        println!(
            "{} No instances attached to this network",
            console::style("ℹ️").dim()
        );
    }
}
