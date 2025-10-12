use crate::config::CliConfig;
use anyhow::Result;
use clap::{Arg, Command};
use reqwest::Client;

pub mod client;
mod list;
mod login;

pub fn command() -> Command {
    Command::new("registry")
        .alias("reg")
        .about("Manage container registries")
        .subcommand_required(false)
        .subcommand(
            Command::new("login")
                .about("Login to a container registry")
                .arg(
                    Arg::new("registry")
                        .help("Registry URL (e.g., ghcr.io, docker.io)")
                        .required(true)
                        .index(1),
                )
                .arg(
                    Arg::new("username")
                        .short('u')
                        .long("username")
                        .help("Username for registry authentication")
                        .required(false),
                )
                .arg(
                    Arg::new("password")
                        .short('p')
                        .long("password")
                        .value_name("PASSWORD")
                        .help("Password for registry authentication (not recommended, use --password-stdin instead)")
                        .required(false)
                        .conflicts_with("password_stdin"),
                )
                .arg(
                    Arg::new("password_stdin")
                        .long("password-stdin")
                        .help("Read password from stdin")
                        .required(false)
                        .num_args(0)
                        .conflicts_with("password"),
                ),
        )
        .subcommand(
            Command::new("list")
                .alias("ls")
                .about("List configured container registries"),
        )
}

pub async fn handle(
    config: &mut CliConfig,
    http_client: &Client,
    registry_matches: &clap::ArgMatches,
) -> Result<()> {
    match registry_matches.subcommand() {
        Some(("login", args)) => login::login_registry(config, args).await,
        Some(("list", args)) | Some(("ls", args)) => {
            list::list_registries(http_client, config, args).await
        }
        Some((_, _)) => {
            eprintln!("Unknown registry command");
            Ok(())
        }
        None => {
            // Default to listing registries when no subcommand is provided
            list::list_registries(http_client, config, &clap::ArgMatches::default()).await
        }
    }
}
