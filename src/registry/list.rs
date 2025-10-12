use crate::config::CliConfig;
use anyhow::Result;
use reqwest::Client;

pub async fn list_registries(
    _http_client: &Client,
    _config: &mut CliConfig,
    _args: &clap::ArgMatches,
) -> Result<()> {
    // TODO: Implement registry listing
    println!("Listing container registries (not yet implemented)");
    Ok(())
}
