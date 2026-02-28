use crate::config::CliConfig;
use anyhow::Result;
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

mod cert;
mod claim;
mod delete;
pub(crate) mod list;

pub fn command() -> Command {
    Command::new("host")
        .alias("hosts")
        .about("Manage hosts (domains)")
        .subcommand_required(false)
        .subcommand(Command::new("list").about("List all hosts").alias("ls"))
        .subcommand(
            Command::new("claim")
                .about("Claim a host (domain)")
                .arg(
                    Arg::new("domain")
                        .help("Domain name to claim (e.g. example.com) or a subdomain name for <name>.unisrv.dev")
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(
            Command::new("delete")
                .alias("rm")
                .about("Delete (unclaim) a host")
                .arg(
                    Arg::new("host")
                        .help("Host UUID, UUID prefix, or domain name")
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(
            Command::new("cert")
                .about("Request a TLS certificate for a host")
                .arg(
                    Arg::new("host")
                        .help("Host UUID, UUID prefix, or domain name")
                        .required(true)
                        .index(1),
                ),
        )
}

pub async fn handle(
    config: &mut CliConfig,
    http_client: &Client,
    matches: &clap::ArgMatches,
) -> Result<()> {
    match matches.subcommand() {
        Some(("list", args)) => list::list_hosts(http_client, config, args).await,
        Some(("claim", args)) => claim::claim_host(http_client, config, args).await,
        Some(("delete", args)) => delete::delete_host(http_client, config, args).await,
        Some(("cert", args)) => cert::request_cert(http_client, config, args).await,
        Some((_, _)) => {
            eprintln!("Unknown host command");
            Ok(())
        }
        None => list::list_hosts(http_client, config, &clap::ArgMatches::default()).await,
    }
}

pub async fn resolve_host_id(input: &str, hosts: &[list::HostResponse]) -> Result<Uuid> {
    // Try exact UUID parse
    if let Ok(parsed_uuid) = Uuid::parse_str(input) {
        return Ok(parsed_uuid);
    }

    // Try exact domain name match
    for host in hosts {
        if host.host == input {
            return Ok(host.id);
        }
    }

    // Try UUID prefix match
    if input.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let matches: Vec<_> = hosts
            .iter()
            .filter(|h| h.id.to_string().starts_with(input))
            .collect();

        if matches.len() == 1 {
            return Ok(matches[0].id);
        } else if matches.is_empty() {
            return Err(anyhow::anyhow!(
                "No host found with UUID starting with '{}'",
                input
            ));
        } else {
            return Err(anyhow::anyhow!(
                "Multiple hosts ({}) found with UUID starting with '{}'",
                matches.len(),
                input
            ));
        }
    }

    Err(anyhow::anyhow!(
        "No host found with domain or UUID '{}'",
        input
    ))
}
