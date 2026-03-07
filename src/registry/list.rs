use crate::config::{CliConfig, DEFAULT_REGISTRY};
use anyhow::Result;
use console::style;
use reqwest::Client;

pub async fn list_registries(
    _http_client: &Client,
    config: &mut CliConfig,
    _args: &clap::ArgMatches,
) -> Result<()> {
    let registries = config.registry_credentials();

    // Default registry
    println!("{}", style("Default registry:").bold());
    println!(
        "  {} {}",
        style(DEFAULT_REGISTRY).cyan(),
        style("(unisrv auth)").green(),
    );

    // Additional registries
    let extra: Vec<_> = registries
        .iter()
        .filter(|(r, _)| r.as_str() != DEFAULT_REGISTRY)
        .collect();

    if !extra.is_empty() {
        println!("\n{}", style("Additional CLI registries:").bold());
        for (registry, token) in extra {
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
    }

    Ok(())
}
