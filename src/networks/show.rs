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
    progress.set_message(format!("🔍 Looking up network '{}'", network_input));

    // Resolve network ID (could be UUID or name)
    let resolved_id =
        resolve_network_id(network_input, super::list::list(client, config).await?).await?;

    progress.set_prefix("Loading network info");
    progress.set_message(format!("{} Loading network details...", INFO));

    let response = client
        .get(&config.url(&format!("/network/{}", resolved_id)))
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
    println!("{}", console::style(&header).bold());
    println!("{}", "━".repeat(header.len() + 5));
    println!(
        "Name:         {}",
        console::style(&network.name).bold().green()
    );
    println!("ID:           {}", console::style(&network.id).yellow());
    println!(
        "CIDR:         {}",
        console::style(&network.ipv4_cidr).cyan()
    );
    println!(
        "Created:      {}",
        console::style(&network.created_at).dim()
    );
    println!();

    if !network.instances.is_empty() {
        let instances_header = format!(
            "{} Attached Instances ({})",
            INSTANCE,
            network.instances.len()
        );
        println!("{}", instances_header);
        println!("{}", "━".repeat(instances_header.len() + 5));
        println!(
            "{:<10} {:<15}",
            console::style("ID").bold().cyan(),
            console::style("IP").bold().cyan()
        );
        println!("{}", "-".repeat(30));

        for instance in &network.instances {
            println!(
                "{:<10} {:<15}",
                console::style(&instance.id.to_string()[..8]).yellow(),
                console::style(&instance.internal_ip).green()
            );
        }
    } else {
        println!(
            "{} No instances attached to this network",
            console::style("ℹ️").dim()
        );
    }
}
