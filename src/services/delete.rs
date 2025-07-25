use anyhow::Result;
use console::Emoji;
use reqwest::Client;

use crate::{config::CliConfig, default_spinner, error};

static SERVICE: Emoji = Emoji("🔧 ", "");
static DELETE: Emoji = Emoji("🗑️ ", "");

pub async fn delete_service(
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

    progress.set_prefix("Deleting service...");

    let response = client
        .delete(&config.url(&format!("/service/{}", resolved_id)))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        println!(
            "{} {} Service {} deleted successfully",
            DELETE,
            SERVICE,
            console::style(&resolved_id.to_string()[..8]).yellow()
        );
    } else {
        error::handle_http_error(response, "delete service").await?;
    }

    Ok(())
}