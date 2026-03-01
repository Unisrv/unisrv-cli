use crate::config::CliConfig;
use anyhow::Result;
use console::style;
use reqwest::Client;

pub async fn list_registries(
    _http_client: &Client,
    config: &mut CliConfig,
    _args: &clap::ArgMatches,
) -> Result<()> {
    let registries = config.registry_credentials();

    if registries.is_empty() {
        let program = std::env::args()
            .next()
            .unwrap_or_else(|| "unisrv".to_string());
        println!(
            "No registries configured. Login with: {} registry login <registry>",
            style(program).bold()
        );
        return Ok(());
    }

    println!("{}", style("Configured registries:").bold());
    for (registry, token) in registries {
        let username = token.username.as_deref().unwrap_or("(anonymous)");
        let expiry = token
            .token_expiry
            .map(|t| {
                if t < chrono::Utc::now() {
                    style(format!("expired {}", t.format("%Y-%m-%d %H:%M UTC")))
                        .red()
                        .to_string()
                } else {
                    style(format!("expires {}", t.format("%Y-%m-%d %H:%M UTC")))
                        .dim()
                        .to_string()
                }
            })
            .unwrap_or_else(|| style("no expiry").dim().to_string());

        println!(
            "  {} {} [{}]",
            style(registry).cyan(),
            style(username).green(),
            expiry,
        );
    }

    Ok(())
}
