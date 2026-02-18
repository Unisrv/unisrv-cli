use anyhow::Result;
use console::Emoji;
use dialoguer::Confirm;
use reqwest::Client;

use crate::{config::CliConfig, default_spinner, error};

use super::list::HostResponse;

static HOST: Emoji = Emoji("🌐 ", "");
static CHECK: Emoji = Emoji("✅ ", "");
static LOCK: Emoji = Emoji("🔒 ", "");

pub async fn claim_host(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let domain_input = args.get_one::<String>("domain").unwrap();

    // Determine the actual domain to claim
    let domain = if domain_input.contains('.') {
        domain_input.clone()
    } else {
        let suggested = format!("{}.unisrv.dev", domain_input);
        let confirm = Confirm::new()
            .with_prompt(format!(
                "Do you want to claim {}?",
                console::style(&suggested).bold().cyan(),
            ))
            .default(true)
            .interact()?;

        if confirm {
            suggested
        } else {
            return Err(anyhow::anyhow!(
                "Please provide a full domain name (e.g. example.com)"
            ));
        }
    };

    let progress = default_spinner();
    progress.set_prefix("Claiming host");
    progress.set_message(format!(
        "{HOST} Claiming {}...",
        console::style(&domain).cyan()
    ));

    let response = client
        .post(config.url("/hosts"))
        .bearer_auth(config.token(client).await?)
        .json(&serde_json::json!({ "host": domain }))
        .send()
        .await?;

    progress.finish_and_clear();

    if response.status().is_success() {
        let host: HostResponse = response.json().await?;
        println!(
            "{}{} Host {} claimed successfully (id: {})",
            CHECK,
            HOST,
            console::style(&host.host).bold().green(),
            console::style(&host.id.to_string()[..8]).yellow()
        );

        let provision_cert = Confirm::new()
            .with_prompt(format!(
                "{}Do you want to provision a TLS certificate for this domain?",
                LOCK
            ))
            .default(true)
            .interact()?;

        if provision_cert {
            println!();
            super::cert::request_cert_for_host(client, config, &host).await?;
        }

        Ok(())
    } else {
        error::handle_http_error(response, "claim host").await?;
        unreachable!()
    }
}
