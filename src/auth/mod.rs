use crate::config::CliConfig;
use anyhow::Result;
use chrono::DateTime;
use clap::Command;
use reqwest::Client;
use serde::Serialize;

pub fn command() -> Command {
    Command::new("auth")
        .about(format!(
            "Auth related commands. For login use {} instead",
            console::style(format!("{} login", env!("CARGO_PKG_NAME"))).bold()
        ))
        .subcommand_required(true)
        .subcommand(
            Command::new("token")
                .about("Fetch and print an authentication token to stdout")
                .arg(
                    clap::Arg::new("json")
                        .help("Output the token in JSON format")
                        .long("json")
                        .short('j')
                        .action(clap::ArgAction::SetTrue),
                ),
        )
}

#[derive(Serialize)]
struct JsonToken {
    token: String,
    expires_at: DateTime<chrono::Utc>,
}

pub async fn handle(
    config: &mut CliConfig,
    http_client: &Client,
    instance_matches: &clap::ArgMatches,
) -> Result<()> {
    match instance_matches.subcommand() {
        Some(("token", args)) => {
            let token = config.token(http_client).await?;

            if *args.get_one::<bool>("json").unwrap_or(&false) {
                let session = config
                    .auth_session()
                    .ok_or_else(|| anyhow::anyhow!("No active auth session"))?;
                let json_token = JsonToken {
                    token,
                    expires_at: session.access_token_expiry,
                };
                println!("{}", serde_json::to_string(&json_token)?);
                return Ok(());
            }
            println!("{token}");
        }
        _ => {
            return Err(anyhow::anyhow!("Unknown auth command"));
        }
    }
    Ok(())
}
