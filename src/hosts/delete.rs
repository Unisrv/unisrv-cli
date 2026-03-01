use anyhow::Result;
use console::Emoji;
use reqwest::Client;

use crate::{config::CliConfig, default_spinner, error};

static HOST: Emoji = Emoji("🌐 ", "");
static DELETE: Emoji = Emoji("🗑️ ", "");

pub async fn delete_host(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let host_input = args.get_one::<String>("host").unwrap();

    let progress = default_spinner();
    progress.set_prefix("Resolving host...");

    let hosts = super::list::list(client, config).await?;
    let resolved_id = super::resolve_host_id(host_input, &hosts)?;

    progress.set_prefix("Deleting host...");

    let response = client
        .delete(config.url(&format!("/hosts/{resolved_id}")))
        .bearer_auth(config.token(client).await?)
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        println!(
            "{}{}Host {} deleted successfully",
            DELETE,
            HOST,
            console::style(&resolved_id.to_string()[..8]).yellow()
        );
    } else {
        error::handle_http_error(response, "delete host").await?;
    }

    Ok(())
}
