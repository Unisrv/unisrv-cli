use crate::{config::CliConfig, default_spinner};
use anyhow::Result;
use cidr::Ipv4Cidr;
use console::Emoji;
use reqwest::Client;
use serde::Serialize;

static NETWORK: Emoji = Emoji("🌐 ", "");
static SUCCESS: Emoji = Emoji("✅ ", "");

#[derive(Serialize)]
pub struct CreateInternalNetworkRequest {
    pub name: String,
    pub ipv4_cidr: String,
}

pub async fn create_network(
    client: &Client,
    config: &mut CliConfig,
    args: &clap::ArgMatches,
) -> Result<()> {
    let name = args.get_one::<String>("name").unwrap();
    let cidr = args
        .get_one::<String>("ipv4_cidr")
        .map(|s| s.as_str())
        .unwrap_or("10.0.0.0/8");

    // Validate CIDR format
    if let Err(e) = cidr.parse::<Ipv4Cidr>() {
        return Err(anyhow::anyhow!(
            "Invalid IPv4 CIDR format '{}': {}. Expected format: x.x.x.x/x (e.g., 10.0.0.0/24)",
            cidr,
            e
        ));
    }

    let request = CreateInternalNetworkRequest {
        name: name.clone(),
        ipv4_cidr: cidr.to_string(),
    };

    let spinner = default_spinner();
    spinner.set_prefix("Creating network");
    spinner.set_message(format!("{} Creating network '{}'", NETWORK, name));

    let response = client
        .post(&config.url("/network"))
        .bearer_auth(config.token(client).await?)
        .json(&request)
        .send()
        .await?;

    spinner.finish_and_clear();

    match response.status() {
        reqwest::StatusCode::CREATED => {
            println!(
                "{} {} created successfully with CIDR {}",
                SUCCESS,
                console::style(format!("Network '{}'", name)).bold().green(),
                console::style(cidr).cyan()
            );
            Ok(())
        }
        reqwest::StatusCode::BAD_REQUEST => {
            let error_text = response.text().await?;
            Err(anyhow::anyhow!("Bad request: {}", error_text))
        }
        reqwest::StatusCode::CONFLICT => {
            Err(anyhow::anyhow!("Network name '{}' already exists", name))
        }
        status => {
            let error_text = response.text().await?;
            Err(anyhow::anyhow!(
                "Failed to create network. Status: {}, Response: {}",
                status,
                error_text
            ))
        }
    }
}
