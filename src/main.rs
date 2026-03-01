use clap::Command;
use reqwest::Client;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
    let matches = Command::new("unisrv")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Caspar Norée Palm <caspar.noreepalm@gmail.com>")
        .about("Provisioning CLI for managing instances")
        .subcommand_required(true)
        .subcommand(unisrv::instances::command())
        .subcommand(unisrv::networks::command())
        .subcommand(unisrv::services::command())
        .subcommand(unisrv::hosts::command())
        .subcommand(unisrv::registry::command())
        .subcommand(unisrv::login::command())
        .subcommand(unisrv::auth::command())
        .subcommand(unisrv::rollout::command())
        .get_matches();
    let mut config = unisrv::config::CliConfig::init();
    let http_client = Client::new();

    let result = match matches.subcommand() {
        Some(("instance", args)) => {
            unisrv::instances::handle(&mut config, &http_client, args).await
        }
        Some(("network", args)) => unisrv::networks::handle(&mut config, &http_client, args).await,
        Some(("service", args)) => unisrv::services::handle(&mut config, &http_client, args).await,
        Some(("host", args)) => unisrv::hosts::handle(&mut config, &http_client, args).await,
        Some(("registry", args)) => unisrv::registry::handle(&mut config, &http_client, args).await,
        Some(("login", args)) => unisrv::login::handle(&mut config, &http_client, args).await,
        Some(("auth", args)) => unisrv::auth::handle(&mut config, &http_client, args).await,
        Some(("rollout", args)) => unisrv::rollout::handle(&mut config, &http_client, args).await,
        _ => unreachable!("subcommand_required(true) ensures a subcommand is always present"),
    };

    if let Err(e) = result {
        log::debug!("Error: {e:?}");
        eprintln!("{} {}", console::style("error:").red().bold(), e);
        std::process::exit(1);
    }
}
