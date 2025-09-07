use anyhow::Result;
use clap::Command;
use reqwest::{Client, Error};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
    let matches = Command::new("cloud-cli")
        .version("0.1.0")
        .author("Caspar Norée Palm <caspar.noreepalm@gmail.com>")
        .about("Provisioning CLI for managing instances")
        .subcommand_required(true)
        .subcommand(unisrv::instances::command())
        .subcommand(unisrv::networks::command())
        .subcommand(unisrv::services::command())
        .subcommand(unisrv::login::command())
        .subcommand(unisrv::auth::command())
        .get_matches();
    let mut config = unisrv::config::CliConfig::init();
    let http_client = Client::new();

    // Match on the subcommands and handle logic
    let r = match matches.subcommand() {
        Some(("instance", instance_matches)) => {
            unisrv::instances::handle(&mut config, &http_client, instance_matches).await
        }
        Some(("network", network_matches)) => {
            unisrv::networks::handle(&mut config, &http_client, network_matches).await
        }
        Some(("service", service_matches)) => {
            unisrv::services::handle(&mut config, &http_client, service_matches).await
        }
        Some(("login", instance_matches)) => {
            unisrv::login::handle(&mut config, &http_client, instance_matches).await
        }
        Some(("auth", instance_matches)) => {
            unisrv::auth::handle(&mut config, &http_client, instance_matches).await
        }
        _ => {
            eprintln!("Unknown command");
            Ok(())
        }
    };

    if let Err(e) = r {
        log::debug!("Error: {e:?}");
        eprintln!("{} {}", console::style("error:").red().bold(), e);
        std::process::exit(1);
    }

    Ok(())
}
