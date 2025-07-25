use crate::{config::CliConfig, default_spinner, networks::resolve_network_id};
use anyhow::Result;
use console::Emoji;
use reqwest::Client;

static TRASH: Emoji = Emoji("🗑️ ", "");
static SUCCESS: Emoji = Emoji("✅ ", "");
static SEARCH: Emoji = Emoji("🔍 ", "");

pub async fn delete_network(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let network_input = args.get_one::<String>("network_id").unwrap();

    let spinner = default_spinner();
    spinner.set_prefix("Resolving network");
    spinner.set_message(format!("{} Looking up network '{}'", SEARCH, network_input));

    // Get network list to resolve the ID
    let network_list = super::list::list(client, config).await?;
    let network_id = resolve_network_id(network_input, network_list).await?;

    spinner.set_prefix("Deleting network");
    spinner.set_message(format!("{} Deleting network {}", TRASH, network_id));

    let response = client
        .delete(&config.url(&format!("/network/{}", network_id)))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    spinner.finish_and_clear();

    match response.status() {
        reqwest::StatusCode::OK => {
            println!(
                "{} {} deleted successfully",
                SUCCESS,
                console::style(format!("Network {}", network_id))
                    .bold()
                    .green()
            );
            Ok(())
        }
        reqwest::StatusCode::BAD_REQUEST => {
            let error_text = response.text().await?;
            Err(anyhow::anyhow!(
                "Cannot delete network: {}",
                if error_text.contains("non-stopped instances") {
                    "Network has running instances. Stop all instances before deleting the network."
                } else {
                    &error_text
                }
            ))
        }
        reqwest::StatusCode::NOT_FOUND => Err(anyhow::anyhow!("Network {} not found", network_id)),
        status => {
            let error_text = response.text().await?;
            Err(anyhow::anyhow!(
                "Failed to delete network. Status: {}, Response: {}",
                status,
                error_text
            ))
        }
    }
}
