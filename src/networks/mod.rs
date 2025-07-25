use crate::config::CliConfig;
use anyhow::Result;
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

mod create;
mod delete;
mod list;
mod show;

pub fn command() -> Command {
    Command::new("network")
        .alias("net")
        .alias("networks")
        .about("Manage networks")
        .subcommand_required(false)
        .subcommand(
            Command::new("new")
                .about("Create a new internal network")
                .arg(
                    Arg::new("name")
                        .help("Name of the network")
                        .required(true)
                        .index(1),
                )
                .arg(
                    Arg::new("ipv4_cidr")
                        .help("IPv4 CIDR block (defaults to 10.0.0.0/8)")
                        .required(false)
                        .index(2),
                ),
        )
        .subcommand(
            Command::new("show")
                .alias("get")
                .about("Get detailed information about a network")
                .arg(
                    Arg::new("network_id")
                        .help("Network UUID, name, or UUID prefix")
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(
            Command::new("delete")
                .alias("rm")
                .about("Delete a network")
                .arg(
                    Arg::new("network_id")
                        .help("Network UUID, name, or UUID prefix")
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(Command::new("list").alias("ls").about("List all networks"))
}

pub async fn handle(config: &mut CliConfig, network_matches: &clap::ArgMatches) -> Result<()> {
    let http_client = Client::new();
    match network_matches.subcommand() {
        Some(("new", args)) => create::create_network(&http_client, config, args).await,
        Some(("show", args)) | Some(("get", args)) => {
            show::show_network(&http_client, config, args).await
        }
        Some(("delete", args)) | Some(("rm", args)) => {
            delete::delete_network(&http_client, config, args).await
        }
        Some(("list", args)) | Some(("ls", args)) => {
            list::list_networks(&http_client, config, args).await
        }
        Some((_, _)) => {
            eprintln!("Unknown network command");
            Ok(())
        }
        None => {
            // Default to listing networks when no subcommand is provided
            list::list_networks(&http_client, config, &clap::ArgMatches::default()).await
        }
    }
}

pub async fn resolve_network_id(input: &str, list: list::NetworkListResponse) -> Result<Uuid> {
    // First try to parse as UUID
    if let Ok(parsed_uuid) = Uuid::parse_str(input) {
        return Ok(parsed_uuid);
    }

    // Try to find by name (exact match)
    for network in &list.networks {
        if network.name == input {
            return Ok(network.id);
        }
    }

    // If not a valid UUID and no name match, check if it could be a UUID prefix
    if input.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let starts_with_input = list
            .networks
            .iter()
            .filter(|network| network.id.to_string().starts_with(input))
            .collect::<Vec<_>>();

        if starts_with_input.len() == 1 {
            return Ok(starts_with_input[0].id);
        } else if starts_with_input.is_empty() {
            return Err(anyhow::anyhow!(
                "No network found with UUID starting with '{}'",
                input
            ));
        } else {
            return Err(anyhow::anyhow!(
                "Multiple networks ({}) found with UUID starting with '{}'.",
                starts_with_input.len(),
                input
            ));
        }
    }

    Err(anyhow::anyhow!(
        "No network found with name '{}' or UUID '{}'",
        input,
        input
    ))
}
