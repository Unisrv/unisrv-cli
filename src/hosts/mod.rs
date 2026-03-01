use crate::config::CliConfig;
use crate::resolve::Identifiable;
use anyhow::Result;
use clap::{Arg, Command};
use reqwest::Client;
use uuid::Uuid;

impl Identifiable for list::HostResponse {
    fn id(&self) -> Uuid {
        self.id
    }
    fn name(&self) -> Option<&str> {
        Some(&self.host)
    }
}

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

pub fn resolve_host_id(input: &str, hosts: &[list::HostResponse]) -> Result<Uuid> {
    crate::resolve::resolve_id(input, hosts, "host")
}
