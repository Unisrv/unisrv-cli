use crate::config::CliConfig;
use crate::resolve::Identifiable;
use anyhow::Result;
use cidr::Ipv4Cidr;
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

impl Identifiable for list::NetworkListItem {
    fn id(&self) -> Uuid {
        self.id
    }
    fn name(&self) -> Option<&str> {
        Some(&self.name)
    }
}

mod create;
mod delete;
pub mod list;
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
                .alias("info")
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

pub async fn handle(
    config: &mut CliConfig,
    http_client: &Client,
    network_matches: &clap::ArgMatches,
) -> Result<()> {
    match network_matches.subcommand() {
        Some(("new", args)) => create::create_network(http_client, config, args).await,
        Some(("show", args)) | Some(("get", args)) => {
            show::show_network(http_client, config, args).await
        }
        Some(("delete", args)) | Some(("rm", args)) => {
            delete::delete_network(http_client, config, args).await
        }
        Some(("list", args)) | Some(("ls", args)) => {
            list::list_networks(http_client, config, args).await
        }
        Some((_, _)) => {
            eprintln!("Unknown network command");
            Ok(())
        }
        None => {
            // Default to listing networks when no subcommand is provided
            list::list_networks(http_client, config, &clap::ArgMatches::default()).await
        }
    }
}

pub fn resolve_network_id(input: &str, list: &list::NetworkListResponse) -> Result<Uuid> {
    crate::resolve::resolve_id(input, &list.networks, "network")
}

pub async fn next_ip(network_cidr: Ipv4Cidr, used_ips: &[String]) -> Result<String> {
    network_cidr
        .iter()
        .addresses()
        .find(|ip| {
            *ip != network_cidr.first().address() && // Skip network address
            *ip != network_cidr.last().address() && // Skip broadcast address
            !used_ips.contains(&ip.to_string()) // Skip already used IPs
        })
        .ok_or(anyhow::anyhow!(
            "No available IP addresses in CIDR {network_cidr}"
        ))
        .map(|ip| ip.to_string())
}
