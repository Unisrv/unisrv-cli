use std::time::Duration;

use anyhow::Result;
use clap::Command;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Error;

mod auth;
mod config;
mod error;
mod instances;
mod login;
mod services;

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
        .subcommand(instances::command())
        .subcommand(services::command())
        .subcommand(login::command())
        .subcommand(auth::command())
        .get_matches();
    let mut config = config::CliConfig::init();

    // Match on the subcommands and handle logic
    let r = match matches.subcommand() {
        Some(("instance", instance_matches)) => {
            instances::handle(&mut config, instance_matches).await
        }
        Some(("service", instance_matches)) => {
            services::handle(&mut config, instance_matches).await
        }
        Some(("login", instance_matches)) => login::handle(&mut config, instance_matches).await,
        Some(("auth", instance_matches)) => auth::handle(&mut config, instance_matches).await,
        _ => {
            eprintln!("Unknown command");
            Ok(())
        }
    };

    if let Err(e) = r {
        log::debug!("Error: {:?}", e);
        eprintln!("{} {}", console::style("error:").red().bold(), e);
        std::process::exit(1);
    }

    Ok(())
}

pub fn default_spinner() -> ProgressBar {
    let spinner_style = ProgressStyle::with_template("{spinner} {prefix:.bold.dim} {wide_msg}")
        .unwrap()
        .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ");

    let progress = ProgressBar::new_spinner();
    progress.set_style(spinner_style);
    progress.enable_steady_tick(Duration::from_millis(50));
    progress
}
